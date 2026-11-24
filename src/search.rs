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
//! * The windows lie on a fixed grid from the range start, so each one can be searched on its
//!   own: [`search_forward_par`] searches them on worker threads and merges the matches in
//!   order, giving exactly what [`search_forward`] gives.

use std::collections::VecDeque;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

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

// One `Matcher` exists per search, so the literal finder's size wastes nothing, and boxing it
// would add an indirection to the hot path.
#[allow(clippy::large_enum_variant)]
enum Engine {
    Literal {
        finder: memchr::memmem::Finder<'static>,
        len: usize,
    },
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
    let pat = if q.regex {
        q.pattern.clone()
    } else {
        regex_syntax::escape(&q.pattern)
    };
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
        .map_err(|e| {
            e.syntax_error()
                .map(|s| s.to_string())
                .unwrap_or_else(|| e.to_string())
        })
}

impl Matcher {
    pub fn new(q: &Query, codec: Codec) -> Result<Matcher, String> {
        if q.pattern.is_empty() {
            return Err("Empty pattern".into());
        }
        let literal_ok =
            !q.regex && q.case_sensitive && !q.whole_word && codec.can_encode(&q.pattern);
        if literal_ok {
            let needle = codec.encode(&q.pattern);
            let len = needle.len();
            let finder = memchr::memmem::Finder::new(&needle).into_owned();
            return Ok(Matcher {
                engine: Engine::Literal { finder, len },
                codec,
                decoded: false,
                query: q.clone(),
            });
        }
        let re = build_regex(q, &codec)?;
        Ok(Matcher {
            engine: Engine::Regex(re),
            codec,
            decoded: codec.enc != Encoding::Utf8,
            query: q.clone(),
        })
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
    // The hot search loop: plain arguments rather than a struct built for every window.
    #[allow(clippy::too_many_arguments)]
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
                    let Some(i) = finder.find(&hay[pos..span_end]) else {
                        break;
                    };
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
        // A window edge inside a UTF-16 surrogate pair moves past the pair, which belongs to
        // the window it starts in (decoding from the middle would make two U+FFFD of it).
        let whole = |x: usize| {
            if x + 2 <= span_end && splits_pair(codec, hay, x) {
                x + 2
            } else {
                x
            }
        };
        let from = whole(align(from));
        let acc = whole(align(accept_end.min(span_end))).max(from);
        let mut text = String::with_capacity(span_end * 2);
        codec.decode_into(&hay[..from], &mut text);
        let u_from = text.len();
        codec.decode_into(&hay[from..acc], &mut text);
        let u_acc = if accept_end > span_end {
            usize::MAX
        } else {
            text.len()
        };
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

/// Whether `x` lies between the two halves of a UTF-16 surrogate pair in `hay`.
fn splits_pair(codec: Codec, hay: &[u8], x: usize) -> bool {
    let unit = |i: usize| match codec.enc {
        Encoding::Utf16Le => u16::from_le_bytes([hay[i], hay[i + 1]]),
        _ => u16::from_be_bytes([hay[i], hay[i + 1]]),
    };
    codec.enc.is_utf16()
        && x >= 2
        && x + 2 <= hay.len()
        && (0xD800..0xDC00).contains(&unit(x - 2))
        && (0xDC00..0xE000).contains(&unit(x))
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
    Scan::new(buf, m, GEOM, range, cancel, progress).run_seq(&mut on_match)
}

/// [`search_forward`] for whole-range jobs (Find All, counts, filters): on a local file and a
/// range of several windows it reads and searches the windows on worker threads, while
/// `on_match` still runs on the calling thread with exactly the matches, in the same order, of
/// [`search_forward`]. Otherwise it is [`search_forward`].
pub fn search_forward_par(
    buf: &TextBuf,
    m: &Matcher,
    range: Range<u64>,
    cancel: &CancelToken,
    progress: Option<&Progress>,
    mut on_match: impl FnMut(u64, u64) -> bool,
) -> SearchEnd {
    search_forward_par_ctx(buf, m, range, cancel, progress, |s, e, _, _| on_match(s, e))
}

/// [`search_forward_ctx`] on worker threads, as [`search_forward_par`] (`hay` is the window
/// the match was found in, which may start before the previous match).
pub fn search_forward_par_ctx(
    buf: &TextBuf,
    m: &Matcher,
    range: Range<u64>,
    cancel: &CancelToken,
    progress: Option<&Progress>,
    mut on_match: impl FnMut(u64, u64, &[u8], u64) -> bool,
) -> SearchEnd {
    let threads = search_threads(buf, &range);
    let scan = Scan::new(buf, m, GEOM, range, cancel, progress);
    if threads < 2 {
        scan.run_seq(&mut on_match)
    } else {
        scan.run_par(ParCfg::new(threads), &mut on_match)
    }
}

/// Worker threads [`search_forward_par`] uses for `range` of `buf`: 1 (the calling thread
/// alone) for chunked and remote sources, non-blocking snapshots and ranges of a few windows;
/// otherwise the available parallelism, at most 4: the windows are copied out of the page
/// cache, and that copy, not the matching, limits a search on more threads (a laptop's memory
/// bandwidth was used up at 4). `BIGTEXT_SEARCH_THREADS` overrides the number, from 1 (the
/// parallel search off) to 16: each thread adds two window buffers, so a search holds up to
/// about (2 x threads + 3) x 17 MB, 190 MB at 4 threads and 600 MB at 16.
pub fn search_threads(buf: &TextBuf, range: &Range<u64>) -> usize {
    static THREADS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let threads = *THREADS.get_or_init(|| {
        std::env::var("BIGTEXT_SEARCH_THREADS")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or_else(|| {
                std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(1)
                    .min(4)
            })
            .clamp(1, 16)
    });
    let local = buf.source.is_mmap() || buf.source.is_pread();
    let span = range.end.min(buf.len()).saturating_sub(range.start);
    if !buf.blocking || !local || span < 4 * WINDOW {
        1
    } else {
        threads
    }
}

/// Window geometry ([`WINDOW`], [`REGEX_OVERLAP`], [`MAX_MATCH`]); tests use small ones so that
/// matches meet window edges often.
#[derive(Clone, Copy, Debug)]
struct Geom {
    window: u64,
    regex_overlap: u64,
    max_match: u64,
}

const GEOM: Geom = Geom {
    window: WINDOW,
    regex_overlap: REGEX_OVERLAP,
    max_match: MAX_MATCH,
};

/// Where the window loop stands: the next search position and the end of the last match
/// (an empty match right there is skipped).
#[derive(Clone, Copy, Debug)]
struct Cursor {
    pos: u64,
    last_end: Option<u64>,
}

enum Flow {
    Next,
    Done(SearchEnd),
}

/// The outcome of searching one window from a cursor.
struct Pass {
    /// The callback asked to stop.
    halted: bool,
    /// A regex match ran into the end of the window's span (it may be longer).
    truncated: Option<u64>,
    /// The window end, or the furthest match end past it.
    max_end: u64,
    /// The end of the last match passed to the callback.
    pos: u64,
}

type OnMatch<'f> = dyn FnMut(u64, u64, &[u8], u64) -> bool + 'f;

/// One forward search of `start..end`.
///
/// The range is cut into windows on a fixed grid (`start + k * window`): a window takes the
/// matches that start in it, searching on into the next one by the overlap, and the next
/// window resumes where the last match ended. Because the grid does not move when a match
/// crosses an edge, every window can be searched on its own (see [`Scan::run_par`]).
struct Scan<'a> {
    buf: &'a TextBuf,
    m: &'a Matcher,
    g: Geom,
    start: u64,
    end: u64,
    /// `end` rounded down to a whole code unit (the last place a match can start).
    end_unit: u64,
    /// `end` is the end of the text: an empty match there counts (`$` after the last line);
    /// at the end of a selection it is the start of the next line and does not.
    at_text_end: bool,
    /// Length of the byte order mark, which is neither searched nor context.
    bom: u64,
    len: u64,
    unit: u64,
    overlap: u64,
    cancel: &'a CancelToken,
    progress: Option<&'a Progress>,
}

impl<'a> Scan<'a> {
    fn new(
        buf: &'a TextBuf,
        m: &'a Matcher,
        g: Geom,
        range: Range<u64>,
        cancel: &'a CancelToken,
        progress: Option<&'a Progress>,
    ) -> Scan<'a> {
        let len = buf.len();
        let unit = buf.codec.unit() as u64;
        // A byte order mark is not text: no match starts in it or before it, and it is not
        // context either (`^` matches at the start of line 1). The end is cut to whole code
        // units: a UTF-16 file of an odd length would otherwise never reach its last window.
        let bom = (buf.codec.bom_len as u64).min(len);
        let end = align_down(range.end.min(len), unit).max(bom);
        let start = align_down(range.start.max(bom).min(end), unit);
        let overlap = match &m.engine {
            Engine::Literal { len, .. } => (*len as u64).saturating_sub(1),
            Engine::Regex(_) => g.regex_overlap,
        };
        Scan {
            buf,
            m,
            g,
            start,
            end,
            end_unit: align_down(end, unit).max(start),
            at_text_end: end >= align_down(len, unit),
            bom,
            len,
            unit,
            overlap,
            cancel,
            progress,
        }
    }

