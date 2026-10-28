//! Edit → Add next occurrence (Ctrl+D): selects the word at the caret, then each press makes
//! the next occurrence of the selection another cursor. The search runs on a worker, so a
//! distant next occurrence in a large file never blocks the window.

use bigtext::jobs::{self, CancelToken};
use bigtext::search::{self, Matcher, Query};

use super::i18n::tr;
use super::search_panel::FindJob;
use super::{BigTextApp, Msg};
use crate::view::multi;

pub struct CursorFound {
    pub doc: u64,
    pub version: u64,
    pub found: Option<(u64, u64)>,
}

/// Selections longer than this are not searched for.
const MAX_SELECTION: u64 = 4096;

impl BigTextApp {
    pub(crate) fn add_next_occurrence(&mut self) {
        let Some(d) = self.docs.get_mut(self.active) else { return };
        if !d.is_ready() || d.hex || d.table {
            return;
        }
        let wrap = d.view.wrap.then_some(d.view.text_cols.saturating_sub(1).max(1));
        if !d.has_selection() {
            let caret = d.view.caret;
            crate::view::text_view::select_word(d, caret, wrap);
            return;
        }
        let (s, e) = d.selection();
        if e - s > MAX_SELECTION {
            return;
        }
        let Some(text) = d.buf.read(s..e).map(|b| d.buf.codec.decode_lossy(&b)) else { return };
        let Ok(m) = Matcher::new(&Query { pattern: text, regex: false, case_sensitive: true, whole_word: false, dot_all: false }, d.buf.codec) else { return };
        let taken: Vec<(u64, u64)> = multi::selections(d).into_iter().map(|(s, e, _)| (s, e)).collect();
        let from = taken.iter().map(|x| x.1).max().unwrap_or(e);
        if let Some(j) = d.find_job.take() {
            j.cancel.cancel();
        }
        let cancel = CancelToken::new();
        d.find_job = Some(FindJob { cancel: cancel.clone() });
        let (snap, id, version) = (d.buf.snapshot(), d.id, d.version);
        let (tx, waker) = (self.tx.clone(), self.waker.clone());
        jobs::spawn("next-occurrence", move || {
            let len = snap.len();
            let first = |range: std::ops::Range<u64>| {
                let mut f = None;
                search::search_forward(&snap, &m, range, &cancel, None, |a, b| {
                    if taken.contains(&(a, b)) {
                        return true;
                    }
                    f = Some((a, b));
                    false
                });
                f
            };
            let found = first(from..len).or_else(|| first(0..(from + m.overlap()).min(len)));
            if !cancel.is_cancelled() {
                let _ = tx.send(Msg::CursorFound(Box::new(CursorFound { doc: id, version, found })));
                waker();
            }
        });
    }

    pub(super) fn on_cursor_found(&mut self, f: CursorFound) {
        let Some(d) = self.docs.iter_mut().find(|d| d.id == f.doc) else { return };
        d.find_job = None;
        if d.version != f.version {
            return;
        }
        match f.found {
            Some((s, e)) => {
                multi::add_cursor(d, s, e);
                d.view.center_caret = true;
            }
            None => self.notify(tr("Every occurrence already has a cursor").into(), None),
        }
    }
}
