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
use std::time::{SystemTime, UNIX_EPOCH};

use crate::csv::{self, Dialect};
use crate::encoding::{Codec, Encoding, LineEnding, NlByte, REPLACEMENT};
use crate::jobs::{CancelToken, Progress};
use crate::replace::{self, ReplaceStats, Template};
use crate::search::Matcher;
use crate::text::TextBuf;

const WRITE_BUF: usize = 8 * 1024 * 1024;

pub enum Transform<'a> {
    Identity,
    /// Convert line breaks to the given style; with `true`, a CR not followed by LF (in a file
    /// whose line breaks are LF) becomes a line break too instead of staying as it is. In a
    /// file whose line breaks are CR every CR is a line break, so the `bool` does not matter.
    Eol(LineEnding, bool),
    /// Replace every match in the range.
    Replace {
        matcher: &'a Matcher,
        template: &'a Template,
        range: std::ops::Range<u64>,
    },
    /// Delete the columns `cols` (sorted, distinct) of a CSV document.
    DeleteColumns {
        dialect: Dialect,
        cols: &'a [usize],
    },
    /// Re-encode into `enc` (with a byte order mark if `bom`), optionally converting line
    /// breaks (the `bool` as in [`Transform::Eol`]). `ReplaceStats::count` is the number of
    /// characters `enc` cannot represent and of sequences not valid in the document's own
    /// encoding (both written as '?').
    Transcode {
        enc: Encoding,
        bom: bool,
        eol: Option<(LineEnding, bool)>,
    },
    /// Sort rows, swap or select columns of a CSV document.
    Csv {
        dialect: Dialect,
        rewrite: &'a csv::Rewrite,
    },
    /// Only the lines `keep` is true for (given a line's bytes without its line break).
    Lines {
        keep: &'a dyn Fn(&[u8]) -> bool,
    },
    /// A text operation (case, trimming, indentation, JSON format) over `range`.
    TextOp {
        op: crate::textops::Op,
        range: std::ops::Range<u64>,
    },
    /// A column of cells filled from the line at `from` to the end.
    ColumnEdit {
        from: u64,
        cells: (u32, u32),
        gen: &'a crate::column_edit::ColumnGen,
    },
    /// The lines sorted (or unique) through temporary runs, each ending with `eol`.
    Sort {
        spec: crate::extsort::SortSpec,
        eol: &'a [u8],
    },
}

const TRANSCODE_STEP: u64 = 4 * 1024 * 1024;

/// Streams `buf` re-encoded into `enc`. Pieces are cut at character boundaries, and a CR at
/// the end of a piece waits for the next one so a CRLF is never split.
///
/// With `eol`, line breaks become that style; with its `true`, a CR not followed by LF (in a
/// file whose line breaks are LF) becomes a line break too instead of staying as it is, as
/// [`Transform::Eol`] does. In a file whose line breaks are CR every CR is a line break.
pub fn transcode_to<W: Write>(
    buf: &TextBuf,
    enc: Encoding,
    bom: bool,
    eol: Option<(LineEnding, bool)>,
    out: &mut W,
    cancel: &CancelToken,
    progress: Option<&Progress>,
) -> io::Result<Option<ReplaceStats>> {
    let target = Codec::new(enc, NlByte::Lf, 0);
    let len = buf.len();
    let mut stats = ReplaceStats {
        count: 0,
        bytes_in: len,
        bytes_out: 0,
    };
    let mut emit = |b: &[u8], stats: &mut ReplaceStats| -> io::Result<()> {
        stats.bytes_out += b.len() as u64;
        out.write_all(b)
    };
    if bom {
        emit(enc.bom(), &mut stats)?;
    }
    let breaks =
        eol.map(|(e, lone_cr)| (target.line_break(e), lone_cr || buf.codec.nl == NlByte::Cr));
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
        let raw = buf
            .read(pos..end)
            .ok_or_else(|| io::Error::other("Could not read the document"))?;
        let (mut text, invalid) = decode_marking_invalid(&buf.codec, &raw);
        stats.count += invalid;
        if let Some((nl, lone_breaks)) = &breaks {
            if pending_cr {
                text.insert(0, '\r');
            }
            pending_cr = end < len && text.ends_with('\r');
            if pending_cr {
                text.pop();
            }
            let mut normalized = text.replace("\r\n", "\n");
            if *lone_breaks {
                normalized = normalized.replace('\r', "\n");
            }
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
        if let Some((nl, _)) = &breaks {
            emit(nl, &mut stats)?;
        }
    }
    Ok(Some(stats))
}

