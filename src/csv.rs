//! CSV and TSV: delimiter detection, quote-aware field splitting, and deleting columns from a
//! whole document in one streaming pass with bounded memory, so any file size works.
//!
//! Fields follow RFC 4180: a field that starts with the quote character runs to the closing
//! quote, `""` inside it is a literal quote, and delimiters and line breaks inside the quotes
//! belong to the field. A quote anywhere else is an ordinary character, as in spreadsheets.

use std::collections::HashMap;
use std::io::{self, Write};
use std::ops::Range;

use crate::encoding::{Codec, Encoding, NlByte};
use crate::jobs::{CancelToken, Progress};
use crate::replace::ReplaceStats;
use crate::text::TextBuf;

/// The delimiters [`detect`] chooses from.
pub const DELIMITERS: [u8; 4] = [b',', b';', b'\t', b'|'];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dialect {
    pub delim: u8,
    pub quote: u8,
}

impl Dialect {
    pub fn new(delim: u8) -> Dialect {
        Dialect { delim, quote: b'"' }
    }

    pub fn name(self) -> &'static str {
        match self.delim {
            b',' => "Comma",
            b';' => "Semicolon",
            b'\t' => "Tab",
            b'|' => "Pipe",
            _ => "Other",
        }
    }
}

/// The code unit at byte `i`: a byte, or a 16-bit unit in UTF-16. Delimiters, quotes and line
/// breaks are ASCII, which never occurs inside a multi-byte UTF-8 character.
#[inline]
fn unit_at(enc: Encoding, b: &[u8], i: usize) -> u16 {
    match enc {
        Encoding::Utf16Le => u16::from_le_bytes([b[i], b[i + 1]]),
        Encoding::Utf16Be => u16::from_be_bytes([b[i], b[i + 1]]),
        _ => b[i] as u16,
    }
}

fn nl_value(codec: &Codec) -> u16 {
    match codec.nl {
        NlByte::Lf => b'\n' as u16,
        NlByte::Cr => b'\r' as u16,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    FieldStart,
    Unquoted,
    Quoted,
    /// A quote inside a quoted field: the closing quote, or the first half of `""`.
    QuoteInQuoted,
}

#[derive(PartialEq, Eq)]
enum Step {
    Inside,
    FieldEnd,
    RecordEnd,
}

impl State {
    /// Moves past the code unit `c`; `nl` is the line-break unit.
    #[inline]
    fn step(&mut self, c: u16, d: Dialect, nl: u16) -> Step {
        let quote = d.quote as u16;
        match *self {
            State::Quoted => {
                if c == quote {
                    *self = State::QuoteInQuoted;
                }
                return Step::Inside;
            }
            State::FieldStart | State::QuoteInQuoted if c == quote => {
                *self = State::Quoted;
                return Step::Inside;
            }
            _ => {}
        }
        if c == d.delim as u16 {
            *self = State::FieldStart;
            Step::FieldEnd
        } else if c == nl {
            *self = State::FieldStart;
            Step::RecordEnd
        } else {
            *self = State::Unquoted;
            Step::Inside
        }
    }
}

/// Splits one record (without its line break) into the byte ranges of its fields, quotes
/// included. There is always at least one field.
pub fn split_fields(codec: &Codec, d: Dialect, b: &[u8], out: &mut Vec<Range<usize>>) {
    out.clear();
    let u = codec.unit();
    let mut st = State::FieldStart;
    let mut start = 0;
    let mut i = 0;
    while i + u <= b.len() {
        if st.step(unit_at(codec.enc, b, i), d, u16::MAX) == Step::FieldEnd {
            out.push(start..i);
            start = i + u;
        }
        i += u;
    }
    out.push(start..b.len());
}

/// The value of a field: enclosing quotes removed and `""` turned into `"`.
pub fn field_value(codec: &Codec, d: Dialect, raw: &[u8]) -> String {
    let s = codec.decode_lossy(raw);
    let q = d.quote as char;
    let Some(inner) = s.strip_prefix(q) else { return s };
    let mut out = String::with_capacity(inner.len());
    let mut it = inner.chars().peekable();
    while let Some(c) = it.next() {
        if c != q {
            out.push(c);
        } else if it.peek() == Some(&q) {
            out.push(q);
            it.next();
        } else {
            // Text after the closing quote is kept as it is.
            out.extend(it);
            break;
        }
    }
    out
}

/// Guesses the delimiter from the start of a file (`sample`, from after the BOM; `whole` if it
/// is the entire file): the candidate that splits the most lines into the same number of fields
/// (more than one), then the one with more fields, then `preferred` (from the file extension).
/// `None` when no candidate splits any line.
pub fn detect(codec: &Codec, sample: &[u8], whole: bool, preferred: u8) -> Option<Dialect> {
    let unit = codec.unit();
    let mut lines = Vec::new();
    let mut from = 0;
    while from < sample.len() && lines.len() < 200 {
        let Some(i) = codec.find_nl(&sample[from..], from as u64) else {
            if whole {
                lines.push(&sample[from..]);
            }
            break;
        };
        let mut line = &sample[from..from + i];
        if codec.nl == NlByte::Lf && line.ends_with(codec.cr_unit()) {
            line = &line[..line.len() - unit];
        }
        lines.push(line);
        from += i + unit;
    }
    let mut fields = Vec::new();
    let mut best: Option<((usize, usize, bool), u8)> = None;
    for delim in DELIMITERS {
        let d = Dialect::new(delim);
        let mut counts: HashMap<usize, usize> = HashMap::new();
        for l in &lines {
            split_fields(codec, d, l, &mut fields);
            if fields.len() > 1 {
                *counts.entry(fields.len()).or_default() += 1;
            }
        }
        if let Some((&n, &freq)) = counts.iter().max_by_key(|(&n, &freq)| (freq, n)) {
            let key = (freq, n, delim == preferred);
            if best.is_none_or(|(b, _)| key > b) {
                best = Some((key, delim));
            }
        }
    }
    best.map(|(_, delim)| Dialect::new(delim))
}

/// How a scan over the document ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanEnd {
    Done,
    /// The callback asked to stop.
    Stopped,
    Cancelled,
    /// Reading the document failed.
    Failed,
}

