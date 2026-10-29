//! Open logs in step: Search → Go to this time in all tabs moves the other tabs to the caret
//! line's time (with a ◆ mark), and View → Link scrolling shows two logs side by side, the
//! other one following this one by time (with a skew), line number or position in the file.

use std::time::{Duration, Instant};

use bigtext::jobs::{self, CancelToken};
use bigtext::timestamp;

use super::i18n::{tr, trf};
use super::{BigTextApp, Msg};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LinkMode {
    #[default]
    Time,
    Line,
    Position,
}

impl LinkMode {
    pub const ALL: [LinkMode; 3] = [LinkMode::Time, LinkMode::Line, LinkMode::Position];

    pub fn label(self) -> &'static str {
        match self {
            LinkMode::Time => "By time",
            LinkMode::Line => "By line number",
            LinkMode::Position => "By position in the file",
        }
    }
}

/// This document leads, the tab `partner` follows.
#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    pub partner: u64,
    pub mode: LinkMode,
    /// Added to this log's times before looking them up in the partner.
    pub skew_ms: i64,
    /// The first row the partner was last moved for.
    pub synced: Option<u64>,
}

/// A time looked up in a document.
pub struct Aligned {
    pub doc: u64,
    pub version: u64,
    pub seq: u64,
    pub off: Option<u64>,
    /// For a linked view (scroll it) rather than Go to this time in all tabs (move the caret).
    pub link: bool,
}

/// Two elements of `v` at once.
pub fn pair_mut<T>(v: &mut [T], a: usize, b: usize) -> (&mut T, &mut T) {
    assert_ne!(a, b);
    if a < b {
        let (x, y) = v.split_at_mut(b);
        (&mut x[a], &mut y[0])
    } else {
        let (x, y) = v.split_at_mut(a);
        (&mut y[0], &mut x[b])
    }
}

/// How often the linked log looks up a new time while scrolling.
const LINK_DEBOUNCE: Duration = Duration::from_millis(100);

impl BigTextApp {
    /// Moves every other open text tab to the time of the caret's line.
    pub(crate) fn time_all_tabs(&mut self) {
        let Some(d) = self.docs.get(self.active) else { return };
        let Some(st) = d.line_bounds(d.view.caret, 64 * 1024).and_then(|b| timestamp::stamp_at(&d.buf, b.0)) else {
            self.error = Some(tr("The caret's line has no timestamp.").into());
            return;
        };
        self.align_seq += 1;
        let (seq, me) = (self.align_seq, d.id);
        let mut n = 0;
        for d in self.docs.iter().filter(|d| d.id != me && d.is_ready() && !d.hex && !d.table) {
            let (snap, doc, version) = (d.buf.snapshot(), d.id, d.version);
            let (tx, waker) = (self.tx.clone(), self.waker.clone());
            jobs::spawn("time-all-tabs", move || {
                let off = timestamp::find_time_utc(&snap, st, &CancelToken::new()).ok();
                let _ = tx.send(Msg::Aligned(Box::new(Aligned { doc, version, seq, off, link: false })));
                waker();
            });
            n += 1;
        }
        let time = timestamp::format_key(st.ms, st.has_date, true);
        let text = if n == 0 { tr("No other text tabs are open.").to_string() } else { trf("Moving {n} other tabs to {time}.", &[("n", &n.to_string()), ("time", &time)]) };
        self.notify(text, None);
    }

    pub(super) fn on_aligned(&mut self, a: Aligned) {
        if a.seq != if a.link { self.link_seq } else { self.align_seq } {
            return;
        }
        if a.link {
            self.link_job = None;
        }
        let Some(off) = a.off else { return };
        let Some(d) = self.docs.iter_mut().find(|d| d.id == a.doc && d.version == a.version) else { return };
        let off = off.min(d.buf.len());
        if a.link {
            d.view.top = off;
        } else {
            d.goto_offset(off);
            d.time_mark = Some((d.version, off));
        }
    }

    /// The tab index of the active document's linked partner, when both can be shown.
    pub(super) fn linked_partner(&self) -> Option<usize> {
        let d = self.docs.get(self.active)?;
        let link = d.link.as_ref()?;
        let text = |d: &super::document::Document| d.is_ready() && !d.hex && !d.table;
        if !text(d) || d.split.is_some() || (self.side_by_side && self.diff.as_ref().is_some_and(|r| r.doc == d.id)) {
            return None;
        }
        self.docs.iter().position(|x| x.id == link.partner && x.id != d.id && text(x))
    }

