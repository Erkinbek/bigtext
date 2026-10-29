//! Scripted GUI benchmark (`bigtext --bench-ui FILE [--report OUT]`): opens a file, measures time
//! to first paint, frame times while scrolling during background indexing, go-to latency before
//! and after indexing, and idle memory. Writes a plain-text report and exits.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use bigtext::jobs::human_bytes;
use bigtext::text::rows;

use crate::app::BigTextApp;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Open,
    WaitFirstPaint,
    GotoBeforeIndex,
    ScrollWhileIndexing,
    WaitIndex,
    GotoAfterIndex,
    ScrollAfterIndex,
    Idle,
    Done,
    SearchStart,
    SearchWait,
    FindNext,
    EditInsert,
    SaveWait,
    CloseDuring,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scenario {
    Scroll,
    Search(String),
    Edit,
    View { hex: bool, wrap: bool },
    /// Close the window while a Find All (`replace == false`) or an on-disk Replace All runs.
    CloseDuring { replace: bool },
}

pub struct BenchUi {
    file: PathBuf,
    report: PathBuf,
    phase: Phase,
    n: usize,
    open_at: Option<Instant>,
    first_paint: Option<Duration>,
    open_worker: Option<Duration>,
    act: bool,
    goto_pre: Vec<f32>,
    goto_post: Vec<f32>,
    scroll_indexing: Vec<f32>,
    scroll_idle: Vec<f32>,
    rng: u64,
    log: String,
    idle_mem: Option<u64>,
    index_time: Option<Duration>,
    index_mem: usize,
    pub screenshot: Option<PathBuf>,
    pub scenario: Scenario,
    t_phase: Option<Instant>,
}