    /// Grid window holding `pos`.
    fn index(&self, pos: u64) -> u64 {
        (pos - self.start) / self.g.window
    }

    /// End of the window holding `pos` (matches starting before it belong to that window).
    fn win_end(&self, pos: u64) -> u64 {
        let grid = self
            .start
            .saturating_add((self.index(pos) + 1).saturating_mul(self.g.window));
        align_down(grid.min(self.end), self.unit).max(pos.min(self.end))
    }

    fn is_final(&self, win_end: u64) -> bool {
        win_end >= self.end_unit
    }

    fn span_end(&self, win_end: u64) -> u64 {
        (win_end + self.overlap).min(self.end)
    }

    /// Bytes a window from `pos` reads: the span plus [`CTX`] on both sides.
    fn read_range(&self, pos: u64, win_end: u64) -> Range<u64> {
        let start = align_down(pos.saturating_sub(CTX), self.unit).max(self.bom.min(pos));
        start..(self.span_end(win_end) + CTX).min(self.len)
    }

    /// Searches the window ending at `win_end` from `cur` in `hay` (bytes from `read_start`,
    /// at least [`Scan::read_range`]), calling `f` for every match in order.
    fn pass(
        &self,
        cur: &mut Cursor,
        hay: &[u8],
        read_start: u64,
        win_end: u64,
        f: &mut dyn FnMut(u64, u64) -> bool,
    ) -> Pass {
        let span_end = self.span_end(win_end);
        let rel = |x: u64| (x - read_start) as usize;
        let accept = rel(win_end) + usize::from(self.is_final(win_end) && self.at_text_end);
        let (end, literal) = (self.end, self.m.is_literal());
        let mut p = Pass {
            halted: false,
            truncated: None,
            max_end: win_end,
            pos: cur.pos,
        };
        self.m.find_in(
            hay,
            read_start,
            rel(cur.pos),
            accept,
            rel(span_end),
            &mut cur.last_end,
            &mut |s, e| {
                if !literal && e == span_end && span_end < end {
                    p.truncated = Some(s);
                    return false;
                }
                p.max_end = p.max_end.max(e);
                p.pos = e;
                if !f(s, e) {
                    p.halted = true;
                    return false;
                }
                true
            },
        );
        p
    }

    /// After a match ran into the span end at `s`: re-search it with a growing window.
    fn finish_truncated(&self, cur: &mut Cursor, s: u64, on_match: &mut OnMatch) -> Flow {
        match extend_match(self.buf, self.m, self.g, s, self.end, self.cancel) {
            Some((s2, e2)) => {
                cur.last_end = Some(e2);
                if !on_match(s2, e2, &[], 0) {
                    return Flow::Done(SearchEnd::Stopped);
                }
                cur.pos = if e2 > s2 { e2 } else { self.buf.next_char(s2) };
            }
            None => cur.pos = self.buf.next_char(s),
        }
        if let Some(p) = self.progress {
            p.set_done(cur.pos.min(self.end) - self.start);
        }
        if cur.pos > self.end {
            Flow::Done(SearchEnd::Completed)
        } else {
            Flow::Next
        }
    }

    /// After a window was searched to its end.
    fn finish_window(&self, cur: &mut Cursor, win_end: u64, max_end: u64) -> Flow {
        let last = self.is_final(win_end);
        if let Some(p) = self.progress {
            p.set_done(if last { self.end } else { win_end } - self.start);
        }
        if last {
            return Flow::Done(SearchEnd::Completed);
        }
        cur.pos = max_end;
        Flow::Next
    }

    /// Reads and searches the window holding `cur.pos`, from there.
    fn seq_step(&self, cur: &mut Cursor, window: &mut Vec<u8>, on_match: &mut OnMatch) -> Flow {
        let win_end = self.win_end(cur.pos);
        let rr = self.read_range(cur.pos, win_end);
        let Some(n) = self.buf.read_to(rr.clone(), window) else {
            return Flow::Done(SearchEnd::Unavailable);
        };
        let hay = &window[..n];
        let p = self.pass(cur, hay, rr.start, win_end, &mut |s, e| {
            on_match(s, e, hay, rr.start)
        });
        if p.halted {
            return Flow::Done(SearchEnd::Stopped);
        }
        match p.truncated {
            Some(s) => self.finish_truncated(cur, s, on_match),
            None => self.finish_window(cur, win_end, p.max_end),
        }
    }

    /// The search on the calling thread, window after window.
    fn run_seq(&self, on_match: &mut OnMatch) -> SearchEnd {
        if let Some(p) = self.progress {
            p.set_total(self.end - self.start);
        }
        let mut cur = Cursor {
            pos: self.start,
            last_end: None,
        };
        let mut window = Vec::new();
        loop {
            if self.cancel.is_cancelled() {
                return SearchEnd::Cancelled;
            }
            if let Flow::Done(r) = self.seq_step(&mut cur, &mut window, on_match) {
                return r;
            }
        }
    }

