//! Cutting a document into pieces: the byte range of a line range, and splitting the whole
//! document into part files of a given size or line count, always at line boundaries. Both
//! stream, so any file size works.

use crate::decompress::{Kind, Packer};
use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};

use crate::jobs::{CancelToken, Progress};
use crate::text::TextBuf;

const STEP: u64 = 16 << 20;

/// Exact start of 0-based `line`: from the line index when it already covers it, otherwise by
/// counting line breaks from the start. `None` when the line does not exist or on cancel.
pub fn exact_line_start(buf: &TextBuf, line: u64, cancel: &CancelToken, progress: Option<&Progress>) -> Option<u64> {
    if line == 0 {
        return Some(0);
    }
    if let Some((off, true)) = buf.line_start(line) {
        return Some(off);
    }
    let codec = buf.codec;
    let unit = codec.unit() as u64;
    let len = buf.len();
    let mut seen = 0u64;
    let mut pos = 0u64;
    while pos < len {
        if cancel.is_cancelled() {
            return None;
        }
        let end = (pos + STEP).min(len);
        let b = buf.read(pos..end)?;
        let n = codec.count_nl(&b, pos);
        if seen + n >= line {
            let i = codec.nth_nl(&b, pos, line - seen)?;
            return Some(pos + i as u64 + unit);
        }
        seen += n;
        if let Some(p) = progress {
            p.set_done(end);
        }
        pos = end;
    }
    None
}

/// Byte range of the 1-based, inclusive line range `from..=to` (including the last line's
/// break). `None` when `from` is past the end or on cancel.
pub fn line_range(buf: &TextBuf, from: u64, to: u64, cancel: &CancelToken, progress: Option<&Progress>) -> Option<Range<u64>> {
    if let Some(p) = progress {
        p.set_total(buf.len());
    }
    let start = exact_line_start(buf, from.max(1) - 1, cancel, progress)?;
    let end = exact_line_start(buf, to.max(from), cancel, progress);
    if cancel.is_cancelled() {
        return None;
    }
    Some(start.max(buf.codec.bom_len as u64)..end.unwrap_or(buf.len()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitBy {
    /// Parts of at least this many bytes (each ends at the first line break after the size).
    Bytes(u64),
    /// Parts of this many lines.
    Lines(u64),
}

/// `app.log` → `app.part001.log` in `dir`.
pub fn part_path(dir: &Path, name: &str, n: usize) -> PathBuf {
    let p = Path::new(name);
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| name.to_string());
    match p.extension() {
        Some(ext) => dir.join(format!("{stem}.part{n:03}.{}", ext.to_string_lossy())),
        None => dir.join(format!("{stem}.part{n:03}")),
    }
}

/// [`part_path`] with the compression's extension added (`app.part001.log.gz`).
pub fn packed_part_path(dir: &Path, name: &str, n: usize, pack: Option<Kind>) -> PathBuf {
    let p = part_path(dir, name, n);
    match pack {
        Some(k) => {
            let mut s = p.into_os_string();
            s.push(".");
            s.push(k.extension());
            PathBuf::from(s)
        }
        None => p,
    }
}

struct Parts<'a> {
    dir: &'a Path,
    name: &'a str,
    pack: Option<Kind>,
    bom: Vec<u8>,
    cur: Option<Packer<BufWriter<File>>>,
    made: Vec<PathBuf>,
    bytes: u64,
    lines: u64,
}

impl Parts<'_> {
    fn write(&mut self, b: &[u8]) -> io::Result<()> {
        if b.is_empty() {
            return Ok(());
        }
        if self.cur.is_none() {
            let p = packed_part_path(self.dir, self.name, self.made.len() + 1, self.pack);
            let f = File::options().write(true).create_new(true).open(&p)?;
            self.made.push(p);
            let mut w = Packer::new(BufWriter::with_capacity(8 << 20, f), self.pack)?;
            w.write_all(&self.bom)?;
            self.cur = Some(w);
        }
        self.bytes += b.len() as u64;
        self.cur.as_mut().unwrap().write_all(b)
    }

    fn close(&mut self) -> io::Result<()> {
        self.bytes = 0;
        self.lines = 0;
        if let Some(w) = self.cur.take() {
            let mut w = w.finish()?;
            w.flush()?;
            w.get_ref().sync_all()?;
        }
        Ok(())
    }
}