/// Per-record bookkeeping of [`scan_removals`]. Only the field starts up to the largest deleted
/// column (plus one) are kept, so memory stays bounded however long a record is.
struct Removal<'a> {
    cols: &'a [usize],
    unit: u64,
    /// Starts of fields `0..=max + 1` of the current record.
    starts: Vec<u64>,
    /// Index of the current field.
    field: usize,
    /// The record has a kept field after every deleted one; its removals are already out.
    decided: bool,
    /// Merges touching ranges (adjacent deleted columns) into one.
    pending: Option<Range<u64>>,
}

type Sink<'s> = dyn FnMut(Range<u64>, &[u8], u64) -> bool + 's;

impl Removal<'_> {
    fn remove(&mut self, r: Range<u64>, chunk: &[u8], base: u64, out: &mut Sink) -> bool {
        if r.is_empty() {
            return true;
        }
        match &mut self.pending {
            Some(p) if p.end == r.start => {
                p.end = r.end;
                true
            }
            _ => match self.pending.replace(r) {
                Some(prev) => out(prev, chunk, base),
                None => true,
            },
        }
    }

    fn flush(&mut self, chunk: &[u8], base: u64, out: &mut Sink) -> bool {
        match self.pending.take() {
            Some(r) => out(r, chunk, base),
            None => true,
        }
    }

    /// Each deleted field goes with the delimiter after it.
    fn remove_with_next(&mut self, upto: usize, chunk: &[u8], base: u64, out: &mut Sink) -> bool {
        for i in 0..self.cols.len() {
            let k = self.cols[i];
            if k >= upto {
                break;
            }
            if !self.remove(self.starts[k]..self.starts[k + 1], chunk, base, out) {
                return false;
            }
        }
        true
    }

    /// Ends a record of `n <= max + 1` fields whose content ends at `end`.
    fn finish(&mut self, n: usize, end: u64, chunk: &[u8], base: u64, out: &mut Sink) -> bool {
        match (0..n).rev().find(|i| self.cols.binary_search(i).is_err()) {
            // Every field goes: the record becomes an empty line.
            None => self.remove(self.starts[0]..end, chunk, base, out),
            Some(last) => {
                if !self.remove_with_next(last, chunk, base, out) {
                    return false;
                }
                // Deleted fields after the last kept one go with the delimiter before them.
                last + 1 >= n || self.remove(self.starts[last + 1] - self.unit..end, chunk, base, out)
            }
        }
    }
}

