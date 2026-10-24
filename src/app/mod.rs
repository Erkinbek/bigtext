//! Application shell: tabs, menus, dialogs, status bar and background-job plumbing.

pub mod document;
pub mod ops;
pub mod search_panel;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Align, Color32, Key, KeyboardShortcut, Layout, Modifiers, RichText};

use bigtext::encoding::{self, Detected, Encoding};
use bigtext::file::{self, OpenMode, Source};
use bigtext::encoding::LineEnding;
use bigtext::jobs::{self, human_bytes, CancelToken, Waker};
use bigtext::replace::Template;
use bigtext::search::Matcher;
use bigtext::text::Piece;

use crate::view::{hex_view, text_view};
use document::{Banner, DocState, Document};

pub struct Opened {
    pub source: Arc<Source>,
    pub detected: Detected,
    pub elapsed: Duration,
}

pub enum Msg {
    Opened { doc: u64, result: Result<Opened, String> },
    Copied { doc: u64, result: Result<String, String>, cut: bool },
    Found { doc: u64, version: u64, found: Option<(u64, u64)>, wrapped: bool },
    TempWritten(Box<ops::TempWritten>),
    Finalized(Box<ops::Finalized>),
    JobFailed { doc: u64, msg: Option<String> },
    ReplaceBatch { doc: u64, version: u64, add_len: u64, pieces: Vec<Piece>, add: Vec<u8>, count: usize },
    ReplaceTooMany { doc: u64, version: u64, matcher: Arc<Matcher>, template: Template, range: std::ops::Range<u64> },
    Preview { doc: u64, items: Vec<(u64, String, String)>, lines: Vec<u64> },
    FileChanged { doc: u64, deleted: bool, grown: bool },
}

pub struct Settings {
    pub font_size: f32,
    pub dark: bool,
    pub show_perf: bool,
    pub backup_on_save: bool,
}

struct GotoDialog {
    text: String,
    by_offset: bool,
    error: Option<String>,
    focus: bool,
}

struct Confirm {
    title: String,
    message: String,
    action: ConfirmAction,
    checkbox: Option<(String, bool)>,
}

#[derive(Clone)]
enum ConfirmAction {
    CloseTab(u64),
    Exit,
    CopyLarge { doc: u64, cut: bool },
    ReplaceOnDisk { doc: u64 },
}

#[derive(Default)]
pub struct FrameStats {
    times: VecDeque<f32>,
    pub last_ms: f32,
}

impl FrameStats {
    fn push(&mut self, ms: f32) {
        self.last_ms = ms;
        self.times.push_back(ms);
        if self.times.len() > 240 {
            self.times.pop_front();
        }
    }
    pub fn avg(&self) -> f32 {
        if self.times.is_empty() {
            0.0
        } else {
            self.times.iter().sum::<f32>() / self.times.len() as f32
        }
    }
    pub fn max(&self) -> f32 {
        self.times.iter().copied().fold(0.0, f32::max)
    }
}

pub struct BigTextApp {
    pub docs: Vec<Document>,
    pub active: usize,
    next_id: u64,
    untitled: usize,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    waker: Waker,
    pub settings: Settings,
    goto: Option<GotoDialog>,
    confirm: Option<Confirm>,
    error: Option<String>,
    pub frame: FrameStats,
    pub process_start: Instant,
    pub first_frame: Option<Duration>,
    pub bench: Option<crate::bench_ui::BenchUi>,
    exit_confirmed: bool,
    exit_after_jobs: bool,
    show_about: bool,
    pub search: search_panel::SearchUi,
    pending_replace: Option<(u64, Arc<Matcher>, Template, std::ops::Range<u64>)>,
    preview: Option<(u64, Vec<(u64, String, String)>, Vec<u64>)>,
}

/// Turns I/O errors into messages a user can act on.
pub fn friendly_io(e: &std::io::Error, path: &Path) -> String {
    let p = file::display_path(path);
    match (e.kind(), e.raw_os_error()) {
        (_, Some(32)) | (_, Some(33)) => format!("“{p}” is in use by another process."),
        (std::io::ErrorKind::PermissionDenied, _) => format!("Access denied: “{p}”. Check permissions or the read-only attribute."),
        (std::io::ErrorKind::NotFound, _) => format!("File not found: “{p}”."),
        _ => format!("{p}: {e}"),
    }
}

fn open_worker(path: &Path, forced: Option<Encoding>) -> Result<Opened, String> {
    let t = Instant::now();
    let src = Source::open(path, OpenMode::Auto).map_err(|e| friendly_io(&e, path))?;
    let n = src.len().min(1 << 20);
    let whole = n == src.len();
    let det = {
        let sample = src.read(0..n).map_err(|e| friendly_io(&e, path))?;
        match forced {
            Some(enc) => encoding::detect_with(&sample, whole, enc, 0),
            None => encoding::detect(&sample, whole),
        }
    };
    Ok(Opened { source: Arc::new(src), detected: det, elapsed: t.elapsed() })
}

