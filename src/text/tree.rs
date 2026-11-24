//! The piece list of a [`TextBuf`](super::TextBuf): a two-level chunked sequence.
//!
//! Pieces live in leaves of at most [`LEAF_MAX`] pieces, each behind an `Arc` with its byte and
//! newline totals; the top level is a list of leaves with prefix sums of bytes, newlines and
//! pieces. Finding a piece is a binary search over the leaves plus a scan of one leaf. An edit
//! changes one leaf (two or three when it splits or merges) and rebuilds the top-level sums:
//! O(P / LEAF_MAX + LEAF_MAX) instead of O(P). Cloning is O(1); a write after a clone (a worker
//! snapshot) copies only the top-level list and the leaves it touches.

use std::sync::Arc;

use super::buffer::Piece;

/// Most pieces in one leaf.
pub(crate) const LEAF_MAX: usize = 512;
/// Fewest pieces in a leaf when there is more than one leaf (smaller ones merge with a neighbour).
const LEAF_MIN: usize = LEAF_MAX / 4;

#[derive(Clone, Debug, Default)]
struct Leaf {
    pieces: Vec<Piece>,
    bytes: u64,
    lines: u64,
    /// Index of the first piece with an estimated newline count.
    first_inexact: Option<usize>,
}

impl Leaf {
    fn new(pieces: Vec<Piece>) -> Leaf {
        let mut l = Leaf {
            pieces,
            ..Leaf::default()
        };
        l.recount();
        l
    }

    fn recount(&mut self) {
        self.bytes = self.pieces.iter().map(|p| p.len).sum();
        self.lines = self.pieces.iter().map(|p| p.nl).sum();
        self.first_inexact = self.pieces.iter().position(|p| !p.exact);
    }
}

/// Totals of the leaves before one.
#[derive(Clone, Copy, Debug, Default)]
struct Sum {
    bytes: u64,
    lines: u64,
    pieces: usize,
}

/// A piece's place in the tree and the totals of everything before it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Pos {
    leaf: usize,
    idx: usize,
    /// Index of the piece in the whole list.
    pub index: usize,
    /// Document offset where the piece starts.
    pub start: u64,
    /// Newline units before the piece.
    pub lines: u64,
    pub piece: Piece,
}

#[derive(Clone, Debug)]
pub(crate) struct PieceTree {
    leaves: Arc<Vec<Arc<Leaf>>>,
    /// `sums[i]` = totals of `leaves[..i]`; one more entry than leaves.
    sums: Arc<Vec<Sum>>,
    /// Index of the first piece with an estimated newline count (`len()` if none).
    inexact_from: usize,
}

impl Default for PieceTree {
    fn default() -> PieceTree {
        PieceTree {
            leaves: Arc::default(),
            sums: Arc::new(vec![Sum::default()]),
            inexact_from: 0,
        }
    }
}

/// Splits `pieces` into evenly filled leaves of at most [`LEAF_MAX`] pieces.
fn make_leaves(pieces: &[Piece]) -> Vec<Arc<Leaf>> {
    if pieces.is_empty() {
        return Vec::new();
    }
    let count = pieces.len().div_ceil(LEAF_MAX);
    let (base, extra) = (pieces.len() / count, pieces.len() % count);
    let mut out = Vec::with_capacity(count);
    let mut s = 0;
    for i in 0..count {
        let e = s + base + usize::from(i < extra);
        out.push(Arc::new(Leaf::new(pieces[s..e].to_vec())));
        s = e;
    }
    out
}

impl PieceTree {
    pub fn from_vec(pieces: Vec<Piece>) -> PieceTree {
        let mut t = PieceTree {
            leaves: Arc::new(make_leaves(&pieces)),
            ..PieceTree::default()
        };
        t.rebuild_sums(0);
        t
    }

    /// Number of pieces.
    pub fn len(&self) -> usize {
        self.total().pieces
    }

    pub fn bytes(&self) -> u64 {
        self.total().bytes
    }

    pub fn lines(&self) -> u64 {
        self.total().lines
    }

    pub fn inexact_from(&self) -> usize {
        self.inexact_from
    }

    fn total(&self) -> Sum {
        *self.sums.last().unwrap()
    }

    pub fn first(&self) -> Option<&Piece> {
        self.leaves.first().and_then(|l| l.pieces.first())
    }

    fn pos(&self, leaf: usize, idx: usize, start: u64, lines: u64) -> Pos {
        Pos {
            leaf,
            idx,
            index: self.sums[leaf].pieces + idx,
            start,
            lines,
            piece: self.leaves[leaf].pieces[idx],
        }
    }

