//! Line filter ("grep"): the lines of a document that contain a match — or, inverted, the lines
//! that do not. It streams over the document like search, so memory stays bounded and any file
//! size works. A filter can be limited to one CSV column: a line then counts only when a match
//! lies inside that field. Lines around each kept line (context, or the rest of a log entry such
//! as a stack trace) can be kept with it.

use std::io::{self, Write};
use std::ops::Range;

use crate::csv::{self, Dialect};
use crate::jobs::{CancelToken, Progress};
use crate::search::{self, Matcher, SearchEnd};
use crate::text::TextBuf;
use crate::timestamp;

/// Lines are scanned in steps of this size when their ends lie outside the search window.
const SCAN: u64 = 1 << 20;
/// A CSV line longer than this is not split into fields (it never matches a column filter).
const FIELD_LINE_MAX: u64 = 4 << 20;
/// Most lines of context before or after a kept line.
pub const CONTEXT_MAX: u32 = 999;
/// Most lines a whole entry adds after a kept line, and looks back for the entry's first line.
pub const ENTRY_MAX: u32 = 2000;
/// Bytes looked back for the first line of an entry.
const ENTRY_BACK: u64 = 256 * 1024;
/// Bytes read at once when stepping over the lines around a kept line.
const NEAR: u64 = 4096;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FilterSpec {
    /// Keep the lines without a match.
    pub invert: bool,
    /// Only matches inside this field (0-based) of a CSV line count.
    pub column: Option<(Dialect, usize)>,
    /// Lines kept before and after each line with a match (not when inverted).
    pub before: u32,
    pub after: u32,
    /// Keep whole log entries (not when inverted): a kept line with a timestamp brings the
    /// untimed lines under it (a stack trace), and a kept line without one brings the lines
    /// back to its entry's first line.
    pub entries: bool,
    /// Only the lines in this byte range (from a line start to a line start, the end left out),
    /// such as the lines between two times.
    pub range: Option<(u64, u64)>,
}

impl FilterSpec {
    /// The byte range the filter reads.
    pub fn bounds(&self, len: u64) -> (u64, u64) {
        let (a, b) = self.range.unwrap_or((0, len));
        let b = b.min(len);
        (a.min(b), b)
    }

    /// Whether lines around the kept lines are kept too.
    pub fn has_context(&self) -> bool {
        !self.invert && (self.before > 0 || self.after > 0 || self.entries)
    }
}

/// What a kept line is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    /// The line has a match (or, inverted, has none).
    Hit,
    /// A line around a hit: context or the rest of its log entry.
    Context,
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

/// One more condition on the lines a filter keeps: a match of `m` (or, `negated`, none).
#[derive(Clone, Copy)]
pub struct Term<'a> {
    pub m: &'a Matcher,
    pub negated: bool,
}

/// A line longer than this is tested against the terms by its start only.
const TERM_LINE_MAX: u64 = 16 << 20;

/// Whether the line `ls..le` meets every term (`None` on a read error).
fn terms_hold(buf: &TextBuf, terms: &[Term], ls: u64, le: u64) -> Option<bool> {
    if terms.is_empty() {
        return Some(true);
    }
    let c = &buf.codec;
    let ls = ls.max(c.bom_len as u64);
    let b = buf.read(ls..le.min(ls + TERM_LINE_MAX).max(ls))?;
    let mut line: &[u8] = &b;
    for cut in [c.nl_unit(), c.cr_unit()] {
        if line.ends_with(cut) {
            line = &line[..line.len() - cut.len()];
        }
    }
    for t in terms {
        let mut found = false;
        t.m.find_in(line, ls, 0, line.len(), line.len(), &mut None, &mut |_, _| {
            found = true;
            false
        });
        if found == t.negated {
            return Some(false);
        }
    }
    Some(true)
}

/// Streams over the document and calls `on_line(start, end, kind)` for each kept line, in order
/// and each line once (`end` includes the line break). Stops when `on_line` returns false.
pub fn filter_lines(buf: &TextBuf, m: &Matcher, spec: FilterSpec, cancel: &CancelToken, progress: Option<&Progress>, on_line: impl FnMut(u64, u64, LineKind) -> bool) -> SearchEnd {
    filter_lines_with(buf, m, spec, &[], cancel, progress, on_line)
}

