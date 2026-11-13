//! Alerts on new lines: the complete lines added to a followed log since the last look, kept
//! when one of some patterns matches them. Only the added bytes are read, with the same search
//! as the filter, so a log that grows by megabytes a second costs little.

use std::collections::BTreeMap;

use crate::filter::{self, FilterSpec};
use crate::jobs::CancelToken;
use crate::search::Matcher;
use crate::text::TextBuf;

/// Bytes of a line kept as its text.
const TEXT_MAX: u64 = 2000;

/// A line that matched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub offset: u64,
    pub text: String,
    /// The first pattern that matched it.
    pub pattern: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Scan {
    /// Where the next look starts: after the last complete line (a line still being written
    /// waits for its line break).
    pub next: u64,
    /// Lines that matched.
    pub count: u64,
    /// The last `keep` of them, in order.
    pub hits: Vec<Hit>,
}

/// Looks at the complete lines from `from` (a line start) on. `None` when cancelled or the file
/// could not be read.
pub fn scan(buf: &TextBuf, matchers: &[Matcher], from: u64, keep: usize, cancel: &CancelToken) -> Option<Scan> {
    let to = filter::line_start_at(buf, buf.len())?;
    if to <= from {
        return Some(Scan { next: from, ..Default::default() });
    }
    // Line start → (end, pattern); a line matched by several patterns counts once.
    let mut lines: BTreeMap<u64, (u64, usize)> = BTreeMap::new();
    for (i, m) in matchers.iter().enumerate() {
        let spec = FilterSpec { range: Some((from, to)), ..Default::default() };
        filter::filter_lines(buf, m, spec, cancel, None, |ls, le, _| {
            lines.entry(ls).or_insert((le, i));
            true
        });
        if cancel.is_cancelled() {
            return None;
        }
    }
    let count = lines.len() as u64;
    let skip = lines.len().saturating_sub(keep);
    let mut hits = Vec::with_capacity(lines.len() - skip);
    for (&ls, &(le, pattern)) in lines.iter().skip(skip) {
        let start = ls.max(buf.codec.bom_len as u64);
        let b = buf.read(start..le.min(start + TEXT_MAX))?;
        let text = buf.codec.decode_lossy(&b);
        hits.push(Hit { offset: ls, text: text.trim_end_matches(['\r', '\n']).to_string(), pattern });
    }
    Some(Scan { next: to, count, hits })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding;
    use crate::file::{OpenMode, Source};
    use crate::index::LineIndex;
    use crate::search::Query;
    use std::sync::Arc;

    fn buf(bytes: &[u8]) -> (tempfile::TempDir, TextBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.log");
        std::fs::write(&path, bytes).unwrap();
        let src = Arc::new(Source::open(&path, OpenMode::Auto).unwrap());
        let det = encoding::detect(&src.read(0..src.len()).unwrap(), true);
        let len = src.len();
        (dir, TextBuf::new(src, LineIndex::new(len), det.codec))
    }

    fn matcher(p: &str, regex: bool) -> Matcher {
        Matcher::new(&Query { pattern: p.into(), regex, case_sensitive: true, ..Default::default() }, encoding::Codec::utf8()).unwrap()
    }

    #[test]
    fn new_complete_lines_only() {
        let text = b"INFO start\nERROR old\nINFO ok\nERROR boom\nWARN disk 91%\nERROR half";
        let (_d, b) = buf(text);
        let from = 21; // after "ERROR old\n"
        let s = scan(&b, &[matcher("ERROR", false), matcher(r"disk \d+%", true)], from, 10, &CancelToken::new()).unwrap();
        assert_eq!(s.count, 2);
        assert_eq!(s.hits, [Hit { offset: 29, text: "ERROR boom".into(), pattern: 0 }, Hit { offset: 40, text: "WARN disk 91%".into(), pattern: 1 }]);
        // The unfinished last line waits.
        assert_eq!(s.next, text.len() as u64 - "ERROR half".len() as u64);
        assert_eq!(scan(&b, &[matcher("ERROR", false)], s.next, 10, &CancelToken::new()).unwrap(), Scan { next: s.next, ..Default::default() });
    }

    #[test]
    fn a_line_counts_once_and_only_the_last_are_kept() {
        let mut text = String::new();
        for i in 0..50 {
            text.push_str(&format!("ERROR fatal {i}\r\n"));
        }
        let (_d, b) = buf(text.as_bytes());
        let s = scan(&b, &[matcher("ERROR", false), matcher("fatal", false)], 0, 3, &CancelToken::new()).unwrap();
        assert_eq!((s.count, s.next), (50, text.len() as u64));
        assert_eq!(s.hits.iter().map(|h| h.text.as_str()).collect::<Vec<_>>(), ["ERROR fatal 47", "ERROR fatal 48", "ERROR fatal 49"]);
        assert!(s.hits.iter().all(|h| h.pattern == 0));
    }
}
