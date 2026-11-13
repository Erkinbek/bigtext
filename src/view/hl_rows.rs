//! The highlighting state at the start of each row on screen.
//!
//! A row's state comes from the row above it (its text scanned), and is remembered per row
//! offset until the document changes, so scrolling scans each row once. After a jump the state
//! comes from the nearest checkpoint of an [`HlIndex`] (one pass from the start of the file, in
//! the background) where that pass has got to, and otherwise by reading at most [`LOOKBACK`]
//! bytes above the row, which misses a comment or string opened further up.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bigtext::jobs::CancelToken;
use bigtext::text::rows::{self, Pending, Row};
use bigtext::text::TextBuf;

use super::highlight::{self, Lang, State};

/// How far above a row highlighting reads to learn what the row starts inside of.
pub const LOOKBACK: u64 = 64 * 1024;

/// Bytes between the checkpoints of an [`HlIndex`].
pub const CHECKPOINT: u64 = 64 * 1024;

/// How often the pass publishes what it has read, in bytes.
const PUBLISH_EVERY: u64 = 32 << 20;

/// The highlighting state at the first segment start at or after every 64 KB of the document,
/// from one pass that scans it from the start, so that a row far below the place where a
/// comment or string opened still starts inside it. About 40 bytes per 64 KB.
#[derive(Clone, Default)]
pub struct HlIndex {
    /// For block `k`: the first segment start at or after `k * CHECKPOINT`, and the state there.
    points: Vec<(u64, State)>,
    /// The pass reached the end: the last checkpoint holds up to it.
    pub done: bool,
}

impl HlIndex {
    /// The nearest checkpoint at or before `at`, if the pass has got that far.
    fn before(&self, at: u64) -> Option<(u64, State)> {
        let k = (at / CHECKPOINT) as usize;
        if k >= self.points.len() && !self.done {
            return None;
        }
        let k = k.min(self.points.len().checked_sub(1)?);
        self.points[..=k]
            .iter()
            .rev()
            .take(3)
            .find(|(p, _)| *p <= at)
            .copied()
    }
}

/// Scans the whole document from its start in `lang` and fills the checkpoints in `out`, publishing
/// them as it goes (every 32 MB and at the end, calling `notify`).
pub fn build_index(
    buf: &TextBuf,
    lang: Lang,
    cancel: &CancelToken,
    out: &Mutex<Arc<HlIndex>>,
    notify: &dyn Fn(),
) {
    let mut idx = HlIndex::default();
    let mut st = State::start(lang, true);
    let Ok(mut seg) = rows::seg_at(buf, 0) else {
        return;
    };
    let mut published = 0;
    loop {
        while (idx.points.len() as u64) * CHECKPOINT <= seg.start {
            idx.points.push((seg.start, st));
        }
        if cancel.is_cancelled() {
            return;
        }
        let Ok(text) = text_of(buf, seg.start, seg.content_end) else {
            return;
        };
        st = highlight::scan(lang, st, &text, seg.has_nl);
        match rows::next_seg(buf, &seg) {
            Ok(Some(s)) => seg = s,
            Ok(None) => break,
            Err(_) => return,
        }
        if seg.start - published >= PUBLISH_EVERY {
            published = seg.start;
            *out.lock().unwrap() = Arc::new(idx.clone());
            notify();
        }
    }
    idx.done = true;
    *out.lock().unwrap() = Arc::new(idx);
    notify();
}

/// Remembered row states, for one document version and language.
#[derive(Default)]
pub struct HlCache {
    version: u64,
    lang: Option<Lang>,
    states: HashMap<u64, State>,
}

impl HlCache {
    /// Rows whose state is remembered (tests read it).
    pub fn get(&self, row_start: u64) -> Option<State> {
        self.states.get(&row_start).copied()
    }
}

/// The state at the start of each of `rows` (the rows on screen, in order; folds may skip lines).
/// Rows whose text is not loaded yet get the default state and are not remembered.
pub fn row_states(
    cache: &mut HlCache,
    buf: &TextBuf,
    version: u64,
    lang: Lang,
    rows: &[Row],
    idx: Option<&HlIndex>,
) -> Vec<State> {
    if !lang.stateful() {
        return Vec::new();
    }
    if cache.version != version || cache.lang != Some(lang) || cache.states.len() > 100_000 {
        cache.states.clear();
        cache.version = version;
        cache.lang = Some(lang);
    }
    let mut out = Vec::with_capacity(rows.len());
    let mut prev: Option<(Row, State)> = None;
    for row in rows {
        let st = match cache.states.get(&row.start) {
            Some(s) => Ok(*s),
            None => match prev {
                Some((p, ps)) if follows(&p, row) => after_row(buf, lang, &p, ps),
                _ => state_at_with(buf, lang, row.start, idx),
            },
        };
        let Ok(st) = st else {
            out.resize(rows.len(), State::default());
            break;
        };
        cache.states.insert(row.start, st);
        out.push(st);
        prev = Some((*row, st));
    }
    out
}

/// True if `row` comes right after `p` (no fold between them).
fn follows(p: &Row, row: &Row) -> bool {
    if p.is_last_in_seg() {
        p.seg.next == row.start
    } else {
        p.end == row.start
    }
}

fn text_of(buf: &TextBuf, from: u64, to: u64) -> Result<String, Pending> {
    let bytes = buf.read(from..to).ok_or(Pending)?;
    let mut s = buf.codec.decode_lossy(&bytes);
    if from == 0 && s.starts_with('\u{FEFF}') {
        s.drain(..3);
    }
    Ok(s)
}

