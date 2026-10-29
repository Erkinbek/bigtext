//! Search → Find in files…: the search pattern in every file of a folder whose name fits
//! (`*.log *.txt`), listed as file:line with the text around each match. Click one to open
//! the file there. Open tabs are searched with their unsaved edits.

use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, RichText, TextEdit};

use bigtext::encoding::Codec;
use bigtext::find_files::{self, Hit};
use bigtext::jobs::{self, CancelToken, Progress, Throttle};
use bigtext::search::{Matcher, Query};
use bigtext::text::TextBuf;

use super::document::Restore;
use super::filter_panel::fmt_count;
use super::i18n::{tr, trf};
use super::{same_path, BigTextApp};

/// Matches kept at most.
pub const HITS_CAP: usize = 100_000;

#[derive(Default)]
pub struct FilesData {
    /// Files with matches.
    pub files: Vec<PathBuf>,
    /// Matches: index into `files`, match.
    pub hits: Vec<(usize, Hit)>,
    pub total: usize,
    pub searched: usize,
    pub binary: usize,
    pub errors: Vec<String>,
    pub capped: bool,
    pub done: bool,
    pub elapsed: Option<Duration>,
}

pub struct FilesResults {
    pub query: Query,
    pub dir: PathBuf,
    pub data: Arc<Mutex<FilesData>>,
    pub cancel: CancelToken,
    pub progress: Progress,
}

pub struct FilesUi {
    /// The dialog.
    pub open: bool,
    pub dir: String,
    pub names: String,
    pub recursive: bool,
    pub results: Option<FilesResults>,
    /// The hit opened last (F4 goes on from it).
    pub current: Option<usize>,
    focus: bool,
}

impl Default for FilesUi {
    fn default() -> Self {
        FilesUi { open: false, dir: String::new(), names: "*.log *.txt".into(), recursive: true, results: None, current: None, focus: false }
    }
}

impl FilesUi {
    pub fn is_running(&self) -> bool {
        self.results.as_ref().is_some_and(|r| !r.data.lock().unwrap().done)
    }
}

/// Matchers for each encoding met, built on first use.
struct Matchers {
    query: Query,
    built: Vec<(Codec, Arc<Matcher>)>,
}

impl Matchers {
    fn get(&mut self, codec: Codec) -> Option<Arc<Matcher>> {
        if let Some((_, m)) = self.built.iter().find(|(c, _)| *c == codec) {
            return Some(m.clone());
        }
        let m = Arc::new(Matcher::new(&self.query, codec).ok()?);
        self.built.push((codec, m.clone()));
        Some(m)
    }
}

/// Everything a search job needs.
struct Job {
    dir: PathBuf,
    names: Vec<String>,
    recursive: bool,
    matchers: Matchers,
    /// Open tabs (searched with their unsaved edits).
    open: Vec<(PathBuf, TextBuf)>,
    data: Arc<Mutex<FilesData>>,
    cancel: CancelToken,
    progress: Progress,
    waker: bigtext::jobs::Waker,
}

fn search_job(job: Job) {
    let Job { dir, names, recursive, mut matchers, open, data, cancel, progress, waker } = job;
    let started = Instant::now();
    let files = match find_files::list_files(&dir, &names, recursive, &cancel) {
        Ok(f) => f,
        Err(e) => {
            let mut g = data.lock().unwrap();
            g.errors.push(super::friendly_io(&e, &dir));
            g.done = true;
            drop(g);
            waker();
            return;
        }
    };
    data.lock().unwrap().total = files.len();
    progress.set_total(files.len() as u64);
    let mut throttle = Throttle::new(Duration::from_millis(100));
    let mut count = 0usize;
    'files: for (n, path) in files.iter().enumerate() {
        if cancel.is_cancelled() {
            break;
        }
        let buf = match open.iter().find(|(p, _)| same_path(p, path)) {
            Some((_, b)) => Ok(Some(b.clone())),
            None => find_files::open_text(path),
        };
        let buf = match buf {
            Ok(Some(b)) => b,
            Ok(None) => {
                let mut g = data.lock().unwrap();
                g.binary += 1;
                g.searched = n + 1;
                continue;
            }
            Err(e) => {
                let mut g = data.lock().unwrap();
                g.errors.push(super::friendly_io(&e, path));
                g.searched = n + 1;
                continue;
            }
        };
        let Some(m) = matchers.get(buf.codec) else {
            data.lock().unwrap().searched = n + 1;
            continue;
        };
        let mut batch = Vec::new();
        let mut file_index = None;
        find_files::search_text(&buf, &m, &cancel, |hit| {
            if count >= HITS_CAP {
                return ControlFlow::Break(());
            }
            count += 1;
            batch.push(hit);
            if throttle.ready() {
                let mut g = data.lock().unwrap();
                let i = *file_index.get_or_insert_with(|| {
                    g.files.push(path.clone());
                    g.files.len() - 1
                });
                g.hits.extend(batch.drain(..).map(|h| (i, h)));
                drop(g);
                waker();
            }
            ControlFlow::Continue(())
        });
        let mut g = data.lock().unwrap();
        if !batch.is_empty() {
            let i = *file_index.get_or_insert_with(|| {
                g.files.push(path.clone());
                g.files.len() - 1
            });
            g.hits.extend(batch.drain(..).map(|h| (i, h)));
        }
        g.searched = n + 1;
        drop(g);
        progress.set_done(n as u64 + 1);
        if count >= HITS_CAP {
            data.lock().unwrap().capped = true;
            break 'files;
        }
    }
    let mut g = data.lock().unwrap();
    g.done = true;
    g.elapsed = Some(started.elapsed());
    drop(g);
    waker();
}