    /// The search with `cfg.threads` workers. Each worker takes the next grid window `k`,
    /// reads it and searches it as if the sequential search had arrived at its start with no
    /// match before (cursor `start + k * window`, no last match end), passing its matches on in
    /// batches. The calling thread takes the windows in order and keeps the real cursor:
    ///
    /// * When the real cursor is the worker's starting cursor (the cursor is at the window
    ///   start, and no empty match there is suppressed), the window's search is the same
    ///   computation on the same bytes, so the worker's matches are used as they are.
    /// * Otherwise (the previous match crossed into this window, or ended right at its start)
    ///   the window is searched here from the real cursor, in the worker's buffer, until it
    ///   reports a match the worker reported too: from then on both searches stand at the same
    ///   cursor (the match end, with it as the last match end) in the same window, so the rest
    ///   of the worker's matches are used. That usually takes one or two matches.
    /// * A worker stops at a match that runs into its span end (the long match is extended
    ///   here, as in the sequential search) and when its read fails (the window is read and
    ///   searched here).
    ///
    /// At most `cfg.in_flight` windows are in progress at once, each holding at most
    /// `cfg.queued` matches not yet taken, so memory stays bounded; a worker that finds more
    /// waits.
    fn run_par(&self, cfg: ParCfg, on_match: &mut OnMatch) -> SearchEnd {
        if let Some(p) = self.progress {
            p.set_total(self.end - self.start);
        }
        let windows = (self.end - self.start).div_ceil(self.g.window);
        let sh = Shared::default();
        let merged = std::thread::scope(|scope| {
            // Stops the workers however this ends: a failed spawn, a panic in `on_match`.
            let _stop = StopOnDrop(&sh);
            let mut started = 0;
            for i in 0..cfg.threads {
                let (me, sh) = (&*self, &sh);
                #[cfg(test)]
                let injected = matches!(cfg.fault, Fault::SpawnFailsFrom(n) if i >= n);
                #[cfg(not(test))]
                let injected = false;
                let spawned = if injected {
                    Err(std::io::Error::other("injected spawn failure"))
                } else {
                    std::thread::Builder::new()
                        .name(format!("search-{i}"))
                        .spawn_scoped(scope, move || {
                            let _panic = PanicGuard(sh);
                            me.worker(sh, cfg, windows)
                        })
                };
                // Windows go to whichever worker asks next, so fewer workers only search
                // slower; with none, the search runs here.
                if spawned.is_err() {
                    break;
                }
                started += 1;
            }
            (started > 0).then(|| self.merge(&sh, cfg, windows, on_match))
        });
        match merged {
            Some(r) => r,
            None => self.run_seq(on_match),
        }
    }

    fn merge(&self, sh: &Shared, cfg: ParCfg, windows: u64, on_match: &mut OnMatch) -> SearchEnd {
        let mut cur = Cursor {
            pos: self.start,
            last_end: None,
        };
        let mut window = Vec::new();
        let mut feed: Option<Feed> = None;
        loop {
            // After a worker panicked the result does not matter: the scope raises the panic
            // in the caller once the workers have stopped, as the search here would have.
            if self.cancel.is_cancelled() || sh.panicked() {
                return SearchEnd::Cancelled;
            }
            let k = self.index(cur.pos);
            let ready = if k < windows {
                self.wait_hay(sh, cfg, k)
            } else {
                None
            };
            let flow = match ready {
                Some((hay, rs, n)) => {
                    if feed.as_ref().is_none_or(|f| f.k != k) {
                        feed = Some(Feed::new(k));
                    }
                    let f = feed.as_mut().expect("feed of window k");
                    self.merge_window(sh, &mut cur, f, &hay[..n], rs, on_match)
                }
                None if self.cancel.is_cancelled() || sh.panicked() => return SearchEnd::Cancelled,
                // No worker result (failed read, or past the last grid window): search here.
                None => self.seq_step(&mut cur, &mut window, on_match),
            };
            if let Flow::Done(r) = flow {
                return r;
            }
        }
    }

    /// Releases the windows before `k` and waits for window `k`'s buffer (`None`: the worker
    /// could not read it, or the search was cancelled).
    fn wait_hay(&self, sh: &Shared, cfg: ParCfg, k: u64) -> Option<(Arc<Vec<u8>>, u64, usize)> {
        let mut b = sh.lock();
        if b.first < k {
            while b.first < k {
                if let Some((hay, _, _)) = b.slots.pop_front().and_then(|s| s.hay) {
                    b.recycle(hay, cfg);
                }
                b.first += 1;
            }
            sh.to_workers.notify_all();
        }
        loop {
            if self.cancel.is_cancelled() || sh.panicked() {
                return None;
            }
            if let Some(s) = b.slot(k) {
                if let Some((hay, rs, n)) = &s.hay {
                    return Some((hay.clone(), *rs, *n));
                }
                if s.end.is_some() {
                    return None;
                }
            }
            b = sh.wait_merger(b);
        }
    }

    /// Takes window `feed.k` (its bytes in `hay`, from `rs_w`) from `cur` to its end, or to a
    /// match that has to be extended.
    fn merge_window(
        &self,
        sh: &Shared,
        cur: &mut Cursor,
        feed: &mut Feed,
        hay: &[u8],
        rs_w: u64,
        on_match: &mut OnMatch,
    ) -> Flow {
        let g_k = self.start + feed.k * self.g.window;
        let worker_end = self.win_end(g_k);
        let mut win_end = self.win_end(cur.pos);
        let mut joined = cur.pos == g_k && cur.last_end.is_none_or(|le| le < g_k);
        let mut max_end = win_end;
        loop {
            if joined {
                while let Some((s, e)) = feed.next(self, sh) {
                    max_end = max_end.max(e);
                    cur.pos = e;
                    cur.last_end = Some(e);
                    if !on_match(s, e, hay, rs_w) {
                        return Flow::Done(SearchEnd::Stopped);
                    }
                }
                if feed.end == Some(WorkerEnd::Complete) {
                    return self.finish_window(cur, win_end, max_end);
                }
                if self.cancel.is_cancelled() || sh.panicked() {
                    return Flow::Done(SearchEnd::Cancelled);
                }
                // The worker stopped early: go on from the cursor here.
            }
            win_end = self.win_end(cur.pos);
            let can_join = win_end == worker_end;
            let rs = self.read_range(cur.pos, win_end).start;
            let sub = &hay[(rs - rs_w) as usize..];
            let (mut stopped, mut join, mut aborted) = (false, false, false);
            let p = self.pass(cur, sub, rs, win_end, &mut |s, e| {
                if sh.panicked() {
                    aborted = true;
                    return false;
                }
                if !on_match(s, e, hay, rs_w) {
                    stopped = true;
                    return false;
                }
                join = can_join && feed.join(self, sh, s, e);
                !join
            });
            if aborted {
                return Flow::Done(SearchEnd::Cancelled);
            }
            if stopped {
                return Flow::Done(SearchEnd::Stopped);
            }
            max_end = max_end.max(p.max_end);
            if join {
                cur.pos = p.pos;
                joined = true;
                continue;
            }
            return match p.truncated {
                Some(s) => self.finish_truncated(cur, s, on_match),
                None => self.finish_window(cur, win_end, max_end),
            };
        }
    }