impl BigTextApp {
    pub fn new(cc: &eframe::CreationContext<'_>, files: Vec<PathBuf>, process_start: Instant) -> Self {
        let (tx, rx) = mpsc::channel();
        let ctx = cc.egui_ctx.clone();
        let waker: Waker = Arc::new(move || ctx.request_repaint());
        cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
        let mut app = BigTextApp {
            docs: Vec::new(),
            active: 0,
            next_id: 1,
            untitled: 0,
            tx,
            rx,
            waker,
            settings: Settings { font_size: 14.0, dark: true, show_perf: false, backup_on_save: false },
            goto: None,
            confirm: None,
            error: None,
            frame: FrameStats::default(),
            process_start,
            first_frame: None,
            bench: None,
            exit_confirmed: false,
            exit_after_jobs: false,
            show_about: false,
            search: search_panel::SearchUi::default(),
            pending_replace: None,
            preview: None,
        };
        app.search.query.case_sensitive = true;
        cc.egui_ctx.set_theme(egui::Theme::Dark);
        for f in files {
            app.open_path(f);
        }
        if app.docs.is_empty() {
            app.new_doc();
        }
        app
    }

    fn alloc_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    pub fn new_doc(&mut self) {
        self.untitled += 1;
        let id = self.alloc_id();
        self.docs.push(Document::untitled(id, self.untitled));
        self.active = self.docs.len() - 1;
    }

    pub fn open_path(&mut self, path: PathBuf) {
        let canon = std::fs::canonicalize(&path).unwrap_or(path);
        if let Some(i) = self.docs.iter().position(|d| d.path.as_deref() == Some(canon.as_path())) {
            self.active = i;
            return;
        }
        // Replace a pristine untitled tab.
        if self.docs.len() == 1 && self.docs[0].path.is_none() && !self.docs[0].is_modified() && self.docs[0].buf.is_empty() {
            self.docs.clear();
        }
        let id = self.alloc_id();
        self.docs.push(Document::loading(id, canon.clone()));
        self.active = self.docs.len() - 1;
        self.spawn_open(id, canon, None);
    }

    fn spawn_open(&self, id: u64, path: PathBuf, forced: Option<Encoding>) {
        let tx = self.tx.clone();
        let waker = self.waker.clone();
        jobs::spawn("open", move || {
            let result = open_worker(&path, forced);
            let _ = tx.send(Msg::Opened { doc: id, result });
            waker();
        });
    }

    pub fn active_doc(&mut self) -> Option<&mut Document> {
        self.docs.get_mut(self.active)
    }

    fn doc_mut(&mut self, id: u64) -> Option<&mut Document> {
        self.docs.iter_mut().find(|d| d.id == id)
    }

