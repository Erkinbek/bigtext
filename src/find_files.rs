//! Find in files: the matches of a search in every file of a folder whose name fits a pattern
//! such as `*.log`, file by file with the streaming search, with line numbers and the text
//! around each match.

use std::io;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::encoding;
use crate::file::{OpenMode, Source};
use crate::filter::{line_end_at, line_start_at};
use crate::index::LineIndex;
use crate::jobs::CancelToken;
use crate::search::{self, Matcher, SearchEnd};
use crate::text::TextBuf;

/// Files listed at most.
pub const MAX_FILES: usize = 100_000;
/// Characters of the line shown before and after a match.
const CONTEXT: u64 = 160;

/// A match: its line (1-based), byte range, and the line's text around it with the match's
/// byte range in that text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub line: u64,
    pub start: u64,
    pub end: u64,
    pub text: String,
    pub mark: (usize, usize),
}

/// `*` and `?` wildcards, ignoring ASCII case.
pub fn wildcard(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.chars().map(|c| c.to_ascii_lowercase()).collect();
    let n: Vec<char> = name.chars().map(|c| c.to_ascii_lowercase()).collect();
    let (mut pi, mut ni) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ni));
            pi += 1;
        } else if let Some((sp, sn)) = star {
            pi = sp + 1;
            ni = sn + 1;
            star = Some((sp, sn + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

/// Patterns from text like `*.log; *.txt` (an empty text matches every file).
pub fn patterns(text: &str) -> Vec<String> {
    text.split([';', ',', ' ']).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect()
}

/// The files under `dir` (in sub-folders too with `recursive`, skipping hidden ones like `.git`)
/// whose names fit one of `patterns`, sorted by path.
pub fn list_files(dir: &Path, patterns: &[String], recursive: bool, cancel: &CancelToken) -> io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut dirs = vec![dir.to_path_buf()];
    let mut first = true;
    while let Some(d) = dirs.pop() {
        if cancel.is_cancelled() || out.len() >= MAX_FILES {
            break;
        }
        let entries = match std::fs::read_dir(&d) {
            Ok(e) => e,
            // An unreadable sub-folder is skipped; the folder asked for must be readable.
            Err(e) if first => return Err(e),
            Err(_) => continue,
        };
        first = false;
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Ok(t) = e.file_type() else { continue };
            if t.is_dir() {
                if recursive && !name.starts_with('.') {
                    dirs.push(e.path());
                }
            } else if t.is_file() && (patterns.is_empty() || patterns.iter().any(|p| wildcard(p, &name))) {
                out.push(e.path());
                if out.len() >= MAX_FILES {
                    break;
                }
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Opens a file for searching: its text with the detected encoding, or `None` for a binary file.
pub fn open_text(path: &Path) -> io::Result<Option<TextBuf>> {
    let src = Source::open(path, OpenMode::Auto)?;
    let n = src.len().min(1 << 20);
    let det = encoding::detect(&src.read(0..n)?, n == src.len());
    if det.looks_binary {
        return Ok(None);
    }
    let len = src.len();
    Ok(Some(TextBuf::new(Arc::new(src), LineIndex::new(len), det.codec).snapshot()))
}

/// Calls `on_hit` for every match of `m` in `buf`, in order, until it breaks.
pub fn search_text(buf: &TextBuf, m: &Matcher, cancel: &CancelToken, mut on_hit: impl FnMut(Hit) -> ControlFlow<()>) -> SearchEnd {
    let codec = buf.codec;
    let mut line = 1u64;
    let mut counted = 0u64;
    search::search_forward(buf, m, 0..buf.len(), cancel, None, |s, e| {
        // Lines before the match.
        let _ = buf.for_each_chunk(counted..s, |base, chunk| {
            line += codec.count_nl(chunk, base);
            true
        });
        counted = s;
        let ls = line_start_at(buf, s).unwrap_or(s).max(codec.bom_len as u64);
        let le = line_end_at(buf, e.max(s)).unwrap_or(e);
        let from = buf.snap_char(s.saturating_sub(CONTEXT * codec.unit() as u64).max(ls));
        let to = buf.snap_char((e + CONTEXT * codec.unit() as u64).min(le));
        let decode = |a: u64, b: u64| -> String {
            let raw = buf.read(a..b.max(a)).map(|b| codec.decode_lossy(&b)).unwrap_or_default();
            raw.chars().map(|c| if c == '\t' { ' ' } else if c.is_control() { '·' } else { c }).collect()
        };
        let mut text = String::new();
        if from > ls {
            text.push('…');
        }
        text.push_str(&decode(from, s));
        let ms = text.len();
        text.push_str(&decode(s, e.min(to)));
        let me = text.len();
        let tail = decode(e.min(to), to);
        text.push_str(tail.trim_end_matches(['·', ' ']));
        if to < le && !tail.is_empty() {
            text.push('…');
        }
        on_hit(Hit { line, start: s, end: e, text, mark: (ms, me) }).is_continue()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::Query;

    #[test]
    fn wildcards() {
        assert!(wildcard("*.log", "App.LOG"));
        assert!(wildcard("*.log", ".log"));
        assert!(!wildcard("*.log", "app.log.gz"));
        assert!(wildcard("app-??.txt", "app-01.txt"));
        assert!(!wildcard("app-??.txt", "app-1.txt"));
        assert!(wildcard("*", "anything"));
        assert!(wildcard("a*b*c", "aXXbYYc"));
        assert!(!wildcard("a*b*c", "aXXbYY"));
        assert_eq!(patterns(" *.log; *.txt,x  "), vec!["*.log", "*.txt", "x"]);
        assert!(patterns("").is_empty());
    }

    #[test]
    fn lists_and_searches_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.log"), "one\ntwo ERROR here\nthree\n\tERROR again\n").unwrap();
        std::fs::write(root.join("b.txt"), "ERROR in txt\n").unwrap();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub").join("c.log"), "x\ny\nz ERROR\n").unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git").join("d.log"), "ERROR\n").unwrap();
        std::fs::write(root.join("bin.log"), b"ERROR\0\0\0\x01\x02binary").unwrap();
        let c = CancelToken::new();
        let flat = list_files(root, &patterns("*.log"), false, &c).unwrap();
        assert_eq!(flat.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect::<Vec<_>>(), vec!["a.log", "bin.log"]);
        let deep = list_files(root, &patterns("*.log"), true, &c).unwrap();
        assert_eq!(deep.len(), 3);
        assert!(open_text(&root.join("bin.log")).unwrap().is_none());

        let buf = open_text(&root.join("a.log")).unwrap().unwrap();
        let m = Matcher::new(&Query { pattern: "ERROR".into(), case_sensitive: true, ..Default::default() }, buf.codec).unwrap();
        let mut hits = Vec::new();
        search_text(&buf, &m, &c, |h| {
            hits.push(h);
            ControlFlow::Continue(())
        });
        assert_eq!(hits.len(), 2);
        assert_eq!((hits[0].line, hits[0].start, hits[0].end, hits[0].text.as_str()), (2, 8, 13, "two ERROR here"));
        assert_eq!(&hits[0].text[hits[0].mark.0..hits[0].mark.1], "ERROR");
        assert_eq!((hits[1].line, hits[1].text.as_str()), (4, " ERROR again"));
        assert!(list_files(&root.join("missing"), &[], false, &c).is_err());
    }

    #[test]
    fn long_lines_are_cut_around_the_match() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("long.txt");
        std::fs::write(&path, format!("{}NEEDLE{}\nnext", "a".repeat(1000), "b".repeat(1000))).unwrap();
        let buf = open_text(&path).unwrap().unwrap();
        let m = Matcher::new(&Query { pattern: "NEEDLE".into(), case_sensitive: true, ..Default::default() }, buf.codec).unwrap();
        let mut hits = Vec::new();
        search_text(&buf, &m, &CancelToken::new(), |h| {
            hits.push(h);
            ControlFlow::Break(())
        });
        let h = &hits[0];
        assert_eq!(h.line, 1);
        assert!(h.text.starts_with('…') && h.text.ends_with('…'));
        assert_eq!(&h.text[h.mark.0..h.mark.1], "NEEDLE");
        assert_eq!(h.text.chars().count(), 1 + 160 + 6 + 160 + 1);
    }
}
