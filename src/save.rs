//! Crash-safe saving: stream the document to a temp file in the target's directory, fsync, then
//! atomically replace the target. The original is never touched until the final rename, and any
//! failure leaves it intact.
//!
//! On Windows the replacement uses `ReplaceFileW`, which keeps the original's attributes and
//! ACLs and handles the optional backup. The caller must release every mapping/handle of the
//! target first (Windows refuses to replace a memory-mapped file).

use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::csv::{self, Dialect};
use crate::encoding::{Codec, Encoding, LineEnding, NlByte};
use crate::jobs::{CancelToken, Progress};
use crate::replace::{self, ReplaceStats, Template};
use crate::search::Matcher;
use crate::text::TextBuf;

const WRITE_BUF: usize = 8 * 1024 * 1024;

pub enum Transform<'a> {
    Identity,
    /// Convert line breaks to the given style.
    Eol(LineEnding),
    /// Replace every match in the range.
    Replace { matcher: &'a Matcher, template: &'a Template, range: std::ops::Range<u64> },
    /// Delete the columns `cols` (sorted, distinct) of a CSV document.
    DeleteColumns { dialect: Dialect, cols: &'a [usize] },
    /// Re-encode into `enc` (with a byte order mark if `bom`), optionally converting line
    /// breaks. `ReplaceStats::count` is the number of characters `enc` cannot represent
    /// (written as '?').
    Transcode { enc: Encoding, bom: bool, eol: Option<LineEnding> },
    /// Sort rows, swap or select columns of a CSV document.
    Csv { dialect: Dialect, rewrite: &'a csv::Rewrite },
}

const TRANSCODE_STEP: u64 = 4 * 1024 * 1024;

/// Streams `buf` re-encoded into `enc`. Pieces are cut at character boundaries, and a CR at
/// the end of a piece waits for the next one so a CRLF is never split.
pub fn transcode_to<W: Write>(buf: &TextBuf, enc: Encoding, bom: bool, eol: Option<LineEnding>, out: &mut W, cancel: &CancelToken, progress: Option<&Progress>) -> io::Result<Option<ReplaceStats>> {
    let target = Codec::new(enc, NlByte::Lf, 0);
    let len = buf.len();
    let mut stats = ReplaceStats { count: 0, bytes_in: len, bytes_out: 0 };
    let mut emit = |b: &[u8], stats: &mut ReplaceStats| -> io::Result<()> {
        stats.bytes_out += b.len() as u64;
        out.write_all(b)
    };
    if bom {
        emit(enc.bom(), &mut stats)?;
    }
    let breaks = eol.map(|e| target.line_break(e));
    let mut pending_cr = false;
    let mut pos = buf.codec.bom_len as u64;
    while pos < len {
        if cancel.is_cancelled() {
            return Ok(None);
        }
        let mut end = buf.snap_char((pos + TRANSCODE_STEP).min(len));
        if end <= pos {
            end = buf.next_char(pos);
        }
        let raw = buf.read(pos..end).ok_or_else(|| io::Error::other("Could not read the document"))?;
        let mut text = buf.codec.decode_lossy(&raw);
        if let Some(nl) = &breaks {
            if pending_cr {
                text.insert(0, '\r');
            }
            pending_cr = end < len && text.ends_with('\r');
            if pending_cr {
                text.pop();
            }
            let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
            let encoded = count_and_encode(&target, &normalized, &mut stats);
            if nl.as_slice() == target.encode("\n").as_slice() {
                emit(&encoded, &mut stats)?;
            } else {
                // Re-encode with the wanted break (it is ASCII, so splitting is safe).
                let mut first = true;
                for part in normalized.split('\n') {
                    if !first {
                        emit(nl, &mut stats)?;
                    }
                    first = false;
                    emit(&target.encode(part), &mut stats)?;
                }
            }
        } else {
            let encoded = count_and_encode(&target, &text, &mut stats);
            emit(&encoded, &mut stats)?;
        }
        if let Some(p) = progress {
            p.set_done(end);
        }
        pos = end;
    }
    if pending_cr {
        if let Some(nl) = &breaks {
            emit(nl, &mut stats)?;
        }
    }
    Ok(Some(stats))
}

fn count_and_encode(target: &Codec, text: &str, stats: &mut ReplaceStats) -> Vec<u8> {
    if !target.can_encode(text) {
        stats.count += text.chars().filter(|c| !target.can_encode(c.encode_utf8(&mut [0; 4]))).count() as u64;
    }
    target.encode(text)
}

#[derive(Debug)]
pub enum SaveError {
    Cancelled,
    Io(String),
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SaveError::Cancelled => write!(f, "cancelled"),
            SaveError::Io(s) => write!(f, "{s}"),
        }
    }
}

