//! Visual segmentation of the document into rows.
//!
//! A *segment* is a line, except that lines longer than [`SEG`] are soft-broken at multiples of
//! `SEG` (counted from the document start, snapped to a character boundary, and only once at
//! least `SEG` bytes of the line precede the break). This makes every row computation bounded:
//! finding the segment around any offset reads at most `2 * SEG` bytes, so a single 2 GB line
//! scrolls like any other text. With word wrap on, segments are further split into rows of
//! `wrap` cells.

use crate::encoding::{Codec, NlByte};

use super::buffer::TextBuf;

pub const SEG: u64 = 64 * 1024;
pub const TAB_WIDTH: usize = 4;

/// Returned when (non-blocking) data is not available yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pending;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Seg {
    pub start: u64,
    /// End of visible content (excludes the newline and the CR of CRLF).
    pub content_end: u64,
    /// Start of the following segment.
    pub next: u64,
    /// True if `start` begins a real line (not a soft break).
    pub line_start: bool,
    /// True if the segment ends with a newline.
    pub has_nl: bool,
}

impl Seg {
    pub fn is_last(&self, len: u64) -> bool {
        !self.has_nl && self.next >= len
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Row {
    pub start: u64,
    pub end: u64,
    pub seg: Seg,
}

impl Row {
    pub fn is_first_in_seg(&self) -> bool {
        self.start == self.seg.start
    }
    pub fn is_last_in_seg(&self) -> bool {
        self.end == self.seg.content_end
    }
}

fn read(buf: &TextBuf, r: std::ops::Range<u64>) -> Result<std::borrow::Cow<'_, [u8]>, Pending> {
    buf.read(r).ok_or(Pending)
}

pub fn is_line_start(buf: &TextBuf, q: u64) -> Result<bool, Pending> {
    let w = buf.codec.unit() as u64;
    if q == 0 {
        return Ok(true);
    }
    if q < w {
        return Ok(false);
    }
    let b = read(buf, q - w..q)?;
    Ok(buf.codec.is_nl_at(&b))
}

/// True if a soft break may sit at `hay[i..]` (not on a newline or the CR of a CRLF).
fn soft_ok(codec: &Codec, hay: &[u8], i: usize) -> bool {
    let rest = &hay[i.min(hay.len())..];
    if codec.is_nl_at(rest) {
        return false;
    }
    if codec.nl == NlByte::Lf {
        let cr = codec.cr_unit();
        if rest.starts_with(cr) && codec.is_nl_at(&rest[cr.len()..]) {
            return false;
        }
    }
    true
}

/// Start of the segment containing offset `p`.
pub fn seg_start(buf: &TextBuf, p: u64) -> Result<u64, Pending> {
    let len = buf.len();
    let codec = buf.codec;
    let unit = codec.unit() as u64;
    let p = p.min(len);
    let p = p - p % unit;
    if p == 0 {
        return Ok(0);
    }
    let lo = p.saturating_sub(2 * SEG);
    let lo = lo - lo % unit;
    let hi = (p + 8).min(len);
    let hay = read(buf, lo..hi)?;
    let before = &hay[..(p - lo) as usize];
    let ls = match codec.rfind_nl(before, lo) {
        Some(i) => Some(lo + i as u64 + unit),
        None if lo == 0 => Some(0),
        None => None,
    };
    let mut m = (p / SEG) * SEG;
    while m > 0 && m >= lo {
        if let Some(l) = ls {
            if m < l + SEG {
                break;
            }
        }
        let i = (m - lo) as usize;
        let snapped = m + codec.snap_forward(&hay[i..]) as u64;
        if snapped <= p && snapped < len && soft_ok(&codec, &hay, (snapped - lo) as usize) {
            return Ok(snapped);
        }
        if snapped <= p {
            break;
        }
        m -= SEG;
    }
    Ok(ls.unwrap_or(lo))
}

/// Segment starting at `q` (which must be a segment start).
pub fn seg_at(buf: &TextBuf, q: u64) -> Result<Seg, Pending> {
    let len = buf.len();
    let codec = buf.codec;
    let unit = codec.unit() as u64;
    let line_start = is_line_start(buf, q)?;
    if q >= len {
        return Ok(Seg { start: len, content_end: len, next: len, line_start, has_nl: false });
    }
    let soft_mult = if line_start { (q + SEG).div_ceil(SEG) * SEG } else { (q / SEG + 1) * SEG };
    let scan_end = (soft_mult + 8).min(len);
    let hay = read(buf, q..scan_end)?;
    let soft = if soft_mult < len {
        let i = (soft_mult - q) as usize;
        let s = soft_mult + codec.snap_forward(&hay[i..]) as u64;
        if s < len && soft_ok(&codec, &hay, (s - q) as usize) {
            Some(s)
        } else {
            None
        }
    } else {
        None
    };
    let limit = match soft {
        Some(s) => ((s - q) as usize).min(hay.len()),
        None => hay.len(),
    };
    // A newline may begin right at the (rejected) soft position, so search a little further.
    let search = &hay[..(limit + 3 * unit as usize).min(hay.len())];
    match codec.find_nl(search, q) {
        Some(i) if soft.is_none() || (i as u64) <= soft.unwrap() - q => {
            let nl = q + i as u64;
            let mut ce = nl;
            if codec.nl == NlByte::Lf {
                let cr = codec.cr_unit();
                if i >= cr.len() && &hay[i - cr.len()..i] == cr {
                    ce -= cr.len() as u64;
                }
            }
            Ok(Seg { start: q, content_end: ce, next: nl + unit, line_start, has_nl: true })
        }
        _ => match soft {
            Some(s) => Ok(Seg { start: q, content_end: s, next: s, line_start, has_nl: false }),
            None => Ok(Seg { start: q, content_end: len, next: len, line_start, has_nl: false }),
        },
    }
}

/// Segment containing `p`.
pub fn seg_containing(buf: &TextBuf, p: u64) -> Result<Seg, Pending> {
    seg_at(buf, seg_start(buf, p)?)
}

pub fn next_seg(buf: &TextBuf, s: &Seg) -> Result<Option<Seg>, Pending> {
    if s.is_last(buf.len()) {
        return Ok(None);
    }
    seg_at(buf, s.next).map(Some)
}

pub fn prev_seg(buf: &TextBuf, s: &Seg) -> Result<Option<Seg>, Pending> {
    if s.start == 0 {
        return Ok(None);
    }
    seg_containing(buf, s.start - 1).map(Some)
}

// ---------------------------------------------------------------------------------------------
// Cells (monospace columns).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellKind {
    Normal,
    Tab,
    /// C0/C1 control character or DEL (shown as a placeholder glyph).
    Control,
    /// U+FFFD from an invalid sequence.
    Invalid,
    /// Zero-width (BOM at the start of the file).
    Hidden,
}

