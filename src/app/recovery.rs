//! A journal of unsaved edits, so that a crash or a power cut does not lose them. A couple of
//! seconds after a pause in editing (or every 30 s of typing), a modified document's piece list
//! and the bytes typed into it are written next to the settings: a few KB for a thousand edits
//! to a 50 GB file. The next start offers to restore them, once the file on disk is checked to
//! be the same. Each running BigText holds a lock, so another one's journals are left alone.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bigtext::encoding::{Codec, Encoding, NlByte};
use bigtext::file::{OpenMode, Source};
use bigtext::index::LineIndex;
use bigtext::jobs;
use bigtext::text::{Buf, TextBuf};

use super::document::{Banner, DocState};
use super::i18n::{tr, trf};
use super::tasks::{self, TaskOutcome};
use super::{BigTextApp, Msg};

/// Typed bytes kept in a journal at most (a huge paste is not journaled).
const ADD_MAX: usize = 256 << 20;

/// A journal on disk.
#[derive(Clone, Debug, PartialEq)]
pub struct Journal {
    /// `<pid>-<document id>`.
    pub id: String,
    pub title: String,
    pub path: Option<PathBuf>,
    /// The file's length and change time when the edits were made.
    pub len: u64,
    pub mtime: Option<Duration>,
    pub codec: Codec,
    pub pieces: Vec<(Buf, u64, u64)>,
    pub add_len: u64,
    pub saved: Option<SystemTime>,
}

/// The journal folder: next to the settings (`BIGTEXT_RECOVERY_DIR` for tests).
pub fn default_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("BIGTEXT_RECOVERY_DIR") {
        return Some(PathBuf::from(d));
    }
    Some(super::extras::state_file()?.parent()?.join("recovery"))
}

fn pieces_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.pieces"))
}

fn add_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.add"))
}

/// Deletes a journal's files.
pub fn remove(dir: &Path, id: &str) {
    let _ = fs::remove_file(pieces_path(dir, id));
    let _ = fs::remove_file(add_path(dir, id));
}

fn mtime_of(t: Option<SystemTime>) -> Option<Duration> {
    t.and_then(|t| t.duration_since(UNIX_EPOCH).ok())
}

/// The text of a journal's `.pieces` file.
pub fn format(j: &Journal) -> String {
    let mut s = String::from("bigtext-journal 1\n");
    s.push_str(&format!("title={}\n", j.title));
    if let Some(p) = &j.path {
        s.push_str(&format!("path={}\n", p.display()));
    }
    s.push_str(&format!("len={}\n", j.len));
    if let Some(m) = j.mtime {
        s.push_str(&format!("mtime={}.{:09}\n", m.as_secs(), m.subsec_nanos()));
    }
    let nl = if j.codec.nl == NlByte::Cr { "cr" } else { "lf" };
    s.push_str(&format!("codec={}|{nl}|{}\n", j.codec.enc.name(), j.codec.bom_len));
    s.push_str(&format!("add={}\n", j.add_len));
    for &(b, start, len) in &j.pieces {
        s.push_str(&format!("{} {start} {len}\n", if b == Buf::Orig { 'o' } else { 'a' }));
    }
    s
}

