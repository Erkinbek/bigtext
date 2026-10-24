//! In-app GUI self-test: `bigtext --self-test [REPORT] [--screenshot-dir DIR]`.
//!
//! Drives the real UI with synthetic egui input (mouse, keyboard, clipboard events) injected
//! through `App::raw_input_hook`, then checks document and app state. It also covers the edge
//! cases from the spec that need a running app: read-only files, files appended / moved /
//! deleted on disk, long and Unicode paths, the chunked (network) read path, large clipboard
//! copies and special small files. Writes a PASS/FAIL report and closes the window.

use std::collections::VecDeque;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::{Event, Key, Modifiers, MouseWheelUnit, PointerButton, Pos2, TouchPhase};

use crate::app::document::{Banner, Document};
use crate::app::BigTextApp;

pub enum Step {
    Next,
    Wait,
    Fail(String),
    Skip(String),
}

const NONE: Modifiers = Modifiers::NONE;
const CTRL: Modifiers = Modifiers { alt: false, ctrl: true, shift: false, mac_cmd: false, command: true };
const CTRL_SHIFT: Modifiers = Modifiers { alt: false, ctrl: true, shift: true, mac_cmd: false, command: true };
const SHIFT: Modifiers = Modifiers::SHIFT;
const ALT: Modifiers = Modifiers::ALT;

pub struct Ctx {
    queue: VecDeque<(Vec<Event>, Modifiers)>,
    pub copied: Vec<String>,
    pub dir: PathBuf,
    pub frames: u32,
    t0: Instant,
    pub mark: u64,
    shot: Option<PathBuf>,
    pending_shot: Option<PathBuf>,
}

type StepFn = Box<dyn FnMut(&mut BigTextApp, &mut Ctx) -> Step>;

pub struct SelfTest {
    steps: Vec<(&'static str, StepFn)>,
    idx: usize,
    settle: u32,
    ctx: Ctx,
    report: PathBuf,
    results: Vec<(&'static str, &'static str, String)>,
    finished: bool,
    started: Instant,
    max_frame_ms: f32,
    shot_path: Option<PathBuf>,
    mods: Modifiers,
    skip_frame: bool,
    shot_log: Vec<String>,
}

const ORIGINAL: &str = "alpha beta gamma\n    indented line\nthird line here\nfoo bar foo baz\nlast line\n";

fn doc(app: &mut BigTextApp) -> &mut Document {
    app.active_doc().expect("active document")
}

fn text(app: &mut BigTextApp) -> String {
    let d = doc(app);
    let snap = d.buf.snapshot();
    let b = snap.read(0..snap.len()).map(|b| b.into_owned()).unwrap_or_default();
    d.buf.codec.decode_lossy(&b)
}

fn pos(app: &mut BigTextApp, row: usize, col: f32) -> Pos2 {
    let d = doc(app);
    let r = d.view.text_rect.unwrap_or(egui::Rect::from_min_size(Pos2::new(60.0, 80.0), egui::vec2(800.0, 600.0)));
    let c = d.view.cell;
    Pos2::new(r.left() + col * c.x - d.view.scroll_x + 1.0, r.top() + row as f32 * c.y + c.y * 0.5)
}

fn check(ok: bool, msg: impl Into<String>) -> Step {
    if ok {
        Step::Next
    } else {
        Step::Fail(msg.into())
    }
}

impl Ctx {
    fn push(&mut self, events: Vec<Event>, m: Modifiers) {
        self.queue.push_back((events, m));
    }
    fn click(&mut self, p: Pos2, m: Modifiers) {
        self.push(vec![Event::PointerMoved(p), Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: true, modifiers: m }], m);
        self.push(vec![Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: false, modifiers: m }], m);
    }
    fn drag(&mut self, a: Pos2, b: Pos2) {
        self.push(vec![Event::PointerMoved(a), Event::PointerButton { pos: a, button: PointerButton::Primary, pressed: true, modifiers: NONE }], NONE);
        self.push(vec![Event::PointerMoved(Pos2::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0))], NONE);
        self.push(vec![Event::PointerMoved(b)], NONE);
        self.push(vec![Event::PointerButton { pos: b, button: PointerButton::Primary, pressed: false, modifiers: NONE }], NONE);
    }
    fn key(&mut self, k: Key, m: Modifiers) {
        self.push(
            vec![
                Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: m },
                Event::Key { key: k, physical_key: None, pressed: false, repeat: false, modifiers: m },
            ],
            m,
        );
    }
    fn typed(&mut self, s: &str) {
        for ch in s.chars() {
            self.push(vec![Event::Text(ch.to_string())], NONE);
        }
    }
    fn event(&mut self, e: Event, m: Modifiers) {
        self.push(vec![e], m);
    }
    fn file(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
    fn first(&self) -> bool {
        self.frames == 1
    }
    fn waited(&self) -> Duration {
        self.t0.elapsed()
    }
}

