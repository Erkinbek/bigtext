//! Search over byte windows of a [`TextBuf`].
//!
//! * Literal, case-sensitive queries use `memchr::memmem` (SIMD) on the raw bytes, after
//!   transcoding the pattern into the file's encoding (UTF-16 hits must be 2-byte aligned).
//! * Everything else uses `regex-automata`'s meta regex (guaranteed linear time). For UTF-8 it
//!   runs on raw bytes; for other encodings each window is decoded to UTF-8 and match offsets are
//!   mapped back to source bytes exactly.
//! * The document is processed in windows of [`WINDOW`] bytes with an overlap, so search never
//!   needs the line index and memory stays bounded. With no edits the windows are borrowed
//!   straight from the mmap; with edits they are assembled across piece boundaries.
//! * Regex matches longer than the overlap are re-searched with a growing window, capped at
//!   [`MAX_MATCH`] (longer matches are reported truncated).

use std::ops::Range;

use regex_automata::meta::Regex;
use regex_automata::util::captures::Captures;
use regex_automata::util::syntax;
use regex_automata::Input;

use crate::encoding::{Codec, Encoding, NlByte};
use crate::jobs::{CancelToken, Progress};
use crate::text::TextBuf;

pub const WINDOW: u64 = 16 * 1024 * 1024;
/// Regex window overlap: matches up to this length never straddle a window edge unseen.
pub const REGEX_OVERLAP: u64 = 1024 * 1024;
pub const MAX_MATCH: u64 = 256 * 1024 * 1024;
/// Bytes of look-behind/look-ahead context given to the regex engine around a window.
const CTX: u64 = 8;

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Query {
    pub pattern: String,
    pub regex: bool,
    pub case_sensitive: bool,
    pub whole_word: bool,
    /// Dot matches newline ("multiline" mode).
    pub dot_all: bool,
}

enum Engine {
    Literal { finder: memchr::memmem::Finder<'static>, len: usize },
    Regex(Regex),
}

pub struct Matcher {
    engine: Engine,
    codec: Codec,
    /// Regex runs on decoded UTF-8 (non-UTF-8 encodings).
    decoded: bool,
    pub query: Query,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchEnd {
    Completed,
    Stopped,
    Cancelled,
    Unavailable,
}

/// Builds the regex pattern string for a query (shared with replace).
pub fn regex_pattern(q: &Query) -> String {
    let pat = if q.regex { q.pattern.clone() } else { regex_syntax::escape(&q.pattern) };
    if q.whole_word {
        if q.pattern.is_ascii() {
            format!(r"(?-u:\b)(?:{pat})(?-u:\b)")
        } else {
            format!(r"\b(?:{pat})\b")
        }
    } else {
        pat
    }
}

pub fn build_regex(q: &Query, codec: &Codec) -> Result<Regex, String> {
    let mut sc = syntax::Config::new()
        .case_insensitive(!q.case_sensitive)
        .multi_line(true)
        .dot_matches_new_line(q.dot_all)
        .utf8(false);
    match codec.nl {
        NlByte::Lf => sc = sc.crlf(true),
        NlByte::Cr => sc = sc.line_terminator(b'\r'),
    }
    Regex::builder()
        .syntax(sc)
        .configure(Regex::config().utf8_empty(true))
        .build(&regex_pattern(q))
        // The syntax error carries the pattern with a caret under the problem.
        .map_err(|e| e.syntax_error().map(|s| s.to_string()).unwrap_or_else(|| e.to_string()))
}

impl Matcher {
    pub fn new(q: &Query, codec: Codec) -> Result<Matcher, String> {
        if q.pattern.is_empty() {
            return Err("Empty pattern".into());
        }
        let literal_ok = !q.regex && q.case_sensitive && !q.whole_word && codec.can_encode(&q.pattern);
        if literal_ok {
            let needle = codec.encode(&q.pattern);
            let len = needle.len();
            let finder = memchr::memmem::Finder::new(&needle).into_owned();
            return Ok(Matcher { engine: Engine::Literal { finder, len }, codec, decoded: false, query: q.clone() });
        }
        let re = build_regex(q, &codec)?;
        Ok(Matcher { engine: Engine::Regex(re), codec, decoded: codec.enc != Encoding::Utf8, query: q.clone() })
    }

