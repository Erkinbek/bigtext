//! Checking a table's rows (column menu → Check rows…): a list under the table of every record
//! with another field count, a stray or unclosed quote, a control character, or text in a
//! number column; click one to go there, or save them all to a file.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use egui::Color32;

use bigtext::csv_check::{self, Checked, Kind, Problem};
use bigtext::jobs::{self, CancelToken, Progress, Throttle};

use super::filter_panel::{file_name, fmt_count};
use super::i18n::{tr, trf};
use super::tasks::{self, TaskOutcome};
use super::BigTextApp;

#[derive(Default)]
pub struct ProblemsData {
    pub checked: Checked,
    pub done: bool,
    pub error: Option<String>,
}

/// The check of one table.
pub struct ProblemsResults {
    pub version: u64,
    pub data: Arc<Mutex<ProblemsData>>,
    pub cancel: CancelToken,
    pub progress: Progress,
}

impl ProblemsResults {
    pub fn is_running(&self) -> bool {
        !self.data.lock().unwrap().done
    }
}

#[derive(Default)]
pub struct ProblemsUi {
    pub open: bool,
    /// Columns (1-based, comma-separated) whose values must be numbers.
    pub numeric: String,
}

/// What is wrong with a record, for the list.
pub fn describe(k: Kind) -> String {
    let col = |c: usize| (c + 1).to_string();
    match k {
        Kind::Fields { got, want } => trf("{got} fields instead of {want}", &[("got", &got.to_string()), ("want", &want.to_string())]),
        Kind::StrayQuote { col: c } => trf("column {n}: a stray quote", &[("n", &col(c))]),
        Kind::Unclosed { col: c } => trf("column {n}: a quote that is never closed", &[("n", &col(c))]),
        Kind::Control { col: c } => trf("column {n}: a control character", &[("n", &col(c))]),
        Kind::NotNumber { col: c } => trf("column {n}: not a number", &[("n", &col(c))]),
        Kind::Huge => tr("longer than 64 KB, not checked").to_string(),
    }
}

impl BigTextApp {
    /// Checks the active table's rows in the background and shows the problems panel.
    pub(crate) fn check_rows(&mut self) {
        let numeric: Vec<usize> = self.problems.numeric.split(',').filter_map(|s| s.trim().parse::<usize>().ok()).filter(|&n| n > 0).map(|n| n - 1).collect();
        let Some(d) = self.docs.get_mut(self.active) else { return };
        let Some(dialect) = d.table_state.dialect.filter(|_| d.is_ready()) else {
            self.notify(tr("Open the file as a table first (View → Table view).").into(), None);
            return;
        };
        let header = d.table_state.header;
        if let Some(p) = d.problems.take() {
            p.cancel.cancel();
        }
        let data = Arc::new(Mutex::new(ProblemsData::default()));
        let (cancel, progress) = (CancelToken::new(), Progress::new(d.buf.len()));
        d.problems = Some(ProblemsResults { version: d.version, data: data.clone(), cancel: cancel.clone(), progress: progress.clone() });
        self.problems.open = true;
        let snap = d.buf.snapshot();
        let waker = self.waker.clone();
        jobs::spawn("check-rows", move || {
            let mut batch: Vec<Problem> = Vec::new();
            let mut throttle = Throttle::new(Duration::from_millis(100));
            let r = csv_check::check(&snap, dialect, header, &numeric, &cancel, Some(&progress), |p| {
                batch.push(*p);
                if throttle.ready() {
                    data.lock().unwrap().checked.problems.append(&mut batch);
                    waker();
                }
            });
            let mut g = data.lock().unwrap();
            match r {
                Ok(Some(c)) => g.checked = c,
                Ok(None) => g.checked.problems.append(&mut batch),
                Err(e) => g.error = Some(e.to_string()),
            }
            g.done = true;
            drop(g);
            waker();
        });
    }

