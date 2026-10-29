//! File → Compare JSON with…: the active JSON document compared with another JSON file by
//! structure — the values that changed, were added or were removed, by path — listed under the
//! text; click one to select it.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bigtext::jobs::{self, CancelToken, Progress};
use bigtext::json_diff::{self, Change};

use super::filter_panel::fmt_count;
use super::i18n::{tr, trf};
use super::BigTextApp;

#[derive(Default)]
pub struct JsonDiffData {
    pub changes: Vec<Change>,
    pub capped: bool,
    pub done: bool,
    pub error: Option<String>,
}

pub struct JsonDiffResults {
    /// The other file's name.
    pub other: String,
    pub version: u64,
    pub data: Arc<Mutex<JsonDiffData>>,
    pub cancel: CancelToken,
    pub progress: Progress,
}

impl JsonDiffResults {
    pub fn is_running(&self) -> bool {
        !self.data.lock().unwrap().done
    }
}

#[derive(Default)]
pub struct JsonDiffUi {
    pub open: bool,
}

/// A JSON file as a text to read from a worker.
fn open_text(path: &std::path::Path) -> Result<bigtext::text::TextBuf, String> {
    let src = bigtext::file::Source::open(path, bigtext::file::OpenMode::Auto).map_err(|e| e.to_string())?;
    let n = src.len().min(1 << 20);
    let det = bigtext::encoding::detect(&src.read(0..n).map_err(|e| e.to_string())?, n == src.len());
    let len = src.len();
    let mut buf = bigtext::text::TextBuf::new(Arc::new(src), bigtext::index::LineIndex::new(len), det.codec);
    buf.blocking = true;
    Ok(buf)
}

impl BigTextApp {
    pub(crate) fn compare_json_dialog(&mut self) {
        let mut dlg = rfd::FileDialog::new().set_title(tr("Compare JSON with")).add_filter("JSON", &["json"]);
        if let Some(dir) = self.docs.get(self.active).and_then(|d| d.path.as_ref()).and_then(|p| p.parent()) {
            dlg = dlg.set_directory(bigtext::file::display_path(dir));
        }
        if let Some(p) = dlg.pick_file() {
            self.compare_json_with(p);
        }
    }

    /// Compares the active document with the JSON file at `other` in the background.
    pub(crate) fn compare_json_with(&mut self, other: PathBuf) {
        let Some(d) = self.docs.get_mut(self.active).filter(|d| d.is_ready()) else { return };
        if let Some(r) = d.json_diff.take() {
            r.cancel.cancel();
        }
        let data = Arc::new(Mutex::new(JsonDiffData::default()));
        let (cancel, progress) = (CancelToken::new(), Progress::new(d.buf.len()));
        let name = super::filter_panel::file_name(&other);
        d.json_diff = Some(JsonDiffResults { other: name, version: d.version, data: data.clone(), cancel: cancel.clone(), progress: progress.clone() });
        self.json_diff.open = true;
        let snap = d.buf.snapshot();
        let waker = self.waker.clone();
        jobs::spawn("json-diff", move || {
            let fail = |e: String| {
                let mut g = data.lock().unwrap();
                g.error = Some(e);
                g.done = true;
            };
            let r = (|| -> Result<(), String> {
                let b = open_text(&other)?;
                let Some(pa) = json_diff::flatten_to_temp(&snap, "a.flat", &cancel, Some(&progress))? else { return Ok(()) };
                let pb = match json_diff::flatten_to_temp(&b, "b.flat", &cancel, Some(&progress)) {
                    Ok(Some(p)) => p,
                    other => {
                        bigtext::decompress::remove_temp(&pa);
                        return other.map(|_| ());
                    }
                };
                let mut batch = Vec::new();
                let res = json_diff::changes(&pa, &pb, &cancel, Some(&progress), |ch| {
                    batch.push(ch);
                    if batch.len() >= 256 {
                        let mut g = data.lock().unwrap();
                        if g.changes.len() >= json_diff::CHANGES_MAX {
                            g.capped = true;
                            return false;
                        }
                        g.changes.append(&mut batch);
                        drop(g);
                        waker();
                    }
                    true
                });
                bigtext::decompress::remove_temp(&pa);
                bigtext::decompress::remove_temp(&pb);
                data.lock().unwrap().changes.append(&mut batch);
                res.map(|_| ())
            })();
            if let Err(e) = r {
                fail(e);
            }
            data.lock().unwrap().done = true;
            waker();
        });
    }

    pub(super) fn json_diff_panel(&mut self, ui: &mut egui::Ui) {
        let font = egui::FontId::monospace(self.settings.font_size.min(14.0));
        let (mut close, mut goto) = (false, None);
        {
            let Some(d) = self.docs.get(self.active) else { return };
            let Some(r) = &d.json_diff else { return };
            let g = r.data.lock().unwrap();
            let n = g.changes.len();
            ui.horizontal(|ui| {
                if g.done && n == 0 && g.error.is_none() {
                    ui.colored_label(egui::Color32::from_rgb(90, 180, 90), trf("Same JSON as {other}", &[("other", &r.other)]));
                } else {
                    ui.strong(trf("{n} JSON differences with {other}", &[("n", &fmt_count(n as u64)), ("other", &r.other)]));
                }
                if g.capped {
                    ui.label(trf("(list capped at {n})", &[("n", &fmt_count(json_diff::CHANGES_MAX as u64))]));
                }
                if r.version != d.version {
                    ui.colored_label(egui::Color32::from_rgb(220, 150, 60), tr("stale (document changed)"));
                }
                if !g.done {
                    let f = r.progress.fraction();
                    ui.add(egui::ProgressBar::new(f).desired_width(140.0).text(format!("{:.0}%", f * 100.0)));
                    ui.ctx().request_repaint_after(Duration::from_millis(100));
                    if ui.small_button(tr("Cancel")).clicked() {
                        r.cancel.cancel();
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("×").clicked() {
                        close = true;
                    }
                });
            });
            if let Some(e) = &g.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            let row_h = ui.ctx().fonts_mut(|f| f.row_height(&font)) + 4.0;
            let cut = |s: &str| if s.chars().count() > 80 { s.chars().take(79).chain(['…']).collect() } else { s.to_string() };
            egui::ScrollArea::both().auto_shrink([false, false]).show_rows(ui, row_h, n, |ui, range| {
                for ch in &g.changes[range.start.min(n)..range.end.min(n)] {
                    let (text, color) = match (&ch.old, &ch.new) {
                        (Some(o), Some(nw)) => (format!("{}: {} → {}", ch.path, cut(o), cut(nw)), ui.visuals().text_color()),
                        (None, Some(nw)) => (format!("{}: {} {}", ch.path, tr("added"), cut(nw)), egui::Color32::from_rgb(90, 180, 90)),
                        (Some(o), None) => (format!("{}: {} {}", ch.path, tr("removed"), cut(o)), egui::Color32::from_rgb(220, 90, 90)),
                        (None, None) => continue,
                    };
                    if ui.selectable_label(false, egui::RichText::new(text).font(font.clone()).color(color)).clicked() && ch.old.is_some() {
                        goto = Some(ch.path.clone());
                    }
                }
            });
        }
        if close {
            self.json_diff.open = false;
        }
        if let Some(p) = goto {
            self.ensure_json_tree();
            self.json_goto(&p);
        }
    }
}
