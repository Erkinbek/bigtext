//! A file's figures in one streaming pass (File → Properties, `bigtext-cli stats`): lines, line
//! breaks by kind, the longest line, NUL bytes, bytes that are not UTF-8, a missing final line
//! break, the first and last time; and its SHA-256.

use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::encoding::{Encoding, NlByte};
use crate::jobs::{CancelToken, Progress, Throttle};
use crate::lines::LineReader;
use crate::text::TextBuf;
use crate::timestamp::{self, Stamp};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Bytes looked at so far.
    pub scanned: u64,
    /// Lines (a last line without a line break counts).
    pub lines: u64,
    pub lf: u64,
    pub crlf: u64,
    pub cr: u64,
    /// The longest line: its length in bytes (without its line break), 0-based number and offset.
    pub longest: (u64, u64, u64),
    pub nul: u64,
    pub first_nul: Option<u64>,
    /// Bytes in sequences that are not UTF-8 (UTF-8 files only).
    pub invalid_utf8: u64,
    pub first_invalid: Option<u64>,
    /// The text ends with a line break.
    pub final_break: bool,
    pub done: bool,
}

/// The walk's state between chunks.
struct Walk {
    s: Stats,
    nl: NlByte,
    unit: u64,
    line_start: u64,
    /// A carriage return waiting to see whether a line feed follows.
    pending_cr: Option<u64>,
    /// The last code unit seen.
    last: u16,
    /// The start of an unfinished UTF-8 sequence at the end of the last chunk.
    carry: Vec<u8>,
    carry_at: u64,
}

impl Walk {
    fn end_line(&mut self, at: u64, cr_before: bool) {
        let len = (at - self.line_start).saturating_sub(if cr_before { self.unit } else { 0 });
        if len > self.s.longest.0 {
            self.s.longest = (len, self.s.lines, self.line_start);
        }
        self.s.lines += 1;
        self.line_start = at + self.unit;
    }

    /// A line feed or carriage return at `at`.
    fn mark(&mut self, c: u16, at: u64) {
        if let Some(p) = self.pending_cr.take() {
            if c == 0x0A && at == p + self.unit {
                self.s.crlf += 1;
                if self.nl == NlByte::Lf {
                    self.end_line(at, true);
                }
                return;
            }
            self.s.cr += 1;
        }
        if c == 0x0D {
            self.pending_cr = Some(at);
            if self.nl == NlByte::Cr {
                self.end_line(at, false);
            }
        } else {
            self.s.lf += 1;
            if self.nl == NlByte::Lf {
                self.end_line(at, false);
            }
        }
    }

    fn bad(&mut self, n: u64, at: u64) {
        self.s.invalid_utf8 += n;
        self.s.first_invalid.get_or_insert(at);
    }

    fn utf8(&mut self, mut b: &[u8], mut at: u64) {
        // First the character the last chunk ended in the middle of.
        if !self.carry.is_empty() {
            let have = self.carry.len();
            let mut tmp = self.carry.clone();
            tmp.extend_from_slice(&b[..b.len().min(3)]);
            let whole = match self.carry[0] {
                0xC0..=0xDF => 2,
                0xE0..=0xEF => 3,
                _ => 4,
            };
            let used = match std::str::from_utf8(&tmp) {
                Ok(_) => whole - have,
                Err(e) if e.valid_up_to() >= have => whole - have,
                Err(e) => match e.error_len() {
                    Some(n) => {
                        let at = self.carry_at;
                        self.bad(n as u64, at);
                        n.saturating_sub(have)
                    }
                    // Still not whole: the chunk was tiny.
                    None => {
                        self.carry.extend_from_slice(b);
                        return;
                    }
                },
            };
            let used = used.min(b.len());
            b = &b[used..];
            at += used as u64;
            self.carry.clear();
        }
        loop {
            match std::str::from_utf8(b) {
                Ok(_) => return,
                Err(e) => {
                    let ok = e.valid_up_to();
                    match e.error_len() {
                        Some(n) => {
                            self.bad(n as u64, at + ok as u64);
                            b = &b[ok + n..];
                            at += (ok + n) as u64;
                        }
                        None => {
                            self.carry = b[ok..].to_vec();
                            self.carry_at = at + ok as u64;
                            return;
                        }
                    }
                }
            }
        }
    }