    pub fn is_literal(&self) -> bool {
        matches!(self.engine, Engine::Literal { .. })
    }

    pub fn overlap(&self) -> u64 {
        match &self.engine {
            Engine::Literal { len, .. } => (*len as u64).saturating_sub(1),
            Engine::Regex(_) => REGEX_OVERLAP,
        }
    }

    pub fn regex(&self) -> Option<&Regex> {
        match &self.engine {
            Engine::Regex(r) => Some(r),
            _ => None,
        }
    }

    pub fn codec(&self) -> Codec {
        self.codec
    }

    /// Finds matches in `hay` (first byte at absolute offset `base`) whose start lies in
    /// `[from, accept_end)` and which end at or before `span_end` (indices into `hay`).
    /// `last_end` suppresses an empty match right after the previous match. Returns false if
    /// `f` asked to stop.
    pub fn find_in(
        &self,
        hay: &[u8],
        base: u64,
        from: usize,
        accept_end: usize,
        span_end: usize,
        last_end: &mut Option<u64>,
        f: &mut dyn FnMut(u64, u64) -> bool,
    ) -> bool {
        let span_end = span_end.min(hay.len());
        match &self.engine {
            Engine::Literal { finder, len } => {
                let utf16 = self.codec.enc.is_utf16();
                let mut pos = from;
                while pos < accept_end && pos < span_end {
                    let Some(i) = finder.find(&hay[pos..span_end]) else { break };
                    let s = pos + i;
                    if s >= accept_end {
                        break;
                    }
                    if utf16 && (base + s as u64) % 2 == 1 {
                        pos = s + 1;
                        continue;
                    }
                    let (sa, ea) = (base + s as u64, base + (s + len) as u64);
                    *last_end = Some(ea);
                    if !f(sa, ea) {
                        return false;
                    }
                    pos = s + len;
                }
                true
            }
            Engine::Regex(re) => {
                if self.decoded {
                    self.find_decoded(re, hay, base, from, accept_end, span_end, last_end, f)
                } else {
                    let mut pos = from;
                    while pos <= span_end && pos < accept_end {
                        let input = Input::new(hay).range(pos..span_end);
                        let Some(m) = re.search(&input) else { break };
                        let (s, e) = (m.start(), m.end());
                        if s >= accept_end {
                            break;
                        }
                        if s == e && *last_end == Some(base + e as u64) {
                            pos = s + utf8_step(hay, s);
                            continue;
                        }
                        *last_end = Some(base + e as u64);
                        if !f(base + s as u64, base + e as u64) {
                            return false;
                        }
                        pos = e;
                    }
                    true
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn find_decoded(
        &self,
        re: &Regex,
        hay: &[u8],
        base: u64,
        from: usize,
        accept_end: usize,
        span_end: usize,
        last_end: &mut Option<u64>,
        f: &mut dyn FnMut(u64, u64) -> bool,
    ) -> bool {
        let codec = self.codec;
        let accept_end = accept_end.min(span_end + 1);
        // Decode [0, from), [from, accept), [accept, span_end) and remember the UTF-8 boundaries.
        let unit = codec.unit();
        let align = |x: usize| x - x % unit;
        let from = align(from);
        let acc = align(accept_end.min(span_end));
        let mut text = String::with_capacity(span_end * 2);
        codec.decode_into(&hay[..from], &mut text);
        let u_from = text.len();
        codec.decode_into(&hay[from..acc], &mut text);
        let u_acc = if accept_end > span_end { usize::MAX } else { text.len() };
        codec.decode_into(&hay[acc..span_end], &mut text);
        let u_end = text.len();
        // Walking cursor for UTF-8 offset -> source offset.
        let mut cur_u = 0usize;
        let mut cur_s = 0usize;
        let mut to_src = |u: usize| -> usize {
            if u < cur_u {
                // Never happens for monotonic queries; recompute from scratch.
                cur_u = 0;
                cur_s = 0;
            }
            for c in text[cur_u..u].chars() {
                cur_s += codec.src_width(c);
            }
            cur_u = u;
            cur_s.min(span_end)
        };
        let mut pos = u_from;
        while pos <= u_end && pos < u_acc {
            let input = Input::new(text.as_bytes()).range(pos..u_end);
            let Some(m) = re.search(&input) else { break };
            let (s, e) = (m.start(), m.end());
            if s >= u_acc {
                break;
            }
            let ss = base + to_src(s) as u64;
            let es = base + to_src(e) as u64;
            if s == e && *last_end == Some(es) {
                pos = s + utf8_step(text.as_bytes(), s);
                continue;
            }
            *last_end = Some(es);
            if !f(ss, es) {
                return false;
            }
            pos = e;
        }
        true
    }
}

fn utf8_step(hay: &[u8], i: usize) -> usize {
    if i >= hay.len() {
        return 1;
    }
    crate::encoding::utf8_char(&hay[i..]).1.max(1)
}

fn align_down(x: u64, unit: u64) -> u64 {
    x - x % unit
}

/// Streams forward over `range`, calling `on_match` for every match in order until it returns
/// false. Works on any snapshot (blocking reads).
pub fn search_forward(
    buf: &TextBuf,
    m: &Matcher,
    range: Range<u64>,
    cancel: &CancelToken,
    progress: Option<&Progress>,
    mut on_match: impl FnMut(u64, u64) -> bool,
) -> SearchEnd {
    search_forward_ctx(buf, m, range, cancel, progress, |s, e, _, _| on_match(s, e))
}

/// Like [`search_forward`], but also passes the current window (`hay`, starting at absolute
/// offset `base`) so callers can copy surrounding bytes without re-reading them. For matches
/// found by extending past the window, `hay` is empty.
pub fn search_forward_ctx(
    buf: &TextBuf,
    m: &Matcher,
    range: Range<u64>,
    cancel: &CancelToken,
    progress: Option<&Progress>,
    mut on_match: impl FnMut(u64, u64, &[u8], u64) -> bool,
) -> SearchEnd {
    let len = buf.len();
    let unit = buf.codec.unit() as u64;
    let end = range.end.min(len);
    let start = align_down(range.start.min(end), unit);
    if let Some(p) = progress {
        p.set_total(end - start);
    }
    let mut pos = start;
    let mut last_end: Option<u64> = None;
    let mut window = Vec::new();
    loop {
        if cancel.is_cancelled() {
            return SearchEnd::Cancelled;
        }
        let win_end = align_down((pos + WINDOW).min(end), unit).max(pos.min(end));
        let span_end = (win_end + m.overlap()).min(end);
        let read_start = align_down(pos.saturating_sub(CTX), unit);
        let read_end = (span_end + CTX).min(len);
        let Some(n) = buf.read_to(read_start..read_end, &mut window) else { return SearchEnd::Unavailable };
        let hay = &window[..n];
        let final_window = win_end >= end;
        let rel = |x: u64| (x - read_start) as usize;
        let accept = if final_window { rel(win_end) + 1 } else { rel(win_end) };
        let mut truncated: Option<u64> = None;
        let mut stopped = false;
        let mut max_end = win_end;
        let literal = m.is_literal();
        m.find_in(hay, read_start, rel(pos), accept, rel(span_end), &mut last_end, &mut |s, e| {
            if !literal && e == span_end && span_end < end {
                truncated = Some(s);
                return false;
            }
            if !on_match(s, e, hay, read_start) {
                stopped = true;
                return false;
            }
            max_end = max_end.max(e);
            true
        });
        if stopped {
            return SearchEnd::Stopped;
        }
        if let Some(s) = truncated {
            // Re-search from the match start with a growing window.
            match extend_match(buf, m, s, end, cancel) {
                Some((s2, e2)) => {
                    last_end = Some(e2);
                    if !on_match(s2, e2, &[], 0) {
                        return SearchEnd::Stopped;
                    }
                    pos = if e2 > s2 { e2 } else { buf.next_char(s2) };
                }
                None => pos = buf.next_char(s),
            }
            if let Some(p) = progress {
                p.set_done(pos.min(end) - start);
            }
            if pos > end {
                break;
            }
            continue;
        }
        if let Some(p) = progress {
            p.set_done(win_end - start);
        }
        if final_window {
            break;
        }
        pos = max_end;
    }
    SearchEnd::Completed
}

fn extend_match(buf: &TextBuf, m: &Matcher, s: u64, end: u64, cancel: &CancelToken) -> Option<(u64, u64)> {
    let len = buf.len();
    let unit = buf.codec.unit() as u64;
    let mut lim = (s + 4 * REGEX_OVERLAP).min(end);
    loop {
        if cancel.is_cancelled() {
            return None;
        }
        let rs = align_down(s.saturating_sub(CTX), unit);
        let hay = buf.read(rs..(lim + CTX).min(len))?;
        let mut found = None;
        let mut le = None;
        m.find_in(&hay, rs, (s - rs) as usize, (s - rs) as usize + 1, (lim - rs) as usize, &mut le, &mut |a, b| {
            found = Some((a, b));
            false
        });
        match found {
            Some((a, b)) if b < lim || lim >= end || lim - s >= MAX_MATCH => return Some((a, b)),
            Some(_) => lim = (s + (lim - s) * 4).min(end).min(s + MAX_MATCH),
            None => return None,
        }
    }
}

/// Last match starting before `before` (and at or after `floor`), searching backwards in windows.
pub fn search_backward(buf: &TextBuf, m: &Matcher, floor: u64, before: u64, cancel: &CancelToken) -> Option<(u64, u64)> {
    let unit = buf.codec.unit() as u64;
    let mut hi = before.min(buf.len());
    while hi > floor {
        if cancel.is_cancelled() {
            return None;
        }
        let lo = align_down(hi.saturating_sub(WINDOW).max(floor), unit);
        let mut best = None;
        let end = (hi + m.overlap()).min(buf.len());
        let r = search_forward(buf, m, lo..end, cancel, None, |s, e| {
            if s < hi {
                best = Some((s, e));
                true
            } else {
                false
            }
        });
        if r == SearchEnd::Cancelled {
            return None;
        }
        if best.is_some() {
            return best;
        }
        if lo == floor {
            break;
        }
        hi = lo + m.overlap().min(WINDOW / 2);
        if hi >= before {
            hi = lo;
        }
    }
    None
}

/// Matches overlapping a small range (viewport highlighting); bounded work, may miss matches
/// longer than a few KB.
pub fn matches_in_view(buf: &TextBuf, m: &Matcher, range: Range<u64>, cap: usize) -> Vec<(u64, u64)> {
    let len = buf.len();
    let unit = buf.codec.unit() as u64;
    let start = align_down(range.start.saturating_sub(4096), unit);
    let end = (range.end + 4096).min(len);
    let rs = align_down(start.saturating_sub(CTX), unit);
    let Some(hay) = buf.read(rs..(end + CTX).min(len)) else { return Vec::new() };
    let mut out = Vec::new();
    let mut le = None;
    let accept = if end == len { (end - rs) as usize + 1 } else { (end - rs) as usize };
    m.find_in(&hay, rs, (start - rs) as usize, accept, (end - rs) as usize, &mut le, &mut |s, e| {
        if e >= range.start && s <= range.end {
            out.push((s, e));
        }
        out.len() < cap
    });
    out
}

/// Captures for the match starting exactly at `start` (regex queries; used by replace).
pub fn captures_at(buf: &TextBuf, m: &Matcher, start: u64, end: u64) -> Option<(Vec<u8>, Captures, usize)> {
    let re = m.regex()?;
    let len = buf.len();
    let unit = buf.codec.unit() as u64;
    let rs = align_down(start.saturating_sub(CTX), unit);
    let re_end = (end + CTX).min(len);
    let raw = buf.read(rs..re_end)?;
    let (hay, s, e) = if m.decoded {
        let mut t = String::new();
        m.codec.decode_into(&raw[..(start - rs) as usize], &mut t);
        let s = t.len();
        m.codec.decode_into(&raw[(start - rs) as usize..(end - rs) as usize], &mut t);
        let e = t.len();
        m.codec.decode_into(&raw[(end - rs) as usize..], &mut t);
        (t.into_bytes(), s, e)
    } else {
        (raw.into_owned(), (start - rs) as usize, (end - rs) as usize)
    };
    let mut caps = re.create_captures();
    re.search_captures(&Input::new(&hay).range(s..e).anchored(regex_automata::Anchored::Yes), &mut caps);
    if !caps.is_match() {
        return None;
    }
    Some((hay, caps, s))
}

#[cfg(test)]
mod tests {
    use super::*;
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

    fn q(p: &str) -> Query {
        Query { pattern: p.into(), case_sensitive: true, ..Default::default() }
    }

    fn all(buf: &TextBuf, m: &Matcher) -> Vec<(u64, u64)> {
        let mut v = Vec::new();
        let r = search_forward(buf, m, 0..buf.len(), &CancelToken::new(), None, |s, e| {
            v.push((s, e));
            true
        });
        assert_eq!(r, SearchEnd::Completed);
        v
    }

    #[test]
    fn literal_basic() {
        let (_f, t) = buf_with(b"abc foo abc foofoo", Codec::utf8());
        let m = Matcher::new(&q("foo"), Codec::utf8()).unwrap();
        assert!(m.is_literal());
        assert_eq!(all(&t, &m), vec![(4, 7), (12, 15), (15, 18)]);
    }

    #[test]
    fn literal_across_window_boundary() {
        let mut data = vec![b'x'; WINDOW as usize - 2];
        data.extend_from_slice(b"NEEDLE");
        data.extend(vec![b'y'; 100]);
        data.extend_from_slice(b"NEEDLE");
        let (_f, t) = buf_with(&data, Codec::utf8());
        let m = Matcher::new(&q("NEEDLE"), Codec::utf8()).unwrap();
        let w = WINDOW - 2;
        assert_eq!(all(&t, &m), vec![(w, w + 6), (w + 106, w + 112)]);
    }

    #[test]
    fn regex_and_case_folding() {
        let (_f, t) = buf_with("Straße STRASSE straße ПРИВЕТ привет".as_bytes(), Codec::utf8());
        let mut query = q("straße");
        query.case_sensitive = false;
        let m = Matcher::new(&query, Codec::utf8()).unwrap();
        assert_eq!(all(&t, &m).len(), 2);
        let mut query = q("привет");
        query.case_sensitive = false;
        let m = Matcher::new(&query, Codec::utf8()).unwrap();
        assert_eq!(all(&t, &m).len(), 2);
        let mut rq = q(r"\d+");
        rq.regex = true;
        let (_g, t2) = buf_with(b"a1 b22 c333", Codec::utf8());
        let m = Matcher::new(&rq, Codec::utf8()).unwrap();
        assert_eq!(all(&t2, &m), vec![(1, 2), (4, 6), (8, 11)]);
    }

    #[test]
    fn whole_word() {
        let (_f, t) = buf_with(b"cat concat cat_x cat.", Codec::utf8());
        let mut query = q("cat");
        query.whole_word = true;
        let m = Matcher::new(&query, Codec::utf8()).unwrap();
        assert_eq!(all(&t, &m), vec![(0, 3), (17, 20)]);
    }

    #[test]
    fn empty_matches_and_anchors() {
        let (_f, t) = buf_with(b"ab\ncd\n", Codec::utf8());
        let mut rq = q("^");
        rq.regex = true;
        let m = Matcher::new(&rq, Codec::utf8()).unwrap();
        assert_eq!(all(&t, &m), vec![(0, 0), (3, 3), (6, 6)]);
        let mut rq = q("x*");
        rq.regex = true;
        let m = Matcher::new(&rq, Codec::utf8()).unwrap();
        assert_eq!(all(&t, &m).len(), 7);
        let mut rq = q("$");
        rq.regex = true;
        let (_g, crlf) = buf_with(b"ab\r\ncd", Codec::utf8());
        let m = Matcher::new(&rq, Codec::utf8()).unwrap();
        assert_eq!(all(&crlf, &m), vec![(2, 2), (6, 6)]);
    }

    #[test]
    fn regex_match_spanning_windows_and_long_matches() {
        // A match shorter than the overlap that crosses a window edge is found exactly.
        let mut data = vec![b'x'; WINDOW as usize - 1000];
        data.extend(vec![b'a'; 500_000]);
        data.push(b'!');
        let (_f, t) = buf_with(&data, Codec::utf8());
        let mut rq = q("a+!");
        rq.regex = true;
        let m = Matcher::new(&rq, Codec::utf8()).unwrap();
        assert_eq!(all(&t, &m), vec![(WINDOW - 1000, data.len() as u64)]);
        // A match longer than the overlap is extended past the window instead of truncated.
        let data = vec![b'a'; WINDOW as usize + 3 * REGEX_OVERLAP as usize];
        let (_g, t) = buf_with(&data, Codec::utf8());
        let mut rq = q("a+");
        rq.regex = true;
        let m = Matcher::new(&rq, Codec::utf8()).unwrap();
        assert_eq!(all(&t, &m), vec![(0, data.len() as u64)]);
        // Whole-file match with dot-all.
        let mut rq = q("(?s).*");
        rq.regex = true;
        let m = Matcher::new(&rq, Codec::utf8()).unwrap();
        assert_eq!(all(&t, &m)[0], (0, data.len() as u64));
    }

    #[test]
    fn utf16_literal_alignment_and_regex() {
        let text = "a\u{0A61}a and ab"; // U+0A61 encodes as 61 0A: misaligned "a" hits must be rejected
        let mut data = vec![0xFF, 0xFE];
        data.extend(text.encode_utf16().flat_map(|u| u.to_le_bytes()));
        let codec = Codec::new(Encoding::Utf16Le, NlByte::Lf, 2);
        let (_f, t) = buf_with(&data, codec);
        let m = Matcher::new(&q("a"), codec).unwrap();
        let hits = all(&t, &m);
        assert!(hits.iter().all(|(s, _)| s % 2 == 0));
        assert_eq!(hits.len(), 4);
        let mut rq = q("A[N]D");
        rq.regex = true;
        rq.case_sensitive = false;
        let m = Matcher::new(&rq, codec).unwrap();
        let hits = all(&t, &m);
        assert_eq!(hits.len(), 1);
        let (s, e) = hits[0];
        assert_eq!(codec.decode_lossy(&t.read(s..e).unwrap()), "and");
    }

    #[test]
    fn cp1251_search() {
        let codec = Codec::new(Encoding::Windows1251, NlByte::Lf, 0);
        let data = codec.encode("Привет мир, привет!\n");
        let (_f, t) = buf_with(&data, codec);
        let m = Matcher::new(&q("мир"), codec).unwrap();
        assert!(m.is_literal());
        assert_eq!(all(&t, &m), vec![(7, 10)]);
        let mut ci = q("ПРИВЕТ");
        ci.case_sensitive = false;
        let m = Matcher::new(&ci, codec).unwrap();
        assert_eq!(all(&t, &m), vec![(0, 6), (12, 18)]);
    }

    #[test]
    fn search_with_edits_spanning_pieces() {
        let (_f, mut t) = buf_with(b"hello world", Codec::utf8());
        t.replace(5..5, b" big");
        t.replace(0..1, b"J");
        let m = Matcher::new(&q("lo big wo"), Codec::utf8()).unwrap();
        assert_eq!(all(&t, &m), vec![(3, 12)]);
    }

    #[test]
    fn backward_and_view() {
        let (_f, t) = buf_with(b"x1 x2 x3 x4", Codec::utf8());
        let m = Matcher::new(&q("x"), Codec::utf8()).unwrap();
        assert_eq!(search_backward(&t, &m, 0, 7, &CancelToken::new()), Some((6, 7)));
        assert_eq!(search_backward(&t, &m, 0, 0, &CancelToken::new()), None);
        assert_eq!(matches_in_view(&t, &m, 3..8, 100).len(), 2);
    }

    #[test]
    fn invalid_regex_reports_error() {
        let mut rq = q("(unclosed");
        rq.regex = true;
        assert!(Matcher::new(&rq, Codec::utf8()).is_err());
    }

    #[test]
    fn chunked_fallback_search() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        let mut data = vec![b'.'; 3_000_000];
        data[1_048_570..1_048_580].copy_from_slice(b"BOUNDARY!!");
        f.write_all(&data).unwrap();
        f.flush().unwrap();
        let src = Arc::new(Source::open(f.path(), OpenMode::Chunked).unwrap());
        let idx = index::build_blocking(&src, Codec::utf8());
        let mut t = TextBuf::new(src, idx, Codec::utf8());
        t.blocking = true;
        let m = Matcher::new(&q("BOUNDARY!!"), Codec::utf8()).unwrap();
        assert_eq!(all(&t, &m), vec![(1_048_570, 1_048_580)]);
    }
}
