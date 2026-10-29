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
    /// Where the table starts: lines before it (Excel's `sep=;`, comments, a title) are not
    /// records, and rewrites keep them as they are.
    pub start: u64,
}

impl Dialect {
    pub fn new(delim: u8) -> Dialect {
        Dialect { delim, quote: b'"', start: 0 }
    }

    /// Where the records start in `buf` (after the byte order mark and the preamble).
    pub fn first(self, buf: &TextBuf) -> u64 {
        self.start.max(buf.codec.bom_len as u64).min(buf.len())
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

/// What the start of a table file says about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TableGuess {
    /// With [`Dialect::start`] after the lines before the table.
    pub dialect: Dialect,
    /// The first row holds the column names.
    pub header: bool,
    /// Lines before the table.
    pub skip: usize,
}

/// Lines before the table that are looked for.
const PREAMBLE_MAX: usize = 20;

/// Like [`detect`], and also finds the lines before the table — Excel's `sep=;` line, blank and
/// `#` lines, a title — and whether the first row holds column names: it does unless its cells
/// are numbers where the rows below hold numbers too. `sample` starts at offset `base` (after
/// the byte order mark).
pub fn detect_table(codec: &Codec, sample: &[u8], whole: bool, preferred: u8, base: u64) -> TableGuess {
    let unit = codec.unit();
    // (start, content) of each line of the sample, and where the next one starts.
    let mut lines: Vec<(usize, &[u8], usize)> = Vec::new();
    let mut from = 0;
    while from < sample.len() && lines.len() < 200 + PREAMBLE_MAX {
        let (end, next) = match codec.find_nl(&sample[from..], from as u64) {
            Some(i) => (from + i, from + i + unit),
            None if whole => (sample.len(), sample.len()),
            None => break,
        };
        let mut line = &sample[from..end];
        if codec.nl == NlByte::Lf && line.ends_with(codec.cr_unit()) {
            line = &line[..line.len() - unit];
        }
        lines.push((from, line, next));
        from = next;
    }
    let text = |b: &[u8]| codec.decode_lossy(b);
    let mut skip = 0;
    let mut d = match lines.first().map(|l| text(l.1)) {
        Some(t) if t.trim().len() == 5 && t.trim().to_ascii_lowercase().starts_with("sep=") && t.trim().is_char_boundary(4) && t.trim().as_bytes()[4].is_ascii() => {
            skip = 1;
            Dialect::new(t.trim().as_bytes()[4])
        }
        _ => detect(codec, sample, whole, preferred).unwrap_or(Dialect::new(preferred)),
    };
    let mut fields = Vec::new();
    let mut count = |l: &[u8]| {
        split_fields(codec, d, l, &mut fields);
        fields.len()
    };
    let counts: Vec<usize> = lines.iter().map(|l| count(l.1)).collect();
    // The usual field count, then the first line (after a sep= line) from which two rows in a
    // row have it.
    let mut tally: HashMap<usize, usize> = HashMap::new();
    for &c in &counts[skip.min(counts.len())..] {
        *tally.entry(c).or_default() += 1;
    }
    let modal = tally.into_iter().max_by_key(|&(c, n)| (n, c)).map(|(c, _)| c).unwrap_or(1);
    if modal > 1 {
        let from = skip;
        let preamble = |i: usize| {
            let t = text(lines[i].1);
            t.trim().is_empty() || t.trim_start().starts_with('#') || counts[i] != modal
        };
        if let Some(k) = (from..lines.len().min(from + PREAMBLE_MAX)).find(|&k| counts[k] == modal && counts.get(k + 1) == Some(&modal)) {
            if (from..k).all(preamble) {
                skip = k;
            }
        }
    }
    if let Some(l) = lines.get(skip).filter(|_| skip > 0) {
        d.start = base + l.0 as u64;
    }
    // Header: numbers in the first row where the rows below have numbers mean it is data.
    let cells = |l: &[u8], fields: &mut Vec<Range<usize>>| -> Vec<bool> {
        split_fields(codec, d, l, fields);
        fields.iter().map(|f| parse_number(&field_value(codec, d, &l[f.clone()])).is_some()).collect()
    };
    let mut header = true;
    if let Some(first) = lines.get(skip) {
        let head = cells(first.1, &mut fields);
        let rows: Vec<Vec<bool>> = lines.iter().skip(skip + 1).take(200).map(|l| cells(l.1, &mut fields)).collect();
        if rows.len() >= 2 {
            let numeric = |c: usize| rows.iter().filter(|r| r.get(c) == Some(&true)).count() * 10 >= rows.len() * 8;
            let cols: Vec<usize> = (0..head.len()).filter(|&c| numeric(c)).collect();
            let data_like = cols.iter().filter(|&&c| head[c]).count();
            header = cols.is_empty() || data_like < cols.len();
        }
    }
    TableGuess { dialect: d, header, skip }
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
    let start = d.first(buf);
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

/// One record of the document: its content (without the line break), where the next record
/// starts, and its fields (quotes included). A record can span lines when a quoted field
/// holds line breaks.
pub struct Record<'a> {
    pub content: Range<u64>,
    pub next: u64,
    pub fields: &'a [Range<u64>],
}

/// Calls `f` for every record in order, until it returns false. Streams over the document, so
/// memory stays bounded by the widest record.
pub fn for_each_record(buf: &TextBuf, d: Dialect, cancel: &CancelToken, progress: Option<&Progress>, mut f: impl FnMut(&Record) -> bool) -> ScanEnd {
    let codec = buf.codec;
    let unit = codec.unit();
    let (nl, cr, lf_mode) = (nl_value(&codec), b'\r' as u16, codec.nl == NlByte::Lf);
    let len = buf.len();
    let start = d.first(buf);
    let mut st = State::FieldStart;
    let mut fields: Vec<Range<u64>> = Vec::new();
    let (mut rec_start, mut field_start) = (start, start);
    let (mut empty, mut prev_cr) = (true, false);
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
            empty = step == Step::RecordEnd;
            match step {
                Step::Inside => {}
                Step::FieldEnd => {
                    fields.push(field_start..p);
                    field_start = p + unit as u64;
                }
                Step::RecordEnd => {
                    let content_end = if lf_mode && prev_cr { p - unit as u64 } else { p }.max(field_start);
                    fields.push(field_start..content_end);
                    if !f(&Record { content: rec_start..content_end, next: p + unit as u64, fields: &fields }) {
                        end = ScanEnd::Stopped;
                        return false;
                    }
                    fields.clear();
                    rec_start = p + unit as u64;
                    field_start = rec_start;
                }
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
        return end;
    }
    if read.is_none() {
        return ScanEnd::Failed;
    }
    if !empty {
        fields.push(field_start..len);
        if !f(&Record { content: rec_start..len, next: len, fields: &fields }) {
            return ScanEnd::Stopped;
        }
    }
    ScanEnd::Done
}

/// A number as spreadsheets read it: `12`, `-3.5`, `1e6`, and `3,5` (decimal comma).
pub fn parse_number(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() || !t.bytes().any(|b| b.is_ascii_digit()) || !t.bytes().all(|b| b.is_ascii_digit() || b"+-.,eE".contains(&b)) {
        return None;
    }
    t.parse().ok().or_else(|| (t.matches(',').count() == 1 && !t.contains('.')).then(|| t.replace(',', ".").parse().ok()).flatten())
}

/// Distinct values counted by [`column_stats`] before it stops tracking new ones.
pub const DISTINCT_MAX: usize = 100_000;

/// Summary of one column.
#[derive(Clone, Debug, Default)]
pub struct ColumnStats {
    pub col: usize,
    pub name: String,
    /// Data rows (the header excluded).
    pub rows: u64,
    pub empty: u64,
    pub numeric: u64,
    pub sum: f64,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub longest: usize,
    /// Distinct values (a lower bound when `distinct_capped`).
    pub distinct: usize,
    pub distinct_capped: bool,
    /// The most frequent values with their counts (approximate when capped).
    pub top: Vec<(String, u64)>,
}

impl ColumnStats {
    pub fn mean(&self) -> Option<f64> {
        (self.numeric > 0).then(|| self.sum / self.numeric as f64)
    }
}

/// Counts, numeric summary, distinct values and the most frequent values of column `col`,
/// in one pass. With `header`, the first record names the column and is not counted.
pub fn column_stats(buf: &TextBuf, d: Dialect, col: usize, header: bool, cancel: &CancelToken, progress: Option<&Progress>) -> Result<ColumnStats, ScanEnd> {
    let codec = buf.codec;
    let mut s = ColumnStats { col, ..Default::default() };
    let mut counts: HashMap<String, u64> = HashMap::new();
    let mut first = header;
    let mut failed = false;
    let end = for_each_record(buf, d, cancel, progress, |r| {
        let value = match r.fields.get(col) {
            Some(f) => match buf.read(f.clone()) {
                Some(b) => field_value(&codec, d, &b),
                None => {
                    failed = true;
                    return false;
                }
            },
            None => String::new(),
        };
        if first {
            first = false;
            s.name = value;
            return true;
        }
        // A blank line at the very end is not a row.
        if r.content.is_empty() && r.next == r.content.end && r.fields.len() == 1 {
            return true;
        }
        s.rows += 1;
        if value.is_empty() {
            s.empty += 1;
            return true;
        }
        s.longest = s.longest.max(value.chars().count());
        if let Some(n) = parse_number(&value) {
            s.numeric += 1;
            s.sum += n;
            s.min = Some(s.min.map_or(n, |m| m.min(n)));
            s.max = Some(s.max.map_or(n, |m| m.max(n)));
        }
        let key: String = value.chars().take(200).collect();
        if let Some(c) = counts.get_mut(&key) {
            *c += 1;
        } else if counts.len() < DISTINCT_MAX {
            counts.insert(key, 1);
        } else {
            s.distinct_capped = true;
        }
        true
    });
    if failed {
        return Err(ScanEnd::Failed);
    }
    if end != ScanEnd::Done {
        return Err(end);
    }
    s.distinct = counts.len();
    let mut top: Vec<(String, u64)> = counts.into_iter().collect();
    top.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    top.truncate(10);
    s.top = top;
    Ok(s)
}

/// Groups kept by [`group_by`] before it stops adding new ones.
pub const GROUPS_MAX: usize = 1_000_000;

/// The rows of one value of the grouping column.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Group {
    pub key: String,
    pub rows: u64,
    /// Numbers in the summed column, their sum, minimum and maximum.
    pub numeric: u64,
    pub sum: f64,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

impl Group {
    pub fn mean(&self) -> Option<f64> {
        (self.numeric > 0).then(|| self.sum / self.numeric as f64)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Groups {
    pub key_name: String,
    pub value_name: Option<String>,
    /// Largest groups first.
    pub groups: Vec<Group>,
    pub rows: u64,
    /// More than [`GROUPS_MAX`] values: rows of the others are not counted.
    pub capped: bool,
}

/// Counts the rows per value of column `key` and, with `value`, sums that column per group
/// (GROUP BY). With `header`, the first record names the columns.
pub fn group_by(buf: &TextBuf, d: Dialect, key: usize, value: Option<usize>, header: bool, cancel: &CancelToken, progress: Option<&Progress>) -> Result<Groups, ScanEnd> {
    let codec = buf.codec;
    let mut g = Groups::default();
    let mut map: HashMap<String, Group> = HashMap::new();
    let mut first = header;
    let mut failed = false;
    let field = |r: &Record, col: usize, failed: &mut bool| -> String {
        match r.fields.get(col) {
            Some(f) => match buf.read(f.clone()) {
                Some(b) => field_value(&codec, d, &b),
                None => {
                    *failed = true;
                    String::new()
                }
            },
            None => String::new(),
        }
    };
    let end = for_each_record(buf, d, cancel, progress, |r| {
        let k = field(r, key, &mut failed);
        let v = value.map(|c| field(r, c, &mut failed));
        if failed {
            return false;
        }
        if first {
            first = false;
            g.key_name = k;
            g.value_name = v;
            return true;
        }
        if r.content.is_empty() && r.next == r.content.end && r.fields.len() == 1 {
            return true;
        }
        g.rows += 1;
        let full = map.len() >= GROUPS_MAX;
        let entry = match map.entry(k) {
            std::collections::hash_map::Entry::Occupied(o) => o.into_mut(),
            std::collections::hash_map::Entry::Vacant(v) if !full => {
                let key = v.key().clone();
                v.insert(Group { key, ..Default::default() })
            }
            std::collections::hash_map::Entry::Vacant(_) => {
                g.capped = true;
                return true;
            }
        };
        entry.rows += 1;
        if let Some(n) = v.as_deref().and_then(parse_number) {
            entry.numeric += 1;
            entry.sum += n;
            entry.min = Some(entry.min.map_or(n, |m| m.min(n)));
            entry.max = Some(entry.max.map_or(n, |m| m.max(n)));
        }
        true
    });
    if failed {
        return Err(ScanEnd::Failed);
    }
    if end != ScanEnd::Done {
        return Err(end);
    }
    g.groups = map.into_values().collect();
    g.groups.sort_by(|a, b| b.rows.cmp(&a.rows).then_with(|| a.key.cmp(&b.key)));
    Ok(g)
}

/// Writes groups as CSV: the value, the row count, and with a summed column its sum, mean,
/// minimum and maximum.
pub fn groups_to_csv<W: Write>(g: &Groups, out: &mut W) -> io::Result<()> {
    let q = |s: &str| if s.contains([',', '"', '\n', '\r']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_string() };
    let num = |x: Option<f64>| x.map(|x| x.to_string()).unwrap_or_default();
    let key = if g.key_name.is_empty() { "value".to_string() } else { g.key_name.clone() };
    match &g.value_name {
        Some(v) => {
            let v = if v.is_empty() { "value".to_string() } else { v.clone() };
            writeln!(out, "{},rows,{},{},{},{}", q(&key), q(&format!("sum of {v}")), q(&format!("mean of {v}")), q(&format!("min of {v}")), q(&format!("max of {v}")))?;
            for x in &g.groups {
                writeln!(out, "{},{},{},{},{},{}", q(&x.key), x.rows, if x.numeric > 0 { x.sum.to_string() } else { String::new() }, num(x.mean()), num(x.min), num(x.max))?;
            }
        }
        None => {
            writeln!(out, "{},rows", q(&key))?;
            for x in &g.groups {
                writeln!(out, "{},{}", q(&x.key), x.rows)?;
            }
        }
    }
    Ok(())
}

/// How a condition compares a field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CondOp {
    Contains,
    NotContains,
    Equals,
    NotEquals,
    Greater,
    Less,
    Empty,
    NotEmpty,
}

impl CondOp {
    pub const ALL: [CondOp; 8] = [CondOp::Contains, CondOp::NotContains, CondOp::Equals, CondOp::NotEquals, CondOp::Greater, CondOp::Less, CondOp::Empty, CondOp::NotEmpty];

