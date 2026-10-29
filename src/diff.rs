//! Streaming line diff of two (possibly huge) files with bounded memory.
//!
//! Identical stretches are skipped with fast chunk comparison (no per-line work). At a
//! difference both sides are rewound to the start of the differing line and the next lines are
//! read into a bounded look-ahead window as (hash, length, offset) records; the two sides are
//! re-synchronised at the earliest pair of positions where three consecutive lines agree (or
//! where both files end). Lines are compared by 64-bit hash plus length, so arbitrarily long
//! lines need no buffering. Differences that do not re-synchronise within the window are
//! reported as one hunk per window, so the pass always makes progress.

use std::collections::HashMap;
use std::ops::Range;

use crate::encoding::count_byte;
use crate::file::Source;
use crate::jobs::{CancelToken, Progress};
use crate::text::TextBuf;

/// What can be compared: a file, or a document with its unsaved edits.
pub trait LineSource: Sync {
    fn len(&self) -> u64;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Fills `out` with the bytes at `off`.
    fn read_into(&self, off: u64, out: &mut [u8]) -> Option<()>;
}

impl LineSource for Source {
    fn len(&self) -> u64 {
        Source::len(self)
    }

    fn read_into(&self, off: u64, out: &mut [u8]) -> Option<()> {
        Source::read_into(self, off, out).ok()
    }
}

/// Use a blocking snapshot ([`TextBuf::snapshot`]) on a worker.
impl LineSource for TextBuf {
    fn len(&self) -> u64 {
        TextBuf::len(self)
    }

    fn read_into(&self, off: u64, out: &mut [u8]) -> Option<()> {
        TextBuf::read_into(self, off, out)
    }
}

const CHUNK: usize = 4 * 1024 * 1024;
/// Look-ahead per side when re-synchronising.
const WINDOW_LINES: usize = 4096;
const WINDOW_BYTES: u64 = 16 * 1024 * 1024;
const ANCHOR: usize = 3;

/// A differing region: `a_count` lines of A starting at `a_line` were replaced by `b_count`
/// lines of B starting at `b_line` (0-based line numbers, byte offsets of the first line).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hunk {
    pub a_line: u64,
    pub a_count: u64,
    pub a_off: u64,
    /// The end of the hunk's lines in A (`a_off` when it has none).
    pub a_end: u64,
    pub b_line: u64,
    pub b_count: u64,
    pub b_off: u64,
    pub b_end: u64,
}