fn wait_ready(app: &mut BigTextApp) -> bool {
    let d = doc(app);
    d.is_ready() && d.view.first_paint.is_some() && d.view.text_rect.is_some() && d.buf.index.is_done()
}

fn setup_files(dir: &Path) -> std::io::Result<()> {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join("edit.txt"), ORIGINAL)?;
    std::fs::write(dir.join("ro.txt"), "read only content\n")?;
    let mut p = std::fs::metadata(dir.join("ro.txt"))?.permissions();
    p.set_readonly(true);
    std::fs::set_permissions(dir.join("ro.txt"), p)?;
    std::fs::write(dir.join("grow.log"), "line 1\nline 2\n")?;
    std::fs::write(dir.join("move.txt"), "will be moved\n")?;
    std::fs::write(dir.join("delete.txt"), "will be deleted\n")?;
    std::fs::write(dir.join("empty.txt"), "")?;
    std::fs::write(dir.join("no-newline.txt"), "first\nsecond without newline")?;
    std::fs::write(dir.join("cr-only.txt"), "old\rmac\rline\rendings\r")?;
    std::fs::write(dir.join("bom-middle.txt"), "start\n\u{FEFF}after bom\nend\n")?;
    let mut nul = b"text with NUL ".to_vec();
    nul.extend_from_slice(&[0; 40]);
    nul.extend_from_slice(b"\n\xFF\xFE invalid \xC3\x28\n");
    std::fs::write(dir.join("nul.bin"), nul)?;
    let utf16: Vec<u8> = [0xFF, 0xFE].into_iter().chain("Привет UTF-16\r\nвторая строка\r\n".encode_utf16().flat_map(|u| u.to_le_bytes())).collect();
    std::fs::write(dir.join("utf16.txt"), utf16)?;
    let (cp, _, _) = encoding_rs::WINDOWS_1251.encode("Съешь же ещё этих мягких французских булок\r\nстрока два\r\n");
    std::fs::write(dir.join("cp1251.txt"), cp)?;
    std::fs::write(dir.join("chunk.txt"), "chunked read path\n".repeat(10_000))?;
    let line = "0123456789 abcdefghijklmnopqrstuvwxyz ABCDEFGHIJKLMNOPQRSTUVWXYZ\n";
    std::fs::write(dir.join("big.txt"), line.repeat(6 * 1024 * 1024 / line.len() + 1))?;
    let uni = dir.join("юникод-日本");
    std::fs::create_dir_all(&uni)?;
    std::fs::write(uni.join("файл.txt"), "unicode path works\n")?;
    let mut long = dir.join("long");
    while long.as_os_str().len() < 300 {
        long = long.join("a_very_long_directory_name_to_exceed_max_path");
    }
    std::fs::create_dir_all(&long)?;
    std::fs::write(long.join("deep.txt"), "long path file\n")?;
    std::fs::write(dir.join("long-path.txt"), long.join("deep.txt").to_string_lossy().as_bytes())?;
    Ok(())
}

fn open_step(name: &'static str) -> StepFn {
    Box::new(move |app, c| {
        if c.first() {
            let p = if name == "long" {
                PathBuf::from(std::fs::read_to_string(c.file("long-path.txt")).unwrap_or_default())
            } else if name == "unicode" {
                c.dir.join("юникод-日本").join("файл.txt")
            } else {
                c.file(name)
            };
            app.open_path(p);
            return Step::Wait;
        }
        if wait_ready(app) {
            Step::Next
        } else {
            Step::Wait
        }
    })
}

