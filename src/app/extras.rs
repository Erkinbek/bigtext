//! Recent files and session restore, file comparison (streaming diff) and its results panel.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bigtext::diff::{self, DiffEnd, Hunk};
use bigtext::file::{self, OpenMode, Source};
use bigtext::jobs::{self, CancelToken, Progress};

use super::document::Restore;
use super::BigTextApp;

pub const RECENT_MAX: usize = 15;
pub const DIFF_CAP: usize = 100_000;

// ---------------------------------------------------------------------------------------------
// Persistence: a small key=value text file in the user's config directory.

#[derive(Clone, Debug, Default)]
pub struct TabState {
    pub path: PathBuf,
    pub caret: u64,
    pub top: u64,
    pub wrap: bool,
    pub bookmarks: Vec<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct Persisted {
    pub recent: Vec<PathBuf>,
    pub tabs: Vec<TabState>,
    pub active: usize,
    pub font_size: Option<f32>,
    pub dark: Option<bool>,
    pub backup_on_save: Option<bool>,
    pub syntax: Option<bool>,
    pub update_check: Option<bool>,
    pub skip_version: Option<String>,
    /// Unix time of the last update check.
    pub update_checked: Option<u64>,
    pub marks: Vec<super::marks::Mark>,
    /// Search history, newest first.
    pub history: Vec<String>,
}

fn query_flags(q: &bigtext::search::Query) -> String {
    [(q.regex, 'r'), (q.case_sensitive, 'c'), (q.whole_word, 'w'), (q.dot_all, 'd')].iter().filter(|x| x.0).map(|x| x.1).collect()
}

fn parse_mark(v: &str) -> Option<super::marks::Mark> {
    let mut it = v.splitn(4, '|');
    let enabled = it.next()? == "1";
    let color = it.next()?.parse().ok()?;
    let flags = it.next()?;
    let pattern = it.next()?.to_string();
    let query = bigtext::search::Query {
        pattern,
        regex: flags.contains('r'),
        case_sensitive: flags.contains('c'),
        whole_word: flags.contains('w'),
        dot_all: flags.contains('d'),
    };
    Some(super::marks::Mark { query, color, enabled })
}

pub fn state_file() -> Option<PathBuf> {
    if std::env::var_os("BIGTEXT_NO_STATE").is_some() {
        return None;
    }
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library").join("Application Support"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    }?;
    Some(base.join("BigText").join("state.txt"))
}

pub fn load() -> Persisted {
    let Some(path) = state_file() else { return Persisted::default() };
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text),
        Err(_) => Persisted::default(),
    }
}

fn parse(text: &str) -> Persisted {
    let mut p = Persisted::default();
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        match k {
            "recent" => p.recent.push(PathBuf::from(v)),
            "active" => p.active = v.parse().unwrap_or(0),
            "font_size" => p.font_size = v.parse().ok(),
            "dark" => p.dark = Some(v == "1"),
            "backup_on_save" => p.backup_on_save = Some(v == "1"),
            "syntax" => p.syntax = Some(v == "1"),
            "update_check" => p.update_check = Some(v == "1"),
            "skip_version" => p.skip_version = Some(v.to_string()),
            "update_checked" => p.update_checked = v.parse().ok(),
            "mark" => p.marks.extend(parse_mark(v)),
            "find" => p.history.push(v.to_string()),
            "tab" => {
                let parts: Vec<&str> = v.splitn(5, '|').collect();
                if parts.len() == 5 {
                    p.tabs.push(TabState {
                        caret: parts[0].parse().unwrap_or(0),
                        top: parts[1].parse().unwrap_or(0),
                        wrap: parts[2] == "1",
                        bookmarks: parts[3].split(',').filter_map(|x| x.parse().ok()).collect(),
                        path: PathBuf::from(parts[4]),
                    });
                }
            }
            _ => {}
        }
    }
    p
}

