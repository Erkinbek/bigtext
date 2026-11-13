//! CSV records as table rows. A record is usually one line, but a quoted field may hold line
//! breaks, and the table shows such a record as one row.
//!
//! [`RecordIndex`] keeps one byte for each 64 KB block of the document: the quote state at the
//! block's start and whether a record starts in it. One pass in the background fills it
//! ([`index_records`]), from the start of the file on. With it, the record around any offset is
//! found by reading at most two blocks: [`seg_start`] and [`seg_at`] are the record versions of
//! `rows::seg_start` and `rows::seg_at`. Like a long line, a record longer than [`SEG`] is
//! soft-broken at multiples of `SEG`, so every row stays bounded, even when an unclosed quote
//! makes the rest of the file one record.

use std::sync::{Arc, Mutex};

use crate::csv::{nl_value, unit_at, Dialect, ScanEnd, State, Step};
use crate::encoding::NlByte;
use crate::jobs::{CancelToken, Progress};
use crate::text::rows::{self, Pending, Row, Seg, SEG};
use crate::text::TextBuf;

/// The state machine's state at the block's first byte.
const STATE: u8 = 0b0011;
/// A record starts at the block's first byte.
const START_AT: u8 = 0b0100;
/// A record starts later inside the block.
const START_IN: u8 = 0b1000;

/// How often the pass publishes what it has read, in bytes.
const PUBLISH_EVERY: u64 = 64 << 20;

fn state_bits(s: State) -> u8 {
    match s {
        State::FieldStart => 0,
        State::Unquoted => 1,
        State::Quoted => 2,
        State::QuoteInQuoted => 3,
    }
}

fn state_of(b: u8) -> State {
    match b & STATE {
        0 => State::FieldStart,
        1 => State::Unquoted,
        2 => State::Quoted,
        _ => State::QuoteInQuoted,
    }
}

/// Where the records of a CSV document start, one byte per 64 KB block.
#[derive(Clone, Debug)]
pub struct RecordIndex {
    pub dialect: Dialect,
    /// Where the records start ([`Dialect::first`]).
    pub first: u64,
    blocks: Vec<u8>,
    /// How far the pass has read; the blocks below it are complete.
    pub scanned: u64,
    pub done: bool,
    /// Some record spans more than one line.
    pub multiline: bool,
}

impl RecordIndex {
    fn new(dialect: Dialect, first: u64) -> RecordIndex {
        RecordIndex {
            dialect,
            first,
            blocks: Vec::new(),
            scanned: first,
            done: false,
            multiline: false,
        }
    }

    /// An index that covers nothing yet, until a pass publishes one.
    pub fn empty(dialect: Dialect) -> RecordIndex {
        RecordIndex::new(dialect, 0)
    }

    /// True if the rows around `p` can be worked out: `p` is past the start of the records and
    /// its block has been read.
    pub fn covers(&self, p: u64) -> bool {
        p >= self.first && (self.done || (p / SEG + 1) * SEG <= self.scanned)
    }

    fn block(&self, b: u64) -> u8 {
        self.blocks.get(b as usize).copied().unwrap_or(0)
    }

    /// Where to start reading block `b`, the state there, and whether a record starts there.
    fn block_start(&self, b: u64) -> (u64, State, bool) {
        let bs = b * SEG;
        if bs <= self.first {
            (self.first, State::FieldStart, true)
        } else {
            let x = self.block(b);
            (bs, state_of(x), x & START_AT != 0)
        }
    }
}

