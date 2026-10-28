//! Matching brackets and tags: the pair at the caret is highlighted, and Search → Go to matching
//! bracket (Ctrl+]) jumps to the partner — or, away from a bracket, to the bracket that encloses
//! the caret. The jump searches the whole file on a worker; the highlight looks at most 1 MB away.

use bigtext::brackets::{self, Pair};
use bigtext::jobs::{self, CancelToken};

use super::i18n::tr;
use super::search_panel::FindJob;
use super::{BigTextApp, Msg};

/// The highlight looks this far for a partner (on the UI thread).
const HIGHLIGHT_LIMIT: u64 = 1 << 20;

pub struct BracketJump {
    pub doc: u64,
    pub version: u64,
    pub to: Option<u64>,
}

/// The pair at the caret, by (document, version, caret).
#[derive(Default)]
pub struct PairCache(Option<((u64, u64, u64), Option<Pair>)>);

impl BigTextApp {
    /// The bracket or tag pair at the active document's caret (cached).
    pub(crate) fn caret_pair(&mut self) -> Option<Pair> {
        let d = self.docs.get(self.active)?;
        if !d.is_ready() || d.hex || d.has_selection() {
            return None;
        }
        let key = (d.id, d.version, d.view.caret);
        if let Some((k, p)) = &self.pairs.0 {
            if *k == key {
                return *p;
            }
        }
        let p = brackets::pair_at(&d.buf, d.view.caret, HIGHLIGHT_LIMIT, &CancelToken::new());
        self.pairs.0 = Some((key, p));
        p
    }

    pub(crate) fn goto_matching_bracket(&mut self) {
        let Some(d) = self.docs.get_mut(self.active) else { return };
        if !d.is_ready() || d.hex {
            return;
        }
        if let Some(j) = d.find_job.take() {
            j.cancel.cancel();
        }
        let cancel = CancelToken::new();
        d.find_job = Some(FindJob { cancel: cancel.clone() });
        let (snap, caret, id, version) = (d.buf.snapshot(), d.view.caret, d.id, d.version);
        let (tx, waker) = (self.tx.clone(), self.waker.clone());
        jobs::spawn("match-bracket", move || {
            let all = u64::MAX;
            let to = if let Some(p) = brackets::at_caret(&snap, caret) {
                brackets::partner(&snap, p, all, &cancel)
            } else if let Some(t) = brackets::tag_at(&snap, caret) {
                brackets::tag_partner(&snap, &t, all, &cancel).map(|u| u.start + 1)
            } else {
                brackets::enclosing(&snap, caret, all, &cancel)
            };
            if !cancel.is_cancelled() {
                let _ = tx.send(Msg::BracketJump(Box::new(BracketJump { doc: id, version, to })));
                waker();
            }
        });
    }

    pub(super) fn on_bracket_jump(&mut self, j: BracketJump) {
        let Some(d) = self.docs.iter_mut().find(|d| d.id == j.doc) else { return };
        d.find_job = None;
        if d.version != j.version {
            return;
        }
        match j.to {
            Some(off) => {
                d.set_caret(off, false);
                d.view.scroll_to_caret = true;
            }
            None => self.notify(tr("No matching bracket").into(), None),
        }
    }
}