    fn poll_messages(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Opened { doc, result } => {
                    let waker = self.waker.clone();
                    let tx = self.tx.clone();
                    if let Some(d) = self.doc_mut(doc) {
                        match result {
                            Ok(o) => {
                                d.open_elapsed = Some(o.elapsed);
                                d.read_only = d.read_only || is_read_only(d.path.as_deref());
                                let prefix = d.reload_prefix.take();
                                let follow_end = d.follow && prefix.is_some();
                                d.install(o.source, o.detected, prefix, waker.clone());
                                if follow_end {
                                    let len = d.buf.len();
                                    d.set_caret(len, false);
                                }
                                watch(d, tx, waker);
                            }
                            Err(e) => d.state = DocState::Failed(e),
                        }
                    }
                }
                Msg::Found { doc, version, found, wrapped } => self.on_found(doc, version, found, wrapped),
                Msg::TempWritten(tw) => self.on_temp_written(*tw),
                Msg::Finalized(f) => self.on_finalized(*f),
                Msg::JobFailed { doc, msg } => self.on_job_failed(doc, msg),
                Msg::ReplaceBatch { doc, version, add_len, pieces, add, count } => self.on_replace_batch(doc, version, add_len, pieces, add, count),
                Msg::ReplaceTooMany { doc, version, matcher, template, range } => self.on_replace_too_many(doc, version, matcher, template, range),
                Msg::Preview { doc, items, lines } => self.preview = Some((doc, items, lines)),
                Msg::FileChanged { doc, deleted, grown } => {
                    let (tx, waker) = (self.tx.clone(), self.waker.clone());
                    if let Some(d) = self.doc_mut(doc) {
                        if d.follow && grown && !deleted && !d.is_modified() && d.save_job.is_none() {
                            reload(d, &tx, &waker, true);
                        } else if d.is_ready() {
                            d.banner = Some(Banner::ChangedOnDisk { deleted, grown });
                        }
                    }
                }
                Msg::Copied { doc, result, cut } => match result {
                    Ok(text) => {
                        let ctx = self.ctx_clone();
                        if let Some(ctx) = ctx {
                            ctx.copy_text(text);
                        }
                        if cut {
                            if let Some(d) = self.doc_mut(doc) {
                                d.delete_selection();
                            }
                        }
                    }
                    Err(e) => self.error = Some(e),
                },
            }
        }
    }

    fn ctx_clone(&self) -> Option<egui::Context> {
        CTX.with(|c| c.borrow().clone())
    }

    fn close_tab(&mut self, id: u64, force: bool) {
        let Some(i) = self.docs.iter().position(|d| d.id == id) else { return };
        if !force && self.docs[i].is_modified() {
            self.confirm = Some(Confirm {
                title: "Unsaved changes".into(),
                message: format!("“{}” has unsaved changes. Close it anyway?", self.docs[i].title),
                action: ConfirmAction::CloseTab(id),
                checkbox: None,
            });
            return;
        }
        let mut d = self.docs.remove(i);
        d.cancel_jobs();
        d.watch_cancel.cancel();
        if let Some(j) = &d.save_job {
            j.cancel.cancel();
        }
        if self.docs.is_empty() {
            self.new_doc();
        }
        self.active = self.active.min(self.docs.len() - 1);
    }

    fn start_copy_job(&mut self, id: u64, cut: bool) {
        let Some(d) = self.docs.iter().find(|d| d.id == id) else { return };
        let (s, e) = d.selection();
        let snap = d.buf.snapshot();
        let tx = self.tx.clone();
        let waker = self.waker.clone();
        jobs::spawn("copy", move || {
            let result = snap.read(s..e).map(|b| snap.codec.decode_lossy(&b)).ok_or_else(|| "Could not read the selection".to_string());
            let _ = tx.send(Msg::Copied { doc: id, result, cut });
            waker();
        });
    }

    fn open_dialog(&mut self) {
        if let Some(files) = rfd::FileDialog::new().set_title("Open").pick_files() {
            for f in files {
                self.open_path(f);
            }
        }
    }

    // ------------------------------------------------------------------------------------------
    // UI pieces.

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let sc = |m: Modifiers, k: Key| KeyboardShortcut::new(m, k);
        let cmd = Modifiers::COMMAND;
        let cmd_shift = Modifiers::COMMAND | Modifiers::SHIFT;
        if ctx.input_mut(|i| i.consume_shortcut(&sc(cmd, Key::O))) {
            self.open_dialog();
        }
        if ctx.input_mut(|i| i.consume_shortcut(&sc(cmd, Key::N))) {
            self.new_doc();
        }
        if ctx.input_mut(|i| i.consume_shortcut(&sc(cmd, Key::W))) {
            if let Some(id) = self.docs.get(self.active).map(|d| d.id) {
                self.close_tab(id, false);
            }
        }
        if ctx.input_mut(|i| i.consume_shortcut(&sc(cmd_shift, Key::S))) {
            self.save(true);
        }
        if ctx.input_mut(|i| i.consume_shortcut(&sc(cmd, Key::S))) {
            self.save(false);
        }
        if ctx.input_mut(|i| i.consume_shortcut(&sc(cmd, Key::F))) {
            self.open_search(false);
        }
        if ctx.input_mut(|i| i.consume_shortcut(&sc(cmd, Key::H))) {
            self.open_search(true);
        }
        if ctx.input_mut(|i| i.consume_shortcut(&sc(Modifiers::SHIFT, Key::F3))) {
            self.find_next(false);
        }
        if ctx.input_mut(|i| i.consume_shortcut(&sc(Modifiers::NONE, Key::F3))) {
            self.find_next(true);
        }
        if ctx.input_mut(|i| i.consume_shortcut(&sc(cmd, Key::G))) {
            self.goto = Some(GotoDialog { text: String::new(), by_offset: false, error: None, focus: true });
        }
        if ctx.input_mut(|i| i.consume_shortcut(&sc(cmd_shift, Key::G))) {
            self.goto = Some(GotoDialog { text: String::new(), by_offset: true, error: None, focus: true });
        }
        if ctx.input_mut(|i| i.consume_shortcut(&sc(cmd, Key::Equals)) || i.consume_shortcut(&sc(cmd, Key::Plus))) {
            self.settings.font_size = (self.settings.font_size + 1.0).min(72.0);
        }
        if ctx.input_mut(|i| i.consume_shortcut(&sc(cmd, Key::Minus))) {
            self.settings.font_size = (self.settings.font_size - 1.0).max(6.0);
        }
        if ctx.input_mut(|i| i.consume_shortcut(&sc(cmd, Key::Num0))) {
            self.settings.font_size = 14.0;
        }
        if ctx.input_mut(|i| i.consume_shortcut(&sc(Modifiers::NONE, Key::F12))) {
            self.settings.show_perf = !self.settings.show_perf;
        }
        if ctx.input_mut(|i| i.consume_shortcut(&sc(Modifiers::ALT, Key::Z))) {
            if let Some(d) = self.active_doc() {
                d.view.wrap = !d.view.wrap;
                d.view.scroll_to_caret = true;
            }
        }
        if ctx.input_mut(|i| i.consume_shortcut(&sc(Modifiers::CTRL, Key::Tab))) && !self.docs.is_empty() {
            self.active = (self.active + 1) % self.docs.len();
        }
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("New\tCtrl+N").clicked() {
                    self.new_doc();
                }
                if ui.button("Open…\tCtrl+O").clicked() {
                    self.open_dialog();
                }
                if ui.button("Save\tCtrl+S").clicked() {
                    self.save(false);
                }
                if ui.button("Save As…\tCtrl+Shift+S").clicked() {
                    self.save(true);
                }
                ui.checkbox(&mut self.settings.backup_on_save, "Keep backup (.bak) on save");
                if let Some(d) = self.docs.get_mut(self.active) {
                    ui.menu_button("Line endings on save", |ui| {
                        ui.radio_value(&mut d.eol_on_save, None, "Keep as is");
                        ui.radio_value(&mut d.eol_on_save, Some(LineEnding::Lf), "Convert to LF");
                        ui.radio_value(&mut d.eol_on_save, Some(LineEnding::CrLf), "Convert to CRLF");
                        ui.radio_value(&mut d.eol_on_save, Some(LineEnding::Cr), "Convert to CR");
                    });
                }
                ui.menu_button("Reopen with encoding", |ui| {
                    for enc in Encoding::ALL {
                        if ui.button(enc.name()).clicked() {
                            self.reopen_with(enc);
                        }
                    }
                });
                ui.separator();
                if ui.button("Close tab\tCtrl+W").clicked() {
                    if let Some(id) = self.docs.get(self.active).map(|d| d.id) {
                        self.close_tab(id, false);
                    }
                }
                if ui.button("Exit").clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("Edit", |ui| {
                let ro = self.docs.get(self.active).map(|d| d.read_only).unwrap_or(false);
                if let Some(d) = self.docs.get_mut(self.active) {
                    if ui.add_enabled(!d.undo.is_empty(), egui::Button::new("Undo\tCtrl+Z")).clicked() {
                        d.undo();
                    }
                    if ui.add_enabled(!d.redo.is_empty(), egui::Button::new("Redo\tCtrl+Y")).clicked() {
                        d.redo();
                    }
                    ui.separator();
                    if ui.button("Select all\tCtrl+A").clicked() {
                        let len = d.buf.len();
                        d.view.anchor = d.min_caret();
                        d.view.caret = len;
                    }
                    if ui.button("Duplicate line\tCtrl+D").clicked() {
                        d.duplicate_line();
                    }
                    if ui.button("Delete line\tCtrl+Shift+K").clicked() {
                        d.delete_line();
                    }
                    ui.separator();
                    let mut r = ro;
                    if ui.checkbox(&mut r, "Read-only").changed() {
                        d.read_only = r;
                    }
                }
            });
            ui.menu_button("Search", |ui| {
                if ui.button("Find…\tCtrl+F").clicked() {
                    self.open_search(false);
                }
                if ui.button("Replace…\tCtrl+H").clicked() {
                    self.open_search(true);
                }
                if ui.button("Find next\tF3").clicked() {
                    self.find_next(true);
                }
                if ui.button("Find previous\tShift+F3").clicked() {
                    self.find_next(false);
                }
                if ui.button("Find all").clicked() {
                    self.open_search(false);
                    self.find_all();
                }
                ui.separator();
                if ui.button("Go to line…\tCtrl+G").clicked() {
                    self.goto = Some(GotoDialog { text: String::new(), by_offset: false, error: None, focus: true });
                }
                if ui.button("Go to byte offset…\tCtrl+Shift+G").clicked() {
                    self.goto = Some(GotoDialog { text: String::new(), by_offset: true, error: None, focus: true });
                }
            });
            ui.menu_button("View", |ui| {
                if let Some(d) = self.docs.get_mut(self.active) {
                    if ui.checkbox(&mut d.view.wrap, "Word wrap\tAlt+Z").changed() {
                        d.view.scroll_to_caret = true;
                    }
                    if ui.checkbox(&mut d.hex, "Hex view").changed() {
                        d.view.scroll_to_caret = true;
                    }
                    ui.checkbox(&mut d.follow, "Follow (tail) file changes");
                }
                if ui.checkbox(&mut self.settings.dark, "Dark theme").changed() {
                    ui.ctx().set_theme(if self.settings.dark { egui::Theme::Dark } else { egui::Theme::Light });
                }
                ui.separator();
                if ui.button("Zoom in\tCtrl+=").clicked() {
                    self.settings.font_size = (self.settings.font_size + 1.0).min(72.0);
                }
                if ui.button("Zoom out\tCtrl+-").clicked() {
                    self.settings.font_size = (self.settings.font_size - 1.0).max(6.0);
                }
                if ui.button("Reset zoom\tCtrl+0").clicked() {
                    self.settings.font_size = 14.0;
                }
                ui.checkbox(&mut self.settings.show_perf, "Performance overlay\tF12");
            });
            ui.menu_button("Help", |ui| {
                if ui.button("Keyboard shortcuts / About").clicked() {
                    self.show_about = true;
                }
            });
        });
    }

    fn reopen_with(&mut self, enc: Encoding) {
        let Some(d) = self.docs.get_mut(self.active) else { return };
        let Some(path) = d.path.clone() else { return };
        if d.is_modified() {
            self.error = Some("Save or discard your changes before reopening with another encoding.".into());
            return;
        }
        d.state = DocState::Loading;
        d.index_cancel.cancel();
        let id = d.id;
        self.spawn_open(id, path, Some(enc));
    }

    fn tab_bar(&mut self, ui: &mut egui::Ui) {
        let mut close = None;
        egui::ScrollArea::horizontal().show(ui, |ui| {
            ui.horizontal(|ui| {
                for (i, d) in self.docs.iter().enumerate() {
                    let mut label = d.title.clone();
                    if d.is_modified() {
                        label.push_str(" ●");
                    }
                    let r = ui.selectable_label(i == self.active, label);
                    if r.clicked() {
                        self.active = i;
                    }
                    if r.middle_clicked() {
                        close = Some(d.id);
                    }
                    let r = r.on_hover_text(d.path.as_deref().map(file::display_path).unwrap_or_default());
                    let _ = r;
                    if ui.small_button("×").clicked() {
                        close = Some(d.id);
                    }
                    ui.separator();
                }
            });
        });
        if let Some(id) = close {
            self.close_tab(id, false);
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let Some(d) = self.docs.get(self.active) else { return };
        ui.horizontal(|ui| {
            if d.is_ready() {
                let caret = d.view.caret;
                let (line, exact) = d.buf.line_of(caret);
                let col = match d.buf.line_start(line) {
                    Some((ls, true)) if caret >= ls && caret - ls <= 128 * 1024 => {
                        let b = d.buf.read(ls..caret);
                        b.map(|b| format!("{}", bigtext::text::rows::cell_width(&d.buf.codec, &b, ls) + 1)).unwrap_or_else(|| "…".into())
                    }
                    Some((ls, _)) if caret >= ls => format!("~{}", caret - ls + 1),
                    _ => "?".into(),
                };
                ui.label(format!("Ln {}{}, Col {}", if exact { "" } else { "~" }, line + 1, col));
                ui.separator();
                ui.label(format!("Offset {caret} (0x{caret:X})"));
                let (s, e) = d.selection();
                if s != e {
                    ui.separator();
                    ui.label(format!("Sel {} bytes", e - s));
                }
                let (lines, lexact) = d.buf.line_count();
                ui.separator();
                ui.label(format!("{}{} lines", if lexact { "" } else { "~" }, lines));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some(j) = &d.save_job {
                    if ui.small_button("Cancel").clicked() {
                        j.cancel.cancel();
                    }
                    ui.add(egui::ProgressBar::new(j.progress.fraction()).desired_width(160.0).text(format!("{} {:.0}%", j.label, j.progress.fraction() * 100.0)));
                    ui.separator();
                }
                if d.follow {
                    ui.label(RichText::new("FOLLOW").color(Color32::from_rgb(90, 180, 90)));
                    ui.separator();
                }
                if let Some(r) = d.results.as_ref().filter(|r| r.is_running()) {
                    ui.add(egui::ProgressBar::new(r.progress.fraction()).desired_width(140.0).text(format!("Searching {:.0}%", r.progress.fraction() * 100.0)));
                    ui.separator();
                } else if d.find_job.is_some() {
                    ui.label("Searching…");
                    ui.separator();
                }
                if d.read_only {
                    ui.label(RichText::new("READ-ONLY").color(Color32::from_rgb(220, 150, 60)));
                    ui.separator();
                }
                ui.label(d.buf.source.kind_name());
                ui.separator();
                ui.label(human_bytes(d.buf.len()));
                ui.separator();
                ui.label(d.line_ending.name());
                ui.separator();
                ui.label(d.buf.codec.enc.name());
                if d.is_ready() && !d.buf.index.is_done() && !d.buf.is_empty() {
                    ui.separator();
                    ui.add(egui::ProgressBar::new(d.buf.index.progress()).desired_width(120.0).text(format!("Indexing {:.0}%", d.buf.index.progress() * 100.0)));
                } else if let Some(t) = d.buf.index.finished_after() {
                    if t > Duration::ZERO {
                        ui.separator();
                        ui.label(format!("Indexed in {:.2}s", t.as_secs_f32()));
                    }
                }
            });
        });
    }

    fn banner(&mut self, ui: &mut egui::Ui) {
        let Some(d) = self.docs.get_mut(self.active) else { return };
        if d.hex && d.banner == Some(Banner::Binary) {
            d.banner = None;
        }
        let Some(b) = d.banner.clone() else { return };
        egui::Frame::new().fill(ui.visuals().faint_bg_color).inner_margin(6.0).show(ui, |ui| {
            ui.horizontal(|ui| {
                match &b {
                    Banner::Binary => {
                        ui.label("This file looks binary.");
                        if ui.button("Switch to hex view").clicked() {
                            d.hex = true;
                            d.banner = None;
                        }
                    }
                    Banner::ChangedOnDisk { deleted, grown } => {
                        ui.label(if *deleted {
                            "The file was deleted or moved on disk."
                        } else if *grown {
                            "The file grew on disk."
                        } else {
                            "The file changed on disk."
                        });
                        if !*deleted && ui.button(if d.is_modified() { "Reload (discard edits)" } else { "Reload" }).clicked() {
                            d.banner = None;
                            reload(d, &self.tx, &self.waker, false);
                            return;
                        }
                        if *grown && !*deleted && !d.is_modified() && ui.button("Follow").clicked() {
                            d.banner = None;
                            d.follow = true;
                            reload(d, &self.tx, &self.waker, true);
                            return;
                        }
                    }
                    Banner::Info(s) => {
                        ui.label(s);
                    }
                    Banner::Error(s) => {
                        ui.colored_label(ui.visuals().error_fg_color, s);
                    }
                }
                if ui.button("Dismiss").clicked() {
                    d.banner = None;
                }
            });
        });
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if let Some(g) = &mut self.goto {
            let mut open = true;
            let mut submit = false;
            egui::Window::new(if g.by_offset { "Go to byte offset" } else { "Go to line" })
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut g.by_offset, false, "Line");
                        ui.radio_value(&mut g.by_offset, true, "Byte offset (dec or 0x hex)");
                    });
                    let r = ui.text_edit_singleline(&mut g.text);
                    if g.focus {
                        r.request_focus();
                        g.focus = false;
                    }
                    if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        submit = true;
                    }
                    if let Some(e) = &g.error {
                        ui.colored_label(ui.visuals().error_fg_color, e);
                    }
                    if ui.button("Go").clicked() {
                        submit = true;
                    }
                });
            if submit {
                let t = g.text.trim().replace(['_', ','], "");
                let parsed = if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
                    u64::from_str_radix(h, 16).ok()
                } else {
                    t.parse::<u64>().ok()
                };
                match parsed {
                    Some(v) => {
                        let by_offset = g.by_offset;
                        if let Some(d) = self.docs.get_mut(self.active) {
                            if by_offset {
                                d.goto_offset(v);
                            } else {
                                d.goto_line(v);
                            }
                        }
                        self.goto = None;
                    }
                    None => g.error = Some("Enter a number".into()),
                }
            } else if !open || ctx.input(|i| i.key_pressed(Key::Escape)) {
                self.goto = None;
            }
        }

        if self.confirm.is_some() {
            // 0 = cancel, 1 = primary, 2 = save (close-tab only)
            let mut decision = None;
            let c = self.confirm.as_mut().unwrap();
            let is_close = matches!(c.action, ConfirmAction::CloseTab(_));
            egui::Modal::new(egui::Id::new("confirm")).show(ctx, |ui| {
                ui.set_max_width(460.0);
                ui.heading(&c.title);
                ui.label(&c.message);
                if let Some((label, v)) = &mut c.checkbox {
                    ui.checkbox(v, label.as_str());
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if is_close && ui.button("Save").clicked() {
                        decision = Some(2);
                    }
                    let primary = match c.action {
                        ConfirmAction::CloseTab(_) | ConfirmAction::Exit => "Discard",
                        ConfirmAction::ReplaceOnDisk { .. } => "Replace on disk",
                        ConfirmAction::CopyLarge { .. } => "Copy",
                    };
                    if ui.button(primary).clicked() {
                        decision = Some(1);
                    }
                    if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                        decision = Some(0);
                    }
                });
            });
            if let Some(dec) = decision {
                let c = self.confirm.take().unwrap();
                let checked = c.checkbox.as_ref().map(|x| x.1).unwrap_or(false);
                match (dec, c.action) {
                    (2, ConfirmAction::CloseTab(id)) => {
                        if let Some(i) = self.docs.iter().position(|d| d.id == id) {
                            self.active = i;
                            self.docs[i].close_after_save = true;
                            self.save(false);
                        }
                    }
                    (1, ConfirmAction::CloseTab(id)) => self.close_tab(id, true),
                    (1, ConfirmAction::Exit) => {
                        self.exit_confirmed = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    (1, ConfirmAction::CopyLarge { doc, cut }) => self.start_copy_job(doc, cut),
                    (1, ConfirmAction::ReplaceOnDisk { doc }) => self.replace_on_disk(doc, checked),
                    _ => self.pending_replace = None,
                }
            }
        }

        if let Some((doc, items, lines)) = &self.preview {
            let mut open = true;
            let mut jump = None;
            let same_doc = self.docs.get(self.active).map(|d| d.id) == Some(*doc);
            egui::Window::new(format!("Replace preview (first {})", items.len())).open(&mut open).default_size([640.0, 400.0]).show(ctx, |ui| {
                if items.is_empty() {
                    ui.label("No matches.");
                }
                egui::ScrollArea::vertical().show(ui, |ui| {
                    egui::Grid::new("preview").striped(true).show(ui, |ui| {
                        for (i, (off, before, after)) in items.iter().enumerate() {
                            if ui.link(format!("{}", lines.get(i).copied().unwrap_or(0))).clicked() {
                                jump = Some(*off);
                            }
                            ui.monospace(before.replace('\n', "⏎"));
                            ui.label("→");
                            ui.monospace(after.replace('\n', "⏎"));
                            ui.end_row();
                        }
                    });
                });
            });
            if let (Some(off), true) = (jump, same_doc) {
                if let Some(d) = self.docs.get_mut(self.active) {
                    d.goto_offset(off);
                }
            }
            if !open {
                self.preview = None;
            }
        }

        if let Some(e) = self.error.clone() {
            egui::Modal::new(egui::Id::new("error")).show(ctx, |ui| {
                ui.heading("Error");
                ui.label(e);
                if ui.button("OK").clicked() {
                    self.error = None;
                }
            });
        }

        if self.show_about {
            egui::Window::new("BigText").open(&mut self.show_about).collapsible(false).show(ctx, |ui| {
                ui.label("A fast, non-freezing large-file text editor.");
                ui.separator();
                egui::Grid::new("keys").striped(true).show(ui, |ui| {
                    for (k, v) in SHORTCUTS {
                        ui.monospace(*k);
                        ui.label(*v);
                        ui.end_row();
                    }
                });
            });
        }
    }

    fn perf_overlay(&self, ctx: &egui::Context) {
        if !self.settings.show_perf {
            return;
        }
        let d = self.docs.get(self.active);
        egui::Area::new(egui::Id::new("perf")).anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-20.0, -40.0)).show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.monospace(format!(
                    "frame {:.2} ms  avg {:.2}  max {:.2}",
                    self.frame.last_ms,
                    self.frame.avg(),
                    self.frame.max()
                ));
                if let Some(p) = bigtext::sysinfo::private_bytes() {
                    ui.monospace(format!("private memory {}", human_bytes(p)));
                }
                if let Some(d) = d {
                    ui.monospace(format!(
                        "index {} ({:.4}% of file)  pieces {}",
                        human_bytes(d.buf.index.memory_bytes() as u64),
                        d.buf.index.memory_bytes() as f64 * 100.0 / d.buf.len().max(1) as f64,
                        d.buf.pieces().len()
                    ));
                    if let (Some(o), Some(fp)) = (d.open_elapsed, d.view.first_paint) {
                        ui.monospace(format!("open {:.1} ms, first paint {:.1} ms", o.as_secs_f64() * 1e3, fp.duration_since(d.opened_at).as_secs_f64() * 1e3));
                    }
                }
                if let Some(f) = self.first_frame {
                    ui.monospace(format!("startup → first frame {:.1} ms", f.as_secs_f64() * 1e3));
                }
            });
        });
    }

    fn writes_in_progress(&self) -> bool {
        self.docs.iter().any(|d| d.save_job.is_some() || matches!(d.state, DocState::Finalizing(_)))
    }

    fn handle_close_request(&mut self, ctx: &egui::Context) {
        if self.exit_after_jobs && !self.writes_in_progress() {
            self.exit_after_jobs = false;
            self.exit_confirmed = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if ctx.input(|i| i.viewport().close_requested()) && self.writes_in_progress() {
            // Let a running save/replace cancel cleanly (temp file removed) or finish its
            // atomic swap before the process exits.
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            for d in &self.docs {
                if let (Some(j), false) = (&d.save_job, matches!(d.state, DocState::Finalizing(_))) {
                    j.cancel.cancel();
                }
            }
            self.exit_after_jobs = true;
            return;
        }
        if ctx.input(|i| i.viewport().close_requested()) && !self.exit_confirmed {
            let n = self.docs.iter().filter(|d| d.is_modified()).count();
            if n > 0 {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.confirm = Some(Confirm {
                    title: "Unsaved changes".into(),
                    message: format!("{n} document(s) have unsaved changes. Exit and discard them?"),
                    action: ConfirmAction::Exit,
                    checkbox: None,
                });
            } else {
                for d in &mut self.docs {
                    d.cancel_jobs();
                    d.watch_cancel.cancel();
                    if let Some(j) = &d.save_job {
                        // Cancelling removes the temp file; the original is untouched.
                        j.cancel.cancel();
                    }
                }
            }
        }
    }
}