/// Reads the whole document once and fills the index in `out`, publishing it as it goes (every
/// 64 MB and at the end, calling `notify` each time), so the rows near the start are exact
/// long before the end is read.
pub fn index_records(
    buf: &TextBuf,
    d: Dialect,
    cancel: &CancelToken,
    progress: Option<&Progress>,
    out: &Mutex<Arc<RecordIndex>>,
    notify: &dyn Fn(),
) -> ScanEnd {
    let codec = buf.codec;
    let unit = codec.unit();
    let nl = nl_value(&codec);
    let len = buf.len();
    let first = d.first(buf);
    let mut idx = RecordIndex::new(d, first);
    // The blocks before the one holding `first` hold no records.
    idx.blocks.resize((first / SEG) as usize + 1, 0);
    let mut next_boundary = (first / SEG + 1) * SEG;
    let mut st = State::FieldStart;
    let mut start_pending = first < len;
    let mut published = first;
    let mut end = ScanEnd::Done;
    if let Some(p) = progress {
        p.set_total(len);
    }
    let read = buf.for_each_chunk(first..len, |base, chunk| {
        if cancel.is_cancelled() {
            end = ScanEnd::Cancelled;
            return false;
        }
        let mut i = 0;
        while i + unit <= chunk.len() {
            let p = base + i as u64;
            if p == next_boundary {
                idx.blocks.push(state_bits(st));
                next_boundary += SEG;
            }
            let block = (p / SEG) as usize;
            if start_pending {
                idx.blocks[block] |= if p.is_multiple_of(SEG) {
                    START_AT
                } else {
                    START_IN
                };
                start_pending = false;
            }
            // Up to the next block boundary (or the chunk's end) only the state machine runs; a
            // record that starts right at that point is marked when it is reached.
            let stop = ((next_boundary - base) as usize).min(chunk.len());
            let mut j = i;
            while j + unit <= stop {
                let c = if unit == 1 {
                    chunk[j] as u16
                } else {
                    unit_at(codec.enc, chunk, j)
                };
                if c == nl && st == State::Quoted {
                    idx.multiline = true;
                }
                if st.step(c, d, nl) == Step::RecordEnd {
                    if j + unit < stop {
                        idx.blocks[block] |= START_IN;
                    } else {
                        start_pending = true;
                    }
                }
                j += unit;
            }
            i = stop.max(i + unit);
        }
        idx.scanned = base + chunk.len() as u64;
        if let Some(pr) = progress {
            pr.set_done(idx.scanned);
        }
        if idx.scanned - published >= PUBLISH_EVERY {
            published = idx.scanned;
            *out.lock().unwrap() = Arc::new(idx.clone());
            notify();
        }
        true
    });
    if end != ScanEnd::Done {
        return end;
    }
    if read.is_none() {
        return ScanEnd::Failed;
    }
    // A document that ends on a block boundary: the state there tells whether the empty row
    // after the last line break starts a record.
    if next_boundary == len {
        idx.blocks
            .push(state_bits(st) | if start_pending { START_AT } else { 0 });
    }
    idx.scanned = len;
    idx.done = true;
    *out.lock().unwrap() = Arc::new(idx);
    notify();
    ScanEnd::Done
}

fn read(buf: &TextBuf, r: std::ops::Range<u64>) -> Result<crate::file::Bytes<'_>, Pending> {
    buf.read(r).ok_or(Pending)
}

/// Runs the state machine over `from..to` starting in `st`. Returns the state at `to` and the
/// last record start in `from..=to` (`from` itself counts when `from_is_start`).
fn scan(
    buf: &TextBuf,
    d: Dialect,
    from: u64,
    to: u64,
    st: State,
    from_is_start: bool,
) -> Result<(State, Option<u64>), Pending> {
    let mut last = from_is_start.then_some(from);
    let mut st = st;
    if to <= from {
        return Ok((st, last));
    }
    let codec = buf.codec;
    let unit = codec.unit();
    let nl = nl_value(&codec);
    let hay = read(buf, from..to)?;
    let mut i = 0;
    while i + unit <= hay.len() {
        if st.step(unit_at(codec.enc, &hay, i), d, nl) == Step::RecordEnd {
            last = Some(from + (i + unit) as u64);
        }
        i += unit;
    }
    Ok((st, last))
}