/// [`filter_lines`] keeping only the lines that also meet every term.
pub fn filter_lines_with(
    buf: &TextBuf,
    m: &Matcher,
    spec: FilterSpec,
    terms: &[Term],
    cancel: &CancelToken,
    progress: Option<&Progress>,
    mut on_line: impl FnMut(u64, u64, LineKind) -> bool,
) -> SearchEnd {
    let mut spec = spec;
    // A document without timestamps has no entries to keep together.
    spec.entries = spec.entries && !spec.invert && looks_timed(buf);
    let mut failed = false;
    let mut check = |ls: u64, le: u64| match terms_hold(buf, terms, ls, le) {
        Some(ok) => ok,
        None => {
            failed = true;
            false
        }
    };
    if !spec.has_context() {
        let end = hit_lines(buf, m, spec, cancel, progress, |ls, le| !check(ls, le) || on_line(ls, le, LineKind::Hit));
        return if failed { SearchEnd::Unavailable } else { end };
    }
    let (start, stop) = spec.bounds(buf.len());
    let mut around = Around { buf, spec, done: start, end: stop, pending: None, stamped: false, failed: false };
    let end = hit_lines(buf, m, spec, cancel, progress, |ls, le| !check(ls, le) || around.hit(ls, le, &mut on_line));
    if around.failed || failed {
        return SearchEnd::Unavailable;
    }
    if end != SearchEnd::Completed {
        return end;
    }
    if !around.flush(buf.len(), &mut on_line) {
        return if around.failed { SearchEnd::Unavailable } else { SearchEnd::Stopped };
    }
    SearchEnd::Completed
}

/// Like [`filter_lines_with`], over the lines starting at `starts` (in order; a filter's earlier
/// result) instead of the whole document: each is kept when it meets every term. Narrowing a
/// filter this way reads only those lines. `progress` counts lines.
pub fn filter_given(
    buf: &TextBuf,
    starts: &[u64],
    spec: FilterSpec,
    terms: &[Term],
    cancel: &CancelToken,
    progress: Option<&Progress>,
    mut on_line: impl FnMut(u64, u64, LineKind) -> bool,
) -> SearchEnd {
    let mut spec = spec;
    spec.invert = false;
    spec.entries = spec.entries && looks_timed(buf);
    if let Some(p) = progress {
        p.set_total(starts.len() as u64);
    }
    let mut around = Around { buf, spec, done: 0, end: buf.len(), pending: None, stamped: false, failed: false };
    for (i, &ls) in starts.iter().enumerate() {
        if i % 4096 == 0 {
            if cancel.is_cancelled() {
                return SearchEnd::Cancelled;
            }
            if let Some(p) = progress {
                p.set_done(i as u64);
            }
        }
        if ls >= buf.len() {
            break;
        }
        let Some(le) = line_end_near(buf, ls) else { return SearchEnd::Unavailable };
        match terms_hold(buf, terms, ls, le) {
            Some(true) => {}
            Some(false) => continue,
            None => return SearchEnd::Unavailable,
        }
        let go = if spec.has_context() { around.hit(ls, le, &mut on_line) } else { on_line(ls, le, LineKind::Hit) };
        if !go {
            return if around.failed { SearchEnd::Unavailable } else { SearchEnd::Stopped };
        }
    }
    if spec.has_context() && !around.flush(buf.len(), &mut on_line) {
        return if around.failed { SearchEnd::Unavailable } else { SearchEnd::Stopped };
    }
    if let Some(p) = progress {
        p.set_done(starts.len() as u64);
    }
    SearchEnd::Completed
}

/// Adds the lines around the hits. A hit's after-context is passed on only when the next hit
/// (or the end) is known, so a hit is never passed on as another hit's context.
struct Around<'a> {
    buf: &'a TextBuf,
    spec: FilterSpec,
    /// End of the last line passed on.
    done: u64,
    /// End of the lines the filter reads.
    end: u64,
    /// End of the last hit, while its after-context is not passed on yet.
    pending: Option<u64>,
    /// Whether the last hit's entry starts with a timestamp (only then its untimed lines follow).
    stamped: bool,
    failed: bool,
}

