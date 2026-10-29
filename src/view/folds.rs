//! Folded objects, arrays and tags: the lines between an opening line and its closing line are
//! hidden, and stepping through rows jumps over them.

use bigtext::text::rows::{self, Pending, Row};
use bigtext::text::TextBuf;

/// A folded value: the line holding its opening bracket stays, the lines up to the one holding
/// the closing bracket are hidden.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fold {
    /// Start of the opening line.
    pub start: u64,
    /// Start of the first hidden line.
    pub hide_from: u64,
    /// Start of the closing line (shown).
    pub end: u64,
    /// Hidden lines.
    pub lines: u64,
}

/// The fold hiding `off` (folds are sorted and do not overlap).
pub fn hidden_by(folds: &[Fold], off: u64) -> Option<&Fold> {
    let i = folds.partition_point(|f| f.hide_from <= off).checked_sub(1)?;
    (off < folds[i].end).then_some(&folds[i])
}

/// The fold whose opening line starts at `start`.
pub fn starting_at(folds: &[Fold], start: u64) -> Option<&Fold> {
    folds.binary_search_by_key(&start, |f| f.start).ok().map(|i| &folds[i])
}

/// Adds `f`, dropping the folds inside it; one inside a fold is not added.
pub fn insert(folds: &mut Vec<Fold>, f: Fold) {
    if hidden_by(folds, f.start).is_some() || folds.iter().any(|x| x.start == f.start) {
        return;
    }
    folds.retain(|x| !(x.start >= f.hide_from && x.start < f.end));
    let i = folds.partition_point(|x| x.start < f.start);
    folds.insert(i, f);
}

pub fn next_row(buf: &TextBuf, row: &Row, wrap: Option<usize>, folds: &[Fold]) -> Result<Option<Row>, Pending> {
    let mut next = rows::next_row(buf, row, wrap)?;
    while let Some(f) = next.and_then(|r| hidden_by(folds, r.start)).copied() {
        next = Some(rows::row_at(buf, f.end, wrap)?);
    }
    Ok(next)
}

pub fn prev_row(buf: &TextBuf, row: &Row, wrap: Option<usize>, folds: &[Fold]) -> Result<Option<Row>, Pending> {
    let mut prev = rows::prev_row(buf, row, wrap)?;
    while let Some(f) = prev.and_then(|r| hidden_by(folds, r.start)).copied() {
        // The last row of the opening line.
        prev = Some(rows::row_at(buf, f.hide_from.saturating_sub(1).max(f.start), wrap)?);
    }
    Ok(prev)
}

/// [`rows::step_rows`] over the rows that are shown.
pub fn step_rows(buf: &TextBuf, row: Row, n: i64, wrap: Option<usize>, folds: &[Fold]) -> Result<Row, Pending> {
    if folds.is_empty() {
        return rows::step_rows(buf, row, n, wrap);
    }
    let mut r = row;
    for _ in 0..n.unsigned_abs() {
        let step = if n > 0 { next_row(buf, &r, wrap, folds)? } else { prev_row(buf, &r, wrap, folds)? };
        match step {
            Some(x) => r = x,
            None => break,
        }
    }
    Ok(r)
}

/// How far a line's start or end is looked for.
const LINE_LOOK: u64 = 1 << 20;

/// Start of the line holding `off` (`None` past `LINE_LOOK` bytes).
fn line_start(buf: &TextBuf, off: u64) -> Option<u64> {
    let from = off.saturating_sub(LINE_LOOK);
    let b = buf.read(from..off)?;
    match b.iter().rposition(|&c| c == b'\n') {
        Some(i) => Some(from + i as u64 + 1),
        None if from == 0 => Some(buf.codec.bom_len as u64),
        None => None,
    }
}

/// Start of the line after the one holding `off`.
fn next_line(buf: &TextBuf, off: u64) -> Option<u64> {
    let to = (off + LINE_LOOK).min(buf.len());
    let b = buf.read(off..to)?;
    b.iter().position(|&c| c == b'\n').map(|i| off + i as u64 + 1)
}

/// The fold from the bracket or tag at `open` to the one at `close`, when lines lie between them.
pub fn fold_between(buf: &TextBuf, open: u64, close: u64) -> Option<Fold> {
    let start = line_start(buf, open)?;
    let hide_from = next_line(buf, open)?;
    let end = line_start(buf, close)?;
    if hide_from >= end {
        return None;
    }
    let lines = buf.line_of(end).0.saturating_sub(buf.line_of(hide_from).0);
    Some(Fold { start, hide_from, end, lines: lines.max(1) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(start: u64, hide_from: u64, end: u64) -> Fold {
        Fold { start, hide_from, end, lines: 1 }
    }

    #[test]
    fn insert_keeps_outer_folds_sorted() {
        let mut v = Vec::new();
        insert(&mut v, f(50, 60, 90));
        insert(&mut v, f(10, 20, 40));
        insert(&mut v, f(0, 5, 100));
        assert_eq!(v, [f(0, 5, 100)]);
        // Inside a fold: not added.
        insert(&mut v, f(30, 35, 45));
        assert_eq!(v.len(), 1);
        insert(&mut v, f(100, 110, 120));
        assert_eq!(hidden_by(&v, 4), None);
        assert_eq!(hidden_by(&v, 5).map(|x| x.start), Some(0));
        assert_eq!(hidden_by(&v, 100), None);
        assert_eq!(hidden_by(&v, 115).map(|x| x.start), Some(100));
        assert_eq!(starting_at(&v, 100).map(|x| x.end), Some(120));
    }
}
