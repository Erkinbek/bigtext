//! Column (block) selection: Alt+drag selects a rectangle of cells across rows. Copy copies it
//! row by row; typing, pasting, Backspace and Delete change every row of it at once, as one
//! undoable edit. Only without word wrap, where a row is a line (or a 64 KB slice of a longer
//! one).

use std::ops::Range;

use bigtext::text::rows::{self, Pending, Row};

use crate::app::document::Document;

/// A block spans at most this many rows (copying and editing run on the UI thread).
pub const BLOCK_MAX_ROWS: usize = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Block {
    /// Start of the row where the drag began, and the cell column there.
    pub anchor: u64,
    pub anchor_cell: u32,
    /// Start of the row under the pointer, and its cell column.
    pub caret: u64,
    pub caret_cell: u32,
}

impl Block {
    pub fn at(row: u64, cell: u32) -> Block {
        Block { anchor: row, anchor_cell: cell, caret: row, caret_cell: cell }
    }

    /// Left and right cell columns.
    pub fn cells(&self) -> (u32, u32) {
        (self.anchor_cell.min(self.caret_cell), self.anchor_cell.max(self.caret_cell))
    }

    /// Starts of the top and bottom rows.
    pub fn span(&self) -> (u64, u64) {
        (self.anchor.min(self.caret), self.anchor.max(self.caret))
    }

    pub fn contains_row(&self, row_start: u64) -> bool {
        let (a, b) = self.span();
        (a..=b).contains(&row_start)
    }
}

pub enum BlockEdit {
    /// Replace each row's part with this text.
    Insert(String),
    /// Replace each row's part with its own line of text.
    PerRow(Vec<String>),
    Backspace,
    Delete,
}

/// The rows of the block, top to bottom.
pub fn block_rows(doc: &Document, b: &Block) -> Result<Vec<Row>, Pending> {
    let (lo, hi) = b.span();
    let mut r = rows::row_at(&doc.buf, lo, None)?;
    let mut v = vec![r];
    while r.start < hi && v.len() < BLOCK_MAX_ROWS {
        match rows::next_row(&doc.buf, &r, None)? {
            Some(n) if n.start <= hi => {
                v.push(n);
                r = n;
            }
            _ => break,
        }
    }
    Ok(v)
}

/// Byte range of each row's part of the block.
fn parts(doc: &Document, b: &Block, rows: &[Row]) -> Result<Vec<Range<u64>>, Pending> {
    let (lo, hi) = b.cells();
    rows.iter().map(|r| Ok(rows::offset_at_cell(&doc.buf, r, lo)?..rows::offset_at_cell(&doc.buf, r, hi)?)).collect()
}

/// The block's text, one line per row (for the clipboard).
pub fn block_text(doc: &Document, b: &Block) -> Option<String> {
    let rows = block_rows(doc, b).ok()?;
    let eol = if doc.line_ending == bigtext::encoding::LineEnding::CrLf { "\r\n" } else { "\n" };
    let mut out = String::new();
    for (i, r) in parts(doc, b, &rows).ok()?.into_iter().enumerate() {
        if i > 0 {
            out.push_str(eol);
        }
        out.push_str(&doc.buf.codec.decode_lossy(&doc.buf.read(r)?));
    }
    Some(out)
}

/// Applies `e` to every row of the block as one undoable edit and leaves a zero-width block
/// (a caret on each row) after the change. Returns false if nothing could be done.
pub fn edit_block(doc: &mut Document, b: &mut Block, e: BlockEdit) -> bool {
    if !doc.editable() {
        return false;
    }
    let Ok(rows) = block_rows(doc, b) else { return false };
    let Ok(ranges) = parts(doc, b, &rows) else { return false };
    let codec = doc.buf.codec;
    let narrow = b.anchor_cell == b.caret_cell;
    let mut edits: Vec<(Range<u64>, Vec<u8>)> = Vec::with_capacity(rows.len());
    for (i, (row, r)) in rows.iter().zip(ranges).enumerate() {
        let (mut s, mut end) = (r.start, r.end);
        match &e {
            BlockEdit::Backspace if narrow && s > row.start => s = doc.buf.prev_char(s),
            BlockEdit::Delete if narrow && end < row.end => end = doc.buf.next_char(end),
            _ => {}
        }
        let text = match &e {
            BlockEdit::Insert(t) => codec.encode(t),
            BlockEdit::PerRow(v) => codec.encode(v.get(i).map(String::as_str).unwrap_or("")),
            _ => Vec::new(),
        };
        if s != end || !text.is_empty() {
            edits.push((s..end, text));
        }
    }
    let Some((first, text)) = edits.first() else { return false };
    let caret = first.start + text.len() as u64;
    // A row start moves by the size change of the edits in the rows above it.
    let shift = |x: u64| -> u64 { edits.iter().filter(|(r, _)| r.start < x).fold(x as i64, |a, (r, t)| a + t.len() as i64 - (r.end - r.start) as i64) as u64 };
    let (anchor_row, caret_row) = (shift(b.anchor), shift(b.caret));
    let (pieces, add) = doc.buf.build_batch(&edits);
    let swap = doc.buf.apply_batch(pieces, &add);
    doc.record(swap, (caret, caret));
    let cell = rows::row_at(&doc.buf, caret, None).and_then(|r| rows::cell_of_offset(&doc.buf, &r, caret)).unwrap_or(b.cells().0);
    *b = Block { anchor: anchor_row, anchor_cell: cell, caret: caret_row, caret_cell: cell };
    true
}
