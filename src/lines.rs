//! Reading a text's lines front to back in large chunks, for jobs that look at every line
//! (merging logs, time histograms).

use std::io;

use crate::encoding::Encoding;
use crate::text::TextBuf;

/// Bytes read at a time.
pub const CHUNK: usize = 1 << 20;
/// Longer lines are cut into pieces of this size.
pub const MAX_LINE: usize = 16 << 20;

/// Where the last `n` lines of `buf` start (a final line break does not make an empty last
/// line), reading backwards from the end. `None` if the text could not be read.
pub fn last_lines_start(buf: &TextBuf, n: u64) -> Option<u64> {
    let codec = buf.codec;
    let unit = codec.unit() as u64;
    let start = (codec.bom_len as u64).min(buf.len());
    if n == 0 {
        return Some(buf.len());
    }
    let mut end = buf.len();
    // A line break at the very end ends the last line rather than starting another.
    if end >= start + unit {
        let tail = buf.read(end - unit..end)?;
        if codec.find_nl(&tail, end - unit).is_some() {
            end -= unit;
        }
    }
    let mut left = n;
    let mut hi = end;
    while hi > start {
        let lo = hi.saturating_sub(1 << 20).max(start);
        let lo = lo - (lo - start) % unit;
        let chunk = buf.read(lo..hi)?;
        let mut upto = chunk.len();
        while let Some(i) = codec.rfind_nl(&chunk[..upto], lo) {
            left -= 1;
            if left == 0 {
                return Some(lo + (i as u64) + unit);
            }
            upto = i;
        }
        hi = lo;
    }
    Some(start)
}

/// The lines of a text, in order, without their line breaks (and without a byte order mark).
pub struct LineReader<'a> {
    buf: &'a TextBuf,
    data: Vec<u8>,
    /// Consumed bytes at the start of `data`.
    at: usize,
    /// Offset of the end of `data`.
    end: u64,
    line: Vec<u8>,
    chunk: Vec<u8>,
    taken: u64,
}

impl<'a> LineReader<'a> {
    pub fn new(buf: &'a TextBuf) -> LineReader<'a> {
        let start = (buf.codec.bom_len as u64).min(buf.len());
        LineReader { buf, data: Vec::new(), at: 0, end: start, line: Vec::new(), chunk: Vec::new(), taken: 0 }
    }

    pub fn buf(&self) -> &'a TextBuf {
        self.buf
    }

    /// Bytes of the text taken so far (line breaks included).
    pub fn taken(&self) -> u64 {
        self.taken
    }

    /// The next line and its offset (`Ok(None)` at the end of the text).
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> io::Result<Option<(u64, &[u8])>> {
        let codec = self.buf.codec;
        let unit = codec.unit();
        loop {
            let base = self.end - (self.data.len() - self.at) as u64;
            let pending = &self.data[self.at..];
            let cut = match codec.find_nl(pending, base) {
                Some(i) if i <= MAX_LINE => Some((i, i + unit)),
                _ if pending.len() > MAX_LINE => {
                    let mut n = MAX_LINE - MAX_LINE % unit;
                    // Not inside a UTF-8 character.
                    while codec.enc == Encoding::Utf8 && n > 0 && pending[n] & 0xC0 == 0x80 {
                        n -= 1;
                    }
                    Some((n, n))
                }
                None if self.end >= self.buf.len() => (!pending.is_empty()).then_some((pending.len(), pending.len())),
                _ => None,
            };
            if let Some((content, taken)) = cut {
                self.line.clear();
                self.line.extend_from_slice(&pending[..content]);
                let cr = codec.cr_unit();
                if content < taken && self.line.ends_with(cr) {
                    self.line.truncate(self.line.len() - cr.len());
                }
                self.at += taken;
                self.taken += taken as u64;
                return Ok(Some((base, &self.line)));
            }
            if self.end >= self.buf.len() {
                return Ok(None);
            }
            self.data.drain(..self.at);
            self.at = 0;
            let to = (self.end + CHUNK as u64).min(self.buf.len());
            // `read_to` writes from the start of its buffer.
            let n = self.buf.read_to(self.end..to, &mut self.chunk).ok_or_else(|| io::Error::other("could not read the file"))?;
            self.data.extend_from_slice(&self.chunk[..n]);
            self.end = to;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding;
    use crate::file::{OpenMode, Source};
    use crate::index::LineIndex;
    use std::sync::Arc;

    fn lines(bytes: &[u8]) -> Vec<(u64, String)> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t");
        std::fs::write(&path, bytes).unwrap();
        let src = Arc::new(Source::open(&path, OpenMode::Auto).unwrap());
        let det = encoding::detect(&src.read(0..src.len()).unwrap(), true);
        let len = src.len();
        let buf = TextBuf::new(src, LineIndex::new(len), det.codec);
        let mut r = LineReader::new(&buf);
        let mut out = Vec::new();
        while let Some((off, l)) = r.next().unwrap() {
            out.push((off, buf.codec.decode_lossy(l)));
        }
        assert_eq!(r.taken() + buf.codec.bom_len as u64, len);
        out
    }

    #[test]
    fn offsets_line_breaks_and_bom() {
        assert_eq!(lines(b"a\r\nbb\n\nccc"), vec![(0, "a".into()), (3, "bb".into()), (6, "".into()), (7, "ccc".into())]);
        assert_eq!(lines(b"\xEF\xBB\xBFx\ny\n"), vec![(3, "x".into()), (5, "y".into())]);
        assert_eq!(lines(b""), vec![]);
        let utf16: Vec<u8> = [0xFF, 0xFE].into_iter().chain("п\r\nq".encode_utf16().flat_map(|u| u.to_le_bytes())).collect();
        assert_eq!(lines(&utf16), vec![(2, "п".into()), (8, "q".into())]);
    }

    #[test]
    fn lines_across_chunks() {
        let mut text = Vec::new();
        for i in 0..200_000 {
            text.extend_from_slice(format!("line {i}\n").as_bytes());
        }
        let got = lines(&text);
        assert_eq!(got.len(), 200_000);
        assert_eq!(got[123_456].1, "line 123456");
        let off = got[123_456].0 as usize;
        assert!(text[off..].starts_with(b"line 123456\n"));
    }
}