impl BigTextApp {
    pub(crate) fn open_find_in_files(&mut self) {
        self.files.open = true;
        self.files.focus = true;
        if self.files.dir.is_empty() {
            if let Some(dir) = self.docs.get(self.active).and_then(|d| d.unpacked_from.as_deref().or(d.path.as_deref())).and_then(Path::parent) {
                self.files.dir = bigtext::file::display_path(dir);
            }
        }
    }

    /// Starts searching the folder of the dialog.
    pub(crate) fn start_find_in_files(&mut self) {
        let query = self.search.query.clone();
        if query.pattern.is_empty() {
            return;
        }
        if let Err(e) = Matcher::new(&query, Codec::utf8()) {
            self.error = Some(e);
            return;
        }
        let dir = PathBuf::from(self.files.dir.trim());
        if !dir.is_dir() {
            self.error = Some(trf("“{dir}” is not a folder.", &[("dir", &self.files.dir)]));
            return;
        }
        if let Some(r) = self.files.results.take() {
            r.cancel.cancel();
        }
        self.search.remember();
        let open: Vec<(PathBuf, TextBuf)> = self.docs.iter().filter(|d| d.is_ready() && !d.temporary).filter_map(|d| Some((d.path.clone()?, d.buf.snapshot()))).collect();
        let data = Arc::new(Mutex::new(FilesData::default()));
        let (cancel, progress) = (CancelToken::new(), Progress::default());
        self.files.results = Some(FilesResults { query: query.clone(), dir: dir.clone(), data: data.clone(), cancel: cancel.clone(), progress: progress.clone() });
        self.files.open = false;
        let names = find_files::patterns(&self.files.names);
        let recursive = self.files.recursive;
        let waker = self.waker.clone();
        let job = Job { dir, names, recursive, matchers: Matchers { query, built: Vec::new() }, open, data, cancel, progress, waker };
        jobs::spawn("find-in-files", move || search_job(job));
    }

    /// Opens the file of match `i` there.
    pub(crate) fn open_files_hit(&mut self, i: usize) {
        self.files.current = Some(i);
        let Some(r) = &self.files.results else { return };
        let (path, start, end) = {
            let g = r.data.lock().unwrap();
            let Some((f, h)) = g.hits.get(i) else { return };
            (g.files[*f].clone(), h.start, h.end)
        };
        match self.docs.iter().position(|d| d.path.as_deref().is_some_and(|p| same_path(p, &path))) {
            Some(k) if self.docs[k].is_ready() => {
                self.active = k;
                let d = &mut self.docs[k];
                let len = d.buf.len();
                d.select(start.min(len), end.min(len));
                d.view.center();
            }
            _ => {
                self.open_path(path);
                if let Some(d) = self.docs.get_mut(self.active) {
                    if d.is_ready() {
                        d.select(start, end);
                        d.view.center();
                    } else {
                        d.restore = Some(Restore { select: Some((start, end)), ..Default::default() });
                    }
                }
            }
        }
    }

