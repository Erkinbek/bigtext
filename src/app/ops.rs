//! Save / Save As, streaming replace-all and the finalize protocol that swaps files on disk.
//!
//! Protocol: (1) a worker streams a snapshot to a temp file and fsyncs it; (2) the UI drops its
//! `Arc<Source>` and cancels jobs holding snapshots; (3) a finalize worker waits until the old
//! source is gone, atomically replaces the target, and reopens it. If anything fails the
//! original is reopened unchanged and the user's edits are re-attached to it.

use std::path::PathBuf;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use bigtext::csv::Dialect;
use bigtext::encoding::{Encoding, LineEnding};
use bigtext::file::{self, Source};
use bigtext::index::LineIndex;
use bigtext::jobs::{self, human_bytes, CancelToken, Progress};
use bigtext::replace::{self, Collected, ReplaceStats, Template};
use bigtext::save::{self, SaveError, Transform};
use bigtext::search::Matcher;
use bigtext::text::Piece;

use super::document::{Banner, DocState};
use super::i18n::{tr, trf};
use super::{open_worker, same_path, BigTextApp, Confirm, ConfirmAction, Msg, Opened};

/// Replacements up to this count are applied as undoable piece-table edits.
pub const UNDO_THRESHOLD: usize = 50_000;

pub struct SaveJob {
    pub cancel: CancelToken,
    pub progress: Progress,
    pub label: String,
}

#[derive(Clone)]
pub enum WriteSpec {
    Identity,
    Eol(LineEnding),
    Replace { matcher: Arc<Matcher>, template: Template, range: std::ops::Range<u64> },
    DeleteColumns { dialect: Dialect, cols: Vec<usize> },
    Transcode { enc: Encoding, bom: bool, eol: Option<LineEnding> },
    Csv { dialect: Dialect, rewrite: bigtext::csv::Rewrite },
    /// Delete the lines a filter shows (or, with `keep`, all the others).
    Lines { sel: Arc<super::lines_edit::LineSel>, keep: bool },
    /// A text operation over a range too large to change in memory.
    TextOp { op: bigtext::textops::Op, range: std::ops::Range<u64> },
    /// A column filled from a line to the end of the file.
    ColumnEdit { from: u64, cells: (u32, u32), gen: bigtext::column_edit::ColumnGen },
    /// The lines sorted on disk.
    Sort { spec: bigtext::extsort::SortSpec, eol: Vec<u8> },
}

/// What a write to disk does, for its progress label and the message when it is done.
#[derive(Clone, PartialEq)]
pub enum WriteKind {
    Save,
    Replace,
    /// These columns (sorted) were deleted.
    DeleteColumns(Vec<usize>),
    /// Saved in another encoding.
    Transcode(Encoding),
    /// A CSV table was rewritten (sorted, columns moved).
    Csv(bigtext::csv::Rewrite),
    /// Lines were deleted.
    Lines,
    TextOp(bigtext::textops::Op),
    ColumnEdit,
    Sort,
}

pub struct TempWritten {
    pub doc: u64,
    pub temp: PathBuf,
    pub target: PathBuf,
    pub backup: bool,
    pub prefix: Option<Vec<u64>>,
    pub stats: ReplaceStats,
    pub kind: WriteKind,
    pub elapsed: Duration,
}

pub struct Finalized {
    pub doc: u64,
    pub prefix: Option<Vec<u64>>,
    pub result: Result<Opened, (String, Result<Opened, String>)>,
    pub stats: ReplaceStats,
    pub kind: WriteKind,
    pub elapsed: Duration,
}

impl BigTextApp {
    pub(crate) fn save(&mut self, save_as: bool) {
        self.save_with(save_as, None);
    }

