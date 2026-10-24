//! Replacement: templates (`$1`, `${name}`, `\n`, `\t`), small batches as piece-table edits and
//! the streaming replace-all that writes a new file with bounded memory.

use std::io::{self, Write};
use std::ops::Range;

use crate::jobs::{CancelToken, Progress};
use crate::search::{self, Matcher, SearchEnd};
use crate::text::TextBuf;

/// Replacement text with escape sequences resolved.
#[derive(Clone, Debug)]
pub struct Template {
    text: String,
    /// `$` references are interpolated (regex mode only).
    interpolate: bool,
}

/// Resolves `\n`, `\r`, `\t`, `\0` and `\\`; other backslash sequences are kept verbatim.
pub fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.peek().copied() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('0') => out.push('\0'),
            Some('\\') => out.push('\\'),
            _ => {
                out.push('\\');
                continue;
            }
        }
        it.next();
    }
    out
}

impl Template {
    pub fn new(replacement: &str, regex_mode: bool) -> Template {
        let text = unescape(replacement);
        let interpolate = regex_mode && text.contains('$');
        Template { text, interpolate }
    }

    /// Replacement bytes (in the document's encoding) for the match `s..e`.
    pub fn expand(&self, buf: &TextBuf, m: &Matcher, s: u64, e: u64) -> Vec<u8> {
        let codec = m.codec();
        if !self.interpolate {
            return codec.encode(&self.text);
        }
        match search::captures_at(buf, m, s, e) {
            Some((hay, caps, _)) => {
                let mut dst = Vec::new();
                caps.interpolate_bytes_into(&hay, self.text.as_bytes(), &mut dst);
                if codec.enc == crate::encoding::Encoding::Utf8 {
                    dst
                } else {
                    codec.encode(&String::from_utf8_lossy(&dst))
                }
            }
            None => codec.encode(&self.text),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReplaceStats {
    pub count: u64,
    pub bytes_in: u64,
    pub bytes_out: u64,
}

/// Outcome of collecting replacements for in-memory application.
pub enum Collected {
    /// Few enough matches: apply as piece-table edits (undoable).
    Edits(Vec<(Range<u64>, Vec<u8>)>),
    /// More than `threshold` matches: use the streaming path.
    TooMany,
    Cancelled,
}

/// Collects up to `threshold` replacements in `range`.
pub fn collect(buf: &TextBuf, m: &Matcher, tpl: &Template, range: Range<u64>, threshold: usize, cancel: &CancelToken, progress: Option<&Progress>) -> Collected {
    let mut edits = Vec::new();
    let mut too_many = false;
    let end = search::search_forward(buf, m, range, cancel, progress, |s, e| {
        if edits.len() >= threshold {
            too_many = true;
            return false;
        }
        edits.push((s..e, tpl.expand(buf, m, s, e)));
        true
    });
    match end {
        SearchEnd::Cancelled | SearchEnd::Unavailable => Collected::Cancelled,
        _ if too_many => Collected::TooMany,
        _ => Collected::Edits(edits),
    }
}

/// First `n` replacements as (offset, before, after) for preview.
pub fn preview(buf: &TextBuf, m: &Matcher, tpl: &Template, range: Range<u64>, n: usize, cancel: &CancelToken) -> Vec<(u64, String, String)> {
    let codec = buf.codec;
    let mut out = Vec::new();
    search::search_forward(buf, m, range, cancel, None, |s, e| {
        let before = buf.read(s..e.min(s + 200)).map(|b| codec.decode_lossy(&b)).unwrap_or_default();
        let after = codec.decode_lossy(&tpl.expand(buf, m, s, e));
        out.push((s, before, after));
        out.len() < n
    });
    out
}

/// Streams the whole document to `out`, replacing every match in `range`.
/// Returns `Ok(None)` if cancelled.
pub fn replace_all_to<W: Write>(
    buf: &TextBuf,
    m: &Matcher,
    tpl: &Template,
    range: Range<u64>,
    out: &mut W,
    cancel: &CancelToken,
    progress: Option<&Progress>,
) -> io::Result<Option<ReplaceStats>> {
    let mut stats = ReplaceStats { bytes_in: buf.len(), ..Default::default() };
    let mut pos = 0u64;
    let mut err: Option<io::Error> = None;
    let copy = |from: u64, to: u64, out: &mut W, stats: &mut ReplaceStats| -> io::Result<()> {
        let mut res = Ok(());
        let ok = buf.for_each_chunk(from..to, |_, b| match out.write_all(b) {
            Ok(()) => {
                stats.bytes_out += b.len() as u64;
                true
            }
            Err(e) => {
                res = Err(e);
                false
            }
        });
        if ok.is_none() {
            return Err(io::Error::other("could not read the document"));
        }
        res
    };
    let end = search::search_forward(buf, m, range, cancel, progress, |s, e| {
        let r = copy(pos, s, out, &mut stats).and_then(|_| {
            let rep = tpl.expand(buf, m, s, e);
            stats.bytes_out += rep.len() as u64;
            out.write_all(&rep)
        });
        if let Err(e) = r {
            err = Some(e);
            return false;
        }
        stats.count += 1;
        pos = e;
        true
    });
    if let Some(e) = err {
        return Err(e);
    }
    match end {
        SearchEnd::Cancelled => return Ok(None),
        SearchEnd::Unavailable => return Err(io::Error::other("could not read the document")),
        _ => {}
    }
    let len = buf.len();
    copy(pos, len, out, &mut stats)?;
    Ok(Some(stats))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::Codec;
    use crate::file::{OpenMode, Source};
    use crate::index;
    use crate::search::Query;
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

    fn run(data: &[u8], pattern: &str, regex: bool, rep: &str) -> String {
        let (_f, t) = buf_with(data);
        let q = Query { pattern: pattern.into(), regex, case_sensitive: true, ..Default::default() };
        let m = Matcher::new(&q, Codec::utf8()).unwrap();
        let tpl = Template::new(rep, regex);
        let mut out = Vec::new();
        replace_all_to(&t, &m, &tpl, 0..t.len(), &mut out, &CancelToken::new(), None).unwrap().unwrap();
        // The batch path must agree with the streaming path.
        if let Collected::Edits(edits) = collect(&t, &m, &tpl, 0..t.len(), 1_000_000, &CancelToken::new(), None) {
            let (pieces, add) = t.build_batch(&edits);
            let mut t2 = t.clone();
            t2.apply_batch(pieces, &add);
            assert_eq!(t2.read(0..t2.len()).unwrap().as_ref(), out.as_slice());
        } else {
            panic!("expected edits");
        }
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn literal_replace() {
        assert_eq!(run(b"a-b-c", "-", false, "+"), "a+b+c");
        assert_eq!(run(b"a-b-c", "-", false, "$1"), "a$1b$1c");
    }

    #[test]
    fn capture_groups() {
        assert_eq!(run(b"2026-10-24", r"(\d+)-(\d+)-(\d+)", true, "$3.$2.$1"), "24.10.2026");
        assert_eq!(run(b"key=value", r"(?P<k>\w+)=(?P<v>\w+)", true, "${v}=${k}"), "value=key");
    }

    #[test]
    fn escapes() {
        assert_eq!(run(b"a,b,c", ",", false, "\\n"), "a\nb\nc");
        assert_eq!(run(b"a,b", ",", false, "\\t\\\\"), "a\t\\b");
    }

    #[test]
    fn empty_matches_grow_file() {
        assert_eq!(run(b"ab\ncd\n", "^", true, "> "), "> ab\n> cd\n> ");
        assert_eq!(run(b"abc", "x*", true, "-"), "-a-b-c-");
    }

    #[test]
    fn shrink_to_nothing() {
        assert_eq!(run(b"xxxx", "x", false, ""), "");
    }

    #[test]
    fn threshold_switches_to_streaming() {
        let (_f, t) = buf_with(&b"a ".repeat(1000));
        let q = Query { pattern: "a".into(), case_sensitive: true, ..Default::default() };
        let m = Matcher::new(&q, Codec::utf8()).unwrap();
        let tpl = Template::new("b", false);
        assert!(matches!(collect(&t, &m, &tpl, 0..t.len(), 10, &CancelToken::new(), None), Collected::TooMany));
    }

    #[test]
    fn cancel_leaves_no_result() {
        let (_f, t) = buf_with(b"aaaa");
        let q = Query { pattern: "a".into(), case_sensitive: true, ..Default::default() };
        let m = Matcher::new(&q, Codec::utf8()).unwrap();
        let c = CancelToken::new();
        c.cancel();
        let mut out = Vec::new();
        assert!(replace_all_to(&t, &m, &Template::new("b", false), 0..4, &mut out, &c, None).unwrap().is_none());
    }
}
