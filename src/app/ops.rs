//! Save / Save As, streaming replace-all and the finalize protocol that swaps files on disk.
//!
//! Protocol: (1) a worker streams a snapshot to a temp file and fsyncs it; (2) the UI drops its
//! `Arc<Source>` and cancels jobs holding snapshots; (3) a finalize worker waits until the old
//! source is gone, atomically replaces the target, and reopens it. If anything fails the
//! original is reopened unchanged and the user's edits are re-attached to it.

use std::path::PathBuf;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use bigtext::encoding::LineEnding;
use bigtext::file::{self, Source};
use bigtext::index::LineIndex;
use bigtext::jobs::{self, human_bytes, CancelToken, Progress};
use bigtext::replace::{self, Collected, ReplaceStats, Template};
use bigtext::save::{self, SaveError, Transform};
use bigtext::search::Matcher;
use bigtext::text::Piece;

use super::document::{Banner, DocState};
use super::{open_worker, BigTextApp, Confirm, ConfirmAction, Msg, Opened};

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
}

pub struct TempWritten {
    pub doc: u64,
    pub temp: PathBuf,
    pub target: PathBuf,
    pub backup: bool,
    pub prefix: Option<Vec<u64>>,
    pub stats: ReplaceStats,
    pub replace: bool,
    pub elapsed: Duration,
}

pub struct Finalized {
    pub doc: u64,
    pub target: PathBuf,
    pub prefix: Option<Vec<u64>>,
    pub result: Result<Opened, (String, Result<Opened, String>)>,
    pub stats: ReplaceStats,
    pub replace: bool,
    pub elapsed: Duration,
}

impl BigTextApp {
    pub(crate) fn save(&mut self, save_as: bool) {
        let Some(d) = self.docs.get(self.active) else { return };
        if !d.is_ready() || d.save_job.is_some() {
            return;
        }
        let target = match (&d.path, save_as) {
            (Some(p), false) => p.clone(),
            _ => {
                let mut dlg = rfd::FileDialog::new().set_title("Save As").set_file_name(&d.title);
                if let Some(dir) = d.path.as_ref().and_then(|p| p.parent()) {
                    dlg = dlg.set_directory(file::display_path(dir));
                }
                match dlg.save_file() {
                    Some(p) => p,
                    None => return,
                }
            }
        };
        let spec = match d.eol_on_save {
            Some(le) => WriteSpec::Eol(le),
            None => WriteSpec::Identity,
        };
        let id = d.id;
        self.start_write(id, target, spec, false);
    }