/// Temp file next to `target` (same volume, so the final rename is atomic).
pub fn temp_path_for(target: &Path) -> PathBuf {
    let dir = target.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    dir.join(format!(".{name}.{}-{nanos:x}.bigtext.tmp", std::process::id()))
}

pub fn backup_path_for(target: &Path) -> PathBuf {
    let mut s = target.as_os_str().to_os_string();
    s.push(".bak");
    PathBuf::from(s)
}

/// Human-readable message for an I/O error while saving.
pub fn describe(e: &io::Error, path: &Path) -> String {
    let p = crate::file::display_path(path);
    match e.raw_os_error() {
        Some(32) | Some(33) => format!("“{p}” is in use by another process. Close it there and try again."),
        Some(5) => format!("Access denied writing “{p}”. The file may be read-only or protected."),
        Some(112) | Some(39) => format!("Not enough disk space to save “{p}”."),
        Some(1224) => format!("“{p}” is still mapped by another program."),
        _ => match e.kind() {
            io::ErrorKind::PermissionDenied => format!("Access denied writing “{p}”."),
            io::ErrorKind::NotFound => format!("Folder not found for “{p}”."),
            _ => format!("Could not save “{p}”: {e}"),
        },
    }
}

/// Line-ending converter that streams with one unit of look-behind.
struct EolWriter<'w, W: Write> {
    out: &'w mut W,
    codec: Codec,
    target: Vec<u8>,
    pending_cr: bool,
    written: u64,
}

impl<W: Write> EolWriter<'_, W> {
    fn emit(&mut self, b: &[u8]) -> io::Result<()> {
        self.written += b.len() as u64;
        self.out.write_all(b)
    }

    fn write(&mut self, chunk: &[u8]) -> io::Result<()> {
        let unit = self.codec.unit();
        let cr = self.codec.cr_unit().to_vec();
        let lf: Vec<u8> = match self.codec.enc {
            crate::encoding::Encoding::Utf16Le => b"\n\0".to_vec(),
            crate::encoding::Encoding::Utf16Be => b"\0\n".to_vec(),
            _ => b"\n".to_vec(),
        };
        let target = self.target.clone();
        if unit == 1 {
            let mut i = 0;
            while i < chunk.len() {
                if self.pending_cr {
                    self.pending_cr = false;
                    if chunk[i] == b'\n' {
                        self.emit(&target)?;
                        i += 1;
                        continue;
                    } else if self.codec.nl == NlByte::Lf {
                        self.emit(b"\r")?;
                    } else {
                        self.emit(&target)?;
                    }
                }
                match memchr::memchr2(b'\r', b'\n', &chunk[i..]) {
                    Some(j) => {
                        self.emit(&chunk[i..i + j])?;
                        let c = chunk[i + j];
                        if c == b'\r' {
                            self.pending_cr = true;
                        } else if self.codec.nl == NlByte::Lf {
                            self.emit(&target)?;
                        } else {
                            self.emit(b"\n")?;
                        }
                        i += j + 1;
                    }
                    None => {
                        self.emit(&chunk[i..])?;
                        break;
                    }
                }
            }
        } else {
            for u in chunk.chunks(2) {
                if self.pending_cr {
                    self.pending_cr = false;
                    if u == lf.as_slice() {
                        self.emit(&target)?;
                        continue;
                    } else if self.codec.nl == NlByte::Lf {
                        self.emit(&cr)?;
                    } else {
                        self.emit(&target)?;
                    }
                }
                if u == cr.as_slice() {
                    self.pending_cr = true;
                } else if u == lf.as_slice() && self.codec.nl == NlByte::Lf {
                    self.emit(&target)?;
                } else {
                    self.emit(u)?;
                }
            }
        }
        Ok(())
    }

    fn finish(&mut self) -> io::Result<()> {
        if self.pending_cr {
            self.pending_cr = false;
            if self.codec.nl == NlByte::Lf {
                let cr = self.codec.cr_unit().to_vec();
                self.emit(&cr)?;
            } else {
                let t = self.target.clone();
                self.emit(&t)?;
            }
        }
        Ok(())
    }
}

