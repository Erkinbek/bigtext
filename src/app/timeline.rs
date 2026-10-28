//! Time histogram (Search → Time histogram): a bar chart under the text of how many log lines
//! fall in each minute (hour, day, …), with the errors or the search matches among them in
//! colour. Hover a bar for its counts; click it to jump to its first line.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use egui::{Color32, Rect, Sense, Stroke};

use bigtext::histogram::{self, Histogram};
use bigtext::jobs::{self, CancelToken, Progress};
use bigtext::loglevel::{self, Level, Levels};
use bigtext::search::Matcher;
use bigtext::timestamp;

use super::filter_panel::fmt_count;
use super::i18n::{tr, trf};
use super::BigTextApp;
use crate::view::highlight::{self, Kind};

/// What the coloured part of a bar counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Count {
    Errors,
    Search,
    Nothing,
}

impl Count {
    fn label(self) -> &'static str {
        match self {
            Count::Errors => "Errors",
            Count::Search => "Search matches",
            Count::Nothing => "Lines only",
        }
    }
}

#[derive(Default)]
pub struct TimelineData {
    pub hist: Option<Histogram>,
    pub done: bool,
    pub error: Option<String>,
}

/// A histogram of one document.
pub struct TimelineResults {
    pub count: Count,
    pub version: u64,
    pub data: Arc<Mutex<TimelineData>>,
    pub cancel: CancelToken,
    pub progress: Progress,
}

impl TimelineResults {
    pub fn is_running(&self) -> bool {
        !self.data.lock().unwrap().done
    }
}

pub struct TimelineUi {
    pub open: bool,
    pub count: Count,
    /// The bars drawn last frame and their first lines (for clicks, and for tests).
    pub bars: Vec<(Rect, u64)>,
}

impl Default for TimelineUi {
    fn default() -> Self {
        TimelineUi { open: false, count: Count::Errors, bars: Vec::new() }
    }
}

/// Bars are at least this many pixels wide.
const BAR_MIN_PX: f32 = 5.0;

impl BigTextApp {
    /// Shows the histogram panel, building the histogram of the active document if needed.
    pub(crate) fn open_timeline(&mut self) {
        self.timeline.open = true;
        let fresh = self.docs.get(self.active).and_then(|d| d.timeline.as_ref().map(|t| t.version == d.version && t.count == self.timeline.count));
        if fresh != Some(true) {
            self.start_timeline();
        }
    }

    pub(crate) fn start_timeline(&mut self) {
        let count = self.timeline.count;
        let Some(d) = self.docs.get(self.active) else { return };
        if !d.is_ready() {
            return;
        }
        let codec = d.buf.codec;
        let m: Option<Arc<Matcher>> = match count {
            Count::Errors => loglevel::query(Levels::only(Level::Error)).and_then(|q| Matcher::new(&q, codec).ok()).map(Arc::new),
            Count::Search if !self.search.query.pattern.is_empty() => match self.search.matcher(codec) {
                Ok(m) => Some(m),
                Err(e) => {
                    self.error = Some(e);
                    return;
                }
            },
            _ => None,
        };
        let d = &mut self.docs[self.active];
        if let Some(t) = d.timeline.take() {
            t.cancel.cancel();
        }
        let data = Arc::new(Mutex::new(TimelineData::default()));
        let (cancel, progress) = (CancelToken::new(), Progress::new(d.buf.len()));
        d.timeline = Some(TimelineResults { count, version: d.version, data: data.clone(), cancel: cancel.clone(), progress: progress.clone() });
        let snap = d.buf.snapshot();
        let waker = self.waker.clone();
        jobs::spawn("timeline", move || {
            let partial = |h: &Histogram| {
                data.lock().unwrap().hist = Some(h.clone());
                waker();
            };
            let r = histogram::build(&snap, m.as_deref(), &cancel, Some(&progress), partial);
            let mut g = data.lock().unwrap();
            match r {
                Ok(Some(h)) => g.hist = Some(h),
                Ok(None) => {}
                Err(e) => g.error = Some(e.to_string()),
            }
            g.done = true;
            drop(g);
            waker();
        });
    }

