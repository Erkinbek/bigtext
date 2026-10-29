//! Time gaps (Search → Find time gaps…): a list under the text of the places where a log's next
//! timestamp is much later than the one before (or earlier: the clock jumped back), largest
//! first or in file order; click one to go there. The status bar shows the time since the
//! previous timestamped line for the caret's line.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use egui::Color32;

use bigtext::gaps::{self, Gaps};
use bigtext::jobs::{self, CancelToken, Progress};
use bigtext::timestamp;

use super::filter_panel::fmt_count;
use super::i18n::{tr, trf};
use super::BigTextApp;

/// Bytes looked back for the previous line with a timestamp.
const BACK: u64 = 64 * 1024;

#[derive(Default)]
pub struct GapsData {
    pub gaps: Gaps,
    pub done: bool,
    pub error: Option<String>,
}

/// The gaps of one document.
pub struct GapsResults {
    pub min_ms: i64,
    pub version: u64,
    pub data: Arc<Mutex<GapsData>>,
    pub cancel: CancelToken,
    pub progress: Progress,
}

impl GapsResults {
    pub fn is_running(&self) -> bool {
        !self.data.lock().unwrap().done
    }
}

pub struct GapsUi {
    pub open: bool,
    /// The smallest gap listed, in seconds.
    pub min_secs: f64,
    pub largest_first: bool,
    /// List order: (document, gaps, largest first) and the indices.
    order: Option<((u64, usize, bool), Vec<u32>)>,
    /// Time since the previous timestamped line, for (document, version, line start).
    delta: Option<((u64, u64, u64), Option<i64>)>,
}

impl Default for GapsUi {
    fn default() -> Self {
        GapsUi { open: false, min_secs: 30.0, largest_first: true, order: None, delta: None }
    }
}

impl BigTextApp {
    /// Shows the gaps panel, finding the gaps of the active document if needed.
    pub(crate) fn open_gaps(&mut self) {
        self.gaps.open = true;
        let min_ms = (self.gaps.min_secs * 1000.0) as i64;
        let fresh = self.docs.get(self.active).and_then(|d| d.gaps.as_ref().map(|g| g.version == d.version && g.min_ms == min_ms));
        if fresh != Some(true) {
            self.start_gaps();
        }
    }

    pub(crate) fn start_gaps(&mut self) {
        let min_ms = ((self.gaps.min_secs * 1000.0) as i64).max(1);
        let Some(d) = self.docs.get_mut(self.active) else { return };
        if !d.is_ready() {
            return;
        }
        if let Some(g) = d.gaps.take() {
            g.cancel.cancel();
        }
        let data = Arc::new(Mutex::new(GapsData::default()));
        let (cancel, progress) = (CancelToken::new(), Progress::new(d.buf.len()));
        d.gaps = Some(GapsResults { min_ms, version: d.version, data: data.clone(), cancel: cancel.clone(), progress: progress.clone() });
        let snap = d.buf.snapshot();
        let waker = self.waker.clone();
        jobs::spawn("gaps", move || {
            let partial = |g: &Gaps| {
                data.lock().unwrap().gaps = g.clone();
                waker();
            };
            let r = gaps::scan(&snap, min_ms, &cancel, Some(&progress), partial);
            let mut g = data.lock().unwrap();
            match r {
                Ok(Some(x)) => g.gaps = x,
                Ok(None) => {}
                Err(e) => g.error = Some(e.to_string()),
            }
            g.done = true;
            drop(g);
            waker();
        });
    }