    /// Save, or Save As when `save_as`; with `encoding` (and whether to write a byte order
    /// mark) the document is re-encoded, always through the Save As dialog.
    pub(crate) fn save_with(&mut self, save_as: bool, encoding: Option<(Encoding, bool)>) {
        let Some(d) = self.docs.get(self.active) else { return };
        if !d.is_ready() || d.save_job.is_some() {
            return;
        }
        let save_as = save_as || encoding.is_some();
        let target = match (&d.path, save_as) {
            // An unpacked copy lives in the temp folder: Save means Save As.
            (Some(p), false) if !d.temporary => p.clone(),
            _ => {
                let name = d.unpacked_from.as_deref().map(bigtext::decompress::inner_name).unwrap_or_else(|| d.title.clone());
                let mut dlg = rfd::FileDialog::new().set_title(super::i18n::tr("Save As")).set_file_name(name);
                if let Some(dir) = d.unpacked_from.as_ref().or(d.path.as_ref()).and_then(|p| p.parent()) {
                    dlg = dlg.set_directory(file::display_path(dir));
                }
                match dlg.save_file() {
                    Some(p) => p,
                    None => return,
                }
            }
        };
        let spec = match (encoding, d.eol_on_save) {
            (Some((enc, bom)), eol) => WriteSpec::Transcode { enc, bom, eol },
            (None, Some(le)) => WriteSpec::Eol(le),
            (None, None) => WriteSpec::Identity,
        };
        let id = d.id;
        let backup = self.settings.backup_on_save;
        self.start_write(id, target, spec, backup);
    }

    pub(crate) fn start_write(&mut self, id: u64, target: PathBuf, spec: WriteSpec, backup: bool) {
        self.start_write_checked(id, target, spec, backup, true);
    }

    /// [`Self::start_write`]; with `check`, a document that would not fit on the target's drive
    /// asks first.
    pub(crate) fn start_write_checked(&mut self, id: u64, target: PathBuf, spec: WriteSpec, backup: bool, check: bool) {
        // A comparison holding the file (or its backup) open would hold up replacing it.
        let holds = self.diff.as_ref().is_some_and(|r| {
            let running = !r.data.lock().unwrap().done;
            (r.doc == id && (running || !matches!(r.other, super::extras::DiffOther::File(_))))
                || r.other.path().is_some_and(|p| same_path(p, &target))
        });
        if holds {
            if let Some(r) = self.diff.take() {
                r.cancel.cancel();
            }
        }
        let Some(d) = self.docs.iter_mut().find(|d| d.id == id) else { return };
        let same_file = d.path.as_deref().is_some_and(|p| same_path(p, &target));
        let prefix = match spec {
            WriteSpec::Identity if same_file => Some(d.buf.index.prefix_checkpoints(d.buf.pristine_prefix())),
            _ => None,
        };
        let kind = match &spec {
            WriteSpec::Replace { .. } => WriteKind::Replace,
            WriteSpec::DeleteColumns { cols, .. } => WriteKind::DeleteColumns(cols.clone()),
            WriteSpec::Transcode { enc, .. } => WriteKind::Transcode(*enc),
            WriteSpec::Csv { rewrite, .. } => WriteKind::Csv(rewrite.clone()),
            WriteSpec::Lines { .. } => WriteKind::Lines,
            WriteSpec::TextOp { op, .. } => WriteKind::TextOp(*op),
            WriteSpec::ColumnEdit { .. } => WriteKind::ColumnEdit,
            WriteSpec::Sort { .. } => WriteKind::Sort,
            WriteSpec::Identity | WriteSpec::Eol(_) => WriteKind::Save,
        };
        let cancel = CancelToken::new();
        let progress = Progress::new(d.buf.len());
        let label = match kind {
            WriteKind::Save | WriteKind::Transcode(_) => "Saving",
            WriteKind::Replace => "Replacing",
            WriteKind::DeleteColumns(_) => "Deleting columns",
            WriteKind::Csv(_) => "Rewriting",
            WriteKind::Lines => "Deleting lines",
            WriteKind::TextOp(op) => op.name(),
            WriteKind::ColumnEdit => "Column editor",
            WriteKind::Sort => "Sorting",
        };
        d.save_job = Some(SaveJob { cancel: cancel.clone(), progress: progress.clone(), label: super::i18n::tr(label).into() });
        let snap = d.buf.snapshot();
        let tx = self.tx.clone();
        let waker = self.waker.clone();
        jobs::spawn("save", move || {
            let t = Instant::now();
            if check {
                // About the document's size (twice that when it becomes UTF-16).
                let need = match &spec {
                    WriteSpec::Transcode { enc, .. } if enc.unit() == 2 => snap.len().saturating_mul(2),
                    _ => snap.len(),
                };
                let dir = target.parent().map(std::path::Path::to_path_buf).unwrap_or_default();
                if let Some(free) = bigtext::sysinfo::free_space(&dir) {
                    if need > free {
                        let resume = super::Resume::Write { doc: id, target, spec, backup };
                        let _ = tx.send(Msg::LowSpace(Box::new(super::LowSpace { need, free, dir, resume })));
                        waker();
                        return;
                    }
                }
            }
            let transform = match &spec {
                WriteSpec::Identity => Transform::Identity,
                WriteSpec::Eol(le) => Transform::Eol(*le),
                WriteSpec::Replace { matcher, template, range } => Transform::Replace { matcher, template, range: range.clone() },
                WriteSpec::DeleteColumns { dialect, cols } => Transform::DeleteColumns { dialect: *dialect, cols },
                WriteSpec::Transcode { enc, bom, eol } => Transform::Transcode { enc: *enc, bom: *bom, eol: *eol },
                WriteSpec::Csv { dialect, rewrite } => Transform::Csv { dialect: *dialect, rewrite },
                WriteSpec::Lines { sel, keep } => Transform::Lines { keep: &|l: &[u8]| sel.shows(l) == *keep },
                WriteSpec::TextOp { op, range } => Transform::TextOp { op: *op, range: range.clone() },
                WriteSpec::ColumnEdit { from, cells, gen } => Transform::ColumnEdit { from: *from, cells: *cells, gen },
                WriteSpec::Sort { spec, eol } => Transform::Sort { spec: *spec, eol },
            };
            let r = save::write_temp(&snap, &target, transform, &cancel, &progress);
            drop(snap);
            let msg = match r {
                Ok((temp, stats)) => Msg::TempWritten(Box::new(TempWritten { doc: id, temp, target, backup, prefix, stats, kind, elapsed: t.elapsed() })),
                Err(SaveError::Cancelled) => Msg::JobFailed { doc: id, msg: None },
                Err(SaveError::Io(e)) => Msg::JobFailed { doc: id, msg: Some(e) },
            };
            let _ = tx.send(msg);
            waker();
        });
    }

