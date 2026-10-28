//! Line filter ("grep"): the lines of a document that contain a match — or, inverted, the lines
//! that do not. It streams over the document like search, so memory stays bounded and any file
//! size works. A filter can be limited to one CSV column: a line then counts only when a match
//! lies inside that field.

use std::io::{self, Write};
use std::ops::Range;

use crate::csv::{self, Dialect};
use crate::jobs::{CancelToken, Progress};
use crate::search::{self, Matcher, SearchEnd};
use crate::text::TextBuf;

/// Lines are scanned in steps of this size when their ends lie outside the search window.
const SCAN: u64 = 1 << 20;
/// A CSV line longer than this is not split into fields (it never matches a column filter).
const FIELD_LINE_MAX: u64 = 4 << 20;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FilterSpec {
    /// Keep the lines without a match.
    pub invert: bool,
    /// Only matches inside this field (0-based) of a CSV line count.
    pub column: Option<(Dialect, usize)>,
}

/// Start of the line containing `off` (blocking snapshot reads).
pub fn line_start_at(buf: &TextBuf, off: u64) -> Option<u64> {
    let unit = buf.codec.unit() as u64;
    let mut hi = off.min(buf.len());
    while hi > 0 {
        let lo = hi.saturating_sub(SCAN);
        let lo = lo - lo % unit;
        let b = buf.read(lo..hi)?;
        if let Some(i) = buf.codec.rfind_nl(&b, lo) {
            return Some(lo + i as u64 + unit);
        }
        hi = lo;
    }
    Some(0)
}

/// End of the line containing `off`, including its line break (= start of the next line).
pub fn line_end_at(buf: &TextBuf, off: u64) -> Option<u64> {
    let unit = buf.codec.unit() as u64;
    let len = buf.len();
    let mut lo = off.min(len);
    while lo < len {
        let hi = (lo + SCAN).min(len);
        let b = buf.read(lo..hi)?;
        if let Some(i) = buf.codec.find_nl(&b, lo) {
            return Some(lo + i as u64 + unit);
        }
        lo = hi;
    }
    Some(len)
}

/// Calls `f(start, end)` for every line in `range` (which starts at a line start); `end`
/// includes the line break. Returns `None` on a read error and `Some(false)` if `f` stopped.
fn each_line(buf: &TextBuf, range: Range<u64>, cancel: &CancelToken, f: &mut dyn FnMut(u64, u64) -> bool) -> Option<bool> {
    let unit = buf.codec.unit() as u64;
    let mut pos = range.start;
    let mut line = range.start;
    while pos < range.end {
        if cancel.is_cancelled() {
            return Some(false);
        }
        let hi = (pos + SCAN).min(range.end);
        let b = buf.read(pos..hi)?;
        let mut from = 0usize;
        while let Some(i) = buf.codec.find_nl(&b[from..], pos + from as u64) {
            let end = pos + (from + i) as u64 + unit;
            if !f(line, end) {
                return Some(false);
            }
            line = end;
            from += i + unit as usize;
        }
        pos = hi;
    }
    if line < range.end && !f(line, range.end) {
        return Some(false);
    }
    Some(true)
}

/// Byte range of field `col` in the line `ls..le`, if the line has that field.
fn field_range(buf: &TextBuf, d: Dialect, col: usize, ls: u64, le: u64, fields: &mut Vec<Range<usize>>) -> Option<Range<u64>> {
    if le - ls > FIELD_LINE_MAX {
        return None;
    }
    let b = buf.read(ls..le)?;
    csv::split_fields(&buf.codec, d, &b, fields);
    fields.get(col).map(|r| ls + r.start as u64..ls + r.end as u64)
}