/// Splits the document into part files in `dir` named after `name` (see [`part_path`]),
/// compressed with `pack` when given. A part never ends inside a line; documents with a byte
/// order mark get it at the start of every part. Returns the files written, or `None` when
/// cancelled (the parts are then removed, as they are on an error).
#[allow(clippy::too_many_arguments)]
pub fn split_to_files(buf: &TextBuf, by: SplitBy, dir: &Path, name: &str, pack: Option<Kind>, cancel: &CancelToken, progress: &Progress) -> io::Result<Option<Vec<PathBuf>>> {
    let codec = buf.codec;
    let unit = codec.unit() as u64;
    let bom = codec.bom_len as u64;
    let len = buf.len();
    progress.set_total(len);
    let mut parts = Parts { dir, name, pack, bom: buf.read(0..bom).map(|b| b.into_owned()).unwrap_or_default(), cur: None, made: Vec::new(), bytes: 0, lines: 0 };
    let r = (|| -> io::Result<bool> {
        let mut pos = bom;
        while pos < len {
            if cancel.is_cancelled() {
                return Ok(false);
            }
            let end = (pos + STEP).min(len);
            let b = buf.read(pos..end).ok_or_else(|| io::Error::other("Could not read the document"))?;
            let mut i = 0usize;
            while i < b.len() {
                let base = pos + i as u64;
                let rest = &b[i..];
                // Index into `rest` just past the line break that ends the current part, if any.
                let cut = match by {
                    SplitBy::Lines(n) => {
                        let want = n.max(1) - parts.lines;
                        match codec.nth_nl(rest, base, want) {
                            Some(k) => Some(k + unit as usize),
                            None => {
                                parts.lines += codec.count_nl(rest, base);
                                None
                            }
                        }
                    }
                    SplitBy::Bytes(n) => {
                        let from = n.saturating_sub(parts.bytes).saturating_sub(1).min(rest.len() as u64) as usize;
                        let from = from - (base as usize + from) % unit as usize;
                        codec.find_nl(&rest[from..], base + from as u64).map(|k| from + k + unit as usize)
                    }
                };
                match cut {
                    Some(k) => {
                        parts.write(&rest[..k])?;
                        parts.close()?;
                        i += k;
                    }
                    None => {
                        parts.write(rest)?;
                        i = b.len();
                    }
                }
            }
            progress.set_done(end);
            pos = end;
        }
        parts.close()?;
        Ok(true)
    })();
    match r {
        Ok(true) => Ok(Some(std::mem::take(&mut parts.made))),
        other => {
            drop(parts.cur.take());
            for p in &parts.made {
                let _ = fs::remove_file(p);
            }
            other.map(|_| None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::{Codec, Encoding, NlByte};
    use crate::file::{OpenMode, Source};
    use crate::index;
    use std::sync::Arc;

    fn buf_with(data: &[u8], codec: Codec) -> (tempfile::NamedTempFile, TextBuf) {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(data).unwrap();
        f.flush().unwrap();
        let src = Arc::new(Source::open(f.path(), OpenMode::Auto).unwrap());
        let idx = index::build_blocking(&src, codec);
        let mut t = TextBuf::new(src, idx, codec);
        t.blocking = true;
        (f, t)
    }

    #[test]
    fn line_ranges_with_and_without_the_index() {
        let (_f, mut t) = buf_with(b"l1\nl2\nl3\nl4", Codec::utf8());
        let c = CancelToken::new();
        assert_eq!(line_range(&t, 2, 3, &c, None), Some(3..9));
        assert_eq!(line_range(&t, 4, 9, &c, None), Some(9..11));
        assert_eq!(line_range(&t, 5, 6, &c, None), None);
        // An edit makes the counts inexact on a fresh buffer: the slow path must agree.
        t.replace(0..0, b"x\n");
        assert_eq!(line_range(&t, 3, 3, &c, None), Some(5..8));
    }

    fn split(data: &[u8], codec: Codec, by: SplitBy) -> Vec<Vec<u8>> {
        let (_f, t) = buf_with(data, codec);
        let dir = tempfile::tempdir().unwrap();
        let files = split_to_files(&t, by, dir.path(), "app.log", None, &CancelToken::new(), &Progress::default()).unwrap().unwrap();
        files.iter().map(|p| fs::read(p).unwrap()).collect()
    }

    #[test]
    fn split_by_lines_and_size() {
        let data = b"a\nbb\nccc\ndddd\ne";
        assert_eq!(split(data, Codec::utf8(), SplitBy::Lines(2)), vec![b"a\nbb\n".to_vec(), b"ccc\ndddd\n".to_vec(), b"e".to_vec()]);
        assert_eq!(split(data, Codec::utf8(), SplitBy::Lines(5)), vec![data.to_vec()]);
        // At least 4 bytes per part, ending at the next line break.
        assert_eq!(split(data, Codec::utf8(), SplitBy::Bytes(4)), vec![b"a\nbb\n".to_vec(), b"ccc\n".to_vec(), b"dddd\n".to_vec(), b"e".to_vec()]);
        assert_eq!(split(b"a\nb\n", Codec::utf8(), SplitBy::Lines(1)), vec![b"a\n".to_vec(), b"b\n".to_vec()]);
        assert_eq!(part_path(Path::new("d"), "app.log", 7), Path::new("d").join("app.part007.log"));
    }

    #[test]
    fn compressed_parts() {
        let data: Vec<u8> = (0..20_000).flat_map(|i: u32| format!("line {i}\n").into_bytes()).collect();
        let (_f, t) = buf_with(&data, Codec::utf8());
        let dir = tempfile::tempdir().unwrap();
        let files = split_to_files(&t, SplitBy::Lines(7000), dir.path(), "app.log", Some(Kind::Zstd), &CancelToken::new(), &Progress::default()).unwrap().unwrap();
        assert_eq!(files.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect::<Vec<_>>(), ["app.part001.log.zst", "app.part002.log.zst", "app.part003.log.zst"]);
        let mut all = Vec::new();
        for p in &files {
            crate::decompress::unpack_to(p, Kind::Zstd, &mut all, &CancelToken::new(), &Progress::default()).unwrap().unwrap();
        }
        assert!(all == data);
    }

    #[test]
    fn split_utf16_keeps_the_bom_in_every_part() {
        let codec = Codec::new(Encoding::Utf16Le, NlByte::Lf, 2);
        let mut data = vec![0xFF, 0xFE];
        data.extend(codec.encode("one\ntwo\nthree\n"));
        let parts = split(&data, codec, SplitBy::Lines(2));
        let mut first = vec![0xFF, 0xFE];
        first.extend(codec.encode("one\ntwo\n"));
        let mut second = vec![0xFF, 0xFE];
        second.extend(codec.encode("three\n"));
        assert_eq!(parts, vec![first, second]);
    }

    #[test]
    fn existing_part_is_not_overwritten() {
        let (_f, t) = buf_with(b"a\nb\nc\n", Codec::utf8());
        let dir = tempfile::tempdir().unwrap();
        fs::write(part_path(dir.path(), "x.txt", 2), b"keep").unwrap();
        let r = split_to_files(&t, SplitBy::Lines(1), dir.path(), "x.txt", None, &CancelToken::new(), &Progress::default());
        assert!(r.is_err());
        assert!(!part_path(dir.path(), "x.txt", 1).exists(), "partial output removed");
        assert_eq!(fs::read(part_path(dir.path(), "x.txt", 2)).unwrap(), b"keep");
    }
}