    fn chunk(&mut self, base: u64, b: &[u8], enc: Encoding) {
        if self.unit == 1 {
            for i in memchr::memchr2_iter(b'\n', b'\r', b) {
                self.mark(b[i] as u16, base + i as u64);
            }
            let nul = crate::encoding::count_byte(b, 0);
            if nul > 0 {
                self.s.nul += nul;
                if self.s.first_nul.is_none() {
                    self.s.first_nul = memchr::memchr(0, b).map(|i| base + i as u64);
                }
            }
            if enc == Encoding::Utf8 {
                self.utf8(b, base);
            }
            if let Some(&c) = b.last() {
                self.last = c as u16;
            }
        } else {
            let be = enc == Encoding::Utf16Be;
            for (k, u) in b.chunks_exact(2).enumerate() {
                let c = if be { u16::from_be_bytes([u[0], u[1]]) } else { u16::from_le_bytes([u[0], u[1]]) };
                let at = base + 2 * k as u64;
                match c {
                    0x0A | 0x0D => self.mark(c, at),
                    0 => {
                        self.s.nul += 1;
                        self.s.first_nul.get_or_insert(at);
                    }
                    _ => {}
                }
                self.last = c;
            }
        }
        self.s.scanned = base + b.len() as u64;
    }

    fn finish(mut self, len: u64, start: u64) -> Stats {
        if self.pending_cr.take().is_some() {
            self.s.cr += 1;
        }
        if !self.carry.is_empty() {
            let (n, at) = (self.carry.len() as u64, self.carry_at);
            self.bad(n, at);
        }
        let nl = if self.nl == NlByte::Lf { 0x0A } else { 0x0D };
        self.s.final_break = len > start && self.last == nl;
        if len > start && !self.s.final_break {
            // The last line, without a line break.
            self.end_line(len, false);
        }
        self.s.scanned = len;
        self.s.done = true;
        self.s
    }
}

/// Walks the whole text once; `partial` gets the figures so far four times a second. `None`
/// when cancelled or the text could not be read.
pub fn scan(buf: &TextBuf, cancel: &CancelToken, progress: Option<&Progress>, mut partial: impl FnMut(&Stats)) -> Option<Stats> {
    let codec = buf.codec;
    let start = (codec.bom_len as u64).min(buf.len());
    let len = buf.len();
    if let Some(p) = progress {
        p.set_total(len);
    }
    let mut w = Walk { s: Stats::default(), nl: codec.nl, unit: codec.unit() as u64, line_start: start, pending_cr: None, last: 0, carry: Vec::new(), carry_at: 0 };
    let mut throttle = Throttle::new(Duration::from_millis(250));
    let mut stopped = false;
    buf.for_each_chunk(start..len, |base, b| {
        if cancel.is_cancelled() {
            stopped = true;
            return false;
        }
        w.chunk(base, b, codec.enc);
        if let Some(p) = progress {
            p.set_done(w.s.scanned);
        }
        if throttle.ready() {
            partial(&w.s);
        }
        true
    })?;
    if stopped {
        return None;
    }
    Some(w.finish(len, start))
}

/// The first and last time in the text: among its first 1,000 lines and its last 1 MB.
pub fn time_span(buf: &TextBuf) -> Option<(Stamp, Stamp)> {
    let codec = buf.codec;
    let mut first = None;
    let mut r = LineReader::new(buf);
    for _ in 0..1000 {
        let Ok(Some((_, l))) = r.next() else { break };
        if let Some(st) = timestamp::parse(&codec.decode_lossy(&l[..l.len().min(640)])) {
            first = Some(st);
            break;
        }
    }
    let from = buf.len().saturating_sub(1 << 20);
    let b = buf.read(from..buf.len())?;
    let text = codec.decode_lossy(&b);
    let last = text.lines().rev().take(1000).find_map(timestamp::parse)?;
    Some((first?, last))
}

