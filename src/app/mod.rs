//! Application shell: tabs, menus, dialogs, status bar and background-job plumbing.

pub mod columns;
pub mod commands;
pub mod csv_tools;
pub mod document;
pub mod export;
pub mod extras;
pub mod fields_panel;
pub mod filter_panel;
pub mod find_in_files;
pub mod follow_id;
pub mod gaps_panel;
pub mod fonts;
pub mod goto;
pub mod i18n;
mod i18n_texts;
pub mod json_panel;
pub mod levels;
pub mod marks;
pub mod match_bracket;
pub mod match_values;
pub mod merge_logs;
pub mod multi_cursor;
pub mod nav;
pub mod ops;
pub mod patterns_window;
pub mod portable;
pub mod saved_searches;
pub mod search_panel;
pub mod sessions;
pub mod tabs;
pub mod tasks;
pub mod text_tools;
pub mod time_range;
pub mod timeline;
pub mod update;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Align, Color32, Key, Layout, RichText};

use bigtext::encoding::{self, Detected, Encoding};
use bigtext::file::{self, OpenMode, Source};
use bigtext::jobs::{self, human_bytes, CancelToken, Waker};
use bigtext::replace::Template;
use bigtext::search::Matcher;
use bigtext::text::Piece;

use crate::view::{hex_view, table_view, text_view};
use document::{Banner, DocState, Document, Restore};

/// What the command line asks for.
#[derive(Default)]
pub struct StartOptions {
    /// Files with the line to open them at.
    pub files: Vec<(PathBuf, Option<u64>)>,
    pub find: Option<bigtext::search::Query>,
    pub follow: bool,
    /// Temporary file that standard input is copied into.
    pub stdin: Option<PathBuf>,
}

pub struct Opened {
    pub source: Arc<Source>,
    pub detected: Detected,
    pub elapsed: Duration,
    /// Canonical path (resolved on the worker; can be slow on network drives).
    pub path: PathBuf,
    pub read_only: bool,
    /// The compressed file that `path` was unpacked from.
    pub unpacked: Option<PathBuf>,
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
    Update(update::Event),
    ColumnsDeleted(Box<columns::ColumnsDeleted>),
    TaskDone { id: u64, outcome: tasks::TaskOutcome },
    TextOp(Box<text_tools::TextOpDone>),
    CsvRewritten(Box<csv_tools::CsvRewritten>),
    TimeFound { doc: u64, version: u64, result: Result<u64, bigtext::timestamp::TimeError> },
    LevelFound(Box<levels::LevelFound>),
    CursorFound(Box<multi_cursor::CursorFound>),
    BracketJump(Box<match_bracket::BracketJump>),
    Json(Box<json_panel::JsonDone>),
    TimeRange(Box<time_range::RangeDone>),
}