/// Reads a `.pieces` file back; `None` when it is not one.
pub fn parse(id: &str, text: &str) -> Option<Journal> {
    let mut lines = text.lines();
    if lines.next()? != "bigtext-journal 1" {
        return None;
    }
    let mut j = Journal { id: id.to_string(), title: String::new(), path: None, len: 0, mtime: None, codec: Codec::utf8(), pieces: Vec::new(), add_len: 0, saved: None };
    for l in lines {
        if let Some((k, v)) = l.split_once('=') {
            match k {
                "title" => j.title = v.to_string(),
                "path" => j.path = Some(PathBuf::from(v)),
                "len" => j.len = v.parse().ok()?,
                "mtime" => {
                    let (s, n) = v.split_once('.')?;
                    j.mtime = Some(Duration::new(s.parse().ok()?, n.parse().ok()?));
                }
                "codec" => {
                    let mut it = v.split('|');
                    let enc = it.next()?;
                    let enc = Encoding::ALL.into_iter().find(|e| e.name() == enc)?;
                    let nl = if it.next()? == "cr" { NlByte::Cr } else { NlByte::Lf };
                    j.codec = Codec::new(enc, nl, it.next()?.parse().ok()?);
                }
                "add" => j.add_len = v.parse().ok()?,
                _ => {}
            }
            continue;
        }
        let mut it = l.split(' ');
        let b = match it.next()? {
            "o" => Buf::Orig,
            "a" => Buf::Add,
            _ => return None,
        };
        j.pieces.push((b, it.next()?.parse().ok()?, it.next()?.parse().ok()?));
    }
    Some(j)
}

/// The edited text of a journal over `source` (an empty one for an untitled document).
fn rebuild(dir: &Path, j: &Journal, source: Arc<Source>) -> Option<TextBuf> {
    let mut add = fs::read(add_path(dir, &j.id)).ok()?;
    if (add.len() as u64) < j.add_len {
        return None;
    }
    add.truncate(j.add_len as usize);
    let index = LineIndex::new(source.len());
    TextBuf::with_pieces(source, index, j.codec, &j.pieces, add)
}

/// Whether the file is still the one the edits were made to.
fn unchanged(j: &Journal) -> bool {
    let Some(p) = &j.path else { return true };
    match fs::metadata(p) {
        Ok(m) => m.len() == j.len && (j.mtime.is_none() || mtime_of(m.modified().ok()) == j.mtime),
        Err(_) => false,
    }
}

/// A document's journal as last written.
#[derive(Default)]
pub struct Entry {
    /// The document version seen, when it changed, and since when edits wait to be written.
    seen: u64,
    changed_at: Option<Instant>,
    waiting_since: Option<Instant>,
    written: u64,
    add_written: u64,
    writing: bool,
    /// Journaled at all (so there are files to delete).
    exists: bool,
}

#[derive(Default)]
pub struct RecoveryState {
    pub enabled: bool,
    pub dir: Option<PathBuf>,
    /// This instance's lock, held while it runs.
    lock: Option<File>,
    entries: HashMap<u64, Entry>,
    /// Journals left by a BigText that did not close properly.
    pub offered: Vec<Journal>,
    /// The journal waiting for its document to open.
    pending: HashMap<u64, Journal>,
}

impl RecoveryState {
    /// Journaling on (it starts once a folder is known).
    pub fn new() -> RecoveryState {
        RecoveryState { enabled: true, ..Default::default() }
    }

    fn id(doc: u64) -> String {
        format!("{}-{doc}", std::process::id())
    }
}

/// Whether the BigText that wrote journals with this process id still runs.
fn running(dir: &Path, pid: &str) -> bool {
    let p = dir.join(format!("{pid}.lock"));
    match File::options().write(true).open(&p) {
        Ok(f) => f.try_lock().is_err(),
        Err(_) => false,
    }
}

impl BigTextApp {
    /// Starts journaling into `dir` (taking this instance's lock) and finds what crashed
    /// instances left.
    pub(crate) fn start_recovery(&mut self, dir: Option<PathBuf>) {
        let Some(dir) = dir else { return };
        if fs::create_dir_all(&dir).is_err() {
            return;
        }
        let lock = File::create(dir.join(format!("{}.lock", std::process::id()))).ok().filter(|f| f.try_lock().is_ok());
        self.recovery.lock = lock;
        self.recovery.dir = Some(dir);
        self.scan_recovery();
    }