#[derive(Clone, Copy, Debug)]
pub struct Cell {
    pub off: u64,
    pub len: u32,
    pub cell: u32,
    pub width: u32,
    pub ch: char,
    pub kind: CellKind,
}

/// Walks the characters of `bytes` (document offset `base`), assigning monospace cells from 0.
/// Stops early when `f` returns false.
pub fn for_each_cell(codec: &Codec, bytes: &[u8], base: u64, mut f: impl FnMut(Cell) -> bool) {
    let mut i = 0usize;
    let mut cell = 0u32;
    while i < bytes.len() {
        let (c, l) = codec.decode_char(&bytes[i..]);
        let off = base + i as u64;
        let (kind, width) = match c {
            '\t' => (CellKind::Tab, TAB_WIDTH as u32 - cell % TAB_WIDTH as u32),
            '\u{FEFF}' if off == 0 => (CellKind::Hidden, 0),
            '\u{FFFD}' => (CellKind::Invalid, 1),
            c if (c as u32) < 0x20 || (0x7F..=0x9F).contains(&(c as u32)) => (CellKind::Control, 1),
            _ => (CellKind::Normal, 1),
        };
        if !f(Cell { off, len: l as u32, cell, width, ch: c, kind }) {
            return;
        }
        cell += width;
        i += l;
    }
}

/// Total cell width of `bytes`.
pub fn cell_width(codec: &Codec, bytes: &[u8], base: u64) -> u32 {
    // Fast path: ASCII without tabs is one cell per byte (controls are one cell too).
    if codec.unit() == 1 && bytes.is_ascii() && memchr::memchr(b'\t', bytes).is_none() {
        return bytes.len() as u32;
    }
    let mut w = 0;
    for_each_cell(codec, bytes, base, |c| {
        w = c.cell + c.width;
        true
    });
    w
}

/// Start offsets of wrapped rows within one segment's content.
pub fn wrap_starts(codec: &Codec, bytes: &[u8], base: u64, wrap: usize) -> Vec<u64> {
    let wrap = wrap.max(1) as u32;
    let mut starts = vec![base];
    let mut row_cell0 = 0u32;
    for_each_cell(codec, bytes, base, |c| {
        if c.cell + c.width - row_cell0 > wrap && c.cell > row_cell0 {
            starts.push(c.off);
            row_cell0 = c.cell;
        }
        true
    });
    starts
}