/// Decodes `raw`, writing each sequence that is not valid in `codec` (which the view shows as
/// U+FFFD) as '?', and returns the text with how many such sequences there were. A U+FFFD that
/// is really in the text stays one.
fn decode_marking_invalid(codec: &Codec, raw: &[u8]) -> (String, u64) {
    let text = codec.decode_lossy(raw);
    if !text.contains(REPLACEMENT) {
        return (text, 0);
    }
    let mut out = String::with_capacity(text.len());
    let mut invalid = 0;
    if codec.enc == Encoding::Utf8 {
        for chunk in raw.utf8_chunks() {
            out.push_str(chunk.valid());
            if !chunk.invalid().is_empty() {
                out.push('?');
                invalid += 1;
            }
        }
    } else {
        let literal = codec.encode(&REPLACEMENT.to_string());
        let mut i = 0;
        while i < raw.len() {
            let (c, l) = codec.decode_char(&raw[i..]);
            let l = l.min(raw.len() - i);
            if c == REPLACEMENT && raw[i..i + l] != literal[..] {
                out.push('?');
                invalid += 1;
            } else {
                out.push(c);
            }
            i += l;
        }
    }
    (out, invalid)
}

fn count_and_encode(target: &Codec, text: &str, stats: &mut ReplaceStats) -> Vec<u8> {
    if !target.can_encode(text) {
        stats.count += text
            .chars()
            .filter(|c| !target.can_encode(c.encode_utf8(&mut [0; 4])))
            .count() as u64;
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
    let dir = target
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    dir.join(format!(
        ".{name}.{}-{nanos:x}.bigtext.tmp",
        std::process::id()
    ))
}

/// Creates `temp` (from [`temp_path_for`]) for writing; it must not exist yet. On Unix, when
/// `target` exists, the new file takes its permission bits before anything is written, so the
/// text of a private file is never readable by others while it is saved. (On Windows
/// `ReplaceFileW` gives the replacement the original's attributes and ACLs.)
pub fn create_temp(temp: &Path, target: &Path) -> io::Result<File> {
    let mut options = File::options();
    options.write(true).create_new(true);
    #[cfg(unix)]
    let original = fs::metadata(target).ok().map(|m| m.permissions());
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        if original.is_some() {
            // The owner only, until the original's bits are set below.
            options.mode(0o600);
        }
    }
    #[cfg(not(unix))]
    let _ = target;
    let file = options.open(temp)?;
    #[cfg(unix)]
    {
        if let Some(perm) = original {
            // As in `finalize`: a file system without Unix permissions (FAT, some network
            // shares) does not stop the save.
            let _ = file.set_permissions(perm);
        }
    }
    Ok(file)
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
        Some(32) | Some(33) => {
            format!("“{p}” is in use by another process. Close it there and try again.")
        }
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
    /// A CR without LF after it becomes `target` too.
    lone_cr: bool,
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
                    } else if self.codec.nl == NlByte::Lf && !self.lone_cr {
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
                    } else if self.codec.nl == NlByte::Lf && !self.lone_cr {
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
            if self.codec.nl == NlByte::Lf && !self.lone_cr {
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
pub fn write_temp(
    buf: &TextBuf,
    target: &Path,
    transform: Transform,
    cancel: &CancelToken,
    progress: &Progress,
) -> Result<(PathBuf, ReplaceStats), SaveError> {
    let temp = temp_path_for(target);
    let file = create_temp(&temp, target).map_err(|e| SaveError::Io(describe(&e, target)))?;
    let result = (|| -> Result<ReplaceStats, SaveError> {
        let mut w = BufWriter::with_capacity(WRITE_BUF, &file);
        let len = buf.len();
        progress.set_total(len);
        let io = |e: io::Error| SaveError::Io(describe(&e, target));
        let stats = match transform {
            Transform::Replace {
                matcher,
                template,
                range,
            } => {
                match replace::replace_all_to(
                    buf,
                    matcher,
                    template,
                    range,
                    &mut w,
                    cancel,
                    Some(progress),
                )
                .map_err(io)?
                {
                    Some(s) => s,
                    None => return Err(SaveError::Cancelled),
                }
            }
            Transform::DeleteColumns { dialect, cols } => {
                match csv::delete_columns_to(buf, dialect, cols, &mut w, cancel, Some(progress))
                    .map_err(io)?
                {
                    Some(s) => s,
                    None => return Err(SaveError::Cancelled),
                }
            }
            Transform::Csv { dialect, rewrite } => {
                match csv::rewrite_to(buf, dialect, rewrite, &mut w, cancel, Some(progress))
                    .map_err(io)?
                {
                    Some(s) => s,
                    None => return Err(SaveError::Cancelled),
                }
            }
            Transform::TextOp { op, range } => match crate::textops::text_op_to(
                buf,
                range,
                op,
                crate::text::rows::TAB_WIDTH,
                &mut w,
                cancel,
                Some(progress),
            ) {
                Ok(Some(n)) => ReplaceStats {
                    count: 0,
                    bytes_in: len,
                    bytes_out: n,
                },
                Ok(None) => return Err(SaveError::Cancelled),
                // The text's own problem (not JSON, not valid in its encoding), said as it is.
                Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                    return Err(SaveError::Io(e.to_string()))
                }
                Err(e) => return Err(io(e)),
            },
            Transform::Sort { spec, eol } => {
                match crate::extsort::sort_to(buf, spec, eol, &mut w, cancel, Some(progress))
                    .map_err(io)?
                {
                    Some(n) => ReplaceStats {
                        count: n,
                        bytes_in: len,
                        bytes_out: 0,
                    },
                    None => return Err(SaveError::Cancelled),
                }
            }
            Transform::ColumnEdit { from, cells, gen } => match crate::column_edit::column_edit_to(
                buf,
                from,
                cells,
                gen,
                &mut w,
                cancel,
                Some(progress),
            )
            .map_err(io)?
            {
                Some(n) => ReplaceStats {
                    count: n,
                    bytes_in: len,
                    bytes_out: 0,
                },
                None => return Err(SaveError::Cancelled),
            },
            // The byte order mark stays (write_pred_lines_to writes it).
            Transform::Lines { keep } => match crate::filter::write_pred_lines_to(
                buf,
                crate::filter::FilterSpec::default(),
                keep,
                &mut w,
                cancel,
                Some(progress),
            )
            .map_err(io)?
            {
                Some(n) => ReplaceStats {
                    count: n.lines,
                    bytes_in: len,
                    bytes_out: n.bytes,
                },
                None => return Err(SaveError::Cancelled),
            },
            Transform::Transcode { enc, bom, eol } => {
                match transcode_to(buf, enc, bom, eol, &mut w, cancel, Some(progress))
                    .map_err(io)?
                {
                    Some(s) => s,
                    None => return Err(SaveError::Cancelled),
                }
            }
            Transform::Identity | Transform::Eol(..) => {
                let mut sink = match &transform {
                    Transform::Eol(style, lone_cr) => Sink::Eol(EolWriter {
                        out: &mut w,
                        codec: buf.codec,
                        // The style asked for: `line_break` of a CR file's own codec is always
                        // CR (what Enter types there), which would leave its breaks as they are.
                        target: Codec {
                            nl: NlByte::Lf,
                            ..buf.codec
                        }
                        .line_break(*style),
                        pending_cr: false,
                        lone_cr: *lone_cr,
                        written: 0,
                    }),
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
                        return Err(SaveError::Io(
                            "Could not read the document while saving.".into(),
                        ));
                    }
                    progress.set_done(done);
                    pos = end;
                }
                sink.finish().map_err(io)?;
                ReplaceStats {
                    count: 0,
                    bytes_in: len,
                    bytes_out: sink.written(),
                }
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
        fn ReplaceFileW(
            replaced: *const u16,
            replacement: *const u16,
            backup: *const u16,
            flags: u32,
            exclude: *mut std::ffi::c_void,
            reserved: *mut std::ffi::c_void,
        ) -> i32;
        fn GetLastError() -> u32;
    }
    fn wide(p: &Path) -> Vec<u16> {
        // `\\?\` turns off the normalisation of `/` and `..`, so do it first (a path like
        // E:/logs/x.log comes from Git Bash).
        let full = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
        let p = full.as_path();
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
    let bak = if backup {
        user_bak.clone()
    } else {
        internal_bak.clone()
    };
    if backup && bak.exists() {
        let _ = fs::remove_file(&bak);
    }
    let (wt, wr, wb) = (wide(target), wide(temp), wide(&bak));
    let mut last = 0u32;
    for attempt in 0..40 {
        // REPLACEFILE_IGNORE_MERGE_ERRORS = 2
        let ok = unsafe {
            ReplaceFileW(
                wt.as_ptr(),
                wr.as_ptr(),
                wb.as_ptr(),
                2,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
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
            32 | 5 | 1175 | 1224 if attempt < 39 => {
                std::thread::sleep(std::time::Duration::from_millis(50))
            }
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
                std::thread::sleep(std::time::Duration::from_millis(50));
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
        let (temp, _) = write_temp(
            &t,
            &path,
            Transform::Identity,
            &CancelToken::new(),
            &Progress::default(),
        )
        .unwrap();
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
        assert!(matches!(
            write_temp(&t, &path, Transform::Identity, &c, &Progress::default()),
            Err(SaveError::Cancelled)
        ));
        drop(t);
        assert_eq!(fs::read(&path).unwrap(), b"original");
        assert_eq!(
            fs::read_dir(dir.path()).unwrap().count(),
            1,
            "temp file left behind"
        );
    }

    #[test]
    fn eol_conversion() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.txt");
        fs::write(&path, b"a\r\nb\nc\rd\r\n").unwrap();
        let t = open(&path, Codec::utf8());
        let (temp, _) = write_temp(
            &t,
            &path,
            Transform::Eol(LineEnding::Lf, false),
            &CancelToken::new(),
            &Progress::default(),
        )
        .unwrap();
        assert_eq!(fs::read(&temp).unwrap(), b"a\nb\nc\rd\n");
        let (temp2, _) = write_temp(
            &t,
            &path,
            Transform::Eol(LineEnding::CrLf, false),
            &CancelToken::new(),
            &Progress::default(),
        )
        .unwrap();
        assert_eq!(fs::read(&temp2).unwrap(), b"a\r\nb\r\nc\rd\r\n");
        // Lone CRs become line breaks too when asked.
        let (temp_lone, _) = write_temp(
            &t,
            &path,
            Transform::Eol(LineEnding::CrLf, true),
            &CancelToken::new(),
            &Progress::default(),
        )
        .unwrap();
        assert_eq!(fs::read(&temp_lone).unwrap(), b"a\r\nb\r\nc\r\nd\r\n");
        let u16c = Codec::new(Encoding::Utf16Le, crate::encoding::NlByte::Lf, 0);
        let p2 = dir.path().join("d.txt");
        fs::write(&p2, u16c.encode("x\r\ny\nz")).unwrap();
        let t2 = open(&p2, u16c);
        let (temp3, _) = write_temp(
            &t2,
            &p2,
            Transform::Eol(LineEnding::CrLf, false),
            &CancelToken::new(),
            &Progress::default(),
        )
        .unwrap();
        assert_eq!(fs::read(&temp3).unwrap(), u16c.encode("x\r\ny\r\nz"));
    }

    /// "Convert to LF/CRLF" changes a file whose line breaks are CR (old Mac style): every CR
    /// is a line break there, whatever the lone-CR choice.
    #[test]
    fn eol_conversion_of_a_cr_file() {
        let dir = tempfile::tempdir().unwrap();
        let save = |t: &TextBuf, target: &Path, style, lone| {
            let (temp, _) = write_temp(
                t,
                target,
                Transform::Eol(style, lone),
                &CancelToken::new(),
                &Progress::default(),
            )
            .unwrap();
            let out = fs::read(&temp).unwrap();
            fs::remove_file(temp).unwrap();
            out
        };
        let path = dir.path().join("mac.txt");
        fs::write(&path, b"a\rb\r\rc").unwrap();
        let t = open(&path, Codec::new(Encoding::Utf8, NlByte::Cr, 0));
        for lone in [false, true] {
            assert_eq!(save(&t, &path, LineEnding::Lf, lone), b"a\nb\n\nc");
            assert_eq!(save(&t, &path, LineEnding::CrLf, lone), b"a\r\nb\r\n\r\nc");
            assert_eq!(save(&t, &path, LineEnding::Cr, lone), b"a\rb\r\rc");
        }
        // UTF-16, with a CR as the last character.
        let u16c = Codec::new(Encoding::Utf16Le, NlByte::Cr, 0);
        let p2 = dir.path().join("mac16.txt");
        fs::write(&p2, u16c.encode("x\ry\r")).unwrap();
        let t2 = open(&p2, u16c);
        assert_eq!(
            save(&t2, &p2, LineEnding::CrLf, false),
            u16c.encode("x\r\ny\r\n")
        );
        assert_eq!(save(&t2, &p2, LineEnding::Lf, false), u16c.encode("x\ny\n"));
    }

    #[test]
    fn only_some_lines_keeping_the_bom() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("l.txt");
        fs::write(&path, b"\xEF\xBB\xBFkeep 1\ndrop\nkeep 2\ndrop\nkeep 3").unwrap();
        let mut codec = Codec::utf8();
        codec.bom_len = 3;
        let t = open(&path, codec);
        let keep = |l: &[u8]| l.starts_with(b"keep");
        let (temp, stats) = write_temp(
            &t,
            &path,
            Transform::Lines { keep: &keep },
            &CancelToken::new(),
            &Progress::default(),
        )
        .unwrap();
        assert_eq!(stats.count, 3);
        drop(t);
        finalize(&temp, &path, false).unwrap();
        assert_eq!(
            fs::read(&path).unwrap(),
            b"\xEF\xBB\xBFkeep 1\nkeep 2\nkeep 3"
        );
    }

    #[test]
    fn streaming_replace_to_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("e.txt");
        fs::write(&path, b"one two one two").unwrap();
        let t = open(&path, Codec::utf8());
        let q = Query {
            pattern: "one".into(),
            case_sensitive: true,
            ..Default::default()
        };
        let m = Matcher::new(&q, Codec::utf8()).unwrap();
        let tpl = Template::new("1", false);
        let (temp, stats) = write_temp(
            &t,
            &path,
            Transform::Replace {
                matcher: &m,
                template: &tpl,
                range: 0..t.len(),
            },
            &CancelToken::new(),
            &Progress::default(),
        )
        .unwrap();
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
        let (temp, stats) = write_temp(
            &t,
            &path,
            Transform::DeleteColumns {
                dialect: Dialect::new(b','),
                cols: &cols,
            },
            &CancelToken::new(),
            &Progress::default(),
        )
        .unwrap();
        assert_eq!(stats.count, 3);
        drop(t);
        finalize(&temp, &path, true).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"id,age\n7,30\n2,41\n");
        assert_eq!(
            fs::read(backup_path_for(&path)).unwrap(),
            b"id,name,age\n1,ann,30\n2,bob,41\n"
        );
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
            let s = transcode_to(&t, enc, bom, eol, &mut out, &CancelToken::new(), None)
                .unwrap()
                .unwrap();
            (out, s.count)
        };
        assert_eq!(
            run(Encoding::Utf8, false, None),
            ("Привет\r\nмир € 😀\r\n".as_bytes().to_vec(), 0)
        );
        let (with_bom, _) = run(Encoding::Utf8, true, Some((LineEnding::Lf, false)));
        assert_eq!(
            with_bom,
            [&[0xEF, 0xBB, 0xBF][..], "Привет\nмир € 😀\n".as_bytes()].concat()
        );
        // Windows-1251 has the euro sign (0x88) but no emoji: one '?'.
        let (cp, missing) = run(Encoding::Windows1251, false, None);
        assert_eq!(missing, 1);
        assert_eq!(
            Codec::new(Encoding::Windows1251, crate::encoding::NlByte::Lf, 0).decode_lossy(&cp),
            "Привет\r\nмир € ?\r\n"
        );
        // Through the save protocol, with a CRLF split across steps never doubled.
        let (temp, stats) = write_temp(
            &t,
            &path,
            Transform::Transcode {
                enc: Encoding::Utf16Be,
                bom: true,
                eol: Some((LineEnding::CrLf, false)),
            },
            &CancelToken::new(),
            &Progress::default(),
        )
        .unwrap();
        let be = Codec::new(Encoding::Utf16Be, crate::encoding::NlByte::Lf, 2);
        assert_eq!(
            fs::read(&temp).unwrap(),
            [&[0xFE, 0xFF][..], &be.encode("Привет\r\nмир € 😀\r\n")].concat()
        );
        assert_eq!(stats.count, 0);
        // A CRLF across the step boundary becomes exactly one LF.
        let p2 = dir.path().join("u.txt");
        let mut big = vec![b'a'; TRANSCODE_STEP as usize - 1];
        big.extend_from_slice(b"\r\nz");
        fs::write(&p2, &big).unwrap();
        let t2 = open(&p2, Codec::utf8());
        let mut out = Vec::new();
        transcode_to(
            &t2,
            Encoding::Utf8,
            false,
            Some((LineEnding::Lf, false)),
            &mut out,
            &CancelToken::new(),
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(&out[out.len() - 3..], b"a\nz");
        assert_eq!(out.len(), big.len() - 1);
    }

    /// Bytes not valid in the document's encoding (shown as U+FFFD) are written as '?' and
    /// counted with the characters the target lacks; a U+FFFD really in the text is neither.
    #[test]
    fn transcode_counts_invalid_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.txt");
        // Two stray bytes, a cut-off sequence and a real U+FFFD.
        fs::write(&path, b"ok \xFF\xFE end \xC3( \xEF\xBF\xBD").unwrap();
        let t = open(&path, Codec::utf8());
        let run = |t: &TextBuf, enc| {
            let mut out = Vec::new();
            let s = transcode_to(t, enc, false, None, &mut out, &CancelToken::new(), None)
                .unwrap()
                .unwrap();
            (Codec::new(enc, NlByte::Lf, 0).decode_lossy(&out), s.count)
        };
        assert_eq!(
            run(&t, Encoding::Utf16Le),
            ("ok ?? end ?( \u{FFFD}".to_string(), 3)
        );
        assert_eq!(
            run(&t, Encoding::Utf8),
            ("ok ?? end ?( \u{FFFD}".to_string(), 3)
        );
        // Windows-1251 has no U+FFFD either: four '?', each counted once.
        assert_eq!(
            run(&t, Encoding::Windows1251),
            ("ok ?? end ?( ?".to_string(), 4)
        );
        // A lone surrogate in UTF-16.
        let u16c = Codec::new(Encoding::Utf16Le, NlByte::Lf, 0);
        let p2 = dir.path().join("bad16.txt");
        fs::write(
            &p2,
            [u16c.encode("a"), vec![0x00, 0xD8], u16c.encode("b\u{FFFD}")].concat(),
        )
        .unwrap();
        let t2 = open(&p2, u16c);
        assert_eq!(run(&t2, Encoding::Utf8), ("a?b\u{FFFD}".to_string(), 1));
        // Through the save protocol.
        let (temp, stats) = write_temp(
            &t,
            &path,
            Transform::Transcode {
                enc: Encoding::Utf16Be,
                bom: false,
                eol: None,
            },
            &CancelToken::new(),
            &Progress::default(),
        )
        .unwrap();
        assert_eq!(stats.count, 3);
        fs::remove_file(temp).unwrap();
    }

    /// The line-break choices of a save apply when it changes the encoding too: a lone CR in
    /// an LF file stays unless asked, and in a CR file every CR is a line break.
    #[test]
    fn transcode_converts_line_breaks_like_a_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mixed.txt");
        fs::write(&path, b"a\r\nb\nc\rd\r\n").unwrap();
        let t = open(&path, Codec::utf8());
        let u16c = Codec::new(Encoding::Utf16Le, NlByte::Lf, 0);
        let run = |t: &TextBuf, eol| {
            let mut out = Vec::new();
            transcode_to(
                t,
                Encoding::Utf16Le,
                false,
                Some(eol),
                &mut out,
                &CancelToken::new(),
                None,
            )
            .unwrap()
            .unwrap();
            u16c.decode_lossy(&out)
        };
        assert_eq!(run(&t, (LineEnding::Lf, false)), "a\nb\nc\rd\n");
        assert_eq!(run(&t, (LineEnding::CrLf, false)), "a\r\nb\r\nc\rd\r\n");
        assert_eq!(run(&t, (LineEnding::Lf, true)), "a\nb\nc\nd\n");
        assert_eq!(run(&t, (LineEnding::CrLf, true)), "a\r\nb\r\nc\r\nd\r\n");
        // The same as a save that keeps the encoding.
        let (temp, _) = write_temp(
            &t,
            &path,
            Transform::Eol(LineEnding::CrLf, false),
            &CancelToken::new(),
            &Progress::default(),
        )
        .unwrap();
        assert_eq!(fs::read(&temp).unwrap(), b"a\r\nb\r\nc\rd\r\n");
        fs::remove_file(temp).unwrap();
        // A file whose line breaks are CR.
        let p2 = dir.path().join("mac.txt");
        fs::write(&p2, b"x\ry\rz").unwrap();
        let mac = open(&p2, Codec::new(Encoding::Utf8, NlByte::Cr, 0));
        assert_eq!(run(&mac, (LineEnding::CrLf, false)), "x\r\ny\r\nz");
        // A lone CR at the end of a step waits for the next one and stays a CR.
        let p3 = dir.path().join("step.txt");
        let mut big = vec![b'a'; TRANSCODE_STEP as usize - 1];
        big.extend_from_slice(b"\rz\n");
        fs::write(&p3, &big).unwrap();
        let t3 = open(&p3, Codec::utf8());
        let mut out = Vec::new();
        transcode_to(
            &t3,
            Encoding::Utf8,
            false,
            Some((LineEnding::CrLf, false)),
            &mut out,
            &CancelToken::new(),
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(&out[out.len() - 5..], b"a\rz\r\n");
        assert_eq!(out.len(), big.len() + 1);
    }

    /// A save keeps a file's permission bits, and the temp file has them before any text is
    /// written to it (a private file is never readable by others while it is saved).
    #[cfg(unix)]
    #[test]
    fn temp_file_takes_the_original_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o7777;
        let set = |p: &Path, m: u32| fs::set_permissions(p, fs::Permissions::from_mode(m)).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("private.txt");
        fs::write(&path, b"secret\n").unwrap();
        // With an execute bit: a new file's default mode (0o666 less the umask) never has one,
        // so a temp file that did not take these bits cannot pass by chance.
        set(&path, 0o700);
        let temp = temp_path_for(&path);
        let f = create_temp(&temp, &path).unwrap();
        assert_eq!((mode(&temp), f.metadata().unwrap().len()), (0o700, 0));
        drop(f);
        fs::remove_file(&temp).unwrap();
        // Save (and CLI --in-place, which writes the same way).
        let mut t = open(&path, Codec::utf8());
        t.replace(0..6, b"SECRET");
        let (temp, _) = write_temp(
            &t,
            &path,
            Transform::Identity,
            &CancelToken::new(),
            &Progress::default(),
        )
        .unwrap();
        assert_eq!(mode(&temp), 0o700);
        finalize(&temp, &path, false).unwrap();
        assert_eq!(mode(&path), 0o700);
        assert_eq!(fs::read(&path).unwrap(), b"SECRET\n");
        // Save As over another existing file takes that file's bits.
        let other = dir.path().join("other.txt");
        fs::write(&other, b"old").unwrap();
        set(&other, 0o710);
        let (temp, _) = write_temp(
            &t,
            &other,
            Transform::Identity,
            &CancelToken::new(),
            &Progress::default(),
        )
        .unwrap();
        assert_eq!(mode(&temp), 0o710);
        finalize(&temp, &other, false).unwrap();
        assert_eq!(mode(&other), 0o710);
        // Save As to a new file keeps the default mode.
        let new = dir.path().join("new.txt");
        let (temp, _) = write_temp(
            &t,
            &new,
            Transform::Identity,
            &CancelToken::new(),
            &Progress::default(),
        )
        .unwrap();
        assert_eq!(mode(&temp) & 0o111, 0);
        fs::remove_file(&temp).unwrap();
    }

    #[test]
    fn save_as_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("src.txt");
        fs::write(&path, b"data").unwrap();
        let t = open(&path, Codec::utf8());
        let target = dir.path().join("новый файл.txt");
        let (temp, _) = write_temp(
            &t,
            &target,
            Transform::Identity,
            &CancelToken::new(),
            &Progress::default(),
        )
        .unwrap();
        finalize(&temp, &target, false).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"data");
        assert_eq!(fs::read(&path).unwrap(), b"data");
    }
}
