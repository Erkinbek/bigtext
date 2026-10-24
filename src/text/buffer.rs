//! Piece table over the original (memory-mapped) file plus an append-only add buffer.
//!
//! The original file is never modified. A `TextBuf` is cheap to clone (all heavy parts are
//! `Arc`s), so workers get consistent snapshots; edits on the UI side copy-on-write.

use std::borrow::Cow;
use std::ops::Range;
use std::sync::Arc;

use crate::encoding::Codec;
use crate::file::Source;
use crate::index::LineIndex;

/// Pieces of an original range shorter than this get exact newline counts by direct scanning
/// when the index does not cover them yet.
const DIRECT_COUNT_MAX: u64 = 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Buf {
    Orig,
    Add,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Piece {
    pub buf: Buf,
    pub start: u64,
    pub len: u64,
    /// Newline units inside the piece (an estimate when `exact` is false).
    pub nl: u64,
    pub exact: bool,
}

/// One reversible edit: `pieces[at..at+removed.len()]` was replaced by `inserted`.
#[derive(Clone, Debug, Default)]
pub struct Swap {
    pub at: usize,
    pub removed: Vec<Piece>,
    pub inserted: Vec<Piece>,
}

#[derive(Clone)]
pub struct TextBuf {
    pub source: Arc<Source>,
    pub index: Arc<LineIndex>,
    pub codec: Codec,
    pieces: Arc<Vec<Piece>>,
    add: Arc<Vec<u8>>,
    /// `cum_bytes[k]` = document offset of piece k; last entry = document length.
    cum_bytes: Arc<Vec<u64>>,
    /// `cum_lines[k]` = newlines before piece k (estimates after the first inexact piece).
    cum_lines: Arc<Vec<u64>>,
    inexact_from: usize,
    /// Reads may block (worker snapshots) or must not (UI thread).
    pub blocking: bool,
}

impl TextBuf {
    pub fn new(source: Arc<Source>, index: Arc<LineIndex>, codec: Codec) -> TextBuf {
        let len = source.len();
        let pieces = if len > 0 {
            vec![Piece { buf: Buf::Orig, start: 0, len, nl: 0, exact: false }]
        } else {
            vec![]
        };
        let mut t = TextBuf {
            source,
            index,
            codec,
            pieces: Arc::new(pieces),
            add: Arc::new(Vec::new()),
            cum_bytes: Arc::new(vec![0]),
            cum_lines: Arc::new(vec![0]),
            inexact_from: 0,
            blocking: false,
        };
        if len > 0 {
            let p = t.make_piece(Buf::Orig, 0, len);
            Arc::make_mut(&mut t.pieces)[0] = p;
        }
        t.rebuild_cum();
        t
    }

    /// Rebuilds this buffer on a different (but byte-identical) source, keeping all edits.
    pub fn with_source(&self, source: Arc<Source>, index: Arc<LineIndex>) -> TextBuf {
        let mut t = self.clone();
        t.source = source;
        t.index = index;
        t
    }

    /// A consistent snapshot for worker threads (blocking reads).
    pub fn snapshot(&self) -> TextBuf {
        let mut s = self.clone();
        s.blocking = true;
        s
    }

    pub fn len(&self) -> u64 {
        *self.cum_bytes.last().unwrap()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn pieces(&self) -> &[Piece] {
        &self.pieces
    }

    pub fn add_len(&self) -> u64 {
        self.add.len() as u64
    }

    /// True if the document is exactly the original file (fast paths may use the mmap directly).
    pub fn is_pristine(&self) -> bool {
        match self.pieces.as_slice() {
            [] => self.source.is_empty(),
            [p] => p.buf == Buf::Orig && p.start == 0 && p.len == self.source.len(),
            _ => false,
        }
    }

    /// Length of the unchanged prefix shared with the original file.
    pub fn pristine_prefix(&self) -> u64 {
        match self.pieces.first() {
            Some(p) if p.buf == Buf::Orig && p.start == 0 => p.len,
            _ => 0,
        }
    }

    /// Index of the piece containing `off` (the last piece for `off == len`).
    fn piece_at(&self, off: u64) -> usize {
        let n = self.pieces.len();
        if n == 0 {
            return 0;
        }
        let pp = self.cum_bytes[..n].partition_point(|&s| s <= off);
        pp.saturating_sub(1).min(n - 1)
    }

    fn piece_bytes(&self, p: &Piece, rel: Range<u64>) -> Option<Cow<'_, [u8]>> {
        let s = p.start + rel.start;
        let e = p.start + rel.end;
        match p.buf {
            Buf::Orig => self.source.get(s..e, self.blocking),
            Buf::Add => Some(Cow::Borrowed(&self.add[s as usize..e as usize])),
        }
    }

    /// Reads a document range. `None` if (non-blocking) data is not available yet.
    pub fn read(&self, range: Range<u64>) -> Option<Cow<'_, [u8]>> {
        let len = self.len();
        let end = range.end.min(len);
        let start = range.start.min(end);
        if start == end {
            return Some(Cow::Borrowed(&[]));
        }
        let k = self.piece_at(start);
        let pstart = self.cum_bytes[k];
        let p = self.pieces[k];
        if end <= pstart + p.len {
            return self.piece_bytes(&p, start - pstart..end - pstart);
        }
        let mut out = Vec::with_capacity((end - start) as usize);
        let mut i = k;
        while i < self.pieces.len() && self.cum_bytes[i] < end {
            let p = self.pieces[i];
            let ps = self.cum_bytes[i];
            let s = start.max(ps) - ps;
            let e = end.min(ps + p.len) - ps;
            out.extend_from_slice(&self.piece_bytes(&p, s..e)?);
            i += 1;
        }
        Some(Cow::Owned(out))
    }

    /// Calls `f` with consecutive chunks covering `range` (zero-copy for mmap pieces).
    pub fn for_each_chunk(&self, range: Range<u64>, mut f: impl FnMut(u64, &[u8]) -> bool) -> Option<()> {
        let end = range.end.min(self.len());
        let mut pos = range.start.min(end);
        while pos < end {
            let k = self.piece_at(pos);
            let p = self.pieces[k];
            let ps = self.cum_bytes[k];
            let e = end.min(ps + p.len);
            let bytes = self.piece_bytes(&p, pos - ps..e - ps)?;
            if !f(pos, &bytes) {
                return Some(());
            }
            pos = e;
        }
        Some(())
    }

    // ----------------------------------------------------------------------------------------
    // Newline counting and line/offset mapping.

    fn count_orig(&self, start: u64, len: u64) -> (u64, bool) {
        let end = start + len;
        let src = &self.source;
        let idx = &self.index;
        if let (Some(a), Some(b)) = (
            idx.nl_before(src, &self.codec, self.blocking, start),
            idx.nl_before(src, &self.codec, self.blocking, end),
        ) {
            return (b - a, true);
        }
        if len <= DIRECT_COUNT_MAX {
            if let Some(bytes) = src.get(start..end, self.blocking) {
                return (self.codec.count_nl(&bytes, start), true);
            }
        }
        ((len as f64 / idx.avg_line_len()) as u64, false)
    }

    fn make_piece(&self, buf: Buf, start: u64, len: u64) -> Piece {
        let (nl, exact) = match buf {
            Buf::Add => (self.codec.count_nl(&self.add[start as usize..(start + len) as usize], start), true),
            Buf::Orig => self.count_orig(start, len),
        };
        Piece { buf, start, len, nl, exact }
    }

    /// Newlines in `[p.start, p.start + rel)` of piece `p`.
    fn piece_prefix_nl(&self, p: &Piece, rel: u64) -> (u64, bool) {
        if rel == 0 {
            return (0, true);
        }
        let rel = rel.min(p.len);
        if rel == p.len && p.exact {
            return (p.nl, true);
        }
        match p.buf {
            Buf::Add => {
                let s = p.start as usize;
                (self.codec.count_nl(&self.add[s..s + rel as usize], p.start), true)
            }
            Buf::Orig => {
                let idx = &self.index;
                let src = &self.source;
                let (a, ea) = idx.nl_before_est(src, &self.codec, self.blocking, p.start);
                let (b, eb) = idx.nl_before_est(src, &self.codec, self.blocking, p.start + rel);
                if ea && eb {
                    return (b.saturating_sub(a), true);
                }
                if rel <= DIRECT_COUNT_MAX {
                    if let Some(bytes) = src.get(p.start..p.start + rel, self.blocking) {
                        return (self.codec.count_nl(&bytes, p.start), true);
                    }
                }
                (b.saturating_sub(a), false)
            }
        }
    }

    /// 0-based line number containing `off`, and whether it is exact.
    pub fn line_of(&self, off: u64) -> (u64, bool) {
        if self.pieces.is_empty() {
            return (0, true);
        }
        let off = off.min(self.len());
        let k = self.piece_at(off);
        let (n, exact) = self.piece_prefix_nl(&self.pieces[k], off - self.cum_bytes[k]);
        (self.cum_lines[k] + n, exact && k <= self.inexact_from)
    }

    /// Start offset of 0-based `line`. `None` if the line is known not to exist. The offset is
    /// exact when the flag is true; otherwise it is an estimate that callers should snap.
    pub fn line_start(&self, line: u64) -> Option<(u64, bool)> {
        if line == 0 {
            return Some((0, true));
        }
        let n = self.pieces.len();
        if n == 0 {
            return None;
        }
        // Piece containing the `line`-th newline (the last piece takes everything beyond).
        let k = self.cum_lines[1..n].partition_point(|&c| c < line);
        let p = self.pieces[k];
        let need = line - self.cum_lines[k];
        let pstart = self.cum_bytes[k];
        let lines_exact = k <= self.inexact_from;
        let is_last = k == n - 1;
        let not_here = || {
            if is_last && lines_exact {
                None
            } else {
                Some((pstart + p.len, false))
            }
        };
        match p.buf {
            Buf::Add => {
                let s = p.start as usize;
                let hay = &self.add[s..s + p.len as usize];
                match self.codec.nth_nl(hay, p.start, need) {
                    Some(i) => Some((pstart + i as u64 + self.codec.unit() as u64, lines_exact)),
                    None => not_here(),
                }
            }
            Buf::Orig => {
                let (base, ebase) = self.index.nl_before_est(&self.source, &self.codec, self.blocking, p.start);
                let target = base + need;
                match self.index.line_start(&self.source, &self.codec, self.blocking, target) {
                    Some(o) if o <= p.start + p.len => Some((pstart + (o - p.start), lines_exact && ebase)),
                    Some(_) => not_here(),
                    None if self.index.is_done() => not_here(),
                    None => {
                        let (orig, _) = self.index.line_start_est(&self.source, &self.codec, self.blocking, target);
                        let orig = orig.clamp(p.start, p.start + p.len);
                        Some((pstart + (orig - p.start), false))
                    }
                }
            }
        }
    }

    /// Total number of lines and whether it is exact.
    pub fn line_count(&self) -> (u64, bool) {
        let n = self.pieces.len();
        if n == 1 && self.pieces[0].buf == Buf::Orig && self.pieces[0].start == 0 {
            if let Some(t) = self.index.total_lines() {
                if self.pieces[0].len == self.index.len() {
                    return (t, true);
                }
            }
            let p = &self.pieces[0];
            let (nl, ex) = self.index.nl_before_est(&self.source, &self.codec, self.blocking, p.len);
            return (nl + 1, ex);
        }
        (self.cum_lines[n] + 1, self.inexact_from >= n)
    }

    /// True if some pieces have estimated newline counts.
    pub fn has_inexact(&self) -> bool {
        self.inexact_from < self.pieces.len()
    }

    /// Re-counts up to `budget` inexact pieces (call when the index has progressed).
    pub fn refresh_counts(&mut self, budget: usize) -> bool {
        if !self.has_inexact() {
            return false;
        }
        let mut changed = false;
        let mut done = 0;
        let mut updates = Vec::new();
        for (i, p) in self.pieces.iter().enumerate().skip(self.inexact_from) {
            if p.exact {
                continue;
            }
            if done >= budget {
                break;
            }
            done += 1;
            let (nl, exact) = self.count_orig(p.start, p.len);
            if exact || nl != p.nl {
                updates.push((i, nl, exact));
            }
        }
        if !updates.is_empty() {
            let pieces = Arc::make_mut(&mut self.pieces);
            for (i, nl, exact) in updates {
                pieces[i].nl = nl;
                pieces[i].exact = exact;
            }
            changed = true;
            self.rebuild_cum();
        }
        changed
    }

    fn rebuild_cum(&mut self) {
        let n = self.pieces.len();
        let mut cb = Vec::with_capacity(n + 1);
        let mut cl = Vec::with_capacity(n + 1);
        let (mut b, mut l) = (0u64, 0u64);
        let mut inexact_from = n;
        for (i, p) in self.pieces.iter().enumerate() {
            cb.push(b);
            cl.push(l);
            b += p.len;
            l += p.nl;
            if !p.exact && inexact_from == n {
                inexact_from = i;
            }
        }
        cb.push(b);
        cl.push(l);
        self.cum_bytes = Arc::new(cb);
        self.cum_lines = Arc::new(cl);
        self.inexact_from = inexact_from;
    }

    // ----------------------------------------------------------------------------------------
    // Editing.

    fn sub_piece(&self, p: &Piece, from: u64, to: u64) -> Piece {
        if from == 0 && to == p.len {
            return *p;
        }
        self.make_piece(p.buf, p.start + from, to - from)
    }

    /// Splits piece `p` at `rel` into (left, right), reusing the parent's count where possible.
    fn split_piece(&self, p: &Piece, rel: u64) -> (Piece, Piece) {
        let left = self.sub_piece(p, 0, rel);
        let right = if p.exact && left.exact {
            Piece { buf: p.buf, start: p.start + rel, len: p.len - rel, nl: p.nl - left.nl.min(p.nl), exact: true }
        } else {
            self.sub_piece(p, rel, p.len)
        };
        (left, right)
    }

    fn append_add(&mut self, text: &[u8]) -> Option<Piece> {
        if text.is_empty() {
            return None;
        }
        let add = Arc::make_mut(&mut self.add);
        let start = add.len() as u64;
        add.extend_from_slice(text);
        let nl = self.codec.count_nl(text, start);
        Some(Piece { buf: Buf::Add, start, len: text.len() as u64, nl, exact: true })
    }

    /// Replaces `range` with `text` and returns the reversible swap.
    pub fn replace(&mut self, range: Range<u64>, text: &[u8]) -> Swap {
        let len = self.len();
        let end = range.end.min(len);
        let start = range.start.min(end);
        let add_len_before = self.add.len() as u64;
        let new_piece = self.append_add(text);
        let n = self.pieces.len();

        let swap = if start == end {
            // Pure insertion.
            let Some(np) = new_piece else { return Swap::default() };
            let (k, rel) = if n == 0 || start == len { (n, 0) } else { let k = self.piece_at(start); (k, start - self.cum_bytes[k]) };
            if rel == 0 {
                // At a piece boundary: try to extend the previous add piece (typing).
                if k > 0 {
                    let prev = self.pieces[k - 1];
                    if prev.buf == Buf::Add && prev.start + prev.len == add_len_before {
                        let ext = Piece { len: prev.len + np.len, nl: prev.nl + np.nl, ..prev };
                        Swap { at: k - 1, removed: vec![prev], inserted: vec![ext] }
                    } else {
                        Swap { at: k, removed: vec![], inserted: vec![np] }
                    }
                } else {
                    Swap { at: 0, removed: vec![], inserted: vec![np] }
                }
            } else {
                let p = self.pieces[k];
                let (l, r) = self.split_piece(&p, rel);
                Swap { at: k, removed: vec![p], inserted: vec![l, np, r] }
            }
        } else {
            let a = self.piece_at(start);
            let b = self.piece_at(end - 1);
            let rel_a = start - self.cum_bytes[a];
            let rel_b = end - self.cum_bytes[b];
            let mut inserted = Vec::with_capacity(3);
            if rel_a > 0 {
                inserted.push(self.sub_piece(&self.pieces[a], 0, rel_a));
            }
            if let Some(np) = new_piece {
                inserted.push(np);
            }
            let pb = self.pieces[b];
            if rel_b < pb.len {
                inserted.push(self.sub_piece(&pb, rel_b, pb.len));
            }
            Swap { at: a, removed: self.pieces[a..=b].to_vec(), inserted }
        };
        self.apply(&swap, true);
        swap
    }

    /// Applies (`forward`) or reverts a swap.
    pub fn apply(&mut self, swap: &Swap, forward: bool) {
        let pieces = Arc::make_mut(&mut self.pieces);
        if forward {
            pieces.splice(swap.at..swap.at + swap.removed.len(), swap.inserted.iter().copied());
        } else {
            pieces.splice(swap.at..swap.at + swap.inserted.len(), swap.removed.iter().copied());
        }
        self.rebuild_cum();
    }

    /// Pieces referencing the bytes of `range` (zero-copy duplication).
    pub fn copy_pieces(&self, range: Range<u64>) -> Vec<Piece> {
        let end = range.end.min(self.len());
        let mut pos = range.start.min(end);
        let mut out = Vec::new();
        while pos < end {
            let k = self.piece_at(pos);
            let p = self.pieces[k];
            let ps = self.cum_bytes[k];
            let e = end.min(ps + p.len);
            out.push(self.sub_piece(&p, pos - ps, e - ps));
            pos = e;
        }
        out
    }

    /// Inserts existing pieces (from [`copy_pieces`]) at `at`.
    pub fn insert_pieces(&mut self, at: u64, new: Vec<Piece>) -> Swap {
        if new.is_empty() {
            return Swap::default();
        }
        let at = at.min(self.len());
        let n = self.pieces.len();
        let swap = if n == 0 || at == self.len() {
            Swap { at: n, removed: vec![], inserted: new }
        } else {
            let k = self.piece_at(at);
            let rel = at - self.cum_bytes[k];
            if rel == 0 {
                Swap { at: k, removed: vec![], inserted: new }
            } else {
                let p = self.pieces[k];
                let (l, r) = self.split_piece(&p, rel);
                let mut ins = vec![l];
                ins.extend(new);
                ins.push(r);
                Swap { at: k, removed: vec![p], inserted: ins }
            }
        };
        self.apply(&swap, true);
        swap
    }

    /// Builds the piece list for a batch of sorted, non-overlapping replacements (worker side).
    /// Returns the new pieces and the bytes to append to the add buffer; new add pieces start at
    /// this snapshot's `add_len()`.
    pub fn build_batch(&self, edits: &[(Range<u64>, Vec<u8>)]) -> (Vec<Piece>, Vec<u8>) {
        let mut out: Vec<Piece> = Vec::with_capacity(self.pieces.len() + edits.len() * 2);
        let mut add_bytes = Vec::new();
        let base = self.add.len() as u64;
        let mut pos = 0u64;
        let push_range = |out: &mut Vec<Piece>, from: u64, to: u64| {
            let mut p = from;
            while p < to {
                let k = self.piece_at(p);
                let pc = self.pieces[k];
                let ps = self.cum_bytes[k];
                let e = to.min(ps + pc.len);
                out.push(self.sub_piece(&pc, p - ps, e - ps));
                p = e;
            }
        };
        for (r, text) in edits {
            let s = r.start.max(pos);
            push_range(&mut out, pos, s);
            if !text.is_empty() {
                let start = base + add_bytes.len() as u64;
                add_bytes.extend_from_slice(text);
                let nl = self.codec.count_nl(text, start);
                out.push(Piece { buf: Buf::Add, start, len: text.len() as u64, nl, exact: true });
            }
            pos = r.end.max(s);
        }
        push_range(&mut out, pos, self.len());
        (out, add_bytes)
    }

    /// Applies a batch built by [`build_batch`] on a snapshot with the same add buffer length.
    pub fn apply_batch(&mut self, pieces: Vec<Piece>, add_bytes: &[u8]) -> Swap {
        Arc::make_mut(&mut self.add).extend_from_slice(add_bytes);
        let swap = Swap { at: 0, removed: self.pieces.to_vec(), inserted: pieces };
        self.apply(&swap, true);
        swap
    }

    // ----------------------------------------------------------------------------------------
    // Character navigation helpers.

    /// Offset of the character boundary after `off`.
    pub fn next_char(&self, off: u64) -> u64 {
        let len = self.len();
        if off >= len {
            return len;
        }
        match self.read(off..(off + 4).min(len)) {
            Some(b) if !b.is_empty() => off + self.codec.decode_char(&b).1 as u64,
            _ => (off + self.codec.unit() as u64).min(len),
        }
    }

    /// Offset of the character boundary before `off`.
    pub fn prev_char(&self, off: u64) -> u64 {
        if off == 0 {
            return 0;
        }
        let s = off.saturating_sub(4);
        match self.read(s..off) {
            Some(b) if !b.is_empty() => off - self.codec.prev_char_len(&b) as u64,
            _ => off.saturating_sub(self.codec.unit() as u64),
        }
    }

    /// Snaps an arbitrary offset forward to a character boundary.
    pub fn snap_char(&self, off: u64) -> u64 {
        let len = self.len();
        let unit = self.codec.unit() as u64;
        let off = (off - off % unit).min(len);
        match self.read(off..(off + 4).min(len)) {
            Some(b) => (off + self.codec.snap_forward(&b) as u64).min(len),
            None => off,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::OpenMode;
    use crate::index;
    use std::io::Write;

    fn buf_with(data: &[u8]) -> (tempfile::NamedTempFile, TextBuf) {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(data).unwrap();
        f.flush().unwrap();
        let src = Arc::new(Source::open(f.path(), OpenMode::Auto).unwrap());
        let idx = index::build_blocking(&src, Codec::utf8());
        let mut t = TextBuf::new(src, idx, Codec::utf8());
        t.blocking = true;
        (f, t)
    }

    fn text(t: &TextBuf) -> String {
        String::from_utf8(t.read(0..t.len()).unwrap().into_owned()).unwrap()
    }

    /// Reference model: a plain String with the same edits.
    fn check_lines(t: &TextBuf) {
        let s = text(t);
        let mut starts = vec![0u64];
        for (i, b) in s.bytes().enumerate() {
            if b == b'\n' {
                starts.push(i as u64 + 1);
            }
        }
        assert_eq!(t.line_count(), (starts.len() as u64, true));
        for (l, &off) in starts.iter().enumerate() {
            assert_eq!(t.line_start(l as u64), Some((off, true)), "line {l}");
            assert_eq!(t.line_of(off).0, l as u64);
        }
    }

    #[test]
    fn insert_delete_undo_redo() {
        let (_f, mut t) = buf_with(b"hello\nworld\n");
        let s1 = t.replace(5..5, b" there");
        assert_eq!(text(&t), "hello there\nworld\n");
        let s2 = t.replace(0..0, b">> ");
        assert_eq!(text(&t), ">> hello there\nworld\n");
        let s3 = t.replace(3..9, b"");
        assert_eq!(text(&t), ">> there\nworld\n");
        let s4 = t.replace(9..15, b"\nnew\nlines\n");
        assert_eq!(text(&t), ">> there\n\nnew\nlines\n");
        check_lines(&t);
        for s in [&s4, &s3, &s2, &s1] {
            t.apply(s, false);
        }
        assert_eq!(text(&t), "hello\nworld\n");
        for s in [&s1, &s2, &s3, &s4] {
            t.apply(s, true);
        }
        assert_eq!(text(&t), ">> there\n\nnew\nlines\n");
    }

    #[test]
    fn typing_coalesces_pieces() {
        let (_f, mut t) = buf_with(b"ab");
        for (i, c) in "xyz123".bytes().enumerate() {
            t.replace(1 + i as u64..1 + i as u64, &[c]);
        }
        assert_eq!(text(&t), "axyz123b");
        assert_eq!(t.pieces().len(), 3);
    }

    #[test]
    fn random_edits_match_model() {
        let base: String = (0..2000).map(|i| format!("line {i}\n")).collect();
        let (_f, mut t) = buf_with(base.as_bytes());
        let mut model = base.into_bytes();
        let mut rng = 42u64;
        let mut next = || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        let mut swaps = Vec::new();
        for _ in 0..300 {
            let len = model.len() as u64;
            let a = next() % (len + 1);
            let b = (a + next() % 50).min(len);
            let ins: Vec<u8> = match next() % 3 {
                0 => vec![],
                1 => b"X\nY".to_vec(),
                _ => b"zz".to_vec(),
            };
            swaps.push(t.replace(a..b, &ins));
            model.splice(a as usize..b as usize, ins);
            assert_eq!(t.len(), model.len() as u64);
        }
        assert_eq!(t.read(0..t.len()).unwrap().as_ref(), model.as_slice());
        check_lines(&t);
        for s in swaps.iter().rev() {
            t.apply(s, false);
        }
        assert!(t.is_pristine());
    }

    #[test]
    fn edit_empty_document() {
        let (_f, mut t) = buf_with(b"");
        assert_eq!(t.line_count(), (1, true));
        t.replace(0..0, b"abc\n");
        assert_eq!(text(&t), "abc\n");
        t.replace(0..4, b"");
        assert_eq!(t.len(), 0);
        assert_eq!(t.line_of(0), (0, true));
    }

    #[test]
    fn batch_replace() {
        let (_f, t) = buf_with(b"aaa bbb aaa ccc aaa");
        let edits = vec![(0..3, b"X".to_vec()), (8..11, b"YY\n".to_vec()), (16..19, Vec::new())];
        let (pieces, add) = t.build_batch(&edits);
        let mut t2 = t.clone();
        let swap = t2.apply_batch(pieces, &add);
        assert_eq!(text(&t2), "X bbb YY\n ccc ");
        check_lines(&t2);
        t2.apply(&swap, false);
        assert_eq!(text(&t2), "aaa bbb aaa ccc aaa");
    }

    #[test]
    fn duplicate_via_pieces() {
        let (_f, mut t) = buf_with(b"one\ntwo\n");
        let pcs = t.copy_pieces(0..4);
        t.insert_pieces(4, pcs);
        assert_eq!(text(&t), "one\none\ntwo\n");
        check_lines(&t);
    }

    #[test]
    fn char_navigation() {
        let (_f, t) = buf_with("aé😀b".as_bytes());
        assert_eq!(t.next_char(0), 1);
        assert_eq!(t.next_char(1), 3);
        assert_eq!(t.next_char(3), 7);
        assert_eq!(t.prev_char(7), 3);
        assert_eq!(t.prev_char(3), 1);
        assert_eq!(t.snap_char(4), 7);
    }
}