fn serialize(p: &Persisted) -> String {
    let mut s = String::from("# BigText session state\n");
    let b = |x: bool| if x { "1" } else { "0" };
    if let Some(f) = p.font_size {
        s.push_str(&format!("font_size={f}\n"));
    }
    if let Some(d) = p.dark {
        s.push_str(&format!("dark={}\n", b(d)));
    }
    if let Some(d) = p.backup_on_save {
        s.push_str(&format!("backup_on_save={}\n", b(d)));
    }
    if let Some(d) = p.syntax {
        s.push_str(&format!("syntax={}\n", b(d)));
    }
    if let Some(d) = p.update_check {
        s.push_str(&format!("update_check={}\n", b(d)));
    }
    if let Some(v) = &p.skip_version {
        s.push_str(&format!("skip_version={v}\n"));
    }
    if let Some(t) = p.update_checked {
        s.push_str(&format!("update_checked={t}\n"));
    }
    for r in &p.recent {
        s.push_str(&format!("recent={}\n", r.display()));
    }
    for m in &p.marks {
        s.push_str(&format!("mark={}|{}|{}|{}\n", b(m.enabled), m.color, query_flags(&m.query), m.query.pattern));
    }
    for h in p.history.iter().filter(|h| !h.contains(['\n', '\r'])) {
        s.push_str(&format!("find={h}\n"));
    }
    for t in &p.tabs {
        let bm: Vec<String> = t.bookmarks.iter().map(|x| x.to_string()).collect();
        s.push_str(&format!("tab={}|{}|{}|{}|{}\n", t.caret, t.top, b(t.wrap), bm.join(","), t.path.display()));
    }
    s.push_str(&format!("active={}\n", p.active));
    s
}

/// Writes the state on a worker thread (never blocks the UI).
pub fn save_async(p: Persisted) {
    let Some(path) = state_file() else { return };
    let text = serialize(&p);
    jobs::spawn("save-state", move || write_state(&path, &text));
}

/// Writes the state before returning: on exit, when a new BigText process reads it right away.
pub fn save_now(p: Persisted) {
    if let Some(path) = state_file() {
        write_state(&path, &serialize(&p));
    }
}