    pub fn name(self) -> &'static str {
        match self {
            CondOp::Contains => "contains",
            CondOp::NotContains => "does not contain",
            CondOp::Equals => "is",
            CondOp::NotEquals => "is not",
            CondOp::Greater => ">",
            CondOp::Less => "<",
            CondOp::Empty => "is empty",
            CondOp::NotEmpty => "is not empty",
        }
    }

    pub fn takes_value(self) -> bool {
        !matches!(self, CondOp::Empty | CondOp::NotEmpty)
    }
}

/// A condition on one column. Text comparisons ignore case; `>` and `<` compare numbers.
#[derive(Clone, Debug, PartialEq)]
pub struct Cond {
    pub col: usize,
    pub op: CondOp,
    pub value: String,
}

impl Cond {
    pub fn test(&self, field: &str) -> bool {
        let (f, v) = (field.to_lowercase(), self.value.to_lowercase());
        match self.op {
            CondOp::Contains => f.contains(&v),
            CondOp::NotContains => !f.contains(&v),
            CondOp::Equals => f.trim() == v.trim(),
            CondOp::NotEquals => f.trim() != v.trim(),
            CondOp::Greater | CondOp::Less => match (parse_number(field), parse_number(&self.value)) {
                (Some(a), Some(b)) => if self.op == CondOp::Greater { a > b } else { a < b },
                _ => false,
            },
            CondOp::Empty => field.trim().is_empty(),
            CondOp::NotEmpty => !field.trim().is_empty(),
        }
    }
}

/// Writes the rows that meet every condition to `out` as they are (with the header when
/// `header`, and the byte order mark). Returns (rows kept, rows), or `None` if cancelled.
pub fn filter_rows_to<W: Write>(buf: &TextBuf, d: Dialect, conds: &[Cond], header: bool, out: &mut W, cancel: &CancelToken, progress: Option<&Progress>) -> io::Result<Option<(u64, u64)>> {
    let codec = buf.codec;
    let read_err = || io::Error::other("could not read the document");
    out.write_all(&buf.read(0..(codec.bom_len as u64).min(buf.len())).ok_or_else(read_err)?)?;
    let (mut kept, mut rows) = (0u64, 0u64);
    let mut first = header;
    let mut err: Option<io::Error> = None;
    let end = for_each_record(buf, d, cancel, progress, |r| {
        let keep = if first {
            first = false;
            true
        } else if r.content.is_empty() && r.next == r.content.end && r.fields.len() == 1 {
            false
        } else {
            rows += 1;
            let ok = conds.iter().all(|c| {
                let v = r.fields.get(c.col).and_then(|f| buf.read(f.clone())).map(|b| field_value(&codec, d, &b)).unwrap_or_default();
                c.test(&v)
            });
            kept += ok as u64;
            ok
        };
        if keep {
            let r2 = buf.read(r.content.start..r.next).ok_or_else(read_err).and_then(|b| out.write_all(&b));
            if let Err(e) = r2 {
                err = Some(e);
                return false;
            }
            // The last record may have no line break.
            if r.next == r.content.end {
                if let Err(e) = out.write_all(&codec.encode("\n")) {
                    err = Some(e);
                    return false;
                }
            }
        }
        true
    });
    if let Some(e) = err {
        return Err(e);
    }
    match end {
        ScanEnd::Cancelled => Ok(None),
        ScanEnd::Done => Ok(Some((kept, rows))),
        _ => Err(read_err()),
    }
}

/// A rewrite of every record of a CSV document.
#[derive(Clone, Debug, PartialEq)]
pub enum Rewrite {
    /// Rows ordered by the value of column `col` (numbers numerically when every non-empty
    /// value is a number, otherwise text, ignoring case); empty values last. The header stays
    /// first. The sort is stable.
    Sort { col: usize, desc: bool, header: bool },
    /// Exchange two columns in every record that has both.
    Swap(usize, usize),
    /// Keep only these columns, in this order.
    Select(Vec<usize>),
}

/// Streams the document rewritten by `rw` to `out` (with its byte order mark). `count` in the
/// result is the number of records. Sorting holds every row's key in memory. Returns
/// `Ok(None)` if cancelled.
pub fn rewrite_to<W: Write>(buf: &TextBuf, d: Dialect, rw: &Rewrite, out: &mut W, cancel: &CancelToken, progress: Option<&Progress>) -> io::Result<Option<ReplaceStats>> {
    let codec = buf.codec;
    let read_err = || io::Error::other("could not read the document");
    let mut stats = ReplaceStats { bytes_in: buf.len(), ..Default::default() };
    let bom = codec.bom_len as u64;
    let mut emit = |b: &[u8], stats: &mut ReplaceStats| -> io::Result<()> {
        stats.bytes_out += b.len() as u64;
        out.write_all(b)
    };
    // The byte order mark and the lines before the table, as they are.
    emit(&buf.read(0..d.first(buf).max(bom.min(buf.len()))).ok_or_else(read_err)?, &mut stats)?;
    let delim = codec.encode(&(d.delim as char).to_string());
    let mut err: Option<io::Error> = None;
    let end = match rw {
        Rewrite::Swap(..) | Rewrite::Select(_) => for_each_record(buf, d, cancel, progress, |r| {
            let res = (|| -> io::Result<()> {
                let order: Vec<usize> = match rw {
                    Rewrite::Swap(a, b) if r.fields.len() > *a.max(b) => (0..r.fields.len()).map(|i| if i == *a { *b } else if i == *b { *a } else { i }).collect(),
                    Rewrite::Swap(..) => (0..r.fields.len()).collect(),
                    Rewrite::Select(cols) => cols.clone(),
                    Rewrite::Sort { .. } => unreachable!(),
                };
                // Keep blank lines blank.
                if !(r.content.is_empty() && r.fields.len() == 1) {
                    for (i, &c) in order.iter().enumerate() {
                        if i > 0 {
                            emit(&delim, &mut stats)?;
                        }
                        if let Some(f) = r.fields.get(c) {
                            emit(&buf.read(f.clone()).ok_or_else(read_err)?, &mut stats)?;
                        }
                    }
                }
                emit(&buf.read(r.content.end..r.next).ok_or_else(read_err)?, &mut stats)?;
                Ok(())
            })();
            stats.count += 1;
            match res {
                Ok(()) => true,
                Err(e) => {
                    err = Some(e);
                    false
                }
            }
        }),
        Rewrite::Sort { col, desc, header } => {
            // (number, text key, start, content end, next) of every data row.
            let mut rows: Vec<(Option<f64>, String, u64, u64, u64)> = Vec::new();
            let mut head: Option<(u64, u64, u64)> = None;
            let mut all_numeric = true;
            let end = for_each_record(buf, d, cancel, progress, |r| {
                let start = r.content.start;
                if *header && head.is_none() {
                    head = Some((start, r.content.end, r.next));
                    return true;
                }
                let value = match r.fields.get(*col).map(|f| buf.read(f.clone())) {
                    Some(Some(b)) => field_value(&codec, d, &b),
                    Some(None) => {
                        err = Some(read_err());
                        return false;
                    }
                    None => String::new(),
                };
                let num = parse_number(&value);
                if num.is_none() && !value.trim().is_empty() {
                    all_numeric = false;
                }
                rows.push((num, value.to_lowercase().chars().take(256).collect(), start, r.content.end, r.next));
                true
            });
            if end == ScanEnd::Done && err.is_none() {
                // Empty values last in both directions.
                let numeric = all_numeric && rows.iter().any(|r| r.0.is_some());
                rows.sort_by(|a, b| {
                    let (ea, eb) = (a.1.trim().is_empty(), b.1.trim().is_empty());
                    if ea != eb {
                        return ea.cmp(&eb);
                    }
                    let o = if numeric { a.0.unwrap_or(0.0).total_cmp(&b.0.unwrap_or(0.0)) } else { a.1.cmp(&b.1) };
                    if *desc {
                        o.reverse()
                    } else {
                        o
                    }
                });
                // A row without a line break (the file's last) gets the break of another record
                // when it moves up.
                let brk = head
                    .map(|h| (h.1, h.2))
                    .into_iter()
                    .chain(rows.iter().map(|r| (r.3, r.4)))
                    .find(|(e, n)| n > e)
                    .and_then(|(e, n)| buf.read(e..n).map(|b| b.into_owned()))
                    .unwrap_or_else(|| codec.encode("\n"));
                let res = (|| -> io::Result<()> {
                    if let Some((s, _, n)) = head {
                        emit(&buf.read(s..n).ok_or_else(read_err)?, &mut stats)?;
                        stats.count += 1;
                    }
                    let last = rows.len().saturating_sub(1);
                    for (i, r) in rows.iter().enumerate() {
                        if cancel.is_cancelled() {
                            return Ok(());
                        }
                        emit(&buf.read(r.2..r.4).ok_or_else(read_err)?, &mut stats)?;
                        if i < last && r.4 == r.3 {
                            emit(&brk, &mut stats)?;
                        }
                        stats.count += 1;
                    }
                    Ok(())
                })();
                if let Err(e) = res {
                    err = Some(e);
                }
            }
            if cancel.is_cancelled() {
                ScanEnd::Cancelled
            } else {
                end
            }
        }
    };
    if let Some(e) = err {
        return Err(e);
    }
    match end {
        ScanEnd::Done => Ok(Some(stats)),
        ScanEnd::Cancelled => Ok(None),
        _ => Err(read_err()),
    }
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
    fn detects_preamble_and_header() {
        let c = Codec::utf8();
        let g = |s: &str| detect_table(&c, s.as_bytes(), true, b',', 3);
        // Excel's sep= line.
        let t = g("sep=;\nname;qty\nann;1\nbob;2\n");
        assert_eq!((t.dialect.delim, t.skip, t.dialect.start, t.header), (b';', 1, 3 + 6, true));
        // Comments and a title before the header.
        let t = g("# exported 2024-05-01\nSales report\n\nid,city,qty\n1,Paris,3\n2,Rome,4\n3,Oslo,5\n");
        assert_eq!((t.skip, t.header), (3, true));
        assert_eq!(t.dialect.start, 3 + "# exported 2024-05-01\nSales report\n\n".len() as u64);
        // No header: numbers where the rows below have numbers.
        let t = g("1,2.5,x\n3,4,y\n5,6,z\n");
        assert_eq!((t.skip, t.header, t.dialect.start), (0, false, 0));
        // All text, or a text name over numbers: a header.
        assert!(g("a,b\nc,d\ne,f\n").header);
        assert!(g("id,qty\n1,2\n3,4\n").header);
        // A ragged first data row is not taken for a preamble.
        assert_eq!(g("a,b,c\n1,2,3\n4,5,6\n7,8,9\n").skip, 0);
    }

    #[test]
    fn preamble_is_kept_by_rewrites() {
        let data = b"sep=,\nk,v\nb,2\na,1\n";
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(data).unwrap();
        f.flush().unwrap();
        let src = std::sync::Arc::new(crate::file::Source::open(f.path(), crate::file::OpenMode::Auto).unwrap());
        let idx = crate::index::build_blocking(&src, Codec::utf8());
        let mut t = TextBuf::new(src, idx, Codec::utf8());
        t.blocking = true;
        let d = detect_table(&t.codec, data, true, b',', 0).dialect;
        assert_eq!(d.start, 6);
        let mut out = Vec::new();
        rewrite_to(&t, d, &Rewrite::Sort { col: 0, desc: false, header: true }, &mut out, &CancelToken::new(), None).unwrap();
        assert_eq!(out, b"sep=,\nk,v\na,1\nb,2\n");
        let mut n = 0;
        for_each_record(&t, d, &CancelToken::new(), None, |_| {
            n += 1;
            true
        });
        assert_eq!(n, 3);
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

#[cfg(test)]
mod group_tests {
    use super::*;
    use crate::file::{OpenMode, Source};
    use crate::index::LineIndex;
    use std::sync::Arc;

    fn buf(text: &str) -> (tempfile::TempDir, TextBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.csv");
        std::fs::write(&path, text).unwrap();
        let src = Arc::new(Source::open(&path, OpenMode::Auto).unwrap());
        let len = src.len();
        (dir, TextBuf::new(src, LineIndex::new(len), Codec::utf8()))
    }

    const T: &str = "city,qty,note\r\nTashkent,3,a\r\nBukhara,5,\"b, c\"\r\nTashkent,x,d\r\nTashkent,7,e\r\n";

    #[test]
    fn groups_count_and_sum() {
        let (_d, b) = buf(T);
        let g = group_by(&b, Dialect::new(b','), 0, Some(1), true, &CancelToken::new(), None).unwrap();
        assert_eq!((g.key_name.as_str(), g.value_name.as_deref(), g.rows, g.capped), ("city", Some("qty"), 4, false));
        assert_eq!(g.groups[0], Group { key: "Tashkent".into(), rows: 3, numeric: 2, sum: 10.0, min: Some(3.0), max: Some(7.0) });
        assert_eq!((g.groups[1].key.as_str(), g.groups[1].rows), ("Bukhara", 1));
        let mut out = Vec::new();
        groups_to_csv(&g, &mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "city,rows,sum of qty,mean of qty,min of qty,max of qty\nTashkent,3,10,5,3,7\nBukhara,1,5,5,5,5\n");
        let g = group_by(&b, Dialect::new(b','), 2, None, true, &CancelToken::new(), None).unwrap();
        assert_eq!(g.groups.iter().map(|x| x.key.as_str()).collect::<Vec<_>>(), vec!["a", "b, c", "d", "e"]);
    }

    #[test]
    fn rows_meeting_every_condition() {
        let (_d, b) = buf(T);
        let conds = [Cond { col: 0, op: CondOp::Equals, value: "tashkent".into() }, Cond { col: 1, op: CondOp::Greater, value: "4".into() }];
        let mut out = Vec::new();
        let r = filter_rows_to(&b, Dialect::new(b','), &conds, true, &mut out, &CancelToken::new(), None).unwrap();
        assert_eq!(r, Some((1, 4)));
        assert_eq!(String::from_utf8(out).unwrap(), "city,qty,note\r\nTashkent,7,e\r\n");
        let c = Cond { col: 2, op: CondOp::Contains, value: ", C".into() };
        assert!(c.test("b, c") && !c.test("d"));
        assert!(Cond { col: 0, op: CondOp::Empty, value: String::new() }.test("  "));
        assert!(!Cond { col: 0, op: CondOp::Less, value: "x".into() }.test("1"));
    }
}

#[cfg(test)]
mod rewrite_tests {
    use super::*;
    use crate::file::{OpenMode, Source};
    use crate::index;
    use std::sync::Arc;

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

    fn rewrite(data: &str, rw: Rewrite) -> String {
        let (_f, t) = buf_with(data.as_bytes());
        let mut out = Vec::new();
        rewrite_to(&t, Dialect::new(b','), &rw, &mut out, &CancelToken::new(), None).unwrap().unwrap();
        String::from_utf8(out).unwrap()
    }

    const DATA: &str = "name,qty,note\r\nb,10,\"x\r\ny\"\r\na,9,\r\nC,,z\r\nd,100,last";

    #[test]
    fn records_span_quoted_line_breaks() {
        let (_f, t) = buf_with(DATA.as_bytes());
        let mut n = Vec::new();
        for_each_record(&t, Dialect::new(b','), &CancelToken::new(), None, |r| {
            n.push(r.fields.len());
            true
        });
        assert_eq!(n, vec![3, 3, 3, 3, 3]);
    }

    #[test]
    fn sort_numeric_text_and_descending() {
        let by_qty = rewrite(DATA, Rewrite::Sort { col: 1, desc: false, header: true });
        assert_eq!(by_qty, "name,qty,note\r\na,9,\r\nb,10,\"x\r\ny\"\r\nd,100,last\r\nC,,z\r\n");
        let by_name_desc = rewrite(DATA, Rewrite::Sort { col: 0, desc: true, header: true });
        assert_eq!(by_name_desc, "name,qty,note\r\nd,100,last\r\nC,,z\r\nb,10,\"x\r\ny\"\r\na,9,\r\n");
    }

    #[test]
    fn swap_and_select_columns() {
        assert_eq!(rewrite("a,b,c\n1,2,3\n\n4,5\n", Rewrite::Swap(0, 2)), "c,b,a\n3,2,1\n\n4,5\n");
        assert_eq!(rewrite("a,b,c\n1,\"2,x\",3\n", Rewrite::Select(vec![2, 1])), "c,b\n3,\"2,x\"\n");
    }

    #[test]
    fn stats() {
        let (_f, t) = buf_with(b"id,amount,city\n1,10.5,Paris\n2,\"3,5\",Rome\n3,,Paris\n4,x,Oslo\n");
        let s = column_stats(&t, Dialect::new(b','), 1, true, &CancelToken::new(), None).unwrap();
        assert_eq!((s.name.as_str(), s.rows, s.empty, s.numeric), ("amount", 4, 1, 2));
        assert_eq!((s.min, s.max, s.mean()), (Some(3.5), Some(10.5), Some(7.0)));
        let c = column_stats(&t, Dialect::new(b','), 2, true, &CancelToken::new(), None).unwrap();
        assert_eq!((c.distinct, c.top[0].clone()), (3, ("Paris".to_string(), 2)));
        assert_eq!(parse_number(" -1e3 "), Some(-1000.0));
        assert_eq!(parse_number("1.2.3"), None);
        assert_eq!(parse_number("inf"), None);
    }
}