    pub(super) fn find_in_files_window(&mut self, ctx: &egui::Context) {
        if !self.files.open {
            return;
        }
        let mut open = true;
        let mut go = false;
        let mut close = false;
        egui::Window::new(tr("Find in files")).open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
            egui::Grid::new("find-in-files").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                ui.label(tr("Find"));
                ui.horizontal(|ui| {
                    let r = ui.add(TextEdit::singleline(&mut self.search.query.pattern).desired_width(300.0).hint_text(tr("text or regex")).font(egui::TextStyle::Monospace));
                    if self.files.focus {
                        r.request_focus();
                        self.files.focus = false;
                    }
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        go = true;
                    }
                    let q = &mut self.search.query;
                    ui.toggle_value(&mut q.case_sensitive, RichText::new("Aa").monospace()).on_hover_text(tr("Case sensitive"));
                    ui.toggle_value(&mut q.whole_word, RichText::new("W").monospace()).on_hover_text(tr("Whole word"));
                    ui.toggle_value(&mut q.regex, RichText::new(".*").monospace()).on_hover_text(tr("Regular expression (linear time)"));
                });
                ui.end_row();
                ui.label(tr("Folder"));
                ui.horizontal(|ui| {
                    ui.add(TextEdit::singleline(&mut self.files.dir).desired_width(300.0));
                    if ui.button(tr("Browse…")).clicked() {
                        let mut dlg = rfd::FileDialog::new().set_title(tr("Folder"));
                        if !self.files.dir.is_empty() {
                            dlg = dlg.set_directory(&self.files.dir);
                        }
                        if let Some(d) = dlg.pick_folder() {
                            self.files.dir = bigtext::file::display_path(&d);
                        }
                    }
                });
                ui.end_row();
                ui.label(tr("File names"));
                ui.add(TextEdit::singleline(&mut self.files.names).desired_width(300.0).hint_text(tr("*.log *.txt (empty: all files)")));
                ui.end_row();
                ui.label("");
                ui.checkbox(&mut self.files.recursive, tr("Include sub-folders"));
                ui.end_row();
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let ok = !self.search.query.pattern.is_empty() && !self.files.dir.trim().is_empty();
                if ui.add_enabled(ok, egui::Button::new(tr("Find"))).clicked() {
                    go = true;
                }
                if ui.button(tr("Cancel")).clicked() {
                    close = true;
                }
            });
        });
        if go {
            self.start_find_in_files();
        }
        if !open || close || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.files.open = false;
        }
    }

    pub(super) fn files_panel(&mut self, ui: &mut egui::Ui) {
        let font = FontId::monospace(self.settings.font_size.min(14.0));
        let mut close = false;
        let mut jump = None;
        let mut again = false;
        {
            let Some(r) = &self.files.results else { return };
            let g = r.data.lock().unwrap();
            ui.horizontal(|ui| {
                ui.strong(trf("{n} matches of “{p}” in {files} files", &[("n", &fmt_count(g.hits.len() as u64)), ("p", &r.query.pattern), ("files", &fmt_count(g.files.len() as u64))]));
                ui.weak(trf("searched {done} of {total} files in {dir}", &[("done", &fmt_count(g.searched as u64)), ("total", &fmt_count(g.total as u64)), ("dir", &bigtext::file::display_path(&r.dir))]));
                if g.capped {
                    ui.label(trf("(stopped at {n})", &[("n", &fmt_count(HITS_CAP as u64))]));
                }
                if g.binary > 0 {
                    ui.weak(trf("{n} binary files skipped", &[("n", &g.binary.to_string())]));
                }
                if !g.done {
                    let f = r.progress.fraction();
                    ui.add(egui::ProgressBar::new(f).desired_width(140.0).text(format!("{:.0}%", f * 100.0)));
                    ui.ctx().request_repaint_after(Duration::from_millis(100));
                    if ui.small_button(tr("Cancel")).clicked() {
                        r.cancel.cancel();
                    }
                } else if let Some(t) = g.elapsed {
                    ui.label(format!("{} {:.2}s", tr("in"), t.as_secs_f64()));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("×").clicked() {
                        close = true;
                    }
                    if ui.small_button(tr("Search again")).clicked() {
                        again = true;
                    }
                });
            });
            for e in g.errors.iter().take(3) {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            let row_h = ui.ctx().fonts_mut(|f| f.row_height(&font)) + 2.0;
            let n = g.hits.len();
            drop(g);
            let data = r.data.clone();
            let dir = r.dir.clone();
            egui::ScrollArea::both().auto_shrink([false, false]).show_rows(ui, row_h, n, |ui, range| {
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                let g = data.lock().unwrap();
                for i in range.start..range.end.min(g.hits.len()) {
                    let (f, h) = &g.hits[i];
                    let name = g.files[*f].strip_prefix(&dir).map(Path::to_path_buf).unwrap_or_else(|_| g.files[*f].clone());
                    let job = hit_job(&bigtext::file::display_path(&name), h, &font, ui.visuals());
                    if ui.selectable_label(false, job).clicked() {
                        jump = Some(i);
                    }
                }
            });
        }
        if let Some(i) = jump {
            self.open_files_hit(i);
        }
        if close {
            if let Some(r) = self.files.results.take() {
                r.cancel.cancel();
            }
        }
        if again {
            self.files.open = true;
        }
    }
}

fn hit_job(name: &str, h: &Hit, font: &FontId, v: &egui::Visuals) -> LayoutJob {
    let mut job = LayoutJob::default();
    let file = TextFormat { font_id: font.clone(), color: Color32::from_rgb(90, 160, 220), ..Default::default() };
    let weak = TextFormat { font_id: font.clone(), color: v.weak_text_color(), ..Default::default() };
    let normal = TextFormat { font_id: font.clone(), color: v.text_color(), ..Default::default() };
    let hl = TextFormat { font_id: font.clone(), color: v.strong_text_color(), background: v.selection.bg_fill, ..Default::default() };
    job.append(name, 0.0, file);
    job.append(&format!(":{}  ", h.line), 0.0, weak);
    let (a, b) = h.mark;
    job.append(&h.text[..a], 0.0, normal.clone());
    job.append(&h.text[a..b], 0.0, hl);
    job.append(&h.text[b..], 0.0, normal);
    job
}
