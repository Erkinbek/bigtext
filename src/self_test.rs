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
    mark_time: Instant,
    log: Vec<String>,
    shot: Option<PathBuf>,
    pending_shot: Option<PathBuf>,
    /// Theme to switch to on the next frame (true = dark).
    theme: Option<bool>,
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
const CSV: &str = "id;name;age;city\r\n1;Ann;30;Tashkent\r\n2;\"Bob; Jr\";41;Samarkand\r\n3;Cy;25;Bukhara\r\n";
const CSV_WITHOUT_NAME_CITY: &str = "id;age\r\n1;30\r\n2;41\r\n3;25\r\n";

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
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(b"packed line 1\npacked line 2\n")?;
    std::fs::write(dir.join("packed.log.gz"), gz.finish()?)?;
    std::fs::write(dir.join("time.log"), "2024-05-01 10:00:00 start\n2024-05-01 10:00:05 a\n    trace\n2024-05-01 10:01:00 b\n2024-05-01 11:00:00 end\n")?;
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
    std::fs::write(dir.join("table.csv"), CSV)?;
    std::fs::write(dir.join("data.json"),"{\"name\": \"bigtext\", \"size\": 42, \"ok\": true}\n")?;
    std::fs::write(dir.join("app.log"), "2026-10-24 10:00:00.000 [ERROR] failed\n2026-10-24 10:00:01.000 [INFO] ok\n")?;
    let base: String = (0..2000).map(|i| format!("row {i}\n")).collect();
    std::fs::write(dir.join("diff-a.txt"), &base)?;
    std::fs::write(dir.join("diff-b.txt"), base.replace("row 100\n", "row one hundred\n").replace("row 1500\n", "row 1500\nextra line\n"))?;
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

/// Saves the window to `file` in the screenshot directory once animations have settled.
fn shot_step(file: &'static str) -> StepFn {
    Box::new(move |_app, c| {
        let Some(path) = c.shot.clone().map(|d| d.join(file)) else { return Step::Skip("no screenshot dir".into()) };
        if c.first() {
            let _ = std::fs::remove_file(&path);
        }
        if c.mark == 0 && c.waited() > Duration::from_millis(900) {
            c.mark = 1;
            c.pending_shot = Some(path.clone());
        }
        if c.mark == 1 && path.exists() {
            Step::Next
        } else {
            Step::Wait
        }
    })
}