/// Finds what to remove so that every record loses the fields at the columns `cols` (sorted,
/// distinct) together with one delimiter each, and calls `out` with each byte range in document
/// order, plus the chunk being scanned when it was found (`chunk` starts at offset `base`) for
/// callers that copy the bytes in between. A record that loses all its fields becomes an empty
/// line. Returns the number of records and how the scan ended.
pub fn scan_removals(buf: &TextBuf, d: Dialect, cols: &[usize], cancel: &CancelToken, progress: Option<&Progress>, out: &mut Sink) -> (u64, ScanEnd) {
    let codec = buf.codec;
    let unit = codec.unit();
    let (nl, cr, lf_mode) = (nl_value(&codec), b'\r' as u16, codec.nl == NlByte::Lf);
    let len = buf.len();
    let start = (codec.bom_len as u64).min(len);
    let Some(&max) = cols.last() else { return (0, ScanEnd::Done) };
    let mut rm = Removal { cols, unit: unit as u64, starts: vec![start], field: 0, decided: false, pending: None };
    let mut st = State::FieldStart;
    let (mut records, mut empty, mut prev_cr) = (0u64, true, false);
    let mut end = ScanEnd::Done;
    if let Some(p) = progress {
        p.set_total(len);
    }
    let read = buf.for_each_chunk(start..len, |base, chunk| {
        if cancel.is_cancelled() {
            end = ScanEnd::Cancelled;
            return false;
        }
        let mut i = 0;
        while i + unit <= chunk.len() {
            let c = unit_at(codec.enc, chunk, i);
            let p = base + i as u64;
            let step = st.step(c, d, nl);
            // True right after a line break: no record has started yet.
            empty = step == Step::RecordEnd;
            let ok = match step {
                Step::Inside => true,
                Step::FieldEnd => {
                    rm.field += 1;
                    if rm.field <= max + 1 {
                        rm.starts.push(p + unit as u64);
                    }
                    if rm.field == max + 1 {
                        rm.decided = true;
                        rm.remove_with_next(max + 1, chunk, base, out)
                    } else {
                        true
                    }
                }
                Step::RecordEnd => {
                    records += 1;
                    let content_end = if lf_mode && prev_cr { p - unit as u64 } else { p };
                    let ok = rm.decided || rm.finish(rm.field + 1, content_end, chunk, base, out);
                    rm.starts.clear();
                    rm.starts.push(p + unit as u64);
                    rm.field = 0;
                    rm.decided = false;
                    ok
                }
            };
            if !ok {
                end = ScanEnd::Stopped;
                return false;
            }
            prev_cr = c == cr;
            i += unit;
        }
        if let Some(pr) = progress {
            pr.set_done(base + chunk.len() as u64);
        }
        true
    });
    if end != ScanEnd::Done {
        return (records, end);
    }
    if read.is_none() {
        return (records, ScanEnd::Failed);
    }
    if !empty {
        records += 1;
        if !rm.decided && !rm.finish(rm.field + 1, len, &[], 0, out) {
            return (records, ScanEnd::Stopped);
        }
    }
    if !rm.flush(&[], 0, out) {
        return (records, ScanEnd::Stopped);
    }
    (records, end)
}

/// Writes `range` of the document to `out`, from `chunk` (at offset `base`) where it holds the
/// bytes and from the document otherwise. Returns the byte count.
fn copy_range<W: Write>(buf: &TextBuf, range: Range<u64>, chunk: &[u8], base: u64, out: &mut W, scratch: &mut Vec<u8>) -> io::Result<u64> {
    let (mut p, to) = (range.start, range.end);
    let n = to.saturating_sub(p);
    let chunk_end = base + chunk.len() as u64;
    while p < to {
        if p >= base && p < chunk_end {
            let e = to.min(chunk_end);
            out.write_all(&chunk[(p - base) as usize..(e - base) as usize])?;
            p = e;
        } else {
            const STEP: u64 = 4 * 1024 * 1024;
            let e = (p + STEP).min(to);
            let e = if p < base { e.min(base) } else { e };
            let got = buf.read_to(p..e, scratch).ok_or_else(|| io::Error::other("could not read the document"))?;
            out.write_all(&scratch[..got])?;
            p = e;
        }
    }
    Ok(n)
}