/// Streams `buf` (a snapshot) through `transform` into a new temp file next to `target`, and
/// fsyncs it. On any error or cancellation the temp file is removed.
pub fn write_temp(buf: &TextBuf, target: &Path, transform: Transform, cancel: &CancelToken, progress: &Progress) -> Result<(PathBuf, ReplaceStats), SaveError> {
    let temp = temp_path_for(target);
    let file = File::options().write(true).create_new(true).open(&temp).map_err(|e| SaveError::Io(describe(&e, target)))?;
    let result = (|| -> Result<ReplaceStats, SaveError> {
        let mut w = BufWriter::with_capacity(WRITE_BUF, &file);
        let len = buf.len();
        progress.set_total(len);
        let io = |e: io::Error| SaveError::Io(describe(&e, target));
        let stats = match transform {
            Transform::Replace { matcher, template, range } => {
                match replace::replace_all_to(buf, matcher, template, range, &mut w, cancel, Some(progress)).map_err(io)? {
                    Some(s) => s,
                    None => return Err(SaveError::Cancelled),
                }
            }
            Transform::DeleteColumns { dialect, cols } => match csv::delete_columns_to(buf, dialect, cols, &mut w, cancel, Some(progress)).map_err(io)? {
                Some(s) => s,
                None => return Err(SaveError::Cancelled),
            },
            Transform::Csv { dialect, rewrite } => match csv::rewrite_to(buf, dialect, rewrite, &mut w, cancel, Some(progress)).map_err(io)? {
                Some(s) => s,
                None => return Err(SaveError::Cancelled),
            },
            Transform::Transcode { enc, bom, eol } => match transcode_to(buf, enc, bom, eol, &mut w, cancel, Some(progress)).map_err(io)? {
                Some(s) => s,
                None => return Err(SaveError::Cancelled),
            },
            Transform::Identity | Transform::Eol(_) => {
                let mut sink = match &transform {
                    Transform::Eol(style) => Sink::Eol(EolWriter { out: &mut w, codec: buf.codec, target: buf.codec.line_break(*style), pending_cr: false, written: 0 }),
                    _ => Sink::Plain(&mut w, 0),
                };
                let mut err = None;
                let mut done = 0u64;
                // Read in bounded pieces so cancellation and progress stay responsive.
                const STEP: u64 = 16 * 1024 * 1024;
                let mut pos = 0;
                while pos < len {
                    if cancel.is_cancelled() {
                        return Err(SaveError::Cancelled);
                    }
                    let end = (pos + STEP).min(len);
                    let r = buf.for_each_chunk(pos..end, |_, b| {
                        if let Err(e) = sink.write(b) {
                            err = Some(e);
                            return false;
                        }
                        done += b.len() as u64;
                        true
                    });
                    if let Some(e) = err.take() {
                        return Err(io(e));
                    }
                    if r.is_none() {
                        return Err(SaveError::Io("Could not read the document while saving.".into()));
                    }
                    progress.set_done(done);
                    pos = end;
                }
                sink.finish().map_err(io)?;
                ReplaceStats { count: 0, bytes_in: len, bytes_out: sink.written() }
            }
        };
        w.flush().map_err(io)?;
        drop(w);
        file.sync_all().map_err(io)?;
        Ok(stats)
    })();
    drop(file);
    match result {
        Ok(s) => Ok((temp, s)),
        Err(e) => {
            let _ = fs::remove_file(&temp);
            Err(e)
        }
    }
}