pub struct Settings {
    pub font_size: f32,
    pub dark: bool,
    pub show_perf: bool,
    pub backup_on_save: bool,
    pub syntax: bool,
    /// Highlight patterns (View → Highlights).
    pub marks: Vec<marks::Mark>,
    /// Font file of the text (None: the built-in font).
    pub font_path: Option<PathBuf>,
    /// The log level bar above log files.
    pub level_bar: bool,
    /// Saved searches (the ★ menu of the search bar).
    pub saved: Vec<saved_searches::SavedSearch>,
    /// View → Show whitespace and line breaks.
    pub show_ws: bool,
    /// Filter view: lines shown before and after each match, and whole log entries.
    pub filter_context: u32,
    pub filter_entries: bool,
    /// Keys whose values are ids (None: the defaults).
    pub id_keys: Option<Vec<String>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum GotoMode {
    Line,
    Offset,
    Time,
}

struct GotoDialog {
    text: String,
    mode: GotoMode,
    error: Option<String>,
    focus: bool,
    /// A running go-to-time search.
    busy: Option<CancelToken>,
}

impl GotoDialog {
    fn new(mode: GotoMode) -> Option<GotoDialog> {
        Some(GotoDialog { text: String::new(), mode, error: None, focus: true, busy: None })
    }
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
    DeleteColumns { doc: u64, dialect: bigtext::csv::Dialect, cols: Vec<usize>, on_disk: bool },
    CsvRewrite { doc: u64, dialect: bigtext::csv::Dialect, rewrite: bigtext::csv::Rewrite, on_disk: bool },
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
    pub self_test: Option<crate::self_test::SelfTest>,
    exit_confirmed: bool,
    exit_after_jobs: bool,
    pub show_about: bool,
    pub recent: Vec<PathBuf>,
    /// Session state is saved and restored (off for benchmarks and the self-test).
    pub persist: bool,
    pub diff: Option<extras::DiffResults>,
    pub search: search_panel::SearchUi,
    pending_replace: Option<(u64, Arc<Matcher>, Template, std::ops::Range<u64>)>,
    preview: Option<(u64, Vec<(u64, String, String)>, Vec<u64>)>,
    pub update: update::UpdateUi,
    pub tasks: Vec<tasks::Task>,
    next_task: u64,
    pub notice: Option<tasks::Notice>,
    pub marks: marks::MarksUi,
    pub export: export::ExportUi,
    pub csv: csv_tools::CsvUi,
    pub merge: merge_logs::MergeUi,
    pub saved_ui: saved_searches::SavedUi,
    pairs: match_bracket::PairCache,
    /// View → JSON structure is shown.
    pub json_open: bool,
    /// Document ids, most recently used first (for Ctrl+Tab).
    mru: Vec<u64>,
    pub switcher: Option<tabs::TabSwitcher>,
    pub sessions: sessions::SessionsUi,
    pub portable: portable::PortableUi,
    pub ids: follow_id::IdUi,
    pub fields: fields_panel::FieldsUi,
    pub gaps: gaps_panel::GapsUi,
    pub range: time_range::TimeRangeUi,
    pub patterns: patterns_window::PatternsUi,
    pub nav: nav::NavHistory,
    pub values: Option<match_values::ValuesDialog>,
    pub files: find_in_files::FilesUi,
    pub timeline: timeline::TimelineUi,
    pub keymap: commands::Keymap,
    pub palette: Option<commands::Palette>,
    pub shortcuts_ui: commands::ShortcutsUi,
    pub fonts: fonts::FontsUi,
    /// Matcher for the occurrences of the selected text, by the text it was built for.
    occurrences: Option<(String, bigtext::encoding::Codec, Option<Arc<Matcher>>)>,
    /// Document to search once it is open (command line --find).
    pending_find: Option<u64>,
}

/// Turns I/O errors into messages a user can act on.
pub fn friendly_io(e: &std::io::Error, path: &Path) -> String {
    let p = file::display_path(path);
    match (e.kind(), e.raw_os_error()) {
        (_, Some(32)) | (_, Some(33)) => i18n::trf("“{p}” is in use by another process.", &[("p", &p)]),
        (std::io::ErrorKind::PermissionDenied, _) => i18n::trf("Access denied: “{p}”. Check permissions or the read-only attribute.", &[("p", &p)]),
        (std::io::ErrorKind::NotFound, _) => i18n::trf("File not found: “{p}”.", &[("p", &p)]),
        _ => format!("{p}: {e}"),
    }
}

fn open_worker(path: &Path, forced: Option<Encoding>) -> Result<Opened, String> {
    let t = Instant::now();
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let path = canonical.as_path();
    let src = Source::open(path, OpenMode::Auto).map_err(|e| friendly_io(&e, path))?;
    let read_only = std::fs::metadata(path).map(|m| m.permissions().readonly()).unwrap_or(false);
    let n = src.len().min(1 << 20);
    let whole = n == src.len();
    let det = {
        let sample = src.read(0..n).map_err(|e| friendly_io(&e, path))?;
        match forced {
            Some(enc) => encoding::detect_with(&sample, whole, enc, 0),
            None => encoding::detect(&sample, whole),
        }
    };
    Ok(Opened { source: Arc::new(src), detected: det, elapsed: t.elapsed(), path: canonical.clone(), read_only, unpacked: None })
}

/// Opens `path`, unpacking it into a temporary file first when it is compressed.
fn open_or_unpack(path: &Path, forced: Option<Encoding>, cancel: &CancelToken, progress: &jobs::Progress) -> Result<Opened, String> {
    let Some(kind) = bigtext::decompress::detect(path) else { return open_worker(path, forced) };
    match bigtext::decompress::unpack(path, kind, cancel, progress) {
        Ok(Some(temp)) => match open_worker(&temp, forced) {
            Ok(mut o) => {
                o.unpacked = Some(path.to_path_buf());
                Ok(o)
            }
            Err(e) => {
                bigtext::decompress::remove_temp(&temp);
                Err(e)
            }
        },
        Ok(None) => Err(i18n::tr("Unpacking was cancelled.").into()),
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => Err(format!("{}: {e}", file::display_path(path))),
        Err(e) => Err(i18n::trf("Could not unpack into {dir}: {e}", &[("dir", &file::display_path(&bigtext::decompress::temp_dir())), ("e", &e.to_string())])),
    }
}

/// Lexical path comparison (no file-system access, so it is safe on the UI thread):
/// ignores the `\\?\` prefix, and case and separators on Windows.
pub fn same_path(a: &Path, b: &Path) -> bool {
    fn norm(p: &Path) -> String {
        let s = file::display_path(p);
        let s = std::path::absolute(&s).map(|p| p.to_string_lossy().into_owned()).unwrap_or(s);
        if cfg!(windows) {
            s.replace('/', "\\").to_lowercase()
        } else {
            s
        }
    }
    norm(a) == norm(b)
}

impl BigTextApp {
    pub fn new(cc: &eframe::CreationContext<'_>, start: StartOptions, process_start: Instant, persist: bool) -> Self {
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
            settings: Settings { font_size: 14.0, dark: true, show_perf: false, backup_on_save: false, syntax: true, marks: Vec::new(), font_path: None, level_bar: true, saved: Vec::new(), show_ws: false, filter_context: 0, filter_entries: false, id_keys: None },
            goto: None,
            confirm: None,
            error: None,
            frame: FrameStats::default(),
            process_start,
            first_frame: None,
            bench: None,
            self_test: None,
            exit_confirmed: false,
            exit_after_jobs: false,
            show_about: false,
            recent: Vec::new(),
            persist,
            diff: None,
            search: search_panel::SearchUi::default(),
            pending_replace: None,
            preview: None,
            update: update::UpdateUi::default(),
            tasks: Vec::new(),
            next_task: 0,
            notice: None,
            marks: marks::MarksUi::default(),
            export: export::ExportUi::default(),
            csv: csv_tools::CsvUi::default(),
            merge: merge_logs::MergeUi::default(),
            saved_ui: saved_searches::SavedUi::default(),
            pairs: match_bracket::PairCache::default(),
            json_open: false,
            mru: Vec::new(),
            switcher: None,
            sessions: sessions::SessionsUi::default(),
            portable: portable::PortableUi::default(),
            ids: follow_id::IdUi::default(),
            fields: fields_panel::FieldsUi::default(),
            gaps: gaps_panel::GapsUi::default(),
            range: time_range::TimeRangeUi::default(),
            patterns: patterns_window::PatternsUi::default(),
            nav: nav::NavHistory::default(),
            values: None,
            files: find_in_files::FilesUi::default(),
            timeline: timeline::TimelineUi::default(),
            keymap: commands::Keymap::default(),
            palette: None,
            shortcuts_ui: commands::ShortcutsUi::default(),
            fonts: fonts::FontsUi::default(),
            occurrences: None,
            pending_find: None,
        };
        app.search.query.case_sensitive = true;
        if persist {
            let mut state = extras::load();
            app.update.restore(&state);
            update::cleanup_in_background();
            jobs::spawn("cleanup-unpacked", bigtext::decompress::cleanup_old);
            app.settings.marks = std::mem::take(&mut state.marks);
            app.search.history = std::mem::take(&mut state.history);
            app.settings.level_bar = state.level_bar.unwrap_or(true);
            app.settings.saved = std::mem::take(&mut state.saved);
            app.settings.show_ws = state.show_ws.unwrap_or(false);
            app.settings.id_keys = state.id_keys.take();
            if let Some((n, e)) = state.filter_ctx {
                app.settings.filter_context = n.min(bigtext::filter::CONTEXT_MAX);
                app.settings.filter_entries = e;
            }
            // A font that is gone or broken is forgotten; the built-in one is used.
            if let Some(p) = state.font.take() {
                if fonts::apply_font(&cc.egui_ctx, Some(&p)).is_ok() {
                    app.settings.font_path = Some(p);
                }
            }
            if let Some(l) = state.lang.as_deref().and_then(i18n::Lang::from_code) {
                i18n::set_lang(l);
            }
            for (id, k) in std::mem::take(&mut state.keys) {
                if let (Some(c), Some(k)) = (commands::Cmd::from_id(&id), commands::key_from_string(&k)) {
                    app.keymap.changed.insert(c, k);
                }
            }
            if start.files.is_empty() && start.stdin.is_none() {
                app.restore_session(state);
            } else {
                app.recent = state.recent;
                app.settings.font_size = state.font_size.unwrap_or(14.0).clamp(6.0, 72.0);
                app.settings.dark = state.dark.unwrap_or(true);
                app.settings.syntax = state.syntax.unwrap_or(true);
            }
        }
        cc.egui_ctx.set_theme(if app.settings.dark { egui::Theme::Dark } else { egui::Theme::Light });
        app.apply_start(start);
        if app.docs.is_empty() {
            app.new_doc();
        }
        app
    }