/// Where a soft break near the block boundary `m` goes: on a character boundary and past a line
/// break or the CR of a CRLF there. `None` if that is the end of the document.
fn soft_pos(buf: &TextBuf, m: u64) -> Result<Option<u64>, Pending> {
    let len = buf.len();
    if m >= len {
        return Ok(None);
    }
    let codec = buf.codec;
    let unit = codec.unit();
    let hay = read(buf, m..(m + 16).min(len))?;
    let mut i = codec.snap_forward(&hay);
    for _ in 0..2 {
        if i < hay.len() && !rows::soft_ok(&codec, &hay, i) {
            i += unit;
        }
    }
    let s = m + i as u64;
    Ok((s < len).then_some(s))
}

/// Start of the row (record, or a 64 KB piece of a longer one) that holds offset `p`, which
/// [`RecordIndex::covers`].
pub fn seg_start(buf: &TextBuf, idx: &RecordIndex, p: u64) -> Result<u64, Pending> {
    let len = buf.len();
    let unit = buf.codec.unit() as u64;
    let p = p.min(len);
    let p = p - p % unit;
    if p <= idx.first {
        return Ok(idx.first.min(len));
    }
    let b = p / SEG;
    let m = b * SEG;
    // A record that starts between the block's start and `p` starts the row.
    let (pos, st, at) = idx.block_start(b);
    if let (_, Some(r)) = scan(buf, idx.dialect, pos, p, st, at)? {
        return Ok(r);
    }
    // The record holding `p` started before this block (so `m > first` and `b >= 1`).
    if idx.block(b - 1) & START_IN == 0 {
        // It started at least SEG before `m`: `m` is a soft break, unless snapping it moved it
        // past `p`, and then `p` belongs to the row before the break.
        return match soft_pos(buf, m)? {
            Some(s) if s <= p => Ok(s),
            _ => seg_start(buf, idx, m - unit),
        };
    }
    // It started inside the previous block, less than SEG before `m`: no soft break yet.
    let (pos, st, at) = idx.block_start(b - 1);
    let (_, r) = scan(buf, idx.dialect, pos, m, st, at)?;
    Ok(r.unwrap_or(pos))
}

/// The row starting at `q`, read from state `st`; `is_start` says whether a record starts there.
fn seg_from(buf: &TextBuf, d: Dialect, q: u64, st: State, is_start: bool) -> Result<Seg, Pending> {
    let len = buf.len();
    if q >= len {
        return Ok(Seg {
            start: len,
            content_end: len,
            next: len,
            line_start: is_start,
            has_nl: false,
        });
    }
    let codec = buf.codec;
    let unit = codec.unit();
    let nl = nl_value(&codec);
    let soft_mult = if is_start {
        (q + SEG).div_ceil(SEG) * SEG
    } else {
        (q / SEG + 1) * SEG
    };
    let soft = if soft_mult < len {
        soft_pos(buf, soft_mult)?
    } else {
        None
    };
    let end = soft.unwrap_or(len);
    let hay = read(buf, q..end)?;
    let mut st = st;
    let mut i = 0;
    while i + unit <= hay.len() {
        if st.step(unit_at(codec.enc, &hay, i), d, nl) == Step::RecordEnd {
            let at = q + i as u64;
            let mut content_end = at;
            if codec.nl == NlByte::Lf {
                let cr = codec.cr_unit();
                if i >= cr.len() && &hay[i - cr.len()..i] == cr {
                    content_end -= cr.len() as u64;
                }
            }
            return Ok(Seg {
                start: q,
                content_end: content_end.max(q),
                next: at + unit as u64,
                line_start: is_start,
                has_nl: true,
            });
        }
        i += unit;
    }
    Ok(Seg {
        start: q,
        content_end: end,
        next: end,
        line_start: is_start,
        has_nl: false,
    })
}

/// The row starting at `q`, a row start that [`RecordIndex::covers`]. `line_start` in the result
/// means a record starts there; `has_nl`, that the record ends with it.
pub fn seg_at(buf: &TextBuf, idx: &RecordIndex, q: u64) -> Result<Seg, Pending> {
    let q = q.max(idx.first);
    let (pos, st, at) = idx.block_start(q / SEG);
    let (st, last) = scan(buf, idx.dialect, pos, q, st, at)?;
    seg_from(buf, idx.dialect, q, st, last == Some(q))
}