    pub(super) fn problems_panel(&mut self, ui: &mut egui::Ui) {
        let font = egui::FontId::monospace(self.settings.font_size.min(14.0));
        let (mut close, mut rerun, mut save, mut jump) = (false, false, false, None);
        let mut numeric = self.problems.numeric.clone();
        {
            let Some(d) = self.docs.get(self.active) else { return };
            let Some(r) = &d.problems else { return };
            let g = r.data.lock().unwrap();
            let stale = r.version != d.version;
            let n = g.checked.problems.len();
            ui.horizontal(|ui| {
                if g.done && g.error.is_none() {
                    ui.strong(trf("{n} of {records} rows have problems", &[("n", &fmt_count(n as u64)), ("records", &fmt_count(g.checked.records))]));
                    ui.label(trf("({fields} fields per row)", &[("fields", &g.checked.fields.to_string())]));
                } else {
                    ui.strong(trf("{n} rows with problems", &[("n", &fmt_count(n as u64))]));
                }
                if g.checked.capped {
                    ui.label(trf("(list capped at {n})", &[("n", &fmt_count(csv_check::PROBLEMS_MAX as u64))]));
                }
                ui.separator();
                ui.label(tr("Number columns:"));
                let resp = ui.add(egui::TextEdit::singleline(&mut numeric).desired_width(70.0).hint_text("3, 5"));
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    rerun = true;
                }
                if !g.done {
                    let f = r.progress.fraction();
                    ui.add(egui::ProgressBar::new(f).desired_width(140.0).text(format!("{:.0}%", f * 100.0)));
                    ui.ctx().request_repaint_after(Duration::from_millis(100));
                    if ui.small_button(tr("Cancel")).clicked() {
                        r.cancel.cancel();
                    }
                }
                if stale {
                    ui.colored_label(Color32::from_rgb(220, 150, 60), tr("stale (document changed)"));
                }
                if (stale || g.done) && ui.small_button(tr("Run again")).clicked() {
                    rerun = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("×").clicked() {
                        close = true;
                    }
                    if ui.add_enabled(g.done && !stale && n > 0, egui::Button::new(tr("Save rows…")).small()).clicked() {
                        save = true;
                    }
                });
            });
            if let Some(e) = &g.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            if n == 0 && g.done && g.error.is_none() {
                ui.weak(tr("Every row has the same number of fields, and nothing else looks wrong."));
            }
            let row_h = ui.ctx().fonts_mut(|f| f.row_height(&font)) + 4.0;
            egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, row_h, n, |ui, range| {
                for p in &g.checked.problems[range.start.min(n)..range.end.min(n)] {
                    let (line, exact) = d.buf.line_of(p.start);
                    let text = format!("{}  {}", trf("line {n}", &[("n", &format!("{}{}", if exact { "" } else { "~" }, fmt_count(line + 1)))]), describe(p.kind));
                    if ui.selectable_label(false, egui::RichText::new(text).font(font.clone())).clicked() && !stale {
                        jump = Some(p.start);
                    }
                }
            });
        }
        if let Some(off) = jump {
            if let Some(d) = self.docs.get_mut(self.active) {
                d.goto_offset(off);
            }
        }
        self.problems.numeric = numeric;
        if close {
            self.problems.open = false;
        }
        if rerun {
            self.check_rows();
        }
        if save {
            self.save_problem_rows();
        }
    }

    /// Writes the header (if any) and every record with a problem to a file.
    fn save_problem_rows(&mut self) {
        let Some(d) = self.docs.get(self.active) else { return };
        let Some(r) = &d.problems else { return };
        let rows: Vec<(u64, u64)> = r.data.lock().unwrap().checked.problems.iter().map(|p| (p.start, p.next)).collect();
        let (snap, header, name) = (d.buf.snapshot(), d.table_state.header, d.title.clone());
        let Some(target) = self.pick_output_file(tr("Save rows"), &format!("{name}.problems.csv")) else { return };
        self.spawn_task(tr("Saving rows").into(), move |progress: &Progress, cancel: &CancelToken| -> TaskOutcome {
            progress.set_total(rows.iter().map(|(a, b)| b - a).sum());
            tasks::write_via_temp(&target, |w| {
                let bom = snap.codec.bom_len as u64;
                if bom > 0 {
                    bigtext::filter::write_range_to(&snap, 0..bom, w, cancel, None)?;
                }
                if header {
                    let first = bigtext::filter::line_end_at(&snap, bom).unwrap_or(bom);
                    if rows.first().is_none_or(|r| r.0 > bom) {
                        bigtext::filter::write_range_to(&snap, bom..first, w, cancel, None)?;
                    }
                }
                for &(a, b) in &rows {
                    if bigtext::filter::write_range_to(&snap, a..b, w, cancel, Some(progress))?.is_none() {
                        return Ok(None);
                    }
                }
                Ok(Some(trf("Saved {n} rows to {file}.", &[("n", &fmt_count(rows.len() as u64)), ("file", &file_name(&target))])))
            })
        });
    }
}
