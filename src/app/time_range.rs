//! A time range (the search bar's Time button): Filter, Find All and the level bar then read only
//! the lines between two times, found with two binary searches instead of a full pass.

use bigtext::jobs::{self, CancelToken};
use bigtext::loglevel::Levels;
use bigtext::timestamp::{self, Stamp};

use super::i18n::{tr, trf};
use super::{BigTextApp, Msg};

/// What to do once the range's lines are found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum After {
    Filter,
    FindAll,
    Levels(Levels),
    Refilter,
}

/// The range in effect.
pub struct ActiveRange {
    pub from: String,
    pub to: String,
    /// Its bytes in (document, version): start, end.
    pub found: Option<(u64, u64, u64, u64)>,
    pending: Option<(u64, After, CancelToken)>,
}

#[derive(Default)]
pub struct TimeRangeUi {
    /// The popup is open.
    pub open: bool,
    pub from: String,
    pub to: String,
    pub active: Option<ActiveRange>,
}

pub struct RangeDone {
    doc: u64,
    version: u64,
    result: Result<(u64, u64), String>,
}

/// The lines to read for the active document.
pub(crate) enum Window {
    /// No range: the whole document.
    All,
    Bytes(u64, u64),
    /// Being found; the action runs again when it is.
    Wait,
}

fn parse(t: &str) -> Result<Option<Stamp>, String> {
    let t = t.trim();
    if t.is_empty() {
        return Ok(None);
    }
    timestamp::parse(t).map(Some).ok_or_else(|| trf("Not a time: {text}", &[("text", t)]))
}

impl BigTextApp {
    /// The bytes of the time range for the active document; when they are not known yet, starts
    /// finding them and runs `after` then.
    pub(crate) fn time_window(&mut self, after: After) -> Window {
        let Some(r) = self.range.active.as_mut() else { return Window::All };
        let Some(d) = self.docs.get(self.active) else { return Window::All };
        if let Some((doc, version, a, b)) = r.found {
            if doc == d.id && version == d.version {
                return Window::Bytes(a, b);
            }
        }
        let (from, to) = match (parse(&r.from), parse(&r.to)) {
            (Ok(a), Ok(b)) => (a, b),
            (Err(e), _) | (_, Err(e)) => {
                self.notify(e, None);
                return Window::All;
            }
        };
        if let Some((_, _, c)) = r.pending.take() {
            c.cancel();
        }
        let cancel = CancelToken::new();
        r.pending = Some((d.id, after, cancel.clone()));
        let (snap, doc, version) = (d.buf.snapshot(), d.id, d.version);
        let (tx, waker) = (self.tx.clone(), self.waker.clone());
        jobs::spawn("time-range", move || {
            let result = timestamp::time_range(&snap, from, to, &cancel).map_err(|e| match e {
                timestamp::TimeError::NoTimestamps => tr("No timestamps found at the start of the file's lines.").to_string(),
                timestamp::TimeError::Cancelled => String::new(),
            });
            let _ = tx.send(Msg::TimeRange(Box::new(RangeDone { doc, version, result })));
            waker();
        });
        Window::Wait
    }

    pub(super) fn on_time_range(&mut self, done: RangeDone) {
        let Some(r) = self.range.active.as_mut() else { return };
        let Some((doc, after, _)) = r.pending.take_if(|p| p.0 == done.doc) else { return };
        match done.result {
            Ok((a, b)) => r.found = Some((doc, done.version, a, b)),
            Err(e) => {
                if !e.is_empty() {
                    self.notify(e, None);
                }
                self.range.active = None;
            }
        }
        if self.docs.get(self.active).map(|d| d.id) != Some(doc) {
            return;
        }
        match after {
            After::Filter => self.start_filter(),
            After::FindAll => self.find_all(),
            After::Levels(lv) => self.filter_levels(lv),
            After::Refilter => self.rerun_filter(),
        }
    }

    /// "between 14:30:00 and 14:35:00", for headers.
    pub(crate) fn range_label(&self) -> Option<String> {
        let r = self.range.active.as_ref()?;
        Some(match (r.from.trim(), r.to.trim()) {
            ("", to) => trf("up to {to}", &[("to", to)]),
            (from, "") => trf("from {from}", &[("from", from)]),
            (from, to) => trf("between {from} and {to}", &[("from", from), ("to", to)]),
        })
    }

    /// Sets the range (empty texts: none) and runs the open filter again within it.
    pub(crate) fn apply_range(&mut self, from: String, to: String) {
        if let Some(p) = self.range.active.take().and_then(|r| r.pending) {
            p.2.cancel();
        }
        if !from.trim().is_empty() || !to.trim().is_empty() {
            for t in [&from, &to] {
                if let Err(e) = parse(t) {
                    self.notify(e, None);
                    return;
                }
            }
            self.range.active = Some(ActiveRange { from, to, found: None, pending: None });
        }
        if self.docs.get(self.active).is_some_and(|d| d.filter.is_some()) {
            self.rerun_filter();
        }
    }

    /// The popup of the search bar's Time button.
    pub(super) fn time_range_window(&mut self, ctx: &egui::Context) {
        if !self.range.open {
            return;
        }
        let mut open = true;
        let (mut apply, mut clear, mut around) = (false, false, None);
        egui::Window::new(tr("Time range")).open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
            ui.label(tr("Filter, Find All and the level bar read only the lines between these times (\"14:30\", \"2024-05-01 14:30:00\"; either may be empty)."));
            egui::Grid::new("time-range").num_columns(2).show(ui, |ui| {
                ui.label(tr("From"));
                ui.add(egui::TextEdit::singleline(&mut self.range.from).desired_width(200.0));
                ui.end_row();
                ui.label(tr("To"));
                ui.add(egui::TextEdit::singleline(&mut self.range.to).desired_width(200.0)).on_hover_text(tr("Up to and including this second"));
                ui.end_row();
            });
            ui.horizontal(|ui| {
                if ui.button(tr("±30 s around the caret")).clicked() {
                    around = Some(30_000);
                }
                if ui.button(tr("±5 min around the caret")).clicked() {
                    around = Some(300_000);
                }
            });
            ui.separator();
            ui.horizontal(|ui| {
                apply = ui.button(tr("Apply")).clicked();
                clear = ui.button(tr("Clear")).clicked();
            });
        });
        if let Some(ms) = around {
            let st = self.docs.get(self.active).and_then(|d| d.line_bounds(d.view.caret, 64 * 1024).and_then(|b| timestamp::stamp_at(&d.buf, b.0)));
            match st {
                Some(st) => {
                    self.range.from = timestamp::format_key(st.ms - ms, st.has_date, true);
                    self.range.to = timestamp::format_key(st.ms + ms, st.has_date, true);
                    apply = true;
                }
                None => self.notify(tr("The caret's line has no timestamp.").into(), None),
            }
        }
        if clear {
            self.range.from.clear();
            self.range.to.clear();
            apply = true;
        }
        if apply {
            let (from, to) = (self.range.from.clone(), self.range.to.clone());
            self.apply_range(from, to);
            open = false;
        }
        self.range.open = open;
    }
}