    pub(super) fn timeline_panel(&mut self, ui: &mut egui::Ui) {
        let mut close = false;
        let mut rerun = false;
        let mut jump = None;
        let dark = ui.visuals().dark_mode;
        let count = self.timeline.count;
        {
            let Some(d) = self.docs.get(self.active) else { return };
            let Some(t) = &d.timeline else { return };
            let g = t.data.lock().unwrap();
            let stale = t.version != d.version;
            let hits_color = match t.count {
                Count::Search => Color32::from_rgb(220, 170, 0),
                _ => highlight::color(Kind::Error, dark),
            };
            let mut picked = count;
            ui.horizontal(|ui| {
                ui.strong(tr("Time histogram"));
                egui::ComboBox::from_id_salt("timeline-count").selected_text(tr(picked.label())).show_ui(ui, |ui| {
                    for c in [Count::Errors, Count::Search, Count::Nothing] {
                        ui.selectable_value(&mut picked, c, tr(c.label()));
                    }
                });
                if !g.done {
                    let f = t.progress.fraction();
                    ui.add(egui::ProgressBar::new(f).desired_width(140.0).text(format!("{:.0}%", f * 100.0)));
                    ui.ctx().request_repaint_after(Duration::from_millis(100));
                    if ui.small_button(tr("Cancel")).clicked() {
                        t.cancel.cancel();
                    }
                }
                if stale {
                    ui.colored_label(Color32::from_rgb(220, 150, 60), tr("stale (document changed)"));
                    if ui.small_button(tr("Run again")).clicked() {
                        rerun = true;
                    }
                }
                if let Some(h) = &g.hist {
                    let mut info = trf("{n} lines", &[("n", &fmt_count(h.lines))]);
                    if t.count != Count::Nothing {
                        let what = if t.count == Count::Errors { tr("errors") } else { tr("matches") };
                        info.push_str(&format!(" · {} {what}", fmt_count(h.hits)));
                    }
                    if h.untimed > 0 {
                        info.push_str(" · ");
                        info.push_str(&trf("{n} before the first time", &[("n", &fmt_count(h.untimed))]));
                    }
                    ui.label(info);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("×").clicked() {
                        close = true;
                    }
                });
            });
            if picked != count {
                self.timeline.count = picked;
                rerun = true;
            }
            if let Some(e) = &g.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            self.timeline.bars.clear();
            match &g.hist {
                Some(h) if !h.buckets.is_empty() => {
                    let caret_key = d.line_bounds(d.view.caret, 64 * 1024).and_then(|b| timestamp::stamp_at(&d.buf, b.0)).filter(|s| h.dated && s.has_date).map(|s| s.ms);
                    jump = chart(ui, h, hits_color, caret_key, stale, &mut self.timeline.bars);
                }
                _ if g.done => {
                    ui.weak(tr("No timestamps found at the start of the file's lines."));
                }
                _ => {
                    ui.weak(tr("Reading the times…"));
                }
            }
        }
        if let Some(off) = jump {
            if let Some(d) = self.docs.get_mut(self.active) {
                d.goto_offset(off);
            }
        }
        if close {
            self.timeline.open = false;
        }
        if rerun {
            self.start_timeline();
        }
    }
}

