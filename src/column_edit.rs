//! The column editor: a number sequence or the same text in a column of cells, row after row.
//! The editor applies it to a block's rows; [`column_edit_to`] streams it from a line to the
//! end of a file of any size.

use std::io::{self, Write};

use crate::jobs::{CancelToken, Progress};
use crate::lines::LineReader;
use crate::text::rows::for_each_cell;
use crate::text::TextBuf;

/// What goes into the column, row by row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ColumnGen {
    /// `start`, `start + step`, … at least `width` digits (zeros in front), in hex if `hex`.
    Numbers { start: i64, step: i64, width: usize, hex: bool },
    Text(String),
}

impl ColumnGen {
    /// The value for row `i` (0-based).
    pub fn value(&self, i: u64) -> String {
        match self {
            ColumnGen::Numbers { start, step, width, hex } => {
                let v = start.saturating_add(step.saturating_mul(i as i64));
                let digits = if *hex { format!("{:x}", v.unsigned_abs()) } else { v.unsigned_abs().to_string() };
                format!("{}{digits:0>width$}", if v < 0 { "-" } else { "" })
            }
            ColumnGen::Text(t) => t.clone(),
        }
    }
}

/// The byte offset in `line` (at document offset `base`) of cell column `col`, as the editor
/// places a caret there (the end of the line when it is shorter).
pub fn cell_offset(codec: &crate::encoding::Codec, line: &[u8], base: u64, col: u32) -> usize {
    let mut result = line.len();
    for_each_cell(codec, line, base, |c| {
        if c.width == 0 {
            return true;
        }
        if col <= c.cell {
            result = (c.off - base) as usize;
            return false;
        }
        if col < c.cell + c.width {
            let at = (c.off - base) as usize;
            result = if col - c.cell <= c.width / 2 { at } else { at + c.len as usize };
            return false;
        }
        true
    });
    result
}

/// Writes `buf` with cells `cells.0..cells.1` of every line from the one starting at `from`
/// to the end replaced by `gen`'s values (lines above it as they are). Returns how many lines
/// were changed, or `None` when cancelled.
pub fn column_edit_to(buf: &TextBuf, from: u64, cells: (u32, u32), gen: &ColumnGen, out: &mut dyn Write, cancel: &CancelToken, progress: Option<&Progress>) -> io::Result<Option<u64>> {
    let codec = buf.codec;
    let len = buf.len();
    let read_err = || io::Error::other("Could not read the document");
    if let Some(p) = progress {
        p.set_total(len);
    }
    let from = from.min(len);
    let mut err = None;
    buf.for_each_chunk(0..from, |_, b| match out.write_all(b) {
        Ok(()) => true,
        Err(e) => {
            err = Some(e);
            false
        }
    })
    .ok_or_else(read_err)?;
    if let Some(e) = err {
        return Err(e);
    }
    let mut r = LineReader::new(buf);
    let mut n = 0u64;
    loop {
        let before = r.taken();
        let Some((off, line)) = r.next()?.map(|(o, l)| (o, l.to_vec())) else { break };
        let total = r.taken() - before;
        if off < from {
            continue;
        }
        let lo = cell_offset(&codec, &line, off, cells.0);
        let hi = cell_offset(&codec, &line, off, cells.1).max(lo);
        out.write_all(&line[..lo])?;
        out.write_all(&codec.encode(&gen.value(n)))?;
        out.write_all(&line[hi..])?;
        // The line break as it is.
        let brk = buf.read(off + line.len() as u64..off + total).ok_or_else(read_err)?;
        out.write_all(&brk)?;
        n += 1;
        if n.is_multiple_of(4096) {
            if cancel.is_cancelled() {
                return Ok(None);
            }
            if let Some(p) = progress {
                p.set_done(off);
            }
        }
    }
    Ok(Some(n))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::Codec;
    use crate::file::{OpenMode, Source};
    use crate::index::LineIndex;
    use std::sync::Arc;

    #[test]
    fn values() {
        let n = |start, step, width, hex| ColumnGen::Numbers { start, step, width, hex };
        assert_eq!((0..3).map(|i| n(1, 1, 3, false).value(i)).collect::<Vec<_>>(), ["001", "002", "003"]);
        assert_eq!(n(-2, 1, 2, false).value(0), "-02");
        assert_eq!(n(250, 5, 0, true).value(2), "104");
        assert_eq!(ColumnGen::Text("| ".into()).value(9), "| ");
    }

    #[test]
    fn down_to_the_end_with_tabs_short_lines_and_crlf() {
        let text = "keep\r\nab\tcd\r\nx\r\nlong line\r\nlast";
        let mut f = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut f, text.as_bytes()).unwrap();
        let s = Arc::new(Source::open(f.path(), OpenMode::Auto).unwrap());
        let buf = TextBuf::new(s.clone(), LineIndex::new(s.len()), Codec::utf8());
        let mut out = Vec::new();
        let gen = ColumnGen::Numbers { start: 1, step: 1, width: 1, hex: false };
        // From the second line, cells 2..4 (the tab ends at cell 4).
        let n = column_edit_to(&buf, 6, (2, 4), &gen, &mut out, &CancelToken::new(), None).unwrap().unwrap();
        assert_eq!(n, 4);
        assert_eq!(String::from_utf8(out).unwrap(), "keep\r\nab1cd\r\nx2\r\nlo3 line\r\nla4");
    }
}