pub fn rows_of_seg(buf: &TextBuf, seg: &Seg, wrap: Option<usize>) -> Result<Vec<Row>, Pending> {
    let Some(w) = wrap else {
        return Ok(vec![Row { start: seg.start, end: seg.content_end, seg: *seg }]);
    };
    let bytes = read(buf, seg.start..seg.content_end)?;
    let starts = wrap_starts(&buf.codec, &bytes, seg.start, w);
    let mut rows = Vec::with_capacity(starts.len());
    for (i, &s) in starts.iter().enumerate() {
        let e = starts.get(i + 1).copied().unwrap_or(seg.content_end);
        rows.push(Row { start: s, end: e, seg: *seg });
    }
    Ok(rows)
}

/// Row containing offset `off`.
pub fn row_at(buf: &TextBuf, off: u64, wrap: Option<usize>) -> Result<Row, Pending> {
    let seg = seg_containing(buf, off)?;
    let rows = rows_of_seg(buf, &seg, wrap)?;
    let i = rows.partition_point(|r| r.start <= off).saturating_sub(1);
    Ok(rows[i])
}

pub fn next_row(buf: &TextBuf, row: &Row, wrap: Option<usize>) -> Result<Option<Row>, Pending> {
    if wrap.is_some() && !row.is_last_in_seg() {
        let rows = rows_of_seg(buf, &row.seg, wrap)?;
        if let Some(r) = rows.iter().find(|r| r.start > row.start) {
            return Ok(Some(*r));
        }
    }
    match next_seg(buf, &row.seg)? {
        None => Ok(None),
        Some(s) => {
            if wrap.is_none() {
                Ok(Some(Row { start: s.start, end: s.content_end, seg: s }))
            } else {
                Ok(rows_of_seg(buf, &s, wrap)?.first().copied())
            }
        }
    }
}

pub fn prev_row(buf: &TextBuf, row: &Row, wrap: Option<usize>) -> Result<Option<Row>, Pending> {
    if wrap.is_some() && !row.is_first_in_seg() {
        let rows = rows_of_seg(buf, &row.seg, wrap)?;
        if let Some(r) = rows.iter().rev().find(|r| r.start < row.start) {
            return Ok(Some(*r));
        }
    }
    match prev_seg(buf, &row.seg)? {
        None => Ok(None),
        Some(s) => {
            if wrap.is_none() {
                Ok(Some(Row { start: s.start, end: s.content_end, seg: s }))
            } else {
                Ok(rows_of_seg(buf, &s, wrap)?.last().copied())
            }
        }
    }
}

/// Moves `n` rows down (positive) or up (negative) from `row`; stops at the document edges.
pub fn step_rows(buf: &TextBuf, row: Row, n: i64, wrap: Option<usize>) -> Result<Row, Pending> {
    let mut r = row;
    if n >= 0 {
        for _ in 0..n {
            match next_row(buf, &r, wrap)? {
                Some(x) => r = x,
                None => break,
            }
        }
    } else {
        for _ in 0..(-n) {
            match prev_row(buf, &r, wrap)? {
                Some(x) => r = x,
                None => break,
            }
        }
    }
    Ok(r)
}

/// Offset within `row` at cell column `col` (clamped to the row's content).
pub fn offset_at_cell(buf: &TextBuf, row: &Row, col: u32) -> Result<u64, Pending> {
    let bytes = read(buf, row.start..row.end)?;
    let mut result = row.end;
    for_each_cell(&buf.codec, &bytes, row.start, |c| {
        if c.width == 0 {
            return true;
        }
        if col <= c.cell {
            result = c.off;
            return false;
        }
        if col < c.cell + c.width {
            result = if col - c.cell <= c.width / 2 { c.off } else { c.off + c.len as u64 };
            return false;
        }
        true
    });
    Ok(result)
}