/// The SHA-256 of the text's bytes (the file's, while it has no unsaved edits), in hex. `None`
/// when cancelled.
pub fn sha256(buf: &TextBuf, cancel: &CancelToken, progress: Option<&Progress>) -> Option<String> {
    let mut h = Sha256::new();
    if let Some(p) = progress {
        p.set_total(buf.len());
    }
    let mut stopped = false;
    buf.for_each_chunk(0..buf.len(), |base, b| {
        if cancel.is_cancelled() {
            stopped = true;
            return false;
        }
        h.update(b);
        if let Some(p) = progress {
            p.set_done(base + b.len() as u64);
        }
        true
    })?;
    if stopped {
        return None;
    }
    Some(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::Codec;
    use crate::file::{OpenMode, Source};
    use crate::index::LineIndex;
    use std::io::Write;
    use std::sync::Arc;

    fn buf(data: &[u8]) -> (tempfile::NamedTempFile, TextBuf) {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(data).unwrap();
        f.flush().unwrap();
        let s = Arc::new(Source::open(f.path(), OpenMode::Auto).unwrap());
        let det = crate::encoding::detect(data, true);
        (f, TextBuf::new(s.clone(), LineIndex::new(s.len()), det.codec))
    }

    fn stats(data: &[u8]) -> Stats {
        let (_f, b) = buf(data);
        scan(&b, &CancelToken::new(), None, |_| {}).unwrap()
    }

    /// As UTF-8 whatever it looks like.
    fn stats_utf8(data: &[u8]) -> Stats {
        let (f, _) = buf(data);
        let s = Arc::new(Source::open(f.path(), OpenMode::Auto).unwrap());
        let b = TextBuf::new(s.clone(), LineIndex::new(s.len()), Codec::utf8());
        scan(&b, &CancelToken::new(), None, |_| {}).unwrap()
    }

    #[test]
    fn breaks_longest_line_nul_and_utf8() {
        let s = stats_utf8(b"one\r\ntwo\nlonger line\rx\0y\n\xFFend");
        assert_eq!((s.lines, s.lf, s.crlf, s.cr), (4, 2, 1, 1));
        assert_eq!(s.longest, (15, 2, 9));
        assert_eq!((s.nul, s.first_nul), (1, Some(22)));
        assert_eq!((s.invalid_utf8, s.first_invalid, s.final_break), (1, Some(25), false));
        let s = stats_utf8("ü\n€\n".as_bytes());
        assert_eq!((s.lines, s.invalid_utf8, s.final_break, s.longest.0), (2, 0, true, 3));
        assert_eq!(stats(b"").lines, 0);
    }

    #[test]
    fn chunk_edges() {
        // Chunks are 4 MB: put a CRLF and a three-byte character across the first edge, and a
        // bad byte across the second.
        let edge = 4 * 1024 * 1024;
        let mut data = vec![b'a'; edge - 1];
        data.extend_from_slice(b"\r\n");
        data.extend(std::iter::repeat_n(b'b', edge - 2));
        data.extend_from_slice("€".as_bytes());
        data.extend_from_slice(b"\n");
        data.extend(std::iter::repeat_n(b'c', edge - 4));
        data.extend_from_slice(b"\xE2\x82zz\n");
        let s = stats_utf8(&data);
        assert_eq!((s.lines, s.crlf, s.lf, s.cr), (3, 1, 2, 0));
        assert_eq!((s.invalid_utf8, s.first_invalid), (2, Some(3 * edge as u64 - 1)));
        assert_eq!(s.longest, (edge as u64 + 1, 1, edge as u64 + 1));
    }

    #[test]
    fn utf16_and_hash() {
        let text: Vec<u8> = [0xFF, 0xFE].into_iter().chain("a\r\nbc\n\0d".encode_utf16().flat_map(|u| u.to_le_bytes())).collect();
        let s = stats(&text);
        assert_eq!((s.lines, s.crlf, s.lf, s.nul, s.longest.0, s.final_break), (3, 1, 1, 1, 4, false));
        let (_f, b) = buf(b"abc");
        assert_eq!(sha256(&b, &CancelToken::new(), None).unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
