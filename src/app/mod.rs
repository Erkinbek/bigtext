//! Application shell: tabs, menus, dialogs, status bar and background-job plumbing.

pub mod document;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Align, Color32, Key, KeyboardShortcut, Layout, Modifiers, RichText};

use bigtext::encoding::{self, Detected, Encoding};
use bigtext::file::{self, OpenMode, Source};
use bigtext::jobs::{self, human_bytes, Waker};

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
}

pub struct Settings {
    pub font_size: f32,
    pub dark: bool,
    pub show_perf: bool,
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
}

#[derive(Clone)]
enum ConfirmAction {
    CloseTab(u64),
    Exit,
    CopyLarge { doc: u64, cut: bool },
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
    show_about: bool,
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
            settings: Settings { font_size: 14.0, dark: true, show_perf: false },
            goto: None,
            confirm: None,
            error: None,
            frame: FrameStats::default(),
            process_start,
            first_frame: None,
            bench: None,
            exit_confirmed: false,
            show_about: false,
        };
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
                    if let Some(d) = self.doc_mut(doc) {
                        match result {
                            Ok(o) => {
                                d.open_elapsed = Some(o.elapsed);
                                d.read_only = d.read_only || is_read_only(d.path.as_deref());
                                d.install(o.source, o.detected, None, waker);
                            }
                            Err(e) => d.state = DocState::Failed(e),
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
            });
            return;
        }
        let d = self.docs.remove(i);
        d.index_cancel.cancel();
        d.watch_cancel.cancel();
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
                    Some((ls, true)) if caret >= ls && caret - ls <= 1 << 20 => {
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
                if d.is_ready() && !d.buf.index.is_done() && d.buf.len() > 0 {
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

        if let Some(c) = self.confirm.clone_meta() {
            let mut decision = None;
            egui::Modal::new(egui::Id::new("confirm")).show(ctx, |ui| {
                ui.heading(&c.0);
                ui.label(&c.1);
                ui.horizontal(|ui| {
                    if ui.button("Yes").clicked() {
                        decision = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        decision = Some(false);
                    }
                });
            });
            if let Some(yes) = decision {
                let action = self.confirm.take().map(|c| c.action);
                if yes {
                    match action {
                        Some(ConfirmAction::CloseTab(id)) => self.close_tab(id, true),
                        Some(ConfirmAction::Exit) => {
                            self.exit_confirmed = true;
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        Some(ConfirmAction::CopyLarge { doc, cut }) => self.start_copy_job(doc, cut),
                        None => {}
                    }
                }
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

    fn handle_close_request(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested()) && !self.exit_confirmed {
            let n = self.docs.iter().filter(|d| d.is_modified()).count();
            if n > 0 {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.confirm = Some(Confirm {
                    title: "Unsaved changes".into(),
                    message: format!("{n} document(s) have unsaved changes. Exit and discard them?"),
                    action: ConfirmAction::Exit,
                });
            } else {
                for d in &self.docs {
                    d.index_cancel.cancel();
                    d.watch_cancel.cancel();
                }
            }
        }
    }
}

trait ConfirmMeta {
    fn clone_meta(&self) -> Option<(String, String)>;
}

impl ConfirmMeta for Option<Confirm> {
    fn clone_meta(&self) -> Option<(String, String)> {
        self.as_ref().map(|c| (c.title.clone(), c.message.clone()))
    }
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
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        if self.docs.get(self.active).is_some_and(|d| d.banner.is_some()) {
            egui::Panel::top("banner").show_separator_line(false).show(ui, |ui| self.banner(ui));
        }

        let mut actions = Vec::new();
        let font_size = &mut self.settings.font_size;
        egui::CentralPanel::no_frame().show(ui, |ui| {
            if let Some(d) = self.docs.get_mut(self.active) {
                if d.hex && d.is_ready() {
                    hex_view::show(ui, d, *font_size, !modal_open);
                } else {
                    let none = |_: &bigtext::text::TextBuf, _: std::ops::Range<u64>| Vec::new();
                    let mut env = text_view::ViewEnv { font_size, highlight: &none, actions: &mut actions, focus: !modal_open };
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
        let busy = self.docs.iter().any(|d| (d.is_ready() && !d.buf.index.is_done()) || matches!(d.state, DocState::Loading | DocState::Finalizing(_)));
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
