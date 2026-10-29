//! Search → Check JSON: the document checked strictly in the background, as one JSON value or
//! as JSON Lines, with the problems listed under the text; click one to go there.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use egui::Color32;

use bigtext::jobs::{self, CancelToken, Progress, Throttle};
use bigtext::json_check::{self, JsonChecked};

use super::filter_panel::fmt_count;
use super::i18n::{tr, trf};
use super::BigTextApp;

#[derive(Default)]
pub struct JsonCheckData {
    pub checked: JsonChecked,
    pub done: bool,
    pub error: Option<String>,
}

pub struct JsonCheckResults {
    /// Checked as JSON Lines.
    pub lines: bool,
    pub version: u64,
    pub data: Arc<Mutex<JsonCheckData>>,
    pub cancel: CancelToken,
    pub progress: Progress,
}

impl JsonCheckResults {
    pub fn is_running(&self) -> bool {
        !self.data.lock().unwrap().done
    }
}

#[derive(Default)]
pub struct JsonCheckUi {
    pub open: bool,
}

/// Whether the document looks like JSON Lines (by its name, or its first lines).
fn looks_like_lines(d: &super::document::Document) -> bool {
    let t = d.title.to_ascii_lowercase();
    if t.ends_with(".jsonl") || t.ends_with(".ndjson") {
        return true;
    }
    let start = d.min_caret();
    let Some(b) = d.buf.read(start..(start + 64 * 1024).min(d.buf.len())) else { return false };
    let lines: Vec<&[u8]> = b.split(|&c| c == b'\n').map(|l| l.trim_ascii()).filter(|l| !l.is_empty()).take(3).collect();
    lines.len() > 1 && lines.iter().all(|l| l.first() == Some(&b'{') && l.last() == Some(&b'}'))
}

impl BigTextApp {
    /// Checks the active document (as JSON Lines when `lines`, otherwise as one value; `None`:
    /// as it looks).
    pub(crate) fn check_json(&mut self, lines: Option<bool>) {
        let Some(d) = self.docs.get_mut(self.active).filter(|d| d.is_ready()) else { return };
        if d.buf.codec.unit() != 1 {
            self.notify(tr("Checking JSON needs a UTF-8 file.").into(), None);
            return;
        }
        let lines = lines.unwrap_or_else(|| looks_like_lines(d));
        if let Some(r) = d.json_check.take() {
            r.cancel.cancel();
        }
        let data = Arc::new(Mutex::new(JsonCheckData::default()));
        let (cancel, progress) = (CancelToken::new(), Progress::new(d.buf.len()));
        d.json_check = Some(JsonCheckResults { lines, version: d.version, data: data.clone(), cancel: cancel.clone(), progress: progress.clone() });
        self.json_check.open = true;
        let snap = d.buf.snapshot();
        let waker = self.waker.clone();
        jobs::spawn("check-json", move || {
            let res = if lines {
                let mut throttle = Throttle::new(Duration::from_millis(100));
                let mut batch = Vec::new();
                let r = json_check::check_lines(&snap, &cancel, Some(&progress), |p| {
                    batch.push(p.clone());
                    if throttle.ready() {
                        data.lock().unwrap().checked.problems.append(&mut batch);
                        waker();
                    }
                });
                r.map_err(|e| e.to_string())
            } else {
                Ok(json_check::check_document(&snap, &cancel, Some(&progress)))
            };
            let mut g = data.lock().unwrap();
            match res {
                Ok(Some(c)) => g.checked = c,
                Ok(None) => {}
                Err(e) => g.error = Some(e),
            }
            g.done = true;
            drop(g);
            waker();
        });
    }

    pub(super) fn json_check_panel(&mut self, ui: &mut egui::Ui) {
        let font = egui::FontId::monospace(self.settings.font_size.min(14.0));
        let (mut close, mut jump, mut rerun) = (false, None, None);
        {
            let Some(d) = self.docs.get(self.active) else { return };
            let Some(r) = &d.json_check else { return };
            let g = r.data.lock().unwrap();
            let n = g.checked.problems.len();
            let stale = r.version != d.version;
            ui.horizontal(|ui| {
                if g.done && n == 0 && g.error.is_none() {
                    let what = if r.lines { trf("{lines} lines", &[("lines", &fmt_count(g.checked.lines))]) } else { trf("{n} values", &[("n", &fmt_count(g.checked.values))]) };
                    ui.colored_label(Color32::from_rgb(90, 180, 90), trf("Valid JSON: {what}", &[("what", &what)]));
                } else {
                    ui.strong(trf("{n} JSON problems", &[("n", &fmt_count(n as u64))]));
                    if !r.lines && n > 0 {
                        ui.weak(tr("(the check stops at the first problem of a document)"));
                    }
                }
                if g.checked.capped {
                    ui.label(trf("(list capped at {n})", &[("n", &fmt_count(json_check::PROBLEMS_MAX as u64))]));
                }
                let mut lines = r.lines;
                ui.radio_value(&mut lines, false, tr("One document"));
                ui.radio_value(&mut lines, true, tr("JSON Lines"));
                if lines != r.lines {
                    rerun = Some(lines);
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
                    if ui.small_button(tr("Run again")).clicked() {
                        rerun = Some(r.lines);
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
            egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, row_h, n, |ui, range| {
                for p in &g.checked.problems[range.start.min(n)..range.end.min(n)] {
                    let (line, exact) = d.buf.line_of(p.at);
                    let text = format!("{}  {}  {}", trf("line {n}", &[("n", &format!("{}{}", if exact { "" } else { "~" }, fmt_count(line + 1)))]), trf("byte {n}", &[("n", &fmt_count(p.at))]), p.message);
                    if ui.selectable_label(false, egui::RichText::new(text).font(font.clone())).clicked() && !stale {
                        jump = Some(p.at);
                    }
                }
            });
        }
        if let Some(off) = jump {
            if let Some(d) = self.docs.get_mut(self.active) {
                d.goto_offset(off);
            }
        }
        if close {
            self.json_check.open = false;
        }
        if let Some(lines) = rerun {
            self.check_json(Some(lines));
        }
    }
}