    /// Opens the files from the command line (at their lines, followed, searched).
    pub(crate) fn apply_start(&mut self, start: StartOptions) {
        let follow = start.follow;
        let opened = |app: &mut BigTextApp, path: PathBuf, line: Option<u64>, end: bool| {
            app.open_path(path);
            let Some(d) = app.docs.get_mut(app.active) else { return };
            d.follow |= follow;
            match (line, d.is_ready()) {
                // Already open: move there now.
                (Some(n), true) => d.goto_line(n),
                (None, true) if end => d.set_caret(d.buf.len(), false),
                (_, false) if line.is_some() || end => d.restore = Some(Restore { line, end: line.is_none() && end, ..Default::default() }),
                _ => {}
            }
        };
        for (path, line) in start.files {
            opened(self, path, line, follow);
        }
        if let Some(path) = start.stdin {
            opened(self, path, None, true);
            if let Some(d) = self.docs.get_mut(self.active) {
                d.title = "stdin".into();
                d.temporary = true;
                d.read_only = true;
                d.follow = true;
            }
        }
        if let Some(q) = start.find {
            self.search.query = q;
            self.search.open = true;
            self.pending_find = self.docs.get(self.active).map(|d| d.id);
        }
    }

    /// Runs the command line's `--find` once its document has opened.
    fn run_pending_find(&mut self) {
        let Some(id) = self.pending_find else { return };
        match self.docs.iter().position(|d| d.id == id) {
            // After a --line go-to has its exact position (it can start as an estimate).
            Some(i) if self.docs[i].is_ready() && self.docs[i].view.pending_goto_line.is_none() => {
                self.pending_find = None;
                self.active = i;
                self.find_next(true);
            }
            Some(i) if matches!(self.docs[i].state, DocState::Failed(_)) => self.pending_find = None,
            Some(_) => {}
            None => self.pending_find = None,
        }
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
        let canon = std::path::absolute(&path).unwrap_or(path);
        if let Some(i) = self.docs.iter().position(|d| d.path.as_deref().or(d.unpacked_from.as_deref()).is_some_and(|p| same_path(p, &canon))) {
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

    fn spawn_open(&mut self, id: u64, path: PathBuf, forced: Option<Encoding>) {
        let tx = self.tx.clone();
        let waker = self.waker.clone();
        let (progress, cancel) = (jobs::Progress::default(), CancelToken::new());
        if let Some(d) = self.doc_mut(id) {
            d.unpacking = Some((progress.clone(), cancel.clone()));
        }
        jobs::spawn("open", move || {
            let result = open_or_unpack(&path, forced, &cancel, &progress);
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
                        d.unpacking = None;
                        match result {
                            Ok(o) => {
                                d.open_elapsed = Some(o.elapsed);
                                d.read_only = d.read_only || o.read_only;
                                d.path = Some(o.path.clone());
                                if let Some(orig) = o.unpacked {
                                    d.read_only = true;
                                    d.banner = Some(Banner::Info(i18n::trf(
                                        "Unpacked copy of {name} ({size}), read-only. To keep changes, turn off Edit → Read-only and use Save As.",
                                        &[("name", &d.title), ("size", &human_bytes(o.source.len()))],
                                    )));
                                    d.unpacked_from = Some(orig);
                                    d.temporary = true;
                                }
                                let prefix = d.reload_prefix.take();
                                let follow_end = d.follow && prefix.is_some();
                                d.install(o.source, o.detected, prefix, waker.clone());
                                if follow_end {
                                    let len = d.buf.len();
                                    d.set_caret(len, false);
                                }
                                if let Some(mut r) = d.restore.take() {
                                    d.pending_filter = r.filter.take();
                                    let len = d.buf.len();
                                    d.view.wrap = r.wrap;
                                    d.bookmarks = r.bookmarks.into_iter().filter(|&b| b <= len).collect();
                                    match r.line {
                                        _ if r.select.is_some() => {
                                            let (s, e) = r.select.unwrap_or_default();
                                            d.select(s.min(len), e.min(len));
                                            d.view.center();
                                        }
                                        None if r.end => d.set_caret(len, false),
                                        Some(line) => d.goto_line(line),
                                        None => {
                                            d.set_caret(r.caret.min(len), false);
                                            d.view.top = r.top.min(len);
                                            d.view.scroll_to_caret = false;
                                        }
                                    }
                                }
                                watch(d, tx, waker);
                                // Standard input has no file to remember.
                                if !d.temporary || d.unpacked_from.is_some() {
                                    let p = d.unpacked_from.clone().unwrap_or(o.path);
                                    self.add_recent(&p);
                                }
                            }
                            Err(e) => d.state = DocState::Failed(e),
                        }
                    } else if let Ok(o) = result.as_ref().map(|o| (o.unpacked.is_some(), o.path.clone())) {
                        // The tab was closed while its file was being unpacked.
                        drop(result);
                        if o.0 {
                            jobs::spawn("remove-unpacked", move || bigtext::decompress::remove_temp(&o.1));
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
                Msg::Update(e) => self.on_update_event(e),
                Msg::ColumnsDeleted(c) => self.on_columns_deleted(*c),
                Msg::TaskDone { id, outcome } => self.on_task_done(id, outcome),
                Msg::TextOp(r) => self.on_text_op(*r),
                Msg::CsvRewritten(c) => self.on_csv_rewritten(*c),
                Msg::TimeFound { doc, version, result } => self.on_time_found(doc, version, result),
                Msg::LevelFound(f) => self.on_level_found(*f),
                Msg::CursorFound(f) => self.on_cursor_found(*f),
                Msg::BracketJump(j) => self.on_bracket_jump(*j),
                Msg::Json(j) => self.on_json(*j),
                Msg::TimeRange(r) => self.on_time_range(*r),
            }
        }
    }

    fn on_time_found(&mut self, doc: u64, version: u64, result: Result<u64, bigtext::timestamp::TimeError>) {
        let Some(g) = &mut self.goto else { return };
        if g.busy.take().is_none() {
            return;
        }
        match result {
            Ok(off) => {
                self.goto = None;
                if let Some(d) = self.doc_mut(doc).filter(|d| d.version == version) {
                    d.goto_offset(off);
                }
            }
            Err(bigtext::timestamp::TimeError::NoTimestamps) => g.error = Some(i18n::tr("No timestamps found at the start of the file's lines.").into()),
            Err(bigtext::timestamp::TimeError::Cancelled) => {}
        }
    }

    fn ctx_clone(&self) -> Option<egui::Context> {
        CTX.with(|c| c.borrow().clone())
    }

    fn close_tab(&mut self, id: u64, force: bool) {
        let Some(i) = self.docs.iter().position(|d| d.id == id) else { return };
        if !force && self.docs[i].is_modified() {
            self.confirm = Some(Confirm {
                title: i18n::tr("Unsaved changes").into(),
                message: i18n::trf("“{name}” has unsaved changes. Close it anyway?", &[("name", &self.docs[i].title)]),
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
        if let (Some(temp), true) = (d.path.clone(), d.temporary) {
            drop(d);
            jobs::spawn("remove-unpacked", move || bigtext::decompress::remove_temp(&temp));
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
            let result = snap.read(s..e).map(|b| snap.codec.decode_lossy(&b)).ok_or_else(|| i18n::tr("Could not read the selection").to_string());
            let _ = tx.send(Msg::Copied { doc: id, result, cut });
            waker();
        });
    }

    fn open_dialog(&mut self) {
        if let Some(files) = rfd::FileDialog::new().set_title(i18n::tr("Open")).pick_files() {
            for f in files {
                self.open_path(f);
            }
        }
    }

    // ------------------------------------------------------------------------------------------
    // UI pieces.

    fn reopen_with(&mut self, enc: Encoding) {
        let Some(d) = self.docs.get_mut(self.active) else { return };
        let Some(path) = d.path.clone() else { return };
        if d.is_modified() {
            self.error = Some(i18n::tr("Save or discard your changes before reopening with another encoding.").into());
            return;
        }
        d.state = DocState::Loading;
        d.index_cancel.cancel();
        let id = d.id;
        self.spawn_open(id, path, Some(enc));
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let ids = self.caret_ids();
        let delta = self.caret_time_delta();
        let mut follow = None;
        ui.horizontal(|ui| {
            let Some(d) = self.docs.get(self.active) else { return };
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
                ui.label(i18n::trf("Ln {line}, Col {col}", &[("line", &format!("{}{}", if exact { "" } else { "~" }, line + 1)), ("col", &col)]));
                if let Some(ms) = delta {
                    ui.separator();
                    ui.label(format!("Δ {}", bigtext::timestamp::format_delta(ms))).on_hover_text(i18n::tr("Time since the previous line with a timestamp"));
                }
                if !d.view.extra.is_empty() {
                    ui.separator();
                    ui.label(i18n::trf("{n} cursors", &[("n", &(d.view.extra.len() + 1).to_string())])).on_hover_text(i18n::tr("Esc: back to one cursor"));
                }
                ui.separator();
                ui.label(i18n::trf("Offset {n}", &[("n", &format!("{caret} (0x{caret:X})"))]));
                if let Some((lines, exact, chars, bytes)) = d.selection_stats() {
                    ui.separator();
                    let n = |x: u64| filter_panel::fmt_count(x);
                    let lines = format!("{}{}", if exact { "" } else { "~" }, n(lines));
                    let text = match chars {
                        Some(c) => i18n::trf("Sel: {lines} lines, {chars} chars, {bytes} bytes", &[("lines", &lines), ("chars", &n(c)), ("bytes", &n(bytes))]),
                        None => i18n::trf("Sel: {lines} lines, {bytes} bytes", &[("lines", &lines), ("bytes", &n(bytes))]),
                    };
                    ui.label(text);
                    if let Some(ms) = d.selection_time_delta() {
                        ui.separator();
                        ui.label(format!("Δt {}", bigtext::timestamp::format_delta(ms))).on_hover_text(i18n::tr("Time between the timestamps of the first and last selected lines"));
                    }
                }
                if let Some(b) = d.view.block {
                    let (lo, hi) = b.cells();
                    ui.separator();
                    ui.label(i18n::trf("Block: {cols} columns wide", &[("cols", &(hi - lo).to_string())]));
                }
                let (lines, lexact) = d.buf.line_count();
                ui.separator();
                ui.label(i18n::trf("{n} lines", &[("n", &format!("{}{}", if lexact { "" } else { "~" }, lines))]));
                if !ids.is_empty() {
                    ui.separator();
                    follow = follow_id::id_chips(ui, &ids);
                }
                // The colours of a merged log's files.
                if let Some(t) = &d.merged {
                    ui.separator();
                    let dark = ui.visuals().dark_mode;
                    for (i, l) in t.labels.iter().enumerate() {
                        ui.label(RichText::new(l).background_color(marks::fill(i, dark)));
                    }
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                self.tasks_status(ui);
                let Some(d) = self.docs.get(self.active) else { return };
                if let Some(j) = &d.save_job {
                    if ui.small_button(i18n::tr("Cancel")).clicked() {
                        j.cancel.cancel();
                    }
                    ui.add(egui::ProgressBar::new(j.progress.fraction()).desired_width(160.0).text(format!("{} {:.0}%", j.label, j.progress.fraction() * 100.0)));
                    ui.separator();
                }
                if d.follow {
                    ui.label(RichText::new(i18n::tr("FOLLOW")).color(Color32::from_rgb(90, 180, 90)));
                    ui.separator();
                }
                if d.table && !d.hex {
                    let delim = d.table_state.dialect.map(|x| i18n::tr(x.name())).unwrap_or("…");
                    ui.label(RichText::new(format!("{} · {delim}", i18n::tr("TABLE"))).color(Color32::from_rgb(90, 160, 220)));
                    ui.separator();
                }
                if let Some(r) = d.results.as_ref().filter(|r| r.is_running()) {
                    ui.add(egui::ProgressBar::new(r.progress.fraction()).desired_width(140.0).text(format!("{} {:.0}%", i18n::tr("Searching"), r.progress.fraction() * 100.0)));
                    ui.separator();
                } else if d.find_job.is_some() {
                    ui.label(i18n::tr("Searching…"));
                    ui.separator();
                }
                if d.read_only {
                    ui.label(RichText::new(i18n::tr("READ-ONLY")).color(Color32::from_rgb(220, 150, 60)));
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
                    ui.add(egui::ProgressBar::new(d.buf.index.progress()).desired_width(120.0).text(format!("{} {:.0}%", i18n::tr("Indexing"), d.buf.index.progress() * 100.0)));
                } else if let Some(t) = d.buf.index.finished_after() {
                    if t > Duration::ZERO {
                        ui.separator();
                        ui.label(i18n::trf("Indexed in {t}s", &[("t", &format!("{:.2}", t.as_secs_f32()))]));
                    }
                }
            });
        });
        if let Some(id) = follow {
            self.follow_id(&id);
        }
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
                        ui.label(i18n::tr("This file looks binary."));
                        if ui.button(i18n::tr("Switch to hex view")).clicked() {
                            d.hex = true;
                            d.banner = None;
                        }
                    }
                    Banner::ChangedOnDisk { deleted, grown } => {
                        ui.label(if *deleted {
                            i18n::tr("The file was deleted or moved on disk.")
                        } else if *grown {
                            i18n::tr("The file grew on disk.")
                        } else {
                            i18n::tr("The file changed on disk.")
                        });
                        if !*deleted && ui.button(i18n::tr(if d.is_modified() { "Reload (discard edits)" } else { "Reload" })).clicked() {
                            d.banner = None;
                            reload(d, &self.tx, &self.waker, false);
                            return;
                        }
                        if *grown && !*deleted && !d.is_modified() && ui.button(i18n::tr("Follow")).clicked() {
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
                if ui.button(i18n::tr("Dismiss")).clicked() {
                    d.banner = None;
                }
            });
        });
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if let Some(g) = &mut self.goto {
            let mut open = true;
            let mut submit = false;
            let title = match g.mode {
                GotoMode::Line => "Go to line",
                GotoMode::Offset => "Go to byte offset",
                GotoMode::Time => "Go to time",
            };
            egui::Window::new(i18n::tr(title))
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut g.mode, GotoMode::Line, i18n::tr("Line"));
                        ui.radio_value(&mut g.mode, GotoMode::Offset, i18n::tr("Byte offset (dec or 0x hex)"));
                        ui.radio_value(&mut g.mode, GotoMode::Time, i18n::tr("Time"));
                    });
                    let hint = match g.mode {
                        GotoMode::Time => "14:32:05 · 2024-05-01 14:32:05",
                        GotoMode::Line => "1234 · 50% · +100 · -100 · $-100",
                        GotoMode::Offset => "4096 · 0x1F00 · 50% · +0x100 · $-4096",
                    };
                    let r = ui.add(egui::TextEdit::singleline(&mut g.text).hint_text(hint));
                    if g.focus {
                        r.request_focus();
                        g.focus = false;
                    }
                    if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        submit = true;
                    }
                    if g.mode == GotoMode::Time {
                        ui.weak(i18n::tr("Jumps to the first line at or after this time (the log must be in time order)."));
                    } else {
                        ui.weak(i18n::tr("+N / -N: from the caret; $-N: from the end; 50%: a share of the file."));
                    }
                    if let Some(e) = &g.error {
                        ui.colored_label(ui.visuals().error_fg_color, e);
                    }
                    ui.horizontal(|ui| {
                        if g.busy.is_some() {
                            ui.spinner();
                            ui.label(i18n::tr("Searching…"));
                        } else if ui.button(i18n::tr("Go")).clicked() {
                            submit = true;
                        }
                    });
                });
            if submit && g.busy.is_none() && g.mode == GotoMode::Time {
                match bigtext::timestamp::parse(g.text.trim()) {
                    Some(st) => {
                        g.error = None;
                        let cancel = CancelToken::new();
                        g.busy = Some(cancel.clone());
                        if let Some(d) = self.docs.get(self.active) {
                            let (snap, id, version) = (d.buf.snapshot(), d.id, d.version);
                            let tx = self.tx.clone();
                            let waker = self.waker.clone();
                            jobs::spawn("goto-time", move || {
                                let result = bigtext::timestamp::find_time(&snap, st, &cancel);
                                if !cancel.is_cancelled() {
                                    let _ = tx.send(Msg::TimeFound { doc: id, version, result });
                                    waker();
                                }
                            });
                        }
                    }
                    None => g.error = Some(i18n::tr("Enter a time like 14:32:05 or 2024-05-01 14:32:05").into()),
                }
            } else if submit && g.busy.is_none() {
                match goto::parse(&g.text, g.mode == GotoMode::Offset) {
                    Some(t) => {
                        if let Some(d) = self.docs.get_mut(self.active) {
                            d.goto_target(t);
                        }
                        self.goto = None;
                    }
                    None => g.error = Some(i18n::tr("Enter a number").into()),
                }
            } else if !open || ctx.input(|i| i.key_pressed(Key::Escape)) {
                if let Some(c) = &g.busy {
                    c.cancel();
                }
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
                    if is_close && ui.button(i18n::tr("Save")).clicked() {
                        decision = Some(2);
                    }
                    let primary = match c.action {
                        ConfirmAction::CloseTab(_) | ConfirmAction::Exit => "Discard",
                        ConfirmAction::ReplaceOnDisk { .. } => "Replace on disk",
                        ConfirmAction::CopyLarge { .. } => "Copy",
                        ConfirmAction::DeleteColumns { .. } => "Delete",
                        ConfirmAction::CsvRewrite { .. } => "Rewrite",
                    };
                    if ui.button(i18n::tr(primary)).clicked() {
                        decision = Some(1);
                    }
                    if ui.button(i18n::tr("Cancel")).clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                        decision = Some(0);
                    }
                });
            });
            if let Some(dec) = decision {
                let c = self.confirm.take().unwrap();
                self.apply_confirm(ctx, dec, c);
            }
        }

        if let Some((doc, items, lines)) = &self.preview {
            let mut open = true;
            let mut jump = None;
            let same_doc = self.docs.get(self.active).map(|d| d.id) == Some(*doc);
            egui::Window::new(i18n::trf("Replace preview (first {n})", &[("n", &items.len().to_string())])).open(&mut open).default_size([640.0, 400.0]).show(ctx, |ui| {
                if items.is_empty() {
                    ui.label(i18n::tr("No matches."));
                }
                egui::ScrollArea::vertical().show(ui, |ui| {
                    egui::Grid::new("preview").striped(true).show(ui, |ui| {
                        for (i, (off, before, after)) in items.iter().enumerate() {
                            if ui.link(format!("{}", lines.get(i).copied().unwrap_or(0))).clicked() {
                                jump = Some(*off);
                            }
                            // ↵ and → exist in the monospace font only.
                            ui.monospace(before.replace('\n', "↵"));
                            ui.monospace("→");
                            ui.monospace(after.replace('\n', "↵"));
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
                ui.heading(i18n::tr("Error"));
                ui.label(e);
                if ui.button(i18n::tr("OK")).clicked() {
                    self.error = None;
                }
            });
        }

        if self.show_about {
            egui::Window::new(i18n::tr("About BigText")).open(&mut self.show_about).collapsible(false).resizable(false).show(ctx, |ui| {
                about_contents(ui);
            });
        }
    }

    /// Carries out the answer to a confirmation: 0 = cancel, 1 = primary, 2 = save.
    fn apply_confirm(&mut self, ctx: &egui::Context, dec: u8, c: Confirm) {
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
            (1, ConfirmAction::DeleteColumns { doc, dialect, cols, on_disk }) => self.delete_columns(doc, dialect, cols, on_disk, checked),
            (1, ConfirmAction::CsvRewrite { doc, dialect, rewrite, on_disk }) => self.rewrite_csv(doc, dialect, rewrite, on_disk, checked),
            _ => {
                self.pending_replace = None;
                // Exit was cancelled: "Restart now" for an update no longer applies.
                self.update.restart = false;
            }
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
                    title: i18n::tr("Unsaved changes").into(),
                    message: i18n::trf("{n} document(s) have unsaved changes. Exit and discard them?", &[("n", &n.to_string())]),
                    action: ConfirmAction::Exit,
                    checkbox: None,
                });
            } else {
                self.persist_state();
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

/// Turns the split view on (second pane starts at the same position) or off.
pub(super) fn toggle_split(d: &mut Document) {
    if d.split.take().is_none() {
        d.split = Some(document::PaneView { top: d.view.top, scroll_x: d.view.scroll_x, ..Default::default() });
    }
    d.active_pane = 0;
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


/// Author and project links shown in Help > About.
pub const AUTHOR: &str = "Erkin Pardayev";
pub const AUTHOR_LINKS: &[(&str, &str, &str)] = &[
    ("GitHub", "github.com/Erkinbek", "https://github.com/Erkinbek"),
    ("Repository", "github.com/Erkinbek/bigtext", "https://github.com/Erkinbek/bigtext"),
    ("Telegram", "@ErkinPardayev", "https://t.me/ErkinPardayev"),
    ("E-mail", "erkin.pardayev@gmail.com", "mailto:erkin.pardayev@gmail.com"),
];

fn about_contents(ui: &mut egui::Ui) {
    ui.vertical_centered(|ui| {
        ui.heading("BigText");
        ui.label(i18n::trf("Version {v}", &[("v", env!("CARGO_PKG_VERSION"))]));
        ui.label(i18n::tr("A fast, non-freezing large-file text editor."));
    });
    ui.separator();
    egui::Grid::new("author").num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
        ui.strong(i18n::tr("Author"));
        ui.label(AUTHOR);
        ui.end_row();
        for (label, text, url) in AUTHOR_LINKS {
            ui.strong(i18n::tr(label));
            ui.hyperlink_to(*text, *url);
            ui.end_row();
        }
    });
    ui.separator();
    ui.small(i18n::tr("Licensed under the MIT license. Built with Rust and egui."));
}


impl BigTextApp {
    pub(crate) fn test_has_confirm(&self) -> bool {
        self.confirm.is_some()
    }
    pub(crate) fn test_error(&self) -> Option<String> {
        self.error.clone()
    }
    pub(crate) fn test_force_exit(&mut self) {
        self.exit_confirmed = true;
        self.confirm = None;
    }
    /// Answers the open confirmation with its primary button.
    pub(crate) fn test_accept_confirm(&mut self) {
        if let (Some(c), Some(ctx)) = (self.confirm.take(), self.ctx_clone()) {
            self.apply_confirm(&ctx, 1, c);
        }
    }
    pub(crate) fn test_close(&mut self, id: u64) {
        self.close_tab(id, true);
    }
    pub(crate) fn test_dismiss(&mut self) {
        self.error = None;
        self.confirm = None;
    }
}

impl eframe::App for BigTextApp {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if let Some(t) = &mut self.self_test {
            t.inject(raw_input);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let t0 = Instant::now();
        let ctx = ui.ctx().clone();
        CTX.with(|c| *c.borrow_mut() = Some(ctx.clone()));
        self.track_jumps();
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
        if let Some(mut t) = self.self_test.take() {
            t.before_frame(self, &ctx);
            self.self_test = Some(t);
        }

        self.update_tick();
        let modal_open = self.goto.is_some() || self.switcher.is_some() || self.files.open || self.export.is_open() || self.csv.rename.is_some() || self.palette.is_some() || self.shortcuts_ui.capturing.is_some() || self.confirm.is_some() || self.error.is_some() || self.update.dialog_open();
        if !modal_open {
            self.shortcuts(&ctx);
            // The mouse's back and forward buttons.
            let (back, forward) = ctx.input(|i| (i.pointer.button_pressed(egui::PointerButton::Extra1), i.pointer.button_pressed(egui::PointerButton::Extra2)));
            if back {
                self.nav_back();
            }
            if forward {
                self.nav_forward();
            }
        }
        self.switcher_keys(&ctx);
        self.track_mru();

        for d in &mut self.docs {
            d.tick();
            d.refine_goto();
        }
        self.run_pending_find();
        self.run_pending_filters();
        self.sync_diff_marks();

        egui::Panel::top("menu").show(ui, |ui| self.menu_bar(ui));
        egui::Panel::top("tabs").show(ui, |ui| self.tab_bar(ui));
        if self.search.open {
            egui::Panel::top("search").show(ui, |ui| self.search_panel(ui));
        }
        if self.level_bar_visible() {
            egui::Panel::top("levels").show(ui, |ui| self.level_bar(ui));
        }
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        if self.search.show_results && self.docs.get(self.active).is_some_and(|d| d.results.is_some()) {
            egui::Panel::bottom("results").resizable(true).default_size(220.0).show(ui, |ui| self.results_panel(ui));
        }
        if self.search.show_filter && self.docs.get(self.active).is_some_and(|d| d.filter.is_some()) {
            egui::Panel::bottom("filter").resizable(true).default_size(260.0).show(ui, |ui| self.filter_panel(ui));
        }
        if self.files.results.is_some() {
            egui::Panel::bottom("files").resizable(true).default_size(240.0).show(ui, |ui| self.files_panel(ui));
        }
        if self.timeline.open && self.docs.get(self.active).is_some_and(|d| d.timeline.is_some()) {
            egui::Panel::bottom("timeline").resizable(true).default_size(170.0).show(ui, |ui| self.timeline_panel(ui));
        }
        if self.gaps.open && self.docs.get(self.active).is_some_and(|d| d.gaps.is_some()) {
            egui::Panel::bottom("gaps").resizable(true).default_size(160.0).show(ui, |ui| self.gaps_panel(ui));
        }
        if self.diff.is_some() {
            egui::Panel::bottom("diff").resizable(true).default_size(200.0).show(ui, |ui| self.diff_panel(ui));
        }
        if self.docs.get(self.active).is_some_and(|d| d.banner.is_some()) {
            egui::Panel::top("banner").show_separator_line(false).show(ui, |ui| self.banner(ui));
        }

        if self.json_open && self.docs.get(self.active).is_some_and(|d| d.is_ready() && !d.hex) {
            egui::Panel::left("json").resizable(true).default_size(320.0).show(ui, |ui| self.json_panel(ui));
        }
        if self.fields.open && self.docs.get(self.active).is_some_and(|d| d.is_ready() && !d.hex) {
            egui::Panel::left("fields").resizable(true).default_size(300.0).show(ui, |ui| self.fields_panel(ui));
        }

        let mut actions = Vec::new();
        let mut table_actions = Vec::new();
        let hl = self.view_highlights(ui.visuals().dark_mode);
        let font_size = &mut self.settings.font_size;
        let syntax = self.settings.syntax;
        let show_ws = self.settings.show_ws;
        egui::CentralPanel::no_frame().show(ui, |ui| {
            if let Some(d) = self.docs.get_mut(self.active) {
                if d.hex && d.is_ready() {
                    hex_view::show(ui, d, *font_size, !modal_open);
                } else if d.table && d.is_ready() {
                    let mut env = table_view::TableEnv { font_size, focus: !modal_open, actions: &mut table_actions };
                    table_view::show(ui, d, &mut env);
                } else {
                    let lang = if syntax { d.effective_lang() } else { crate::view::highlight::Lang::Plain };
                    match d.split.take() {
                        None => {
                            let mut env = text_view::ViewEnv { font_size, highlight: &hl, actions: &mut actions, focus: !modal_open, lang, pane: 0, show_ws };
                            text_view::show(ui, d, &mut env);
                        }
                        Some(mut pane) => {
                            // Two panes over the same document: independent scrolling, shared caret.
                            let full = ui.available_rect_before_wrap();
                            let mid = full.center().y;
                            let top = egui::Rect::from_min_max(full.min, egui::pos2(full.right(), mid - 2.0));
                            let bottom = egui::Rect::from_min_max(egui::pos2(full.left(), mid + 2.0), full.max);
                            ui.painter().hline(full.x_range(), mid, ui.visuals().widgets.noninteractive.bg_stroke);
                            ui.scope_builder(egui::UiBuilder::new().max_rect(top), |ui| {
                                let mut env = text_view::ViewEnv { font_size: &mut *font_size, highlight: &hl, actions: &mut actions, focus: !modal_open, lang, pane: 0, show_ws };
                                text_view::show(ui, d, &mut env);
                            });
                            pane.swap_with(&mut d.view);
                            ui.scope_builder(egui::UiBuilder::new().max_rect(bottom), |ui| {
                                let mut env = text_view::ViewEnv { font_size: &mut *font_size, highlight: &hl, actions: &mut actions, focus: !modal_open, lang, pane: 1, show_ws };
                                text_view::show(ui, d, &mut env);
                            });
                            pane.swap_with(&mut d.view);
                            d.split = Some(pane);
                        }
                    }
                }
            }
        });
        for a in table_actions {
            self.on_table_action(a);
        }
        for a in actions {
            match a {
                text_view::ViewAction::CopyLarge { cut, len } => {
                    if let Some(id) = self.docs.get(self.active).map(|d| d.id) {
                        if len > 100 * 1024 * 1024 {
                            self.confirm = Some(Confirm {
                                title: i18n::tr("Large copy").into(),
                                message: i18n::trf("Copy {size} to the clipboard? This needs that much memory.", &[("size", &human_bytes(len))]),
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
        self.update_dialog(&ctx);
        self.marks_window(&ctx);
        self.export_windows(&ctx);
        self.csv_windows(&ctx);
        self.merge_window(&ctx);
        self.find_in_files_window(&ctx);
        self.switcher_window(&ctx);
        self.portable_window(&ctx);
        self.id_patterns_window(&ctx);
        self.time_range_window(&ctx);
        self.patterns_window(&ctx);
        self.values_window(&ctx);
        self.palette_window(&ctx);
        self.shortcuts_window(&ctx);
        self.fonts_window(&ctx);
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
                || d.filter.as_ref().is_some_and(|r| r.is_running())
                || d.timeline.as_ref().is_some_and(|t| t.is_running())
                || d.gaps.as_ref().is_some_and(|g| g.is_running())
                || d.patterns.as_ref().is_some_and(|p| p.is_running())
        }) || !self.tasks.is_empty()
            || self.files.is_running();
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
        if let Some(t) = &mut self.self_test {
            t.after_frame(&ctx, ms);
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.apply_update_on_exit();
        // Unpacked copies are deleted once nothing maps them any more.
        let temps: Vec<PathBuf> = self.docs.iter().filter(|d| d.temporary).filter_map(|d| d.path.clone()).collect();
        if !temps.is_empty() {
            for d in &mut self.docs {
                d.cancel_jobs();
                d.watch_cancel.cancel();
            }
            self.docs.clear();
            for t in temps {
                for _ in 0..20 {
                    if std::fs::remove_file(&t).is_ok() || !t.exists() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                if let Some(dir) = t.parent() {
                    let _ = std::fs::remove_dir(dir);
                }
            }
        }
    }
}