fn stats(v: &[f32]) -> String {
    if v.is_empty() {
        return "n/a".into();
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let avg = s.iter().sum::<f32>() / s.len() as f32;
    let p99 = s[((s.len() as f32 * 0.99) as usize).min(s.len() - 1)];
    format!("n={} avg={:.2} ms p99={:.2} ms max={:.2} ms", s.len(), avg, p99, s[s.len() - 1])
}

impl BenchUi {
    pub fn new(file: PathBuf, report: PathBuf) -> BenchUi {
        BenchUi {
            file,
            report,
            phase: Phase::Open,
            n: 0,
            open_at: None,
            first_paint: None,
            open_worker: None,
            act: false,
            goto_pre: Vec::new(),
            goto_post: Vec::new(),
            scroll_indexing: Vec::new(),
            scroll_idle: Vec::new(),
            rng: 0x1234_5678,
            log: String::new(),
            idle_mem: None,
            index_time: None,
            index_mem: 0,
            screenshot: None,
            scenario: Scenario::Scroll,
            t_phase: None,
        }
    }

    fn rand(&mut self) -> u64 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        self.rng
    }

    fn set(&mut self, p: Phase) {
        self.phase = p;
        self.n = 0;
    }

    pub fn before_frame(&mut self, app: &mut BigTextApp, ctx: &egui::Context) {
        ctx.request_repaint();
        self.act = false;
        self.n += 1;
        match self.phase {
            Phase::Open => {
                self.open_at = Some(Instant::now());
                app.open_path(self.file.clone());
                self.set(Phase::WaitFirstPaint);
            }
            Phase::WaitFirstPaint => {
                if let Some(d) = app.active_doc() {
                    if let (Some(fp), Some(o)) = (d.view.first_paint, self.open_at) {
                        self.first_paint = Some(fp.duration_since(o));
                        self.open_worker = d.open_elapsed;
                        let next = match self.scenario {
                            Scenario::Scroll => Phase::GotoBeforeIndex,
                            Scenario::Search(_) => Phase::SearchStart,
                            Scenario::Edit => Phase::EditInsert,
                            Scenario::View { hex, wrap } => {
                                d.hex = hex;
                                d.view.wrap = wrap;
                                Phase::Idle
                            }
                            Scenario::CloseDuring { .. } => Phase::CloseDuring,
                        };
                        self.set(next);
                    }
                }
            }
            Phase::GotoBeforeIndex => {
                let r = self.rand();
                let Some(d) = app.active_doc() else { return };
                if d.buf.index.is_done() || self.n > 10 {
                    self.set(Phase::ScrollWhileIndexing);
                    return;
                }
                let (lines, _) = d.buf.line_count();
                d.goto_line(1 + r % lines.max(1));
                self.act = true;
            }
            Phase::ScrollWhileIndexing => {
                let Some(d) = app.active_doc() else { return };
                if d.buf.index.is_done() || self.n > 600 {
                    self.set(Phase::WaitIndex);
                    return;
                }
                page_down(d);
                self.act = true;
            }
            Phase::WaitIndex => {
                let Some(d) = app.active_doc() else { return };
                if d.buf.index.is_done() {
                    self.index_time = d.buf.index.finished_after();
                    self.index_mem = d.buf.index.memory_bytes();
                    self.set(Phase::GotoAfterIndex);
                }
            }
            Phase::GotoAfterIndex => {
                let r = self.rand();
                let Some(d) = app.active_doc() else { return };
                if self.n > 30 {
                    self.set(Phase::ScrollAfterIndex);
                    return;
                }
                let (lines, _) = d.buf.line_count();
                d.goto_line(1 + r % lines.max(1));
                self.act = true;
            }
            Phase::ScrollAfterIndex => {
                let Some(d) = app.active_doc() else { return };
                if self.n > 300 {
                    self.set(Phase::Idle);
                    return;
                }
                page_down(d);
                self.act = true;
            }
            Phase::Idle => {
                if self.n == 20 && self.screenshot.is_some() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
                }
                let shot = ctx.input(|i| {
                    i.raw.events.iter().find_map(|e| match e {
                        egui::Event::Screenshot { image, .. } => Some(image.clone()),
                        _ => None,
                    })
                });
                if let (Some(img), Some(path)) = (shot, self.screenshot.as_ref()) {
                    let rgba: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_srgba_unmultiplied()).collect();
                    let _ = write_png(path, img.size[0] as u32, img.size[1] as u32, &rgba);
                }
                if self.n > 60 {
                    self.idle_mem = bigtext::sysinfo::private_bytes();
                    self.write_report(app);
                    self.set(Phase::Done);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            Phase::Done => {}
            Phase::CloseDuring => {
                let Scenario::CloseDuring { replace } = self.scenario.clone() else { return };
                if self.n == 1 {
                    // Rare enough that Find All is not capped early.
                    app.search.query.pattern = if replace { "timeout".into() } else { "timeout connection".into() };
                    app.search.query.case_sensitive = true;
                    if replace {
                        app.search.replace_with = "TIMEOUT!".into();
                        app.replace_all();
                    } else {
                        app.find_all();
                    }
                    return;
                }
                if replace && app.test_has_confirm() {
                    // "More than 50,000 matches" prompt: accept the on-disk replace.
                    let id = app.active_doc().map(|d| d.id).unwrap_or(0);
                    app.test_dismiss();
                    app.replace_on_disk(id, false);
                    return;
                }
                let progress = app.active_doc().map(|d| match (&d.save_job, &d.results) {
                    (Some(j), _) if j.label == "Replacing" => j.progress.fraction(),
                    (_, Some(r)) if !replace => r.progress.fraction(),
                    _ => 0.0,
                });
                if let Some(p) = progress {
                    if p > 0.05 {
                        let _ = writeln!(self.log, "closing at {:.0}% of the {}", p * 100.0, if replace { "replace" } else { "search" });
                        self.write_report(app);
                        self.set(Phase::Done);
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
            }
            Phase::SearchStart => {
                let Scenario::Search(p) = self.scenario.clone() else { return };
                app.search.query.pattern = p;
                app.open_search(false);
                self.t_phase = Some(Instant::now());
                app.find_all();
                self.set(Phase::SearchWait);
            }
            Phase::SearchWait => {
                let Some(d) = app.active_doc() else { return };
                let Some(r) = &d.results else { return };
                let g = r.data.lock().unwrap();
                if g.done {
                    let n = g.matches.len();
                    drop(g);
                    let t = self.t_phase.map(|t| t.elapsed()).unwrap_or_default();
                    let mb = d.buf.len() as f64 / 1048576.0;
                    let _ = writeln!(self.log, "find all: {n} matches in {:.3} s ({:.0} MB/s, through the GUI job)", t.as_secs_f64(), mb / t.as_secs_f64().max(1e-9));
                    self.set(Phase::FindNext);
                }
            }
            Phase::FindNext => {
                if self.n <= 5 {
                    app.find_next(true);
                    self.act = true;
                } else {
                    let caret = app.active_doc().map(|d| d.view.caret).unwrap_or(0);
                    let _ = writeln!(self.log, "after 5x F3 caret at offset {caret}");
                    self.set(Phase::Idle);
                }
            }
            Phase::EditInsert => {
                let Some(d) = app.active_doc() else { return };
                let indexing = !d.buf.index.is_done();
                d.set_caret(d.min_caret(), false);
                d.insert_text("EDITED BY BIGTEXT BENCH\n");
                let _ = writeln!(self.log, "inserted text at start (index still running: {indexing}); saving");
                self.t_phase = Some(Instant::now());
                app.save(false);
                self.set(Phase::SaveWait);
            }
            Phase::SaveWait => {
                let Some(d) = app.active_doc() else { return };
                if d.save_job.is_none() && d.is_ready() && !d.is_modified() {
                    let t = self.t_phase.map(|t| t.elapsed()).unwrap_or_default();
                    let head = d.path.as_ref().and_then(|p| std::fs::read(p).ok().map(|b| b[..b.len().min(24)].to_vec()));
                    let _ = writeln!(
                        self.log,
                        "save finished in {:.3} s; file now starts with {:?}; new size {}",
                        t.as_secs_f64(),
                        head.map(|h| String::from_utf8_lossy(&h).into_owned()),
                        d.buf.len()
                    );
                    self.set(Phase::Idle);
                } else if self.n > 20_000 {
                    let _ = writeln!(self.log, "save did not finish; state {:?}", d.state);
                    self.set(Phase::Idle);
                }
            }
        }
    }

    pub fn after_frame(&mut self, _app: &mut BigTextApp, _ctx: &egui::Context, ms: f32) {
        if !self.act {
            return;
        }
        match self.phase {
            Phase::GotoBeforeIndex => self.goto_pre.push(ms),
            Phase::ScrollWhileIndexing => self.scroll_indexing.push(ms),
            Phase::GotoAfterIndex => self.goto_post.push(ms),
            Phase::ScrollAfterIndex => self.scroll_idle.push(ms),
            Phase::FindNext => self.goto_post.push(ms),
            _ => {}
        }
    }

    fn write_report(&mut self, app: &BigTextApp) {
        let d = app.docs.get(app.active);
        let mut s = String::new();
        let _ = writeln!(s, "BigText GUI benchmark");
        let _ = writeln!(s, "file: {}", self.file.display());
        if let Some(d) = d {
            let _ = writeln!(s, "size: {} ({} bytes), backing: {}, encoding: {}", human_bytes(d.buf.len()), d.buf.len(), d.buf.source.kind_name(), d.buf.codec.enc.name());
            let (lines, exact) = d.buf.line_count();
            let _ = writeln!(s, "lines: {}{}", if exact { "" } else { "~" }, lines);
        }
        if let Some(f) = app.first_frame {
            let _ = writeln!(s, "startup -> first frame: {:.1} ms", f.as_secs_f64() * 1e3);
        }
        if let Some(o) = self.open_worker {
            let _ = writeln!(s, "open + detect (worker): {:.1} ms", o.as_secs_f64() * 1e3);
        }
        if let Some(fp) = self.first_paint {
            let _ = writeln!(s, "open request -> first screen of text: {:.1} ms", fp.as_secs_f64() * 1e3);
        }
        let _ = writeln!(s, "go-to line before indexing (frame time incl. jump+layout): {}", stats(&self.goto_pre));
        let _ = writeln!(s, "page-down frames while indexing: {}", stats(&self.scroll_indexing));
        if let Some(t) = self.index_time {
            let mb = d.map(|d| d.buf.len()).unwrap_or(0) as f64 / (1024.0 * 1024.0);
            let _ = writeln!(s, "background index: {:.2} s ({:.0} MB/s), index memory {}", t.as_secs_f64(), mb / t.as_secs_f64().max(1e-9), human_bytes(self.index_mem as u64));
        }
        let _ = writeln!(s, "go-to line after indexing (frame time): {}", stats(&self.goto_post));
        let _ = writeln!(s, "page-down frames after indexing: {}", stats(&self.scroll_idle));
        if let Some(m) = self.idle_mem {
            let _ = writeln!(s, "idle private memory: {}", human_bytes(m));
        }
        s.push_str(&self.log);
        let _ = std::fs::write(&self.report, s);
    }
}

fn page_down(d: &mut crate::app::document::Document) {
    let wrap = crate::view::text_view::wrap_cols(&d.view);
    let n = d.view.rows_on_screen.max(1) as i64;
    if let Ok(top) = rows::row_at(&d.buf, d.view.top, wrap) {
        if let Ok(r) = crate::view::folds::step_rows(&d.buf, top, n, wrap, &d.view.folds) {
            d.view.top = r.start;
            d.view.caret = r.start.max(d.min_caret());
            d.view.anchor = d.view.caret;
        }
    }
}

/// Minimal PNG writer (stored deflate blocks) for benchmark screenshots.
pub fn write_png(path: &std::path::Path, w: u32, h: u32, rgba: &[u8]) -> std::io::Result<()> {
    fn crc32(data: &[u8]) -> u32 {
        let mut table = [0u32; 256];
        for (i, t) in table.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            *t = c;
        }
        let mut c = 0xFFFF_FFFFu32;
        for &b in data {
            c = table[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
        }
        c ^ 0xFFFF_FFFF
    }
    let stride = w as usize * 4;
    let mut raw = Vec::with_capacity((stride + 1) * h as usize);
    for row in rgba.chunks(stride) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut z = vec![0x78, 0x01];
    let chunks: Vec<&[u8]> = raw.chunks(65_535).collect();
    for (i, c) in chunks.iter().enumerate() {
        z.push((i + 1 == chunks.len()) as u8);
        let n = c.len() as u16;
        z.extend_from_slice(&n.to_le_bytes());
        z.extend_from_slice(&(!n).to_le_bytes());
        z.extend_from_slice(c);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + x as u32) % 65_521;
        b = (b + a) % 65_521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut out = vec![0x89u8, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut chunk = |ty: &[u8], data: &[u8]| {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut td = ty.to_vec();
        td.extend_from_slice(data);
        out.extend_from_slice(&td);
        out.extend_from_slice(&crc32(&td).to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    std::fs::write(path, out)
}