    pub(super) fn gaps_panel(&mut self, ui: &mut egui::Ui) {
        let font = egui::FontId::monospace(self.settings.font_size.min(14.0));
        let (mut close, mut rerun, mut jump) = (false, false, None);
        let (mut secs, mut largest) = (self.gaps.min_secs, self.gaps.largest_first);
        {
            let Some(d) = self.docs.get(self.active) else { return };
            let Some(r) = &d.gaps else { return };
            let g = r.data.lock().unwrap();
            let stale = r.version != d.version;
            let n = g.gaps.gaps.len();
            ui.horizontal(|ui| {
                ui.strong(trf("{n} time gaps", &[("n", &fmt_count(n as u64))]));
                ui.label(tr("of at least"));
                ui.add(egui::DragValue::new(&mut secs).range(0.001..=86_400.0 * 30.0).speed(1.0).suffix(" s"));
                ui.checkbox(&mut largest, tr("Largest first"));
                if g.gaps.capped {
                    ui.label(trf("(list capped at {n})", &[("n", &fmt_count(gaps::GAPS_MAX as u64))]));
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
                        rerun = true;
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
            if n == 0 {
                if g.done && g.gaps.stamped == 0 {
                    ui.weak(tr("No timestamps found at the start of the file's lines."));
                } else if g.done {
                    ui.weak(tr("No gaps that long."));
                }
            } else {
                let key = (d.id, n, largest);
                if self.gaps.order.as_ref().map(|o| o.0) != Some(key) {
                    let mut idx: Vec<u32> = (0..n as u32).collect();
                    if largest {
                        idx.sort_by_key(|&i| std::cmp::Reverse(g.gaps.gaps[i as usize].ms().abs()));
                    }
                    self.gaps.order = Some((key, idx));
                }
                let order = &self.gaps.order.as_ref().expect("order").1;
                let row_h = ui.ctx().fonts_mut(|f| f.row_height(&font)) + 4.0;
                let dated = g.gaps.dated;
                egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, row_h, n, |ui, range| {
                    for &i in &order[range.start.min(n)..range.end.min(n)] {
                        let gap = g.gaps.gaps[i as usize];
                        let back = gap.ms() < 0;
                        let (line, exact) = d.buf.line_of(gap.after);
                        let text = format!(
                            "{:>18}   {} → {}   {}{}",
                            timestamp::format_delta(gap.ms()),
                            timestamp::format_key(gap.from, dated, true),
                            timestamp::format_key(gap.to, dated, true),
                            trf("line {n}", &[("n", &format!("{}{}", if exact { "" } else { "~" }, fmt_count(line + 1)))]),
                            if back { format!("  {}", tr("(clock went back)")) } else { String::new() },
                        );
                        let mut rt = egui::RichText::new(text).font(font.clone());
                        if back {
                            rt = rt.color(Color32::from_rgb(220, 150, 60));
                        }
                        if ui.selectable_label(false, rt).clicked() && !stale {
                            jump = Some(gap.after);
                        }
                    }
                });
            }
        }
        if let Some(off) = jump {
            if let Some(d) = self.docs.get_mut(self.active) {
                d.goto_offset(off);
            }
        }
        if close {
            self.gaps.open = false;
        }
        self.gaps.largest_first = largest;
        if secs != self.gaps.min_secs {
            self.gaps.min_secs = secs;
            rerun = true;
        }
        if rerun {
            self.start_gaps();
        }
    }

    /// Time from the previous line with a timestamp to the caret's line, for logs (cached).
    pub(crate) fn caret_time_delta(&mut self) -> Option<i64> {
        let d = self.docs.get(self.active)?;
        if !d.is_ready() || d.hex || d.table || d.effective_lang() != crate::view::highlight::Lang::Log {
            return None;
        }
        let (ls, _, _) = d.line_bounds(d.view.caret, BACK)?;
        let key = (d.id, d.version, ls);
        if let Some((k, v)) = self.gaps.delta {
            if k == key {
                return v;
            }
        }
        let v = prev_delta(&d.buf, ls);
        self.gaps.delta = Some((key, v));
        v
    }
}

/// Milliseconds between the timestamp of the line at `ls` and that of the nearest line above it
/// with one (within [`BACK`] bytes).
fn prev_delta(buf: &bigtext::text::TextBuf, ls: u64) -> Option<i64> {
    let here = timestamp::stamp_at(buf, ls)?;
    let codec = buf.codec;
    let unit = codec.unit() as u64;
    let lo = ls.saturating_sub(BACK);
    let lo = lo - lo % unit;
    let b = buf.read(lo..ls)?;
    // Line starts above `ls`, nearest first.
    let mut end = b.len().saturating_sub(unit as usize);
    loop {
        let start = match codec.rfind_nl(&b[..end], lo) {
            Some(i) => i + unit as usize,
            None if lo == 0 => 0,
            None => return None,
        };
        let line = &b[start..end];
        let text = codec.decode_lossy(&line[..line.len().min(640) - line.len().min(640) % unit as usize]);
        if let Some(prev) = timestamp::parse(&text) {
            if prev.has_date != here.has_date {
                return None;
            }
            let ms = if prev.tz.is_some() && here.tz.is_some() { here.utc_ms() - prev.utc_ms() } else { here.ms - prev.ms };
            return Some(ms);
        }
        if start == 0 {
            return None;
        }
        end = start - unit as usize;
    }
}