    /// Scans leaf `leaf` for the first piece where `stop(start, lines, piece)` holds (the last
    /// piece of the leaf if none does).
    fn scan(&self, leaf: usize, stop: impl Fn(u64, u64, &Piece) -> bool) -> Pos {
        let l = &self.leaves[leaf];
        let (mut start, mut lines) = (self.sums[leaf].bytes, self.sums[leaf].lines);
        let last = l.pieces.len() - 1;
        for (i, p) in l.pieces.iter().enumerate() {
            if i == last || stop(start, lines, p) {
                return self.pos(leaf, i, start, lines);
            }
            start += p.len;
            lines += p.nl;
        }
        unreachable!("leaves are never empty")
    }

    /// The piece containing document offset `off` (the last piece for `off >= bytes()`).
    pub fn at_offset(&self, off: u64) -> Option<Pos> {
        let n = self.leaves.len();
        if n == 0 {
            return None;
        }
        let leaf = self.sums[1..n].partition_point(|s| s.bytes <= off);
        Some(self.scan(leaf, |start, _, p| start + p.len > off))
    }

    /// The first piece whose end has at least `line` newlines before it (the last piece if
    /// none has).
    pub fn at_line(&self, line: u64) -> Option<Pos> {
        let n = self.leaves.len();
        if n == 0 {
            return None;
        }
        let leaf = self.sums[1..n].partition_point(|s| s.lines < line);
        Some(self.scan(leaf, |_, lines, p| lines + p.nl >= line))
    }

    /// Leaf holding piece `index` and the index inside it (`index == len()` maps to one past
    /// the last piece of the last leaf).
    fn locate(&self, index: usize) -> (usize, usize) {
        let n = self.leaves.len();
        let leaf = self.sums[1..n].partition_point(|s| s.pieces <= index);
        (leaf, index - self.sums[leaf].pieces)
    }

    /// Piece number `index` (`< len()`).
    pub fn get(&self, index: usize) -> Piece {
        let (leaf, idx) = self.locate(index);
        self.leaves[leaf].pieces[idx]
    }

