//! Search → Lines only in this file…: the lines of one open document that occur nowhere in
//! another, whatever their order, listed below the text; white space, case, timestamps and a
//! mask can be ignored as in File → Compare.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bigtext::jobs::{self, human_bytes, CancelToken, Progress};
use bigtext::setdiff::{self, Counts};

use super::extras::DiffSwitches;
use super::filter_panel::{file_name, fmt_count, line_job, COPY_MAX};
use super::i18n::{tr, trf};
use super::tasks;
use super::{BigTextApp, Msg};

/// Lines listed at most (Save lines writes them all).
pub const ONLY_IN_CAP: usize = 1_000_000;

/// The dialog: the document whose lines are listed (A), the one they are looked for in (B),
/// and what is ignored.
pub struct OnlyInDialog {
    pub a: u64,
    pub b: u64,
    pub switches: DiffSwitches,
}

#[derive(Default)]
pub struct OnlyInData {
    /// Start offsets of the lines found so far.
    pub lines: Vec<u64>,
    pub capped: bool,
    pub done: bool,
    pub counts: Option<Counts>,
    pub error: Option<String>,
    pub elapsed: Option<Duration>,
}

pub struct OnlyInResults {
    /// The document whose lines are listed, and its version then.
    pub doc: u64,
    pub version: u64,
    pub other: u64,
    pub other_title: String,
    pub switches: DiffSwitches,
    pub data: Arc<Mutex<OnlyInData>>,
    pub cancel: CancelToken,
    pub progress: Progress,
}

impl BigTextApp {
    /// Indices of the tabs with text to compare.
    fn text_tabs(&self) -> Vec<usize> {
        (0..self.docs.len()).filter(|&i| self.docs[i].is_ready() && !self.docs[i].hex).collect()
    }

    pub(crate) fn open_only_in(&mut self) {
        let tabs = self.text_tabs();
        if tabs.len() < 2 {
            self.notify(tr("Open the other file in a tab too, then look for the lines only in one of them.").into(), None);
            return;
        }
        let a = if tabs.contains(&self.active) { self.active } else { tabs[0] };
        let b = tabs.iter().copied().find(|&i| i != a).unwrap_or(tabs[1]);
        let switches = self.only_in_dialog.take().map(|d| d.switches).unwrap_or_else(|| self.diff_switches.clone());
        self.only_in_dialog = Some(OnlyInDialog { a: self.docs[a].id, b: self.docs[b].id, switches });
    }