/// Cell column of offset `off` within `row`.
pub fn cell_of_offset(buf: &TextBuf, row: &Row, off: u64) -> Result<u32, Pending> {
    let end = off.clamp(row.start, row.end);
    let bytes = read(buf, row.start..end)?;
    Ok(cell_width(&buf.codec, &bytes, row.start))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::{Encoding, NlByte};
    use crate::file::{OpenMode, Source};
    use crate::index;
    use std::io::Write;
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

    fn all_segs(t: &TextBuf) -> Vec<Seg> {
        let mut v = vec![seg_at(t, 0).unwrap()];
        while let Some(s) = next_seg(t, v.last().unwrap()).unwrap() {
            assert!(s.start > v.last().unwrap().start || t.len() == 0);
            v.push(s);
        }
        v
    }

    fn check_invariants(t: &TextBuf) {
        let segs = all_segs(t);
        let len = t.len();
        for w in segs.windows(2) {
            assert_eq!(w[0].next, w[1].start);
        }
        for s in &segs {
            assert!(s.content_end - s.start <= 2 * SEG + 4, "segment too long: {s:?}");
            for p in [s.start, (s.start + s.next) / 2, s.next.saturating_sub(1).max(s.start)] {
                if p < s.next || (p == s.start && s.start == len) {
                    let p = t.snap_char(p).min(s.next.saturating_sub(1).max(s.start));
                    assert_eq!(seg_start(t, p).unwrap(), s.start, "p={p} seg={s:?}");
                }
            }
        }
        // Backwards iteration visits the same segments.
        let mut back = vec![*segs.last().unwrap()];
        while let Some(s) = prev_seg(t, back.last().unwrap()).unwrap() {
            back.push(s);
        }
        back.reverse();
        assert_eq!(back, segs);
    }

    #[test]
    fn short_lines() {
        let (_f, t) = buf_with(b"ab\ncd\r\n\nlast", Codec::utf8());
        let segs = all_segs(&t);
        assert_eq!(segs.len(), 4);
        assert_eq!((segs[1].start, segs[1].content_end, segs[1].next), (3, 5, 7));
        assert!(segs[3].is_last(t.len()));
        check_invariants(&t);
    }

    #[test]
    fn trailing_newline_gives_empty_last_row() {
        let (_f, t) = buf_with(b"a\nb\n", Codec::utf8());
        let segs = all_segs(&t);
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[2].start, 4);
        let (_g, e) = buf_with(b"", Codec::utf8());
        assert_eq!(all_segs(&e).len(), 1);
    }

    #[test]
    fn long_line_is_soft_broken() {
        let mut data = b"short\n".to_vec();
        data.extend(std::iter::repeat_n(b'x', 300_000));
        data.extend_from_slice(b"\nend\n");
        let (_f, t) = buf_with(&data, Codec::utf8());
        let segs = all_segs(&t);
        assert!(segs.len() > 5);
        assert!(segs.iter().filter(|s| !s.line_start).count() >= 3);
        check_invariants(&t);
    }

    #[test]
    fn long_multibyte_line_snaps_to_char_boundaries() {
        let mut data = Vec::new();
        data.extend("é".repeat(200_001).as_bytes());
        data.extend("😀".repeat(50_001).as_bytes());
        let (_f, t) = buf_with(&data, Codec::utf8());
        for s in all_segs(&t) {
            let b = t.read(s.start..(s.start + 1).min(t.len())).unwrap();
            if !b.is_empty() {
                assert_ne!(b[0] & 0xC0, 0x80, "segment starts inside a char: {s:?}");
            }
        }
        check_invariants(&t);
    }

    #[test]
    fn utf16_segments() {
        let text = format!("abc\n{}\nz", "щ".repeat(100_000));
        let mut data = vec![0xFF, 0xFE];
        data.extend(text.encode_utf16().flat_map(|u| u.to_le_bytes()));
        let (_f, t) = buf_with(&data, Codec::new(Encoding::Utf16Le, NlByte::Lf, 2));
        let segs = all_segs(&t);
        assert!(segs.iter().all(|s| s.start % 2 == 0));
        assert_eq!(segs[0].content_end, 2 + 6);
        check_invariants(&t);
    }

    #[test]
    fn wrap_rows_and_cells() {
        let (_f, t) = buf_with(b"0123456789abcdef\n\tx", Codec::utf8());
        let seg = seg_at(&t, 0).unwrap();
        let rows = rows_of_seg(&t, &seg, Some(5)).unwrap();
        assert_eq!(rows.iter().map(|r| r.start).collect::<Vec<_>>(), vec![0, 5, 10, 15]);
        let r = row_at(&t, 12, Some(5)).unwrap();
        assert_eq!(r.start, 10);
        let n = next_row(&t, &rows[3], Some(5)).unwrap().unwrap();
        assert_eq!(n.start, 17);
        assert_eq!(prev_row(&t, &n, Some(5)).unwrap().unwrap().start, 15);
        assert_eq!(cell_of_offset(&t, &n, 19).unwrap(), 5);
        assert_eq!(offset_at_cell(&t, &n, 4).unwrap(), 18);
        assert_eq!(offset_at_cell(&t, &n, 99).unwrap(), 19);
    }

    #[test]
    fn step_rows_clamps() {
        let (_f, t) = buf_with(b"a\nb\nc\nd", Codec::utf8());
        let r0 = row_at(&t, 0, None).unwrap();
        assert_eq!(step_rows(&t, r0, 2, None).unwrap().start, 4);
        assert_eq!(step_rows(&t, r0, 100, None).unwrap().start, 6);
        let last = row_at(&t, 7, None).unwrap();
        assert_eq!(step_rows(&t, last, -100, None).unwrap().start, 0);
    }
}