    /// All pieces with their document offsets, from the start.
    pub fn iter(&self) -> Iter<'_> {
        Iter {
            leaves: &self.leaves,
            leaf: 0,
            idx: 0,
            start: 0,
        }
    }

    /// The pieces from `pos` on, with their document offsets.
    pub fn iter_from(&self, pos: &Pos) -> Iter<'_> {
        Iter {
            leaves: &self.leaves,
            leaf: pos.leaf,
            idx: pos.idx,
            start: pos.start,
        }
    }

    /// Replaces `remove` pieces from `at` with `insert`.
    pub fn splice(&mut self, at: usize, remove: usize, insert: &[Piece]) {
        let total = self.len();
        assert!(
            at <= total && remove <= total - at,
            "splice {at}+{remove} of {total}"
        );
        let n = self.leaves.len();
        if n == 0 {
            *self = PieceTree::from_vec(insert.to_vec());
            return;
        }
        let (a, la) = self.locate(at);
        let (b, lb) = if remove == 0 {
            (a, la)
        } else {
            let (b, i) = self.locate(at + remove - 1);
            (b, i + 1)
        };
        if a == b {
            let new_len = self.leaves[a].pieces.len() - (lb - la) + insert.len();
            if new_len > 0 && new_len <= LEAF_MAX && (new_len >= LEAF_MIN || n == 1) {
                // The common case: the edit stays inside one leaf.
                let leaves = Arc::make_mut(&mut self.leaves);
                let leaf = Arc::make_mut(&mut leaves[a]);
                leaf.pieces.splice(la..lb, insert.iter().copied());
                leaf.recount();
                self.rebuild_sums(a);
                return;
            }
        }
        let (mut first, mut last) = (a, b);
        let mut merged = Vec::with_capacity(la + insert.len() + LEAF_MAX);
        merged.extend_from_slice(&self.leaves[a].pieces[..la]);
        merged.extend_from_slice(insert);
        merged.extend_from_slice(&self.leaves[b].pieces[lb..]);
        if merged.len() < LEAF_MIN {
            // Too small to stand alone: take in a neighbour (resplit below if that overflows).
            if last + 1 < n {
                last += 1;
                merged.extend_from_slice(&self.leaves[last].pieces);
            } else if first > 0 {
                first -= 1;
                merged.splice(0..0, self.leaves[first].pieces.iter().copied());
            }
        }
        Arc::make_mut(&mut self.leaves).splice(first..=last, make_leaves(&merged));
        self.rebuild_sums(first);
    }

    /// Pieces with estimated newline counts, from the first one on, at most `budget`.
    pub fn inexact(&self, budget: usize) -> Vec<(usize, Piece)> {
        let mut out = Vec::new();
        if self.inexact_from >= self.len() {
            return out;
        }
        let (leaf, _) = self.locate(self.inexact_from);
        for (li, l) in self.leaves.iter().enumerate().skip(leaf) {
            let Some(fi) = l.first_inexact else { continue };
            let base = self.sums[li].pieces;
            for (i, p) in l.pieces.iter().enumerate().skip(fi) {
                if out.len() >= budget {
                    return out;
                }
                if !p.exact {
                    out.push((base + i, *p));
                }
            }
        }
        out
    }

    /// Sets the newline counts of pieces by index (`updates` sorted by index).
    pub fn set_counts(&mut self, updates: &[(usize, u64, bool)]) {
        let Some(&(first, ..)) = updates.first() else {
            return;
        };
        let from = self.locate(first).0;
        let at: Vec<(usize, usize)> = updates.iter().map(|u| self.locate(u.0)).collect();
        let leaves = Arc::make_mut(&mut self.leaves);
        let mut i = 0;
        while i < updates.len() {
            let leaf = at[i].0;
            let l = Arc::make_mut(&mut leaves[leaf]);
            while i < updates.len() && at[i].0 == leaf {
                let (_, nl, exact) = updates[i];
                l.pieces[at[i].1].nl = nl;
                l.pieces[at[i].1].exact = exact;
                i += 1;
            }
            l.recount();
        }
        self.rebuild_sums(from);
    }

    /// Recomputes the sums after `leaves[from..]` changed (those before are untouched).
    fn rebuild_sums(&mut self, from: usize) {
        let sums = Arc::make_mut(&mut self.sums);
        sums.truncate(from + 1);
        let mut s = sums[from];
        for l in &self.leaves[from..] {
            s.bytes += l.bytes;
            s.lines += l.lines;
            s.pieces += l.pieces.len();
            sums.push(s);
        }
        if self.inexact_from >= sums[from].pieces {
            self.inexact_from = s.pieces;
            for (i, l) in self.leaves.iter().enumerate().skip(from) {
                if let Some(fi) = l.first_inexact {
                    self.inexact_from = sums[i].pieces + fi;
                    break;
                }
            }
        }
    }

    /// Checks the structure's invariants (tests).
    #[cfg(test)]
    pub fn check(&self) {
        let n = self.leaves.len();
        assert_eq!(self.sums.len(), n + 1);
        let mut s = Sum::default();
        let mut inexact = None;
        for (i, l) in self.leaves.iter().enumerate() {
            assert!(!l.pieces.is_empty() && l.pieces.len() <= LEAF_MAX);
            assert!(
                n == 1 || l.pieces.len() >= LEAF_MIN,
                "leaf {i} of {n} too small"
            );
            let copy = Leaf::new(l.pieces.clone());
            assert_eq!(
                (copy.bytes, copy.lines, copy.first_inexact),
                (l.bytes, l.lines, l.first_inexact)
            );
            assert_eq!(
                (self.sums[i].bytes, self.sums[i].lines, self.sums[i].pieces),
                (s.bytes, s.lines, s.pieces)
            );
            if inexact.is_none() {
                inexact = l.first_inexact.map(|f| s.pieces + f);
            }
            assert!(l.pieces.iter().all(|p| p.len > 0));
            s.bytes += l.bytes;
            s.lines += l.lines;
            s.pieces += l.pieces.len();
        }
        assert_eq!(self.inexact_from, inexact.unwrap_or(s.pieces));
    }

    #[cfg(test)]
    pub fn leaf_count(&self) -> usize {
        self.leaves.len()
    }
}

/// Pieces in order with their document offsets.
pub(crate) struct Iter<'a> {
    leaves: &'a [Arc<Leaf>],
    leaf: usize,
    idx: usize,
    start: u64,
}

impl Iterator for Iter<'_> {
    type Item = (u64, Piece);

    fn next(&mut self) -> Option<(u64, Piece)> {
        loop {
            let l = self.leaves.get(self.leaf)?;
            if let Some(p) = l.pieces.get(self.idx) {
                let s = self.start;
                self.start += p.len;
                self.idx += 1;
                return Some((s, *p));
            }
            self.leaf += 1;
            self.idx = 0;
        }
    }
}
