//! Log level bar: one click lists only the ERROR (WARN, INFO, DEBUG) lines in the filter view;
//! F8 / Shift+F8 jump to the next / previous line of those levels (errors when none is chosen).

use egui::RichText;

use bigtext::filter::{self, FilterSpec};
use bigtext::jobs::{self, CancelToken};
use bigtext::loglevel::{self, Level, Levels};
use bigtext::search::{self, Matcher};

use super::commands::{key_text, Cmd};
use super::i18n::{tr, trf};
use super::{BigTextApp, Msg};
use crate::view::highlight::{self, Kind, Lang};

/// A line of the levels was found by F8 / Shift+F8.
pub struct LevelFound {
    pub doc: u64,
    pub version: u64,
    pub levels: Levels,
    pub found: Option<(u64, u64)>,
    pub wrapped: bool,
}

fn kind(l: Level) -> Kind {
    match l {
        Level::Error => Kind::Error,
        Level::Warn => Kind::Warn,
        Level::Info => Kind::Info,
        Level::Debug => Kind::Debug,
    }
}

impl BigTextApp {
    /// The levels the active document's filter shows (empty when it has no level filter).
    fn shown_levels(&self) -> Levels {
        self.docs.get(self.active).and_then(|d| d.filter.as_ref()).and_then(|r| r.levels).unwrap_or_default()
    }

    /// Whether the level bar is shown for the active document.
    pub(crate) fn level_bar_visible(&self) -> bool {
        self.settings.level_bar && self.docs.get(self.active).is_some_and(|d| d.is_ready() && !d.hex && !d.table && d.effective_lang() == Lang::Log)
    }

    /// Lists the lines of `levels` in the filter view (an empty set closes a level filter).
    pub(crate) fn filter_levels(&mut self, levels: Levels) {
        let Some(d) = self.docs.get_mut(self.active) else { return };
        if !d.is_ready() {
            return;
        }
        let Some(query) = loglevel::query(levels) else {
            if d.filter.as_ref().is_some_and(|r| r.levels.is_some()) {
                if let Some(r) = d.filter.take() {
                    r.cancel.cancel();
                }
            }
            return;
        };
        // Conditions added to a level filter stay when the levels change.
        let terms = d.filter.as_ref().filter(|r| r.levels.is_some()).map(|r| r.terms.clone()).unwrap_or_default();
        let codec = d.buf.codec;
        let range = match self.time_window(super::time_range::After::Levels(levels)) {
            super::time_range::Window::Wait => return,
            super::time_range::Window::Bytes(a, b) => Some((a, b)),
            super::time_range::Window::All => None,
        };
        match Matcher::new(&query, codec) {
            Ok(m) => {
                let spec = self.with_context(FilterSpec { range, ..Default::default() });
                self.run_filter_req(super::filter_panel::FilterRequest { query, m: std::sync::Arc::new(m), spec, levels: Some(levels), follow: None, terms }, None);
            }
            Err(e) => self.error = Some(e),
        }
    }

    /// Moves to the next (previous) line of the shown levels, or of ERROR when none is shown.
    pub(crate) fn next_level(&mut self, forward: bool) {
        let levels = Some(self.shown_levels()).filter(|l| !l.is_empty()).unwrap_or(Levels::only(Level::Error));
        let Some(d) = self.docs.get_mut(self.active) else { return };
        if !d.is_ready() {
            return;
        }
        let m = match loglevel::query(levels).map(|q| Matcher::new(&q, d.buf.codec)) {
            Some(Ok(m)) => m,
            Some(Err(e)) => {
                self.error = Some(e);
                return;
            }
            None => return,
        };
        if let Some(j) = d.find_job.take() {
            j.cancel.cancel();
        }
        let cancel = CancelToken::new();
        d.find_job = Some(super::search_panel::FindJob { cancel: cancel.clone() });
        let (snap, caret, version, id) = (d.buf.snapshot(), d.view.caret, d.version, d.id);
        let (tx, waker) = (self.tx.clone(), self.waker.clone());
        jobs::spawn("next-level", move || {
            let len = snap.len();
            let mut wrapped = false;
            let found = if forward {
                // From the next line, so the caret's own line is skipped.
                let from = filter::line_end_at(&snap, caret).unwrap_or(len);
                let first = |range: std::ops::Range<u64>| {
                    let mut f = None;
                    search::search_forward(&snap, &m, range, &cancel, None, |a, b| {
                        f = Some((a, b));
                        false
                    });
                    f
                };
                first(from..len).or_else(|| {
                    wrapped = true;
                    first(0..from.min(len))
                })
            } else {
                let from = filter::line_start_at(&snap, caret).unwrap_or(0);
                search::search_backward(&snap, &m, 0, from, &cancel).or_else(|| {
                    wrapped = true;
                    search::search_backward(&snap, &m, from, len, &cancel)
                })
            };
            if !cancel.is_cancelled() {
                let _ = tx.send(Msg::LevelFound(Box::new(LevelFound { doc: id, version, levels, found, wrapped })));
                waker();
            }
        });
    }

    pub(super) fn on_level_found(&mut self, f: LevelFound) {
        let Some(d) = self.docs.iter_mut().find(|d| d.id == f.doc) else { return };
        d.find_job = None;
        if d.version != f.version {
            return;
        }
        match f.found {
            Some((s, e)) => {
                d.select(s, e);
                d.view.center();
                if f.wrapped {
                    self.notify(tr("Wrapped around").into(), None);
                }
            }
            None => self.notify(trf("No {levels} lines", &[("levels", &f.levels.names())]), None),
        }
    }

    pub(super) fn level_bar(&mut self, ui: &mut egui::Ui) {
        let on = self.shown_levels();
        let dark = ui.visuals().dark_mode;
        let nav = if on.is_empty() { Levels::only(Level::Error) } else { on };
        let keys = |c: Cmd| self.keymap.get(c).map(|k| format!(" ({})", key_text(k))).unwrap_or_default();
        let (prev_tip, next_tip) = (
            format!("{}{}", trf("Previous {levels} line", &[("levels", &nav.names())]), keys(Cmd::PrevLevel)),
            format!("{}{}", trf("Next {levels} line", &[("levels", &nav.names())]), keys(Cmd::NextLevel)),
        );
        let mut toggle = None;
        let mut step = None;
        let mut hide = false;
        ui.horizontal(|ui| {
            ui.label(tr("Levels:"));
            for l in Level::ALL {
                let text = RichText::new(l.name()).monospace().strong().color(highlight::color(kind(l), dark));
                let tip = if on.contains(l) { tr("Click to stop showing these lines") } else { tr("Show only the lines of this level (click more levels to add them)") };
                if ui.selectable_label(on.contains(l), text).on_hover_text(tip).clicked() {
                    toggle = Some(l);
                }
            }
            ui.separator();
            if ui.button("◀").on_hover_text(prev_tip).clicked() {
                step = Some(false);
            }
            if ui.button("▶").on_hover_text(next_tip).clicked() {
                step = Some(true);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("×").on_hover_text(tr("Hide the level bar (View → Log level bar)")).clicked() {
                    hide = true;
                }
            });
        });
        if let Some(l) = toggle {
            self.filter_levels(on.toggled(l));
        }
        if let Some(forward) = step {
            self.next_level(forward);
        }
        if hide {
            self.settings.level_bar = false;
            self.persist_state();
        }
    }
}