/// The state after row `p`, which starts in state `st`.
fn after_row(buf: &TextBuf, lang: Lang, p: &Row, st: State) -> Result<State, Pending> {
    let text = text_of(buf, p.start, p.end)?;
    Ok(highlight::scan(
        lang,
        st,
        &text,
        p.is_last_in_seg() && p.seg.has_nl,
    ))
}

/// The state at offset `at`: scanned from the nearest checkpoint of `idx` when its pass has got
/// that far (at most a few blocks), otherwise as [`state_at`] does.
pub fn state_at_with(
    buf: &TextBuf,
    lang: Lang,
    at: u64,
    idx: Option<&HlIndex>,
) -> Result<State, Pending> {
    let Some((p, mut st)) = idx.and_then(|i| i.before(at)).filter(|_| lang.stateful()) else {
        return state_at(buf, lang, at);
    };
    let mut seg = rows::seg_at(buf, p)?;
    while seg.start < at {
        let end = seg.content_end.min(at);
        let text = text_of(buf, seg.start, end)?;
        st = highlight::scan(lang, st, &text, end == seg.content_end && seg.has_nl);
        if end < seg.content_end {
            break;
        }
        match rows::next_seg(buf, &seg)? {
            Some(s) => seg = s,
            None => break,
        }
    }
    Ok(st)
}

/// The state at offset `at`, read from at most [`LOOKBACK`] bytes above it (from the file start
/// when that is closer). Unless that reaches the file start, reading starts over at the first
/// line that begins a declaration (`class Foo`, `#include`), which is surely outside comments and
/// strings, where the 64 KB boundary may not be.
pub fn state_at(buf: &TextBuf, lang: Lang, at: u64) -> Result<State, Pending> {
    if at == 0 || !lang.stateful() {
        return Ok(State::start(lang, true));
    }
    let mut seg = rows::seg_containing(buf, at.saturating_sub(LOOKBACK))?;
    let mut synced = seg.start == 0;
    let mut st = State::start(lang, synced);
    while seg.start < at {
        let end = seg.content_end.min(at);
        let text = text_of(buf, seg.start, end)?;
        if !synced && seg.line_start && lang.sync_line(&text) {
            st = State::default();
            synced = true;
        }
        st = highlight::scan(lang, st, &text, end == seg.content_end && seg.has_nl);
        if end < seg.content_end {
            break;
        }
        match rows::next_seg(buf, &seg)? {
            Some(s) => seg = s,
            None => break,
        }
    }
    Ok(st)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bigtext::encoding::Codec;
    use bigtext::file::{OpenMode, Source};
    use bigtext::index::LineIndex;
    use std::sync::Arc;

    fn buf(text: &str) -> (tempfile::NamedTempFile, TextBuf) {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut f, text.as_bytes()).unwrap();
        let s = Arc::new(Source::open(f.path(), OpenMode::Auto).unwrap());
        let b = TextBuf::new(s.clone(), LineIndex::new(s.len()), Codec::utf8());
        (f, b)
    }

    #[test]
    fn checkpoints_see_a_comment_opened_far_above() {
        let mut text = String::from("/* opened here\n");
        while text.len() < 200_000 {
            text.push_str(" just comment text, no closing mark\n");
        }
        text.push_str("*/\nSELECT 1;\n");
        let (_f, b) = buf(&text);
        let out = Mutex::new(Arc::new(HlIndex::default()));
        build_index(&b, Lang::Sql, &CancelToken::new(), &out, &|| {});
        let idx = out.lock().unwrap().clone();
        assert!(idx.done);
        let deep = text[..150_000].rfind('\n').unwrap() as u64 + 1;
        assert!(state_at_with(&b, Lang::Sql, deep, Some(&idx))
            .unwrap()
            .in_comment());
        assert!(
            !state_at(&b, Lang::Sql, deep).unwrap().in_comment(),
            "64 KB above alone does not see it"
        );
        let after = text.rfind("SELECT").unwrap() as u64;
        assert!(!state_at_with(&b, Lang::Sql, after, Some(&idx))
            .unwrap()
            .in_comment());
        // A pass that has not got this far leaves the old way in charge.
        let partial = HlIndex {
            points: idx.points[..1].to_vec(),
            done: false,
        };
        assert!(partial.before(deep).is_none());
    }

    #[test]
    fn states_follow_rows_and_jumps() {
        let text = "SELECT 1;\n/* one\ntwo\nthree */\nSELECT 2;\n";
        let (_f, b) = buf(text);
        let rows: Vec<Row> = super::super::text_view::layout_rows(&b, 0, 6, None).unwrap();
        let mut cache = HlCache::default();
        let st = row_states(&mut cache, &b, 1, Lang::Sql, &rows, None);
        let inside: Vec<bool> = st.iter().map(|s| s.in_comment()).collect();
        assert_eq!(inside[..5], [false, false, true, true, false]);
        // A jump straight to "three */" reads the lines above it.
        let at = text.find("three").unwrap() as u64;
        assert!(state_at(&b, Lang::Sql, at).unwrap().in_comment());
        assert!(cache.get(at).is_some_and(|s| s.in_comment()));
        // Wrapped rows of one line carry the state too.
        let rows = super::super::text_view::layout_rows(&b, 0, 8, Some(4)).unwrap();
        let st = row_states(&mut HlCache::default(), &b, 1, Lang::Sql, &rows, None);
        let two = rows
            .iter()
            .position(|r| r.start == text.find("two").unwrap() as u64)
            .unwrap();
        assert!(st[two].in_comment() && st[two - 1].in_comment());
        assert!(row_states(&mut cache, &b, 1, Lang::Json, &rows, None).is_empty());
    }
}
