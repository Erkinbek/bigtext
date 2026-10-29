//! View → Fold: the object, array or element at the caret folds into one row with a "… N lines …"
//! chip; Fold all at level 1 or 2 folds a JSON document's values at that depth. The ranges are
//! found on a worker, so folding the root of a huge array does not block.

use bigtext::brackets;
use bigtext::jobs::{self, CancelToken};
use bigtext::json;

use super::i18n::{tr, trf};
use super::search_panel::FindJob;
use super::{BigTextApp, Msg};
use crate::view::folds::{self, Fold};

/// Folds kept at most (fold all on a huge array).
const FOLDS_MAX: usize = 1_000_000;

pub struct FoldMsg {
    pub doc: u64,
    pub version: u64,
    pub folds: Vec<Fold>,
    /// Fold all: the folds replace the ones there are.
    pub all: bool,
    pub capped: bool,
}

/// Brackets and tags are looked for this far (all of the file).
const ALL: u64 = u64::MAX;

/// The fold of the bracket or tag at `caret`, or of the brackets around it (going out until
/// they span lines).
fn fold_at(buf: &bigtext::text::TextBuf, caret: u64, cancel: &CancelToken) -> Option<Fold> {
    if let Some(p) = brackets::at_caret(buf, caret) {
        if let Some(f) = brackets::partner(buf, p, ALL, cancel).and_then(|q| folds::fold_between(buf, p.min(q), p.max(q))) {
            return Some(f);
        }
    }
    if let Some(t) = brackets::tag_at(buf, caret) {
        if let Some(f) = brackets::tag_partner(buf, &t, ALL, cancel).and_then(|u| folds::fold_between(buf, t.start.min(u.start), t.start.max(u.start))) {
            return Some(f);
        }
    }
    let mut from = caret;
    for _ in 0..256 {
        let o = brackets::enclosing(buf, from, ALL, cancel)?;
        let c = brackets::partner(buf, o, ALL, cancel)?;
        if let Some(f) = folds::fold_between(buf, o, c) {
            return Some(f);
        }
        from = o;
    }
    None
}

/// The objects and arrays `level` deep in a JSON document, as folds (at most FOLDS_MAX).
fn fold_level(buf: &bigtext::text::TextBuf, level: usize, cancel: &CancelToken) -> Result<(Vec<Fold>, bool), String> {
    let mut parents = vec![json::root(buf, cancel)?];
    let mut capped = false;
    let mut ranges = Vec::new();
    for depth in 1..=level {
        ranges.clear();
        'parents: for &p in &parents {
            let mut resume = None;
            loop {
                let page = json::children(buf, p, resume, 10_000, cancel)?;
                for c in page.items.iter().filter(|c| matches!(c.kind, json::Kind::Object | json::Kind::Array)) {
                    if ranges.len() >= FOLDS_MAX {
                        capped = true;
                        break 'parents;
                    }
                    ranges.push((c.start, c.end));
                }
                match page.resume {
                    Some(r) => resume = Some(r),
                    None => break,
                }
            }
        }
        if depth < level {
            parents = ranges.iter().map(|r| r.0).collect();
        }
    }
    let folds = ranges.iter().filter_map(|&(s, e)| folds::fold_between(buf, s, e.saturating_sub(1))).collect();
    Ok((folds, capped))
}

impl BigTextApp {
    /// Folding needs single-byte text shown as text.
    pub(crate) fn foldable(&self) -> bool {
        self.docs.get(self.active).is_some_and(|d| d.is_ready() && !d.hex && !d.table && d.buf.codec.unit() == 1)
    }

    pub(crate) fn fold_at_caret(&mut self) {
        if !self.foldable() {
            return;
        }
        let Some(d) = self.docs.get_mut(self.active) else { return };
        if let Some(j) = d.find_job.take() {
            j.cancel.cancel();
        }
        let cancel = CancelToken::new();
        d.find_job = Some(FindJob { cancel: cancel.clone() });
        let (snap, caret, doc, version) = (d.buf.snapshot(), d.view.caret, d.id, d.version);
        let (tx, waker) = (self.tx.clone(), self.waker.clone());
        jobs::spawn("fold", move || {
            let fold = fold_at(&snap, caret, &cancel);
            if !cancel.is_cancelled() {
                let _ = tx.send(Msg::Folds(Box::new(FoldMsg { doc, version, folds: fold.into_iter().collect(), all: false, capped: false })));
                waker();
            }
        });
    }

    pub(crate) fn fold_all(&mut self, level: usize) {
        if !self.foldable() {
            return;
        }
        let Some(d) = self.docs.get_mut(self.active) else { return };
        if let Some(j) = d.find_job.take() {
            j.cancel.cancel();
        }
        let cancel = CancelToken::new();
        d.find_job = Some(FindJob { cancel: cancel.clone() });
        let (snap, doc, version) = (d.buf.snapshot(), d.id, d.version);
        let (tx, waker) = (self.tx.clone(), self.waker.clone());
        jobs::spawn("fold-all", move || {
            let result = fold_level(&snap, level, &cancel);
            if !cancel.is_cancelled() {
                let (folds, capped) = result.unwrap_or_default();
                let _ = tx.send(Msg::Folds(Box::new(FoldMsg { doc, version, folds, all: true, capped })));
                waker();
            }
        });
    }

    /// Unfolds the fold whose opening line holds the caret.
    pub(crate) fn unfold_at_caret(&mut self) {
        let Some(d) = self.docs.get_mut(self.active) else { return };
        let caret = d.view.caret;
        d.view.folds.retain(|f| !(f.start <= caret && caret < f.hide_from));
    }

    pub(crate) fn unfold_all(&mut self) {
        if let Some(d) = self.docs.get_mut(self.active) {
            d.view.folds.clear();
        }
    }

    pub(super) fn on_folds(&mut self, m: FoldMsg) {
        let Some(d) = self.docs.iter_mut().find(|d| d.id == m.doc) else { return };
        d.find_job = None;
        if d.version != m.version {
            return;
        }
        let empty = m.folds.is_empty();
        if m.all {
            d.view.folds = m.folds;
        } else {
            for f in m.folds {
                folds::insert(&mut d.view.folds, f);
            }
        }
        d.view.folds_version = d.version;
        // A caret in a folded value would open it again: it goes to the value's first line.
        if let Some(f) = folds::hidden_by(&d.view.folds, d.view.caret).copied().filter(|_| !m.all) {
            d.set_caret(f.start, false);
        }
        if empty {
            self.notify(tr(if m.all { "No objects or arrays at that level." } else { "Nothing to fold here." }).into(), None);
        } else if m.capped {
            self.notify(trf("Folded the first {n} values.", &[("n", &FOLDS_MAX.to_string())]), None);
        }
    }
}