enum Sink<'w, W: Write> {
    Plain(&'w mut W, u64),
    Eol(EolWriter<'w, W>),
}

impl<W: Write> Sink<'_, W> {
    fn write(&mut self, b: &[u8]) -> io::Result<()> {
        match self {
            Sink::Plain(w, n) => {
                *n += b.len() as u64;
                w.write_all(b)
            }
            Sink::Eol(e) => e.write(b),
        }
    }
    fn finish(&mut self) -> io::Result<()> {
        match self {
            Sink::Plain(..) => Ok(()),
            Sink::Eol(e) => e.finish(),
        }
    }
    fn written(&self) -> u64 {
        match self {
            Sink::Plain(_, n) => *n,
            Sink::Eol(e) => e.written,
        }
    }
}

/// Atomically moves `temp` over `target`, optionally keeping `target.bak`. Retries briefly on
/// transient sharing violations (virus scanners, indexers). On failure the temp file is removed
/// and the original is left as it was.
pub fn finalize(temp: &Path, target: &Path, backup: bool) -> Result<(), String> {
    let r = finalize_impl(temp, target, backup);
    if r.is_err() {
        let _ = fs::remove_file(temp);
    }
    r
}

#[cfg(windows)]
fn finalize_impl(temp: &Path, target: &Path, backup: bool) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn ReplaceFileW(replaced: *const u16, replacement: *const u16, backup: *const u16, flags: u32, exclude: *mut std::ffi::c_void, reserved: *mut std::ffi::c_void) -> i32;
        fn GetLastError() -> u32;
    }
    fn wide(p: &Path) -> Vec<u16> {
        let s = p.as_os_str();
        let st = s.to_string_lossy();
        let mut v: Vec<u16> = if p.is_absolute() && !st.starts_with(r"\\") {
            r"\\?\".encode_utf16().chain(s.encode_wide()).collect()
        } else if let Some(unc) = st.strip_prefix(r"\\").filter(|r| !r.starts_with(r"?\")) {
            format!(r"\\?\UNC\{unc}").encode_utf16().collect()
        } else {
            s.encode_wide().collect()
        };
        v.push(0);
        v
    }
    if !target.exists() {
        return rename_retry(temp, target).map_err(|e| describe(&e, target));
    }
    // ReplaceFileW always gets a backup name so a half-finished replace can be undone.
    let user_bak = backup_path_for(target);
    let internal_bak = temp.with_extension("bak.tmp");
    let bak = if backup { user_bak.clone() } else { internal_bak.clone() };
    if backup && bak.exists() {
        let _ = fs::remove_file(&bak);
    }
    let (wt, wr, wb) = (wide(target), wide(temp), wide(&bak));
    let mut last = 0u32;
    for attempt in 0..40 {
        // REPLACEFILE_IGNORE_MERGE_ERRORS = 2
        let ok = unsafe { ReplaceFileW(wt.as_ptr(), wr.as_ptr(), wb.as_ptr(), 2, std::ptr::null_mut(), std::ptr::null_mut()) };
        if ok != 0 {
            if !backup {
                let _ = fs::remove_file(&internal_bak);
            }
            return Ok(());
        }
        last = unsafe { GetLastError() };
        match last {
            // ERROR_UNABLE_TO_MOVE_REPLACEMENT_2: original was moved to the backup name. Restore it.
            1177 => {
                let _ = fs::rename(&bak, target);
                break;
            }
            // Sharing violation, access denied (scanners), unable to remove replaced, mapped file.
            32 | 5 | 1175 | 1224 if attempt < 39 => std::thread::sleep(Duration::from_millis(50)),
            _ => break,
        }
    }
    Err(describe(&io::Error::from_raw_os_error(last as i32), target))
}

#[cfg(windows)]
fn rename_retry(from: &Path, to: &Path) -> io::Result<()> {
    let mut last = None;
    for _ in 0..40 {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) if matches!(e.raw_os_error(), Some(32) | Some(5)) => {
                last = Some(e);
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| io::Error::other("rename failed")))
}