    pub(super) fn only_in_window(&mut self, ctx: &egui::Context) {
        if self.only_in_dialog.is_none() {
            return;
        }
        let tabs: Vec<(u64, String)> = self.text_tabs().into_iter().map(|i| (self.docs[i].id, self.docs[i].title.clone())).collect();
        let Some(dlg) = self.only_in_dialog.as_mut() else { return };
        let mut open = true;
        let mut go = false;
        egui::Window::new(tr("Lines only in one file")).open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
            ui.label(tr("Lists the lines of one file that occur nowhere in the other, whatever their order."));
            let pick = |ui: &mut egui::Ui, salt: &str, id: &mut u64| {
                let shown = tabs.iter().find(|t| t.0 == *id).map(|t| t.1.clone()).unwrap_or_default();
                egui::ComboBox::from_id_salt(salt).selected_text(shown).width(240.0).show_ui(ui, |ui| {
                    for (tid, title) in &tabs {
                        ui.selectable_value(id, *tid, title);
                    }
                });
            };
            egui::Grid::new("only-in").num_columns(2).show(ui, |ui| {
                ui.label(tr("Lines of"));
                pick(ui, "only-in-a", &mut dlg.a);
                ui.end_row();
                ui.label(tr("not found in"));
                ui.horizontal(|ui| {
                    pick(ui, "only-in-b", &mut dlg.b);
                    if ui.small_button("⇅").on_hover_text(tr("The other way round")).clicked() {
                        std::mem::swap(&mut dlg.a, &mut dlg.b);
                    }
                });
                ui.end_row();
            });
            ui.horizontal(|ui| {
                let s = &mut dlg.switches;
                ui.label(tr("Ignore:"));
                ui.checkbox(&mut s.whitespace, tr("white space"));
                ui.checkbox(&mut s.case, tr("case"));
                ui.checkbox(&mut s.timestamps, tr("timestamps")).on_hover_text(tr("The time at the start of each line"));
                ui.label(tr("mask"));
                ui.add(egui::TextEdit::singleline(&mut s.mask).desired_width(140.0).hint_text("req=[0-9a-f]+").font(egui::TextStyle::Monospace))
                    .on_hover_text(tr("A regular expression; its matches do not count (ids, durations)"));
            });
            ui.weak(tr("Log lines rarely repeat exactly: ignore the timestamps, and mask ids and durations."));
            go = ui.add_enabled(dlg.a != dlg.b, egui::Button::new(tr("Find"))).clicked();
        });
        if go {
            if let Some(d) = self.only_in_dialog.take() {
                self.start_only_in(d.a, d.b, d.switches.clone());
                self.only_in_dialog = None;
                // Remember the switches for next time.
                self.diff_switches = d.switches;
            }
        } else if !open {
            self.only_in_dialog = None;
        }
    }

    /// Lists the lines of document `a` that are not in document `b`.
    pub(crate) fn start_only_in(&mut self, a: u64, b: u64, switches: DiffSwitches) {
        let opts = match switches.options() {
            Ok(o) => o,
            Err(e) => {
                self.notify(e, None);
                return;
            }
        };
        let (Some(da), Some(db)) = (self.docs.iter().find(|d| d.id == a), self.docs.iter().find(|d| d.id == b)) else { return };
        if let Some(r) = self.only_in.take() {
            r.cancel.cancel();
        }
        let (sa, sb) = (da.buf.snapshot(), db.buf.snapshot());
        let data = Arc::new(Mutex::new(OnlyInData::default()));
        let (cancel, progress) = (CancelToken::new(), Progress::default());
        self.only_in = Some(OnlyInResults { doc: a, version: da.version, other: b, other_title: db.title.clone(), switches, data: data.clone(), cancel: cancel.clone(), progress: progress.clone() });
        let waker = self.waker.clone();
        jobs::spawn("only-in", move || {
            let t = Instant::now();
            let mut batch = Vec::new();
            let mut kept = 0usize;
            let mut last = Instant::now();
            let r = setdiff::only_in(&sa, &sb, &opts, setdiff::BUDGET, &cancel, Some(&progress), |off| {
                if kept < ONLY_IN_CAP {
                    kept += 1;
                    batch.push(off);
                    if last.elapsed() > Duration::from_millis(100) {
                        data.lock().unwrap().lines.append(&mut batch);
                        last = Instant::now();
                        waker();
                    }
                }
                true
            });
            let mut g = data.lock().unwrap();
            g.lines.append(&mut batch);
            g.capped = r.as_ref().ok().and_then(|c| c.as_ref()).is_some_and(|c| c.only > kept as u64);
            match r {
                Ok(c) => g.counts = c,
                Err(e) => g.error = Some(e.to_string()),
            }
            g.done = true;
            g.elapsed = Some(t.elapsed());
            drop(g);
            waker();
        });
    }

    /// Writes all the lines (not only the listed ones) to a file the user picks.
    fn save_only_in(&mut self) {
        let Some(r) = &self.only_in else { return };
        let (Some(da), Some(db)) = (self.docs.iter().find(|d| d.id == r.doc), self.docs.iter().find(|d| d.id == r.other)) else { return };
        let Ok(opts) = r.switches.options() else { return };
        let (sa, sb, title) = (da.buf.snapshot(), db.buf.snapshot(), da.title.clone());
        let Some(target) = self.pick_output_file(tr("Save lines"), &format!("{title}.only.txt")) else { return };
        self.spawn_task(tr("Saving lines").into(), move |progress, cancel| {
            tasks::write_via_temp(&target, |w| {
                let mut err = None;
                let r = setdiff::only_in(&sa, &sb, &opts, setdiff::BUDGET, cancel, Some(progress), |off| match setdiff::write_line_at(&sa, off, w) {
                    Ok(()) => true,
                    Err(e) => {
                        err = Some(e);
                        false
                    }
                })?;
                if let Some(e) = err {
                    return Err(e);
                }
                Ok(r.map(|c| trf("Saved {n} lines to {file}.", &[("n", &fmt_count(c.only)), ("file", &file_name(&target))])))
            })
        });
    }

    /// Copies the listed lines (up to [`COPY_MAX`]).
    fn copy_only_in(&mut self) {
        let Some(r) = &self.only_in else { return };
        let Some(d) = self.docs.iter().find(|d| d.id == r.doc) else { return };
        let (snap, id, lines) = (d.buf.snapshot(), d.id, r.data.lock().unwrap().lines.clone());
        let (tx, waker) = (self.tx.clone(), self.waker.clone());
        jobs::spawn("copy-lines", move || {
            let mut out = Vec::new();
            let mut result = Ok(());
            for off in lines {
                if let Err(e) = setdiff::write_line_at(&snap, off, &mut out) {
                    result = Err(e.to_string());
                    break;
                }
                if out.len() as u64 > COPY_MAX {
                    result = Err(trf("The lines are larger than {size}. Use “Save lines…” instead.", &[("size", &human_bytes(COPY_MAX))]));
                    break;
                }
            }
            let result = result.map(|()| snap.codec.decode_lossy(&out));
            let _ = tx.send(Msg::Copied { doc: id, result, cut: false });
            waker();
        });
    }

    pub(super) fn only_in_panel(&mut self, ui: &mut egui::Ui) {
        let Some(r) = &self.only_in else { return };
        let Some(di) = self.docs.iter().position(|d| d.id == r.doc) else {
            self.only_in = None;
            return;
        };
        let font = egui::FontId::monospace(self.settings.font_size.min(14.0));
        let row_h = ui.ctx().fonts_mut(|f| f.row_height(&font)) + 4.0;
        let (n, capped, done, counts, err, elapsed) = {
            let g = r.data.lock().unwrap();
            (g.lines.len(), g.capped, g.done, g.counts, g.error.clone(), g.elapsed)
        };
        let d = &self.docs[di];
        let stale = d.version != r.version || !self.docs.iter().any(|x| x.id == r.other);
        let (mut close, mut save, mut copy, mut rerun) = (false, false, false, false);
        let mut jump = None;
        ui.horizontal(|ui| {
            match counts {
                Some(c) => ui.strong(trf("{n} lines of {a} are not in {b}", &[("n", &fmt_count(c.only)), ("a", &d.title), ("b", &r.other_title)])),
                None => ui.strong(trf("Lines of {a} not in {b}", &[("a", &d.title), ("b", &r.other_title)])),
            };
            if let Some(c) = counts {
                ui.label(trf("({n} in both)", &[("n", &fmt_count(c.common))]));
            }
            if let Some(what) = r.switches.summary() {
                ui.weak(trf("ignoring {what}", &[("what", &what)]));
            }
            if capped {
                ui.label(trf("(list capped at {n}; Save lines writes all)", &[("n", &fmt_count(ONLY_IN_CAP as u64))]));
            }
            if let Some(e) = &err {
                ui.colored_label(ui.visuals().error_fg_color, e);
            } else if !done {
                let f = r.progress.fraction();
                ui.add(egui::ProgressBar::new(f).desired_width(160.0).text(format!("{:.0}%", f * 100.0)));
                ui.ctx().request_repaint_after(Duration::from_millis(50));
                if ui.small_button(tr("Cancel")).clicked() {
                    r.cancel.cancel();
                }
            } else if let Some(t) = elapsed {
                ui.label(format!("{} {:.2}s", tr("in"), t.as_secs_f64()));
            }
            if stale && done {
                ui.colored_label(ui.visuals().warn_fg_color, tr("(outdated, run again)"));
                rerun = ui.small_button(tr("Run again")).clicked();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                close = ui.small_button("×").clicked();
                save = ui.add_enabled(done && !stale && err.is_none(), egui::Button::new(tr("Save lines…")).small()).clicked();
                copy = ui.add_enabled(done && !stale && n > 0, egui::Button::new(tr("Copy lines")).small()).clicked();
            });
        });
        let buf = &d.buf;
        let data = r.data.clone();
        egui::ScrollArea::both().auto_shrink([false, false]).show_rows(ui, row_h, n, |ui, range| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
            let lines: Vec<u64> = {
                let g = data.lock().unwrap();
                g.lines[range.start.min(g.lines.len())..range.end.min(g.lines.len())].to_vec()
            };
            for off in lines {
                let job = line_job(buf, off, None, &font, ui.visuals(), stale, false);
                if ui.selectable_label(false, job).clicked() {
                    jump = Some(off);
                }
            }
        });
        if let (Some(off), false) = (jump, stale) {
            self.active = di;
            let d = &mut self.docs[di];
            d.set_caret(off.max(d.min_caret()), false);
            d.view.center();
        }
        if save {
            self.save_only_in();
        }
        if copy {
            self.copy_only_in();
        }
        if rerun {
            if let Some(r) = &self.only_in {
                let (a, b, s) = (r.doc, r.other, r.switches.clone());
                if self.docs.iter().any(|d| d.id == b) {
                    self.start_only_in(a, b, s);
                }
            }
        }
        if close {
            if let Some(r) = self.only_in.take() {
                r.cancel.cancel();
            }
        }
    }
}