/// The line on the other side that corresponds to `line` on this one (`a_to_b`: from A to B):
/// the start of the hunk it is in, or the same distance past the last hunk before it.
pub fn map_line(hunks: &[Hunk], line: u64, a_to_b: bool) -> u64 {
    let side = |h: &Hunk| if a_to_b { (h.a_line, h.a_count, h.b_line, h.b_count) } else { (h.b_line, h.b_count, h.a_line, h.a_count) };
    let k = hunks.partition_point(|h| side(h).0 <= line);
    let Some(h) = k.checked_sub(1).map(|k| side(&hunks[k])) else { return line };
    let (from, n, to, m) = h;
    if line < from + n {
        to
    } else {
        to + m + (line - from - n)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffEnd {
    Completed,
    Stopped,
    Cancelled,
    Failed,
}

#[derive(Clone, Copy, Debug)]
struct Line {
    off: u64,
    len: u64,
    hash: u64,
    /// The length compared: of the line, or of its normalised form.
    key: u64,
}

/// What does not count as a difference.
#[derive(Clone, Debug, Default)]
pub struct DiffOptions {
    /// Spaces, tabs and carriage returns.
    pub whitespace: bool,
    /// Upper and lower case (ASCII).
    pub case: bool,
    /// The timestamp at the start of a line.
    pub timestamps: bool,
    /// Text matching this pattern (`req=[0-9a-f]+`).
    pub mask: Option<regex_automata::meta::Regex>,
}

impl DiffOptions {
    pub(crate) fn plain(&self) -> bool {
        !self.whitespace && !self.case && !self.timestamps && self.mask.is_none()
    }

    /// The line as it is compared.
    pub(crate) fn normalise(&self, line: &[u8]) -> Vec<u8> {
        let mut b: &[u8] = line;
        if self.timestamps {
            b = &b[crate::patterns::time_prefix(b)..];
        }
        let masked;
        if let Some(re) = &self.mask {
            let mut out = Vec::with_capacity(b.len());
            let mut at = 0;
            for m in re.find_iter(b) {
                out.extend_from_slice(&b[at..m.start()]);
                at = m.end();
            }
            out.extend_from_slice(&b[at..]);
            masked = out;
            b = &masked;
        }
        b.iter()
            .filter(|c| !(self.whitespace && matches!(c, b' ' | b'\t' | b'\r')))
            .map(|&c| if self.case { c.to_ascii_lowercase() } else { c })
            .collect()
    }
}

/// The hash of a line's bytes (its length mixed in), as comparisons use it.
pub fn line_hash(b: &[u8]) -> u64 {
    let mut h = LineHasher::new();
    h.update(b);
    h.finish() ^ b.len() as u64
}

/// Streaming 64-bit hash (FNV-1a over 8-byte words with a final mix).
#[derive(Clone, Copy)]
struct LineHasher {
    h: u64,
    tail: u64,
    tail_len: u32,
}

impl LineHasher {
    fn new() -> Self {
        LineHasher { h: 0xcbf2_9ce4_8422_2325, tail: 0, tail_len: 0 }
    }
    #[inline]
    fn word(&mut self, w: u64) {
        self.h = (self.h ^ w).wrapping_mul(0x0000_0100_0000_01b3).rotate_left(29);
    }
    fn update(&mut self, mut b: &[u8]) {
        while self.tail_len != 0 && !b.is_empty() {
            self.tail |= (b[0] as u64) << (8 * self.tail_len);
            self.tail_len += 1;
            b = &b[1..];
            if self.tail_len == 8 {
                let t = self.tail;
                self.word(t);
                self.tail = 0;
                self.tail_len = 0;
            }
        }
        let mut chunks = b.chunks_exact(8);
        for c in &mut chunks {
            self.word(u64::from_le_bytes(c.try_into().unwrap()));
        }
        for &x in chunks.remainder() {
            self.tail |= (x as u64) << (8 * self.tail_len);
            self.tail_len += 1;
        }
    }
    fn finish(mut self) -> u64 {
        if self.tail_len > 0 {
            let t = self.tail ^ (0xff << (8 * self.tail_len.min(7)));
            self.word(t);
        }
        let mut z = self.h;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

/// Sequential reader over a source with a reusable chunk buffer.
struct Reader<'a> {
    src: &'a dyn LineSource,
    pos: u64,
    line: u64,
    buf: Vec<u8>,
    buf_start: u64,
    buf_len: usize,
    opts: &'a DiffOptions,
    /// The line read so far, when lines are normalised.
    text: Vec<u8>,
}

impl<'a> Reader<'a> {
    fn new(src: &'a dyn LineSource, opts: &'a DiffOptions) -> Self {
        Reader { src, pos: 0, line: 0, buf: vec![0; CHUNK], buf_start: 0, buf_len: 0, opts, text: Vec::new() }
    }

    fn eof(&self) -> bool {
        self.pos >= self.src.len()
    }

    /// Bytes available at `pos` (refilling the buffer as needed).
    fn peek(&mut self) -> Option<&[u8]> {
        let len = self.src.len();
        if self.pos >= len {
            return Some(&[]);
        }
        if self.pos < self.buf_start || self.pos >= self.buf_start + self.buf_len as u64 {
            let n = ((len - self.pos) as usize).min(CHUNK);
            self.src.read_into(self.pos, &mut self.buf[..n])?;
            self.buf_start = self.pos;
            self.buf_len = n;
        }
        let s = (self.pos - self.buf_start) as usize;
        Some(&self.buf[s..self.buf_len])
    }

    fn advance(&mut self, n: u64, newlines: u64) {
        self.pos += n;
        self.line += newlines;
    }

    /// Reads the next line (including its '\n') as a hash record.
    fn next_line(&mut self) -> Option<Line> {
        if !self.opts.plain() {
            return self.next_normalised();
        }
        let off = self.pos;
        let mut h = LineHasher::new();
        let mut len = 0u64;
        loop {
            let chunk = self.peek()?;
            if chunk.is_empty() {
                break;
            }
            match memchr::memchr(b'\n', chunk) {
                Some(i) => {
                    h.update(&chunk[..=i]);
                    len += i as u64 + 1;
                    self.pos += i as u64 + 1;
                    self.line += 1;
                    return Some(Line { off, len, hash: h.finish() ^ len, key: len });
                }
                None => {
                    let n = chunk.len();
                    h.update(chunk);
                    len += n as u64;
                    self.pos += n as u64;
                }
            }
        }
        if len == 0 {
            None
        } else {
            Some(Line { off, len, hash: h.finish() ^ len, key: len })
        }
    }

    /// [`Self::next_line`] hashing the line's normalised form (lines over 1 MB as they are past
    /// that).
    fn next_normalised(&mut self) -> Option<Line> {
        let off = self.pos;
        let mut text = std::mem::take(&mut self.text);
        text.clear();
        let mut len = 0u64;
        loop {
            let Some(chunk) = self.peek() else {
                self.text = text;
                return None;
            };
            if chunk.is_empty() {
                break;
            }
            let (part, done) = match memchr::memchr(b'\n', chunk) {
                Some(i) => (i + 1, true),
                None => (chunk.len(), false),
            };
            if text.len() < (1 << 20) {
                let room = ((1 << 20) - text.len()).min(part);
                text.extend_from_slice(&chunk[..room]);
            }
            len += part as u64;
            self.pos += part as u64;
            if done {
                self.line += 1;
                break;
            }
        }
        let norm = self.opts.normalise(text.strip_suffix(b"\n").unwrap_or(&text));
        self.text = text;
        if len == 0 {
            return None;
        }
        let mut h = LineHasher::new();
        h.update(&norm);
        let key = norm.len() as u64;
        Some(Line { off, len, hash: h.finish() ^ key, key })
    }
}

/// Length of the common prefix of two byte slices.
fn common_prefix(a: &[u8], b: &[u8]) -> usize {
    let n = a.len().min(b.len());
    let mut i = 0;
    while i + 64 <= n && a[i..i + 64] == b[i..i + 64] {
        i += 64;
    }
    while i < n && a[i] == b[i] {
        i += 1;
    }
    i
}

fn read_window(r: &mut Reader) -> Option<Vec<Line>> {
    let mut v = Vec::new();
    let mut bytes = 0;
    while v.len() < WINDOW_LINES && bytes < WINDOW_BYTES {
        match r.next_line() {
            Some(l) => {
                bytes += l.len;
                v.push(l);
            }
            None if r.eof() => break,
            None => return None,
        }
    }
    Some(v)
}

/// Earliest (i, j) with `ANCHOR` agreeing lines (fewer at the end of both files), minimising i + j.
fn find_anchor(a: &[Line], b: &[Line], a_eof: bool, b_eof: bool) -> Option<(usize, usize)> {
    let mut first_in_b: HashMap<(u64, u64), Vec<usize>> = HashMap::with_capacity(b.len());
    for (j, l) in b.iter().enumerate() {
        first_in_b.entry((l.hash, l.key)).or_default().push(j);
    }
    let agree = |i: usize, j: usize| -> bool {
        for k in 0..ANCHOR {
            match (a.get(i + k), b.get(j + k)) {
                (Some(x), Some(y)) if x.hash == y.hash && x.key == y.key => {}
                // Both sides end together: the tail matches.
                (None, None) if a_eof && b_eof => return true,
                _ => return false,
            }
        }
        true
    };
    let mut best: Option<(usize, usize)> = None;
    for (i, l) in a.iter().enumerate() {
        if let Some((bi, bj)) = best {
            if i > bi + bj {
                break;
            }
        }
        if let Some(js) = first_in_b.get(&(l.hash, l.key)) {
            for &j in js {
                if let Some((bi, bj)) = best {
                    if i + j >= bi + bj {
                        break;
                    }
                }
                if agree(i, j) {
                    best = Some((i, j));
                    break;
                }
            }
        }
    }
    // Both windows reach the ends of their files: the remainders differ entirely.
    if best.is_none() && a_eof && b_eof {
        return Some((a.len(), b.len()));
    }
    best
}

/// Compares `a` and `b`, calling `on_hunk` for each differing region in order (return false to
/// stop). Progress is reported in bytes of `a` + `b` consumed.
pub fn diff(a: &dyn LineSource, b: &dyn LineSource, cancel: &CancelToken, progress: Option<&Progress>, on_hunk: impl FnMut(Hunk) -> bool) -> DiffEnd {
    diff_with(a, b, &DiffOptions::default(), cancel, progress, on_hunk)
}

/// [`diff`] where lines that differ only in what `opts` ignores count as the same.
pub fn diff_with(a: &dyn LineSource, b: &dyn LineSource, opts: &DiffOptions, cancel: &CancelToken, progress: Option<&Progress>, mut on_hunk: impl FnMut(Hunk) -> bool) -> DiffEnd {
    let mut ra = Reader::new(a, opts);
    let mut rb = Reader::new(b, opts);
    if let Some(p) = progress {
        p.set_total(a.len() + b.len());
    }
    loop {
        if cancel.is_cancelled() {
            return DiffEnd::Cancelled;
        }
        if let Some(p) = progress {
            p.set_done(ra.pos + rb.pos);
        }
        // Fast path: skip identical bytes chunk by chunk, stopping at line starts.
        let left = (ra.src.len() - ra.pos.min(ra.src.len()), rb.src.len() - rb.pos.min(rb.src.len()));
        let (Some(x), Some(y)) = (ra.peek(), rb.peek()) else { return DiffEnd::Failed };
        if x.is_empty() && y.is_empty() {
            return DiffEnd::Completed;
        }
        let lim = x.len().min(y.len());
        let n = common_prefix(x, y);
        // A same start without a line break where a file ends ("b\n" against "b") is not a
        // same line.
        let line_break = memchr::memrchr(b'\n', &x[..lim]);
        let ends = lim as u64 >= left.0 || lim as u64 >= left.1;
        if n == lim && lim > 0 && (line_break.is_some() || !ends) {
            let adv = line_break.map(|i| i + 1).unwrap_or(lim);
            let nl = count_byte(&x[..adv], b'\n');
            ra.advance(adv as u64, nl);
            rb.advance(adv as u64, nl);
            continue;
        }
        // Mismatch (or one side ended): rewind to the start of the differing line.
        let back = memchr::memrchr(b'\n', &x[..n]).map(|i| i + 1).unwrap_or(0);
        let nl = count_byte(&x[..back], b'\n');
        ra.advance(back as u64, nl);
        rb.advance(back as u64, nl);
        // Lines equal once normalised, one by one, before looking for an anchor.
        if !opts.plain() {
            let mut same = false;
            loop {
                let (pa, la, pb, lb) = (ra.pos, ra.line, rb.pos, rb.line);
                match (ra.next_line(), rb.next_line()) {
                    (Some(x), Some(y)) if x.hash == y.hash && x.key == y.key => same = true,
                    _ => {
                        (ra.pos, ra.line, rb.pos, rb.line) = (pa, la, pb, lb);
                        break;
                    }
                }
            }
            if same {
                continue;
            }
        }
        // Slow path: re-synchronise using line hashes.
        let (sa_line, sa_off, sb_line, sb_off) = (ra.line, ra.pos, rb.line, rb.pos);
        let Some(wa) = read_window(&mut ra) else { return DiffEnd::Failed };
        let Some(wb) = read_window(&mut rb) else { return DiffEnd::Failed };
        let (a_eof, b_eof) = (ra.eof(), rb.eof());
        let (i, j) = find_anchor(&wa, &wb, a_eof, b_eof).unwrap_or((wa.len(), wb.len()));
        if i > 0 || j > 0 {
            let end = |w: &[Line], k: usize, off: u64| k.checked_sub(1).map(|k| w[k].off + w[k].len).unwrap_or(off);
            let h = Hunk { a_line: sa_line, a_count: i as u64, a_off: sa_off, a_end: end(&wa, i, sa_off), b_line: sb_line, b_count: j as u64, b_off: sb_off, b_end: end(&wb, j, sb_off) };
            if !on_hunk(h) {
                return DiffEnd::Stopped;
            }
        }
        // Position both readers right after the hunk (before the anchor).
        let reposition = |r: &mut Reader, w: &[Line], k: usize, start_line: u64, start_off: u64| {
            r.pos = w.get(k).map(|l| l.off).unwrap_or_else(|| w.last().map(|l| l.off + l.len).unwrap_or(start_off));
            r.line = start_line + k as u64;
        };
        reposition(&mut ra, &wa, i, sa_line, sa_off);
        reposition(&mut rb, &wb, j, sb_line, sb_off);
        if i == 0 && j == 0 {
            // Anchored immediately (identical lines after a rewind): consume one equal line
            // to guarantee progress.
            let (la, lb) = (ra.next_line(), rb.next_line());
            if la.is_none() && lb.is_none() {
                return DiffEnd::Completed;
            }
        }
        if ra.eof() && rb.eof() {
            return DiffEnd::Completed;
        }
    }
}

/// Convenience for tests and small inputs: all hunks.
pub fn diff_all(a: &dyn LineSource, b: &dyn LineSource) -> Vec<Hunk> {
    let mut v = Vec::new();
    diff(a, b, &CancelToken::new(), None, |h| {
        v.push(h);
        true
    });
    v
}

/// Byte range of `count` lines starting at `off` (for previews).
pub fn hunk_range(off: u64, len: u64) -> Range<u64> {
    off..off + len
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::OpenMode;
    use std::io::Write;

    fn src(data: &[u8]) -> (tempfile::NamedTempFile, Source) {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(data).unwrap();
        f.flush().unwrap();
        let s = Source::open(f.path(), OpenMode::Auto).unwrap();
        (f, s)
    }

    fn lines(n: usize) -> String {
        (0..n).map(|i| format!("line number {i}\n")).collect()
    }

    fn summary(h: &[Hunk]) -> Vec<(u64, u64, u64, u64)> {
        h.iter().map(|h| (h.a_line, h.a_count, h.b_line, h.b_count)).collect()
    }

    #[test]
    fn ignoring_timestamps_case_whitespace_and_masks() {
        let a = "2024-05-01 10:00:00 INFO start req=ab12\n2024-05-01 10:00:01 INFO   Working\n2024-05-01 10:00:02 ERROR boom\n";
        let b = "2024-06-09 08:30:00 INFO start req=ff99\n2024-06-09 08:30:01 info working\n2024-06-09 08:30:02 ERROR bang\n";
        let (_a, sa) = src(a.as_bytes());
        let (_b, sb) = src(b.as_bytes());
        let run = |o: &DiffOptions| {
            let mut v = Vec::new();
            diff_with(&sa, &sb, o, &CancelToken::new(), None, |h| {
                v.push(h);
                true
            });
            summary(&v)
        };
        assert_eq!(run(&DiffOptions::default()), vec![(0, 3, 0, 3)]);
        let all = DiffOptions { whitespace: true, case: true, timestamps: true, mask: Some(regex_automata::meta::Regex::new("req=[0-9a-f]+").unwrap()) };
        assert_eq!(run(&all), vec![(2, 1, 2, 1)]);
        let no_mask = DiffOptions { mask: None, ..all.clone() };
        // One equal line between two changes is too short an anchor: one hunk.
        assert_eq!(run(&no_mask), vec![(0, 3, 0, 3)]);
        // Many lines that differ only in their times: no hunks, and quickly.
        let big_a: String = (0..20_000).map(|i| format!("10:00:{:02} line {i}\n", i % 60)).collect();
        let big_b: String = (0..20_000).map(|i| format!("11:30:{:02} line {i}\n", (i + 7) % 60)).collect();
        let (_c, sc) = src(big_a.as_bytes());
        let (_d, sd) = src(big_b.as_bytes());
        let t = std::time::Instant::now();
        let mut n = 0;
        diff_with(&sc, &sd, &DiffOptions { timestamps: true, ..Default::default() }, &CancelToken::new(), None, |_| {
            n += 1;
            true
        });
        assert_eq!(n, 0);
        assert!(t.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn edited_document_against_its_file() {
        use crate::index::LineIndex;
        use std::sync::Arc;
        let text = lines(3000);
        let (_f, s) = src(text.as_bytes());
        let s = Arc::new(s);
        let codec = crate::encoding::Codec::utf8();
        let mut buf = TextBuf::new(s.clone(), LineIndex::new(s.len()), codec);
        assert!(diff_all(&*s, &buf.snapshot()).is_empty());
        // Insert a line after line 2000, then change line 9.
        let at = text.find("line number 2000\n").unwrap() as u64 + 17;
        buf.replace(at..at, b"new line\n");
        let at = text.find("line number 9\n").unwrap() as u64;
        buf.replace(at..at + 13, b"line number nine");
        let mut v = Vec::new();
        diff(&buf.snapshot(), &*s, &CancelToken::new(), None, |h| {
            v.push(h);
            true
        });
        assert_eq!(summary(&v), vec![(9, 1, 9, 1), (2001, 1, 2001, 0)]);
        assert_eq!(v[0].a_off, at_line(&text, 9));
    }

    #[test]
    fn last_line_losing_its_line_break() {
        let (_a, sa) = src(b"a\nb\n");
        let (_b, sb) = src(b"a\nb");
        let h = diff_all(&sa, &sb);
        assert_eq!(summary(&h), vec![(1, 1, 1, 1)]);
        assert_eq!((h[0].a_off, h[0].a_end, h[0].b_off, h[0].b_end), (2, 4, 2, 3));
        let (_c, sc) = src(b"a\nb");
        assert!(diff_all(&sb, &sc).is_empty());
    }

    #[test]
    fn hunk_ends_and_line_mapping() {
        let a = lines(100);
        let b = a.replace("line number 10\n", "line number ten\nand more\n").replace("line number 50\n", "");
        let (_a, sa) = src(a.as_bytes());
        let (_b, sb) = src(b.as_bytes());
        let h = diff_all(&sa, &sb);
        assert_eq!(summary(&h), vec![(10, 1, 10, 2), (50, 1, 51, 0)]);
        assert_eq!((h[0].a_end, h[0].b_end), (at_line(&a, 11), at_line(&b, 12)));
        assert_eq!((h[1].a_end, h[1].b_end), (at_line(&a, 51), at_line(&b, 51)));
        assert_eq!(h[1].b_off, h[1].b_end);
        let to_b: Vec<u64> = [0, 10, 11, 40, 50, 51, 99].iter().map(|&l| map_line(&h, l, true)).collect();
        assert_eq!(to_b, vec![0, 10, 12, 41, 51, 51, 99]);
        let to_a: Vec<u64> = [0, 10, 11, 12, 41, 51, 98].iter().map(|&l| map_line(&h, l, false)).collect();
        assert_eq!(to_a, vec![0, 10, 10, 11, 40, 51, 98]);
    }

    fn at_line(text: &str, n: usize) -> u64 {
        text.split_inclusive('\n').take(n).map(str::len).sum::<usize>() as u64
    }

    #[test]
    fn identical_files() {
        let t = lines(1000);
        let (_a, a) = src(t.as_bytes());
        let (_b, b) = src(t.as_bytes());
        assert!(diff_all(&a, &b).is_empty());
        let (_e, e) = src(b"");
        let (_f, f) = src(b"");
        assert!(diff_all(&e, &f).is_empty());
    }

    #[test]
    fn single_change_insert_delete() {
        let base = lines(100);
        let changed = base.replace("line number 50\n", "CHANGED\n");
        let (_a, a) = src(base.as_bytes());
        let (_b, b) = src(changed.as_bytes());
        assert_eq!(summary(&diff_all(&a, &b)), vec![(50, 1, 50, 1)]);

        let inserted = base.replace("line number 10\n", "line number 10\nnew 1\nnew 2\n");
        let (_c, c) = src(inserted.as_bytes());
        assert_eq!(summary(&diff_all(&a, &c)), vec![(11, 0, 11, 2)]);

        let deleted = base.replace("line number 70\nline number 71\n", "");
        let (_d, d) = src(deleted.as_bytes());
        assert_eq!(summary(&diff_all(&a, &d)), vec![(70, 2, 70, 0)]);
    }

    #[test]
    fn differences_at_edges() {
        let base = lines(20);
        let (_a, a) = src(base.as_bytes());
        let head = format!("HEAD\n{base}");
        let (_b, b) = src(head.as_bytes());
        assert_eq!(summary(&diff_all(&a, &b)), vec![(0, 0, 0, 1)]);
        let tail = format!("{base}TAIL\n");
        let (_c, c) = src(tail.as_bytes());
        assert_eq!(summary(&diff_all(&a, &c)), vec![(20, 0, 20, 1)]);
        let (_e, e) = src(b"");
        assert_eq!(summary(&diff_all(&e, &a)), vec![(0, 0, 0, 20)]);
        // Last line without newline differs.
        let (_x, x) = src(b"a\nb\nc");
        let (_y, y) = src(b"a\nb\nd");
        assert_eq!(summary(&diff_all(&x, &y)), vec![(2, 1, 2, 1)]);
    }

    #[test]
    fn many_scattered_changes_in_big_file() {
        let base = lines(300_000); // ~6 MB, more than one chunk
        let mut changed = String::new();
        for (i, l) in base.lines().enumerate() {
            if i % 50_000 == 7 {
                changed.push_str("XXX\n");
            } else {
                changed.push_str(l);
                changed.push('\n');
            }
        }
        let (_a, a) = src(base.as_bytes());
        let (_b, b) = src(changed.as_bytes());
        let h = diff_all(&a, &b);
        assert_eq!(h.len(), 6);
        for (k, x) in h.iter().enumerate() {
            assert_eq!((x.a_line, x.a_count, x.b_count), (k as u64 * 50_000 + 7, 1, 1));
            let at = base.lines().take(x.a_line as usize).map(|l| l.len() + 1).sum::<usize>() as u64;
            assert_eq!(x.a_off, at);
        }
    }

    #[test]
    fn completely_different_files_progress() {
        let a_text: String = (0..10_000).map(|i| format!("a{i}\n")).collect();
        let b_text: String = (0..10_000).map(|i| format!("b{i}\n")).collect();
        let (_a, a) = src(a_text.as_bytes());
        let (_b, b) = src(b_text.as_bytes());
        let h = diff_all(&a, &b);
        let total_a: u64 = h.iter().map(|x| x.a_count).sum();
        let total_b: u64 = h.iter().map(|x| x.b_count).sum();
        assert_eq!((total_a, total_b), (10_000, 10_000));
    }

    #[test]
    fn hasher_is_streaming_consistent() {
        let data: Vec<u8> = (0..1000u32).map(|i| (i * 7 % 251) as u8).collect();
        let mut one = LineHasher::new();
        one.update(&data);
        for split in [1, 3, 7, 8, 9, 500] {
            let mut two = LineHasher::new();
            two.update(&data[..split]);
            two.update(&data[split..]);
            assert_eq!(one.finish(), two.finish());
        }
    }
}