    /// Links the active document with the tab `partner`.
    pub(crate) fn link_with(&mut self, partner: u64, mode: LinkMode) {
        let skew_ms = self.docs.get(self.active).and_then(|d| d.link.as_ref()).map(|l| l.skew_ms).unwrap_or(0);
        if let Some(d) = self.docs.get_mut(self.active).filter(|d| d.id != partner) {
            d.link = Some(Link { partner, mode, skew_ms, synced: None });
            d.split = None;
        }
    }

    pub(crate) fn unlink(&mut self) {
        if let Some(d) = self.docs.get_mut(self.active) {
            d.link = None;
        }
        if let Some((c, _)) = self.link_job.take() {
            c.cancel();
        }
    }

    /// Moves the linked partner to where the active document now is.
    pub(super) fn sync_link(&mut self, ctx: &egui::Context) {
        let Some(pi) = self.linked_partner() else { return };
        let a = &self.docs[self.active];
        let Some(link) = a.link.clone() else { return };
        let top = a.view.top;
        if link.synced == Some(top) {
            return;
        }
        match link.mode {
            LinkMode::Line => {
                let (line, _) = a.buf.line_of(top);
                let b = &mut self.docs[pi];
                let last = b.buf.line_count().0.saturating_sub(1);
                if let Some((ls, _)) = b.buf.line_start(line.min(last)) {
                    b.view.top = ls;
                }
            }
            LinkMode::Position => {
                let f = top as f64 / a.buf.len().max(1) as f64;
                let b = &mut self.docs[pi];
                let off = ((b.buf.len() as f64 * f) as u64).min(b.buf.len());
                let wrap = crate::view::text_view::wrap_cols(&b.view);
                b.view.top = bigtext::text::rows::row_at(&b.buf, off, wrap).map(|r| r.start).unwrap_or(off);
            }
            LinkMode::Time => {
                // At most one lookup per 100 ms while scrolling, the latest one winning.
                if self.link_job.as_ref().is_some_and(|(_, t)| t.elapsed() < LINK_DEBOUNCE) {
                    ctx.request_repaint_after(LINK_DEBOUNCE);
                    return;
                }
                if let Some(st) = a.line_bounds(top, 64 * 1024).and_then(|b| timestamp::stamp_at(&a.buf, b.0)) {
                    let st = timestamp::Stamp { ms: st.ms + link.skew_ms, ..st };
                    if let Some((c, _)) = self.link_job.take() {
                        c.cancel();
                    }
                    self.link_seq += 1;
                    let b = &self.docs[pi];
                    let (snap, doc, version, seq) = (b.buf.snapshot(), b.id, b.version, self.link_seq);
                    let (tx, waker, cancel) = (self.tx.clone(), self.waker.clone(), CancelToken::new());
                    self.link_job = Some((cancel.clone(), Instant::now()));
                    jobs::spawn("link-time", move || {
                        let off = timestamp::find_time_utc(&snap, st, &cancel).ok();
                        if !cancel.is_cancelled() {
                            let _ = tx.send(Msg::Aligned(Box::new(Aligned { doc, version, seq, off, link: true })));
                            waker();
                        }
                    });
                }
            }
        }
        if let Some(l) = self.docs[self.active].link.as_mut() {
            l.synced = Some(top);
        }
    }

    /// View → Link scrolling with ▸ (the mode, then the tab).
    pub(super) fn link_menu(&mut self, ui: &mut egui::Ui) {
        let Some(d) = self.docs.get(self.active) else { return };
        let (me, linked) = (d.id, d.link.clone());
        let tabs: Vec<(u64, String)> = self.docs.iter().filter(|x| x.id != me && x.is_ready() && !x.hex && !x.table).map(|x| (x.id, x.title.clone())).collect();
        ui.add_enabled_ui(!tabs.is_empty() || linked.is_some(), |ui| {
            ui.menu_button(tr("Link scrolling with"), |ui| {
                for m in LinkMode::ALL {
                    if ui.radio(self.link_mode == m, tr(m.label())).clicked() {
                        self.link_mode = m;
                        if let Some(l) = &linked {
                            self.link_with(l.partner, m);
                        }
                    }
                }
                ui.separator();
                for (id, title) in tabs {
                    if ui.radio(linked.as_ref().is_some_and(|l| l.partner == id), title).clicked() {
                        self.link_with(id, self.link_mode);
                        ui.close();
                    }
                }
                if linked.is_some() {
                    ui.separator();
                    if ui.button(tr("Unlink")).clicked() {
                        self.unlink();
                        ui.close();
                    }
                }
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairs_in_either_order() {
        let mut v = vec![1, 2, 3, 4];
        let (a, b) = pair_mut(&mut v, 3, 1);
        std::mem::swap(a, b);
        assert_eq!(v, [1, 4, 3, 2]);
        let (a, b) = pair_mut(&mut v, 0, 2);
        *a += *b;
        assert_eq!(v, [4, 4, 3, 2]);
    }
}