    pub(super) fn start_write(&mut self, id: u64, target: PathBuf, spec: WriteSpec, backup: bool) {
        let Some(d) = self.docs.iter_mut().find(|d| d.id == id) else { return };
        let same_file = d.path.as_deref().is_some_and(|p| same_path(p, &target));
        let prefix = match spec {
            WriteSpec::Identity if same_file => Some(d.buf.index.prefix_checkpoints(d.buf.pristine_prefix())),
            _ => None,
        };
        let replace = matches!(spec, WriteSpec::Replace { .. });
        let cancel = CancelToken::new();
        let progress = Progress::new(d.buf.len());
        d.save_job = Some(SaveJob {
            cancel: cancel.clone(),
            progress: progress.clone(),
            label: if replace { "Replacing".into() } else { "Saving".into() },
        });
        let snap = d.buf.snapshot();
        let tx = self.tx.clone();
        let waker = self.waker.clone();
        jobs::spawn("save", move || {
            let t = Instant::now();
            let transform = match &spec {
                WriteSpec::Identity => Transform::Identity,
                WriteSpec::Eol(le) => Transform::Eol(*le),
                WriteSpec::Replace { matcher, template, range } => Transform::Replace { matcher, template, range: range.clone() },
            };
            let r = save::write_temp(&snap, &target, transform, &cancel, &progress);
            drop(snap);
            let msg = match r {
                Ok((temp, stats)) => Msg::TempWritten(Box::new(TempWritten { doc: id, temp, target, backup, prefix, stats, replace, elapsed: t.elapsed() })),
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
        d.state = DocState::Finalizing(if tw.replace { "Replacing on disk…".into() } else { "Saving…".into() });
        let original = d.path.clone();
        jobs::spawn("finalize", move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            while weak.strong_count() > 0 && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            let result = match save::finalize(&tw.temp, &tw.target, tw.backup) {
                Ok(()) => open_worker(&tw.target, None).map_err(|e| (format!("Saved, but reopening failed: {e}"), Err(e))),
                Err(e) => {
                    let reopened = match &original {
                        Some(p) => open_worker(p, None),
                        None => Err("no original file".into()),
                    };
                    Err((e, reopened))
                }
            };
            let _ = tx.send(Msg::Finalized(Box::new(Finalized {
                doc: tw.doc,
                target: tw.target,
                prefix: tw.prefix,
                result,
                stats: tw.stats,
                replace: tw.replace,
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
                let target = std::fs::canonicalize(&f.target).unwrap_or(f.target.clone());
                let same = d.path.as_deref().is_some_and(|p| same_path(p, &target));
                d.title = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                d.path = Some(target);
                let codec = d.buf.codec;
                let mut det = o.detected;
                // Keep the user's encoding choice rather than re-sniffing it.
                det.codec.enc = codec.enc;
                d.install(o.source, det, if same { f.prefix } else { None }, waker.clone());
                d.eol_on_save = None;
                d.banner = Some(Banner::Info(if f.replace {
                    format!("Replaced {} matches; wrote {} in {:.2}s.", f.stats.count, human_bytes(f.stats.bytes_out), f.elapsed.as_secs_f64())
                } else {
                    format!("Saved {} in {:.2}s.", human_bytes(d.buf.len()), f.elapsed.as_secs_f64())
                }));
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
                d.banner = Some(Banner::Info("Cancelled. The file on disk was not modified.".into()));
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
            self.search.status = "Document is read-only".into();
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
        d.save_job = Some(SaveJob { cancel: cancel.clone(), progress: progress.clone(), label: "Counting matches".into() });
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
            self.search.status = "Document changed during replace; nothing replaced".into();
            return;
        }
        if count == 0 {
            self.search.status = "No matches".into();
            return;
        }
        let (s, _) = d.selection();
        let swap = d.buf.apply_batch(pieces, &add);
        d.record(swap, (s, s));
        self.search.status = format!("Replaced {count} matches (undo with Ctrl+Z)");
    }

    pub(super) fn on_replace_too_many(&mut self, doc: u64, version: u64, matcher: Arc<Matcher>, template: Template, range: std::ops::Range<u64>) {
        let Some(d) = self.docs.iter_mut().find(|d| d.id == doc) else { return };
        d.save_job = None;
        if d.version != version {
            return;
        }
        if d.path.is_none() {
            self.error = Some(format!("More than {UNDO_THRESHOLD} matches. Save the document to a file first; large replacements are streamed to disk."));
            return;
        }
        self.pending_replace = Some((doc, matcher, template, range));
        self.confirm = Some(Confirm {
            title: "Replace All on disk".into(),
            message: format!(
                "More than {UNDO_THRESHOLD} matches. Replace All will stream the result to a new file and atomically \
                 replace “{}”. Undo will not be available{}.",
                d.title,
                if d.is_modified() { "; your unsaved edits are included" } else { "" }
            ),
            action: ConfirmAction::ReplaceOnDisk { doc },
            checkbox: Some(("Keep a backup (.bak)".into(), true)),
        });
    }

    pub(super) fn replace_on_disk(&mut self, doc: u64, backup: bool) {
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

fn same_path(a: &std::path::Path, b: &std::path::Path) -> bool {
    let ca = std::fs::canonicalize(a).unwrap_or_else(|_| a.to_path_buf());
    let cb = std::fs::canonicalize(b).unwrap_or_else(|_| b.to_path_buf());
    ca == cb
}