/// Streams the document to `out` without the columns `cols` (sorted, distinct). `count` in the
/// result is the number of records. Returns `Ok(None)` if cancelled.
pub fn delete_columns_to<W: Write>(buf: &TextBuf, d: Dialect, cols: &[usize], out: &mut W, cancel: &CancelToken, progress: Option<&Progress>) -> io::Result<Option<ReplaceStats>> {
    let mut stats = ReplaceStats { bytes_in: buf.len(), ..Default::default() };
    let mut pos = 0u64;
    let mut err = None;
    let mut scratch = Vec::new();
    let (records, end) = scan_removals(buf, d, cols, cancel, progress, &mut |r, chunk, base| match copy_range(buf, pos..r.start, chunk, base, out, &mut scratch) {
        Ok(n) => {
            stats.bytes_out += n;
            pos = r.end;
            true
        }
        Err(e) => {
            err = Some(e);
            false
        }
    });
    if let Some(e) = err {
        return Err(e);
    }
    match end {
        ScanEnd::Cancelled => return Ok(None),
        ScanEnd::Failed | ScanEnd::Stopped => return Err(io::Error::other("could not read the document")),
        ScanEnd::Done => {}
    }
    stats.bytes_out += copy_range(buf, pos..buf.len(), &[], 0, out, &mut scratch)?;
    stats.count = records;
    Ok(Some(stats))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::{OpenMode, Source};
    use crate::index;
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

    fn delete_bytes(data: &[u8], codec: Codec, delim: u8, cols: &[usize]) -> Vec<u8> {
        let (_f, t) = buf_with(data, codec);
        let mut out = Vec::new();
        delete_columns_to(&t, Dialect::new(delim), cols, &mut out, &CancelToken::new(), None).unwrap().unwrap();
        out
    }

    fn delete(data: &str, cols: &[usize]) -> String {
        String::from_utf8(delete_bytes(data.as_bytes(), Codec::utf8(), b',', cols)).unwrap()
    }

    /// Straightforward reference: parse the whole text into records, drop columns, join.
    fn reference(data: &str, delim: char, cols: &[usize]) -> String {
        let (bom, body) = match data.strip_prefix('\u{feff}') {
            Some(b) => ("\u{feff}", b),
            None => ("", data),
        };
        let mut out = String::from(bom);
        let chars: Vec<char> = body.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let mut fields = vec![String::new()];
            let mut in_q = false;
            let mut at_start = true;
            let mut term = "";
            while i < chars.len() {
                let c = chars[i];
                i += 1;
                if in_q {
                    if c == '"' {
                        if chars.get(i) == Some(&'"') {
                            fields.last_mut().unwrap().push_str("\"\"");
                            i += 1;
                            continue;
                        }
                        in_q = false;
                    }
                    fields.last_mut().unwrap().push(c);
                    continue;
                }
                if c == '"' && at_start {
                    in_q = true;
                    at_start = false;
                    fields.last_mut().unwrap().push(c);
                } else if c == delim {
                    fields.push(String::new());
                    at_start = true;
                } else if c == '\n' {
                    term = "\n";
                    break;
                } else {
                    at_start = false;
                    fields.last_mut().unwrap().push(c);
                }
            }
            let mut cr = "";
            if term == "\n" && fields.last().unwrap().ends_with('\r') && !in_q {
                fields.last_mut().unwrap().pop();
                cr = "\r";
            }
            let kept: Vec<&str> = fields.iter().enumerate().filter(|(k, _)| !cols.contains(k)).map(|(_, f)| f.as_str()).collect();
            out.push_str(&kept.join(&delim.to_string()));
            out.push_str(cr);
            out.push_str(term);
        }
        out
    }

    #[test]
    fn deletes_columns() {
        let t = "a,b,c\n1,2,3\n";
        assert_eq!(delete(t, &[1]), "a,c\n1,3\n");
        assert_eq!(delete(t, &[0]), "b,c\n2,3\n");
        assert_eq!(delete(t, &[2]), "a,b\n1,2\n");
        assert_eq!(delete(t, &[1, 2]), "a\n1\n");
        assert_eq!(delete(t, &[0, 2]), "b\n2\n");
        assert_eq!(delete(t, &[0, 1, 2]), "\n\n");
        assert_eq!(delete(t, &[5]), t);
        assert_eq!(delete(t, &[]), t);
    }

    #[test]
    fn ragged_quoted_and_line_ends() {
        assert_eq!(delete("a,b,c\n1\n1,2\n1,2,3,4\n", &[1]), "a,c\n1\n1\n1,3,4\n");
        assert_eq!(delete("\"x,y\",b\n\"multi\nline\",2\n", &[0]), "b\n2\n");
        assert_eq!(delete("\"he said \"\"hi\"\"\",z\n", &[1]), "\"he said \"\"hi\"\"\"\n");
        assert_eq!(delete("a,b\r\n1,2\r\n", &[1]), "a\r\n1\r\n");
        let bom = Codec::new(Encoding::Utf8, NlByte::Lf, 3);
        assert_eq!(delete_bytes("\u{feff}a,b\n1,2".as_bytes(), bom, b',', &[0]), "\u{feff}b\n2".as_bytes());
        assert_eq!(delete("a,b\n\n1,2\n", &[0]), "b\n\n2\n");
        assert_eq!(delete("a,b,c", &[1]), "a,c");
        assert_eq!(delete("", &[0]), "");
    }

    #[test]
    fn other_delimiters_and_utf16() {
        let out = delete_bytes("a;b;c\n1;2;3\n".as_bytes(), Codec::utf8(), b';', &[0]);
        assert_eq!(out, b"b;c\n2;3\n");
        let out = delete_bytes(b"a\tb\n1\t2\n", Codec::utf8(), b'\t', &[1]);
        assert_eq!(out, b"a\n1\n");
        let utf16 = |s: &str| -> Vec<u8> { [0xFF, 0xFE].into_iter().chain(s.encode_utf16().flat_map(|u| u.to_le_bytes())).collect() };
        let codec = Codec::new(Encoding::Utf16Le, NlByte::Lf, 2);
        assert_eq!(delete_bytes(&utf16("ä,b,\"c,d\"\r\n1,2,3\r\n"), codec, b',', &[1]), utf16("ä,\"c,d\"\r\n1,3\r\n"));
    }

    #[test]
    fn matches_reference_across_chunks() {
        // Pseudo-random CSV well over one 4 MB read chunk, with quotes, embedded line breaks,
        // CRLF, empty and ragged lines.
        let mut seed = 12345u64;
        let mut rnd = |n: u64| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) % n
        };
        let mut s = String::new();
        while s.len() < 5 << 20 {
            let fields = 1 + rnd(6);
            for f in 0..fields {
                if f > 0 {
                    s.push(',');
                }
                match rnd(6) {
                    0 => s.push_str("\"q,\"\"x\"\"\ny\""),
                    1 => {}
                    2 => s.push_str("plain text"),
                    _ => s.push_str(&rnd(100000).to_string()),
                }
            }
            s.push_str(if rnd(4) == 0 { "\r\n" } else { "\n" });
        }
        for cols in [&[0][..], &[1, 2], &[0, 3, 5], &[4]] {
            assert!(delete(&s, cols) == reference(&s, ',', cols), "columns {cols:?}");
        }
    }

    #[test]
    fn cancel_leaves_no_result() {
        let (_f, t) = buf_with(b"a,b\n", Codec::utf8());
        let c = CancelToken::new();
        c.cancel();
        let mut out = Vec::new();
        assert!(delete_columns_to(&t, Dialect::new(b','), &[0], &mut out, &c, None).unwrap().is_none());
    }

    #[test]
    fn fields_and_values() {
        let c = Codec::utf8();
        let d = Dialect::new(b',');
        let line = br#"a,"b,""c""",,d"e"#;
        let mut f = Vec::new();
        split_fields(&c, d, line, &mut f);
        let vals: Vec<String> = f.iter().map(|r| field_value(&c, d, &line[r.clone()])).collect();
        assert_eq!(vals, ["a", "b,\"c\"", "", "d\"e"]);
        split_fields(&c, d, b"", &mut f);
        assert_eq!((f.len(), f[0].clone()), (1, 0..0));
        assert_eq!(field_value(&c, d, br#""ab"cd"#), "abcd");
    }

    #[test]
    fn detects_delimiter() {
        let c = Codec::utf8();
        let det = |s: &str, pref| detect(&c, s.as_bytes(), true, pref).map(|d| d.delim);
        assert_eq!(det("a;b;c\n1;2,5;3\n4;5;6\n", b','), Some(b';'));
        assert_eq!(det("name,desc\nx,\"a; b; c\"\ny,z\n", b','), Some(b','));
        assert_eq!(det("a\tb, c\tx\n1\t2, 3\ty\n", b','), Some(b'\t'));
        assert_eq!(det("a|b\n1|2\n", b','), Some(b'|'));
        assert_eq!(det("a,b;c\n1,2;3\n", b';'), Some(b';'));
        assert_eq!(det("single\ncolumn\n", b','), None);
        // A cut-off last line does not count.
        assert_eq!(detect(&c, b"a;b\n1;2\n3,4,5,6", false, b',').map(|d| d.delim), Some(b';'));
    }
}
