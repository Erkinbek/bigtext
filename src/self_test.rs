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
const LEVELS_LOG: &str = "2024-05-01 10:00:00 INFO start\n2024-05-01 10:00:01 DEBUG cfg loaded\n2024-05-01 10:00:02 WARN slow disk\n2024-05-01 10:00:03 ERROR db timeout\n2024-05-01 10:00:04 INFO no error here\n2024-05-01 10:00:05 [error] nginx style\n";
const REQUESTS_LOG: &str = "2024-05-01 10:00:00 INFO request_id=r-1 GET /a\n2024-05-01 10:00:01 INFO request_id=r-2 GET /b\n2024-05-01 10:00:02 INFO request_id=r-1 done\n2024-05-01 10:00:03 ERROR request_id=r-2 failed\njava.lang.IllegalStateException: boom\n\tat x.Y.z(Y.java:1)\n2024-05-01 10:00:05 INFO request_id=r-2 done\n";
const BRACKETS_JSON: &str = "{\"a\": [1, \"]\", {\"b\": 2}]}\n";
const CATALOG_XML: &str = "<?xml version=\"1.0\"?>\n<catalog>\n  <item id=\"1\"><price>10</price></item>\n  <item id=\"2\"><price>12</price></item>\n</catalog>\n";
const OUTLINE_JSON: &str = "{\"name\": \"demo\", \"items\": [{\"id\": 10}, {\"id\": 11}, {\"id\": 12}]}\n";
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