impl Around<'_> {
    fn hit(&mut self, ls: u64, le: u64, on_line: &mut impl FnMut(u64, u64, LineKind) -> bool) -> bool {
        if !self.flush(ls, on_line) {
            return false;
        }
        let mut from = ls;
        for _ in 0..self.spec.before {
            if from <= self.done {
                break;
            }
            match prev_line_start(self.buf, from) {
                Some(s) => from = s.max(self.done),
                None => return self.fail(),
            }
        }
        if self.spec.entries {
            if timestamp::stamp_at(self.buf, ls).is_some() {
                self.stamped = true;
            } else {
                match self.entry_head(ls) {
                    Some(Head::Found(h)) => {
                        from = from.min(h);
                        self.stamped = true;
                    }
                    // The lines up to the hit continue the entry already passed on.
                    Some(Head::Reached) => {}
                    Some(Head::Missing) => self.stamped = false,
                    None => return self.fail(),
                }
            }
        }
        while from < ls {
            let Some(e) = line_end_near(self.buf, from) else { return self.fail() };
            let e = e.min(ls);
            if !on_line(from, e, LineKind::Context) {
                return false;
            }
            from = e;
        }
        if !on_line(ls, le, LineKind::Hit) {
            return false;
        }
        self.done = le;
        self.pending = Some(le);
        true
    }

    /// Passes on the after-context of the last hit, up to `limit` (the next hit's start).
    fn flush(&mut self, limit: u64, on_line: &mut impl FnMut(u64, u64, LineKind) -> bool) -> bool {
        let Some(mut ls) = self.pending.take() else { return true };
        let limit = limit.min(self.end);
        if self.spec.entries && self.stamped {
            for _ in 0..ENTRY_MAX {
                if ls >= limit || timestamp::stamp_at(self.buf, ls).is_some() {
                    break;
                }
                let Some(le) = line_end_near(self.buf, ls) else { return self.fail() };
                self.done = le;
                if !on_line(ls, le, LineKind::Context) {
                    return false;
                }
                ls = le;
            }
        }
        for _ in 0..self.spec.after {
            if ls >= limit {
                break;
            }
            let Some(le) = line_end_near(self.buf, ls) else { return self.fail() };
            self.done = le;
            if !on_line(ls, le, LineKind::Context) {
                return false;
            }
            ls = le;
        }
        true
    }

    /// The first line of the entry an untimed line at `ls` belongs to: the nearest line above
    /// with a timestamp, looking back no further than the lines already passed on.
    fn entry_head(&self, ls: u64) -> Option<Head> {
        let mut s = ls;
        for _ in 0..ENTRY_MAX {
            if s <= self.done {
                return Some(Head::Reached);
            }
            if ls - s > ENTRY_BACK {
                break;
            }
            s = prev_line_start(self.buf, s)?;
            if s < self.done {
                return Some(Head::Reached);
            }
            if timestamp::stamp_at(self.buf, s).is_some() {
                return Some(Head::Found(s));
            }
        }
        Some(Head::Missing)
    }

    fn fail(&mut self) -> bool {
        self.failed = true;
        false
    }
}

enum Head {
    Found(u64),
    /// Reached the lines already passed on.
    Reached,
    Missing,
}

/// Whether one of the first lines of the document has a timestamp.
fn looks_timed(buf: &TextBuf) -> bool {
    let mut ls = 0;
    for _ in 0..200 {
        if ls >= buf.len() {
            break;
        }
        if timestamp::stamp_at(buf, ls).is_some() {
            return true;
        }
        match line_end_near(buf, ls) {
            Some(e) => ls = e,
            None => break,
        }
    }
    false
}

/// Start of the line before the one starting at `ls` (> 0).
fn prev_line_start(buf: &TextBuf, ls: u64) -> Option<u64> {
    let unit = buf.codec.unit() as u64;
    let nl = ls.saturating_sub(unit);
    let lo = nl.saturating_sub(NEAR);
    let lo = lo - lo % unit;
    let b = buf.read(lo..nl)?;
    match buf.codec.rfind_nl(&b, lo) {
        Some(i) => Some(lo + i as u64 + unit),
        None if lo == 0 => Some(0),
        None => line_start_at(buf, nl),
    }
}

/// End of the line starting at `ls`, including its line break.
fn line_end_near(buf: &TextBuf, ls: u64) -> Option<u64> {
    let len = buf.len();
    let hi = (ls + NEAR).min(len);
    let b = buf.read(ls..hi)?;
    match buf.codec.find_nl(&b, ls) {
        Some(i) => Some(ls + (i + buf.codec.unit()) as u64),
        None if hi == len => Some(len),
        None => line_end_at(buf, ls),
    }
}