fn row_of(s: Seg) -> Row {
    Row {
        start: s.start,
        end: s.content_end,
        seg: s,
    }
}

/// The row holding offset `off`.
pub fn row_at(buf: &TextBuf, idx: &RecordIndex, off: u64) -> Result<Row, Pending> {
    seg_at(buf, idx, seg_start(buf, idx, off)?).map(row_of)
}

pub fn next_row(buf: &TextBuf, idx: &RecordIndex, row: &Row) -> Result<Option<Row>, Pending> {
    if row.seg.is_last(buf.len()) {
        return Ok(None);
    }
    let s = if row.seg.has_nl {
        // The record ended: the next one starts in a known state, no need to look back.
        seg_from(buf, idx.dialect, row.seg.next, State::FieldStart, true)?
    } else {
        seg_at(buf, idx, row.seg.next)?
    };
    Ok(Some(row_of(s)))
}

pub fn prev_row(buf: &TextBuf, idx: &RecordIndex, row: &Row) -> Result<Option<Row>, Pending> {
    if row.start <= idx.first {
        return Ok(None);
    }
    let unit = buf.codec.unit() as u64;
    row_at(buf, idx, row.start - unit).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::csv::for_each_record;
    use crate::encoding::{Codec, Encoding};
    use crate::file::{OpenMode, Source};
    use crate::index;
    use std::io::Write;

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

    fn build(buf: &TextBuf, d: Dialect) -> Arc<RecordIndex> {
        let out = Mutex::new(Arc::new(RecordIndex::new(d, 0)));
        let end = index_records(buf, d, &CancelToken::new(), None, &out, &|| {});
        assert_eq!(end, ScanEnd::Done);
        let idx = out.lock().unwrap().clone();
        assert!(idx.done);
        idx
    }

    /// A small deterministic generator, so a failing case can be rerun.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    /// Records with plain, quoted, escaped, empty and multi-line fields (some longer than a
    /// block), with LF or CRLF line breaks.
    fn csv_text(seed: u64, records: usize, crlf: bool, long: bool) -> String {
        let mut r = Rng(seed);
        let eol = if crlf { "\r\n" } else { "\n" };
        let mut s = String::new();
        for _ in 0..records {
            let fields = 1 + r.below(5);
            for f in 0..fields {
                if f > 0 {
                    s.push(',');
                }
                match r.below(10) {
                    0 => {}
                    1 => s.push_str("\"a,b\"\"c\""),
                    2 => {
                        s.push('"');
                        for _ in 0..1 + r.below(4) {
                            s.push_str("line ű");
                            s.push_str(eol);
                        }
                        s.push_str("end\"");
                    }
                    3 if long && r.below(20) == 0 => {
                        s.push('"');
                        let n = 1 + r.below(3);
                        for k in 0..n * 3000 {
                            s.push_str(&format!("long {k} text, \"\" quoted"));
                            s.push_str(if k % 7 == 0 { eol } else { " " });
                        }
                        s.push('"');
                    }
                    4 => s.push_str("12\" screen"),
                    _ => s.push_str(&format!("v{}", r.below(100_000))),
                }
            }
            s.push_str(eol);
        }
        s
    }

    /// Every row from the start, following `next_row`.
    fn walk(buf: &TextBuf, idx: &RecordIndex) -> Vec<Row> {
        let mut v = vec![row_at(buf, idx, idx.first).unwrap()];
        while let Some(r) = next_row(buf, idx, v.last().unwrap()).unwrap() {
            assert!(r.start > v.last().unwrap().start, "rows go forward");
            v.push(r);
        }
        v
    }

    fn check(data: &[u8], codec: Codec, d: Dialect, seed: u64) {
        let (_f, buf) = buf_with(data, codec);
        let idx = build(&buf, d);
        let len = buf.len();
        let mut recs = Vec::new();
        for_each_record(&buf, d, &CancelToken::new(), None, |r| {
            recs.push((r.content.clone(), r.next));
            true
        });
        let rows = walk(&buf, &idx);
        // The rows that start records are the records, in order.
        let starts: Vec<&Row> = rows.iter().filter(|r| r.seg.line_start).collect();
        let starts: Vec<&Row> = starts.into_iter().filter(|r| r.start < len).collect();
        assert_eq!(starts.len(), recs.len(), "one row starts each record");
        for (row, (content, next)) in starts.iter().zip(&recs) {
            assert_eq!(row.start, content.start);
            if row.seg.has_nl {
                assert_eq!((row.end, row.seg.next), (content.end, *next));
            }
        }
        // Rows tile the document and stay bounded.
        for w in rows.windows(2) {
            assert_eq!(w[0].seg.next, w[1].start);
            assert!(w[0].seg.next - w[0].start <= 2 * SEG + 16);
        }
        // From any offset, the row around it is the walked row holding it, and going back from
        // each row gives the one before.
        let mut r = Rng(seed ^ 0x5555);
        for _ in 0..400 {
            let p = idx.first + r.below(len - idx.first + 1);
            let got = row_at(&buf, &idx, p).unwrap();
            let unit = codec.unit() as u64;
            let p = p.min(len) - p.min(len) % unit;
            let want = rows.iter().rev().find(|row| row.start <= p).unwrap();
            if want.seg.next > p || p == len {
                assert_eq!(got.start, want.start, "row at {p}");
                assert_eq!(got.seg, want.seg, "row at {p}");
            }
        }
        for w in rows.windows(2) {
            if w[1].start < len {
                let back = prev_row(&buf, &idx, &w[1]).unwrap().unwrap();
                assert_eq!(back.seg, w[0].seg);
            }
        }
    }

    #[test]
    fn rows_follow_records_across_blocks() {
        for (seed, crlf, long) in [
            (1, false, false),
            (2, true, false),
            (3, false, true),
            (4, true, true),
        ] {
            let text = csv_text(seed, if long { 6000 } else { 20_000 }, crlf, long);
            assert!(text.len() as u64 > 3 * SEG);
            check(text.as_bytes(), Codec::utf8(), Dialect::new(b','), seed);
        }
    }

    #[test]
    fn multiline_is_noticed() {
        let (_f, buf) = buf_with(b"a,b\n1,2\n", Codec::utf8());
        assert!(!build(&buf, Dialect::new(b',')).multiline);
        let (_f, buf) = buf_with(b"a,b\n\"1\n2\",3\n", Codec::utf8());
        assert!(build(&buf, Dialect::new(b',')).multiline);
    }

    #[test]
    fn utf16_and_preamble() {
        let text = format!("sep=,\n{}", csv_text(7, 3000, true, true));
        let d = Dialect {
            start: 6,
            ..Dialect::new(b',')
        };
        check(text.as_bytes(), Codec::utf8(), d, 7);
        let wide: Vec<u8> = csv_text(8, 3000, false, true)
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        check(
            &wide,
            Codec::new(Encoding::Utf16Le, NlByte::Lf, 0),
            Dialect::new(b','),
            8,
        );
    }

    #[test]
    fn an_unclosed_quote_is_cut_into_bounded_rows() {
        let mut text = csv_text(9, 2000, false, false);
        text.push_str("x,\"never closed\n");
        for k in 0..20_000 {
            text.push_str(&format!("{k},more\n"));
        }
        check(text.as_bytes(), Codec::utf8(), Dialect::new(b','), 9);
        let (_f, buf) = buf_with(text.as_bytes(), Codec::utf8());
        let idx = build(&buf, Dialect::new(b','));
        let rows = walk(&buf, &idx);
        let tail = rows.iter().filter(|r| !r.seg.line_start).count();
        assert!(tail > 1, "the open record is shown in 64 KB pieces");
    }

    #[test]
    fn a_partial_index_covers_only_what_was_read() {
        let mut idx = RecordIndex::new(Dialect::new(b','), 0);
        idx.scanned = 3 * SEG + 10;
        assert!(idx.covers(0) && idx.covers(3 * SEG - 1));
        assert!(!idx.covers(3 * SEG));
        idx.done = true;
        assert!(idx.covers(10 * SEG));
    }
}