impl SelfTest {
    pub fn new(report: PathBuf, shot_dir: Option<PathBuf>) -> SelfTest {
        let dir = std::env::current_dir().unwrap_or_default().join("target").join("selftest");
        let mut steps: Vec<(&'static str, StepFn)> = Vec::new();
        let mut s = |name: &'static str, f: StepFn| steps.push((name, f));
        // `s` mutably borrows `steps` until its last use below.

        s("setup test files", Box::new(|_, c| match setup_files(&c.dir) {
            Ok(()) => Step::Next,
            Err(e) => Step::Fail(e.to_string()),
        }));
        s("open edit.txt", open_step("edit.txt"));
        s("mouse: click places caret", Box::new(|app, c| {
            if c.first() {
                let p = pos(app, 0, 6.0);
                c.click(p, NONE);
                return Step::Wait;
            }
            let d = doc(app);
            check(d.view.caret == 6 && !d.has_selection(), format!("caret {} sel {:?}", d.view.caret, d.selection()))
        }));
        s("mouse: double-click selects word", Box::new(|app, c| {
            // Wait out egui's triple-click window (0.6 s) after the previous click.
            if c.mark == 0 && c.waited() < Duration::from_millis(750) {
                return Step::Wait;
            }
            if c.mark == 0 {
                c.mark = 1;
                let p = pos(app, 0, 7.0);
                c.click(p, NONE);
                c.click(p, NONE);
                return Step::Wait;
            }
            let sel = doc(app).selection();
            check(sel == (6, 10), format!("selection {sel:?}"))
        }));
        s("mouse: triple-click selects the line", Box::new(|app, c| {
            if c.first() {
                let p = pos(app, 0, 7.0);
                c.click(p, NONE);
                return Step::Wait;
            }
            let sel = doc(app).selection();
            check(sel == (0, 17), format!("selection {sel:?}"))
        }));
        s("mouse: drag selects across lines", Box::new(|app, c| {
            if c.mark == 0 && c.waited() < Duration::from_millis(750) {
                return Step::Wait;
            }
            if c.mark == 0 {
                c.mark = 1;
                let (a, b) = (pos(app, 0, 0.0), pos(app, 1, 4.0));
                c.drag(a, b);
                return Step::Wait;
            }
            let sel = doc(app).selection();
            check(sel == (0, 21), format!("selection {sel:?}"))
        }));
        s("mouse: shift+click extends selection", Box::new(|app, c| {
            if c.first() {
                let p = pos(app, 2, 5.0);
                c.click(p, SHIFT);
                return Step::Wait;
            }
            let sel = doc(app).selection();
            check(sel == (0, 40), format!("selection {sel:?}"))
        }));
        s("keys: Home is smart, End goes to line end", Box::new(|app, c| {
            if c.first() {
                let p = pos(app, 1, 10.0);
                c.click(p, NONE);
                c.key(Key::Home, NONE);
                return Step::Wait;
            }
            if c.frames == 2 {
                let caret = doc(app).view.caret;
                if caret != 21 {
                    return Step::Fail(format!("smart home caret {caret}, expected 21"));
                }
                c.key(Key::End, NONE);
                return Step::Wait;
            }
            let caret = doc(app).view.caret;
            check(caret == 34, format!("end caret {caret}"))
        }));
        s("typing: Enter auto-indents, text inserted", Box::new(|app, c| {
            if c.first() {
                c.key(Key::Enter, NONE);
                c.typed("new");
                return Step::Wait;
            }
            let t = text(app);
            let expected = ORIGINAL.replacen("indented line\n", "indented line\n    new\n", 1);
            check(t == expected, format!("text {t:?}"))
        }));
        s("undo restores original (Ctrl+Z)", Box::new(|app, c| {
            if c.first() {
                for _ in 0..4 {
                    c.key(Key::Z, CTRL);
                }
                return Step::Wait;
            }
            let t = text(app);
            check(t == ORIGINAL && !doc(app).is_modified(), format!("text {t:?}"))
        }));
        s("redo (Ctrl+Y) and undo again", Box::new(|app, c| {
            if c.first() {
                for _ in 0..4 {
                    c.key(Key::Y, CTRL);
                }
                return Step::Wait;
            }
            if c.frames == 2 {
                if !text(app).contains("    new\n") {
                    return Step::Fail("redo did not restore the edit".into());
                }
                for _ in 0..4 {
                    c.key(Key::Z, CTRL);
                }
                return Step::Wait;
            }
            check(text(app) == ORIGINAL, "second undo failed")
        }));
        s("clipboard: Ctrl+A + copy", Box::new(|_app, c| {
            if c.first() {
                c.key(Key::A, CTRL);
                c.event(Event::Copy, CTRL);
                return Step::Wait;
            }
            let last = c.copied.last().cloned().unwrap_or_default();
            check(last == ORIGINAL, format!("copied {last:?}"))
        }));
        s("clipboard: paste at document start", Box::new(|app, c| {
            if c.first() {
                c.key(Key::Home, CTRL);
                c.event(Event::Paste("PASTED\n".into()), CTRL);
                return Step::Wait;
            }
            if c.frames == 2 {
                let t = text(app);
                if t != format!("PASTED\n{ORIGINAL}") {
                    return Step::Fail(format!("after paste {t:?}"));
                }
                c.key(Key::Z, CTRL);
                return Step::Wait;
            }
            check(text(app) == ORIGINAL, "undo paste failed")
        }));
        s("Ctrl+D duplicates line, Ctrl+Shift+K deletes it", Box::new(|app, c| {
            if c.first() {
                let p = pos(app, 3, 2.0);
                c.click(p, NONE);
                c.key(Key::D, CTRL);
                return Step::Wait;
            }
            if c.frames == 2 {
                if !text(app).contains("foo bar foo baz\nfoo bar foo baz\n") {
                    return Step::Fail(format!("duplicate failed: {:?}", text(app)));
                }
                c.key(Key::K, CTRL_SHIFT);
                return Step::Wait;
            }
            check(text(app) == ORIGINAL, format!("delete line result {:?}", text(app)))
        }));
        s("Backspace + Delete, one undo restores", Box::new(|app, c| {
            if c.first() {
                let p = pos(app, 0, 5.0);
                c.click(p, NONE);
                c.key(Key::Backspace, NONE);
                c.key(Key::Delete, NONE);
                return Step::Wait;
            }
            if c.frames == 2 {
                if !text(app).starts_with("alphbeta") {
                    return Step::Fail(format!("got {:?}", text(app)));
                }
                c.key(Key::Z, CTRL);
                return Step::Wait;
            }
            check(text(app) == ORIGINAL, "undo of deletes failed")
        }));
        s("zoom: Ctrl+wheel, Ctrl+=, Ctrl+-, Ctrl+0", Box::new(|app, c| {
            if c.first() {
                let p = pos(app, 2, 4.0);
                c.event(Event::PointerMoved(p), NONE);
                c.event(Event::MouseWheel { unit: MouseWheelUnit::Line, delta: egui::vec2(0.0, 3.0), phase: TouchPhase::Move, modifiers: CTRL }, CTRL);
                return Step::Wait;
            }
            // egui animates ctrl+wheel zoom over a few frames; let it settle.
            if c.frames < 40 {
                return Step::Wait;
            }
            if c.frames == 40 {
                if app.settings.font_size <= 14.0 {
                    return Step::Fail(format!("ctrl+wheel font {}", app.settings.font_size));
                }
                c.key(Key::Num0, CTRL);
                c.key(Key::Equals, CTRL);
                return Step::Wait;
            }
            if c.frames == 41 {
                if app.settings.font_size != 15.0 {
                    return Step::Fail(format!("ctrl+= font {}", app.settings.font_size));
                }
                c.key(Key::Minus, CTRL);
                return Step::Wait;
            }
            check(app.settings.font_size == 14.0, format!("font {}", app.settings.font_size))
        }));
        s("search: Ctrl+F, type, Enter finds first match", Box::new(|app, c| {
            if c.first() {
                let p = pos(app, 4, 0.0);
                c.click(p, NONE);
                app.search.query.pattern.clear();
                c.key(Key::F, CTRL);
                c.push(vec![], NONE);
                c.typed("foo");
                c.key(Key::Enter, NONE);
                return Step::Wait;
            }
            let sel = doc(app).selection();
            if sel == (51, 54) {
                return Step::Next;
            }
            if c.waited() > Duration::from_secs(3) {
                return Step::Fail(format!("pattern {:?} selection {sel:?} open {}", app.search.query.pattern, app.search.open));
            }
            Step::Wait
        }));
        s("search: F3 next, Shift+F3 previous (wraps)", Box::new(|app, c| {
            if c.first() {
                c.key(Key::F3, NONE);
                c.mark = 0;
                return Step::Wait;
            }
            let sel = doc(app).selection();
            if c.mark == 0 {
                if sel == (59, 62) {
                    c.mark = 1;
                    c.key(Key::F3, NONE);
                }
                return Step::Wait;
            }
            if c.mark == 1 {
                if sel == (51, 54) {
                    c.mark = 2;
                    c.key(Key::F3, SHIFT);
                }
                return Step::Wait;
            }
            check(sel == (59, 62), format!("shift+F3 selection {sel:?}"))
        }));
        s("search: Find All lists both matches", Box::new(|app, c| {
            if c.first() {
                app.find_all();
                return Step::Wait;
            }
            let d = doc(app);
            let Some(r) = &d.results else { return Step::Fail("no results".into()) };
            let g = r.data.lock().unwrap();
            if !g.done {
                return Step::Wait;
            }
            check(g.matches == vec![(51, 54), (59, 62)], format!("{:?}", g.matches))
        }));
        s("replace all (undoable batch) + undo", Box::new(|app, c| {
            if c.first() {
                app.search.replace_with = "QUX".into();
                app.replace_all();
                return Step::Wait;
            }
            if c.mark != 9 {
                if doc(app).save_job.is_some() {
                    return Step::Wait;
                }
                let t = text(app);
                if !t.contains("QUX bar QUX baz") {
                    return Step::Fail(format!("after replace {t:?}"));
                }
                c.mark = 9;
                app.search.open = false;
                c.push(vec![], NONE);
                c.push(vec![], NONE);
                c.key(Key::Z, CTRL);
                return Step::Wait;
            }
            check(text(app) == ORIGINAL, "undo of replace all failed")
        }));
        s("go to line (Ctrl+G) and byte offset (Ctrl+Shift+G)", Box::new(|app, c| {
            if c.first() {
                c.key(Key::G, CTRL);
                c.push(vec![], NONE);
                c.typed("3");
                c.key(Key::Enter, NONE);
                return Step::Wait;
            }
            if c.frames == 2 {
                let caret = doc(app).view.caret;
                if caret != 35 {
                    return Step::Fail(format!("goto line 3 caret {caret}"));
                }
                c.key(Key::G, CTRL_SHIFT);
                c.push(vec![], NONE);
                c.typed("0x10");
                c.key(Key::Enter, NONE);
                return Step::Wait;
            }
            let caret = doc(app).view.caret;
            check(caret == 16, format!("goto offset 0x10 caret {caret}"))
        }));
        s("save (Ctrl+S) writes the file atomically", Box::new(|app, c| {
            if c.first() {
                c.key(Key::End, CTRL);
                c.typed("X");
                c.key(Key::S, CTRL);
                return Step::Wait;
            }
            let d = doc(app);
            if d.save_job.is_some() || !d.is_ready() || d.is_modified() {
                return Step::Wait;
            }
            let disk = std::fs::read_to_string(c.file("edit.txt")).unwrap_or_default();
            let leftovers = std::fs::read_dir(&c.dir).map(|r| r.filter_map(|e| e.ok()).filter(|e| e.file_name().to_string_lossy().ends_with(".bigtext.tmp")).count()).unwrap_or(99);
            check(disk == format!("{ORIGINAL}X") && leftovers == 0, format!("disk {disk:?}, temp files {leftovers}"))
        }));
        s("close modified tab asks to save (Esc cancels)", Box::new(|app, c| {
            if c.first() {
                c.typed("Y");
                c.key(Key::W, CTRL);
                return Step::Wait;
            }
            if c.frames == 2 {
                if !app.test_has_confirm() {
                    return Step::Fail("no unsaved-changes prompt".into());
                }
                c.key(Key::Escape, NONE);
                return Step::Wait;
            }
            let ok = !app.test_has_confirm() && doc(app).title == "edit.txt" && doc(app).is_modified();
            if ok {
                c.key(Key::Z, CTRL);
            }
            check(ok, "prompt did not cancel cleanly")
        }));
        s("Alt+Z word wrap, hex view, F12 overlay", Box::new(|app, c| {
            if c.first() {
                c.key(Key::Z, ALT);
                c.key(Key::F12, NONE);
                return Step::Wait;
            }
            if c.frames == 2 {
                if !doc(app).view.wrap || !app.settings.show_perf {
                    return Step::Fail("wrap/perf toggles failed".into());
                }
                c.key(Key::Z, ALT);
                c.key(Key::F12, NONE);
                let d = doc(app);
                d.hex = true;
                d.set_caret(10, false);
                c.push(vec![], NONE);
                c.push(vec![], NONE);
                c.key(Key::ArrowRight, NONE);
                return Step::Wait;
            }
            let d = doc(app);
            let caret = d.view.caret;
            d.hex = false;
            check(caret == 11 && !d.view.wrap, format!("hex caret {caret}"))
        }));
        s("About window shows author", Box::new(|app, c| {
            if c.first() {
                app.show_about = true;
                c.pending_shot = c.shot.clone().map(|d| d.join("selftest-about.png"));
                c.push(vec![], NONE);
                c.push(vec![], NONE);
                return Step::Wait;
            }
            if c.frames < 8 {
                return Step::Wait;
            }
            app.show_about = false;
            check(crate::app::AUTHOR_LINKS.iter().any(|l| l.2.contains("Erkinbek/bigtext")), "missing repository link")
        }));
        s("open read-only file", open_step("ro.txt"));
        s("read-only file blocks edits", Box::new(|app, c| {
            if c.first() {
                if !doc(app).read_only {
                    return Step::Fail("read-only attribute not detected".into());
                }
                c.typed("Z");
                return Step::Wait;
            }
            check(text(app) == "read only content\n" && !doc(app).is_modified(), "edit went through")
        }));
        s("saving over a read-only file fails cleanly", Box::new(|app, c| {
            if c.first() {
                doc(app).read_only = false;
                c.typed("Z");
                c.key(Key::S, CTRL);
                return Step::Wait;
            }
            let d = doc(app);
            if d.save_job.is_some() || !d.is_ready() {
                return Step::Wait;
            }
            let err = app.test_error();
            let disk = std::fs::read_to_string(c.file("ro.txt")).unwrap_or_default();
            let kept = text(app).starts_with('Z') && doc(app).is_modified();
            app.test_dismiss();
            check(err.is_some() && disk == "read only content\n" && kept, format!("error {err:?} disk {disk:?} edits kept {kept}"))
        }));
        s("open growing log", open_step("grow.log"));
        s("appended file shows a change banner", Box::new(|app, c| {
            if c.first() {
                let mut f = std::fs::OpenOptions::new().append(true).open(c.file("grow.log")).unwrap();
                let _ = f.write_all(b"line 3\n");
                return Step::Wait;
            }
            match &doc(app).banner {
                Some(Banner::ChangedOnDisk { grown: true, deleted: false }) => Step::Next,
                _ => Step::Wait,
            }
        }));
        s("follow mode tails appended lines", Box::new(|app, c| {
            if c.first() {
                let d = doc(app);
                d.banner = None;
                d.follow = true;
                let mut f = std::fs::OpenOptions::new().append(true).open(c.file("grow.log")).unwrap();
                let _ = f.write_all(b"line 4\nline 5\n");
                return Step::Wait;
            }
            let d = doc(app);
            let want = "line 1\nline 2\nline 3\nline 4\nline 5\n".len() as u64;
            if d.is_ready() && d.buf.len() == want && d.view.caret == want {
                return Step::Next;
            }
            Step::Wait
        }));
        s("open file to be moved", open_step("move.txt"));
        s("moved file shows a banner, text stays readable", Box::new(|app, c| {
            if c.first() {
                if let Err(e) = std::fs::rename(c.file("move.txt"), c.file("moved.txt")) {
                    return Step::Fail(format!("rename refused: {e}"));
                }
                return Step::Wait;
            }
            match &doc(app).banner {
                Some(Banner::ChangedOnDisk { deleted: true, .. }) => check(text(app) == "will be moved\n", "text lost"),
                _ => Step::Wait,
            }
        }));
        s("open file to be deleted", open_step("delete.txt"));
        s("deleting an open file: refused or banner, never a crash", Box::new(|app, c| {
            if c.first() {
                c.mark = std::fs::remove_file(c.file("delete.txt")).is_ok() as u64;
                return Step::Wait;
            }
            if c.mark == 0 {
                return check(text(app) == "will be deleted\n", "text lost");
            }
            match &doc(app).banner {
                Some(Banner::ChangedOnDisk { deleted: true, .. }) => check(text(app) == "will be deleted\n", "text lost"),
                _ => Step::Wait,
            }
        }));
        s("open path longer than 260 characters", open_step("long"));
        s("edit + save on the long path", Box::new(|app, c| {
            if c.first() {
                c.key(Key::Home, CTRL);
                c.typed("L");
                c.key(Key::S, CTRL);
                return Step::Wait;
            }
            let d = doc(app);
            if d.save_job.is_some() || !d.is_ready() || d.is_modified() {
                if app.test_error().is_some() {
                    return Step::Fail(app.test_error().unwrap_or_default());
                }
                return Step::Wait;
            }
            let p = PathBuf::from(std::fs::read_to_string(c.file("long-path.txt")).unwrap_or_default());
            let disk = std::fs::read_to_string(&p).unwrap_or_default();
            check(disk == "Llong path file\n", format!("disk {disk:?} (path len {})", p.as_os_str().len()))
        }));
        s("open Unicode path", open_step("unicode"));
        s("Unicode path content", Box::new(|app, _| check(text(app) == "unicode path works\n", "wrong content")));
        s("chunked read path (forced, as for network drives)", Box::new(|app, c| {
            if c.first() {
                std::env::set_var("BIGTEXT_FORCE_CHUNKED", "1");
                app.open_path(c.file("chunk.txt"));
                return Step::Wait;
            }
            if !wait_ready(app) {
                return Step::Wait;
            }
            std::env::remove_var("BIGTEXT_FORCE_CHUNKED");
            let d = doc(app);
            let kind = d.buf.source.kind_name();
            let (lines, _) = d.buf.line_count();
            check(kind == "chunked" && lines == 10_001, format!("backing {kind}, lines {lines}"))
        }));
        s("UNC path (\\\\localhost\\share) opens with chunked reads", Box::new(|app, c| {
            if c.first() {
                let local = c.file("chunk.txt");
                let s = local.to_string_lossy().replace('/', "\\");
                let drive = s.chars().next().unwrap_or('C');
                let unc = PathBuf::from(format!(r"\\localhost\{drive}$\{}", &s[3..]));
                if std::fs::metadata(&unc).is_err() {
                    return Step::Skip("administrative share not accessible".into());
                }
                app.open_path(unc);
                return Step::Wait;
            }
            if !wait_ready(app) {
                return Step::Wait;
            }
            let d = doc(app);
            check(d.buf.source.kind_name() == "chunked", format!("backing {}", d.buf.source.kind_name()))
        }));
        for (name, lines) in [("empty.txt", 1u64), ("no-newline.txt", 2), ("cr-only.txt", 5), ("bom-middle.txt", 4), ("utf16.txt", 3), ("cp1251.txt", 3)] {
            s(Box::leak(format!("special file {name}").into_boxed_str()), Box::new(move |app, c| {
                if c.first() {
                    app.open_path(c.file(name));
                    return Step::Wait;
                }
                if !wait_ready(app) {
                    return Step::Wait;
                }
                let (n, exact) = doc(app).buf.line_count();
                let t = text(app);
                let content_ok = match name {
                    "utf16.txt" => t.contains("Привет UTF-16"),
                    "cp1251.txt" => t.contains("французских"),
                    "cr-only.txt" => doc(app).buf.codec.nl == bigtext::encoding::NlByte::Cr,
                    _ => true,
                };
                check(n == lines && exact && content_ok, format!("lines {n} (exact {exact}), content ok {content_ok}, encoding {}", doc(app).buf.codec.enc.name()))
            }));
        }
        s("typing into an empty file", Box::new(|app, c| {
            if c.first() {
                app.open_path(c.file("empty.txt"));
                c.push(vec![], NONE);
                c.typed("abc");
                c.key(Key::Enter, NONE);
                return Step::Wait;
            }
            let t = text(app);
            check(t == "abc\n" || t == "abc\r\n", format!("{t:?}"))
        }));
        s("binary file offers hex view", Box::new(|app, c| {
            if c.first() {
                app.open_path(c.file("nul.bin"));
                return Step::Wait;
            }
            if !wait_ready(app) {
                return Step::Wait;
            }
            check(doc(app).looks_binary && doc(app).banner == Some(Banner::Binary), "binary not detected")
        }));
        s("large copy (6 MB) runs on a worker", Box::new(|app, c| {
            if c.first() {
                app.open_path(c.file("big.txt"));
                return Step::Wait;
            }
            if c.mark == 0 {
                if !wait_ready(app) {
                    return Step::Wait;
                }
                c.mark = 1;
                c.copied.clear();
                c.key(Key::A, CTRL);
                c.event(Event::Copy, CTRL);
                return Step::Wait;
            }
            let want = doc(app).buf.len() as usize;
            match c.copied.last() {
                Some(s) if s.len() == want => Step::Next,
                Some(s) => Step::Fail(format!("copied {} of {want} bytes", s.len())),
                None => Step::Wait,
            }
        }));
        SelfTest {
            steps,
            idx: 0,
            settle: 0,
            ctx: Ctx { queue: VecDeque::new(), copied: Vec::new(), dir, frames: 0, t0: Instant::now(), mark: 0, shot: shot_dir.clone(), pending_shot: None },
            report,
            results: Vec::new(),
            finished: false,
            started: Instant::now(),
            max_frame_ms: 0.0,
            shot_path: None,
            mods: NONE,
            skip_frame: false,
            shot_log: Vec::new(),
        }
    }

    /// Replaces real pointer/keyboard input with the queued synthetic events.
    pub fn inject(&mut self, raw: &mut egui::RawInput) {
        raw.events.retain(|e| {
            !matches!(
                e,
                Event::PointerMoved(_) | Event::PointerButton { .. } | Event::MouseWheel { .. } | Event::Key { .. } | Event::Text(_) | Event::Copy | Event::Cut | Event::Paste(_) | Event::PointerGone
            )
        });
        raw.focused = true;
        let next = self.ctx.queue.pop_front();
        // Modifiers last for the whole frame, so they are set per injected frame and released
        // on the first frame without queued input.
        let mods = next.as_ref().map(|n| n.1).unwrap_or(NONE);
        if mods != self.mods {
            raw.events.insert(0, Event::ModifiersChanged(mods));
            self.mods = mods;
        }
        if let Some((events, _)) = next {
            raw.events.extend(events);
            if self.ctx.queue.is_empty() {
                self.settle = 2;
            }
        }
    }

    pub fn before_frame(&mut self, app: &mut BigTextApp, ctx: &egui::Context) {
        ctx.request_repaint();
        if self.finished {
            return;
        }
        if let Some(path) = self.ctx.pending_shot.take() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            self.shot_log.push(format!("requested {}", path.display()));
            self.shot_path = Some(path);
        }
        let shot = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(img) = shot {
            self.shot_log.push(format!("screenshot event {}x{}", img.size[0], img.size[1]));
            if let Some(path) = self.shot_path.take() {
                let rgba: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_srgba_unmultiplied()).collect();
                let r = crate::bench_ui::write_png(&path, img.size[0] as u32, img.size[1] as u32, &rgba);
                self.skip_frame = true; // PNG encoding is test-harness work
                self.shot_log.push(format!("saved {} -> {:?}", path.display(), r.err()));
            }
        }
        if !self.ctx.queue.is_empty() {
            return;
        }
        if self.settle > 0 {
            self.settle -= 1;
            return;
        }
        if self.idx >= self.steps.len() {
            // Test documents are left modified on purpose; exit without the unsaved prompt.
            app.test_force_exit();
            self.finish(ctx);
            return;
        }
        self.ctx.frames += 1;
        let (name, f) = &mut self.steps[self.idx];
        let name = *name;
        let r = f(app, &mut self.ctx);
        let res = match r {
            Step::Next => Some(("PASS", String::new())),
            Step::Fail(m) => Some(("FAIL", m)),
            Step::Skip(m) => Some(("SKIP", m)),
            Step::Wait if self.ctx.t0.elapsed() > Duration::from_secs(20) => Some(("FAIL", "timed out".to_string())),
            Step::Wait => None,
        };
        if let Some((status, msg)) = res {
            if self.idx == 0 {
                // Test-file setup runs on this thread; do not count it as UI work.
                self.max_frame_ms = 0.0;
                self.skip_frame = true;
            }
            self.results.push((name, status, msg));
            self.idx += 1;
            self.ctx.frames = 0;
            self.ctx.mark = 0;
            self.ctx.t0 = Instant::now();
            self.ctx.queue.clear();
        }
    }

    pub fn after_frame(&mut self, ctx: &egui::Context, frame_ms: f32) {
        if !std::mem::take(&mut self.skip_frame) {
            self.max_frame_ms = self.max_frame_ms.max(frame_ms);
        }
        let copied: Vec<String> = ctx.output(|o| {
            o.commands
                .iter()
                .filter_map(|c| match c {
                    egui::OutputCommand::CopyText(s) => Some(s.clone()),
                    _ => None,
                })
                .collect()
        });
        self.ctx.copied.extend(copied);
    }

    fn finish(&mut self, ctx: &egui::Context) {
        self.finished = true;
        let pass = self.results.iter().filter(|r| r.1 == "PASS").count();
        let fail = self.results.iter().filter(|r| r.1 == "FAIL").count();
        let skip = self.results.iter().filter(|r| r.1 == "SKIP").count();
        let mut out = format!(
            "BigText GUI self-test: {pass} passed, {fail} failed, {skip} skipped in {:.1}s (max frame {:.2} ms)\n\n",
            self.started.elapsed().as_secs_f64(),
            self.max_frame_ms
        );
        for (name, status, msg) in &self.results {
            out.push_str(&format!("[{status}] {name}{}\n", if msg.is_empty() { String::new() } else { format!(" — {msg}") }));
        }
        for l in &self.shot_log {
            out.push_str(&format!("note: {l}
"));
        }
        let _ = std::fs::write(&self.report, out);
        // Restore the read-only test file so the directory can be cleaned up.
        if let Ok(meta) = std::fs::metadata(self.ctx.file("ro.txt")) {
            let mut p = meta.permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            p.set_readonly(false);
            let _ = std::fs::set_permissions(self.ctx.file("ro.txt"), p);
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}