/// Streams over the document and calls `on_line(start, end)` for each kept line, in order
/// (`end` includes the line break). Stops when `on_line` returns false.
pub fn filter_lines(
    buf: &TextBuf,
    m: &Matcher,
    spec: FilterSpec,
    cancel: &CancelToken,
    progress: Option<&Progress>,
    mut on_line: impl FnMut(u64, u64) -> bool,
) -> SearchEnd {
    let len = buf.len();
    let codec = buf.codec;
    // The line the last match fell in, and whether it was kept.
    let mut cur: Option<(u64, u64, bool, Option<Range<u64>>)> = None;
    // Inverted: start of the first line not yet classified.
    let mut cursor = 0u64;
    let mut stopped = false;
    let mut failed = false;
    let mut fields = Vec::new();
    let end = search::search_forward_ctx(buf, m, 0..len, cancel, progress, |s, e, hay, base| {
        let inside = matches!(cur, Some((_, le, _, _)) if s < le);
        if !inside {
            let ls = line_start_in(&codec, hay, base, s).or_else(|| line_start_at(buf, s));
            let le = line_end_in(&codec, hay, base, s).or_else(|| line_end_at(buf, s));
            let (Some(ls), Some(le)) = (ls, le) else {
                failed = true;
                return false;
            };
            // An empty match after the final line break is not a line.
            if ls == len && len > 0 {
                return true;
            }
            let field = match spec.column {
                Some((d, col)) => field_range(buf, d, col, ls, le, &mut fields),
                None => None,
            };
            cur = Some((ls, le, false, field));
        }
        let Some((ls, le, kept, field)) = cur.as_mut() else { return true };
        if *kept {
            return true;
        }
        let hit = match (&spec.column, &field) {
            (None, _) => true,
            (Some(_), Some(f)) => s < f.end && (e > f.start || s == f.start) || s == e && s == f.end,
            (Some(_), None) => false,
        };
        if !hit {
            return true;
        }
        *kept = true;
        let (ls, le) = (*ls, *le);
        if spec.invert {
            if ls > cursor {
                match each_line(buf, cursor..ls, cancel, &mut on_line) {
                    Some(true) => {}
                    Some(false) => {
                        stopped = true;
                        return false;
                    }
                    None => {
                        failed = true;
                        return false;
                    }
                }
            }
            cursor = le;
            true
        } else if on_line(ls, le) {
            true
        } else {
            stopped = true;
            false
        }
    });
    if failed {
        return SearchEnd::Unavailable;
    }
    if stopped {
        return SearchEnd::Stopped;
    }
    if end != SearchEnd::Completed {
        return end;
    }
    if spec.invert && cursor < len {
        match each_line(buf, cursor..len, cancel, &mut on_line) {
            Some(true) => {}
            Some(false) if cancel.is_cancelled() => return SearchEnd::Cancelled,
            Some(false) => return SearchEnd::Stopped,
            None => return SearchEnd::Unavailable,
        }
    }
    SearchEnd::Completed
}

fn line_start_in(codec: &crate::encoding::Codec, hay: &[u8], base: u64, s: u64) -> Option<u64> {
    if hay.is_empty() || s < base || s - base > hay.len() as u64 {
        return None;
    }
    let rel = (s - base) as usize;
    match codec.rfind_nl(&hay[..rel], base) {
        Some(i) => Some(base + (i + codec.unit()) as u64),
        None if base == 0 => Some(0),
        None => None,
    }
}

fn line_end_in(codec: &crate::encoding::Codec, hay: &[u8], base: u64, s: u64) -> Option<u64> {
    if hay.is_empty() || s < base || s - base > hay.len() as u64 {
        return None;
    }
    let rel = (s - base) as usize;
    codec.find_nl(&hay[rel..], s).map(|i| s + (i + codec.unit()) as u64)
}

/// Counts of an export: lines and bytes written.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Written {
    pub lines: u64,
    pub bytes: u64,
}

/// Writes the kept lines to `out` (with the document's byte order mark, if it has one).
/// Returns `None` when cancelled.
pub fn write_lines_to<W: Write>(buf: &TextBuf, m: &Matcher, spec: FilterSpec, out: &mut W, cancel: &CancelToken, progress: Option<&Progress>) -> io::Result<Option<Written>> {
    let bom = buf.codec.bom_len as u64;
    let mut w = Written::default();
    if bom > 0 {
        let b = buf.read(0..bom).ok_or_else(|| io::Error::other("Could not read the document"))?;
        out.write_all(&b)?;
        w.bytes += bom;
    }
    let mut err: Option<io::Error> = None;
    let end = filter_lines(buf, m, spec, cancel, progress, |ls, le| {
        let r = buf.for_each_chunk(ls.max(bom)..le, |_, b| match out.write_all(b) {
            Ok(()) => true,
            Err(e) => {
                err = Some(e);
                false
            }
        });
        if r.is_none() && err.is_none() {
            err = Some(io::Error::other("Could not read the document"));
        }
        w.lines += 1;
        w.bytes += le - ls.max(bom);
        err.is_none()
    });
    if let Some(e) = err {
        return Err(e);
    }
    match end {
        SearchEnd::Completed => Ok(Some(w)),
        SearchEnd::Cancelled => Ok(None),
        _ => Err(io::Error::other("Could not read the document")),
    }
}