fn write_state(path: &Path, text: &str) {
    // One writer at a time, since all of them use the same temporary file.
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

// ---------------------------------------------------------------------------------------------
// File comparison.

#[derive(Default)]
pub struct DiffData {
    pub hunks: Vec<Hunk>,
    pub done: bool,
    pub capped: bool,
    pub end: Option<DiffEnd>,
    pub error: Option<String>,
    pub other: Option<Arc<Source>>,
    pub elapsed: Option<Duration>,
}

pub struct DiffResults {
    pub doc: u64,
    pub other_path: PathBuf,
    pub data: Arc<Mutex<DiffData>>,
    pub cancel: CancelToken,
    pub progress: Progress,
}

impl BigTextApp {
    pub(super) fn add_recent(&mut self, path: &Path) {
        let p = PathBuf::from(file::display_path(path));
        self.recent.retain(|r| !super::same_path(r, &p));
        self.recent.insert(0, p);
        self.recent.truncate(RECENT_MAX);
        self.persist_state();
    }

    pub(super) fn persisted(&self) -> Persisted {
        Persisted {
            recent: self.recent.clone(),
            tabs: self
                .docs
                .iter()
                .filter_map(|d| {
                    Some(TabState {
                        path: PathBuf::from(file::display_path(d.path.as_deref()?)),
                        caret: d.view.caret,
                        top: d.view.top,
                        wrap: d.view.wrap,
                        bookmarks: d.bookmarks.clone(),
                    })
                })
                .collect(),
            active: self.active,
            font_size: Some(self.settings.font_size),
            dark: Some(self.settings.dark),
            backup_on_save: Some(self.settings.backup_on_save),
            syntax: Some(self.settings.syntax),
            update_check: Some(self.update.auto),
            skip_version: self.update.skipped.map(|v| v.to_string()),
            update_checked: Some(self.update.last_check).filter(|&t| t > 0),
            marks: self.settings.marks.clone(),
            history: self.search.history.clone(),
        }
    }

    pub(super) fn persist_state(&self) {
        if self.persist {
            save_async(self.persisted());
        }
    }

    /// Restores the previous session (tabs, positions, bookmarks, settings).
    pub(super) fn restore_session(&mut self, p: Persisted) {
        if let Some(f) = p.font_size {
            self.settings.font_size = f.clamp(6.0, 72.0);
        }
        if let Some(d) = p.dark {
            self.settings.dark = d;
        }
        if let Some(b) = p.backup_on_save {
            self.settings.backup_on_save = b;
        }
        if let Some(s) = p.syntax {
            self.settings.syntax = s;
        }
        self.recent = p.recent;
        for t in p.tabs {
            self.open_path(t.path.clone());
            if let Some(d) = self.docs.last_mut() {
                d.restore = Some(Restore { caret: t.caret, top: t.top, wrap: t.wrap, bookmarks: t.bookmarks, line: None });
            }
        }
        if !self.docs.is_empty() {
            self.active = p.active.min(self.docs.len() - 1);
        }
    }

    pub(super) fn compare_dialog(&mut self) {
        let Some(d) = self.docs.get(self.active) else { return };
        if d.path.is_none() || !d.is_ready() {
            self.error = Some("Open a saved file first; the comparison reads both files from disk.".into());
            return;
        }
        if let Some(other) = rfd::FileDialog::new().set_title("Compare with…").pick_file() {
            self.start_diff(other);
        }
    }

    pub(crate) fn start_diff(&mut self, other: PathBuf) {
        let Some(d) = self.docs.get(self.active) else { return };
        if let Some(r) = self.diff.take() {
            r.cancel.cancel();
        }
        let a = d.buf.source.clone();
        let data = Arc::new(Mutex::new(DiffData::default()));
        let cancel = CancelToken::new();
        let progress = Progress::default();
        self.diff = Some(DiffResults { doc: d.id, other_path: other.clone(), data: data.clone(), cancel: cancel.clone(), progress: progress.clone() });
        if d.is_modified() {
            self.search.status = "Comparing the saved file; unsaved edits are not included".into();
        }
        let waker = self.waker.clone();
        jobs::spawn("diff", move || {
            let t = Instant::now();
            let b = match Source::open(&other, OpenMode::Auto) {
                Ok(b) => Arc::new(b),
                Err(e) => {
                    let mut g = data.lock().unwrap();
                    g.error = Some(super::friendly_io(&e, &other));
                    g.done = true;
                    waker();
                    return;
                }
            };
            data.lock().unwrap().other = Some(b.clone());
            let mut batch = Vec::new();
            let mut count = 0usize;
            let mut capped = false;
            let mut last_wake = Instant::now();
            let end = diff::diff(&a, &b, &cancel, Some(&progress), |h| {
                if count >= DIFF_CAP {
                    capped = true;
                    return false;
                }
                count += 1;
                batch.push(h);
                if last_wake.elapsed() > Duration::from_millis(50) {
                    data.lock().unwrap().hunks.append(&mut batch);
                    last_wake = Instant::now();
                    waker();
                }
                true
            });
            let mut g = data.lock().unwrap();
            g.hunks.append(&mut batch);
            g.capped = capped;
            g.end = Some(end);
            g.done = true;
            g.elapsed = Some(t.elapsed());
            if end == DiffEnd::Failed {
                g.error = Some("Reading one of the files failed.".into());
            }
            drop(g);
            waker();
        });
    }

    /// Copies the differences' offsets into the compared document for its scrollbar marks.
    pub(super) fn sync_diff_marks(&mut self) {
        let (id, data) = match &self.diff {
            Some(r) => (Some(r.doc), Some(r.data.clone())),
            None => (None, None),
        };
        for d in &mut self.docs {
            if Some(d.id) != id {
                d.diff_marks.clear();
            } else if let Some(data) = &data {
                let g = data.lock().unwrap();
                if g.hunks.len() != d.diff_marks.len() {
                    d.diff_marks = g.hunks.iter().map(|h| h.a_off).collect();
                }
            }
        }
    }

    pub(super) fn diff_panel(&mut self, ui: &mut egui::Ui) {
        let Some(r) = &self.diff else { return };
        let font = egui::FontId::monospace(self.settings.font_size.min(13.0));
        let row_h = ui.ctx().fonts_mut(|f| f.row_height(&font)) + 2.0;
        let (n, done, capped, err, elapsed, other) = {
            let g = r.data.lock().unwrap();
            (g.hunks.len(), g.done, g.capped, g.error.clone(), g.elapsed, g.other.clone())
        };
        let mut close = false;
        let mut open_other: Option<u64> = None;
        let mut jump: Option<u64> = None;
        ui.horizontal(|ui| {
            ui.strong(format!("Compare with {}", file::display_path(&r.other_path)));
            if let Some(e) = &err {
                ui.colored_label(ui.visuals().error_fg_color, e);
            } else if !done {
                ui.add(egui::ProgressBar::new(r.progress.fraction()).desired_width(160.0).text(format!("comparing {:.0}%", r.progress.fraction() * 100.0)));
                ui.ctx().request_repaint_after(Duration::from_millis(50));
                if ui.small_button("Cancel").clicked() {
                    r.cancel.cancel();
                }
            } else if n == 0 {
                ui.label(format!("files are identical ({:.2}s)", elapsed.unwrap_or_default().as_secs_f64()));
            } else {
                ui.label(format!("{n} difference(s){} in {:.2}s", if capped { " (capped, more exist)" } else { "" }, elapsed.unwrap_or_default().as_secs_f64()));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("×").clicked() {
                    close = true;
                }
            });
        });
        let a_src = self.docs.iter().find(|d| d.id == r.doc).map(|d| (d.buf.source.clone(), d.buf.codec));
        let data = r.data.clone();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, row_h, n, |ui, range| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            let g = data.lock().unwrap();
            for i in range {
                let Some(h) = g.hunks.get(i).copied() else { break };
                let snippet = |src: &Source, off: u64, codec: bigtext::encoding::Codec| -> String {
                    match src.get(off..(off + 160).min(src.len()), false) {
                        Some(b) => {
                            let s = codec.decode_lossy(&b);
                            s.lines().next().unwrap_or("").chars().map(|c| if c.is_control() { '·' } else { c }).collect()
                        }
                        None => "…".into(),
                    }
                };
                let (a_text, codec) = match &a_src {
                    Some((s, c)) if h.a_count > 0 => (snippet(s, h.a_off, *c), *c),
                    Some((_, c)) => (String::new(), *c),
                    None => (String::new(), bigtext::encoding::Codec::utf8()),
                };
                let b_text = match &other {
                    Some(s) if h.b_count > 0 => snippet(s, h.b_off, codec),
                    _ => String::new(),
                };
                ui.horizontal(|ui| {
                    let label = format!("A {:>8} −{:<5} B {:>8} +{:<5}", h.a_line + 1, h.a_count, h.b_line + 1, h.b_count);
                    if ui.add(egui::Button::new(egui::RichText::new(label).font(font.clone())).frame(false)).on_hover_text("Go to this line in A").clicked() {
                        jump = Some(h.a_line + 1);
                    }
                    if ui.small_button("B").on_hover_text("Open the other file at this line").clicked() {
                        open_other = Some(h.b_line + 1);
                    }
                    // Both sides get half of the remaining width so B stays visible next to a long A line.
                    let w = ((ui.available_width() - 2.0 * ui.spacing().item_spacing.x - 16.0) / 2.0).max(40.0);
                    let side = |ui: &mut egui::Ui, text: String, color: egui::Color32| {
                        ui.allocate_ui_with_layout(egui::vec2(w, row_h), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.set_width(w);
                            ui.add(egui::Label::new(egui::RichText::new(text).font(font.clone()).color(color)).truncate());
                        });
                    };
                    side(ui, a_text, egui::Color32::from_rgb(220, 110, 110));
                    // The arrow is in the monospace font only.
                    ui.monospace("→");
                    side(ui, b_text, egui::Color32::from_rgb(110, 190, 110));
                });
            }
        });
        let (doc_id, other_path) = (r.doc, r.other_path.clone());
        if let Some(line) = jump {
            if let Some(i) = self.docs.iter().position(|d| d.id == doc_id) {
                self.active = i;
                self.docs[i].goto_line(line);
            }
        }
        if let Some(line) = open_other {
            let existing = self.docs.iter().position(|d| d.path.as_deref().is_some_and(|p| super::same_path(p, &other_path)));
            match existing {
                Some(i) => {
                    self.active = i;
                    self.docs[i].goto_line(line);
                }
                None => {
                    self.open_path(other_path);
                    if let Some(d) = self.docs.last_mut() {
                        d.restore = Some(Restore { line: Some(line), ..Default::default() });
                    }
                }
            }
        }
        if close {
            if let Some(r) = self.diff.take() {
                r.cancel.cancel();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_roundtrip() {
        let p = Persisted {
            recent: vec![PathBuf::from(r"C:\logs\a.log"), PathBuf::from("/tmp/b|c.txt")],
            tabs: vec![TabState { path: PathBuf::from(r"D:\big file.txt"), caret: 123, top: 100, wrap: true, bookmarks: vec![0, 55, 1 << 40] }],
            active: 0,
            font_size: Some(16.0),
            dark: Some(false),
            backup_on_save: Some(true),
            syntax: Some(false),
            update_check: Some(false),
            skip_version: Some("0.2.0".into()),
            update_checked: Some(1_761_550_000),
            marks: vec![super::super::marks::Mark {
                query: bigtext::search::Query { pattern: r"id=\d+|x".into(), regex: true, case_sensitive: true, whole_word: false, dot_all: false },
                color: 3,
                enabled: false,
            }],
            history: vec!["ERROR".into(), "a|b=c".into()],
        };
        let q = parse(&serialize(&p));
        assert_eq!((q.marks.clone(), q.history.clone()), (p.marks.clone(), p.history.clone()));
        assert_eq!(q.recent, p.recent);
        assert_eq!(q.tabs.len(), 1);
        let t = &q.tabs[0];
        assert_eq!((t.caret, t.top, t.wrap, t.bookmarks.clone(), t.path.clone()), (123, 100, true, vec![0, 55, 1 << 40], PathBuf::from(r"D:\big file.txt")));
        assert_eq!((q.font_size, q.dark, q.backup_on_save, q.syntax), (Some(16.0), Some(false), Some(true), Some(false)));
        assert_eq!((q.update_check, q.skip_version.as_deref(), q.update_checked), (Some(false), Some("0.2.0"), Some(1_761_550_000)));
    }
}
