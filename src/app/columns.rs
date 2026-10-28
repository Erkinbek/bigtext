//! Deleting CSV columns chosen in the table view. Documents up to `IN_MEMORY_MAX` get the result
//! built on a worker and applied as one undoable edit; bigger files are rewritten on disk like a
//! large Replace All (temp file, atomic swap, optional backup).

use bigtext::csv::{self, Dialect};
use bigtext::jobs::{self, human_bytes, CancelToken, Progress};
use bigtext::text::Piece;

use super::document::Banner;
use super::i18n::{tr, trf};
use super::ops::{SaveJob, WriteSpec};
use super::{BigTextApp, Confirm, ConfirmAction, Msg};

/// Larger documents are rewritten on disk (no undo) instead of in memory.
pub const IN_MEMORY_MAX: u64 = 64 * 1024 * 1024;

pub struct ColumnsDeleted {
    pub doc: u64,
    pub version: u64,
    pub add_len: u64,
    pub pieces: Vec<Piece>,
    pub add: Vec<u8>,
    pub cols: Vec<usize>,
    pub records: u64,
}

/// "1 column" / "3 columns".
pub fn count_label(n: usize) -> String {
    if n == 1 {
        tr("1 column").into()
    } else {
        trf("{n} columns", &[("n", &n.to_string())])
    }
}

/// `“a”, “b” and 3 more`.
fn name_list(names: &[String]) -> String {
    const SHOWN: usize = 6;
    let mut s = names.iter().take(SHOWN).map(|n| format!("“{n}”")).collect::<Vec<_>>().join(", ");
    if names.len() > SHOWN {
        s.push_str(&trf(" and {n} more", &[("n", &(names.len() - SHOWN).to_string())]));
    }
    s
}

impl BigTextApp {
    /// Asks before deleting `cols` (sorted) of the active document.
    pub(super) fn ask_delete_columns(&mut self, cols: Vec<usize>, names: Vec<String>) {
        let Some(d) = self.docs.get(self.active) else { return };
        let Some(dialect) = d.table_state.dialect else { return };
        if cols.is_empty() || !d.editable() {
            return;
        }
        let on_disk = d.buf.len() > IN_MEMORY_MAX;
        if on_disk && d.path.is_none() {
            self.error = Some(tr("Save the document to a file first; columns of large documents are deleted by rewriting the file.").into());
            return;
        }
        let what = trf(if cols.len() == 1 { "Delete the column {names} and its values from every row?" } else { "Delete the columns {names} and their values from every row?" }, &[("names", &name_list(&names))]);
        let message = if on_disk {
            format!(
                "{what}\n\n{}{}",
                trf(
                    "“{file}” is {size}, so the result is written to a new file that atomically replaces it, as Replace All does on large files. Undo will not be available.",
                    &[("file", &d.title), ("size", &human_bytes(d.buf.len()))]
                ),
                if d.is_modified() { tr(" Your unsaved edits are included.") } else { "" }
            )
        } else {
            format!("{what}\n\n{}", tr("You can undo it with Ctrl+Z."))
        };
        self.confirm = Some(Confirm {
            title: tr(if cols.len() == 1 { "Delete column" } else { "Delete columns" }).into(),
            message,
            action: ConfirmAction::DeleteColumns { doc: d.id, dialect, cols, on_disk },
            checkbox: on_disk.then(|| (tr("Keep a backup (.bak)").into(), true)),
        });
    }

    pub(super) fn delete_columns(&mut self, doc: u64, dialect: Dialect, cols: Vec<usize>, on_disk: bool, backup: bool) {
        let Some(d) = self.docs.iter_mut().find(|d| d.id == doc) else { return };
        if !d.editable() {
            return;
        }
        if on_disk {
            let Some(path) = d.path.clone() else { return };
            self.start_write(doc, path, WriteSpec::DeleteColumns { dialect, cols }, backup);
            return;
        }
        let cancel = CancelToken::new();
        let progress = Progress::new(d.buf.len());
        d.save_job = Some(SaveJob { cancel: cancel.clone(), progress: progress.clone(), label: tr("Deleting columns").into() });
        let snap = d.buf.snapshot();
        let version = d.version;
        let (tx, waker) = (self.tx.clone(), self.waker.clone());
        jobs::spawn("delete-columns", move || {
            let mut out = Vec::with_capacity(snap.len() as usize);
            let msg = match csv::delete_columns_to(&snap, dialect, &cols, &mut out, &cancel, Some(&progress)) {
                Ok(Some(stats)) => {
                    // The whole document becomes one new piece: a single undo step.
                    let (pieces, add) = snap.build_batch(&[(0..snap.len(), out)]);
                    Msg::ColumnsDeleted(Box::new(ColumnsDeleted { doc, version, add_len: snap.add_len(), pieces, add, cols, records: stats.count }))
                }
                Ok(None) => Msg::JobFailed { doc, msg: None },
                Err(e) => Msg::JobFailed { doc, msg: Some(trf("Deleting columns failed: {e}", &[("e", &e.to_string())])) },
            };
            let _ = tx.send(msg);
            waker();
        });
    }

    pub(super) fn on_columns_deleted(&mut self, c: ColumnsDeleted) {
        let Some(d) = self.docs.iter_mut().find(|d| d.id == c.doc) else { return };
        d.save_job = None;
        if d.version != c.version || d.buf.add_len() != c.add_len {
            d.banner = Some(Banner::Info(tr("The document changed while the columns were being deleted; nothing was deleted.").into()));
            return;
        }
        // Rows get shorter, so keep the caret and the first visible row on their lines.
        let (top_line, _) = d.buf.line_of(d.view.top);
        let (caret_line, _) = d.buf.line_of(d.view.caret);
        let swap = d.buf.apply_batch(c.pieces, &c.add);
        let line_start = |line: u64| d.buf.line_start(line).map(|(o, _)| o).unwrap_or(0).max(d.min_caret());
        let (caret, top) = (line_start(caret_line), line_start(top_line));
        d.record(swap, (caret, caret));
        d.view.top = top;
        d.table_state.columns_deleted(&c.cols);
        d.banner = Some(Banner::Info(trf("Deleted {cols} from all {n} lines. Undo with Ctrl+Z.", &[("cols", &count_label(c.cols.len())), ("n", &c.records.to_string())])));
    }
}