/// Copies the byte range `range` of the document to `out`, cancellably.
pub fn write_range_to<W: Write>(buf: &TextBuf, range: Range<u64>, out: &mut W, cancel: &CancelToken, progress: Option<&Progress>) -> io::Result<Option<u64>> {
    let mut pos = range.start;
    let mut err = None;
    while pos < range.end {
        if cancel.is_cancelled() {
            return Ok(None);
        }
        let end = (pos + 16 * SCAN).min(range.end);
        let r = buf.for_each_chunk(pos..end, |_, b| match out.write_all(b) {
            Ok(()) => true,
            Err(e) => {
                err = Some(e);
                false
            }
        });
        if let Some(e) = err.take() {
            return Err(e);
        }
        if r.is_none() {
            return Err(io::Error::other("Could not read the document"));
        }
        if let Some(p) = progress {
            p.add(end - pos);
        }
        pos = end;
    }
    Ok(Some(range.end - range.start))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::{Codec, Encoding, NlByte};
    use crate::file::{OpenMode, Source};
    use crate::index;
    use crate::search::Query;
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

    fn lines(buf: &TextBuf, pat: &str, regex: bool, spec: FilterSpec) -> Vec<String> {
        let q = Query { pattern: pat.into(), regex, case_sensitive: true, ..Default::default() };
        let m = Matcher::new(&q, buf.codec).unwrap();
        let mut v = Vec::new();
        let end = filter_lines(buf, &m, spec, &CancelToken::new(), None, |s, e| {
            v.push(buf.codec.decode_lossy(&buf.read(s..e).unwrap()));
            true
        });
        assert_eq!(end, SearchEnd::Completed);
        v
    }

    const LOG: &[u8] = b"INFO start\nERROR one ERROR two\nINFO mid\nWARN x\nERROR three";

    #[test]
    fn keeps_each_matching_line_once() {
        let (_f, t) = buf_with(LOG, Codec::utf8());
        assert_eq!(lines(&t, "ERROR", false, FilterSpec::default()), vec!["ERROR one ERROR two\n", "ERROR three"]);
        assert_eq!(lines(&t, "^(INFO|WARN)", true, FilterSpec::default()), vec!["INFO start\n", "INFO mid\n", "WARN x\n"]);
        assert!(lines(&t, "nothing", false, FilterSpec::default()).is_empty());
    }

    #[test]
    fn inverted() {
        let (_f, t) = buf_with(LOG, Codec::utf8());
        let inv = FilterSpec { invert: true, ..Default::default() };
        assert_eq!(lines(&t, "ERROR", false, inv), vec!["INFO start\n", "INFO mid\n", "WARN x\n"]);
        assert_eq!(lines(&t, "nothing", false, inv).len(), 5);
        assert!(lines(&t, "^", true, inv).is_empty());
        // A trailing line break does not make an extra empty line.
        let (_g, t2) = buf_with(b"a\nb\n", Codec::utf8());
        assert_eq!(lines(&t2, "a", false, inv), vec!["b\n"]);
        assert_eq!(lines(&t2, "^", true, FilterSpec::default()), vec!["a\n", "b\n"]);
    }

    #[test]
    fn csv_column() {
        let (_f, t) = buf_with(b"id,name,city\n1,Ann,Paris\n2,Paris,Rome\n3,\"Bob, Paris\",Oslo\n", Codec::utf8());
        let col = |c, invert| FilterSpec { invert, column: Some((Dialect::new(b','), c)) };
        assert_eq!(lines(&t, "Paris", false, col(2, false)), vec!["1,Ann,Paris\n"]);
        assert_eq!(lines(&t, "Paris", false, col(1, false)), vec!["2,Paris,Rome\n", "3,\"Bob, Paris\",Oslo\n"]);
        assert_eq!(lines(&t, "Paris", false, col(1, true)), vec!["id,name,city\n", "1,Ann,Paris\n"]);
    }

    #[test]
    fn utf16_and_export() {
        let codec = Codec::new(Encoding::Utf16Le, NlByte::Lf, 2);
        let mut data = vec![0xFF, 0xFE];
        data.extend(codec.encode("keep 1\ndrop\nkeep 2\n"));
        let (_f, t) = buf_with(&data, codec);
        assert_eq!(lines(&t, "keep", false, FilterSpec::default()), vec!["\u{feff}keep 1\n", "keep 2\n"]);
        let q = Query { pattern: "keep".into(), case_sensitive: true, ..Default::default() };
        let m = Matcher::new(&q, codec).unwrap();
        let mut out = Vec::new();
        let w = write_lines_to(&t, &m, FilterSpec::default(), &mut out, &CancelToken::new(), None).unwrap().unwrap();
        assert_eq!(w.lines, 2);
        let mut want = vec![0xFF, 0xFE];
        want.extend(codec.encode("keep 1\nkeep 2\n"));
        assert_eq!(out, want);
    }

    #[test]
    fn lines_longer_than_the_search_window() {
        let mut data = b"short\n".to_vec();
        data.extend(vec![b'x'; search::WINDOW as usize + 100]);
        data.extend_from_slice(b"NEEDLE tail\nlast\n");
        let (_f, t) = buf_with(&data, Codec::utf8());
        let q = Query { pattern: "NEEDLE".into(), case_sensitive: true, ..Default::default() };
        let m = Matcher::new(&q, Codec::utf8()).unwrap();
        let mut got = Vec::new();
        filter_lines(&t, &m, FilterSpec::default(), &CancelToken::new(), None, |s, e| {
            got.push((s, e));
            true
        });
        assert_eq!(got, vec![(6, data.len() as u64 - 5)]);
    }

    #[test]
    fn line_bounds() {
        let (_f, t) = buf_with(b"ab\ncd\nef", Codec::utf8());
        assert_eq!((line_start_at(&t, 4), line_end_at(&t, 4)), (Some(3), Some(6)));
        assert_eq!((line_start_at(&t, 2), line_end_at(&t, 2)), (Some(0), Some(3)));
        assert_eq!((line_start_at(&t, 7), line_end_at(&t, 7)), (Some(6), Some(8)));
    }
}