    pub(super) fn on_temp_written(&mut self, tw: TempWritten) {
        let waker = self.waker.clone();
        let tx = self.tx.clone();
        let Some(d) = self.docs.iter_mut().find(|d| d.id == tw.doc) else {
            let _ = std::fs::remove_file(&tw.temp);
            return;
        };
        // Release everything that maps the old file, keeping the edits for a failure rollback.
        d.cancel_jobs();
        d.watch_cancel.cancel();
        let weak: Weak<Source> = Arc::downgrade(&d.buf.source);
        d.buf = d.buf.with_source(Arc::new(Source::empty()), LineIndex::new(0));
        d.state = DocState::Finalizing(match tw.kind {
            WriteKind::Save | WriteKind::Transcode(_) => tr("Saving…").into(),
            WriteKind::Replace => tr("Replacing on disk…").into(),
            WriteKind::DeleteColumns(_) => tr("Deleting columns on disk…").into(),
            WriteKind::Csv(_) => tr("Rewriting on disk…").into(),
            WriteKind::Lines => tr("Deleting lines on disk…").into(),
            WriteKind::TextOp(op) => trf("{op} on disk…", &[("op", tr(op.name()))]),
            WriteKind::ColumnEdit => tr("Filling the column on disk…").into(),
            WriteKind::Sort => tr("Sorting on disk…").into(),
        });
        let original = d.path.clone();
        jobs::spawn("finalize", move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            while weak.strong_count() > 0 && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            let result = match save::finalize(&tw.temp, &tw.target, tw.backup) {
                Ok(()) => open_worker(&tw.target, match tw.kind {
                    WriteKind::Transcode(enc) => Some(enc),
                    _ => None,
                }, bigtext::file::OpenMode::Auto).map_err(|e| (trf("Saved, but reopening failed: {e}", &[("e", &e)]), Err(e))),
                Err(e) => {
                    let reopened = match &original {
                        Some(p) => open_worker(p, None, bigtext::file::OpenMode::Auto),
                        None => Err("no original file".into()),
                    };
                    Err((e, reopened))
                }
            };
            let _ = tx.send(Msg::Finalized(Box::new(Finalized {
                doc: tw.doc,
                prefix: tw.prefix,
                result,
                stats: tw.stats,
                kind: tw.kind,
                elapsed: tw.elapsed,
            })));
            waker();
        });
    }

    pub(super) fn on_finalized(&mut self, f: Finalized) {
        let waker = self.waker.clone();
        let mut close = None;
        let mut error = None;
        let Some(d) = self.docs.iter_mut().find(|d| d.id == f.doc) else { return };
        d.save_job = None;
        match f.result {
            Ok(o) => {
                let target = o.path.clone();
                let same = d.path.as_deref().is_some_and(|p| same_path(p, &target));
                // Saved as a real file: the unpacked copy is not needed any more.
                if !same && d.temporary {
                    d.temporary = false;
                    d.unpacked_from = None;
                    if let Some(temp) = d.path.clone() {
                        jobs::spawn("remove-unpacked", move || bigtext::decompress::remove_temp(&temp));
                    }
                }
                d.title = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                d.path = Some(target);
                let codec = d.buf.codec;
                let mut det = o.detected;
                // Keep the user's encoding choice rather than re-sniffing it.
                if !matches!(f.kind, WriteKind::Transcode(_)) {
                    det.codec.enc = codec.enc;
                }
                d.install(o.source, det, if same { f.prefix } else { None }, waker.clone());
                d.eol_on_save = None;
                let secs = f.elapsed.as_secs_f64();
                d.banner = Some(Banner::Info(match &f.kind {
                    WriteKind::Replace => trf("Replaced {n} matches; wrote {size} in {secs}s.", &[("n", &f.stats.count.to_string()), ("size", &human_bytes(f.stats.bytes_out)), ("secs", &format!("{secs:.2}"))]),
                    WriteKind::DeleteColumns(cols) => {
                        d.table_state.columns_deleted(cols);
                        trf("Deleted {cols} from all {n} lines; wrote {size} in {secs}s.", &[("cols", &super::columns::count_label(cols.len())), ("n", &f.stats.count.to_string()), ("size", &human_bytes(f.stats.bytes_out)), ("secs", &format!("{secs:.2}"))])
                    }
                    WriteKind::Sort => trf("Sorted into {n} lines; wrote {size} in {secs}s.", &[("n", &f.stats.count.to_string()), ("size", &human_bytes(d.buf.len())), ("secs", &format!("{secs:.2}"))]),
                    WriteKind::ColumnEdit => trf("Filled the column on {n} lines in {secs}s.", &[("n", &f.stats.count.to_string()), ("secs", &format!("{secs:.2}"))]),
                    WriteKind::TextOp(op) => trf("{op}: wrote {size} in {secs}s.", &[("op", tr(op.name())), ("size", &human_bytes(f.stats.bytes_out)), ("secs", &format!("{secs:.2}"))]),
                    WriteKind::Lines => trf("Kept {n} lines; wrote {size} in {secs}s.", &[("n", &f.stats.count.to_string()), ("size", &human_bytes(f.stats.bytes_out)), ("secs", &format!("{secs:.2}"))]),
                    WriteKind::Save => trf("Saved {size} in {secs}s.", &[("size", &human_bytes(d.buf.len())), ("secs", &format!("{secs:.2}"))]),
                    WriteKind::Csv(rw) => {
                        if matches!(rw, bigtext::csv::Rewrite::Derive { .. }) {
                            d.table_state.keep_layout();
                        }
                        if let bigtext::csv::Rewrite::Swap(a, b) = rw {
                            d.table_state.columns_swapped(*a, *b);
                        }
                        super::i18n::trf("Rewrote all {n} lines; wrote {size} in {secs}s.", &[("n", &f.stats.count.to_string()), ("size", &human_bytes(f.stats.bytes_out)), ("secs", &format!("{secs:.2}"))])
                    }
                    WriteKind::Transcode(enc) if f.stats.count > 0 => super::i18n::trf(
                        "Saved as {enc} ({size}). {n} characters do not exist in {enc} and were written as “?”.",
                        &[("enc", enc.name()), ("size", &human_bytes(f.stats.bytes_out)), ("n", &f.stats.count.to_string())],
                    ),
                    WriteKind::Transcode(enc) => super::i18n::trf("Saved as {enc} ({size}) in {secs}s.", &[("enc", enc.name()), ("size", &human_bytes(f.stats.bytes_out)), ("secs", &format!("{secs:.2}"))]),
                }));
                if let Some(Banner::Info(t)) = &d.banner {
                    self.announcement = Some(t.clone());
                }
                if d.close_after_save {
                    close = Some(d.id);
                }
                super::watch(d, self.tx.clone(), waker);
            }
            Err((msg, reopened)) => {
                match reopened {
                    Ok(o) => d.rebind(o.source, waker.clone()),
                    Err(e) => d.state = DocState::Failed(format!("{msg}\n{e}")),
                }
                d.close_after_save = false;
                error = Some(msg);
                super::watch(d, self.tx.clone(), waker);
            }
        }
        if let Some(e) = error {
            self.error = Some(e);
        }
        if let Some(id) = close {
            self.close_tab(id, true);
        }
    }

    pub(super) fn on_job_failed(&mut self, doc: u64, msg: Option<String>) {
        if let Some(d) = self.docs.iter_mut().find(|d| d.id == doc) {
            d.save_job = None;
            d.close_after_save = false;
            if msg.is_none() {
                d.banner = Some(Banner::Info(tr("Cancelled. The file on disk was not modified.").into()));
            }
        }
        if let Some(m) = msg {
            self.error = Some(m);
        }
    }

    // ------------------------------------------------------------------------------------------
    // Replace.

    fn replace_setup(&mut self) -> Option<(u64, Arc<Matcher>, Template, std::ops::Range<u64>)> {
        let d = self.docs.get(self.active)?;
        if !d.is_ready() {
            return None;
        }
        if d.read_only {
            self.search.status = tr("Document is read-only").into();
            return None;
        }
        let m = match self.search.matcher(d.buf.codec) {
            Ok(m) => m,
            Err(e) => {
                self.search.status = e;
                return None;
            }
        };
        let tpl = Template::new(&self.search.replace_with, self.search.query.regex);
        let range = if self.search.in_selection && d.has_selection() {
            let (s, e) = d.selection();
            s..e
        } else {
            0..d.buf.len()
        };
        Some((d.id, m, tpl, range))
    }

    pub(super) fn replace_current(&mut self) {
        let Some((_, m, tpl, _)) = self.replace_setup() else { return };
        let d = &mut self.docs[self.active];
        let (s, e) = d.selection();
        let is_match = s != e && bigtext::search::matches_in_view(&d.buf, &m, s..e, 64).contains(&(s, e));
        if is_match && d.editable() {
            let snap = d.buf.snapshot();
            let bytes = tpl.expand(&snap, &m, s, e);
            drop(snap);
            d.edit(s..e, &bytes, super::document::EditKind::Other);
        }
        self.find_next(true);
    }

    pub(crate) fn replace_all(&mut self) {
        let Some((id, m, tpl, range)) = self.replace_setup() else { return };
        let d = &mut self.docs[self.active];
        if d.save_job.is_some() {
            return;
        }
        let cancel = CancelToken::new();
        let progress = Progress::new(range.end - range.start);
        d.save_job = Some(SaveJob { cancel: cancel.clone(), progress: progress.clone(), label: tr("Counting matches").into() });
        let snap = d.buf.snapshot();
        let version = d.version;
        let tx = self.tx.clone();
        let waker = self.waker.clone();
        jobs::spawn("replace-collect", move || {
            let msg = match replace::collect(&snap, &m, &tpl, range.clone(), UNDO_THRESHOLD, &cancel, Some(&progress)) {
                Collected::Edits(edits) => {
                    let count = edits.len();
                    let (pieces, add) = snap.build_batch(&edits);
                    Msg::ReplaceBatch { doc: id, version, add_len: snap.add_len(), pieces, add, count }
                }
                Collected::TooMany => Msg::ReplaceTooMany { doc: id, version, matcher: m, template: tpl, range },
                Collected::Cancelled => Msg::JobFailed { doc: id, msg: None },
            };
            let _ = tx.send(msg);
            waker();
        });
    }

    pub(super) fn on_replace_batch(&mut self, doc: u64, version: u64, add_len: u64, pieces: Vec<Piece>, add: Vec<u8>, count: usize) {
        let Some(d) = self.docs.iter_mut().find(|d| d.id == doc) else { return };
        d.save_job = None;
        if d.version != version || d.buf.add_len() != add_len {
            self.search.status = tr("Document changed during replace; nothing replaced").into();
            return;
        }
        if count == 0 {
            self.search.status = tr("No matches").into();
            return;
        }
        let (s, _) = d.selection();
        let swap = d.buf.apply_batch(pieces, &add);
        d.record(swap, (s, s));
        self.search.status = trf("Replaced {n} matches (undo with Ctrl+Z)", &[("n", &count.to_string())]);
    }

    pub(super) fn on_replace_too_many(&mut self, doc: u64, version: u64, matcher: Arc<Matcher>, template: Template, range: std::ops::Range<u64>) {
        let Some(d) = self.docs.iter_mut().find(|d| d.id == doc) else { return };
        d.save_job = None;
        if d.version != version {
            return;
        }
        if d.path.is_none() {
            self.error = Some(trf("More than {n} matches. Save the document to a file first; large replacements are streamed to disk.", &[("n", &UNDO_THRESHOLD.to_string())]));
            return;
        }
        self.pending_replace = Some((doc, matcher, template, range));
        self.confirm = Some(Confirm {
            title: tr("Replace All on disk").into(),
            message: format!(
                "{}{}",
                trf(
                    "More than {n} matches. Replace All will stream the result to a new file and atomically replace “{file}”. Undo will not be available.",
                    &[("n", &UNDO_THRESHOLD.to_string()), ("file", &d.title)]
                ),
                if d.is_modified() { tr(" Your unsaved edits are included.") } else { "" }
            ),
            action: ConfirmAction::ReplaceOnDisk { doc },
            checkbox: Some((tr("Keep a backup (.bak)").into(), true)),
        });
    }

    pub(crate) fn replace_on_disk(&mut self, doc: u64, backup: bool) {
        let Some((id, matcher, template, range)) = self.pending_replace.take() else { return };
        if id != doc {
            return;
        }
        let Some(path) = self.docs.iter().find(|d| d.id == doc).and_then(|d| d.path.clone()) else { return };
        self.start_write(doc, path, WriteSpec::Replace { matcher, template, range }, backup);
    }

    pub(super) fn replace_preview(&mut self) {
        let Some((id, m, tpl, range)) = self.replace_setup() else { return };
        let snap = self.docs[self.active].buf.snapshot();
        let tx = self.tx.clone();
        let waker = self.waker.clone();
        jobs::spawn("replace-preview", move || {
            let items = replace::preview(&snap, &m, &tpl, range, 200, &CancelToken::new());
            let lines = items.iter().map(|(off, _, _)| snap.line_of(*off).0 + 1).collect();
            let _ = tx.send(Msg::Preview { doc: id, items, lines });
            waker();
        });
    }
}