    fn worker(&self, sh: &Shared, cfg: ParCfg, windows: u64) {
        let mut spare: Option<Vec<u8>> = None;
        loop {
            let (k, mut window) = {
                let mut b = sh.lock();
                loop {
                    if b.stop || self.cancel.is_cancelled() {
                        return;
                    }
                    b.next = b.next.max(b.first);
                    if b.next >= windows {
                        return;
                    }
                    if b.next - b.first < cfg.in_flight as u64 {
                        break;
                    }
                    b = sh.wait_workers(b);
                }
                let k = b.next;
                b.next += 1;
                b.slots.push_back(Slot::default());
                let window = spare.take().or_else(|| b.spare.pop()).unwrap_or_default();
                (k, window)
            };
            let pos = self.start + k * self.g.window;
            let win_end = self.win_end(pos);
            let rr = self.read_range(pos, win_end);
            #[cfg(test)]
            if cfg.fault == Fault::PanicAt(k, false) {
                panic!("injected panic before reading window {k}");
            }
            let Some(n) = self.buf.read_to(rr.clone(), &mut window) else {
                sh.finish(k, Vec::new(), WorkerEnd::Failed);
                spare = Some(window);
                continue;
            };
            let hay = Arc::new(window);
            {
                let mut b = sh.lock();
                match b.slot(k) {
                    Some(s) => s.hay = Some((hay.clone(), rr.start, n)),
                    None => {
                        drop(b);
                        spare = Arc::try_unwrap(hay).ok();
                        continue;
                    }
                }
                sh.to_merger.notify_one();
            }
            #[cfg(test)]
            if cfg.fault == Fault::PanicAt(k, true) {
                panic!("injected panic while searching window {k}");
            }
            let mut batch = Vec::with_capacity(cfg.batch);
            let mut aborted = false;
            let mut cur = Cursor {
                pos,
                last_end: None,
            };
            let p = self.pass(&mut cur, &hay[..n], rr.start, win_end, &mut |s, e| {
                batch.push((s, e));
                if batch.len() >= cfg.batch && !sh.publish(self, k, &mut batch, cfg) {
                    aborted = true;
                    return false;
                }
                true
            });
            let end = if aborted {
                WorkerEnd::Failed
            } else if p.truncated.is_some() {
                WorkerEnd::Truncated
            } else {
                WorkerEnd::Complete
            };
            sh.finish(k, batch, end);
            spare = Arc::try_unwrap(hay).ok();
        }
    }
}

/// How a parallel search is spread out.
#[derive(Clone, Copy, Debug)]
struct ParCfg {
    threads: usize,
    /// Windows read or searched ahead of the one being merged (each holds a buffer).
    in_flight: usize,
    /// Matches of one window waiting to be merged before its worker waits.
    queued: usize,
    /// Matches passed on at once.
    batch: usize,
    #[cfg(test)]
    fault: Fault,
}

impl ParCfg {
    fn new(threads: usize) -> ParCfg {
        ParCfg {
            threads,
            in_flight: threads + 2,
            queued: 64 * 1024,
            batch: 4096,
            #[cfg(test)]
            fault: Fault::None,
        }
    }
}

/// A failure to inject in tests.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    None,
    /// Spawning worker `n` and later ones fails.
    SpawnFailsFrom(usize),
    /// The worker of window `k` panics, after handing over the window's bytes if `true`.
    PanicAt(u64, bool),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorkerEnd {
    /// Searched to the window end.
    Complete,
    /// Stopped at a match that runs into the span end.
    Truncated,
    /// Not read, abandoned or cancelled.
    Failed,
}

/// One window being searched by a worker.
#[derive(Default)]
struct Slot {
    /// The window's bytes, their offset and length.
    hay: Option<(Arc<Vec<u8>>, u64, usize)>,
    batches: VecDeque<Vec<(u64, u64)>>,
    queued: usize,
    end: Option<WorkerEnd>,
}

#[derive(Default)]
struct Board {
    /// Window of `slots[0]`: the one being merged; earlier ones are done.
    first: u64,
    /// Next window to hand to a worker (`slots` holds `first..next`).
    next: u64,
    slots: VecDeque<Slot>,
    stop: bool,
    /// Window buffers for reuse.
    spare: Vec<Vec<u8>>,
}

impl Board {
    fn slot(&mut self, k: u64) -> Option<&mut Slot> {
        let i = k.checked_sub(self.first)?;
        self.slots.get_mut(usize::try_from(i).ok()?)
    }

    fn recycle(&mut self, hay: Arc<Vec<u8>>, cfg: ParCfg) {
        if self.spare.len() < cfg.in_flight {
            if let Ok(v) = Arc::try_unwrap(hay) {
                self.spare.push(v);
            }
        }
    }
}

#[derive(Default)]
struct Shared {
    board: Mutex<Board>,
    to_merger: Condvar,
    to_workers: Condvar,
    /// A worker panicked: everyone stops, and the scope raises the panic in the caller.
    panicked: AtomicBool,
}

impl Shared {
    fn panicked(&self) -> bool {
        self.panicked.load(Ordering::Relaxed)
    }

    fn lock(&self) -> MutexGuard<'_, Board> {
        self.board.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn wait_workers<'g>(&self, g: MutexGuard<'g, Board>) -> MutexGuard<'g, Board> {
        self.to_workers
            .wait(g)
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The merger also wakes up now and then to notice a cancellation.
    fn wait_merger<'g>(&self, g: MutexGuard<'g, Board>) -> MutexGuard<'g, Board> {
        self.to_merger
            .wait_timeout(g, Duration::from_millis(20))
            .unwrap_or_else(PoisonError::into_inner)
            .0
    }

    /// Hands a batch of window `k`'s matches to the merger, waiting while too many are
    /// queued. False if the window is no longer wanted.
    fn publish(&self, scan: &Scan, k: u64, batch: &mut Vec<(u64, u64)>, cfg: ParCfg) -> bool {
        let mut b = self.lock();
        loop {
            if b.stop || scan.cancel.is_cancelled() {
                return false;
            }
            let Some(s) = b.slot(k) else {
                return false;
            };
            if s.queued < cfg.queued {
                s.queued += batch.len();
                let full = std::mem::replace(batch, Vec::with_capacity(cfg.batch));
                s.batches.push_back(full);
                self.to_merger.notify_one();
                return true;
            }
            b = self.wait_workers(b);
        }
    }

    fn finish(&self, k: u64, batch: Vec<(u64, u64)>, end: WorkerEnd) {
        let mut b = self.lock();
        if let Some(s) = b.slot(k) {
            if !batch.is_empty() {
                s.queued += batch.len();
                s.batches.push_back(batch);
            }
            s.end = Some(end);
        }
        self.to_merger.notify_one();
    }
}

/// Makes a panicking worker stop the search, so that nobody waits for its window.
struct PanicGuard<'a>(&'a Shared);

impl Drop for PanicGuard<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.panicked.store(true, Ordering::Relaxed);
            self.0.lock().stop = true;
            self.0.to_merger.notify_all();
            self.0.to_workers.notify_all();
        }
    }
}

struct StopOnDrop<'a>(&'a Shared);

impl Drop for StopOnDrop<'_> {
    fn drop(&mut self) {
        self.0.lock().stop = true;
        self.0.to_workers.notify_all();
    }
}

/// The merger's reader of one window's worker matches.
struct Feed {
    k: u64,
    batch: Vec<(u64, u64)>,
    at: usize,
    /// How the worker ended, once all its matches are taken.
    end: Option<WorkerEnd>,
}

impl Feed {
    fn new(k: u64) -> Feed {
        Feed {
            k,
            batch: Vec::new(),
            at: 0,
            end: None,
        }
    }

    /// The worker's next match, waiting for it; `None` at the worker's end or on cancel.
    fn peek(&mut self, scan: &Scan, sh: &Shared) -> Option<(u64, u64)> {
        if let Some(&m) = self.batch.get(self.at) {
            return Some(m);
        }
        if self.end.is_some() {
            return None;
        }
        let mut b = sh.lock();
        loop {
            if sh.panicked() {
                self.end = Some(WorkerEnd::Failed);
                return None;
            }
            if scan.cancel.is_cancelled() {
                return None;
            }
            let Some(s) = b.slot(self.k) else {
                self.end = Some(WorkerEnd::Failed);
                return None;
            };
            if let Some(v) = s.batches.pop_front() {
                s.queued -= v.len();
                sh.to_workers.notify_all();
                self.batch = v;
                self.at = 0;
                if let Some(&m) = self.batch.first() {
                    return Some(m);
                }
                continue;
            }
            if let Some(e) = s.end {
                self.end = Some(e);
                return None;
            }
            b = sh.wait_merger(b);
        }
    }