/// Demo files for the screenshot tour: two logs cut from the big file that differ in a few
/// lines, and a pretty-printed JSON document.
fn setup_tour_files(dir: &Path, big: &Path) -> std::io::Result<()> {
    use std::fmt::Write as _;
    use std::io::Read as _;
    std::fs::create_dir_all(dir)?;
    let mut head = vec![0u8; 48 << 20];
    let mut f = std::fs::File::open(big)?;
    let mut got = 0;
    while got < head.len() {
        match f.read(&mut head[got..])? {
            0 => break,
            k => got += k,
        }
    }
    head.truncate(got);
    if let Some(p) = memchr::memrchr(b'\n', &head) {
        head.truncate(p + 1);
    }
    std::fs::write(dir.join("server-a.log"), &head)?;
    let mut lines: Vec<&[u8]> = head.split_inclusive(|&b| b == b'\n').collect();
    let n = lines.len();
    if n > 300_000 {
        lines[250_000] = b"2026-10-01 03:21:07.412 [ERROR] id=250001 payment gateway timeout after 30000 ms\n";
        lines.remove(90_000);
        lines.insert(51_000, b"2026-10-01 00:42:17.003 [WARN] id=51000 retry scheduled: upstream returned 503\n");
        lines[1_200] = b"2026-10-01 00:01:13.551 [ERROR] id=1200 database connection refused (pool exhausted)\n";
    }
    std::fs::write(dir.join("server-b.log"), lines.concat())?;

    let tags = ["express", "gift", "priority", "wholesale", "returning", "promo"];
    let cities = ["Tashkent", "Samarkand", "Bukhara", "Almaty", "Istanbul", "Berlin", "Seoul"];
    let mut j = String::from("[\n");
    for i in 0..40_000u64 {
        let _ = write!(
            j,
            "  {{\n    \"order_id\": {},\n    \"customer\": \"customer-{:05}\",\n    \"city\": \"{}\",\n    \"total\": {}.{:02},\n    \"paid\": {},\n    \"tags\": [\"{}\", \"{}\"],\n    \"coupon\": null\n  }}{}\n",
            100_000 + i,
            (i * 7_919) % 100_000,
            cities[(i % 7) as usize],
            (i * 37) % 900 + 10,
            (i * 13) % 100,
            i % 5 != 0,
            tags[(i % 6) as usize],
            tags[((i / 6) % 6) as usize],
            if i + 1 < 40_000 { "," } else { "" }
        );
    }
    j.push_str("]\n");
    std::fs::write(dir.join("orders.json"), j)
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
        s("screenshot: initial view", Box::new(|_app, c| {
            if c.first() {
                c.pending_shot = c.shot.clone().map(|d| d.join("selftest-start.png"));
            }
            if c.waited() < Duration::from_millis(500) {
                return Step::Wait;
            }
            Step::Next
        }));
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
            // egui animates ctrl+wheel zoom over time; let it settle.
            if c.mark == 0 && c.waited() < Duration::from_millis(1500) {
                return Step::Wait;
            }
            if c.mark == 0 {
                c.mark = 1;
                if app.settings.font_size <= 14.0 {
                    return Step::Fail(format!("ctrl+wheel font {}", app.settings.font_size));
                }
                c.key(Key::Num0, CTRL);
                c.key(Key::Equals, CTRL);
                return Step::Wait;
            }
            if c.mark == 1 {
                c.mark = 2;
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
        s("scrollbar marks Find All results and bookmarks", Box::new(|app, c| {
            use crate::view::text_view::{MARK_BOOKMARK, MARK_FOUND};
            if c.first() {
                doc(app).bookmarks = vec![35];
                return Step::Wait;
            }
            let kinds = doc(app).view.scroll_marks.kinds();
            doc(app).bookmarks.clear();
            check(kinds & MARK_FOUND != 0 && kinds & MARK_BOOKMARK != 0, format!("marks {kinds:#b}"))
        }));
        s("filter: lines with a match, then inverted", Box::new(|app, c| {
            let lines = |app: &mut BigTextApp| -> Option<Vec<u64>> {
                let r = doc(app).filter.as_ref()?;
                let g = r.data.lock().unwrap();
                g.done.then(|| g.lines.clone())
            };
            if c.first() {
                app.search.invert = false;
                app.start_filter();
                return Step::Wait;
            }
            if c.mark == 0 {
                let Some(v) = lines(app) else { return Step::Wait };
                if v != vec![51] {
                    return Step::Fail(format!("filter lines {v:?}"));
                }
                c.mark = 1;
                app.search.invert = true;
                app.start_filter();
                return Step::Wait;
            }
            let Some(v) = lines(app) else { return Step::Wait };
            app.search.invert = false;
            app.search.show_filter = false;
            doc(app).filter = None;
            check(v == vec![0, 17, 35, 67], format!("inverted filter lines {v:?}"))
        }));
        s("highlights and occurrences of the selected word", Box::new(|app, _c| {
            app.add_mark(bigtext::search::Query { pattern: "line".into(), case_sensitive: true, ..Default::default() });
            let mark = crate::app::marks::fill(app.settings.marks[0].color, true);
            doc(app).select(30, 34);
            let hl = app.view_highlights(true);
            let d = doc(app);
            let v = hl(&d.buf, 0..d.buf.len());
            let marked = v.iter().filter(|x| x.2 == mark).count();
            let occ = v.iter().filter(|x| x.2 == egui::Color32::from_white_alpha(30)).map(|x| (x.0, x.1)).collect::<Vec<_>>();
            app.settings.marks.clear();
            doc(app).select(0, 0);
            check(marked == 3 && occ == vec![(30, 34), (41, 45), (72, 76)], format!("marked {marked}, occurrences {occ:?}"))
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
        s("Edit → Text: sort the selected lines, then undo", Box::new(|app, c| {
            if c.first() {
                doc(app).select(0, 51);
                app.text_op(bigtext::textops::Op::SortAsc);
                return Step::Wait;
            }
            if c.mark == 0 {
                let t = text(app);
                if t == ORIGINAL {
                    return if c.waited() > Duration::from_secs(3) { Step::Fail("not sorted".into()) } else { Step::Wait };
                }
                if !t.starts_with("    indented line\nalpha beta gamma\nthird line here\nfoo") || doc(app).selection() != (0, 51) {
                    return Step::Fail(format!("sorted {t:?} selection {:?}", doc(app).selection()));
                }
                c.mark = 1;
                c.key(Key::Z, CTRL);
                return Step::Wait;
            }
            check(text(app) == ORIGINAL, "undo of the sort failed")
        }));
        s("block selection: Alt+drag, copy, type on every row, undo", Box::new(|app, c| {
            if c.first() {
                c.copied.clear();
                let (a, b) = (pos(app, 0, 0.0), pos(app, 2, 4.0));
                c.push(vec![Event::PointerMoved(a), Event::PointerButton { pos: a, button: PointerButton::Primary, pressed: true, modifiers: ALT }], ALT);
                c.push(vec![Event::PointerMoved(Pos2::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0))], ALT);
                c.push(vec![Event::PointerMoved(b)], ALT);
                c.push(vec![Event::PointerButton { pos: b, button: PointerButton::Primary, pressed: false, modifiers: ALT }], ALT);
                c.push(vec![], NONE);
                c.event(Event::Copy, CTRL);
                c.typed("X");
                return Step::Wait;
            }
            if c.mark == 0 {
                let t = text(app);
                if t == ORIGINAL {
                    return if c.waited() > Duration::from_secs(3) { Step::Fail(format!("no block edit, block {:?}", doc(app).view.block)) } else { Step::Wait };
                }
                let copied = c.copied.last().cloned().unwrap_or_default();
                // One row per line, joined with the document's line break (LF here).
                if !t.starts_with("Xa beta gamma\nXindented line\nXd line here\nfoo") || copied != "alph\n    \nthir" {
                    return Step::Fail(format!("text {t:?} copied {copied:?}"));
                }
                c.mark = 1;
                c.key(Key::Z, CTRL);
                return Step::Wait;
            }
            check(text(app) == ORIGINAL && doc(app).view.block.is_none(), "undo of the block edit failed")
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
        s("screenshot: after wrap/hex toggles", Box::new(|_app, c| {
            if c.first() {
                c.pending_shot = c.shot.clone().map(|d| d.join("selftest-before-about.png"));
            }
            if c.waited() < Duration::from_millis(500) {
                return Step::Wait;
            }
            Step::Next
        }));
        s("About window shows author", Box::new(|app, c| {
            if c.first() {
                app.show_about = true;
                return Step::Wait;
            }
            // Screenshot once the window's fade-in animation has finished.
            if c.mark == 0 && c.waited() > Duration::from_millis(600) {
                c.mark = 1;
                c.pending_shot = c.shot.clone().map(|d| d.join("selftest-about.png"));
            }
            if c.waited() < Duration::from_millis(1200) {
                return Step::Wait;
            }
            app.show_about = false;
            let d = doc(app);
            let state = format!("top {} caret {} len {} hex {} wrap {} rows {}", d.view.top, d.view.caret, d.buf.len(), d.hex, d.view.wrap, d.view.rows_on_screen);
            c.log.push(format!("about step view state: {state}"));
            check(crate::app::AUTHOR_LINKS.iter().any(|l| l.2.contains("Erkinbek/bigtext")), "missing repository link")
        }));
        s("bookmarks: Ctrl+F2 toggles, F2 / Shift+F2 navigate", Box::new(|app, c| {
            if c.first() {
                let p1 = pos(app, 1, 3.0);
                let p3 = pos(app, 3, 3.0);
                c.click(p1, NONE);
                c.key(Key::F2, CTRL);
                c.click(p3, NONE);
                c.key(Key::F2, CTRL);
                c.key(Key::Home, CTRL);
                c.key(Key::F2, NONE);
                return Step::Wait;
            }
            if c.frames == 2 {
                let d = doc(app);
                if d.bookmarks != vec![17, 51] || d.view.caret != 17 {
                    return Step::Fail(format!("bookmarks {:?} caret {}", d.bookmarks, d.view.caret));
                }
                c.key(Key::F2, NONE);
                return Step::Wait;
            }
            if c.frames == 3 {
                if doc(app).view.caret != 51 {
                    return Step::Fail(format!("F2 caret {}", doc(app).view.caret));
                }
                c.key(Key::F2, SHIFT);
                return Step::Wait;
            }
            if c.frames == 4 {
                if doc(app).view.caret != 17 {
                    return Step::Fail(format!("Shift+F2 caret {}", doc(app).view.caret));
                }
                // Typing above a bookmark keeps it on its line.
                c.key(Key::Home, CTRL);
                c.typed("ab");
                return Step::Wait;
            }
            let b = doc(app).bookmarks.clone();
            let ok = b == vec![19, 53];
            c.key(Key::Z, CTRL);
            check(ok, format!("bookmarks after typing {b:?}"))
        }));
        s("split view: two panes, shared caret, independent scroll", Box::new(|app, c| {
            if c.first() {
                app.open_path(c.file("big.txt"));
                return Step::Wait;
            }
            if c.mark == 0 {
                if !wait_ready(app) {
                    return Step::Wait;
                }
                c.mark = 1;
                c.key(Key::Backslash, CTRL);
                c.push(vec![], NONE);
                c.push(vec![], NONE);
                return Step::Wait;
            }
            if c.mark == 1 {
                let d = doc(app);
                let Some(sp) = d.split.clone() else { return Step::Fail("split did not open".into()) };
                let Some(r) = sp.text_rect else { return Step::Wait };
                // Scroll the lower pane and click into it.
                let p = Pos2::new(r.left() + 40.0, r.top() + d.view.cell.y * 2.5);
                c.mark = 2;
                c.mark_time = Instant::now();
                c.event(Event::PointerMoved(p), NONE);
                c.event(Event::MouseWheel { unit: MouseWheelUnit::Line, delta: egui::vec2(0.0, -20.0), phase: TouchPhase::Move, modifiers: NONE }, NONE);
                return Step::Wait;
            }
            if c.mark == 2 {
                // Let egui's smooth scrolling finish before clicking.
                if c.mark_time.elapsed() < Duration::from_millis(1500) {
                    return Step::Wait;
                }
                let d = doc(app);
                let r = d.split.as_ref().and_then(|s| s.text_rect).unwrap_or(egui::Rect::NOTHING);
                let p = Pos2::new(r.left() + 40.0, r.top() + d.view.cell.y * 2.5);
                c.mark = 3;
                c.click(p, NONE);
                return Step::Wait;
            }
            let d = doc(app);
            let sp = d.split.clone().unwrap_or_default();
            let ok = d.active_pane == 1 && sp.top > 0 && d.view.top == 0 && d.view.caret > sp.top;
            let msg = format!("active {} pane2 top {} pane1 top {} caret {}", d.active_pane, sp.top, d.view.top, d.view.caret);
            if ok {
                c.key(Key::Backslash, CTRL);
            }
            check(ok, msg)
        }));
        s("syntax highlighting by extension (JSON, log)", Box::new(|app, c| {
            if c.first() {
                app.open_path(c.file("data.json"));
                app.open_path(c.file("app.log"));
                return Step::Wait;
            }
            if !wait_ready(app) {
                return Step::Wait;
            }
            use crate::view::highlight::Lang;
            let log = Lang::from_path(doc(app).path.as_deref());
            let json = Lang::from_path(Some(&c.file("data.json")));
            check(log == Lang::Log && json == Lang::Json && app.settings.syntax, format!("log {log:?} json {json:?}"))
        }));
        s("compare files (streaming diff)", Box::new(|app, c| {
            if c.first() {
                app.open_path(c.file("diff-a.txt"));
                return Step::Wait;
            }
            if c.mark == 0 {
                if !wait_ready(app) {
                    return Step::Wait;
                }
                c.mark = 1;
                app.start_diff(c.file("diff-b.txt"));
                return Step::Wait;
            }
            let Some(r) = &app.diff else { return Step::Fail("no diff results".into()) };
            let g = r.data.lock().unwrap();
            if !g.done {
                return Step::Wait;
            }
            let got: Vec<(u64, u64, u64, u64)> = g.hunks.iter().map(|h| (h.a_line, h.a_count, h.b_line, h.b_count)).collect();
            drop(g);
            app.diff = None;
            check(got == vec![(100, 1, 100, 1), (1501, 0, 1501, 1)], format!("{got:?}"))
        }));
        s("CSV opens as a table (semicolon detected)", Box::new(|app, c| {
            if c.first() {
                app.open_path(c.file("table.csv"));
                return Step::Wait;
            }
            if !wait_ready(app) || doc(app).table_state.header_cells.is_empty() {
                return Step::Wait;
            }
            let d = doc(app);
            let delim = d.table_state.dialect.map(|x| x.delim);
            let cols = d.table_state.header_cells.len();
            check(d.table && delim == Some(b';') && cols == 4, format!("table {} delimiter {delim:?} columns {cols}", d.table))
        }));
        s("CSV: click and Ctrl+click select columns", Box::new(|app, c| {
            if c.first() {
                let cells = &doc(app).table_state.header_cells;
                let at = |col: usize| cells.iter().find(|(c, _)| *c == col).map(|(_, r)| r.center());
                let (Some(a), Some(b)) = (at(1), at(3)) else { return Step::Fail("header cells missing".into()) };
                c.click(a, NONE);
                c.click(b, CTRL);
                return Step::Wait;
            }
            let sel: Vec<usize> = doc(app).table_state.selected.iter().copied().collect();
            check(sel == vec![1, 3], format!("selected {sel:?}"))
        }));
        s("CSV: Delete asks, removes the columns and their values; Ctrl+Z restores", Box::new(|app, c| {
            if c.first() {
                c.key(Key::Delete, NONE);
                return Step::Wait;
            }
            if c.mark == 0 {
                if !app.test_has_confirm() {
                    return Step::Fail("no confirmation".into());
                }
                // Screenshot of the selection and the question once the dialog has faded in.
                if c.waited() < Duration::from_millis(600) {
                    return Step::Wait;
                }
                c.pending_shot = c.shot.clone().map(|d| d.join("selftest-csv-delete.png"));
                c.mark = 3;
                return Step::Wait;
            }
            if c.mark == 3 {
                let shot = c.shot.clone().map(|d| d.join("selftest-csv-delete.png"));
                if shot.is_some_and(|p| !p.exists()) && c.waited() < Duration::from_secs(3) {
                    return Step::Wait;
                }
                app.test_accept_confirm();
                c.mark = 1;
                return Step::Wait;
            }
            if c.mark == 1 {
                if doc(app).save_job.is_some() {
                    return Step::Wait;
                }
                let t = text(app);
                if t != CSV_WITHOUT_NAME_CITY || !doc(app).table_state.selected.is_empty() {
                    return Step::Fail(format!("after delete {t:?}"));
                }
                c.mark = 2;
                c.key(Key::Z, CTRL);
                return Step::Wait;
            }
            check(text(app) == CSV && !doc(app).is_modified(), format!("after undo {:?}", text(app)))
        }));
        s("open time.log", open_step("time.log"));
        s("go to time (Ctrl+Shift+T) and the Δt of a selection", Box::new(|app, c| {
            if c.first() {
                c.key(Key::T, CTRL_SHIFT);
                c.push(vec![], NONE);
                c.typed("10:00:30");
                c.key(Key::Enter, NONE);
                return Step::Wait;
            }
            let caret = doc(app).view.caret;
            if caret != 58 {
                return if c.waited() > Duration::from_secs(3) { Step::Fail(format!("caret {caret}")) } else { Step::Wait };
            }
            doc(app).select(0, 80);
            let dt = doc(app).selection_time_delta();
            doc(app).select(0, 0);
            check(dt == Some(60_000), format!("Δt {dt:?}"))
        }));
        s("open a .gz file: unpacked copy, read-only", open_step("packed.log.gz"));
        s("unpacked copy content; closing the tab deletes the copy", Box::new(|app, c| {
            if c.first() {
                let d = doc(app);
                let temp = d.path.clone().unwrap_or_default();
                let ok = d.read_only && d.unpacked_from.as_ref().is_some_and(|p| p.ends_with("packed.log.gz")) && temp.exists() && d.title == "packed.log.gz";
                let state = format!("read-only {} from {:?} temp {temp:?}", d.read_only, d.unpacked_from);
                let t = text(app);
                if !ok || t != "packed line 1\npacked line 2\n" {
                    return Step::Fail(format!("{state} text {t:?}"));
                }
                c.copied = vec![temp.to_string_lossy().into_owned()];
                c.key(Key::W, CTRL);
                return Step::Wait;
            }
            let temp = PathBuf::from(&c.copied[0]);
            if temp.exists() {
                return if c.waited() > Duration::from_secs(5) { Step::Fail(format!("{temp:?} still exists")) } else { Step::Wait };
            }
            c.copied.clear();
            Step::Next
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
        SelfTest::build(steps, report, dir, shot_dir)
    }

    /// Screenshot tour for the README and website: `bigtext --screenshots DIR [BIG_FILE]`.
    /// Opens a huge file and a few generated demo files, sets up each feature and saves PNGs.
    pub fn tour(shot_dir: PathBuf, big: PathBuf) -> SelfTest {
        use crate::view::highlight::Lang;
        let dir = std::env::current_dir().unwrap_or_default().join("target").join("screens-data");
        let mut steps: Vec<(&'static str, StepFn)> = Vec::new();
        let mut s = |name: &'static str, f: StepFn| steps.push((name, f));

        let src = big.clone();
        s("create demo files", Box::new(move |_, c| match setup_tour_files(&c.dir, &src) {
            Ok(()) => Step::Next,
            Err(e) => Step::Fail(e.to_string()),
        }));
        s("open big file", Box::new(move |app, c| {
            if c.first() {
                app.open_path(big.clone());
                return Step::Wait;
            }
            if wait_ready(app) {
                Step::Next
            } else {
                Step::Wait
            }
        }));
        s("log view with performance overlay", Box::new(|app, _c| {
            app.settings.show_perf = true;
            let d = doc(app);
            d.lang = Some(Lang::Log);
            let (n, _) = d.buf.line_count();
            d.goto_line(n / 2 + 1_337);
            Step::Next
        }));
        s("shot hero.png", shot_step("hero.png"));
        s("regex Find All over the whole file", Box::new(|app, c| {
            if c.first() {
                app.settings.show_perf = false;
                app.search.query = bigtext::search::Query {
                    pattern: r"\[ERROR\] id=7777\d\b".into(),
                    regex: true,
                    case_sensitive: true,
                    whole_word: false,
                    dot_all: false,
                };
                app.search.open = true;
                app.find_all();
                return Step::Wait;
            }
            let d = doc(app);
            let Some(r) = &d.results else { return Step::Fail("no results".into()) };
            let done = r.data.lock().unwrap().done;
            if done {
                Step::Next
            } else {
                Step::Wait
            }
        }));
        s("shot search.png", shot_step("search.png"));
        s("hex view in the middle of the file", Box::new(|app, _c| {
            app.search.open = false;
            app.search.show_results = false;
            let d = doc(app);
            d.goto_offset(d.buf.len() / 2 / 16 * 16);
            d.hex = true;
            Step::Next
        }));
        s("shot hex.png", shot_step("hex.png"));
        s("compare two logs", Box::new(|app, c| {
            if c.first() {
                doc(app).hex = false;
                app.open_path(c.file("server-a.log"));
                return Step::Wait;
            }
            if c.mark == 0 {
                if !wait_ready(app) {
                    return Step::Wait;
                }
                c.mark = 1;
                app.start_diff(c.file("server-b.log"));
                return Step::Wait;
            }
            let Some(r) = &app.diff else { return Step::Fail("no diff results".into()) };
            let done = r.data.lock().unwrap().done;
            if done {
                Step::Next
            } else {
                Step::Wait
            }
        }));
        s("shot compare.png", shot_step("compare.png"));
        s("JSON, light theme, split view, bookmarks", Box::new(|app, c| {
            if c.first() {
                app.diff = None;
                c.theme = Some(false);
                app.open_path(c.file("orders.json"));
                return Step::Wait;
            }
            if c.mark == 0 {
                if !wait_ready(app) {
                    return Step::Wait;
                }
                c.mark = 1;
                c.key(Key::Backslash, CTRL);
                c.push(vec![], NONE);
                c.push(vec![], NONE);
                return Step::Wait;
            }
            let d = doc(app);
            let line = |n: u64| d.buf.line_start(n).map(|x| x.0).unwrap_or(0);
            let (top2, b1, b2, caret) = (line(1_204), line(4), line(15), line(7));
            let Some(sp) = d.split.as_mut() else { return Step::Fail("split did not open".into()) };
            sp.top = top2;
            d.bookmarks = vec![b1, b2];
            d.set_caret(caret + 4, false);
            Step::Next
        }));
        s("shot json.png", shot_step("json.png"));
        let report = shot_dir.join("screenshots-report.txt");
        SelfTest::build(steps, report, dir, Some(shot_dir))
    }

    fn build(steps: Vec<(&'static str, StepFn)>, report: PathBuf, dir: PathBuf, shot_dir: Option<PathBuf>) -> SelfTest {
        SelfTest {
            steps,
            idx: 0,
            settle: 0,
            ctx: Ctx { queue: VecDeque::new(), copied: Vec::new(), dir, frames: 0, t0: Instant::now(), mark: 0, mark_time: Instant::now(), log: Vec::new(), shot: shot_dir, pending_shot: None, theme: None },
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
        if let Some(dark) = self.ctx.theme.take() {
            app.settings.dark = dark;
            ctx.set_theme(if dark { egui::Theme::Dark } else { egui::Theme::Light });
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
        for l in self.ctx.log.iter().chain(&self.shot_log) {
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