const CODEC_TXT: &str = "data: aGVsbG8gd29ybGQ=\njwt eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjMiLCJuYW1lIjoiQW5uIn0.c2lnbmF0dXJl end\nbin AAEC\n";

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
    std::fs::write(dir.join("cli.log"), "first 10:00\nsecond 10:00 10:05\nthird\n")?;
    std::fs::write(dir.join("time.log"), "2024-05-01 10:00:00 start\n2024-05-01 10:00:05 a\n    trace\n2024-05-01 10:01:00 b\n2024-05-01 11:00:00 end\n")?;
    std::fs::write(dir.join("levels.log"), LEVELS_LOG)?;
    std::fs::write(dir.join("requests.log"), REQUESTS_LOG)?;
    for n in 1..=3 {
        std::fs::write(dir.join(format!("tab{n}.txt")), format!("tab {n}\n"))?;
    }
    std::fs::write(dir.join("sales.csv"), "city,qty\nTashkent,3\nBukhara,5\nTashkent,7\n")?;
    std::fs::write(dir.join("broken.csv"), "id,qty\n1,2\n2\n3,x\n")?;
    std::fs::write(dir.join("cities.csv"), "city,country\nTashkent,UZ\nBukhara,UZ\n")?;
    std::fs::write(dir.join("app.logfmt"), "level=info msg=ok\nlevel=error msg=\"bad x\" code=7\n")?;
    std::fs::write(dir.join("excel.csv"), "sep=;\n# exported\nname;qty\nann;1\nbob;2\n")?;
    std::fs::write(dir.join("numbers.csv"), "1,2\n3,4\n5,6\n")?;
    std::fs::write(dir.join("outline.json"), OUTLINE_JSON)?;
    std::fs::write(dir.join("catalog.xml"), CATALOG_XML)?;
    std::fs::write(dir.join("events.jsonl"), "{\"id\": 1, \"msg\": \"start\"}\n{\"id\": 2, \"msg\": \"a, b\", \"user\": \"ann\"}\n")?;
    std::fs::write(dir.join("ws.txt"), "a b\tc\r\nnext\r\n")?;
    std::fs::write(dir.join("brackets.json"), BRACKETS_JSON)?;
    std::fs::write(dir.join("multi.txt"), "foo x foo y foo\nfirst\nsecond\n")?;
    // Unix times: 2024-05-01 14:32:05, 14:33:05 and 14:34:05 UTC.
    std::fs::write(dir.join("epoch.log"), "1714573925 start\n1714573985 middle\n1714574045 end\n")?;
    std::fs::create_dir_all(dir.join("tree").join("sub"))?;
    std::fs::write(dir.join("tree").join("a.log"), "one\ntwo NEEDLE\n")?;
    std::fs::write(dir.join("tree").join("c.txt"), "NEEDLE in a .txt file\n")?;
    std::fs::write(dir.join("tree").join("sub").join("b.log"), "NEEDLE x\n")?;
    std::fs::write(dir.join("merge-a.log"), "2024-05-01 10:00:00 a1\n2024-05-01 10:00:02 a2\n  at x()\n")?;
    std::fs::write(dir.join("merge-b.log"), "2024-05-01 10:00:01 b1\r\n2024-05-01 10:00:03 b2\r\n")?;
    std::fs::write(dir.join("journal"), "Jan  1 00:00:01 host app[1]: started\nJan  1 00:00:02 host app[1]: ERROR boom\n")?;
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
    std::fs::write(dir.join("patch-me.txt"), &base)?;
    std::fs::write(dir.join("md.log"), "2024-05-01 10:00:00 INFO start\n2024-05-01 10:00:01 ERROR <boom>\n")?;
    std::fs::write(dir.join("inst.txt"), "handed over\nby a second launch\n")?;
    std::fs::write(dir.join("dups.txt"), "apple\npear\napple\nfig\napple\npear\n")?;
    let block: String = (0..10_000).map(|i| format!("k{:04}\n", (i * 7919) % 10_000)).collect();
    std::fs::write(dir.join("sort-big.txt"), block.repeat(1150))?;
    std::fs::write(dir.join("cols.txt"), "aaaa\nbbbb\ncccc\ndddd\n")?;
    std::fs::write(dir.join("codec.txt"), CODEC_TXT)?;
    std::fs::write(dir.join("big-case.txt"), b"abc def\n".repeat(9_000_000))?;
    std::fs::write(dir.join("drop.log"), "10:00 INFO a\n10:01 DEBUG b\n10:02 DEBUG c\n10:03 INFO d\n10:04 WARN e\n10:05 DEBUG f\n")?;
    std::fs::write(dir.join("journal.txt"), "first line\nsecond line\n")?;
    std::fs::write(dir.join("props.txt"), "one\r\ntwo\nthe longest line\nend")?;
    {
        use std::io::Write as _;
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&b"needs room\n".repeat(30_000))?;
        std::fs::write(dir.join("space.log.gz"), gz.finish()?)?;
        std::fs::write(dir.join("space-save.txt"), "save me\n".repeat(1000))?;
    }
    {
        use std::io::Write as _;
        let zip = |name: &str, files: &[(&str, Vec<u8>)]| -> std::io::Result<()> {
            let mut z = zip::ZipWriter::new(std::fs::File::create(dir.join(name))?);
            for (n, data) in files {
                z.start_file(*n, zip::write::SimpleFileOptions::default()).map_err(std::io::Error::other)?;
                z.write_all(data)?;
            }
            z.finish().map_err(std::io::Error::other)?;
            Ok(())
        };
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(b"packed line\n")?;
        zip("bundle.zip", &[("logs/app.log", b"one\ntwo\n".to_vec()), ("logs/old/app.log.gz", gz.finish()?), ("readme.txt", b"hi\n".to_vec())])?;
        zip("single.zip", &[("only.log", b"only\n".to_vec())])?;
        let mut xz = lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::with_preset(1)).map_err(std::io::Error::other)?;
        xz.write_all(b"from xz\n")?;
        std::fs::write(dir.join("small.log.xz"), xz.finish().map_err(std::io::Error::other)?)?;
    }
    std::fs::write(dir.join("rot.log"), (0..500).map(|i| format!("old line {i}\n")).collect::<String>())?;
    std::fs::write(dir.join("cmp-a.csv"), "id,name,qty\n1,apple,5\n2,banana,7\n3,cherry,9\n")?;
    std::fs::write(dir.join("cmp-b.csv"), "id,qty,name\n3,9,cherry\n1,6,apple\n4,1,date\n")?;
    let tasks = |h: u32, order: &mut dyn Iterator<Item = u32>| order.map(|i| format!("2024-05-0{} {h:02}:{:02}:{:02} INFO task {i} done\n", h / 10, i / 60, i % 60)).collect::<String>();
    std::fs::write(dir.join("only-a.log"), tasks(10, &mut (0..50)) + "2024-05-01 10:01:00 ERROR disk full\n")?;
    std::fs::write(dir.join("only-b.log"), tasks(23, &mut (0..50).rev()) + "2024-05-02 23:59:00 WARN slow\n")?;
    std::fs::write(dir.join("diff-b.txt"), base.replace("row 100\n", "row one hundred\n").replace("row 1500\n", "row 1500\nextra line\n"))?;
    let times = |h: u32| (0..200).map(|i| format!("2024-05-01 {h:02}:{:02}:{:02} INFO step {i} req=r{}\n", i / 60, i % 60, i * h)).collect::<String>();
    std::fs::write(dir.join("times-a.log"), times(9))?;
    std::fs::write(dir.join("times-b.log"), times(14))?;
    std::fs::write(dir.join("cmp-saved.txt"), (0..300).map(|i| format!("entry {i}\n")).collect::<String>())?;
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
        s("Ctrl+Shift+D duplicates line, Ctrl+Shift+K deletes it", Box::new(|app, c| {
            if c.first() {
                let p = pos(app, 3, 2.0);
                c.click(p, NONE);
                c.key(Key::D, CTRL_SHIFT);
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
        s("interface in Russian and Uzbek", Box::new(|app, c| {
            use crate::app::i18n::{lang, set_lang, tr, Lang};
            if c.first() {
                set_lang(Lang::Ru);
                app.search.status = tr("Wrapped around").into();
                return Step::Wait;
            }
            if c.frames < 4 {
                return Step::Wait;
            }
            if lang() == Lang::Ru {
                let ok = tr("File") == "Файл" && app.search.status == "Поиск начат сначала";
                if !ok {
                    return Step::Fail(format!("status {:?}", app.search.status));
                }
                set_lang(Lang::Uz);
                return Step::Wait;
            }
            let uz = tr("Search") == "Qidiruv";
            set_lang(Lang::En);
            app.search.status.clear();
            check(uz, "Uzbek texts missing")
        }));
        s("font: an installed monospace font, then the built-in one", Box::new(|app, c| {
            if c.first() {
                let Some((_, path)) = crate::app::fonts::system_fonts().into_iter().next() else { return Step::Skip("no monospace font found".into()) };
                app.test_set_font(Some(path));
                return Step::Wait;
            }
            if c.frames < 4 {
                return Step::Wait;
            }
            let ok = app.settings.font_path.is_some() && app.fonts.error.is_none();
            let err = app.fonts.error.clone();
            app.test_set_font(None);
            check(ok && app.settings.font_path.is_none(), format!("font error {err:?}"))
        }));
        s("command palette (Ctrl+Shift+P) runs a command", Box::new(|app, c| {
            if c.first() {
                c.key(Key::P, CTRL_SHIFT);
                c.push(vec![], NONE);
                c.typed("word wrap");
                c.key(Key::Enter, NONE);
                return Step::Wait;
            }
            if !doc(app).view.wrap {
                return if c.waited() > Duration::from_secs(3) { Step::Fail(format!("palette open {}", app.palette.is_some())) } else { Step::Wait };
            }
            doc(app).view.wrap = false;
            check(app.palette.is_none(), "palette still open")
        }));
        s("status bar: lines, characters and bytes of the selection", Box::new(|app, _c| {
            doc(app).select(0, 35);
            let two_lines = doc(app).selection_stats();
            doc(app).select(6, 10);
            let word = doc(app).selection_stats();
            doc(app).select(0, 0);
            let none = doc(app).selection_stats();
            check(two_lines == Some((2, true, Some(35), 35)) && word == Some((1, true, Some(4), 4)) && none.is_none(), format!("{two_lines:?} {word:?} {none:?}"))
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
        s("go to: lines from the caret, from the end, and a share of the file", Box::new(|app, _c| {
            use crate::app::goto::parse;
            let d = doc(app);
            let mut at = |t: &str, offset: bool| {
                d.goto_target(parse(t, offset).expect("parses"));
                (d.buf.line_of(d.view.caret).0, d.view.caret)
            };
            at("2", false);
            let plus = at("+2", false).0;
            let minus = at("-1", false).0;
            let last = at("$", false).0;
            let before = at("$-1", false).0;
            let (_, half) = at("50%", false);
            let len = d.buf.len();
            let (count, _) = d.buf.line_count();
            let line_start = half == 0 || d.buf.read(half - 1..half).is_some_and(|b| b[0] == b'\n');
            let ok = plus == 3 && minus == 2 && last == count - 1 && before == count - 2 && line_start && half <= len / 2 + 1;
            check(ok, format!("+2 → {plus}, -1 → {minus}, $ → {last}, $-1 → {before} of {count}, 50% → {half} of {len}"))
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
        s("follow reads the file in chunks, not mapped", Box::new(|app, c| {
            if c.first() {
                app.open_path(c.file("rot.log"));
                return Step::Wait;
            }
            if c.mark == 0 {
                if !wait_ready(app) {
                    return Step::Wait;
                }
                doc(app).follow = true;
                c.mark = 1;
                return Step::Wait;
            }
            let d = doc(app);
            let kind = d.buf.source.kind_name();
            if kind != "chunked" || d.reloading || !d.is_ready() {
                return if c.waited() > Duration::from_secs(5) { Step::Fail(format!("backing {kind}")) } else { Step::Wait };
            }
            check(!d.buf.is_empty() && d.follow, "following")
        }));
        s("follow: a log truncated and written again (copytruncate) is followed from its end", Box::new(|app, c| {
            if c.first() {
                if let Err(e) = std::fs::write(c.file("rot.log"), "fresh 1\nfresh 2\n") {
                    return Step::Fail(format!("could not truncate the followed file: {e}"));
                }
                return Step::Wait;
            }
            let t = text(app);
            let d = doc(app);
            if t != "fresh 1\nfresh 2\n" || d.rotations == 0 || d.reloading {
                return if c.waited() > Duration::from_secs(8) { Step::Fail(format!("rotations {}, text {:?}", doc(app).rotations, t)) } else { Step::Wait };
            }
            let d = doc(app);
            check(d.view.caret == d.buf.len() && matches!(d.banner, Some(crate::app::document::Banner::Rotated(_))), format!("caret {} of {}, banner {:?}", d.view.caret, d.buf.len(), d.banner.is_some()))
        }));
        s("follow: a log renamed away and created again is followed in the new file", Box::new(|app, c| {
            if c.first() {
                doc(app).banner = None;
                let r = std::fs::rename(c.file("rot.log"), c.file("rot.log.1")).and_then(|_| std::fs::write(c.file("rot.log"), "new file line\n"));
                if let Err(e) = r {
                    return Step::Fail(format!("could not rotate the followed file: {e}"));
                }
                return Step::Wait;
            }
            let t = text(app);
            let d = doc(app);
            let to = match &d.banner {
                Some(crate::app::document::Banner::Rotated(to)) => to.clone(),
                _ => None,
            };
            if t != "new file line\n" || to.is_none() || d.reloading {
                return if c.waited() > Duration::from_secs(8) { Step::Fail(format!("text {t:?}, rotated to {to:?}")) } else { Step::Wait };
            }
            let named = to.as_deref().and_then(|p| p.file_name()).is_some_and(|n| n == "rot.log.1");
            d.follow = false;
            check(named && d.rotations >= 2, format!("rotated to {to:?}, rotations {}", d.rotations))
        }));
        s("an archive lists its files; the one picked (a .gz inside) opens unpacked", Box::new(|app, c| {
            if c.first() {
                app.open_path(c.file("bundle.zip"));
                return Step::Wait;
            }
            let waited = c.waited() > Duration::from_secs(8);
            if c.mark == 0 {
                let n = app.archive_picker.as_ref().map(|p| p.members.len());
                if n != Some(3) {
                    return if waited { Step::Fail(format!("picker {n:?}")) } else { Step::Wait };
                }
                app.pick_archive_member("logs/old/app.log.gz");
                c.mark = 1;
                return Step::Wait;
            }
            let Some(i) = app.docs.iter().position(|d| d.title == "bundle.zip › app.log.gz" && d.is_ready()) else {
                return if waited { Step::Fail(format!("tabs {:?}", app.docs.iter().map(|d| d.title.clone()).collect::<Vec<_>>())) } else { Step::Wait };
            };
            app.active = i;
            let d = doc(app);
            let (ro, temp, member) = (d.read_only, d.temporary, d.member.clone());
            let t = text(app);
            check(t == "packed line\n" && ro && temp && member.as_deref() == Some("logs/old/app.log.gz"), format!("text {t:?}, read-only {ro}, temporary {temp}, member {member:?}"))
        }));
        s("a zip with one file and an .xz file open straight away", Box::new(|app, c| {
            if c.first() {
                app.open_path(c.file("single.zip"));
                app.open_path(c.file("small.log.xz"));
                return Step::Wait;
            }
            let text_of = |app: &BigTextApp, title: &str| {
                app.docs.iter().find(|d| d.title == title && d.is_ready()).and_then(|d| d.buf.read(0..d.buf.len())).map(|b| String::from_utf8_lossy(&b).into_owned())
            };
            let (zip, xz) = (text_of(app, "single.zip › only.log"), text_of(app, "small.log.xz"));
            if zip.is_none() || xz.is_none() {
                return if c.waited() > Duration::from_secs(8) { Step::Fail(format!("zip {zip:?}, xz {xz:?}")) } else { Step::Wait };
            }
            check(zip.as_deref() == Some("only\n") && xz.as_deref() == Some("from xz\n") && app.archive_picker.is_none(), format!("zip {zip:?}, xz {xz:?}"))
        }));
        s("low free space: unpacking asks first, Continue goes on", Box::new(|app, c| {
            if c.first() {
                std::env::set_var("BIGTEXT_FAKE_FREE", "1000");
                app.open_path(c.file("space.log.gz"));
                return Step::Wait;
            }
            let waited = c.waited() > Duration::from_secs(8);
            if c.mark == 0 {
                if !app.test_has_confirm() {
                    return if waited { Step::Fail("no low-space question".into()) } else { Step::Wait };
                }
                app.test_accept_confirm();
                c.mark = 1;
                return Step::Wait;
            }
            if !wait_ready(app) {
                return if waited { Step::Fail("the copy did not open after Continue".into()) } else { Step::Wait };
            }
            std::env::remove_var("BIGTEXT_FAKE_FREE");
            let n = doc(app).buf.len();
            check(n == 330_000 && doc(app).unpacked_from.is_some(), format!("{n} bytes"))
        }));
        s("low free space: saving asks first, Continue saves", Box::new(|app, c| {
            if c.first() {
                app.open_path(c.file("space-save.txt"));
                return Step::Wait;
            }
            let waited = c.waited() > Duration::from_secs(8);
            match c.mark {
                0 => {
                    if !wait_ready(app) {
                        return Step::Wait;
                    }
                    std::env::set_var("BIGTEXT_FAKE_FREE", "1000");
                    c.key(Key::Home, CTRL);
                    c.typed("x");
                    c.key(Key::S, CTRL);
                    c.mark = 1;
                    Step::Wait
                }
                1 => {
                    if !app.test_has_confirm() {
                        return if waited { Step::Fail("no low-space question".into()) } else { Step::Wait };
                    }
                    std::env::remove_var("BIGTEXT_FAKE_FREE");
                    app.test_accept_confirm();
                    c.mark = 2;
                    Step::Wait
                }
                _ => {
                    let d = doc(app);
                    if d.save_job.is_some() || d.is_modified() || !d.is_ready() {
                        return if waited { Step::Fail("the save did not finish after Continue".into()) } else { Step::Wait };
                    }
                    let disk = std::fs::read_to_string(c.file("space-save.txt")).unwrap_or_default();
                    check(disk.starts_with("xsave me\n"), format!("disk starts {:?}", &disk[..disk.len().min(12)]))
                }
            }
        }));
        s("File → Properties counts lines and breaks and hashes the file", Box::new(|app, c| {
            if c.first() {
                app.open_path(c.file("props.txt"));
                return Step::Wait;
            }
            let waited = c.waited() > Duration::from_secs(8);
            if c.mark == 0 {
                if !wait_ready(app) {
                    return Step::Wait;
                }
                c.key(Key::I, CTRL);
                c.mark = 1;
                return Step::Wait;
            }
            let Some(p) = &app.properties else {
                return if waited { Step::Fail("Ctrl+I did not open the properties".into()) } else { Step::Wait };
            };
            let (st, sha) = {
                let g = p.data.lock().unwrap();
                (g.stats, g.sha.clone())
            };
            let Some(st) = st.filter(|s| s.done) else { return if waited { Step::Fail("no figures".into()) } else { Step::Wait } };
            if c.mark == 1 {
                if (st.lines, st.crlf, st.lf, st.longest.0, st.longest.1, st.final_break) != (4, 1, 2, 16, 2, false) {
                    return Step::Fail(format!("{st:?}"));
                }
                c.mark = 2;
                return Step::Wait;
            }
            if c.mark == 2 {
                // The Compute button.
                app.test_properties_sha();
                c.mark = 3;
                return Step::Wait;
            }
            let Some(h) = sha else { return if waited { Step::Fail("no SHA-256".into()) } else { Step::Wait } };
            use sha2::Digest;
            let want: String = sha2::Sha256::digest(std::fs::read(c.file("props.txt")).unwrap_or_default()).iter().map(|b| format!("{b:02x}")).collect();
            app.properties = None;
            check(h == want, format!("{h} != {want}"))
        }));
        s("unsaved edits are journaled after a pause, and the journal goes once they are undone", Box::new(|app, c| {
            let dir = c.dir.join("recovery");
            if c.first() {
                app.start_recovery(Some(dir.clone()));
                app.recovery.enabled = true;
                app.open_path(c.file("journal.txt"));
                return Step::Wait;
            }
            let waited = c.waited() > Duration::from_secs(8);
            let id = format!("{}-{}", std::process::id(), doc(app).id);
            let pieces = dir.join(format!("{id}.pieces"));
            match c.mark {
                0 => {
                    if !wait_ready(app) {
                        return Step::Wait;
                    }
                    c.key(Key::Home, CTRL);
                    c.typed("abc");
                    c.mark = 1;
                    Step::Wait
                }
                1 => {
                    if !pieces.exists() {
                        return if waited { Step::Fail("no journal after the pause".into()) } else { Step::Wait };
                    }
                    let add = std::fs::read(dir.join(format!("{id}.add"))).unwrap_or_default();
                    if add != b"abc" {
                        return Step::Fail(format!("journaled bytes {add:?}"));
                    }
                    c.key(Key::Z, CTRL);
                    c.mark = 2;
                    Step::Wait
                }
                _ => {
                    if doc(app).is_modified() || pieces.exists() {
                        return if waited { Step::Fail("the journal stayed after the undo".into()) } else { Step::Wait };
                    }
                    Step::Next
                }
            }
        }));
        s("a journal a crashed BigText left is offered and puts the edits back", Box::new(|app, c| {
            use crate::app::recovery::{self, Journal};
            let dir = c.dir.join("recovery");
            if c.first() {
                let path = c.file("journal.txt");
                let m = std::fs::metadata(&path).unwrap();
                let j = Journal {
                    id: "999999-7".into(),
                    title: "journal.txt".into(),
                    path: Some(path.clone()),
                    len: m.len(),
                    mtime: m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()),
                    codec: bigtext::encoding::Codec::utf8(),
                    pieces: vec![(bigtext::text::Buf::Add, 0, 4), (bigtext::text::Buf::Orig, 0, m.len())],
                    add_len: 4,
                    saved: None,
                };
                let _ = std::fs::write(dir.join("999999-7.add"), b"NEW ");
                let _ = std::fs::write(dir.join("999999-7.pieces"), recovery::format(&j));
                app.scan_recovery();
                let Some(i) = app.recovery.offered.iter().position(|j| j.id == "999999-7") else {
                    return Step::Fail(format!("not offered: {:?}", app.recovery.offered.iter().map(|j| j.id.clone()).collect::<Vec<_>>()));
                };
                app.restore_journal(i);
                return Step::Wait;
            }
            let t = text(app);
            let d = doc(app);
            if t != "NEW first line\nsecond line\n" || !d.is_modified() {
                return if c.waited() > Duration::from_secs(5) { Step::Fail(format!("text {t:?}")) } else { Step::Wait };
            }
            let gone = !dir.join("999999-7.pieces").exists();
            let id = d.id;
            app.test_close(id);
            check(gone && app.recovery.offered.is_empty(), format!("journal removed {gone}"))
        }));
        s("filter DEBUG, delete these lines in place (one undo), then keep only them", Box::new(|app, c| {
            if c.first() {
                app.open_path(c.file("drop.log"));
                return Step::Wait;
            }
            let waited = c.waited() > Duration::from_secs(8);
            let filtered = |app: &mut BigTextApp| doc(app).filter.as_ref().is_some_and(|r| r.data.lock().unwrap().done);
            match c.mark {
                0 => {
                    if !wait_ready(app) {
                        return Step::Wait;
                    }
                    app.search.query = bigtext::search::Query { pattern: "DEBUG".into(), case_sensitive: true, ..Default::default() };
                    app.search.invert = false;
                    app.start_filter();
                    c.mark = 1;
                    Step::Wait
                }
                1 => {
                    if !filtered(app) {
                        return if waited { Step::Fail("the filter did not finish".into()) } else { Step::Wait };
                    }
                    app.remove_filtered_lines(false);
                    c.mark = 2;
                    Step::Wait
                }
                2 => {
                    let t = text(app);
                    if t != "10:00 INFO a\n10:03 INFO d\n10:04 WARN e\n" {
                        return if waited { Step::Fail(format!("after delete {t:?}")) } else { Step::Wait };
                    }
                    c.key(Key::Z, CTRL);
                    c.mark = 3;
                    Step::Wait
                }
                3 => {
                    if doc(app).is_modified() {
                        return if waited { Step::Fail("undo did not bring the lines back".into()) } else { Step::Wait };
                    }
                    app.start_filter();
                    c.mark = 4;
                    Step::Wait
                }
                4 => {
                    if !filtered(app) {
                        return if waited { Step::Fail("the filter did not finish".into()) } else { Step::Wait };
                    }
                    app.remove_filtered_lines(true);
                    c.mark = 5;
                    Step::Wait
                }
                _ => {
                    let t = text(app);
                    if t != "10:01 DEBUG b\n10:02 DEBUG c\n10:05 DEBUG f\n" {
                        return if waited { Step::Fail(format!("after keep {t:?}")) } else { Step::Wait };
                    }
                    c.key(Key::Z, CTRL);
                    app.search.show_filter = false;
                    app.search.query = bigtext::search::Query::default();
                    Step::Next
                }
            }
        }));
        s("UPPERCASE on 72 MB: asks, then rewrites the file on disk", Box::new(|app, c| {
            if c.first() {
                app.open_path(c.file("big-case.txt"));
                return Step::Wait;
            }
            let waited = c.waited() > Duration::from_secs(30);
            match c.mark {
                0 => {
                    if !wait_ready(app) {
                        return Step::Wait;
                    }
                    app.text_op(bigtext::textops::Op::Upper);
                    c.mark = 1;
                    Step::Wait
                }
                1 => {
                    if !app.test_has_confirm() {
                        return if waited { Step::Fail("no question before changing on disk".into()) } else { Step::Wait };
                    }
                    app.test_accept_confirm();
                    c.mark = 2;
                    Step::Wait
                }
                _ => {
                    let d = doc(app);
                    let busy = d.save_job.is_some() || !d.is_ready();
                    let head = d.buf.read(0..16).map(|b| b.to_vec()).unwrap_or_default();
                    if busy || head != b"ABC DEF\nABC DEF\n" {
                        return if waited { Step::Fail(format!("head {:?}", String::from_utf8_lossy(&head))) } else { Step::Wait };
                    }
                    let len = d.buf.len();
                    let disk = std::fs::metadata(c.file("big-case.txt")).map(|m| m.len()).unwrap_or(0);
                    let id = d.id;
                    app.test_close(id);
                    check(len == 72_000_000 && disk == len, format!("{len} bytes, {disk} on disk"))
                }
            }
        }));
        s("Base64 decode the word at the caret, then encode the result back", Box::new(|app, c| {
            use bigtext::textops::Op;
            if c.first() {
                app.open_path(c.file("codec.txt"));
                return Step::Wait;
            }
            let waited = c.waited() > Duration::from_secs(5);
            match c.mark {
                0 => {
                    if !wait_ready(app) {
                        return Step::Wait;
                    }
                    doc(app).set_caret(10, false);
                    app.text_op(Op::B64Decode);
                    c.mark = 1;
                    Step::Wait
                }
                1 => {
                    let t = text(app);
                    if !t.starts_with("data: hello world\n") {
                        return if waited { Step::Fail(format!("decoded {:?}", &t[..t.len().min(30)])) } else { Step::Wait };
                    }
                    // The decoded text stays selected.
                    app.text_op(Op::B64Encode);
                    c.mark = 2;
                    Step::Wait
                }
                _ => {
                    let t = text(app);
                    if t != CODEC_TXT {
                        return if waited { Step::Fail(format!("encoded {:?}", &t[..t.len().min(30)])) } else { Step::Wait };
                    }
                    Step::Next
                }
            }
        }));
        s("a JWT and bytes that are not text are shown, not put in the document", Box::new(|app, c| {
            use bigtext::textops::Op;
            if c.first() {
                app.text_result = None;
                let at = CODEC_TXT.find("eyJzdWI").unwrap_or(0) as u64;
                doc(app).set_caret(at, false);
                app.text_op(Op::B64Decode);
                return Step::Wait;
            }
            let waited = c.waited() > Duration::from_secs(5);
            if c.mark == 0 {
                let Some(r) = &app.text_result else { return if waited { Step::Fail("no JWT window".into()) } else { Step::Wait } };
                if !(r.text.contains("\"name\": \"Ann\"") && r.text.contains("\"alg\": \"HS256\"") && !r.binary) {
                    return Step::Fail(r.text.clone());
                }
                app.text_result = None;
                let at = CODEC_TXT.find("AAEC").unwrap_or(0) as u64 + 1;
                doc(app).set_caret(at, false);
                app.text_op(Op::B64Decode);
                c.mark = 1;
                return Step::Wait;
            }
            let Some(r) = &app.text_result else { return if waited { Step::Fail("no window for the bytes".into()) } else { Step::Wait } };
            let ok = r.binary && r.bytes == [0, 1, 2] && r.text.starts_with("00000000  00 01 02");
            let unchanged = text(app) == CODEC_TXT;
            app.text_result = None;
            check(ok && unchanged, format!("result ok {ok}, document unchanged {unchanged}"))
        }));
        s("Shift+Alt+arrows make a block from the keyboard; typing fills every row", Box::new(|app, c| {
            const SHIFT_ALT: Modifiers = Modifiers { alt: true, ctrl: false, shift: true, mac_cmd: false, command: false };
            if c.first() {
                app.open_path(c.file("cols.txt"));
                return Step::Wait;
            }
            if c.mark == 0 {
                if !wait_ready(app) {
                    return Step::Wait;
                }
                doc(app).set_caret(1, false);
                c.key(Key::ArrowDown, SHIFT_ALT);
                c.key(Key::ArrowDown, SHIFT_ALT);
                c.key(Key::ArrowRight, SHIFT_ALT);
                c.typed("X");
                c.mark = 1;
                return Step::Wait;
            }
            let t = text(app);
            if t != "aXaa\nbXbb\ncXcc\ndddd\n" {
                return if c.waited() > Duration::from_secs(3) { Step::Fail(format!("{t:?}, block {:?}", doc(app).view.block)) } else { Step::Wait };
            }
            Step::Next
        }));
        s("Column editor: numbers on the block's rows, then a column down to the end on disk", Box::new(|app, c| {
            use bigtext::column_edit::ColumnGen;
            let waited = c.waited() > Duration::from_secs(8);
            match c.mark {
                0 => {
                    if !app.apply_column_edit(ColumnGen::Numbers { start: 1, step: 1, width: 2, hex: false }, false) {
                        return Step::Fail("no block to fill".into());
                    }
                    c.mark = 1;
                    Step::Wait
                }
                1 => {
                    let t = text(app);
                    if t != "aX01aa\nbX02bb\ncX03cc\ndddd\n" {
                        return if waited { Step::Fail(format!("numbered {t:?}")) } else { Step::Wait };
                    }
                    doc(app).view.block = None;
                    doc(app).set_caret(7, false);
                    app.apply_column_edit(ColumnGen::Text(">".into()), true);
                    c.mark = 2;
                    Step::Wait
                }
                2 => {
                    if !app.test_has_confirm() {
                        return if waited { Step::Fail("no question before rewriting".into()) } else { Step::Wait };
                    }
                    app.test_accept_confirm();
                    c.mark = 3;
                    Step::Wait
                }
                _ => {
                    let disk = std::fs::read_to_string(c.file("cols.txt")).unwrap_or_default();
                    let d = doc(app);
                    if disk != "aX01aa\n>bX02bb\n>cX03cc\n>dddd\n" || d.save_job.is_some() || !d.is_ready() {
                        return if waited { Step::Fail(format!("disk {disk:?}")) } else { Step::Wait };
                    }
                    Step::Next
                }
            }
        }));
        s("Count duplicate lines opens a table, most frequent first", Box::new(|app, c| {
            if c.first() {
                app.open_path(c.file("dups.txt"));
                return Step::Wait;
            }
            if c.mark == 0 {
                if !wait_ready(app) {
                    return Step::Wait;
                }
                app.text_op(bigtext::textops::Op::CountDuplicates);
                c.mark = 1;
                return Step::Wait;
            }
            let Some(i) = app.docs.iter().position(|d| d.title == "Line counts of dups.txt" && d.is_ready()) else {
                return if c.waited() > Duration::from_secs(8) { Step::Fail("no counts table".into()) } else { Step::Wait };
            };
            app.active = i;
            let t = text(app);
            check(t == "count,line\n3,apple\n2,pear\n1,fig\n" && doc(app).table, format!("{t:?}"))
        }));
        s("Remove duplicate lines on 69 MB: sorted on disk through temporary runs", Box::new(|app, c| {
            if c.first() {
                app.open_path(c.file("sort-big.txt"));
                return Step::Wait;
            }
            let waited = c.waited() > Duration::from_secs(60);
            match c.mark {
                0 => {
                    if !wait_ready(app) {
                        return Step::Wait;
                    }
                    app.text_op(bigtext::textops::Op::Unique);
                    c.mark = 1;
                    Step::Wait
                }
                1 => {
                    if !app.test_has_confirm() {
                        return if waited { Step::Fail("no question before sorting on disk".into()) } else { Step::Wait };
                    }
                    app.test_accept_confirm();
                    c.mark = 2;
                    Step::Wait
                }
                _ => {
                    let d = doc(app);
                    if d.save_job.is_some() || !d.is_ready() || d.buf.len() != 60_000 {
                        return if waited { Step::Fail(format!("{} bytes", doc(app).buf.len())) } else { Step::Wait };
                    }
                    let disk = std::fs::read_to_string(c.file("sort-big.txt")).unwrap_or_default();
                    let want: String = (0..10_000).map(|i| format!("k{i:04}\n")).collect();
                    let id = d.id;
                    app.test_close(id);
                    check(disk == want, format!("{} bytes on disk", disk.len()))
                }
            }
        }));
        s("a second launch opens its file here, at its line", Box::new({
            let mut child: Option<std::process::Child> = None;
            move |app, c| {
                let dir = c.dir.join("instance");
                if c.first() {
                    app.start_instance_server(dir.join("instance.lock"));
                    let arg = format!("{}:2", c.file("inst.txt").display());
                    match std::process::Command::new(std::env::current_exe().unwrap_or_default()).arg(arg).env("BIGTEXT_INSTANCE_DIR", &dir).env("BIGTEXT_NO_STATE", "1").spawn() {
                        Ok(ch) => child = Some(ch),
                        Err(e) => return Step::Fail(format!("could not start a second BigText: {e}")),
                    }
                    return Step::Wait;
                }
                let exited = child.as_mut().and_then(|ch| ch.try_wait().ok().flatten());
                let opened = app.docs.iter().position(|d| d.title == "inst.txt" && d.is_ready());
                if exited.is_none() || opened.is_none() {
                    if c.waited() > Duration::from_secs(15) {
                        if let Some(ch) = child.as_mut() {
                            let _ = ch.kill();
                        }
                        return Step::Fail(format!("second launch exited {exited:?}, tab opened {}", opened.is_some()));
                    }
                    return Step::Wait;
                }
                let i = opened.unwrap_or(0);
                app.active = i;
                let d = doc(app);
                let line = d.buf.line_of(d.view.caret).0;
                check(exited.is_some_and(|s| s.success()) && line == 1, format!("exit {exited:?}, caret line {line}"))
            }
        }));
        s("Show in folder points at the file, or the archive an unpacked copy came from", Box::new(|app, c| {
            if c.first() {
                app.open_path(c.file("packed.log.gz"));
                return Step::Wait;
            }
            if !wait_ready(app) {
                return if c.waited() > Duration::from_secs(5) { Step::Fail("the .gz did not open".into()) } else { Step::Wait };
            }
            let id = doc(app).id;
            let p = app.shell_path(id);
            let cmd = p.as_deref().map(|p| crate::app::shell::command(crate::app::shell::ShellAction::Reveal, p));
            let args: Vec<String> = cmd.as_ref().map(|c| c.get_args().map(|a| a.to_string_lossy().into_owned()).collect()).unwrap_or_default();
            app.test_close(id);
            let ok = p.as_deref().is_some_and(|p| p.ends_with("packed.log.gz")) && (!cfg!(windows) || args.first().is_some_and(|a| a.starts_with("/select,") && a.contains("packed.log.gz")));
            check(ok, format!("{p:?} {args:?}"))
        }));
        s("Ctrl+Shift+V lists what was copied; Down, Enter pastes the one before", Box::new(|app, c| {
            if c.first() {
                app.clips.items.clear();
                app.open_path(c.file("cols.txt"));
                return Step::Wait;
            }
            match c.mark {
                0 => {
                    if !wait_ready(app) {
                        return Step::Wait;
                    }
                    if let Some(ctx) = app.test_ctx() {
                        ctx.copy_text("first clip".into());
                        ctx.copy_text("second clip".into());
                    }
                    c.mark = 1;
                    Step::Wait
                }
                1 => {
                    if app.clips.items.len() < 2 {
                        return if c.waited() > Duration::from_secs(3) { Step::Fail(format!("history {:?}", app.clips.items)) } else { Step::Wait };
                    }
                    doc(app).set_caret(0, false);
                    c.key(Key::V, CTRL_SHIFT);
                    c.key(Key::ArrowDown, NONE);
                    c.key(Key::Enter, NONE);
                    c.mark = 2;
                    Step::Wait
                }
                _ => {
                    let t = text(app);
                    if !t.starts_with("first clip") {
                        return if c.waited() > Duration::from_secs(3) { Step::Fail(format!("{:?}, window {:?}", &t[..t.len().min(20)], app.clips.open)) } else { Step::Wait };
                    }
                    c.key(Key::Z, CTRL);
                    check(app.clips.open.is_none() && app.clips.items[0] == "first clip", format!("{:?}", app.clips.items))
                }
            }
        }));
        s("Copy as Markdown: a fenced log block captioned file:line", Box::new(|app, c| {
            if c.first() {
                app.open_path(c.file("md.log"));
                return Step::Wait;
            }
            if c.mark == 0 {
                if !wait_ready(app) {
                    return Step::Wait;
                }
                let len = doc(app).buf.len();
                doc(app).select(0, len);
                c.copied.clear();
                if let Some(ctx) = app.test_ctx() {
                    app.copy_as(&ctx, crate::app::copy_as::CopyAs::Markdown);
                }
                c.mark = 1;
                return Step::Wait;
            }
            let Some(got) = c.copied.last().cloned() else {
                return if c.waited() > Duration::from_secs(3) { Step::Fail("nothing copied".into()) } else { Step::Wait };
            };
            check(got == "md.log:1\n```log\n2024-05-01 10:00:00 INFO start\n2024-05-01 10:00:01 ERROR <boom>\n```\n", format!("{got:?}"))
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
            check(got == vec![(100, 1, 100, 1), (1501, 0, 1501, 1)], format!("{got:?}"))
        }));
        s("F7 / Shift+F7 step through the differences, around the ends", Box::new(|app, c| {
            if c.first() {
                doc(app).set_caret(0, false);
                return Step::Wait;
            }
            let caret = doc(app).view.caret;
            let line = doc(app).buf.line_of(caret).0;
            let want = [None, Some(100), Some(1501), Some(100), Some(1501)];
            if let Some(w) = want[c.mark as usize] {
                if line != w {
                    return if c.waited() > Duration::from_secs(3) { Step::Fail(format!("stage {}: line {line}, want {w}", c.mark)) } else { Step::Wait };
                }
            }
            if c.mark as usize == want.len() - 1 {
                c.mark = 0;
                app.diff = None;
                return Step::Next;
            }
            c.key(Key::F7, if c.mark == 3 { SHIFT } else { NONE });
            c.mark += 1;
            Step::Wait
        }));
        s("side by side: the other file opens beside, its lines marked, scrolling along", Box::new(|app, c| {
            if c.first() {
                app.open_path(c.file("diff-a.txt"));
                app.side_by_side = true;
                app.start_diff(c.file("diff-b.txt"));
                return Step::Wait;
            }
            let waited = c.waited() > Duration::from_secs(5);
            let Some(r) = &app.diff else { return Step::Fail("no comparison".into()) };
            let done = r.data.lock().unwrap().done;
            let b_ready = r.side.as_ref().is_some_and(|s| s.doc.is_ready());
            if !done || !b_ready {
                return if waited { Step::Fail(format!("done {done}, B ready {b_ready}")) } else { Step::Wait };
            }
            let b_top = r.side.as_ref().map(|s| s.doc.buf.line_of(s.doc.view.top).0).unwrap_or(0);
            let spans = crate::app::extras::diff_spans(&r.data.lock().unwrap().hunks, 0..u64::MAX, false).len();
            if c.mark == 0 {
                let Some((off, true)) = doc(app).buf.line_start(1600) else { return Step::Wait };
                doc(app).view.top = off;
                doc(app).view.scroll_to_caret = false;
                c.mark = 1;
                return Step::Wait;
            }
            if b_top != 1601 && !waited {
                return Step::Wait;
            }
            app.side_by_side = false;
            app.diff = None;
            check(b_top == 1601 && spans == 2, format!("B top line {b_top}, {spans} spans in B"))
        }));
        s("compare two tables by key: changed, removed and added rows open as a table", Box::new(|app, c| {
            if c.first() {
                app.open_path(c.file("cmp-a.csv"));
                return Step::Wait;
            }
            // A table's dialect is known once it has been shown.
            let id = |app: &BigTextApp, name: &str| app.docs.iter().find(|d| d.title == name && d.is_ready() && d.table_state.dialect.is_some()).map(|d| d.id);
            if c.mark == 0 {
                if id(app, "cmp-a.csv").is_none() {
                    return if c.waited() > Duration::from_secs(5) { Step::Fail("cmp-a.csv did not open as a table".into()) } else { Step::Wait };
                }
                app.open_path(c.file("cmp-b.csv"));
                c.mark = 1;
                return Step::Wait;
            }
            if c.mark == 1 {
                let (Some(a), Some(b)) = (id(app, "cmp-a.csv"), id(app, "cmp-b.csv")) else {
                    return if c.waited() > Duration::from_secs(5) { Step::Fail("cmp-b.csv did not open as a table".into()) } else { Step::Wait };
                };
                app.start_table_compare(crate::app::table_compare::CompareTablesDialog { a, key_a: 0, b, key_b: 0, all: false });
                c.mark = 2;
                return Step::Wait;
            }
            let Some(i) = app.docs.iter().position(|d| d.title == "cmp-a.csv vs cmp-b.csv" && d.is_ready()) else {
                return if c.waited() > Duration::from_secs(10) { Step::Fail("no result table".into()) } else { Step::Wait };
            };
            app.active = i;
            let want = "status,id,changed,name,qty\nchanged,1,qty,apple,5 → 6\nremoved,2,,banana,7\nadded,4,,date,1\n";
            let (got, table) = (text(app), doc(app).table);
            check(got == want && table, format!("table {table}: {got:?}"))
        }));
        s("lines only in one log, timestamps ignored, whatever the order", Box::new(|app, c| {
            if c.first() {
                app.open_path(c.file("only-a.log"));
                app.open_path(c.file("only-b.log"));
                return Step::Wait;
            }
            let id = |app: &BigTextApp, name: &str| app.docs.iter().find(|d| d.title == name && d.is_ready()).map(|d| d.id);
            if c.mark == 0 {
                let (Some(a), Some(b)) = (id(app, "only-a.log"), id(app, "only-b.log")) else { return Step::Wait };
                app.start_only_in(a, b, crate::app::extras::DiffSwitches { timestamps: true, ..Default::default() });
                c.mark = 1;
                return Step::Wait;
            }
            let Some(r) = &app.only_in else { return Step::Fail("no results".into()) };
            let g = r.data.lock().unwrap();
            if !g.done {
                return Step::Wait;
            }
            let (counts, lines) = (g.counts, g.lines.clone());
            drop(g);
            let text = app.docs.iter().find(|d| d.id == r.doc).and_then(|d| lines.first().and_then(|&o| d.buf.read(o..o + 35))).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
            app.only_in = None;
            let ok = counts == Some(bigtext::setdiff::Counts { only: 1, common: 50 }) && lines.len() == 1 && text.starts_with("2024-05-01 10:01:00 ERROR disk full");
            check(ok, format!("{counts:?}, {} lines, first {text:?}", lines.len()))
        }));
        s("save the comparison as a .diff and apply it to a copy: it becomes B", Box::new(|app, c| {
            if c.first() {
                let _ = std::fs::remove_file(c.file("a-b.diff"));
                app.open_path(c.file("diff-a.txt"));
                app.start_diff(c.file("diff-b.txt"));
                return Step::Wait;
            }
            let waited = c.waited() > Duration::from_secs(10);
            match c.mark {
                0 => {
                    if !app.diff.as_ref().is_some_and(|r| r.data.lock().unwrap().done) {
                        return if waited { Step::Fail("the comparison did not finish".into()) } else { Step::Wait };
                    }
                    app.export_diff(c.file("a-b.diff"));
                    c.mark = 1;
                    Step::Wait
                }
                1 => {
                    if !c.file("a-b.diff").exists() || !app.tasks.is_empty() {
                        return if waited { Step::Fail("no .diff written".into()) } else { Step::Wait };
                    }
                    app.diff = None;
                    app.open_path(c.file("patch-me.txt"));
                    c.mark = 2;
                    Step::Wait
                }
                2 => {
                    if !wait_ready(app) {
                        return Step::Wait;
                    }
                    app.apply_patch_file(c.file("a-b.diff"));
                    c.mark = 3;
                    Step::Wait
                }
                3 => {
                    if !doc(app).is_modified() {
                        return if waited { Step::Fail("the patch was not applied".into()) } else { Step::Wait };
                    }
                    let want = std::fs::read_to_string(c.file("diff-b.txt")).unwrap_or_default();
                    let got = text(app);
                    if got != want {
                        return Step::Fail(format!("patched text differs from B ({} vs {} bytes)", got.len(), want.len()));
                    }
                    c.key(Key::Z, CTRL);
                    c.mark = 4;
                    Step::Wait
                }
                _ => {
                    if doc(app).is_modified() {
                        return if waited { Step::Fail("undo did not restore the file".into()) } else { Step::Wait };
                    }
                    check(text(app) == std::fs::read_to_string(c.file("diff-a.txt")).unwrap_or_default(), "after undo")
                }
            }
        }));
        s("a .gz name saves compressed: the .diff.gz opens as an unpacked copy", Box::new(|app, c| {
            if c.first() {
                let _ = std::fs::remove_file(c.file("a-b.diff.gz"));
                app.open_path(c.file("diff-a.txt"));
                app.start_diff(c.file("diff-b.txt"));
                return Step::Wait;
            }
            let waited = c.waited() > Duration::from_secs(10);
            match c.mark {
                0 => {
                    if !app.diff.as_ref().is_some_and(|r| r.data.lock().unwrap().done) {
                        return if waited { Step::Fail("the comparison did not finish".into()) } else { Step::Wait };
                    }
                    app.export_diff(c.file("a-b.diff.gz"));
                    c.mark = 1;
                    Step::Wait
                }
                1 => {
                    if !c.file("a-b.diff.gz").exists() || !app.tasks.is_empty() {
                        return if waited { Step::Fail("no .diff.gz written".into()) } else { Step::Wait };
                    }
                    app.diff = None;
                    app.open_path(c.file("a-b.diff.gz"));
                    c.mark = 2;
                    Step::Wait
                }
                _ => {
                    if !wait_ready(app) {
                        return if waited { Step::Fail("the .gz did not open".into()) } else { Step::Wait };
                    }
                    let from = doc(app).unpacked_from.clone();
                    let t = text(app);
                    let packed = std::fs::metadata(c.file("a-b.diff.gz")).map(|m| m.len()).unwrap_or(0);
                    let ok = from.as_deref().is_some_and(|p| p.ends_with("a-b.diff.gz")) && t.contains("+row one hundred\n") && t.contains("\n+extra line\n") && (packed as usize) < t.len();
                    check(ok, format!("from {from:?}, {} bytes from {packed}", t.len()))
                }
            }
        }));
        s("compare with the saved file shows unsaved edits; after a save, with the .bak", Box::new({
            let mut backup_was = false;
            move |app, c| {
                use crate::app::extras::DiffOther;
                if c.first() {
                    app.open_path(c.file("cmp-saved.txt"));
                    return Step::Wait;
                }
                let result = |app: &BigTextApp| {
                    let r = app.diff.as_ref()?;
                    let g = r.data.lock().unwrap();
                    g.done.then(|| (r.other.clone(), g.hunks.iter().map(|h| (h.a_line, h.a_count, h.b_line, h.b_count)).collect::<Vec<_>>()))
                };
                let waited = c.waited() > Duration::from_secs(10);
                match c.mark {
                    0 => {
                        if !wait_ready(app) {
                            return Step::Wait;
                        }
                        c.key(Key::Home, CTRL);
                        c.typed("NEW ");
                        c.mark = 1;
                        Step::Wait
                    }
                    1 => {
                        if !doc(app).is_modified() {
                            return if waited { Step::Fail("typing did not modify the file".into()) } else { Step::Wait };
                        }
                        app.start_diff_with(DiffOther::Saved);
                        c.mark = 2;
                        Step::Wait
                    }
                    2 => match result(app) {
                        None => Step::Wait,
                        Some((DiffOther::Saved, h)) if h == vec![(0, 1, 0, 1)] => {
                            c.mark = 3;
                            backup_was = app.settings.backup_on_save;
                            app.settings.backup_on_save = true;
                            c.key(Key::S, CTRL);
                            Step::Wait
                        }
                        Some(other) => Step::Fail(format!("saved compare: {other:?}")),
                    },
                    3 => {
                        let d = doc(app);
                        if d.save_job.is_some() || d.is_modified() {
                            return if waited { Step::Fail("save did not finish".into()) } else { Step::Wait };
                        }
                        app.settings.backup_on_save = backup_was;
                        if app.diff.is_some() {
                            return Step::Fail("the comparison with the saved file was kept through the save".into());
                        }
                        let bak = bigtext::save::backup_path_for(&c.file("cmp-saved.txt"));
                        if !bak.exists() {
                            return Step::Fail("no .bak after saving".into());
                        }
                        app.start_diff_with(DiffOther::Backup(bak));
                        c.mark = 4;
                        Step::Wait
                    }
                    _ => match result(app) {
                        None => Step::Wait,
                        Some((other, h)) => {
                            app.diff = None;
                            check(matches!(other, DiffOther::Backup(_)) && h == vec![(0, 1, 0, 1)], format!("backup compare: {other:?} {h:?}"))
                        }
                    },
                }
            }
        }));
        s("compare ignoring timestamps and a mask: logs that differ only there are the same", Box::new(|app, c| {
            if c.first() {
                app.open_path(c.file("times-a.log"));
                return Step::Wait;
            }
            let hunks = |app: &BigTextApp| {
                let r = app.diff.as_ref()?;
                let g = r.data.lock().unwrap();
                g.done.then(|| g.hunks.len())
            };
            match c.mark {
                0 => {
                    if !wait_ready(app) {
                        return Step::Wait;
                    }
                    app.diff_switches = Default::default();
                    app.start_diff(c.file("times-b.log"));
                    c.mark = 1;
                    Step::Wait
                }
                1 => match hunks(app) {
                    None => Step::Wait,
                    Some(0) => Step::Fail("plain compare found no differences".into()),
                    Some(_) => {
                        app.diff_switches.timestamps = true;
                        app.diff_switches.mask = "req=r[0-9]+".into();
                        app.start_diff(c.file("times-b.log"));
                        c.mark = 2;
                        Step::Wait
                    }
                },
                _ => match hunks(app) {
                    None => Step::Wait,
                    Some(n) => {
                        let summary = app.diff.as_ref().and_then(|r| r.switches.summary()).unwrap_or_default();
                        app.diff = None;
                        app.diff_switches = Default::default();
                        check(n == 0 && summary.contains("req=r"), format!("{n} hunks, summary {summary:?}"))
                    }
                },
            }
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
        s("CSV: sort by a column, move a column, rename a header; each undoes", Box::new(|app, c| {
            use crate::view::table_view::TableAction;
            const SORTED: &str = "id;name;age;city\r\n3;Cy;25;Bukhara\r\n2;\"Bob; Jr\";41;Samarkand\r\n1;Ann;30;Tashkent\r\n";
            const MOVED: &str = "id;age;name;city\r\n1;30;Ann;Tashkent\r\n2;41;\"Bob; Jr\";Samarkand\r\n3;25;Cy;Bukhara\r\n";
            const RENAMED: &str = "\"No; 1\";name;age;city\r\n1;Ann;30;Tashkent\r\n2;\"Bob; Jr\";41;Samarkand\r\n3;Cy;25;Bukhara\r\n";
            // Each stage: start an action, wait for its result, undo, wait for the original.
            let stage = c.mark / 2;
            if doc(app).save_job.is_some() {
                return Step::Wait;
            }
            let t = text(app);
            if c.mark % 2 == 0 {
                if t != CSV {
                    return if c.waited() > Duration::from_secs(3) { Step::Fail(format!("stage {stage}: not back to the original: {t:?}")) } else { Step::Wait };
                }
                match stage {
                    0 => app.on_table_action(TableAction::Sort { col: 3, desc: false }),
                    1 => app.on_table_action(TableAction::Move { col: 2, right: false }),
                    2 => {
                        app.on_table_action(TableAction::Rename { col: 0, name: "id".into() });
                        app.test_rename("No; 1");
                    }
                    _ => return Step::Next,
                }
                c.mark += 1;
                return Step::Wait;
            }
            let want = [SORTED, MOVED, RENAMED][stage as usize];
            if t != want {
                return if c.waited() > Duration::from_secs(3) { Step::Fail(format!("stage {stage}: {t:?}")) } else { Step::Wait };
            }
            c.mark += 1;
            c.key(Key::Z, CTRL);
            Step::Wait
        }));
        s("CSV: column statistics", Box::new(|app, c| {
            if c.first() {
                app.on_table_action(crate::view::table_view::TableAction::Stats { col: 2 });
                return Step::Wait;
            }
            let Some((_, s)) = app.csv.stats.clone() else {
                return if c.waited() > Duration::from_secs(3) { Step::Fail("no statistics".into()) } else { Step::Wait };
            };
            app.csv.stats = None;
            check(s.name == "age" && s.rows == 3 && s.numeric == 3 && s.sum == 96.0 && s.max == Some(41.0), format!("{s:?}"))
        }));
        s("command line: FILE:LINE with --find searches from that line", Box::new(|app, c| {
            if c.first() {
                let (path, line) = crate::cli::split_line(&format!("{}:2", c.file("cli.log").display()), |p| p.exists());
                let find = bigtext::search::Query { pattern: "10:0".into(), case_sensitive: true, ..Default::default() };
                app.apply_start(crate::app::StartOptions { files: vec![(path, line)], find: Some(find), ..Default::default() });
                return Step::Wait;
            }
            let d = doc(app);
            let sel = d.selection();
            if !d.is_ready() || sel != (19, 23) {
                return if c.waited() > Duration::from_secs(3) { Step::Fail(format!("title {} selection {sel:?}", d.title)) } else { Step::Wait };
            }
            app.search.open = false;
            Step::Next
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
        s("time gaps: largest first, click one to go there; Δ to the previous time", Box::new(|app, c| {
            if c.first() {
                app.gaps.min_secs = 30.0;
                app.open_gaps();
                return Step::Wait;
            }
            let Some(r) = doc(app).gaps.as_ref() else { return Step::Fail("no gaps".into()) };
            let g = r.data.lock().unwrap();
            if !g.done {
                return Step::Wait;
            }
            let got: Vec<i64> = g.gaps.gaps.iter().map(|x| x.ms()).collect();
            let after_b = g.gaps.gaps.first().map(|x| x.after);
            drop(g);
            if got != [55_000, 3_540_000] || !app.gaps.open {
                return Step::Fail(format!("gaps {got:?}"));
            }
            // The caret on "b" (after the trace line): 55 s since "a".
            doc(app).set_caret(after_b.unwrap_or(0), false);
            let delta = app.caret_time_delta();
            app.gaps.open = false;
            check(delta == Some(55_000), format!("Δ {delta:?}"))
        }));
        s("save in another encoding (UTF-16 LE with BOM)", Box::new(|app, c| {
            let target = c.file("time-utf16.log");
            if c.first() {
                let id = doc(app).id;
                let spec = crate::app::ops::WriteSpec::Transcode { enc: bigtext::encoding::Encoding::Utf16Le, bom: true, eol: None };
                app.start_write(id, target, spec, false);
                return Step::Wait;
            }
            let d = doc(app);
            if d.save_job.is_some() || !d.is_ready() || !d.path.as_ref().is_some_and(|p| p.ends_with("time-utf16.log")) {
                return if c.waited() > Duration::from_secs(5) { Step::Fail(format!("state {:?} path {:?}", d.state, d.path)) } else { Step::Wait };
            }
            let enc = d.buf.codec.enc;
            let bytes = std::fs::read(&target).unwrap_or_default();
            let t = text(app);
            check(
                enc == bigtext::encoding::Encoding::Utf16Le && bytes.starts_with(&[0xFF, 0xFE, b'2', 0]) && t.starts_with("\u{feff}2024-05-01 10:00:00 start\n"),
                format!("encoding {enc:?}, first bytes {:?}, text {:?}", &bytes[..bytes.len().min(6)], &t[..t.len().min(30)]),
            )
        }));
        s("open levels.log", open_step("levels.log"));
        s("log level bar: ERROR lines in the filter, F8 / Shift+F8", Box::new(|app, c| {
            use bigtext::loglevel::{Level, Levels};
            let line = |n: usize| LEVELS_LOG.split_inclusive('\n').take(n).map(str::len).sum::<usize>() as u64;
            if c.first() {
                if !app.level_bar_visible() {
                    return Step::Fail("the level bar is not shown for a .log file".into());
                }
                app.filter_levels(Levels::only(Level::Error));
                return Step::Wait;
            }
            if c.mark == 0 {
                let Some(r) = doc(app).filter.as_ref() else { return Step::Fail("no filter".into()) };
                let g = r.data.lock().unwrap();
                if !g.done {
                    return Step::Wait;
                }
                if g.lines != vec![line(3), line(5)] || r.levels != Some(Levels::only(Level::Error)) {
                    return Step::Fail(format!("filter lines {:?}", g.lines));
                }
                drop(g);
                c.mark = 1;
                doc(app).set_caret(0, false);
                c.key(Key::F8, NONE);
                return Step::Wait;
            }
            // F8 from the top: ERROR, then [error], then around to ERROR; Shift+F8 back to [error].
            let want = [(1, line(3) + 20, line(3) + 25), (2, line(5) + 20, line(5) + 27), (3, line(3) + 20, line(3) + 25), (4, line(5) + 20, line(5) + 27)];
            let (_, s0, e0) = want[(c.mark - 1) as usize];
            let sel = doc(app).selection();
            if sel != (s0, e0) || doc(app).find_job.is_some() {
                return if c.waited() > Duration::from_secs(5) { Step::Fail(format!("step {} selection {sel:?}, want {:?}", c.mark, (s0, e0))) } else { Step::Wait };
            }
            if c.mark == 4 {
                c.mark = 0;
                app.filter_levels(Levels::default());
                return check(doc(app).filter.is_none(), "an empty level set keeps the filter");
            }
            c.mark += 1;
            c.key(Key::F8, if c.mark == 4 { SHIFT } else { NONE });
            Step::Wait
        }));
        s("filter with ±1 context line: hits and dimmed lines around them", Box::new(|app, c| {
            use bigtext::loglevel::{Level, Levels};
            let line = |n: usize| LEVELS_LOG.split_inclusive('\n').take(n).map(str::len).sum::<usize>() as u64;
            if c.first() {
                app.settings.filter_context = 1;
                app.filter_levels(Levels::only(Level::Error));
                return Step::Wait;
            }
            let Some(r) = doc(app).filter.as_ref() else { return Step::Fail("no filter".into()) };
            let g = r.data.lock().unwrap();
            if !g.done {
                return Step::Wait;
            }
            let kinds: Vec<bool> = (0..g.lines.len()).map(|i| g.is_context(i)).collect();
            let ok = g.lines == vec![line(2), line(3), line(4), line(5)] && kinds == [true, false, true, false] && g.hits == 2 && r.spec.before == 1;
            let got = format!("lines {:?} context {kinds:?} hits {}", g.lines, g.hits);
            drop(g);
            app.settings.filter_context = 0;
            app.filter_levels(Levels::default());
            check(ok, got)
        }));
        s("back and forward: Alt+Left returns from an F8 jump, Alt+Right goes again", Box::new(|app, c| {
            if c.first() {
                doc(app).set_caret(0, false);
                return Step::Wait;
            }
            let caret = doc(app).view.caret;
            let line = doc(app).buf.line_of(caret).0;
            let (want, next_key) = match c.mark {
                0 => (None, Some((Key::F8, NONE))),
                1 => (Some(line == 3), Some((Key::ArrowLeft, ALT))),
                2 => (Some(caret == 0), Some((Key::ArrowRight, ALT))),
                _ => (Some(line == 3), None),
            };
            match want {
                Some(false) => return if c.waited() > Duration::from_secs(5) { Step::Fail(format!("stage {}: caret {caret} line {line}", c.mark)) } else { Step::Wait },
                Some(true) if next_key.is_none() => {
                    c.mark = 0;
                    return Step::Next;
                }
                _ => {}
            }
            let (k, m) = next_key.expect("a key");
            c.key(k, m);
            c.mark += 1;
            Step::Wait
        }));
        s("time range: the level bar only between 10:00:02 and 10:00:04", Box::new(|app, c| {
            use bigtext::loglevel::{Level, Levels};
            let line = |n: usize| LEVELS_LOG.split_inclusive('\n').take(n).map(str::len).sum::<usize>() as u64;
            if c.first() {
                app.apply_range("10:00:02".into(), "10:00:04".into());
                app.filter_levels(Levels::only(Level::Error));
                return Step::Wait;
            }
            let Some(r) = doc(app).filter.as_ref() else { return if c.waited() > Duration::from_secs(5) { Step::Fail("no filter".into()) } else { Step::Wait } };
            let g = r.data.lock().unwrap();
            if !g.done {
                return Step::Wait;
            }
            let ok = g.lines == vec![line(3)] && r.spec.range == Some((line(2), line(5)));
            let got = format!("lines {:?} range {:?}", g.lines, r.spec.range);
            drop(g);
            app.apply_range(String::new(), String::new());
            app.filter_levels(Levels::default());
            check(ok && app.range.active.is_none(), got)
        }));
        s("a log without an extension is recognised by its timestamps", open_step("journal"));
        s("open requests.log", open_step("requests.log"));
        s("follow an id: chips for the caret line, Ctrl+Shift+I filters to its entries", Box::new(|app, c| {
            let line = |n: usize| REQUESTS_LOG.split_inclusive('\n').take(n).map(str::len).sum::<usize>() as u64;
            if c.first() {
                doc(app).set_caret(line(1) + 5, false);
                let ids = app.caret_ids();
                if ids.len() != 1 || ids[0].value != "r-2" || ids[0].key != "request_id" {
                    return Step::Fail(format!("ids {ids:?}"));
                }
                c.key(Key::I, CTRL_SHIFT);
                return Step::Wait;
            }
            let Some(r) = doc(app).filter.as_ref() else { return if c.waited() > Duration::from_secs(5) { Step::Fail("no filter".into()) } else { Step::Wait } };
            let g = r.data.lock().unwrap();
            if !g.done {
                return Step::Wait;
            }
            let ok = g.lines == vec![line(1), line(3), line(4), line(5), line(6)] && g.hits == 3 && g.summary.errors == 1 && r.follow.as_deref() == Some("r-2");
            let span = g.summary.describe();
            let got = format!("lines {:?} hits {} summary {span}", g.lines, g.hits);
            drop(g);
            check(ok && span.contains("10:00:01") && span.contains("10:00:05"), got)
        }));
        s("narrow a filter: + “failed”, remove it, then without “done”", Box::new(|app, c| {
            let line = |n: usize| REQUESTS_LOG.split_inclusive('\n').take(n).map(str::len).sum::<usize>() as u64;
            let wants = [vec![line(3), line(4), line(5)], vec![line(1), line(3), line(4), line(5), line(6)], vec![line(1), line(3), line(4), line(5)]];
            if c.first() {
                app.narrow_filter("failed", false);
                return Step::Wait;
            }
            let Some(r) = doc(app).filter.as_ref() else { return Step::Fail("no filter".into()) };
            let g = r.data.lock().unwrap();
            if !g.done {
                return Step::Wait;
            }
            let (lines, terms) = (g.lines.clone(), r.terms.len());
            drop(g);
            let k = c.mark as usize;
            if lines != wants[k] || terms != [1, 0, 1][k] {
                return Step::Fail(format!("stage {k}: lines {lines:?} terms {terms}"));
            }
            c.mark += 1;
            match k {
                0 => app.remove_filter_term(0),
                1 => app.narrow_filter("done", true),
                _ => {
                    c.mark = 0;
                    return Step::Next;
                }
            }
            Step::Wait
        }));
        s("message patterns: templates by count; a template filters its lines", Box::new(|app, c| {
            let line = |n: usize| REQUESTS_LOG.split_inclusive('\n').take(n).map(str::len).sum::<usize>() as u64;
            if c.first() {
                app.patterns.only_filter = false;
                app.open_patterns();
                return Step::Wait;
            }
            if c.mark == 0 {
                let Some(r) = doc(app).patterns.as_ref() else { return Step::Fail("no patterns".into()) };
                let g = r.data.lock().unwrap();
                if !g.done {
                    return Step::Wait;
                }
                let Some(p) = g.p.clone() else { return Step::Fail("no result".into()) };
                drop(g);
                let top = &p.templates[0];
                if p.lines != 7 || p.templates.len() != 6 || top.text != "INFO request_id=r-<n> done" || top.count != 2 {
                    return Step::Fail(format!("{} lines, templates {:?}", p.lines, p.templates.iter().map(|t| (&t.text, t.count)).collect::<Vec<_>>()));
                }
                c.mark = 1;
                app.filter_by_template(&top.text, false);
                return Step::Wait;
            }
            let Some(r) = doc(app).filter.as_ref() else { return Step::Fail("no filter".into()) };
            let g = r.data.lock().unwrap();
            if !g.done {
                return Step::Wait;
            }
            let lines = g.lines.clone();
            drop(g);
            c.mark = 0;
            app.patterns.open = false;
            check(lines == vec![line(2), line(6)], format!("lines {lines:?}"))
        }));
        s("Find All → Top values of a capture group, as a table", Box::new(|app, c| {
            if c.first() {
                app.search.query = bigtext::search::Query { pattern: "request_id=(?P<rid>r-[0-9])".into(), regex: true, case_sensitive: true, whole_word: false, dot_all: false };
                app.find_all();
                return Step::Wait;
            }
            match c.mark {
                0 => {
                    let done = doc(app).results.as_ref().is_some_and(|r| !r.is_running());
                    if !done {
                        return Step::Wait;
                    }
                    app.open_values(false);
                    if app.values.is_none() {
                        return Step::Fail("no group dialog for a pattern with a group".into());
                    }
                    app.values = None;
                    app.run_values(false, 1);
                    c.mark = 1;
                    Step::Wait
                }
                1 => {
                    let d = doc(app);
                    if !d.title.starts_with("Values of") || !d.is_ready() {
                        return if c.waited() > Duration::from_secs(5) { Step::Fail(format!("active tab {:?}", d.title)) } else { Step::Wait };
                    }
                    let t = text(app);
                    if t != "value,matches,percent\nr-2,3,60.00\nr-1,2,40.00\n" {
                        return Step::Fail(format!("text {t:?}"));
                    }
                    c.copied = vec![doc(app).path.clone().unwrap_or_default().to_string_lossy().into_owned()];
                    c.key(Key::W, CTRL);
                    c.mark = 2;
                    Step::Wait
                }
                _ => {
                    if PathBuf::from(&c.copied[0]).exists() {
                        return if c.waited() > Duration::from_secs(5) { Step::Fail("the temporary table was not removed".into()) } else { Step::Wait };
                    }
                    c.copied.clear();
                    c.mark = 0;
                    Step::Next
                }
            }
        }));
        s("F4 / Shift+F4 step through Find All; Ctrl+1, Ctrl+9, Ctrl+PageUp pick tabs", Box::new(|app, c| {
            if c.first() {
                c.copied = vec![app.active.to_string()];
                doc(app).set_caret(0, false);
                c.key(Key::F4, NONE);
                return Step::Wait;
            }
            let home: usize = c.copied[0].parse().unwrap_or(0);
            let last = app.docs.len() - 1;
            let matches = app.docs[home].results.as_ref().map(|r| r.data.lock().unwrap().matches.clone()).unwrap_or_default();
            let sel = app.docs[home].selection();
            let ok = match c.mark {
                0 => matches.len() > 1 && sel == matches[0],
                1 => sel == matches[1],
                2 => sel == matches[0],
                3 => app.active == 0,
                4 => app.active == last,
                _ => app.active == last - 1,
            };
            if !ok {
                return Step::Fail(format!("stage {}: selection {sel:?}, matches {matches:?}, active {} of {}", c.mark, app.active, last + 1));
            }
            let next = match c.mark {
                0 => Some((Key::F4, NONE)),
                1 => Some((Key::F4, SHIFT)),
                2 => Some((Key::Num1, CTRL)),
                3 => Some((Key::Num9, CTRL)),
                4 => Some((Key::PageUp, CTRL)),
                _ => None,
            };
            c.mark += 1;
            match next {
                Some((k, m)) => {
                    c.key(k, m);
                    Step::Wait
                }
                None => {
                    app.active = home;
                    c.mark = 0;
                    c.copied.clear();
                    Step::Next
                }
            }
        }));
        s("journal: log highlighting and level bar", Box::new(|app, _c| {
            let d = doc(app);
            check(d.effective_lang() == crate::view::highlight::Lang::Log && app.level_bar_visible(), format!("lang {:?}", doc(app).effective_lang()))
        }));
        s("open merge-a.log", open_step("merge-a.log"));
        s("open merge-b.log", open_step("merge-b.log"));
        s("merge logs by time: a new read-only document with coloured file labels", Box::new(|app, c| {
            if c.first() {
                app.open_merge();
                return Step::Wait;
            }
            if c.mark == 0 {
                let ids: Vec<u64> = app.docs.iter().filter(|d| d.title.starts_with("merge-")).map(|d| d.id).collect();
                if ids.len() != 2 || !app.merge.open {
                    return Step::Fail(format!("{} merge files open, window open {}", ids.len(), app.merge.open));
                }
                app.test_pick_for_merge(&ids);
                app.start_merge();
                c.mark = 1;
                return Step::Wait;
            }
            let d = doc(app);
            if !d.title.starts_with("Merged:") || !d.is_ready() {
                return if c.waited() > Duration::from_secs(5) { Step::Fail(format!("active tab {:?}", d.title)) } else { Step::Wait };
            }
            c.mark = 0;
            let (ro, temp, tags) = (d.read_only, d.temporary, d.merged.as_ref().map(|t| t.labels.clone()));
            let want = "merge-a.log │ 2024-05-01 10:00:00 a1\nmerge-b.log │ 2024-05-01 10:00:01 b1\nmerge-a.log │ 2024-05-01 10:00:02 a2\nmerge-a.log │   at x()\nmerge-b.log │ 2024-05-01 10:00:03 b2\n";
            let t = text(app);
            let hl = app.view_highlights(true);
            let d = doc(app);
            let label_colors = [crate::app::marks::fill(0, true), crate::app::marks::fill(1, true)];
            let painted = hl(&d.buf, 0..d.buf.len()).iter().filter(|h| label_colors.contains(&h.2)).count();
            check(
                t == want && ro && temp && tags == Some(vec!["merge-a.log".into(), "merge-b.log".into()]) && painted == 5,
                format!("text {t:?} read-only {ro} temporary {temp} tags {tags:?} painted labels {painted}"),
            )
        }));
        s("closing the merged log deletes its temporary file", Box::new(|app, c| {
            if c.first() {
                let temp = doc(app).path.clone().unwrap_or_default();
                if !temp.exists() {
                    return Step::Fail(format!("{temp:?} missing"));
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
        s("back to levels.log", open_step("levels.log"));
        s("time histogram: lines and errors per second; a click on a bar jumps there", Box::new(|app, c| {
            let line = |n: usize| LEVELS_LOG.split_inclusive('\n').take(n).map(str::len).sum::<usize>() as u64;
            if c.first() {
                app.timeline.count = crate::app::timeline::Count::Errors;
                app.open_timeline();
                doc(app).set_caret(0, false);
                return Step::Wait;
            }
            if c.mark == 0 {
                let Some(t) = doc(app).timeline.as_ref() else { return Step::Fail("no histogram".into()) };
                let g = t.data.lock().unwrap();
                let Some(h) = g.hist.as_ref().filter(|_| g.done) else {
                    return if c.waited() > Duration::from_secs(5) { Step::Fail("histogram not done".into()) } else { Step::Wait };
                };
                let got = (h.lines, h.hits, h.buckets.len(), h.untimed, h.dated);
                if got != (6, 2, 6, 0, true) {
                    return Step::Fail(format!("lines, hits, buckets, untimed, dated = {got:?}"));
                }
                drop(g);
                // Click once the bars have kept their place for a frame (the panel settles).
                let bar = app.timeline.bars.get(3).map(|b| format!("{:?}", b.0));
                if app.timeline.bars.len() != 6 || c.copied.last() != bar.as_ref() {
                    c.copied = bar.into_iter().collect();
                    return if c.waited() > Duration::from_secs(5) { Step::Fail(format!("{} bars drawn", app.timeline.bars.len())) } else { Step::Wait };
                }
                c.copied.clear();
                c.mark = 1;
                let r = app.timeline.bars[3].0;
                c.click(Pos2::new(r.center().x, r.bottom() - 2.0), NONE);
                return Step::Wait;
            }
            let caret = doc(app).view.caret;
            if caret != line(3) {
                return if c.waited() > Duration::from_secs(5) { Step::Fail(format!("caret {caret}, want {}", line(3))) } else { Step::Wait };
            }
            c.mark = 0;
            app.timeline.open = false;
            Step::Next
        }));
        s("find in files: the *.log files of a folder and its sub-folders", Box::new(|app, c| {
            if c.first() {
                app.open_find_in_files();
                app.files.dir = c.file("tree").to_string_lossy().into_owned();
                app.files.names = "*.log".into();
                app.files.recursive = true;
                app.search.query = bigtext::search::Query { pattern: "NEEDLE".into(), case_sensitive: true, ..Default::default() };
                app.start_find_in_files();
                return Step::Wait;
            }
            let Some(r) = app.files.results.as_ref() else { return Step::Fail("no results".into()) };
            let g = r.data.lock().unwrap();
            if !g.done {
                return if c.waited() > Duration::from_secs(5) { Step::Fail("still searching".into()) } else { Step::Wait };
            }
            let got: Vec<(String, u64, String)> = g.hits.iter().map(|(f, h)| (g.files[*f].file_name().unwrap_or_default().to_string_lossy().into_owned(), h.line, h.text.clone())).collect();
            let want = vec![("a.log".to_string(), 2, "two NEEDLE".to_string()), ("b.log".to_string(), 1, "NEEDLE x".to_string())];
            check(got == want && g.total == 2 && g.searched == 2 && !app.files.open, format!("hits {got:?}, {} of {} files", g.searched, g.total))
        }));
        s("a match from Find in files opens its file with the match selected", Box::new(|app, c| {
            if c.first() {
                app.open_files_hit(1);
                return Step::Wait;
            }
            let d = doc(app);
            if d.title != "b.log" || !d.is_ready() {
                return if c.waited() > Duration::from_secs(5) { Step::Fail(format!("active tab {:?}", d.title)) } else { Step::Wait };
            }
            let sel = d.selection();
            if let Some(r) = app.files.results.take() {
                r.cancel.cancel();
            }
            check(sel == (0, 6), format!("selection {sel:?}"))
        }));
        s("open epoch.log", open_step("epoch.log"));
        s("go to time in a log of Unix times", Box::new(|app, c| {
            if c.first() {
                c.key(Key::T, CTRL_SHIFT);
                c.push(vec![], NONE);
                c.typed("2024-05-01 14:33:30");
                c.key(Key::Enter, NONE);
                return Step::Wait;
            }
            // The first line at or after 14:33:30 is the third one.
            let want = "1714573925 start\n1714573985 middle\n".len() as u64;
            let caret = doc(app).view.caret;
            if caret != want {
                return if c.waited() > Duration::from_secs(3) { Step::Fail(format!("caret {caret}, want {want}")) } else { Step::Wait };
            }
            doc(app).select(0, want);
            let dt = doc(app).selection_time_delta();
            doc(app).select(0, 0);
            check(dt == Some(60_000), format!("Δt {dt:?}"))
        }));
        s("levels.log again", open_step("levels.log"));
        s("saved search: saved with its \"!\", applied as a filter later", Box::new(|app, c| {
            let line = |n: usize| LEVELS_LOG.split_inclusive('\n').take(n).map(str::len).sum::<usize>() as u64;
            if c.first() {
                app.search.query = bigtext::search::Query { pattern: "INFO".into(), case_sensitive: true, ..Default::default() };
                app.search.invert = true;
                app.save_search("Not info");
                app.search.query.pattern = "something else".into();
                app.search.invert = false;
                let Some(i) = app.settings.saved.iter().position(|s| s.name == "Not info") else { return Step::Fail("not saved".into()) };
                app.apply_saved(i);
                return Step::Wait;
            }
            let d = doc(app);
            let Some(r) = d.filter.as_ref() else { return Step::Fail("no filter".into()) };
            let g = r.data.lock().unwrap();
            if !g.done {
                return Step::Wait;
            }
            let ok = g.lines == vec![line(1), line(2), line(3), line(5)] && r.spec.invert && r.query.pattern == "INFO";
            let got = format!("lines {:?} invert {} pattern {:?}", g.lines, r.spec.invert, r.query.pattern);
            drop(g);
            if let Some(r) = doc(app).filter.take() {
                r.cancel.cancel();
            }
            app.search.invert = false;
            app.search.open = false;
            check(ok, got)
        }));
        s("open multi.txt", open_step("multi.txt"));
        s("Ctrl+D: the word, then its next occurrences as cursors; typing changes all", Box::new(|app, c| {
            if c.first() {
                let p = pos(app, 0, 1.0);
                c.click(p, NONE);
                c.key(Key::D, CTRL);
                return Step::Wait;
            }
            let n = doc(app).view.extra.len();
            match c.mark {
                // The word, then two more occurrences.
                0 | 1 if doc(app).find_job.is_none() && n == c.mark as usize && doc(app).selection() != (1, 1) => {
                    c.mark += 1;
                    c.key(Key::D, CTRL);
                    Step::Wait
                }
                2 if doc(app).find_job.is_none() && n == 2 => {
                    c.mark = 3;
                    c.typed("bar");
                    Step::Wait
                }
                3 if text(app).starts_with("bar x bar y bar\n") => {
                    c.mark = 4;
                    c.key(Key::Escape, NONE);
                    Step::Wait
                }
                4 if n == 0 => {
                    c.mark = 0;
                    Step::Next
                }
                _ if c.waited() > Duration::from_secs(5) => Step::Fail(format!("step {}: {n} extra cursors, selection {:?}, text {:?}", c.mark, doc(app).selection(), text(app))),
                _ => Step::Wait,
            }
        }));
        s("Alt+click adds a cursor; Backspace at both", Box::new(|app, c| {
            if c.first() {
                let (a, b) = (pos(app, 1, 5.0), pos(app, 2, 6.0));
                c.click(a, NONE);
                c.click(b, ALT);
                c.key(Key::Backspace, NONE);
                return Step::Wait;
            }
            let t = text(app);
            if !t.ends_with("firs\nsecon\n") {
                return if c.waited() > Duration::from_secs(3) { Step::Fail(format!("text {t:?}, {} extra cursors", doc(app).view.extra.len())) } else { Step::Wait };
            }
            c.key(Key::Escape, NONE);
            Step::Next
        }));
        s("open brackets.json", open_step("brackets.json"));
        s("the bracket pair at the caret is highlighted; Ctrl+] jumps between them", Box::new(|app, c| {
            use bigtext::brackets::Pair;
            let close = BRACKETS_JSON.trim_end().len() as u64 - 1;
            let inner = BRACKETS_JSON.find("{\"b").unwrap() as u64;
            if c.first() {
                doc(app).set_caret(0, false);
                if app.caret_pair() != Some(Pair::Brackets(0, close)) {
                    return Step::Fail(format!("pair {:?}", app.caret_pair()));
                }
                c.key(Key::CloseBracket, CTRL);
                return Step::Wait;
            }
            let caret = doc(app).view.caret;
            match c.mark {
                0 if caret == close => {
                    c.mark = 1;
                    c.key(Key::CloseBracket, CTRL);
                    Step::Wait
                }
                1 if caret == 0 => {
                    // Inside "b": to the brace around it.
                    c.mark = 2;
                    doc(app).set_caret(inner + 3, false);
                    c.key(Key::CloseBracket, CTRL);
                    Step::Wait
                }
                2 if caret == inner => {
                    c.mark = 0;
                    Step::Next
                }
                _ if c.waited() > Duration::from_secs(3) => Step::Fail(format!("step {} caret {caret}", c.mark)),
                _ => Step::Wait,
            }
        }));
        s("open ws.txt", open_step("ws.txt"));
        s("show whitespace", Box::new(|app, _c| {
            app.settings.show_ws = true;
            Step::Next
        }));
        s("whitespace shown as · → ↵ keeps every character in its place", Box::new(|app, c| {
            if c.mark == 0 {
                c.mark = 1;
                // "a b" then a one-cell tab: "c" is in cell 4.
                let p = pos(app, 0, 4.0);
                c.click(p, NONE);
                return Step::Wait;
            }
            let caret = doc(app).view.caret;
            if caret != 4 {
                return if c.waited() > Duration::from_secs(3) { Step::Fail(format!("caret {caret}")) } else { Step::Wait };
            }
            c.mark = 0;
            app.settings.show_ws = false;
            Step::Next
        }));
        s("open outline.json", open_step("outline.json"));
        s("JSON structure: the top level's keys in the side panel", Box::new(|app, c| {
            if c.first() {
                app.json_open = false;
                app.toggle_json_panel();
                return Step::Wait;
            }
            let Some(t) = doc(app).json.as_ref() else { return Step::Fail("no outline".into()) };
            let keys: Vec<String> = t.nodes.iter().skip(1).filter_map(|n| n.child.key.clone()).collect();
            if keys.len() < 2 {
                return if c.waited() > Duration::from_secs(5) { Step::Fail(format!("keys {keys:?}, error {:?}", t.error)) } else { Step::Wait };
            }
            check(keys == vec!["name".to_string(), "items".to_string()], format!("keys {keys:?}"))
        }));
        s("JSON path: go to $.items[2].id, and the path at the caret", Box::new(|app, c| {
            let at = |s: &str| OUTLINE_JSON.find(s).unwrap() as u64;
            if c.first() {
                app.json_goto("$.items[2].id");
                return Step::Wait;
            }
            if c.mark == 0 {
                let sel = doc(app).selection();
                if sel != (at("12"), at("12") + 2) {
                    return if c.waited() > Duration::from_secs(5) { Step::Fail(format!("selection {sel:?}")) } else { Step::Wait };
                }
                c.mark = 1;
                doc(app).set_caret(at("11"), false);
                app.json_path_at_caret();
                return Step::Wait;
            }
            let path = doc(app).json.as_ref().map(|t| t.path.clone()).unwrap_or_default();
            if path != "$.items[1].id" {
                return if c.waited() > Duration::from_secs(5) { Step::Fail(format!("path {path:?}")) } else { Step::Wait };
            }
            c.mark = 0;
            app.json_open = false;
            Step::Next
        }));
        s("compare JSON by structure: one changed value, key order ignored", Box::new(|app, c| {
            if c.first() {
                let other = c.file("outline2.json");
                let _ = std::fs::write(&other, "{\n  \"items\": [{\"id\": 10}, {\"id\": 11}, {\"id\": 13}],\n  \"name\": \"demo\"\n}\n");
                app.compare_json_with(other);
                return Step::Wait;
            }
            let Some(r) = doc(app).json_diff.as_ref() else { return Step::Fail("no comparison".into()) };
            let g = r.data.lock().unwrap();
            if !g.done {
                return Step::Wait;
            }
            let got: Vec<(String, Option<String>, Option<String>)> = g.changes.iter().map(|c| (c.path.clone(), c.old.clone(), c.new.clone())).collect();
            let err = g.error.clone();
            drop(g);
            app.json_diff.open = false;
            check(got == [("$.items[2].id".to_string(), Some("12".to_string()), Some("13".to_string()))], format!("{got:?} {err:?}"))
        }));
        s("open catalog.xml", open_step("catalog.xml"));
        s("XML structure: elements in the side panel, a path, the path at the caret", Box::new(|app, c| {
            let at = |s: &str| CATALOG_XML.find(s).unwrap() as u64;
            if c.first() {
                app.json_open = false;
                app.toggle_json_panel();
                return Step::Wait;
            }
            if c.mark == 0 {
                let Some(t) = doc(app).json.as_ref() else { return Step::Fail("no outline".into()) };
                let keys: Vec<String> = t.nodes.iter().skip(1).filter_map(|n| n.child.key.clone()).collect();
                if keys.len() < 2 {
                    return if c.waited() > Duration::from_secs(5) { Step::Fail(format!("keys {keys:?}, error {:?}", t.error)) } else { Step::Wait };
                }
                if keys != ["item id=\"1\"", "item id=\"2\""] || !t.xml {
                    return Step::Fail(format!("keys {keys:?}"));
                }
                c.mark = 1;
                app.json_goto("/catalog/item[2]/price");
                return Step::Wait;
            }
            if c.mark == 1 {
                let sel = doc(app).selection();
                if sel != (at("<price>12"), at("<price>12") + 17) {
                    return if c.waited() > Duration::from_secs(5) { Step::Fail(format!("selection {sel:?}")) } else { Step::Wait };
                }
                c.mark = 2;
                doc(app).set_caret(at(">10<"), false);
                app.json_path_at_caret();
                return Step::Wait;
            }
            let path = doc(app).json.as_ref().map(|t| t.path.clone()).unwrap_or_default();
            if path != "/catalog/item/price" {
                return if c.waited() > Duration::from_secs(5) { Step::Fail(format!("path {path:?}")) } else { Step::Wait };
            }
            c.mark = 0;
            app.json_open = false;
            Step::Next
        }));
        s("open events.jsonl", open_step("events.jsonl"));
        s("JSON Lines as a table: a column per key", Box::new(|app, c| {
            if c.first() {
                app.open_jsonl_table();
                return Step::Wait;
            }
            let d = doc(app);
            if !d.title.ends_with("(table)") || !d.is_ready() {
                return if c.waited() > Duration::from_secs(5) { Step::Fail(format!("active tab {:?}", d.title)) } else { Step::Wait };
            }
            let (table, ro, temp) = (d.table, d.read_only, d.temporary);
            let t = text(app);
            check(t == "id,msg,user\n1,start,\n2,\"a, b\",ann\n" && table && ro && temp, format!("text {t:?} table {table} read-only {ro} temporary {temp}"))
        }));
        s("close the JSON Lines table", Box::new(|app, c| {
            if c.first() {
                c.copied = vec![doc(app).path.clone().unwrap_or_default().to_string_lossy().into_owned()];
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
        s("log fields: the caret's JSON line as key → value rows", Box::new(|app, c| {
            if c.first() {
                doc(app).set_caret(27, false);
                c.key(Key::L, CTRL_SHIFT);
                return Step::Wait;
            }
            if !app.fields.open {
                return Step::Fail("Ctrl+Shift+L did not open the panel".into());
            }
            match app.caret_fields() {
                Some(crate::app::fields_panel::Parsed::Fields(_, f)) => {
                    let got: Vec<(String, String)> = f.into_iter().map(|f| (f.key, f.value)).collect();
                    let want = [("id", "2"), ("msg", "a, b"), ("user", "ann")].map(|(k, v)| (k.to_string(), v.to_string()));
                    check(got == want, format!("fields {got:?}"))
                }
                _ => Step::Fail("the line was not parsed".into()),
            }
        }));
        s("check JSON: events.jsonl is valid JSON Lines", Box::new(|app, c| {
            if c.first() {
                app.check_json(None);
                return Step::Wait;
            }
            let Some(r) = doc(app).json_check.as_ref() else { return Step::Fail("no check".into()) };
            let g = r.data.lock().unwrap();
            if !g.done {
                return Step::Wait;
            }
            let (lines, n, values, mode) = (g.checked.lines, g.checked.problems.len(), g.checked.values, r.lines);
            drop(g);
            app.json_check.open = false;
            check(mode && lines == 2 && n == 0 && values == 7, format!("lines mode {mode}, {lines} lines, {n} problems, {values} values"))
        }));
        s("JSON fields: every key with its counts", Box::new(|app, c| {
            if c.first() {
                app.json_fields.result = None;
                app.start_json_fields();
                return Step::Wait;
            }
            let Some((_, f)) = app.json_fields.result.as_ref() else { return if c.waited() > Duration::from_secs(5) { Step::Fail("no fields".into()) } else { Step::Wait } };
            let got: Vec<(String, u64)> = f.stats.iter().map(|s| (s.path.clone(), s.count)).collect();
            let records = f.records;
            app.json_fields.result = None;
            check(records == 2 && got == [("id".to_string(), 2), ("msg".to_string(), 2), ("user".to_string(), 1)], format!("{records} records, {got:?}"))
        }));
        s("filter JSON Lines by a field query", Box::new(|app, c| {
            if c.first() {
                app.search.query.pattern = ".user exists and .id > 1".into();
                app.search.fields = true;
                app.start_filter();
                return Step::Wait;
            }
            let Some(r) = doc(app).filter.as_ref() else { return Step::Fail(format!("no filter ({})", app.search.status)) };
            let g = r.data.lock().unwrap();
            if !g.done {
                return Step::Wait;
            }
            let (lines, json) = (g.lines.clone(), r.json.is_some());
            drop(g);
            app.search.fields = false;
            if let Some(r) = doc(app).filter.take() {
                r.cancel.cancel();
            }
            check(lines == [26] && json, format!("lines {lines:?} json {json}"))
        }));
        s("log fields: count a field's values into a table", Box::new(|app, c| {
            if c.first() {
                app.count_field_values("user".into(), bigtext::fields::Syntax::Json);
                return Step::Wait;
            }
            if c.mark == 1 {
                let temp = PathBuf::from(&c.copied[0]);
                if temp.exists() {
                    return if c.waited() > Duration::from_secs(5) { Step::Fail(format!("{temp:?} still exists")) } else { Step::Wait };
                }
                c.copied.clear();
                c.mark = 0;
                return Step::Next;
            }
            let d = doc(app);
            if !d.title.starts_with("user") || !d.is_ready() {
                return if c.waited() > Duration::from_secs(5) { Step::Fail(format!("active tab {:?}", d.title)) } else { Step::Wait };
            }
            let t = text(app);
            if t != "user,lines,percent,first,last\nann,1,100.00,,\n" {
                return Step::Fail(format!("text {t:?}"));
            }
            c.mark = 1;
            c.copied = vec![doc(app).path.clone().unwrap_or_default().to_string_lossy().into_owned()];
            c.key(Key::W, CTRL);
            app.fields.open = false;
            Step::Wait
        }));
        s("open app.logfmt", open_step("app.logfmt"));
        s("logfmt lines as a table", Box::new(|app, c| {
            if c.first() {
                app.open_logfmt_table();
                return Step::Wait;
            }
            if c.mark == 0 {
                let d = doc(app);
                if !d.title.ends_with("(table)") || !d.is_ready() {
                    return if c.waited() > Duration::from_secs(5) { Step::Fail(format!("active tab {:?}", d.title)) } else { Step::Wait };
                }
                let t = text(app);
                if t != "level,msg,code\ninfo,ok,\nerror,bad x,7\n" {
                    return Step::Fail(format!("text {t:?}"));
                }
                c.key(Key::W, CTRL);
                c.mark = 1;
                return Step::Wait;
            }
            if app.docs.iter().any(|d| d.title == "app.logfmt (table)") {
                return Step::Wait;
            }
            c.mark = 0;
            Step::Next
        }));
        s("open broken.csv", open_step("broken.csv"));
        s("check table rows: a short row and text in a number column", Box::new(|app, c| {
            use bigtext::csv_check::Kind;
            if c.first() {
                return Step::Wait;
            }
            if c.mark == 0 {
                if doc(app).table_state.dialect.is_none() {
                    return if c.waited() > Duration::from_secs(3) { Step::Fail("no dialect".into()) } else { Step::Wait };
                }
                app.problems.numeric = "2".into();
                app.check_rows();
                c.mark = 1;
                return Step::Wait;
            }
            let Some(r) = doc(app).problems.as_ref() else { return Step::Fail("no check".into()) };
            let g = r.data.lock().unwrap();
            if !g.done {
                return Step::Wait;
            }
            let kinds: Vec<Kind> = g.checked.problems.iter().map(|p| p.kind).collect();
            let starts: Vec<u64> = g.checked.problems.iter().map(|p| p.start).collect();
            drop(g);
            c.mark = 0;
            app.problems.open = false;
            app.problems.numeric.clear();
            check(kinds == [Kind::Fields { got: 1, want: 2 }, Kind::NotNumber { col: 1 }] && starts == [11, 13], format!("{kinds:?} at {starts:?}"))
        }));
        s("open excel.csv", open_step("excel.csv"));
        s("a table after Excel's sep=; line and a comment, with its header", Box::new(|app, c| {
            let st = &doc(app).table_state;
            let Some(d) = st.dialect else { return if c.waited() > Duration::from_secs(3) { Step::Fail("no dialect".into()) } else { Step::Wait } };
            check(d.delim == b';' && st.skip == 2 && st.header && d.start == 17, format!("delim {:?} skip {} header {} start {}", d.delim as char, st.skip, st.header, d.start))
        }));
        s("open numbers.csv", open_step("numbers.csv"));
        s("a table of numbers has no header row", Box::new(|app, c| {
            let st = &doc(app).table_state;
            if st.dialect.is_none() {
                return if c.waited() > Duration::from_secs(3) { Step::Fail("no dialect".into()) } else { Step::Wait };
            }
            check(!st.header && st.skip == 0, format!("header {} skip {}", st.header, st.skip))
        }));
        s("open sales.csv", open_step("sales.csv"));
        s("table: hide a column from the view, freeze the first one", Box::new(|app, c| {
            use crate::view::table_view::TableAction;
            if c.first() {
                return Step::Wait;
            }
            if c.mark == 0 {
                if doc(app).table_state.header_cells.len() != 2 {
                    return if c.waited() > Duration::from_secs(3) { Step::Fail(format!("header cells {:?}", doc(app).table_state.header_cells)) } else { Step::Wait };
                }
                app.on_table_action(TableAction::Hide { cols: vec![1] });
                app.on_table_action(TableAction::Freeze { upto: 1 });
                c.mark = 1;
                return Step::Wait;
            }
            let len = doc(app).buf.len();
            let st = &doc(app).table_state;
            let cells: Vec<usize> = st.header_cells.iter().map(|h| h.0).collect();
            let ok = cells == [0] && st.frozen == 1 && len == 41;
            let got = format!("cells {cells:?} frozen {} len {len}", st.frozen);
            let st = &mut doc(app).table_state;
            st.hidden.clear();
            st.frozen = 0;
            c.mark = 0;
            check(ok, got)
        }));
        s("CSV: group rows by a column and sum another", Box::new(|app, c| {
            if c.first() {
                return Step::Wait;
            }
            if c.mark == 0 {
                if doc(app).table_state.dialect.is_none() {
                    return if c.waited() > Duration::from_secs(3) { Step::Fail("no dialect".into()) } else { Step::Wait };
                }
                app.on_table_action(crate::view::table_view::TableAction::GroupBy { col: 0 });
                let names = app.csv.group.as_ref().map(|g| g.names.clone()).unwrap_or_default();
                if names != vec!["city".to_string(), "qty".to_string()] {
                    return Step::Fail(format!("column names {names:?}"));
                }
                app.csv.group = None;
                app.start_group_by(0, Some(1));
                c.mark = 1;
                return Step::Wait;
            }
            let Some((_, g)) = app.csv.groups.as_ref() else {
                return if c.waited() > Duration::from_secs(5) { Step::Fail("no groups".into()) } else { Step::Wait };
            };
            let got: Vec<(String, u64, f64)> = g.groups.iter().map(|x| (x.key.clone(), x.rows, x.sum)).collect();
            c.mark = 0;
            let ok = got == vec![("Tashkent".to_string(), 2, 10.0), ("Bukhara".to_string(), 1, 5.0)];
            app.csv.groups = None;
            check(ok, format!("groups {got:?}"))
        }));
        s("pivot a table: cities as rows, quantities as columns", Box::new(|app, c| {
            use bigtext::csv_pivot::{Agg, Bucket, PivotSpec};
            if c.first() {
                app.start_pivot(PivotSpec { rows: 0, cols: 1, value: None, agg: Agg::Count, bucket: Bucket::AsIs, header: true });
                return Step::Wait;
            }
            let d = doc(app);
            if c.mark == 0 {
                if !d.title.starts_with("Pivot of") || !d.is_ready() {
                    return if c.waited() > Duration::from_secs(5) { Step::Fail(format!("active tab {:?}", d.title)) } else { Step::Wait };
                }
                let t = text(app);
                if t != "city,3,5,7,Total\nTashkent,1,0,1,2\nBukhara,0,1,0,1\n" {
                    return Step::Fail(format!("text {t:?}"));
                }
                c.key(Key::W, CTRL);
                c.mark = 1;
                return Step::Wait;
            }
            if app.docs.iter().any(|d| d.title.starts_with("Pivot of")) {
                return Step::Wait;
            }
            if let Some(i) = app.docs.iter().position(|d| d.title == "sales.csv") {
                app.active = i;
            }
            c.mark = 0;
            Step::Next
        }));
        s("chart a column: text values counted; a bar opens the conditions for it", Box::new(|app, c| {
            use bigtext::csv_chart::ChartKind;
            if c.first() {
                app.start_column_chart(0);
                return Step::Wait;
            }
            let Some(r) = doc(app).chart.as_ref() else { return Step::Fail("no chart".into()) };
            let g = r.data.lock().unwrap();
            if !g.done {
                return Step::Wait;
            }
            let bars: Vec<(String, u64)> = g.chart.as_ref().map(|c| c.bars.iter().map(|b| (b.label.clone(), b.count)).collect()).unwrap_or_default();
            let kind = g.chart.as_ref().map(|c| c.kind);
            drop(g);
            app.chart_bar_clicked(0);
            let conds = app.csv.conds.take().map(|d| d.conds.iter().map(|c| (c.col, c.value.clone())).collect::<Vec<_>>()).unwrap_or_default();
            app.chart.open = false;
            let want = vec![("Tashkent".to_string(), 2), ("Bukhara".to_string(), 1)];
            check(kind == Some(ChartKind::Text) && bars == want && conds == [(0, "Tashkent".to_string())], format!("{kind:?} {bars:?} {conds:?}"))
        }));
        s("add a column from an expression, then undo it", Box::new(|app, c| {
            const SALES: &str = "city,qty\nTashkent,3\nBukhara,5\nTashkent,7\n";
            if c.first() {
                app.ask_rewrite(bigtext::csv::Rewrite::Derive { name: "double".into(), expr: "{qty} * 2".into(), names: vec!["city".into(), "qty".into()], header: true });
                return Step::Wait;
            }
            if doc(app).save_job.is_some() {
                return Step::Wait;
            }
            let t = text(app);
            if c.mark == 0 {
                if t != "city,qty,double\nTashkent,3,6\nBukhara,5,10\nTashkent,7,14\n" {
                    return if c.waited() > Duration::from_secs(5) { Step::Fail(format!("text {t:?}")) } else { Step::Wait };
                }
                c.key(Key::Z, CTRL);
                c.mark = 1;
                return Step::Wait;
            }
            c.mark = 0;
            check(t == SALES, format!("after undo {t:?}"))
        }));
        s("open cities.csv", open_step("cities.csv"));
        s("join two tables by a key into a new table", Box::new(|app, c| {
            let id_of = |app: &BigTextApp, name: &str| app.docs.iter().find(|d| d.title == name).map(|d| d.id);
            if c.first() {
                return Step::Wait;
            }
            match c.mark {
                0 => {
                    let ready = app.docs.iter().filter(|d| (d.title == "sales.csv" || d.title == "cities.csv") && d.table_state.dialect.is_some()).count() == 2;
                    if !ready {
                        return if c.waited() > Duration::from_secs(3) { Step::Fail("the tables are not ready".into()) } else { Step::Wait };
                    }
                    let (Some(left), Some(right)) = (id_of(app, "sales.csv"), id_of(app, "cities.csv")) else { return Step::Fail("no tabs".into()) };
                    app.start_join(crate::app::join_tables::JoinDialog { left, right, left_key: 0, right_key: 0, inner: false });
                    c.mark = 1;
                    Step::Wait
                }
                1 => {
                    let d = doc(app);
                    if !d.title.starts_with("sales.csv + cities.csv") || !d.is_ready() {
                        return if c.waited() > Duration::from_secs(5) { Step::Fail(format!("active tab {:?}", d.title)) } else { Step::Wait };
                    }
                    let t = text(app);
                    if t != "city,qty,country\nTashkent,3,UZ\nBukhara,5,UZ\nTashkent,7,UZ\n" {
                        return Step::Fail(format!("text {t:?}"));
                    }
                    c.key(Key::W, CTRL);
                    c.mark = 2;
                    Step::Wait
                }
                _ => {
                    if app.docs.iter().any(|d| d.title.starts_with("sales.csv + ")) {
                        return Step::Wait;
                    }
                    if let Some(i) = app.docs.iter().position(|d| d.title == "sales.csv") {
                        app.active = i;
                    }
                    c.mark = 0;
                    Step::Next
                }
            }
        }));
        s("CSV: export the table as Markdown and as an Excel workbook", Box::new(|app, c| {
            use bigtext::table_export::Format;
            let (md, xlsx) = (c.file("sales.md"), c.file("sales.xlsx"));
            if c.first() {
                app.export_table_to(None, Format::Markdown, md.clone());
                app.export_table_to(None, Format::Xlsx, xlsx.clone());
                return Step::Wait;
            }
            if !app.tasks.is_empty() || !xlsx.exists() || !md.exists() {
                return if c.waited() > Duration::from_secs(5) { Step::Fail(format!("{} tasks running", app.tasks.len())) } else { Step::Wait };
            }
            let text = std::fs::read_to_string(&md).unwrap_or_default();
            let sheet = std::fs::File::open(&xlsx).ok().and_then(|f| zip::ZipArchive::new(f).ok()).and_then(|mut z| {
                let mut s = String::new();
                std::io::Read::read_to_string(&mut z.by_name("xl/worksheets/sheet1.xml").ok()?, &mut s).ok()?;
                Some(s)
            });
            let ok = text == "| city | qty |\n| --- | --- |\n| Tashkent | 3 |\n| Bukhara | 5 |\n| Tashkent | 7 |\n" && sheet.as_deref().is_some_and(|s| s.contains(r#"<c r="B4"><v>7</v></c>"#));
            check(ok, format!("markdown {text:?}, sheet {sheet:?}"))
        }));
        s("CSV: rows meeting several conditions open as a new table", Box::new(|app, c| {
            use bigtext::csv::{Cond, CondOp};
            if c.first() {
                app.start_conditions(vec![Cond { col: 0, op: CondOp::Equals, value: "tashkent".into() }, Cond { col: 1, op: CondOp::Greater, value: "4".into() }]);
                return Step::Wait;
            }
            let d = doc(app);
            if !d.title.ends_with("(filtered)") || !d.is_ready() {
                return if c.waited() > Duration::from_secs(5) { Step::Fail(format!("active tab {:?}", d.title)) } else { Step::Wait };
            }
            let table = d.table;
            let t = text(app);
            c.key(Key::W, CTRL);
            check(t == "city,qty\nTashkent,7\n" && table, format!("text {t:?} table {table}"))
        }));
        s("Ctrl+Tab goes back to the tab used before", Box::new(|app, c| {
            if c.first() {
                c.copied = vec![doc(app).id.to_string()];
                app.open_path(c.file("levels.log"));
                return Step::Wait;
            }
            if c.mark == 0 {
                c.mark = 1;
                c.key(Key::Tab, CTRL);
                return Step::Wait;
            }
            let want: u64 = c.copied[0].parse().unwrap_or(0);
            let open = app.switcher.is_some();
            if doc(app).id != want || open {
                let title = doc(app).title.clone();
                return if c.waited() > Duration::from_secs(3) { Step::Fail(format!("active {title:?}, switcher open {open}")) } else { Step::Wait };
            }
            c.mark = 0;
            c.copied.clear();
            Step::Next
        }));
        s("open tab1.txt", open_step("tab1.txt"));
        s("open tab2.txt", open_step("tab2.txt"));
        s("open tab3.txt", open_step("tab3.txt"));
        s("close tabs to the right, pin a tab, move a tab", Box::new(|app, _c| {
            let title_at = |app: &BigTextApp, i: usize| app.docs.get(i).map(|d| d.title.clone()).unwrap_or_default();
            let Some(t1) = app.docs.iter().find(|d| d.title == "tab1.txt").map(|d| d.id) else { return Step::Fail("tab1 not open".into()) };
            app.close_to_right(t1);
            let left: Vec<String> = app.docs.iter().map(|d| d.title.clone()).filter(|t| t.starts_with("tab") && t.ends_with(".txt")).collect();
            if left != vec!["tab1.txt".to_string()] {
                return Step::Fail(format!("after closing to the right: {left:?}"));
            }
            app.toggle_pin(t1);
            if title_at(app, 0) != "tab1.txt" || !app.docs[0].pinned {
                return Step::Fail(format!("pinned tab is at {:?}", app.docs.iter().position(|d| d.id == t1)));
            }
            // A pinned tab cannot move among the unpinned ones; unpinned, it can.
            let last = app.docs.len() - 1;
            app.move_tab(0, last);
            if title_at(app, 0) != "tab1.txt" {
                return Step::Fail("a pinned tab moved out of the pinned group".into());
            }
            app.toggle_pin(t1);
            app.move_tab(0, last);
            check(title_at(app, last) == "tab1.txt" && !app.docs[last].pinned, format!("last tab {:?}", title_at(app, last)))
        }));
        s("levels.log for the session", open_step("levels.log"));
        s("Ctrl+Shift+C copies file:line", Box::new(|app, c| {
            let line = |n: usize| LEVELS_LOG.split_inclusive('\n').take(n).map(str::len).sum::<usize>() as u64;
            if c.first() {
                c.copied.clear();
                doc(app).set_caret(line(2) + 3, false);
                c.key(Key::C, CTRL_SHIFT);
                return Step::Wait;
            }
            let got = c.copied.last().cloned().unwrap_or_default();
            if got.is_empty() {
                return if c.waited() > Duration::from_secs(3) { Step::Fail("nothing copied".into()) } else { Step::Wait };
            }
            check(got.ends_with("levels.log:3") && std::path::Path::new(got.trim_end_matches(":3")).exists(), format!("copied {got:?}"))
        }));
        s("a session brings back a closed file at its place, pinned, with its filter", Box::new(|app, c| {
            use crate::app::sessions;
            use bigtext::loglevel::{Level, Levels};
            let line = |n: usize| LEVELS_LOG.split_inclusive('\n').take(n).map(str::len).sum::<usize>() as u64;
            if c.first() {
                app.filter_levels(Levels::only(Level::Error));
                doc(app).set_caret(line(4), false);
                let id = doc(app).id;
                if !doc(app).pinned {
                    app.toggle_pin(id);
                }
                let text = sessions::serialize(&app.current_session());
                c.copied = vec![text];
                // Close it (unpinned first, as a user would), then open the session.
                app.toggle_pin(id);
                app.test_close(id);
                if app.docs.iter().any(|d| d.title == "levels.log") {
                    return Step::Fail("levels.log did not close".into());
                }
                app.apply_session(sessions::parse(&c.copied[0]));
                return Step::Wait;
            }
            let Some(d) = app.docs.iter().find(|d| d.title == "levels.log") else { return Step::Fail("levels.log not reopened".into()) };
            let done = d.filter.as_ref().is_some_and(|f| !f.is_running());
            if !d.is_ready() || !done {
                return if c.waited() > Duration::from_secs(5) { Step::Fail(format!("ready {} filter {}", d.is_ready(), d.filter.is_some())) } else { Step::Wait };
            }
            let ok = d.pinned && d.view.caret == line(4) && d.filter.as_ref().and_then(|f| f.levels) == Some(Levels::only(Level::Error)) && app.docs[0].title == "levels.log";
            let got = format!("pinned {} caret {} levels {:?} first tab {:?}", d.pinned, d.view.caret, d.filter.as_ref().and_then(|f| f.levels), app.docs[0].title);
            let id = d.id;
            app.toggle_pin(id);
            c.copied.clear();
            check(ok, got)
        }));
        s("settings export and import (highlights, saved searches, keys)", Box::new(|app, c| {
            use crate::app::commands::Cmd;
            let file = c.file("my.settings");
            let q = bigtext::search::Query { pattern: "TIMEOUT".into(), case_sensitive: true, ..Default::default() };
            app.settings.marks.push(crate::app::marks::Mark { query: q.clone(), color: 3, enabled: true });
            app.settings.saved.push(crate::app::saved_searches::SavedSearch { name: "Timeouts".into(), query: q.clone(), invert: false, column: None });
            app.keymap.changed.insert(Cmd::GotoTime, None);
            if let Err(e) = app.export_settings_to(&file) {
                return Step::Fail(e);
            }
            let text = std::fs::read_to_string(&file).unwrap_or_default();
            if text.contains("tab=") || text.contains("recent=") {
                return Step::Fail(format!("files leaked into the settings: {text}"));
            }
            app.settings.marks.retain(|m| m.query != q);
            app.settings.saved.clear();
            app.keymap.changed.clear();
            if let Err(e) = app.import_settings_from(&file, None) {
                return Step::Fail(e);
            }
            let ok = app.settings.marks.iter().any(|m| m.query == q && m.color == 3) && app.settings.saved.iter().any(|s| s.name == "Timeouts") && app.keymap.get(Cmd::GotoTime).is_none();
            app.settings.marks.retain(|m| m.query != q);
            app.settings.saved.clear();
            app.keymap.changed.clear();
            check(ok, format!("settings file: {text}"))
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
        s("compare side by side", Box::new(|app, c| {
            app.side_by_side = true;
            match app.diff.as_ref().and_then(|r| r.side.as_ref()) {
                Some(s) if s.doc.is_ready() && c.mark == 0 => {
                    // The first difference, a few lines down.
                    app.step_diff(true);
                    c.mark = 1;
                    Step::Wait
                }
                Some(_) if c.mark == 1 => Step::Next,
                _ => Step::Wait,
            }
        }));
        s("shot compare-side.png", shot_step("compare-side.png"));
        s("side by side off", Box::new(|app, _| {
            app.side_by_side = false;
            Step::Next
        }));
        s("file properties", Box::new(|app, c| {
            if c.first() {
                app.open_properties();
                return Step::Wait;
            }
            let done = app.properties.as_ref().is_some_and(|p| p.data.lock().unwrap().stats.is_some_and(|s| s.done));
            if done {
                Step::Next
            } else {
                Step::Wait
            }
        }));
        s("shot properties.png", shot_step("properties.png"));
        s("properties off", Box::new(|app, _| {
            app.properties = None;
            Step::Next
        }));
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