    fn next(&mut self, scan: &Scan, sh: &Shared) -> Option<(u64, u64)> {
        let m = self.peek(scan, sh)?;
        self.at += 1;
        Some(m)
    }

    /// Whether the worker reported the match `s..e` (skipping its matches before `s`).
    fn join(&mut self, scan: &Scan, sh: &Shared, s: u64, e: u64) -> bool {
        while let Some((ws, we)) = self.peek(scan, sh) {
            if ws < s {
                self.at += 1;
                continue;
            }
            if (ws, we) == (s, e) {
                self.at += 1;
                return true;
            }
            return false;
        }
        false
    }
}

fn extend_match(
    buf: &TextBuf,
    m: &Matcher,
    g: Geom,
    s: u64,
    end: u64,
    cancel: &CancelToken,
) -> Option<(u64, u64)> {
    let len = buf.len();
    let unit = buf.codec.unit() as u64;
    let mut lim = (s + 4 * g.regex_overlap).min(end);
    loop {
        if cancel.is_cancelled() {
            return None;
        }
        let rs = align_down(s.saturating_sub(CTX), unit);
        let hay = buf.read(rs..(lim + CTX).min(len))?;
        let mut found = None;
        let mut le = None;
        m.find_in(
            &hay,
            rs,
            (s - rs) as usize,
            // Accept only a match starting at `s`: one whole code unit, or a UTF-16 search
            // rounds the range down to nothing and the long match is lost.
            (s - rs + unit) as usize,
            (lim - rs) as usize,
            &mut le,
            &mut |a, b| {
                found = Some((a, b));
                false
            },
        );
        match found {
            Some((a, b)) if b < lim || lim >= end || lim - s >= g.max_match => return Some((a, b)),
            Some(_) => lim = (s + (lim - s) * 4).min(end).min(s + g.max_match),
            None => return None,
        }
    }
}