/// The lines with a match (or, inverted, without one), without context.
fn hit_lines(
    buf: &TextBuf,
    m: &Matcher,
    spec: FilterSpec,
    cancel: &CancelToken,
    progress: Option<&Progress>,
    mut on_line: impl FnMut(u64, u64) -> bool,
) -> SearchEnd {
    let (from, len) = spec.bounds(buf.len());
    let codec = buf.codec;
    // The line the last match fell in, and whether it was kept.
    let mut cur: Option<(u64, u64, bool, Option<Range<u64>>)> = None;
    // Inverted: start of the first line not yet classified.
    let mut cursor = from;
    let mut stopped = false;
    let mut failed = false;
    let mut fields = Vec::new();
    let end = search::search_forward_ctx(buf, m, from..len, cancel, progress, |s, e, hay, base| {
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

/// Writes the kept lines to `out` (with the document's byte order mark, if it has one). With
/// context, a "--" line separates groups of lines that are not next to each other, like grep.
/// Returns `None` when cancelled.
pub fn write_lines_to<W: Write>(buf: &TextBuf, m: &Matcher, spec: FilterSpec, terms: &[Term], out: &mut W, cancel: &CancelToken, progress: Option<&Progress>) -> io::Result<Option<Written>> {
    write_kept(buf, spec, out, |on_line| filter_lines_with(buf, m, spec, terms, cancel, progress, on_line))
}

/// [`write_lines_to`] for the lines [`filter_predicate`] keeps.
pub fn write_pred_lines_to<W: Write>(buf: &TextBuf, spec: FilterSpec, keep: &dyn Fn(&[u8]) -> bool, out: &mut W, cancel: &CancelToken, progress: Option<&Progress>) -> io::Result<Option<Written>> {
    write_kept(buf, spec, out, |on_line| filter_predicate(buf, spec, keep, cancel, progress, on_line))
}

/// Writes the lines that `run` passes on, with the byte order mark and "--" between groups.
fn write_kept<W: Write>(buf: &TextBuf, spec: FilterSpec, out: &mut W, run: impl FnOnce(&mut dyn FnMut(u64, u64, LineKind) -> bool) -> SearchEnd) -> io::Result<Option<Written>> {
    let bom = buf.codec.bom_len as u64;
    let mut w = Written::default();
    if bom > 0 {
        let b = buf.read(0..bom).ok_or_else(|| io::Error::other("Could not read the document"))?;
        out.write_all(&b)?;
        w.bytes += bom;
    }
    let mut err: Option<io::Error> = None;
    let mut prev: Option<u64> = None;
    let end = run(&mut |ls, le, _| {
        if let Some(pe) = prev.filter(|&pe| spec.has_context() && ls > pe) {
            let mut sep = buf.codec.encode("--");
            sep.extend(line_break_before(buf, pe));
            if let Err(e) = out.write_all(&sep) {
                err = Some(e);
                return false;
            }
            w.bytes += sep.len() as u64;
        }
        prev = Some(le);
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

/// Like [`filter_lines`], keeping the lines for which `keep` is true (given a line's bytes without
/// its line break) — say, JSON Lines that meet a field query — instead of the lines with a match.
pub fn filter_predicate(
    buf: &TextBuf,
    spec: FilterSpec,
    keep: &dyn Fn(&[u8]) -> bool,
    cancel: &CancelToken,
    progress: Option<&Progress>,
    mut on_line: impl FnMut(u64, u64, LineKind) -> bool,
) -> SearchEnd {
    let mut spec = spec;
    spec.entries = spec.entries && !spec.invert && looks_timed(buf);
    let len = buf.len();
    let (start, stop) = spec.bounds(len);
    if let Some(p) = progress {
        p.set_total(len);
    }
    let unit = buf.codec.unit() as u64;
    let ctx = spec.has_context();
    let mut around = Around { buf, spec, done: start, end: stop, pending: None, stamped: false, failed: false };
    let mut lines = crate::lines::LineReader::new(buf);
    let mut n = 0u32;
    loop {
        n += 1;
        if n.is_multiple_of(4096) {
            if cancel.is_cancelled() {
                return SearchEnd::Cancelled;
            }
            if let Some(p) = progress {
                p.set_done(lines.taken());
            }
        }
        let (off, hit, le) = match lines.next() {
            Ok(Some((off, line))) => (off, keep(line) != spec.invert, (off + line.len() as u64 + unit).min(len)),
            Ok(None) => break,
            Err(_) => return SearchEnd::Unavailable,
        };
        if off < start {
            continue;
        }
        if off >= stop {
            break;
        }
        if !hit {
            continue;
        }
        let go = if ctx { around.hit(off, le, &mut on_line) } else { on_line(off, le, LineKind::Hit) };
        if !go {
            return if around.failed { SearchEnd::Unavailable } else { SearchEnd::Stopped };
        }
    }
    if ctx && !around.flush(len, &mut on_line) {
        return if around.failed { SearchEnd::Unavailable } else { SearchEnd::Stopped };
    }
    SearchEnd::Completed
}

/// The line break that ends at `end` (CR LF, or the document's single line break).
pub fn line_break_before(buf: &TextBuf, end: u64) -> Vec<u8> {
    let c = &buf.codec;
    let (nl, cr) = (c.nl_unit(), c.cr_unit());
    let two = (nl.len() + cr.len()) as u64;
    if c.nl == crate::encoding::NlByte::Lf && end >= two {
        if let Some(b) = buf.read(end - two..end) {
            if b.starts_with(cr) && b.ends_with(nl) {
                return b.to_vec();
            }
        }
    }
    nl.to_vec()
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
        let end = filter_lines(buf, &m, spec, &CancelToken::new(), None, |s, e, _| {
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
        let col = |c, invert| FilterSpec { invert, column: Some((Dialect::new(b','), c)), ..Default::default() };
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
        let w = write_lines_to(&t, &m, FilterSpec::default(), &[], &mut out, &CancelToken::new(), None).unwrap().unwrap();
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
        filter_lines(&t, &m, FilterSpec::default(), &CancelToken::new(), None, |s, e, _| {
            got.push((s, e));
            true
        });
        assert_eq!(got, vec![(6, data.len() as u64 - 5)]);
    }

    /// Kept lines as "H:text" / "C:text" (without line breaks).
    fn kinds(buf: &TextBuf, pat: &str, spec: FilterSpec) -> Vec<String> {
        let q = Query { pattern: pat.into(), regex: true, case_sensitive: true, ..Default::default() };
        let m = Matcher::new(&q, buf.codec).unwrap();
        let mut v = Vec::new();
        let mut prev = 0;
        let end = filter_lines(buf, &m, spec, &CancelToken::new(), None, |s, e, k| {
            assert!(s >= prev, "lines out of order or repeated");
            prev = e;
            let t = buf.codec.decode_lossy(&buf.read(s..e).unwrap());
            v.push(format!("{}:{}", if k == LineKind::Hit { 'H' } else { 'C' }, t.trim_end_matches(['\r', '\n'])));
            true
        });
        assert_eq!(end, SearchEnd::Completed);
        v
    }

    fn ctx(before: u32, after: u32) -> FilterSpec {
        FilterSpec { before, after, ..Default::default() }
    }

    #[test]
    fn context_lines() {
        let (_f, t) = buf_with(b"1\n2\n3 X\n4\n5\n6\n7 X\n8\n9 X\n10\n", Codec::utf8());
        assert_eq!(kinds(&t, "X", ctx(1, 1)), ["C:2", "H:3 X", "C:4", "C:6", "H:7 X", "C:8", "H:9 X", "C:10"]);
        // A hit inside another hit's after-context stays a hit.
        assert_eq!(kinds(&t, "X", ctx(0, 3)), ["H:3 X", "C:4", "C:5", "C:6", "H:7 X", "C:8", "H:9 X", "C:10"]);
        assert_eq!(kinds(&t, "X", ctx(5, 0)), ["C:1", "C:2", "H:3 X", "C:4", "C:5", "C:6", "H:7 X", "C:8", "H:9 X"]);
        assert_eq!(kinds(&t, "^1$", ctx(2, 1)), ["H:1", "C:2"]);
        assert_eq!(kinds(&t, "10", ctx(1, 2)), ["C:9 X", "H:10"]);
        // Inverted filters keep no context.
        let inv = FilterSpec { invert: true, before: 1, after: 1, ..Default::default() };
        assert_eq!(kinds(&t, "[0-9]", inv), Vec::<String>::new());
    }

    #[test]
    fn separators_crlf_and_last_line() {
        let (_f, t) = buf_with(b"a\r\nb X\r\nc\r\nd", Codec::utf8());
        assert_eq!(kinds(&t, "X", ctx(1, 5)), ["C:a", "H:b X", "C:c", "C:d"]);
        let q = Query { pattern: "[ad]".into(), regex: true, case_sensitive: true, ..Default::default() };
        let m = Matcher::new(&q, t.codec).unwrap();
        let mut out = Vec::new();
        write_lines_to(&t, &m, ctx(0, 0), &[], &mut out, &CancelToken::new(), None).unwrap();
        assert_eq!(out, b"a\r\nd");
        out.clear();
        let w = write_lines_to(&t, &m, FilterSpec { entries: true, ..Default::default() }, &[], &mut out, &CancelToken::new(), None).unwrap().unwrap();
        assert_eq!(out, b"a\r\n--\r\nd");
        assert_eq!(w.lines, 2);
        out.clear();
        write_lines_to(&t, &m, ctx(1, 0), &[], &mut out, &CancelToken::new(), None).unwrap();
        assert_eq!(out, b"a\r\n--\r\nc\r\nd");
    }

    const TRACE: &[u8] = b"2024-05-01 10:00:00 INFO start\n2024-05-01 10:00:01 ERROR boom\njava.lang.IllegalStateException: x\n\tat a.B.c(B.java:1)\n\tat a.B.d(B.java:2)\n2024-05-01 10:00:02 INFO next\n2024-05-01 10:00:03 WARN slow\n";

    #[test]
    fn whole_entries() {
        let (_f, t) = buf_with(TRACE, Codec::utf8());
        let entries = FilterSpec { entries: true, ..Default::default() };
        let got = kinds(&t, "ERROR", entries);
        assert_eq!(got.len(), 4, "{got:?}");
        assert!(got[0].starts_with("H:") && got[1].starts_with("C:java") && got[3].starts_with("C:\tat a.B.d"), "{got:?}");
        // A hit inside a stack trace brings the entry's first line and the rest of the trace.
        let got = kinds(&t, "B.java:1", entries);
        assert_eq!(got.len(), 4, "{got:?}");
        assert!(got[0].starts_with("C:2024-05-01 10:00:01 ERROR") && got[2].starts_with("H:\tat a.B.c"), "{got:?}");
        // Two hits in one entry: each line once.
        let got = kinds(&t, "ERROR|B.java:2", entries);
        assert_eq!(got.len(), 4, "{got:?}");
        assert!(got[3].starts_with("H:\tat a.B.d"), "{got:?}");
        // The lines of an untimed document do not pull the rest of the file.
        let (_g, u) = buf_with(b"a\nb X\nc\nd\n", Codec::utf8());
        assert_eq!(kinds(&u, "X", entries), ["H:b X"]);
        // Entries and after-context add up.
        let got = kinds(&t, "ERROR", FilterSpec { entries: true, after: 1, ..Default::default() });
        assert_eq!(got.len(), 5, "{got:?}");
        assert!(got[4].starts_with("C:2024-05-01 10:00:02 INFO next"), "{got:?}");
    }

    #[test]
    fn context_in_utf16() {
        let codec = Codec::new(Encoding::Utf16Le, NlByte::Lf, 2);
        let mut data = vec![0xFF, 0xFE];
        data.extend(codec.encode("one\ntwo X\nthree\nfour X\n"));
        let (_f, t) = buf_with(&data, codec);
        assert_eq!(kinds(&t, "X", ctx(1, 0)), ["C:\u{feff}one", "H:two X", "C:three", "H:four X"]);
        let q = Query { pattern: "one|four".into(), regex: true, case_sensitive: true, ..Default::default() };
        let m = Matcher::new(&q, codec).unwrap();
        let mut out = Vec::new();
        write_lines_to(&t, &m, FilterSpec { entries: true, ..Default::default() }, &[], &mut out, &CancelToken::new(), None).unwrap();
        let mut want = vec![0xFF, 0xFE];
        want.extend(codec.encode("one\n--\nfour X\n"));
        assert_eq!(out, want);
    }

    fn matcher(pat: &str, codec: Codec) -> Matcher {
        Matcher::new(&Query { pattern: pat.into(), regex: true, case_sensitive: true, ..Default::default() }, codec).unwrap()
    }

    #[test]
    fn terms_narrow_the_lines() {
        let (_f, t) = buf_with(b"ERROR db timeout user=1\nERROR db ok user=2\nINFO db timeout user=1\nERROR net timeout user=1 healthcheck\n", Codec::utf8());
        let (m, timeout, health) = (matcher("ERROR", t.codec), matcher("timeout", t.codec), matcher("healthcheck$", t.codec));
        let terms = [Term { m: &timeout, negated: false }, Term { m: &health, negated: true }];
        let mut got = Vec::new();
        let end = filter_lines_with(&t, &m, FilterSpec::default(), &terms, &CancelToken::new(), None, |s, e, _| {
            got.push(t.codec.decode_lossy(&t.read(s..e).unwrap()));
            true
        });
        assert_eq!(end, SearchEnd::Completed);
        // "healthcheck$" matches before the line break.
        assert_eq!(got, ["ERROR db timeout user=1\n"]);
        // The same over the lines an earlier filter kept.
        let mut first = Vec::new();
        filter_lines(&t, &m, FilterSpec::default(), &CancelToken::new(), None, |s, _, _| {
            first.push(s);
            true
        });
        assert_eq!(first.len(), 3);
        let mut within = Vec::new();
        let end = filter_given(&t, &first, FilterSpec::default(), &terms, &CancelToken::new(), None, |s, e, k| {
            within.push((t.codec.decode_lossy(&t.read(s..e).unwrap()), k));
            true
        });
        assert_eq!(end, SearchEnd::Completed);
        assert_eq!(within, [("ERROR db timeout user=1\n".to_string(), LineKind::Hit)]);
        // With context, the lines around come from the whole document.
        let mut ctx = Vec::new();
        filter_given(&t, &first, FilterSpec { after: 1, ..Default::default() }, &terms, &CancelToken::new(), None, |s, _, k| {
            ctx.push((s, k));
            true
        });
        assert_eq!(ctx, [(0, LineKind::Hit), (24, LineKind::Context)]);
        // An inverted base with a term: lines without ERROR that have "timeout".
        let mut inv = Vec::new();
        filter_lines_with(&t, &m, FilterSpec { invert: true, ..Default::default() }, &terms[..1], &CancelToken::new(), None, |s, _, _| {
            inv.push(s);
            true
        });
        assert_eq!(inv, [43]);
    }

    #[test]
    fn lines_by_a_predicate() {
        let (_f, t) = buf_with(b"{\"a\":1}\nplain\n{\"a\":2}\n", Codec::utf8());
        let keep = |l: &[u8]| l.starts_with(b"{");
        let mut got = Vec::new();
        let end = filter_predicate(&t, FilterSpec::default(), &keep, &CancelToken::new(), None, |s, e, _| {
            got.push(t.codec.decode_lossy(&t.read(s..e).unwrap()));
            true
        });
        assert_eq!(end, SearchEnd::Completed);
        assert_eq!(got, ["{\"a\":1}\n", "{\"a\":2}\n"]);
        let mut out = Vec::new();
        write_pred_lines_to(&t, FilterSpec { invert: true, ..Default::default() }, &keep, &mut out, &CancelToken::new(), None).unwrap();
        assert_eq!(out, b"plain\n");
        out.clear();
        write_pred_lines_to(&t, FilterSpec { after: 1, ..Default::default() }, &keep, &mut out, &CancelToken::new(), None).unwrap();
        assert_eq!(out, b"{\"a\":1}\nplain\n{\"a\":2}\n");
    }

    #[test]
    fn only_lines_in_a_range() {
        let (_f, t) = buf_with(b"a X\nb\nc X\nd\ne X\n", Codec::utf8());
        let range = Some((4, 12)); // "b\n" up to (not including) "e X\n"
        assert_eq!(lines(&t, "X", false, FilterSpec { range, ..Default::default() }), ["c X\n"]);
        assert_eq!(lines(&t, "X", false, FilterSpec { range, invert: true, ..Default::default() }), ["b\n", "d\n"]);
        let around = kinds(&t, "X", FilterSpec { range, before: 5, after: 5, ..Default::default() });
        assert_eq!(around, ["C:b", "H:c X", "C:d"]);
        assert_eq!(lines(&t, "X", false, FilterSpec { range: Some((12, 99)), ..Default::default() }), ["e X\n"]);
    }

    #[test]
    fn line_bounds() {
        let (_f, t) = buf_with(b"ab\ncd\nef", Codec::utf8());
        assert_eq!((line_start_at(&t, 4), line_end_at(&t, 4)), (Some(3), Some(6)));
        assert_eq!((line_start_at(&t, 2), line_end_at(&t, 2)), (Some(0), Some(3)));
        assert_eq!((line_start_at(&t, 7), line_end_at(&t, 7)), (Some(6), Some(8)));
    }
}