#[cfg(not(windows))]
fn finalize_impl(temp: &Path, target: &Path, backup: bool) -> Result<(), String> {
    if let Ok(meta) = fs::metadata(target) {
        let _ = fs::set_permissions(temp, meta.permissions());
        if backup {
            let bak = backup_path_for(target);
            let _ = fs::remove_file(&bak);
            if fs::hard_link(target, &bak).is_err() {
                fs::copy(target, &bak).map_err(|e| describe(&e, &bak))?;
            }
        }
    }
    fs::rename(temp, target).map_err(|e| describe(&e, target))?;
    if let Some(dir) = target.parent() {
        if let Ok(d) = File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::{Encoding, LineEnding};
    use crate::file::{OpenMode, Source};
    use crate::index;
    use crate::search::Query;
    use std::sync::Arc;

    fn open(path: &Path, codec: Codec) -> TextBuf {
        let src = Arc::new(Source::open(path, OpenMode::Auto).unwrap());
        let idx = index::build_blocking(&src, codec);
        let mut t = TextBuf::new(src, idx, codec);
        t.blocking = true;
        t
    }

    #[test]
    fn save_roundtrip_with_edits_and_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        fs::write(&path, b"hello world\n").unwrap();
        let mut t = open(&path, Codec::utf8());
        t.replace(0..5, b"HELLO");
        let (temp, _) = write_temp(&t, &path, Transform::Identity, &CancelToken::new(), &Progress::default()).unwrap();
        drop(t); // release the mapping before replacing (required on Windows)
        finalize(&temp, &path, true).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"HELLO world\n");
        assert_eq!(fs::read(backup_path_for(&path)).unwrap(), b"hello world\n");
        assert!(!temp.exists());
    }

    #[test]
    fn cancelled_save_leaves_original() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("b.txt");
        fs::write(&path, b"original").unwrap();
        let t = open(&path, Codec::utf8());
        let c = CancelToken::new();
        c.cancel();
        assert!(matches!(write_temp(&t, &path, Transform::Identity, &c, &Progress::default()), Err(SaveError::Cancelled)));
        drop(t);
        assert_eq!(fs::read(&path).unwrap(), b"original");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1, "temp file left behind");
    }

    #[test]
    fn eol_conversion() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.txt");
        fs::write(&path, b"a\r\nb\nc\rd\r\n").unwrap();
        let t = open(&path, Codec::utf8());
        let (temp, _) = write_temp(&t, &path, Transform::Eol(LineEnding::Lf), &CancelToken::new(), &Progress::default()).unwrap();
        assert_eq!(fs::read(&temp).unwrap(), b"a\nb\nc\rd\n");
        let (temp2, _) = write_temp(&t, &path, Transform::Eol(LineEnding::CrLf), &CancelToken::new(), &Progress::default()).unwrap();
        assert_eq!(fs::read(&temp2).unwrap(), b"a\r\nb\r\nc\rd\r\n");
        let u16c = Codec::new(Encoding::Utf16Le, crate::encoding::NlByte::Lf, 0);
        let p2 = dir.path().join("d.txt");
        fs::write(&p2, u16c.encode("x\r\ny\nz")).unwrap();
        let t2 = open(&p2, u16c);
        let (temp3, _) = write_temp(&t2, &p2, Transform::Eol(LineEnding::CrLf), &CancelToken::new(), &Progress::default()).unwrap();
        assert_eq!(fs::read(&temp3).unwrap(), u16c.encode("x\r\ny\r\nz"));
    }

    #[test]
    fn streaming_replace_to_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("e.txt");
        fs::write(&path, b"one two one two").unwrap();
        let t = open(&path, Codec::utf8());
        let q = Query { pattern: "one".into(), case_sensitive: true, ..Default::default() };
        let m = Matcher::new(&q, Codec::utf8()).unwrap();
        let tpl = Template::new("1", false);
        let (temp, stats) = write_temp(&t, &path, Transform::Replace { matcher: &m, template: &tpl, range: 0..t.len() }, &CancelToken::new(), &Progress::default()).unwrap();
        assert_eq!(stats.count, 2);
        drop(t);
        finalize(&temp, &path, false).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"1 two 1 two");
        assert!(!backup_path_for(&path).exists());
    }

    #[test]
    fn delete_columns_on_disk_with_edits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.csv");
        fs::write(&path, b"id,name,age\n1,ann,30\n2,bob,41\n").unwrap();
        let mut t = open(&path, Codec::utf8());
        t.replace(12..13, b"7"); // unsaved edit: id 1 -> 7
        let cols = [1];
        let (temp, stats) = write_temp(&t, &path, Transform::DeleteColumns { dialect: Dialect::new(b','), cols: &cols }, &CancelToken::new(), &Progress::default()).unwrap();
        assert_eq!(stats.count, 3);
        drop(t);
        finalize(&temp, &path, true).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"id,age\n7,30\n2,41\n");
        assert_eq!(fs::read(backup_path_for(&path)).unwrap(), b"id,name,age\n1,ann,30\n2,bob,41\n");
    }

    #[test]
    fn transcode_between_encodings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.txt");
        let u16 = Codec::new(Encoding::Utf16Le, crate::encoding::NlByte::Lf, 2);
        let mut data = vec![0xFF, 0xFE];
        data.extend(u16.encode("Привет\r\nмир € 😀\r\n"));
        fs::write(&path, &data).unwrap();
        let t = open(&path, u16);
        let run = |enc, bom, eol| {
            let mut out = Vec::new();
            let s = transcode_to(&t, enc, bom, eol, &mut out, &CancelToken::new(), None).unwrap().unwrap();
            (out, s.count)
        };
        assert_eq!(run(Encoding::Utf8, false, None), ("Привет\r\nмир € 😀\r\n".as_bytes().to_vec(), 0));
        let (with_bom, _) = run(Encoding::Utf8, true, Some(LineEnding::Lf));
        assert_eq!(with_bom, [&[0xEF, 0xBB, 0xBF][..], "Привет\nмир € 😀\n".as_bytes()].concat());
        // Windows-1251 has the euro sign (0x88) but no emoji: one '?'.
        let (cp, missing) = run(Encoding::Windows1251, false, None);
        assert_eq!(missing, 1);
        assert_eq!(Codec::new(Encoding::Windows1251, crate::encoding::NlByte::Lf, 0).decode_lossy(&cp), "Привет\r\nмир € ?\r\n");
        // Through the save protocol, with a CRLF split across steps never doubled.
        let (temp, stats) = write_temp(&t, &path, Transform::Transcode { enc: Encoding::Utf16Be, bom: true, eol: Some(LineEnding::CrLf) }, &CancelToken::new(), &Progress::default()).unwrap();
        let be = Codec::new(Encoding::Utf16Be, crate::encoding::NlByte::Lf, 2);
        assert_eq!(fs::read(&temp).unwrap(), [&[0xFE, 0xFF][..], &be.encode("Привет\r\nмир € 😀\r\n")].concat());
        assert_eq!(stats.count, 0);
        // A CRLF across the step boundary becomes exactly one LF.
        let p2 = dir.path().join("u.txt");
        let mut big = vec![b'a'; TRANSCODE_STEP as usize - 1];
        big.extend_from_slice(b"\r\nz");
        fs::write(&p2, &big).unwrap();
        let t2 = open(&p2, Codec::utf8());
        let mut out = Vec::new();
        transcode_to(&t2, Encoding::Utf8, false, Some(LineEnding::Lf), &mut out, &CancelToken::new(), None).unwrap().unwrap();
        assert_eq!(&out[out.len() - 3..], b"a\nz");
        assert_eq!(out.len(), big.len() - 1);
    }

    #[test]
    fn save_as_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("src.txt");
        fs::write(&path, b"data").unwrap();
        let t = open(&path, Codec::utf8());
        let target = dir.path().join("новый файл.txt");
        let (temp, _) = write_temp(&t, &target, Transform::Identity, &CancelToken::new(), &Progress::default()).unwrap();
        finalize(&temp, &target, false).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"data");
        assert_eq!(fs::read(&path).unwrap(), b"data");
    }
}
