//! Piece table over the original (memory-mapped) file plus an append-only add buffer.
//!
//! The original file is never modified. A `TextBuf` is cheap to clone (all heavy parts are
//! `Arc`s), so workers get consistent snapshots; edits on the UI side copy-on-write. The pieces
//! are kept in a chunked tree ([`super::tree`]) and the add buffer in chunks
//! ([`super::addbuf`]), so an edit costs O(P / 512) rather than O(P) for P pieces, and an edit
//! while a worker holds a snapshot copies one leaf and the leaf list, never the whole piece
//! list or the whole add buffer.

use std::ops::Range;
use std::sync::Arc;

use super::addbuf::AddBuf;
use super::tree::{PieceTree, Pos};
use crate::encoding::Codec;
use crate::file::{Bytes, Source};
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

/// One reversible edit.
#[derive(Clone, Debug)]
pub struct Swap(SwapKind);

#[derive(Clone, Debug)]
enum SwapKind {
    /// Pieces `at..at + removed.len()` were replaced by `inserted`.
    Splice {
        at: usize,
        removed: Vec<Piece>,
        inserted: Vec<Piece>,
    },
    /// The whole piece list was replaced (a batch): both lists are kept as they are, sharing
    /// their leaves, so applying either way is O(1).
    Whole { before: PieceTree, after: PieceTree },
}

/// A batch of replacements prepared by [`TextBuf::build_batch`] (on a worker): the finished
/// piece tree and the bytes it adds. It is applied with [`TextBuf::apply_batch`].
#[derive(Clone)]
pub struct Batch {
    tree: PieceTree,
    add: Vec<u8>,
    /// The add buffer length of the snapshot it was built on: its add pieces start there.
    base: u64,
}

impl Batch {
    /// The add buffer length it was built on; apply only to a document with the same.
    pub fn add_len(&self) -> u64 {
        self.base
    }

    /// Pieces of the document after the batch.
    pub fn piece_count(&self) -> usize {
        self.tree.len()
    }
}

impl Default for Swap {
    fn default() -> Swap {
        Swap(SwapKind::Splice {
            at: 0,
            removed: Vec::new(),
            inserted: Vec::new(),
        })
    }
}

impl Swap {
    fn splice(at: usize, removed: Vec<Piece>, inserted: Vec<Piece>) -> Swap {
        Swap(SwapKind::Splice {
            at,
            removed,
            inserted,
        })
    }
}

#[derive(Clone)]
pub struct TextBuf {
    pub source: Arc<Source>,
    pub index: Arc<LineIndex>,
    pub codec: Codec,
    pieces: PieceTree,
    add: AddBuf,
    /// Reads may block (worker snapshots) or must not (UI thread).
    pub blocking: bool,
}

impl TextBuf {
    pub fn new(source: Arc<Source>, index: Arc<LineIndex>, codec: Codec) -> TextBuf {
        let len = source.len();
        let mut t = TextBuf {
            source,
            index,
            codec,
            pieces: PieceTree::default(),
            add: AddBuf::default(),
            blocking: false,
        };
        if len > 0 {
            t.pieces = PieceTree::from_vec(vec![t.make_piece(Buf::Orig, 0, len)]);
        }
        t
    }

    /// A buffer of `pieces` (kind, start, length) over `source` and `add`, as a recovery journal
    /// wrote them; `None` when a piece points outside them.
    pub fn with_pieces(
        source: Arc<Source>,
        index: Arc<LineIndex>,
        codec: Codec,
        pieces: &[(Buf, u64, u64)],
        add: Vec<u8>,
    ) -> Option<TextBuf> {
        for &(b, s, l) in pieces {
            let limit = match b {
                Buf::Orig => source.len(),
                Buf::Add => add.len() as u64,
            };
            if l == 0 || s.checked_add(l)? > limit {
                return None;
            }
        }
        let mut t = TextBuf {
            source,
            index,
            codec,
            pieces: PieceTree::default(),
            add: AddBuf::from_vec(add),
            blocking: false,
        };
        let ps: Vec<Piece> = pieces
            .iter()
            .map(|&(b, s, l)| t.make_piece(b, s, l))
            .collect();
        t.pieces = PieceTree::from_vec(ps);
        Some(t)
    }

    /// A copy of the bytes edits inserted (what [`Buf::Add`] pieces point into) from offset
    /// `from` on.
    pub fn add_bytes_from(&self, from: u64) -> Vec<u8> {
        self.add.copy_from(from)
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
        self.pieces.bytes()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The pieces in document order.
    pub fn pieces(&self) -> impl Iterator<Item = Piece> + '_ {
        self.pieces.iter().map(|(_, p)| p)
    }

    pub fn piece_count(&self) -> usize {
        self.pieces.len()
    }

    pub fn add_len(&self) -> u64 {
        self.add.len()
    }

