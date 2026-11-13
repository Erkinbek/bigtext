//! The highlighting state at the start of each row on screen.
//!
//! A row's state comes from the row above it (its text scanned), and is remembered per row
//! offset until the document changes, so scrolling scans each row once. After a jump the state
//! is found by reading at most [`LOOKBACK`] bytes above the row: the file is never read from its
//! start, so a comment opened further up than that is not seen.

use std::collections::HashMap;

use bigtext::text::rows::{self, Pending, Row};
use bigtext::text::TextBuf;

use super::highlight::{self, Lang, State};

/// How far above a row highlighting reads to learn what the row starts inside of.
pub const LOOKBACK: u64 = 64 * 1024;

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
                _ => state_at(buf, lang, row.start),
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
    fn states_follow_rows_and_jumps() {
        let text = "SELECT 1;\n/* one\ntwo\nthree */\nSELECT 2;\n";
        let (_f, b) = buf(text);
        let rows: Vec<Row> = super::super::text_view::layout_rows(&b, 0, 6, None).unwrap();
        let mut cache = HlCache::default();
        let st = row_states(&mut cache, &b, 1, Lang::Sql, &rows);
        let inside: Vec<bool> = st.iter().map(|s| s.in_comment()).collect();
        assert_eq!(inside[..5], [false, false, true, true, false]);
        // A jump straight to "three */" reads the lines above it.
        let at = text.find("three").unwrap() as u64;
        assert!(state_at(&b, Lang::Sql, at).unwrap().in_comment());
        assert!(cache.get(at).is_some_and(|s| s.in_comment()));
        // Wrapped rows of one line carry the state too.
        let rows = super::super::text_view::layout_rows(&b, 0, 8, Some(4)).unwrap();
        let st = row_states(&mut HlCache::default(), &b, 1, Lang::Sql, &rows);
        let two = rows
            .iter()
            .position(|r| r.start == text.find("two").unwrap() as u64)
            .unwrap();
        assert!(st[two].in_comment() && st[two - 1].in_comment());
        assert!(row_states(&mut cache, &b, 1, Lang::Json, &rows).is_empty());
    }
}