/// Draws the bars; returns the first line of a clicked bar.
fn chart(ui: &mut egui::Ui, h: &Histogram, hits_color: Color32, caret_key: Option<i64>, stale: bool, out: &mut Vec<(Rect, u64)>) -> Option<u64> {
    // All the height: a panel shrinks to what its content takes.
    let size = egui::vec2(ui.available_width(), ui.available_height().max(60.0));
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let axis_h = ui.text_style_height(&egui::TextStyle::Small) + 4.0;
    let plot = Rect::from_min_max(rect.min, egui::pos2(rect.max.x, rect.max.y - axis_h));
    let max_bars = ((plot.width() / BAR_MIN_PX) as usize).max(10);
    let step = h.step_for(max_bars);
    let bars = h.bars(step);
    let (a, b) = h.span()?;
    let first = a.div_euclid(step) * step;
    let n = ((b.div_euclid(step) - a.div_euclid(step)) + 1) as f32;
    let bar_w = plot.width() / n;
    let top = bars.iter().map(|(_, x)| x.lines).max().unwrap_or(1).max(1) as f32;
    let painter = ui.painter_at(rect);
    let v = ui.visuals();
    let base = if stale { v.weak_text_color().gamma_multiply(0.4) } else { v.weak_text_color().gamma_multiply(0.7) };
    let hits_color = if stale { hits_color.gamma_multiply(0.5) } else { hits_color };
    let hover = resp.hover_pos();
    let mut hovered = None;
    for (start, x) in &bars {
        let i = ((start - first) / step) as f32;
        let x0 = plot.left() + i * bar_w;
        let col = Rect::from_min_max(egui::pos2(x0, plot.top()), egui::pos2(x0 + bar_w, plot.bottom()));
        let hgt = (x.lines as f32 / top * plot.height()).max(1.0);
        let gap = if bar_w > 3.0 { 1.0 } else { 0.0 };
        let r = Rect::from_min_max(egui::pos2(x0, plot.bottom() - hgt), egui::pos2(x0 + bar_w - gap, plot.bottom()));
        let over = hover.is_some_and(|p| p.x >= col.left() && p.x < col.right());
        painter.rect_filled(r, 0.0, if over { v.strong_text_color() } else { base });
        if x.hits > 0 {
            let hh = (x.hits as f32 / top * plot.height()).max(1.5);
            painter.rect_filled(Rect::from_min_max(egui::pos2(x0, plot.bottom() - hh), egui::pos2(x0 + bar_w - gap, plot.bottom())), 0.0, hits_color);
        }
        out.push((col, x.first));
        if over {
            hovered = Some((*start, *x));
        }
    }
    if let Some(k) = caret_key.filter(|k| (first..first + n as i64 * step).contains(k)) {
        let x = plot.left() + (k - first) as f32 / step as f32 * bar_w;
        painter.vline(x, plot.y_range(), Stroke::new(1.5, v.selection.stroke.color));
    }
    let small = egui::TextStyle::Small.resolve(ui.style());
    let label = |k: i64| timestamp::format_key(k, h.dated, step < 60_000);
    painter.text(egui::pos2(plot.left() + 2.0, rect.bottom()), egui::Align2::LEFT_BOTTOM, label(first), small.clone(), v.text_color());
    painter.text(egui::pos2(plot.right() - 2.0, rect.bottom()), egui::Align2::RIGHT_BOTTOM, label(first + n as i64 * step), small.clone(), v.text_color());
    painter.text(egui::pos2(plot.center().x, rect.bottom()), egui::Align2::CENTER_BOTTOM, trf("1 bar = {step}", &[("step", &step_text(step))]), small, v.weak_text_color());
    if let Some((start, x)) = hovered {
        let mut tip = format!("{} – {}\n{}", label(start), label(start + step), trf("{n} lines", &[("n", &fmt_count(x.lines))]));
        if x.hits > 0 {
            tip.push_str(&format!(", {}", trf("{n} highlighted", &[("n", &fmt_count(x.hits))])));
        }
        tip.push('\n');
        tip.push_str(tr("Click to go to the first line"));
        resp.clone().on_hover_text_at_pointer(tip);
    }
    if resp.clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            return out.iter().find(|(r, _)| p.x >= r.left() && p.x < r.right()).map(|x| x.1);
        }
    }
    None
}

/// "5 min", "1 h", "2 d".
fn step_text(ms: i64) -> String {
    match ms {
        m if m % 86_400_000 == 0 => format!("{} {}", m / 86_400_000, tr("d")),
        m if m % 3_600_000 == 0 => format!("{} {}", m / 3_600_000, tr("h")),
        m if m % 60_000 == 0 => format!("{} {}", m / 60_000, tr("min")),
        m => format!("{} {}", m / 1000, tr("s")),
    }
}