    /// True if the document is exactly the original file (fast paths may use the mmap directly).
    pub fn is_pristine(&self) -> bool {
        match (self.pieces.len(), self.pieces.first()) {
            (0, _) => self.source.is_empty(),
            (1, Some(p)) => p.buf == Buf::Orig && p.start == 0 && p.len == self.source.len(),
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

    fn piece_bytes(&self, p: &Piece, rel: Range<u64>) -> Option<Bytes<'_>> {
        let s = p.start + rel.start;
        let e = p.start + rel.end;
        match p.buf {
            Buf::Orig => self.source.get(s..e, self.blocking),
            Buf::Add => Some(Bytes::borrowed(self.add.slice(s, e))),
        }
    }

    /// Reads a document range. `None` if (non-blocking) data is not available yet.
    pub fn read(&self, range: Range<u64>) -> Option<Bytes<'_>> {
        let len = self.len();
        let end = range.end.min(len);
        let start = range.start.min(end);
        if start == end {
            return Some(Bytes::borrowed(&[]));
        }
        let pos = self.pieces.at_offset(start)?;
        let p = pos.piece;
        if end <= pos.start + p.len {
            return self.piece_bytes(&p, start - pos.start..end - pos.start);
        }
        let mut out = Vec::with_capacity((end - start) as usize);
        for (ps, p) in self.pieces.iter_from(&pos) {
            if ps >= end {
                break;
            }
            let s = start.max(ps) - ps;
            let e = end.min(ps + p.len) - ps;
            out.extend_from_slice(&self.piece_bytes(&p, s..e)?);
        }
        Some(Bytes::owned(out))
    }

    /// Reads `range` into `out`, reusing its allocation, and returns the byte count. Original
    /// bytes are fetched with positional reads in blocking snapshots (workers), which on a
    /// cached file beats first-touch page faults and keeps private memory flat.
    pub fn read_to(&self, range: Range<u64>, out: &mut Vec<u8>) -> Option<usize> {
        let end = range.end.min(self.len());
        let start = range.start.min(end);
        let n = (end - start) as usize;
        if out.len() < n {
            out.resize(n, 0);
        }
        self.read_into(start, &mut out[..n])?;
        Some(n)
    }

    /// Fills `out` with the bytes at `start` (`None` past the end or when data is unavailable).
    pub fn read_into(&self, start: u64, out: &mut [u8]) -> Option<()> {
        let end = start
            .checked_add(out.len() as u64)
            .filter(|&e| e <= self.len())?;
        if start == end {
            return Some(());
        }
        let mut w = 0usize;
        for (ps, p) in self.pieces.iter_from(&self.pieces.at_offset(start)?) {
            if ps >= end {
                break;
            }
            let pos = start.max(ps);
            let e = end.min(ps + p.len);
            let len = (e - pos) as usize;
            let s = p.start + (pos - ps);
            let dst = &mut out[w..w + len];
            match p.buf {
                Buf::Orig if self.blocking => self.source.read_into(s, dst).ok()?,
                Buf::Orig => dst.copy_from_slice(&self.source.get(s..s + len as u64, false)?),
                Buf::Add => dst.copy_from_slice(self.add.slice(s, s + len as u64)),
            }
            w += len;
        }
        Some(())
    }