/// Starts a background watcher that reports size/mtime changes and deletion of the file.
pub(super) fn watch(d: &mut Document, tx: Sender<Msg>, waker: Waker) {
    d.watch_cancel.cancel();
    let cancel = CancelToken::new();
    d.watch_cancel = cancel.clone();
    let Some(path) = d.path.clone() else { return };
    let (len0, mtime0) = (d.buf.source.len(), d.buf.source.modified());
    let id = d.id;
    jobs::spawn("watch", move || {
        let mut last: Option<(bool, u64, Option<std::time::SystemTime>)> = None;
        while !cancel.is_cancelled() {
            for _ in 0..10 {
                std::thread::sleep(Duration::from_millis(100));
                if cancel.is_cancelled() {
                    return;
                }
            }
            let state = match std::fs::metadata(&path) {
                Ok(m) => (false, m.len(), m.modified().ok()),
                Err(_) => (true, 0, None),
            };
            let changed = state.0 || state.1 != len0 || state.2 != mtime0;
            if changed && last != Some(state) {
                last = Some(state);
                let _ = tx.send(Msg::FileChanged { doc: id, deleted: state.0, grown: state.1 > len0 });
                waker();
            }
        }
    });
}

/// Reopens the document from disk. With `append`, the file is assumed to have only grown, so
/// the existing line index is carried over and indexing resumes where it stopped.
pub(super) fn reload(d: &mut Document, tx: &Sender<Msg>, waker: &Waker, append: bool) {
    let Some(path) = d.path.clone() else { return };
    d.reload_prefix = if append { Some(d.buf.index.prefix_checkpoints(d.buf.source.len())) } else { None };
    let enc = d.buf.codec.enc;
    let id = d.id;
    let tx = tx.clone();
    let waker = waker.clone();
    jobs::spawn("reload", move || {
        let result = open_worker(&path, Some(enc));
        let _ = tx.send(Msg::Opened { doc: id, result });
        waker();
    });
}

