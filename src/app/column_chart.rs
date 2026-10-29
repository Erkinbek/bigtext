//! Chart… (a column header's menu): under the table, how many rows have each value of a column —
//! in ranges for numbers, in periods for times, the most frequent values for text. Click a bar to
//! filter the rows to it (or, for times, to go to its first row).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bigtext::csv::{Cond, CondOp};
use bigtext::csv_chart::{self, ChartKind, ColumnChart};
use bigtext::jobs::{self, CancelToken, Progress};

use super::csv_tools::CondDialog;
use super::filter_panel::fmt_count;
use super::i18n::{tr, trf};
use super::BigTextApp;

#[derive(Default)]
pub struct ChartData {
    pub chart: Option<ColumnChart>,
    pub done: bool,
    pub error: Option<String>,
}

pub struct ChartResults {
    pub col: usize,
    pub name: String,
    pub version: u64,
    pub data: Arc<Mutex<ChartData>>,
    pub cancel: CancelToken,
    pub progress: Progress,
}

impl ChartResults {
    pub fn is_running(&self) -> bool {
        !self.data.lock().unwrap().done
    }
}

#[derive(Default)]
pub struct ChartUi {
    pub open: bool,
}

impl BigTextApp {
    pub(crate) fn start_column_chart(&mut self, col: usize) {
        let name = self.column_names_of(self.active).get(col).cloned().unwrap_or_default();
        let Some(d) = self.docs.get_mut(self.active) else { return };
        let Some(dialect) = d.table_state.dialect else { return };
        if let Some(c) = d.chart.take() {
            c.cancel.cancel();
        }
        let data = Arc::new(Mutex::new(ChartData::default()));
        let (cancel, progress) = (CancelToken::new(), Progress::new(d.buf.len()));
        d.chart = Some(ChartResults { col, name, version: d.version, data: data.clone(), cancel: cancel.clone(), progress: progress.clone() });
        self.chart.open = true;
        let (snap, header) = (d.buf.snapshot(), d.table_state.header);
        let waker = self.waker.clone();
        jobs::spawn("column-chart", move || {
            let partial = |c: &ColumnChart| {
                data.lock().unwrap().chart = Some(c.clone());
                waker();
            };
            let r = csv_chart::column_chart(&snap, dialect, col, header, &cancel, Some(&progress), partial);
            let mut g = data.lock().unwrap();
            match r {
                Ok(c) => g.chart = Some(c),
                Err(bigtext::csv::ScanEnd::Cancelled) => {}
                Err(_) => g.error = Some(tr("Could not read the document.").into()),
            }
            g.done = true;
            drop(g);
            waker();
        });
    }

    pub(super) fn chart_panel(&mut self, ui: &mut egui::Ui) {
        let (mut close, mut pick) = (false, None);
        {
            let Some(d) = self.docs.get(self.active) else { return };
            let Some(r) = &d.chart else { return };
            let g = r.data.lock().unwrap();
            ui.horizontal(|ui| {
                ui.strong(trf("Values of “{name}”", &[("name", &r.name)]));
                if let Some(c) = &g.chart {
                    let kind = match c.kind {
                        ChartKind::Numbers => tr("numbers"),
                        ChartKind::Times => tr("times"),
                        ChartKind::Text => tr("text"),
                    };
                    ui.label(format!("{kind} · {}", trf("{n} rows", &[("n", &fmt_count(c.values))])));
                    if c.other > 0 {
                        ui.weak(trf("({n} empty or other)", &[("n", &fmt_count(c.other))]));
                    }
                    if c.kind == ChartKind::Text && c.bars.len() >= csv_chart::TEXT_BARS {
                        ui.weak(trf("the {n} most frequent", &[("n", &csv_chart::TEXT_BARS.to_string())]));
                    }
                }
                if !g.done {
                    let f = r.progress.fraction();
                    ui.add(egui::ProgressBar::new(f).desired_width(140.0).text(format!("{:.0}%", f * 100.0)));
                    ui.ctx().request_repaint_after(Duration::from_millis(100));
                    if ui.small_button(tr("Cancel")).clicked() {
                        r.cancel.cancel();
                    }
                }
                if r.version != d.version {
                    ui.colored_label(egui::Color32::from_rgb(220, 150, 60), tr("stale (document changed)"));
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
            let Some(c) = &g.chart else { return };
            let top = c.bars.iter().map(|b| b.count).max().unwrap_or(1).max(1) as f32;
            let font = egui::FontId::monospace(12.0);
            let row_h = 18.0;
            let color = ui.visuals().selection.bg_fill;
            egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, row_h, c.bars.len(), |ui, range| {
                for (i, b) in c.bars.iter().enumerate().skip(range.start).take(range.len()) {
                    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), egui::Sense::click());
                    let label_w = (rect.width() * 0.35).min(260.0);
                    let count_w = 90.0;
                    let p = ui.painter_at(rect);
                    let v = ui.visuals();
                    if resp.hovered() {
                        p.rect_filled(rect, 2.0, v.faint_bg_color);
                    }
                    let label: String = if b.label.chars().count() > 34 { b.label.chars().take(33).chain(['…']).collect() } else { b.label.clone() };
                    p.text(egui::pos2(rect.left() + label_w - 6.0, rect.center().y), egui::Align2::RIGHT_CENTER, label, font.clone(), v.text_color());
                    let bar_room = (rect.width() - label_w - count_w).max(10.0);
                    let w = (b.count as f32 / top * bar_room).max(1.0);
                    p.rect_filled(egui::Rect::from_min_size(egui::pos2(rect.left() + label_w, rect.top() + 3.0), egui::vec2(w, row_h - 6.0)), 2.0, color);
                    p.text(egui::pos2(rect.left() + label_w + w + 6.0, rect.center().y), egui::Align2::LEFT_CENTER, fmt_count(b.count), font.clone(), v.weak_text_color());
                    if resp.on_hover_text(format!("{}\n{}", b.label, tr("Click: the rows of this bar"))).clicked() {
                        pick = Some(i);
                    }
                }
            });
        }
        if close {
            self.chart.open = false;
        }
        if let Some(i) = pick {
            self.chart_bar_clicked(i);
        }
    }

    /// The rows of bar `i`: the conditions dialog for them, or the first row of a period.
    pub(crate) fn chart_bar_clicked(&mut self, i: usize) {
        let names = self.column_names_of(self.active);
        let Some(d) = self.docs.get_mut(self.active) else { return };
        let Some(r) = &d.chart else { return };
        let col = r.col;
        let Some((kind, bar)) = r.data.lock().unwrap().chart.as_ref().and_then(|c| c.bars.get(i).map(|b| (c.kind, b.clone()))) else { return };
        let show = |x: f64| if x.fract() == 0.0 { format!("{}", x as i64) } else { format!("{x}") };
        let conds = match kind {
            ChartKind::Times => {
                d.goto_offset(bar.first);
                return;
            }
            ChartKind::Text => vec![Cond { col, op: CondOp::Equals, value: bar.label }],
            ChartKind::Numbers => vec![Cond { col, op: CondOp::Greater, value: show(bar.lo) }, Cond { col, op: CondOp::Less, value: show(bar.hi) }],
        };
        self.csv.conds = Some(CondDialog { conds, names });
    }
}