    /// Calls `f` with consecutive chunks (at most 4 MB each) covering `range`.
    pub fn for_each_chunk(
        &self,
        range: Range<u64>,
        mut f: impl FnMut(u64, &[u8]) -> bool,
    ) -> Option<()> {
        const STEP: u64 = 4 * 1024 * 1024;
        let end = range.end.min(self.len());
        let mut pos = range.start.min(end);
        let mut buf = Vec::new();
        while pos < end {
            let e = (pos + STEP).min(end);
            let n = self.read_to(pos..e, &mut buf)?;
            if !f(pos, &buf[..n]) {
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
            Buf::Add => (
                self.codec
                    .count_nl(self.add.slice(start, start + len), start),
                true,
            ),
            Buf::Orig => self.count_orig(start, len),
        };
        Piece {
            buf,
            start,
            len,
            nl,
            exact,
        }
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
            Buf::Add => (
                self.codec
                    .count_nl(self.add.slice(p.start, p.start + rel), p.start),
                true,
            ),
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
        let off = off.min(self.len());
        let Some(pos) = self.pieces.at_offset(off) else {
            return (0, true);
        };
        let (n, exact) = self.piece_prefix_nl(&pos.piece, off - pos.start);
        (
            pos.lines + n,
            exact && pos.index <= self.pieces.inexact_from(),
        )
    }

    /// Start offset of 0-based `line`. `None` if the line is known not to exist. The offset is
    /// exact when the flag is true; otherwise it is an estimate that callers should snap.
    pub fn line_start(&self, line: u64) -> Option<(u64, bool)> {
        if line == 0 {
            return Some((0, true));
        }
        // Piece containing the `line`-th newline (the last piece takes everything beyond).
        let pos = self.pieces.at_line(line)?;
        let p = pos.piece;
        let need = line - pos.lines;
        let pstart = pos.start;
        let lines_exact = pos.index <= self.pieces.inexact_from();
        let is_last = pos.index == self.pieces.len() - 1;
        let not_here = || {
            if is_last && lines_exact {
                None
            } else {
                Some((pstart + p.len, false))
            }
        };
        match p.buf {
            Buf::Add => {
                let hay = self.add.slice(p.start, p.start + p.len);
                match self.codec.nth_nl(hay, p.start, need) {
                    Some(i) => Some((pstart + i as u64 + self.codec.unit() as u64, lines_exact)),
                    None => not_here(),
                }
            }
            Buf::Orig => {
                let (base, ebase) =
                    self.index
                        .nl_before_est(&self.source, &self.codec, self.blocking, p.start);
                let target = base + need;
                match self
                    .index
                    .line_start(&self.source, &self.codec, self.blocking, target)
                {
                    Some(o) if o <= p.start + p.len => {
                        Some((pstart + (o - p.start), lines_exact && ebase))
                    }
                    Some(_) => not_here(),
                    None if self.index.is_done() => not_here(),
                    None => {
                        let (orig, _) = self.index.line_start_est(
                            &self.source,
                            &self.codec,
                            self.blocking,
                            target,
                        );
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
        if let (1, Some(p)) = (n, self.pieces.first()) {
            if p.buf == Buf::Orig && p.start == 0 {
                if let Some(t) = self.index.total_lines() {
                    if p.len == self.index.len() {
                        return (t, true);
                    }
                }
                let (nl, ex) =
                    self.index
                        .nl_before_est(&self.source, &self.codec, self.blocking, p.len);
                return (nl + 1, ex);
            }
        }
        (self.pieces.lines() + 1, self.pieces.inexact_from() >= n)
    }

    /// True if some pieces have estimated newline counts.
    pub fn has_inexact(&self) -> bool {
        self.pieces.inexact_from() < self.pieces.len()
    }

    /// Re-counts up to `budget` inexact pieces (call when the index has progressed).
    pub fn refresh_counts(&mut self, budget: usize) -> bool {
        if !self.has_inexact() {
            return false;
        }
        let updates: Vec<(usize, u64, bool)> = self
            .pieces
            .inexact(budget)
            .into_iter()
            .filter_map(|(i, p)| {
                let (nl, exact) = self.count_orig(p.start, p.len);
                (exact || nl != p.nl).then_some((i, nl, exact))
            })
            .collect();
        if updates.is_empty() {
            return false;
        }
        self.pieces.set_counts(&updates);
        true
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
            Piece {
                buf: p.buf,
                start: p.start + rel,
                len: p.len - rel,
                nl: p.nl - left.nl.min(p.nl),
                exact: true,
            }
        } else {
            self.sub_piece(p, rel, p.len)
        };
        (left, right)
    }

    fn append_add(&mut self, text: &[u8]) -> Option<Piece> {
        if text.is_empty() {
            return None;
        }
        let start = self.add.push(text);
        let nl = self.codec.count_nl(text, start);
        Some(Piece {
            buf: Buf::Add,
            start,
            len: text.len() as u64,
            nl,
            exact: true,
        })
    }

    /// The piece containing `off` (the last one for `off == len`); the document must not be
    /// empty.
    fn pos_at(&self, off: u64) -> Pos {
        self.pieces.at_offset(off).expect("document is not empty")
    }

    /// Replaces `range` with `text` and returns the reversible swap.
    pub fn replace(&mut self, range: Range<u64>, text: &[u8]) -> Swap {
        let len = self.len();
        let end = range.end.min(len);
        let start = range.start.min(end);
        let add_len_before = self.add.len();
        let new_piece = self.append_add(text);
        let n = self.pieces.len();

        let swap = if start == end {
            // Pure insertion.
            let Some(np) = new_piece else {
                return Swap::default();
            };
            let (k, rel, piece) = if n == 0 || start == len {
                (n, 0, None)
            } else {
                let pos = self.pos_at(start);
                (pos.index, start - pos.start, Some(pos.piece))
            };
            if rel == 0 {
                // At a piece boundary: try to extend the previous add piece (typing), unless the
                // new text went into another chunk of the add buffer.
                let prev = (k > 0).then(|| self.pieces.get(k - 1));
                match prev {
                    Some(prev)
                        if prev.buf == Buf::Add
                            && prev.start + prev.len == add_len_before
                            && self.add.contiguous(prev.start, np.start + np.len) =>
                    {
                        let ext = Piece {
                            len: prev.len + np.len,
                            nl: prev.nl + np.nl,
                            ..prev
                        };
                        Swap::splice(k - 1, vec![prev], vec![ext])
                    }
                    _ => Swap::splice(k, vec![], vec![np]),
                }
            } else {
                let p = piece.unwrap();
                let (l, r) = self.split_piece(&p, rel);
                Swap::splice(k, vec![p], vec![l, np, r])
            }
        } else {
            let pa = self.pos_at(start);
            let pb = self.pos_at(end - 1);
            let rel_a = start - pa.start;
            let rel_b = end - pb.start;
            let mut inserted = Vec::with_capacity(3);
            if rel_a > 0 {
                inserted.push(self.sub_piece(&pa.piece, 0, rel_a));
            }
            if let Some(np) = new_piece {
                inserted.push(np);
            }
            if rel_b < pb.piece.len {
                inserted.push(self.sub_piece(&pb.piece, rel_b, pb.piece.len));
            }
            let removed = self
                .pieces
                .iter_from(&pa)
                .take(pb.index - pa.index + 1)
                .map(|(_, p)| p)
                .collect();
            Swap::splice(pa.index, removed, inserted)
        };
        self.apply(&swap, true);
        swap
    }

    /// Applies (`forward`) or reverts a swap.
    pub fn apply(&mut self, swap: &Swap, forward: bool) {
        match &swap.0 {
            SwapKind::Splice {
                at,
                removed,
                inserted,
            } => {
                let (old, new) = if forward {
                    (removed, inserted)
                } else {
                    (inserted, removed)
                };
                if !old.is_empty() || !new.is_empty() {
                    self.pieces.splice(*at, old.len(), new);
                }
            }
            SwapKind::Whole { before, after } => {
                let to = if forward { after } else { before };
                self.pieces = to.clone();
            }
        }
    }

    /// Pieces referencing the bytes of `range` (zero-copy duplication).
    pub fn copy_pieces(&self, range: Range<u64>) -> Vec<Piece> {
        let end = range.end.min(self.len());
        let start = range.start.min(end);
        let mut out = Vec::new();
        if start == end {
            return out;
        }
        for (ps, p) in self.pieces.iter_from(&self.pos_at(start)) {
            if ps >= end {
                break;
            }
            let s = start.max(ps) - ps;
            let e = end.min(ps + p.len) - ps;
            out.push(self.sub_piece(&p, s, e));
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
            Swap::splice(n, vec![], new)
        } else {
            let pos = self.pos_at(at);
            let rel = at - pos.start;
            if rel == 0 {
                Swap::splice(pos.index, vec![], new)
            } else {
                let (l, r) = self.split_piece(&pos.piece, rel);
                let mut ins = Vec::with_capacity(new.len() + 2);
                ins.push(l);
                ins.extend(new);
                ins.push(r);
                Swap::splice(pos.index, vec![pos.piece], ins)
            }
        };
        self.apply(&swap, true);
        swap
    }

    /// Prepares a batch of sorted, non-overlapping replacements, on a worker: the whole new
    /// piece tree and the bytes it adds, so that [`TextBuf::apply_batch`] on the UI thread does
    /// no work that grows with the document. The new add pieces start at this snapshot's
    /// `add_len()`, which [`Batch::add_len`] returns for the caller to compare.
    pub fn build_batch(&self, edits: &[(Range<u64>, Vec<u8>)]) -> Batch {
        let mut out: Vec<Piece> = Vec::with_capacity(self.pieces.len() + edits.len() * 2);
        let added: usize = edits.iter().map(|(_, t)| t.len()).sum();
        let mut add_bytes = Vec::with_capacity(added);
        let base = self.add.len();
        // Ranges past the end are cut to it.
        let len = self.len();
        let mut pos = 0u64;
        // One pass over the pieces: the ranges kept are in document order.
        let mut it = self.pieces.iter().peekable();
        let mut push_range = |out: &mut Vec<Piece>, from: u64, to: u64| {
            if from >= to {
                return;
            }
            while let Some(&(ps, pc)) = it.peek() {
                let pe = ps + pc.len;
                if ps >= to {
                    break;
                }
                if pe > from {
                    out.push(self.sub_piece(&pc, from.max(ps) - ps, to.min(pe) - ps));
                }
                if pe > to {
                    break;
                }
                it.next();
            }
        };
        for (r, text) in edits {
            let s = r.start.max(pos).min(len);
            push_range(&mut out, pos, s);
            if !text.is_empty() {
                let start = base + add_bytes.len() as u64;
                add_bytes.extend_from_slice(text);
                let nl = self.codec.count_nl(text, start);
                out.push(Piece {
                    buf: Buf::Add,
                    start,
                    len: text.len() as u64,
                    nl,
                    exact: true,
                });
            }
            pos = r.end.max(s).min(len);
        }
        push_range(&mut out, pos, len);
        Batch {
            tree: PieceTree::from_vec(out),
            add: add_bytes,
            base,
        }
    }

    /// Applies a batch built by [`TextBuf::build_batch`] on a snapshot with the same add buffer
    /// length (callers check [`Batch::add_len`] first). O(1) apart from taking the batch's bytes,
    /// which are moved rather than copied when they are large.
    pub fn apply_batch(&mut self, batch: Batch) -> Swap {
        debug_assert_eq!(
            self.add.len(),
            batch.base,
            "batch built on another add buffer"
        );
        // All of the batch's bytes go in as one text, so its pieces stay inside one chunk.
        self.add.push_owned(batch.add);
        let swap = Swap(SwapKind::Whole {
            before: self.pieces.clone(),
            after: batch.tree,
        });
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

    #[test]
    fn rebuilt_from_its_pieces() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut f, b"hello world\nsecond line\n").unwrap();
        let src = Arc::new(Source::open(f.path(), crate::file::OpenMode::Auto).unwrap());
        let mut t = TextBuf::new(src.clone(), LineIndex::new(src.len()), Codec::utf8());
        t.replace(0..5, b"HELLO");
        t.replace(12..12, b"inserted\n");
        let pieces: Vec<(Buf, u64, u64)> = t.pieces().map(|p| (p.buf, p.start, p.len)).collect();
        let back = TextBuf::with_pieces(
            src.clone(),
            LineIndex::new(src.len()),
            Codec::utf8(),
            &pieces,
            t.add_bytes_from(0),
        )
        .unwrap();
        assert_eq!(
            &*back.read(0..back.len()).unwrap(),
            b"HELLO world\ninserted\nsecond line\n"
        );
        assert_eq!(back.line_count().0, t.line_count().0);
        // A piece past the end is refused.
        assert!(TextBuf::with_pieces(
            src.clone(),
            LineIndex::new(src.len()),
            Codec::utf8(),
            &[(Buf::Orig, 0, 999)],
            Vec::new()
        )
        .is_none());
        assert!(TextBuf::with_pieces(
            src,
            LineIndex::new(0),
            Codec::utf8(),
            &[(Buf::Add, 0, 3)],
            b"ab".to_vec()
        )
        .is_none());
    }
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
        assert_eq!(t.piece_count(), 3);
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
        let edits = vec![
            (0..3, b"X".to_vec()),
            (8..11, b"YY\n".to_vec()),
            (16..19, Vec::new()),
        ];
        let batch = t.build_batch(&edits);
        let mut t2 = t.clone();
        let swap = t2.apply_batch(batch);
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

    /// Edit costs on large piece lists and a large add buffer, with and without a live worker
    /// snapshot. `cargo test --release --lib bench_edit_costs -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn bench_edit_costs() {
        use std::time::Instant;
        let mut rng = 7u64;
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        for target in [100_000usize, 1_000_000] {
            // Every line's first byte replaced in one batch: about two pieces per line.
            let lines = target / 2;
            let base: String = (0..lines).map(|i| format!("line {i:07}\n")).collect();
            let (_f, t) = buf_with(base.as_bytes());
            let edits: Vec<(Range<u64>, Vec<u8>)> = (0..lines as u64)
                .map(|i| (i * 13..i * 13 + 1, b"L".to_vec()))
                .collect();
            let at = Instant::now();
            let batch = t.build_batch(&edits);
            let built = at.elapsed();
            let mut t = t.clone();
            t.blocking = false;
            let at = Instant::now();
            t.apply_batch(batch);
            let applied = at.elapsed();
            let keys = 2000;
            let mut run = |t: &mut TextBuf, mode: u8| {
                let mut keep = None;
                let at = Instant::now();
                for _ in 0..keys {
                    if mode == 2 || (mode == 1 && keep.is_none()) {
                        keep = Some(t.snapshot());
                    }
                    let off = next() % (t.len() + 1);
                    t.replace(off..off, b"k");
                }
                drop(keep);
                at.elapsed().as_secs_f64() * 1e6 / keys as f64
            };
            let plain = run(&mut t, 0);
            let once = run(&mut t, 1);
            let each = run(&mut t, 2);
            eprintln!(
                "{target:>8} pieces: build_batch (worker) {:.1} ms, apply_batch (UI) {:.3} ms; \
                 insert {plain:.1} us, with a snapshot {once:.1} us, with a new snapshot per key \
                 {each:.1} us",
                built.as_secs_f64() * 1e3,
                applied.as_secs_f64() * 1e3
            );
        }
        // A 100 MB paste, then typing while a worker holds a snapshot taken before each key.
        let (_f, mut t) = buf_with(b"some text\n");
        t.blocking = false;
        t.replace(5..5, &vec![b'p'; 100 << 20]);
        let keys = 50;
        let at = Instant::now();
        let mut keep = Vec::new();
        for i in 0..keys {
            keep.push(t.snapshot());
            t.replace(5 + i..5 + i, b"k");
            keep.truncate(1);
        }
        eprintln!(
            "100 MB add buffer: insert with a new snapshot per key {:.1} us",
            at.elapsed().as_secs_f64() * 1e6 / keys as f64
        );
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

    /// One edit on the reference model, reversible.
    enum ModelEdit {
        Splice {
            at: usize,
            old: Vec<u8>,
            new: Vec<u8>,
        },
        Whole {
            old: Vec<u8>,
            new: Vec<u8>,
        },
    }

    impl ModelEdit {
        fn apply(&self, model: &mut Vec<u8>, forward: bool) {
            match self {
                ModelEdit::Splice { at, old, new } => {
                    let (from, to) = if forward { (old, new) } else { (new, old) };
                    model.splice(*at..*at + from.len(), to.iter().copied());
                }
                ModelEdit::Whole { old, new } => {
                    let to = if forward { new } else { old };
                    model.clone_from(to);
                }
            }
        }
    }

    /// Checks content, structure and (once the index is built) a sample of lines.
    fn check_model(t: &mut TextBuf, model: &[u8], next: &mut impl FnMut() -> u64) {
        t.pieces.check();
        assert_eq!(t.len(), model.len() as u64);
        assert_eq!(t.read(0..t.len()).unwrap().as_ref(), model);
        let mut buf = Vec::new();
        let len = model.len() as u64;
        for _ in 0..20 {
            let a = next() % (len + 1);
            let b = (a + next() % 3000).min(len);
            assert_eq!(
                t.read(a..b).unwrap().as_ref(),
                &model[a as usize..b as usize]
            );
            assert_eq!(t.read_to(a..b, &mut buf), Some((b - a) as usize));
            assert_eq!(&buf[..(b - a) as usize], &model[a as usize..b as usize]);
        }
        if !t.index.is_done() {
            // Estimated counts: only that the mapping answers.
            assert!(!t.has_inexact() || !t.line_count().1);
            for _ in 0..20 {
                let (l, _) = t.line_of(next() % (len + 1));
                t.line_start(l);
            }
            return;
        }
        while t.refresh_counts(512) {}
        assert!(!t.has_inexact());
        let starts: Vec<u64> = std::iter::once(0)
            .chain(
                model
                    .iter()
                    .enumerate()
                    .filter(|&(_, &b)| b == b'\n')
                    .map(|(i, _)| i as u64 + 1),
            )
            .collect();
        assert_eq!(t.line_count(), (starts.len() as u64, true));
        for _ in 0..100 {
            let l = (next() % starts.len() as u64) as usize;
            assert_eq!(t.line_start(l as u64), Some((starts[l], true)), "line {l}");
            assert_eq!(t.line_of(starts[l]), (l as u64, true));
            let off = next() % (len + 1);
            let line = starts.partition_point(|&s| s <= off) - 1;
            assert_eq!(t.line_of(off), (line as u64, true));
        }
        assert_eq!(t.line_start(starts.len() as u64), None);
    }

    /// Thousands of random edits (replaces, typing runs, big deletes, duplicated pieces,
    /// batches, big inserts) with undo and redo interleaved, snapshots kept across later edits,
    /// estimated newline counts refreshed once the index is built, and journal round trips.
    #[test]
    fn random_edits_tree_stress() {
        // Over 1 MB with no index yet: pieces of the original get estimated counts.
        let base: String = (0..60_000)
            .map(|i| format!("row {i:06} {}\n", "x".repeat(i % 37)))
            .collect();
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(base.as_bytes()).unwrap();
        f.flush().unwrap();
        let src = Arc::new(Source::open(f.path(), OpenMode::Auto).unwrap());
        let mut t = TextBuf::new(src.clone(), LineIndex::new(src.len()), Codec::utf8());
        t.blocking = true;
        assert!(t.has_inexact());
        let mut model = base.into_bytes();
        let mut rng = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        let mut done: Vec<(Swap, ModelEdit)> = Vec::new();
        let mut undone: Vec<(Swap, ModelEdit)> = Vec::new();
        let mut snaps: Vec<(TextBuf, Vec<u8>)> = Vec::new();
        let mut caret = 0u64;
        let (mut max_leaves, mut merged) = (0, false);
        let rounds = 8000;
        for round in 0..rounds {
            if round == rounds / 2 {
                // The index finishes: estimated counts become exact on refresh.
                t.index = index::build_blocking(&src, Codec::utf8());
                check_model(&mut t, &model, &mut next);
            }
            let len = model.len() as u64;
            let leaves_before = t.pieces.leaf_count();
            let rec = match next() % 100 {
                0..=11 if !done.is_empty() => {
                    let (s, m) = done.pop().unwrap();
                    t.apply(&s, false);
                    m.apply(&mut model, false);
                    undone.push((s, m));
                    None
                }
                12..=17 if !undone.is_empty() => {
                    let (s, m) = undone.pop().unwrap();
                    t.apply(&s, true);
                    m.apply(&mut model, true);
                    done.push((s, m));
                    None
                }
                18..=39 => {
                    // A typing run at the caret: coalesces into one add piece.
                    caret = caret.min(len);
                    let c = [b'a' + (next() % 26) as u8];
                    let s = t.replace(caret..caret, &c);
                    let m = ModelEdit::Splice {
                        at: caret as usize,
                        old: vec![],
                        new: c.to_vec(),
                    };
                    caret += 1;
                    Some((s, m))
                }
                40..=41 => {
                    // A big delete across many pieces and leaves.
                    let a = next() % (len + 1);
                    let b = (a + next() % (len / 4).clamp(1, 200_000)).min(len);
                    let old = model[a as usize..b as usize].to_vec();
                    let m = ModelEdit::Splice {
                        at: a as usize,
                        old,
                        new: vec![],
                    };
                    Some((t.replace(a..b, b""), m))
                }
                42..=44 => {
                    // Duplicating a range by its pieces.
                    let a = next() % (len + 1);
                    let b = (a + next() % 5000).min(len);
                    let at = next() % (len + 1);
                    let pcs = t.copy_pieces(a..b);
                    let m = ModelEdit::Splice {
                        at: at as usize,
                        old: vec![],
                        new: model[a as usize..b as usize].to_vec(),
                    };
                    Some((t.insert_pieces(at, pcs), m))
                }
                45 => {
                    // A batch of replacements over the whole document, built on a snapshot.
                    let mut edits = Vec::new();
                    let mut p = 0u64;
                    while edits.len() < 300 {
                        let a = p + next() % 4000;
                        if a > len {
                            break;
                        }
                        let b = (a + next() % 20).min(len);
                        let text = if next() % 2 == 0 {
                            b"B\n".to_vec()
                        } else {
                            vec![]
                        };
                        edits.push((a..b, text));
                        p = b + 1;
                    }
                    let batch = t.snapshot().build_batch(&edits);
                    let mut new = model.clone();
                    for (r, text) in edits.iter().rev() {
                        new.splice(r.start as usize..r.end as usize, text.iter().copied());
                    }
                    let m = ModelEdit::Whole {
                        old: model.clone(),
                        new,
                    };
                    Some((t.apply_batch(batch), m))
                }
                46 if next() % 4 == 0 => {
                    // A big insert: a chunk of the add buffer of its own.
                    let at = next() % (len + 1);
                    let text: Vec<u8> = (0..(next() % (1 << 21)) as usize)
                        .map(|i| if i % 50 == 49 { b'\n' } else { b'q' })
                        .collect();
                    let m = ModelEdit::Splice {
                        at: at as usize,
                        old: vec![],
                        new: text.clone(),
                    };
                    Some((t.replace(at..at, &text), m))
                }
                _ => {
                    let a = next() % (len + 1);
                    let b = (a + next() % 60).min(len);
                    let text: &[u8] = match next() % 4 {
                        0 => b"",
                        1 => b"X\nY",
                        2 => b"\n",
                        _ => b"zz",
                    };
                    caret = a + text.len() as u64;
                    let m = ModelEdit::Splice {
                        at: a as usize,
                        old: model[a as usize..b as usize].to_vec(),
                        new: text.to_vec(),
                    };
                    Some((t.replace(a..b, text), m))
                }
            };
            if let Some((s, m)) = rec {
                m.apply(&mut model, true);
                done.push((s, m));
                undone.clear();
            }
            assert_eq!(t.len(), model.len() as u64, "round {round}");
            let leaves = t.pieces.leaf_count();
            max_leaves = max_leaves.max(leaves);
            merged |= leaves < leaves_before;
            if next() % 40 == 0 {
                snaps.push((t.snapshot(), model.clone()));
                if snaps.len() > 4 {
                    snaps.remove(next() as usize % snaps.len());
                }
            }
            if round % 400 == 0 {
                check_model(&mut t, &model, &mut next);
                for (s, m) in &snaps {
                    assert_eq!(s.read(0..s.len()).unwrap().as_ref(), m.as_slice());
                    s.pieces.check();
                }
                // The recovery journal's view of the buffer rebuilds the same text.
                let list: Vec<(Buf, u64, u64)> =
                    t.pieces().map(|p| (p.buf, p.start, p.len)).collect();
                let mut back = TextBuf::with_pieces(
                    src.clone(),
                    t.index.clone(),
                    Codec::utf8(),
                    &list,
                    t.add_bytes_from(0),
                )
                .unwrap();
                back.blocking = true;
                check_model(&mut back, &model, &mut next);
            }
        }
        check_model(&mut t, &model, &mut next);
        assert!(max_leaves > 20, "leaves never split: {max_leaves}");
        assert!(merged, "leaves never merged");
        assert!(
            t.add.chunk_count() > 3,
            "add buffer in {} chunks",
            t.add.chunk_count()
        );
        for (s, m) in &snaps {
            assert_eq!(s.read(0..s.len()).unwrap().as_ref(), m.as_slice());
        }
        // Everything undone gives back the original file.
        while let Some((s, m)) = done.pop() {
            t.apply(&s, false);
            m.apply(&mut model, false);
        }
        assert!(t.is_pristine());
        check_model(&mut t, &model, &mut next);
    }

    #[test]
    fn add_buffer_never_copies_shared_old_bytes() {
        let mut a = AddBuf::default();
        let s = a.push(&vec![b'p'; 3 << 20]);
        let snap = a.clone();
        // A shared last chunk larger than the copy limit is left alone.
        let t = a.push(b"typed");
        assert_eq!((s, t), (0, 3 << 20));
        assert_eq!(a.chunk_count(), 2);
        assert!(!a.contiguous(t - 1, t + 1));
        assert!(a.contiguous(t, t + 5));
        assert_eq!(a.slice(t, t + 5), b"typed");
        // Unshared: appended in place.
        a.push(b"more");
        assert_eq!(a.chunk_count(), 2);
        assert_eq!(snap.len(), 3 << 20);
        assert_eq!(a.copy_from(t), b"typedmore");
        assert_eq!(a.copy_from(t + 2), b"pedmore");
        assert_eq!(a.copy_from(0).len(), (3 << 20) + 9);
        // A small shared last chunk is copied rather than split.
        let snap2 = a.clone();
        a.push(b"!");
        assert_eq!(a.chunk_count(), 2);
        assert_eq!(snap2.copy_from(t), b"typedmore");
        assert_eq!(a.copy_from(t), b"typedmore!");
    }

    #[test]
    fn typing_after_a_chunk_switch_does_not_merge_across_chunks() {
        let (_f, mut t) = buf_with(b"ab");
        t.replace(1..1, &vec![b'x'; 2 << 20]);
        let _snap = t.snapshot();
        let n = t.piece_count();
        t.replace(1..1, b"y");
        t.replace(2..2, b"z");
        assert_eq!(t.piece_count(), n + 1);
        assert_eq!(&*t.read(0..3).unwrap(), b"ayz");
    }

    /// A batch is built on a snapshot taken on another thread and applied later; its large
    /// bytes reach the add buffer without a copy, and small ones still join the last chunk.
    #[test]
    fn batch_built_on_a_worker_moves_its_bytes() {
        let (_f, mut t) = buf_with(&b"0123456789\n".repeat(20_000));
        t.replace(0..0, b">");
        let snap = t.snapshot();
        let big = vec![b'x'; 200_000];
        let batch =
            std::thread::spawn(move || snap.build_batch(&[(1..11, big), (13..14, b"y".to_vec())]))
                .join()
                .unwrap();
        assert_eq!(batch.add_len(), t.add_len());
        let ptr = batch.add.as_ptr();
        let base = t.add_len();
        let swap = t.apply_batch(batch);
        assert_eq!(
            t.add.slice(base, base + 1).as_ptr(),
            ptr,
            "bytes were copied"
        );
        let want = format!(">{}\n0y23456789\n", "x".repeat(200_000));
        assert_eq!(&text(&t)[..want.len()], want);
        check_lines(&t);
        t.apply(&swap, false);
        assert_eq!(&text(&t)[..22], ">0123456789\n0123456789");
        // A small batch is copied into the last chunk as before.
        let chunks = t.add.chunk_count();
        let batch = t.build_batch(&[(1..2, b"Z".to_vec())]);
        t.apply_batch(batch);
        assert_eq!(t.add.chunk_count(), chunks);
        assert_eq!(&text(&t)[..4], ">Z12");
    }

    #[test]
    fn batch_undo_and_redo() {
        let (_f, mut t) = buf_with(b"aaa bbb aaa\n");
        let s1 = t.replace(0..0, b">");
        let batch = t.build_batch(&[(1..4, b"X".to_vec())]);
        let s2 = t.apply_batch(batch);
        assert_eq!(text(&t), ">X bbb aaa\n");
        let s3 = t.replace(2..2, b"!");
        assert_eq!(text(&t), ">X! bbb aaa\n");
        for s in [&s3, &s2, &s1] {
            t.apply(s, false);
        }
        assert!(t.is_pristine());
        for (s, want) in [
            (&s1, ">aaa bbb aaa\n"),
            (&s2, ">X bbb aaa\n"),
            (&s3, ">X! bbb aaa\n"),
        ] {
            t.apply(s, true);
            assert_eq!(text(&t), want);
            check_lines(&t);
        }
    }
}