/// Last match starting before `before` (and at or after `floor`), searching backwards in windows.
pub fn search_backward(
    buf: &TextBuf,
    m: &Matcher,
    floor: u64,
    before: u64,
    cancel: &CancelToken,
) -> Option<(u64, u64)> {
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
pub fn matches_in_view(
    buf: &TextBuf,
    m: &Matcher,
    range: Range<u64>,
    cap: usize,
) -> Vec<(u64, u64)> {
    let len = buf.len();
    let unit = buf.codec.unit() as u64;
    let start = align_down(range.start.saturating_sub(4096), unit);
    let end = (range.end + 4096).min(len);
    let rs = align_down(start.saturating_sub(CTX), unit);
    let Some(hay) = buf.read(rs..(end + CTX).min(len)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut le = None;
    let accept = if end == len {
        (end - rs) as usize + 1
    } else {
        (end - rs) as usize
    };
    m.find_in(
        &hay,
        rs,
        (start - rs) as usize,
        accept,
        (end - rs) as usize,
        &mut le,
        &mut |s, e| {
            if e >= range.start && s <= range.end {
                out.push((s, e));
            }
            out.len() < cap
        },
    );
    out
}

/// Captures for the match starting exactly at `start` (regex queries; used by replace).
/// The text of capture group `g` (0: the whole match) of the match `start..end`, if the group
/// took part in it.
pub fn group_text(buf: &TextBuf, m: &Matcher, start: u64, end: u64, g: usize) -> Option<String> {
    let (hay, caps, _) = captures_at(buf, m, start, end)?;
    let span = caps.get_group(g)?;
    let b = &hay[span.range()];
    Some(if m.decoded {
        String::from_utf8_lossy(b).into_owned()
    } else {
        m.codec.decode_lossy(b)
    })
}

/// The names of the capture groups of a regular expression ("" for unnamed ones), from group 1.
pub fn group_names(m: &Matcher) -> Vec<String> {
    let Some(re) = m.regex() else {
        return Vec::new();
    };
    re.group_info()
        .pattern_names(regex_automata::PatternID::ZERO)
        .skip(1)
        .map(|n| n.unwrap_or("").to_string())
        .collect()
}

pub fn captures_at(
    buf: &TextBuf,
    m: &Matcher,
    start: u64,
    end: u64,
) -> Option<(Vec<u8>, Captures, usize)> {
    let mut caps = m.regex()?.create_captures();
    let mut hay = Vec::new();
    let s = captures_into(buf, m, start, end, &mut caps, &mut hay)?;
    Some((hay, caps, s))
}

/// [`captures_at`] into caller-owned `caps` and `hay`, so a loop over many matches allocates
/// nothing per match. Returns the offset of the match start in `hay`.
pub fn captures_into(
    buf: &TextBuf,
    m: &Matcher,
    start: u64,
    end: u64,
    caps: &mut Captures,
    hay: &mut Vec<u8>,
) -> Option<usize> {
    let re = m.regex()?;
    let len = buf.len();
    let unit = buf.codec.unit() as u64;
    let rs = align_down(start.saturating_sub(CTX), unit);
    let re_end = (end + CTX).min(len);
    let (s, e) = if m.decoded {
        let raw = buf.read(rs..re_end)?;
        hay.clear();
        let mut t = String::from_utf8(std::mem::take(hay)).unwrap_or_default();
        m.codec.decode_into(&raw[..(start - rs) as usize], &mut t);
        let s = t.len();
        m.codec
            .decode_into(&raw[(start - rs) as usize..(end - rs) as usize], &mut t);
        let e = t.len();
        m.codec.decode_into(&raw[(end - rs) as usize..], &mut t);
        *hay = t.into_bytes();
        (s, e)
    } else {
        // `read` borrows from the mapping or block cache; `read_to` would issue one positional
        // read per match on worker snapshots.
        let raw = buf.read(rs..re_end)?;
        hay.clear();
        hay.extend_from_slice(&raw);
        ((start - rs) as usize, (end - rs) as usize)
    };
    re.search_captures(
        &Input::new(&hay[..])
            .range(s..e)
            .anchored(regex_automata::Anchored::Yes),
        caps,
    );
    caps.is_match().then_some(s)
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
        Query {
            pattern: p.into(),
            case_sensitive: true,
            ..Default::default()
        }
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
        let (_f, t) = buf_with(
            "Straße STRASSE straße ПРИВЕТ привет".as_bytes(),
            Codec::utf8(),
        );
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

    /// A regex match cut at the end of a window is searched again from its start with a longer
    /// span. In UTF-16 that search accepted no start at all, so the match was dropped and found
    /// again one character later: Replace All left the first character of a long match.
    #[test]
    fn long_match_is_extended_in_every_encoding() {
        let mut rq = q("a+");
        rq.regex = true;
        let utf16 = Codec::new(Encoding::Utf16Le, NlByte::Lf, 2);
        let cp1251 = Codec::new(Encoding::Windows1251, NlByte::Lf, 0);
        for codec in [Codec::utf8(), utf16, cp1251] {
            let mut data = if codec.enc == Encoding::Utf16Le {
                vec![0xFF, 0xFE]
            } else {
                Vec::new()
            };
            let bom = data.len() as u64;
            data.extend(codec.encode(&format!("xx{}\nyy", "a".repeat(300))));
            let (_f, t) = buf_with(&data, codec);
            let m = Matcher::new(&rq, codec).unwrap();
            let unit = codec.unit() as u64;
            let s = bom + 2 * unit;
            assert_eq!(
                extend_match(&t, &m, GEOM, s, t.len(), &CancelToken::new()),
                Some((s, s + 300 * unit)),
                "{:?}",
                codec.enc
            );
        }
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
        assert_eq!(
            search_backward(&t, &m, 0, 7, &CancelToken::new()),
            Some((6, 7))
        );
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

    // ---- Grid windows and the parallel search ----

    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n.max(1)
        }
        fn pick<T: Copy>(&mut self, v: &[T]) -> T {
            v[self.below(v.len() as u64) as usize]
        }
    }

    /// Runs one search the given way, checking that `hay` holds every match it is passed with
    /// and that progress only moves forward.
    fn run_with(
        buf: &TextBuf,
        m: &Matcher,
        g: Geom,
        range: Range<u64>,
        par: Option<ParCfg>,
        progress: Option<&Progress>,
    ) -> (Vec<(u64, u64)>, SearchEnd) {
        let cancel = CancelToken::new();
        let scan = Scan::new(buf, m, g, range, &cancel, progress);
        let mut v = Vec::new();
        let mut done = 0;
        let mut f = |s: u64, e: u64, hay: &[u8], base: u64| {
            if !hay.is_empty() {
                assert!(
                    base <= s && e <= base + hay.len() as u64,
                    "{s}..{e} not in hay"
                );
                let want = buf.read(s..e).unwrap();
                assert_eq!(&hay[(s - base) as usize..(e - base) as usize], &want[..]);
            }
            if let Some(p) = progress {
                assert!(p.done() >= done, "progress went back");
                done = p.done();
            }
            v.push((s, e));
            true
        };
        let r = match par {
            None => scan.run_seq(&mut f),
            Some(cfg) => scan.run_par(cfg, &mut f),
        };
        (v, r)
    }

    /// The forward search as it was before the grid: each window starts where the last match
    /// of the previous one ended and spans `window` bytes from there (with the byte order mark
    /// and text-end rules of that loop).
    fn legacy(buf: &TextBuf, m: &Matcher, g: Geom, range: Range<u64>) -> Vec<(u64, u64)> {
        let cancel = CancelToken::new();
        let mut out = Vec::new();
        let len = buf.len();
        let unit = buf.codec.unit() as u64;
        let bom = (buf.codec.bom_len as u64).min(len);
        let end = align_down(range.end.min(len), unit).max(bom);
        let start = align_down(range.start.max(bom).min(end), unit);
        let overlap = if m.is_literal() {
            m.overlap()
        } else {
            g.regex_overlap
        };
        let mut pos = start;
        let mut last_end: Option<u64> = None;
        loop {
            let win_end = align_down((pos + g.window).min(end), unit).max(pos.min(end));
            let span_end = (win_end + overlap).min(end);
            let read_start = align_down(pos.saturating_sub(CTX), unit).max(bom.min(pos));
            let read_end = (span_end + CTX).min(len);
            let hay = buf.read(read_start..read_end).unwrap();
            let final_window = win_end >= end;
            let rel = |x: u64| (x - read_start) as usize;
            let accept = rel(win_end) + usize::from(final_window && end >= align_down(len, unit));
            let mut truncated: Option<u64> = None;
            let mut max_end = win_end;
            m.find_in(
                &hay,
                read_start,
                rel(pos),
                accept,
                rel(span_end),
                &mut last_end,
                &mut |s, e| {
                    if !m.is_literal() && e == span_end && span_end < end {
                        truncated = Some(s);
                        return false;
                    }
                    out.push((s, e));
                    max_end = max_end.max(e);
                    true
                },
            );
            if let Some(s) = truncated {
                match extend_match(buf, m, g, s, end, &cancel) {
                    Some((s2, e2)) => {
                        last_end = Some(e2);
                        out.push((s2, e2));
                        pos = if e2 > s2 { e2 } else { buf.next_char(s2) };
                    }
                    None => pos = buf.next_char(s),
                }
                if pos > end {
                    break;
                }
                continue;
            }
            if final_window {
                break;
            }
            pos = max_end;
        }
        out
    }

    fn queries() -> Vec<Query> {
        let ci = |p: &str| Query {
            case_sensitive: false,
            ..q(p)
        };
        let word = |p: &str| Query {
            whole_word: true,
            ..q(p)
        };
        let re = |p: &str| Query {
            regex: true,
            ..q(p)
        };
        vec![
            q("ab"),
            q("a"),
            q("aba"),
            q("b a"),
            q("aaaa"),
            q("ж"),
            q("Жж"),
            q("\n"),
            ci("AB"),
            ci("Ж"),
            ci("A B"),
            word("ab"),
            word("a"),
            re("a+"),
            re("x*"),
            re(r"\b"),
            re(r"\B"),
            re("^"),
            re("$"),
            re("^a.*$"),
            re(r"a[ab ]*b"),
            re(r"[^\n]{3,8}"),
            re(r"\w+"),
            re("b|ab|aab"),
            re("a{2,}?"),
            re(r"(?s)a.{0,90}b"),
            re("a*?b*"),
            re("(?:ab)*"),
            re(r"[ab]+\r?\n"),
            re(r"\b\w"),
            re("(?s).*"),
            re("ж+"),
            re("(?i)ЖЖ|a"),
        ]
    }

    fn random_text(rng: &mut Rng, cyrillic: bool) -> String {
        let pieces: &[&str] = if cyrillic {
            &["a", "b", " ", "\n", "ж", "Ж", "ab", "aaaaaaa", "\r\n", "x"]
        } else {
            &[
                "a", "b", " ", "\n", "x", "é", "Ж", "ж", "ab", "aaaaaaa", "\r\n", "😀",
            ]
        };
        let n = rng.below(1500);
        let mut s = String::new();
        // Mostly short pieces, now and then a long run (long matches, long lines).
        for _ in 0..n {
            if rng.below(40) == 0 {
                let p = rng.pick(&["a", "ab", "x", "b "]);
                for _ in 0..rng.below(120) {
                    s.push_str(p);
                }
            } else {
                s.push_str(rng.pick(pieces));
            }
        }
        s
    }

    fn random_geom(rng: &mut Rng) -> Geom {
        Geom {
            window: rng.pick(&[4, 8, 16, 24, 64, 100, 256, 1000]),
            regex_overlap: rng.pick(&[2, 4, 16, 50, 300]),
            max_match: rng.pick(&[8, 64, 1000, 100_000]),
        }
    }

    fn random_cfg(rng: &mut Rng) -> ParCfg {
        ParCfg {
            threads: 2 + rng.below(3) as usize,
            in_flight: 1 + rng.below(5) as usize,
            queued: 1 + rng.below(20) as usize,
            batch: 1 + rng.below(8) as usize,
            fault: Fault::None,
        }
    }

    /// A document in one of the test encodings, sometimes edited through the piece table.
    fn random_doc(rng: &mut Rng) -> (tempfile::NamedTempFile, TextBuf, &'static str) {
        let kind = rng.below(4);
        let (codec, cyrillic, name) = match kind {
            0 | 1 => (Codec::utf8(), false, "utf-8"),
            2 => (
                Codec::new(Encoding::Utf16Le, NlByte::Lf, 2),
                false,
                "utf-16",
            ),
            _ => (
                Codec::new(Encoding::Windows1251, NlByte::Lf, 0),
                true,
                "cp1251",
            ),
        };
        let text = random_text(rng, cyrillic);
        let mut data = if codec.enc.is_utf16() {
            vec![0xFF, 0xFE]
        } else {
            Vec::new()
        };
        data.extend(codec.encode(&text));
        let (f, mut t) = buf_with(&data, codec);
        if kind == 1 {
            // Edits: windows are assembled across piece boundaries.
            for _ in 0..rng.below(12) {
                let len = t.len();
                let a = rng.below(len + 1);
                let b = (a + rng.below(30)).min(len);
                let ins: String = random_text(rng, false)
                    .chars()
                    .take(rng.below(60) as usize)
                    .collect();
                let a = t.snap_char(a);
                let b = t.snap_char(b).max(a);
                t.replace(a..b, ins.as_bytes());
            }
            return (f, t, "utf-8 edited");
        }
        (f, t, name)
    }

    #[test]
    fn parallel_search_matches_sequential_exactly() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let qs = queries();
        let (mut compared, mut with_matches) = (0, 0);
        for round in 0..crate::robustness::rounds(120) {
            let (_f, t, name) = random_doc(&mut rng);
            for query in &qs {
                let Ok(m) = Matcher::new(query, t.codec) else {
                    continue;
                };
                let len = t.len();
                let range = if rng.below(3) == 0 {
                    let a = rng.below(len + 1);
                    a..a + rng.below(len + 1 - a + 10)
                } else {
                    0..len
                };
                let g = random_geom(&mut rng);
                let cfg = random_cfg(&mut rng);
                let progress = Progress::default();
                let (seq, r1) = run_with(&t, &m, g, range.clone(), None, None);
                let (par, r2) = run_with(&t, &m, g, range.clone(), Some(cfg), Some(&progress));
                assert_eq!(r1, SearchEnd::Completed);
                assert_eq!(r2, SearchEnd::Completed);
                assert_eq!(
                    seq, par,
                    "round {round} {name} {query:?} {range:?} {g:?} {cfg:?}"
                );
                assert_eq!(progress.done(), progress.total());
                compared += 1;
                with_matches += usize::from(!seq.is_empty());
            }
        }
        assert!(compared > 3000 && with_matches > compared / 2);
    }

    #[test]
    fn grid_windows_find_what_moving_windows_found() {
        // With matches shorter than the overlap it does not matter where the windows lie: the
        // grid gives exactly what windows that moved with the last match gave.
        let mut rng = Rng(0x2545_F491_4F6C_DD1D);
        let qs = queries();
        for round in 0..crate::robustness::rounds(80) {
            let (_f, t, name) = random_doc(&mut rng);
            let g = Geom {
                regex_overlap: 1 << 20,
                ..random_geom(&mut rng)
            };
            for query in &qs {
                let Ok(m) = Matcher::new(query, t.codec) else {
                    continue;
                };
                let (now, _) = run_with(&t, &m, g, 0..t.len(), None, None);
                let before = legacy(&t, &m, g, 0..t.len());
                assert_eq!(now, before, "round {round} {name} {query:?} {g:?}");
            }
        }
    }

    #[test]
    fn odd_length_utf16_search_ends() {
        // A UTF-16 document cut in the middle of a code unit: the search used to go round at
        // the last whole unit for ever.
        let mut data = vec![0xFF, 0xFE];
        data.extend("ab ab".encode_utf16().flat_map(|u| u.to_le_bytes()));
        data.push(b'a');
        let codec = Codec::new(Encoding::Utf16Le, NlByte::Lf, 2);
        let (_f, t) = buf_with(&data, codec);
        let m = Matcher::new(&q("b"), codec).unwrap();
        assert_eq!(all(&t, &m), vec![(4, 6), (10, 12)]);
        let g = Geom { window: 4, ..GEOM };
        let cfg = ParCfg {
            threads: 2,
            in_flight: 2,
            queued: 1,
            batch: 1,
            fault: Fault::None,
        };
        let (par, _) = run_with(&t, &m, g, 0..t.len(), Some(cfg), None);
        assert_eq!(par, vec![(4, 6), (10, 12)]);
    }

    fn dense_doc() -> (tempfile::NamedTempFile, TextBuf) {
        let mut rng = Rng(7);
        let mut text = String::new();
        while text.len() < 200_000 {
            text.push_str(rng.pick(&["ab ", "a", "b", "\n", "aab x "]));
        }
        buf_with(text.as_bytes(), Codec::utf8())
    }

    const SMALL: Geom = Geom {
        window: 512,
        regex_overlap: 64,
        max_match: 4096,
    };

    fn rq(p: &str) -> Query {
        Query {
            regex: true,
            ..q(p)
        }
    }

    #[test]
    fn parallel_search_stops_when_asked() {
        let (_f, t) = dense_doc();
        let cfg = ParCfg {
            threads: 4,
            in_flight: 4,
            queued: 16,
            batch: 4,
            fault: Fault::None,
        };
        for pat in ["ab", r"\w+", "x*"] {
            let m = Matcher::new(&rq(pat), Codec::utf8()).unwrap();
            let (seq, _) = run_with(&t, &m, SMALL, 0..t.len(), None, None);
            for stop_at in [1, 2, 7, 100, 1000, seq.len() / 2, seq.len() - 1] {
                let cancel = CancelToken::new();
                let scan = Scan::new(&t, &m, SMALL, 0..t.len(), &cancel, None);
                let mut got = Vec::new();
                let r = scan.run_par(cfg, &mut |s, e, _, _| {
                    assert!(got.len() < stop_at, "called after it asked to stop");
                    got.push((s, e));
                    got.len() < stop_at
                });
                assert_eq!(r, SearchEnd::Stopped);
                assert_eq!(got, seq[..stop_at]);
            }
        }
    }

    #[test]
    fn parallel_search_cancels() {
        let (_f, t) = dense_doc();
        let cfg = ParCfg::new(4);
        let m = Matcher::new(&rq(r"\w+"), Codec::utf8()).unwrap();
        let (seq, _) = run_with(&t, &m, SMALL, 0..t.len(), None, None);
        // Cancelled before it starts: nothing is reported.
        let cancel = CancelToken::new();
        cancel.cancel();
        let scan = Scan::new(&t, &m, SMALL, 0..t.len(), &cancel, None);
        let mut n = 0;
        let r = scan.run_par(cfg, &mut |_, _, _, _| {
            n += 1;
            true
        });
        assert_eq!(r, SearchEnd::Cancelled);
        assert_eq!(n, 0);
        // Cancelled from the callback: what was reported is the start of the full list.
        for at in [1, 50, 3000] {
            let cancel = CancelToken::new();
            let scan = Scan::new(&t, &m, SMALL, 0..t.len(), &cancel, None);
            let mut got = Vec::new();
            let r = scan.run_par(cfg, &mut |s, e, _, _| {
                got.push((s, e));
                if got.len() == at {
                    cancel.cancel();
                }
                true
            });
            assert_eq!(r, SearchEnd::Cancelled);
            assert!(got.len() >= at && got.len() < seq.len());
            assert_eq!(got, seq[..got.len()]);
        }
        // Cancelled from another thread while the workers wait for a slow merger.
        let cancel = CancelToken::new();
        let scan = Scan::new(&t, &m, SMALL, 0..t.len(), &cancel, None);
        let started = std::time::Instant::now();
        let r = std::thread::scope(|s| {
            s.spawn(|| {
                std::thread::sleep(Duration::from_millis(30));
                cancel.cancel();
            });
            scan.run_par(cfg, &mut |_, _, _, _| {
                std::thread::sleep(Duration::from_micros(200));
                true
            })
        });
        assert_eq!(r, SearchEnd::Cancelled);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn parallel_search_with_real_windows() {
        // Four and a half windows of a local file: the public entry point searches on several
        // threads and gives what the sequential search gives, also across window edges.
        let n = 4 * WINDOW as usize + WINDOW as usize / 2;
        let mut data = vec![b'x'; n];
        for k in 1..5 {
            let at = k * WINDOW as usize;
            data[at - 3..at + 3].copy_from_slice(b"NEEDLE");
            data[at + 100..at + 106].copy_from_slice(b"NEEDLE");
        }
        for i in (0..n).step_by(1_000_003) {
            data[i] = b'\n';
        }
        let (_f, t) = buf_with(&data, Codec::utf8());
        if std::env::var_os("BIGTEXT_SEARCH_THREADS").is_none() {
            let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
            assert_eq!(search_threads(&t, &(0..t.len())) > 1, threads > 1);
        }
        for query in [q("NEEDLE"), rq("N[A-Z]+E"), rq(r"x\nx"), rq("^")] {
            let m = Matcher::new(&query, Codec::utf8()).unwrap();
            let seq = all(&t, &m);
            let mut par = Vec::new();
            let r = search_forward_par(&t, &m, 0..t.len(), &CancelToken::new(), None, |s, e| {
                par.push((s, e));
                true
            });
            assert_eq!(r, SearchEnd::Completed);
            assert_eq!(par, seq, "{query:?}");
        }
    }

    /// Runs `f` on a thread of its own and fails the test if it takes more than 30 s (a search
    /// that hangs or crawls would otherwise stall the whole test run).
    fn within<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)));
        });
        match rx.recv_timeout(Duration::from_secs(30)) {
            Ok(Ok(v)) => v,
            Ok(Err(p)) => std::panic::resume_unwind(p),
            Err(_) => panic!("timed out"),
        }
    }

    #[test]
    fn long_match_cut_at_the_span_end_is_found_once() {
        // `a+` over 300 `a`s with 64-byte windows and a 16-byte overlap: the match is cut at the
        // span end and extended. In UTF-16 the extension used to fail, and with the grid the
        // search then went on one character at a time.
        let g = Geom {
            window: 64,
            regex_overlap: 16,
            max_match: 1 << 20,
        };
        let utf16 = Codec::new(Encoding::Utf16Le, NlByte::Lf, 2);
        let cp1251 = Codec::new(Encoding::Windows1251, NlByte::Lf, 0);
        for codec in [Codec::utf8(), utf16, cp1251] {
            for lead in ["xx", "x", ""] {
                within(move || {
                    let mut data = if codec.enc.is_utf16() {
                        vec![0xFF, 0xFE]
                    } else {
                        Vec::new()
                    };
                    let bom = data.len() as u64;
                    let text = format!("{lead}{}\nyy", "a".repeat(300));
                    data.extend(codec.encode(&text));
                    let (_f, t) = buf_with(&data, codec);
                    let m = Matcher::new(&rq("a+"), codec).unwrap();
                    let unit = codec.unit() as u64;
                    let s = bom + lead.len() as u64 * unit;
                    let want = vec![(s, s + 300 * unit)];
                    let cfg = ParCfg {
                        in_flight: 3,
                        queued: 2,
                        batch: 1,
                        ..ParCfg::new(3)
                    };
                    let (seq, r1) = run_with(&t, &m, g, 0..t.len(), None, None);
                    let (par, r2) = run_with(&t, &m, g, 0..t.len(), Some(cfg), None);
                    assert_eq!((r1, r2), (SearchEnd::Completed, SearchEnd::Completed));
                    assert_eq!(seq, want, "{:?} {lead:?}", codec.enc);
                    assert_eq!(par, want, "{:?} {lead:?}", codec.enc);
                    assert_eq!(legacy(&t, &m, g, 0..t.len()), want);
                });
            }
        }
    }

    #[test]
    fn extended_matches_agree_with_moving_windows() {
        // Matches far longer than the overlap, so they are cut at the span end and extended:
        // the grid, the moving windows and the parallel search all find the same ones.
        let mut rng = Rng(0xD1B5_4A32_D192_ED03);
        // Repeats of one ASCII character: a match cut at the span end ends right there, which
        // is what sends it to be extended. A match that grows in longer steps (`(?:ab)+`, or
        // `\w+` over multi-byte characters) can be cut short of the span end and is reported
        // short, at the grid edges now as at the moving ones before; with the real 1 MB
        // overlap that takes a match longer than 1 MB.
        let long = ["a+", "[ab]+", "x+", "[ab ]+", "[a-z ]{3,}", "(?i)A+"];
        let mut extended = 0;
        for round in 0..crate::robustness::rounds(60) {
            let (_f, t, name) = random_doc(&mut rng);
            let g = Geom {
                window: rng.pick(&[8, 16, 24, 64, 100]),
                // At least the shortest match (three UTF-16 units), or one starting just
                // before an edge would not fit in the span at all.
                regex_overlap: rng.pick(&[6, 8, 16]),
                max_match: 1 << 20,
            };
            for pat in long {
                let Ok(m) = Matcher::new(&rq(pat), t.codec) else {
                    continue;
                };
                let cfg = random_cfg(&mut rng);
                let before = legacy(&t, &m, g, 0..t.len());
                let (seq, _) = run_with(&t, &m, g, 0..t.len(), None, None);
                let (par, _) = run_with(&t, &m, g, 0..t.len(), Some(cfg), None);
                assert_eq!(seq, before, "round {round} {name} {pat} {g:?}");
                assert_eq!(par, seq, "round {round} {name} {pat} {g:?} {cfg:?}");
                // Longer than a window and its overlap: found only by extending it.
                extended += seq
                    .iter()
                    .filter(|(s, e)| e - s > g.window + g.regex_overlap)
                    .count();
            }
        }
        assert!(extended > 100, "only {extended} extended matches");
    }

    type Matches = Vec<(u64, u64)>;

    /// The dense document, searched with an injected failure: what the search returned (`None`:
    /// it panicked), the matches it reported, and those of the sequential search.
    fn with_fault(fault: Fault) -> (Option<SearchEnd>, Matches, Matches) {
        within(move || {
            let (_f, t) = dense_doc();
            let m = Matcher::new(&rq(r"\w+"), Codec::utf8()).unwrap();
            let (seq, _) = run_with(&t, &m, SMALL, 0..t.len(), None, None);
            let cancel = CancelToken::new();
            let scan = Scan::new(&t, &m, SMALL, 0..t.len(), &cancel, None);
            let cfg = ParCfg {
                queued: 16,
                batch: 4,
                fault,
                ..ParCfg::new(4)
            };
            let mut got = Vec::new();
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                scan.run_par(cfg, &mut |s, e, _, _| {
                    got.push((s, e));
                    true
                })
            }));
            (r.ok(), got, seq)
        })
    }

    #[test]
    fn failed_spawns_fall_back_to_fewer_threads() {
        for from in [0, 1, 3] {
            let (r, got, seq) = with_fault(Fault::SpawnFailsFrom(from));
            assert_eq!(r, Some(SearchEnd::Completed));
            assert_eq!(got, seq, "spawn fails from {from}");
        }
    }

    #[test]
    fn a_worker_panic_fails_the_search_instead_of_hanging() {
        for k in [0, 1, 7, 150] {
            for searching in [false, true] {
                let (r, got, seq) = with_fault(Fault::PanicAt(k, searching));
                assert_eq!(r, None, "window {k}: the panic did not reach the caller");
                assert_eq!(got, seq[..got.len()]);
            }
        }
    }
}