    pub(crate) fn scan_recovery(&mut self) {
        let Some(dir) = self.recovery.dir.clone() else { return };
        let mine = std::process::id().to_string();
        let mut found = Vec::new();
        for e in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(id) = name.strip_suffix(".pieces") else { continue };
            let pid = id.split('-').next().unwrap_or("");
            if pid == mine || running(&dir, pid) {
                continue;
            }
            if let Some(mut j) = fs::read_to_string(e.path()).ok().and_then(|t| parse(id, &t)) {
                j.saved = e.metadata().and_then(|m| m.modified()).ok();
                found.push(j);
            }
        }
        // Lock files of instances that are gone.
        for e in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if let Some(pid) = name.strip_suffix(".lock") {
                if pid != mine && !running(&dir, pid) && !found.iter().any(|j| j.id.starts_with(&format!("{pid}-"))) {
                    let _ = fs::remove_file(e.path());
                }
            }
        }
        found.sort_by(|a, b| a.id.cmp(&b.id));
        self.recovery.offered = found;
    }

    /// Writes the journals of documents that changed and settled, and drops those of documents
    /// that are saved, unchanged or closed (every frame).
    pub(super) fn journal_tick(&mut self) {
        let Some(dir) = self.recovery.dir.clone() else { return };
        let now = Instant::now();
        let ids: Vec<u64> = self.docs.iter().map(|d| d.id).collect();
        let gone: Vec<u64> = self.recovery.entries.keys().copied().filter(|id| !ids.contains(id)).collect();
        for id in gone {
            if let Some(e) = self.recovery.entries.remove(&id) {
                if e.exists {
                    remove(&dir, &RecoveryState::id(id));
                }
            }
        }
        for d in &self.docs {
            let wanted = self.recovery.enabled
                && d.is_ready()
                && d.is_modified()
                && !d.temporary
                && d.save_job.is_none()
                && !matches!(d.state, DocState::Finalizing(_))
                && d.buf.add_len() as usize <= ADD_MAX;
            if !wanted {
                if let Some(e) = self.recovery.entries.remove(&d.id) {
                    if e.exists {
                        remove(&dir, &RecoveryState::id(d.id));
                    }
                }
                continue;
            }
            let e = self.recovery.entries.entry(d.id).or_default();
            if e.seen != d.version {
                e.seen = d.version;
                e.changed_at = Some(now);
                e.waiting_since.get_or_insert(now);
            }
            let settled = e.changed_at.is_some_and(|t| now - t >= Duration::from_secs(2)) || e.waiting_since.is_some_and(|t| now - t >= Duration::from_secs(30));
            if e.writing || e.written == d.version || !settled {
                continue;
            }
            e.writing = true;
            let add = d.buf.add_bytes();
            // Only the new typed bytes are appended; a smaller add buffer is written again.
            let (append, delta) = if add.len() as u64 >= e.add_written && e.exists { (true, add[e.add_written as usize..].to_vec()) } else { (false, add.to_vec()) };
            let j = Journal {
                id: RecoveryState::id(d.id),
                title: d.title.clone(),
                path: d.path.clone(),
                len: d.buf.source.len(),
                mtime: mtime_of(d.buf.source.modified()),
                codec: d.buf.codec,
                pieces: d.buf.pieces().iter().map(|p| (p.buf, p.start, p.len)).collect(),
                add_len: add.len() as u64,
                saved: None,
            };
            let (doc, version, dir, tx, waker) = (d.id, d.version, dir.clone(), self.tx.clone(), self.waker.clone());
            jobs::spawn("journal", move || {
                let r = (|| -> io::Result<()> {
                    let mut f = File::options().create(true).write(true).append(append).truncate(!append).open(add_path(&dir, &j.id))?;
                    f.write_all(&delta)?;
                    f.sync_data()?;
                    let temp = dir.join(format!("{}.pieces.tmp", j.id));
                    fs::write(&temp, format(&j))?;
                    fs::rename(&temp, pieces_path(&dir, &j.id))
                })();
                let _ = tx.send(Msg::Journaled { doc, version, add_len: j.add_len, ok: r.is_ok() });
                waker();
            });
        }
    }

    pub(super) fn on_journaled(&mut self, doc: u64, version: u64, add_len: u64, ok: bool) {
        let Some(e) = self.recovery.entries.get_mut(&doc) else { return };
        e.writing = false;
        if ok {
            e.written = version;
            e.add_written = add_len;
            e.exists = true;
            e.waiting_since = None;
        }
    }

    /// A clean exit: this instance's journals and lock go.
    pub(super) fn end_recovery(&mut self) {
        let Some(dir) = self.recovery.dir.clone() else { return };
        for (id, e) in self.recovery.entries.drain() {
            if e.exists {
                remove(&dir, &RecoveryState::id(id));
            }
        }
        self.recovery.lock = None;
        let _ = fs::remove_file(dir.join(format!("{}.lock", std::process::id())));
    }

    /// Opens the journal's file (or a new document) with the edits put back.
    pub(crate) fn restore_journal(&mut self, i: usize) {
        let Some(dir) = self.recovery.dir.clone() else { return };
        if i >= self.recovery.offered.len() {
            return;
        }
        let j = self.recovery.offered.remove(i);
        match &j.path {
            None => {
                self.new_doc();
                let Some(buf) = rebuild(&dir, &j, Arc::new(Source::empty())) else {
                    self.error = Some(tr("The journal is damaged; nothing could be restored.").into());
                    return;
                };
                if let Some(d) = self.docs.get_mut(self.active) {
                    d.restore_edits(buf);
                    d.title = j.title.clone();
                }
                remove(&dir, &j.id);
            }
            Some(p) => {
                if !unchanged(&j) {
                    self.error = Some(trf("{file} changed on disk after these edits were made; putting them back would scramble it. Save the edited text as a new file instead.", &[("file", &bigtext::file::display_path(p))]));
                    self.recovery.offered.insert(i, j);
                    return;
                }
                self.open_path(p.clone());
                if let Some(d) = self.docs.get(self.active) {
                    self.recovery.pending.insert(d.id, j);
                }
                self.apply_pending_journal();
            }
        }
    }

    /// Puts back the journal waiting for a document that has opened (every frame).
    pub(super) fn apply_pending_journal(&mut self) {
        if self.recovery.pending.is_empty() {
            return;
        }
        let Some(dir) = self.recovery.dir.clone() else { return };
        let ready: Vec<u64> = self.docs.iter().filter(|d| d.is_ready() && self.recovery.pending.contains_key(&d.id)).map(|d| d.id).collect();
        for id in ready {
            let Some(j) = self.recovery.pending.remove(&id) else { continue };
            let Some(d) = self.docs.iter_mut().find(|d| d.id == id) else { continue };
            if d.is_modified() || d.buf.source.len() != j.len {
                self.error = Some(tr("The document changed before the edits could be put back; nothing was restored.").into());
                continue;
            }
            match rebuild(&dir, &j, d.buf.source.clone()) {
                Some(buf) => {
                    d.restore_edits(buf);
                    d.banner = Some(Banner::Info(tr("Unsaved edits from the last session were put back. Save to keep them.").into()));
                    remove(&dir, &j.id);
                }
                None => self.error = Some(tr("The journal is damaged; nothing could be restored.").into()),
            }
        }
    }

    /// Writes the journal's edited text to a file the user picks.
    fn save_journal_as(&mut self, i: usize) {
        let Some(dir) = self.recovery.dir.clone() else { return };
        let Some(j) = self.recovery.offered.get(i).cloned() else { return };
        let name = j.path.as_deref().and_then(|p| p.file_name()).map(|n| format!("{} (recovered)", n.to_string_lossy())).unwrap_or_else(|| format!("{}.txt", j.title));
        let Some(target) = rfd::FileDialog::new().set_title(tr("Save text as")).set_file_name(name).save_file() else { return };
        self.recovery.offered.remove(i);
        self.spawn_task(tr("Saving the recovered text").into(), move |progress, cancel| {
            let source = match &j.path {
                Some(p) => match Source::open(p, OpenMode::Auto) {
                    Ok(s) => Arc::new(s),
                    Err(e) => return TaskOutcome::Failed(super::friendly_io(&e, p)),
                },
                None => Arc::new(Source::empty()),
            };
            let Some(buf) = rebuild(&dir, &j, source) else { return TaskOutcome::Failed(tr("The journal is damaged; nothing could be restored.").into()) };
            let snap = buf.snapshot();
            progress.set_total(snap.len());
            let out = tasks::write_via_temp(&target, |w| {
                let mut stopped = false;
                snap.for_each_chunk(0..snap.len(), |base, b| {
                    if cancel.is_cancelled() || w.write_all(b).is_err() {
                        stopped = true;
                        return false;
                    }
                    progress.set_done(base + b.len() as u64);
                    true
                })
                .ok_or_else(|| io::Error::other("reading failed"))?;
                Ok((!stopped).then(|| trf("Saved the recovered text to {file}.", &[("file", &super::filter_panel::file_name(&target))])))
            });
            if matches!(out, TaskOutcome::Done { .. }) {
                remove(&dir, &j.id);
            }
            out
        });
    }

    pub(super) fn recovery_window(&mut self, ctx: &egui::Context) {
        if self.recovery.offered.is_empty() {
            return;
        }
        let Some(dir) = self.recovery.dir.clone() else { return };
        let (mut restore, mut save, mut discard) = (None, None, None);
        egui::Window::new(tr("Unsaved edits from the last session")).collapsible(false).resizable(false).show(ctx, |ui| {
            ui.label(tr("BigText did not close properly. These documents had edits that were not saved:"));
            ui.separator();
            for (i, j) in self.recovery.offered.iter().enumerate() {
                ui.horizontal(|ui| {
                    let name = j.path.as_deref().map(bigtext::file::display_path).unwrap_or_else(|| j.title.clone());
                    ui.strong(name);
                    if let Some(t) = j.saved {
                        ui.weak(super::archive::date(t));
                    }
                });
                ui.horizontal(|ui| {
                    let ok = unchanged(j);
                    if ui.add_enabled(ok, egui::Button::new(tr("Restore"))).on_disabled_hover_text(tr("The file changed on disk since; save the edited text as a new file instead.")).clicked() {
                        restore = Some(i);
                    }
                    if ui.button(tr("Save text as…")).clicked() {
                        save = Some(i);
                    }
                    if ui.button(tr("Discard")).clicked() {
                        discard = Some(i);
                    }
                });
                ui.add_space(4.0);
            }
        });
        if let Some(i) = restore {
            self.restore_journal(i);
        } else if let Some(i) = save {
            self.save_journal_as(i);
        } else if let Some(i) = discard {
            let j = self.recovery.offered.remove(i);
            remove(&dir, &j.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_round_trip() {
        let j = Journal {
            id: "12-3".into(),
            title: "app.log".into(),
            path: Some(PathBuf::from(r"D:\logs\app.log")),
            len: 1 << 40,
            mtime: Some(Duration::new(1_761_000_000, 123_456_789)),
            codec: Codec::new(Encoding::Utf16Le, NlByte::Lf, 2),
            pieces: vec![(Buf::Orig, 0, 100), (Buf::Add, 0, 5), (Buf::Orig, 100, (1 << 40) - 100)],
            add_len: 5,
            saved: None,
        };
        assert_eq!(parse("12-3", &format(&j)), Some(j.clone()));
        let untitled = Journal { path: None, mtime: None, codec: Codec::utf8(), ..j };
        assert_eq!(parse("12-3", &format(&untitled)), Some(untitled));
        assert_eq!(parse("x", "not a journal\n"), None);
    }
}
