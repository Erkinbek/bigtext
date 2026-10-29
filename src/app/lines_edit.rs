//! Filter panel → Delete these lines / Keep only these lines: the open document changes in
//! place, as one undoable edit up to [`UNDO_THRESHOLD`] runs of lines, beyond that rewritten on
//! disk (temp file, atomic swap, optional .bak) like Replace All.

use std::ops::Range;
use std::sync::Arc;

use bigtext::filter::{self, FilterSpec};
use bigtext::jobs::{self, CancelToken};
use bigtext::jsonq::FieldQuery;
use bigtext::search::Matcher;
use bigtext::text::Piece;

use super::filter_panel::{fmt_count, line_meets, term_refs, FilterTerm};
use super::i18n::{tr, trf};
use super::ops::{WriteSpec, UNDO_THRESHOLD};
use super::{BigTextApp, Confirm, ConfirmAction, Msg};

/// The lines a filter shows, as a test of one line's bytes.
pub struct LineSel {
    pub matcher: Arc<Matcher>,
    pub json: Option<Arc<FieldQuery>>,
    pub invert: bool,
    pub terms: Vec<FilterTerm>,
}

impl LineSel {
    pub fn shows(&self, line: &[u8]) -> bool {
        let base = match &self.json {
            Some(q) => q.matches(line),
            None => {
                let mut found = false;
                self.matcher.find_in(line, 0, 0, line.len(), line.len(), &mut None, &mut |_, _| {
                    found = true;
                    false
                });
                found
            }
        };
        base != self.invert && line_meets(&term_refs(&self.terms), line)
    }
}

/// The edit that removes the lines, made on a worker.
pub struct LinesRemoved {
    pub doc: u64,
    pub version: u64,
    pub add_len: u64,
    pub pieces: Vec<Piece>,
    pub add: Vec<u8>,
    pub lines: u64,
}

impl BigTextApp {
    /// Why the filter's lines cannot be removed in place, if they cannot.
    pub(crate) fn lines_edit_blocked(&self) -> Option<&'static str> {
        let d = self.docs.get(self.active)?;
        let r = d.filter.as_ref()?;
        if !d.editable() {
            Some(tr("The document is read-only"))
        } else if r.version != d.version {
            Some(tr("The document changed since the filter ran. Run the filter again first."))
        } else if r.spec.has_context() || r.spec.column.is_some() || r.spec.range.is_some() {
            Some(tr("Turn off the lines around matches, the column and the time range first."))
        } else {
            None
        }
    }

    /// Deletes the lines the filter shows (or, with `keep`, all the others).
    pub(crate) fn remove_filtered_lines(&mut self, keep: bool) {
        if let Some(why) = self.lines_edit_blocked() {
            self.notify(why.into(), None);
            return;
        }
        let Some(d) = self.docs.get(self.active) else { return };
        let Some(r) = &d.filter else { return };
        let sel = Arc::new(LineSel { matcher: r.matcher.clone(), json: r.json.clone(), invert: r.spec.invert, terms: r.terms.clone() });
        let (snap, doc, version, add_len) = (d.buf.snapshot(), d.id, d.version, d.buf.add_len());
        let (tx, waker) = (self.tx.clone(), self.waker.clone());
        self.search.status = tr("Finding the lines…").into();
        jobs::spawn("collect-lines", move || {
            let bom = snap.codec.bom_len as u64;
            let mut runs: Vec<Range<u64>> = Vec::new();
            let mut lines = 0u64;
            let mut too_many = false;
            let gone = |l: &[u8]| sel.shows(l) != keep;
            filter::filter_predicate(&snap, FilterSpec::default(), &gone, &CancelToken::new(), None, |ls, le, _| {
                lines += 1;
                let ls = ls.max(bom);
                match runs.last_mut() {
                    // Next to the last run: one longer edit.
                    Some(last) if last.end == ls => last.end = le,
                    _ => runs.push(ls..le),
                }
                too_many = runs.len() > UNDO_THRESHOLD;
                !too_many
            });
            let msg = if too_many {
                Msg::LinesTooMany { doc, version, sel, keep }
            } else {
                let edits: Vec<(Range<u64>, Vec<u8>)> = runs.into_iter().map(|r| (r, Vec::new())).collect();
                let (pieces, add) = snap.build_batch(&edits);
                Msg::LinesRemoved(Box::new(LinesRemoved { doc, version, add_len, pieces, add, lines }))
            };
            let _ = tx.send(msg);
            waker();
        });
    }

    pub(super) fn on_lines_removed(&mut self, l: LinesRemoved) {
        let Some(d) = self.docs.iter_mut().find(|d| d.id == l.doc) else { return };
        if d.version != l.version || d.buf.add_len() != l.add_len {
            self.search.status = tr("The document changed meanwhile; no line was deleted.").into();
            return;
        }
        if l.lines == 0 {
            self.search.status = tr("No line to delete.").into();
            return;
        }
        let (s, _) = d.selection();
        let swap = d.buf.apply_batch(l.pieces, &l.add);
        d.record(swap, (s, s));
        let len = d.buf.len();
        d.set_caret(s.min(len), false);
        // The filter's lines are gone (or all that is left).
        if let Some(r) = d.filter.take() {
            r.cancel.cancel();
        }
        self.search.status = trf("Deleted {n} lines (undo with Ctrl+Z).", &[("n", &fmt_count(l.lines))]);
    }

    pub(super) fn on_lines_too_many(&mut self, doc: u64, version: u64, sel: Arc<LineSel>, keep: bool) {
        let Some(d) = self.docs.iter().find(|d| d.id == doc) else { return };
        if d.version != version {
            return;
        }
        if d.path.is_none() {
            self.error = Some(trf("More than {n} separate runs of lines. Save the document to a file first; then they are deleted on disk.", &[("n", &fmt_count(UNDO_THRESHOLD as u64))]));
            return;
        }
        let message = format!(
            "{}{}",
            trf(
                "More than {n} separate runs of lines. They will be left out of a new file that atomically replaces “{file}”; undo will not be available.",
                &[("n", &fmt_count(UNDO_THRESHOLD as u64)), ("file", &d.title)]
            ),
            if d.is_modified() { tr(" Your unsaved edits are included.") } else { "" }
        );
        self.confirm = Some(Confirm {
            title: tr("Delete lines on disk").into(),
            message,
            action: ConfirmAction::LinesOnDisk { doc, sel, keep },
            checkbox: Some((tr("Keep a backup (.bak)").into(), true)),
        });
    }

    pub(super) fn lines_on_disk(&mut self, doc: u64, sel: Arc<LineSel>, keep: bool, backup: bool) {
        let Some(path) = self.docs.iter().find(|d| d.id == doc).and_then(|d| d.path.clone()) else { return };
        if let Some(d) = self.docs.iter_mut().find(|d| d.id == doc) {
            if let Some(r) = d.filter.take() {
                r.cancel.cancel();
            }
        }
        self.start_write(doc, path, WriteSpec::Lines { sel, keep }, backup);
    }
}