thread_local! {
    static CTX: std::cell::RefCell<Option<egui::Context>> = const { std::cell::RefCell::new(None) };
}

fn is_read_only(path: Option<&Path>) -> bool {
    path.and_then(|p| std::fs::metadata(p).ok()).map(|m| m.permissions().readonly()).unwrap_or(false)
}

pub const SHORTCUTS: &[(&str, &str)] = &[
    ("Ctrl+O / Ctrl+N", "Open / new"),
    ("Ctrl+S / Ctrl+Shift+S", "Save / save as"),
    ("Ctrl+W, Ctrl+Tab", "Close tab, next tab"),
    ("Ctrl+F / Ctrl+H", "Find / replace"),
    ("F3 / Shift+F3", "Find next / previous"),
    ("Ctrl+G / Ctrl+Shift+G", "Go to line / byte offset"),
    ("Ctrl+Z / Ctrl+Y", "Undo / redo"),
    ("Ctrl+D / Ctrl+Shift+K", "Duplicate line / delete line"),
    ("Ctrl+A", "Select all (large-file safe)"),
    ("Ctrl+wheel, Ctrl+= / Ctrl+-", "Zoom"),
    ("Alt+Z", "Toggle word wrap"),
    ("F12", "Performance overlay"),
];

impl eframe::App for BigTextApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let t0 = Instant::now();
        let ctx = ui.ctx().clone();
        CTX.with(|c| *c.borrow_mut() = Some(ctx.clone()));
        self.poll_messages();
        self.handle_close_request(&ctx);

        // Dropped files.
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        for p in dropped {
            self.open_path(p);
        }

        if let Some(mut b) = self.bench.take() {
            b.before_frame(self, &ctx);
            self.bench = Some(b);
        }

        let modal_open = self.goto.is_some() || self.confirm.is_some() || self.error.is_some();
        if !modal_open {
            self.shortcuts(&ctx);
        }

        for d in &mut self.docs {
            d.tick();
            d.refine_goto();
        }

        egui::Panel::top("menu").show(ui, |ui| self.menu_bar(ui));
        egui::Panel::top("tabs").show(ui, |ui| self.tab_bar(ui));
        if self.search.open {
            egui::Panel::top("search").show(ui, |ui| self.search_panel(ui));
        }
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        if self.search.show_results && self.docs.get(self.active).is_some_and(|d| d.results.is_some()) {
            egui::Panel::bottom("results").resizable(true).default_size(220.0).show(ui, |ui| self.results_panel(ui));
        }
        if self.docs.get(self.active).is_some_and(|d| d.banner.is_some()) {
            egui::Panel::top("banner").show_separator_line(false).show(ui, |ui| self.banner(ui));
        }

        let mut actions = Vec::new();
        let matcher = self.view_highlighter();
        let font_size = &mut self.settings.font_size;
        egui::CentralPanel::no_frame().show(ui, |ui| {
            if let Some(d) = self.docs.get_mut(self.active) {
                if d.hex && d.is_ready() {
                    hex_view::show(ui, d, *font_size, !modal_open);
                } else {
                    let hl = |b: &bigtext::text::TextBuf, r: std::ops::Range<u64>| match &matcher {
                        Some(m) => bigtext::search::matches_in_view(b, m, r, 2000),
                        None => Vec::new(),
                    };
                    let mut env = text_view::ViewEnv { font_size, highlight: &hl, actions: &mut actions, focus: !modal_open };
                    text_view::show(ui, d, &mut env);
                }
            }
        });
        for a in actions {
            match a {
                text_view::ViewAction::CopyLarge { cut, len } => {
                    if let Some(id) = self.docs.get(self.active).map(|d| d.id) {
                        if len > 100 * 1024 * 1024 {
                            self.confirm = Some(Confirm {
                                title: "Large copy".into(),
                                message: format!("Copy {} to the clipboard? This needs that much memory.", human_bytes(len)),
                                action: ConfirmAction::CopyLarge { doc: id, cut },
                                checkbox: None,
                            });
                        } else {
                            self.start_copy_job(id, cut);
                        }
                    }
                }
            }
        }

        self.dialogs(&ctx);
        self.perf_overlay(&ctx);

        let title = match self.docs.get(self.active) {
            Some(d) => format!("{}{} — BigText", d.title, if d.is_modified() { " ●" } else { "" }),
            None => "BigText".into(),
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));

        // Keep repainting while background work is visible in the UI.
        let busy = self.docs.iter().any(|d| {
            (d.is_ready() && !d.buf.index.is_done())
                || matches!(d.state, DocState::Loading | DocState::Finalizing(_))
                || d.save_job.is_some()
                || d.find_job.is_some()
                || d.results.as_ref().is_some_and(|r| r.is_running())
        });
        if busy {
            ctx.request_repaint_after(Duration::from_millis(100));
        }

        let ms = t0.elapsed().as_secs_f32() * 1000.0;
        self.frame.push(ms);
        if self.first_frame.is_none() {
            self.first_frame = Some(self.process_start.elapsed());
        }
        if let Some(mut b) = self.bench.take() {
            b.after_frame(self, &ctx, ms);
            self.bench = Some(b);
        }
    }
}
