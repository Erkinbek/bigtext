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
    /// Replacement text used exactly as written: no escapes, no `$` references (the command
    /// line's plain-text mode, where `C:\new` is a path, not a line break).
    pub fn verbatim(replacement: &str) -> Template {
        Template {
            text: replacement.to_string(),
            interpolate: false,
        }
    }

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
pub fn collect(
    buf: &TextBuf,
    m: &Matcher,
    tpl: &Template,
    range: Range<u64>,
    threshold: usize,
    cancel: &CancelToken,
    progress: Option<&Progress>,
) -> Collected {
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
pub fn preview(
    buf: &TextBuf,
    m: &Matcher,
    tpl: &Template,
    range: Range<u64>,
    n: usize,
    cancel: &CancelToken,
) -> Vec<(u64, String, String)> {
    let codec = buf.codec;
    let mut out = Vec::new();
    search::search_forward(buf, m, range, cancel, None, |s, e| {
        let before = buf
            .read(s..e.min(s + 200))
            .map(|b| codec.decode_lossy(&b))
            .unwrap_or_default();
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
    let mut stats = ReplaceStats {
        bytes_in: buf.len(),
        ..Default::default()
    };
    let mut pos = 0u64;
    let mut err: Option<io::Error> = None;
    let mut scratch = Vec::new();
    // Copies [from, to) to `out`, straight from the search window when it holds those bytes.
    let mut copy = |from: u64,
                    to: u64,
                    hay: &[u8],
                    base: u64,
                    out: &mut W,
                    stats: &mut ReplaceStats|
     -> io::Result<()> {
        if from >= to {
            return Ok(());
        }
        stats.bytes_out += to - from;
        if from >= base && to <= base + hay.len() as u64 {
            return out.write_all(&hay[(from - base) as usize..(to - base) as usize]);
        }
        const STEP: u64 = 4 * 1024 * 1024;
        let mut p = from;
        while p < to {
            let e = (p + STEP).min(to);
            let n = buf
                .read_to(p..e, &mut scratch)
                .ok_or_else(|| io::Error::other("could not read the document"))?;
            out.write_all(&scratch[..n])?;
            p = e;
        }
        Ok(())
    };
    let end = search::search_forward_ctx(buf, m, range, cancel, progress, |s, e, hay, base| {
        let r = copy(pos, s, hay, base, out, &mut stats).and_then(|_| {
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
    copy(pos, len, &[], 0, out, &mut stats)?;
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
        buf_in(data, Codec::utf8())
    }

    fn buf_in(data: &[u8], codec: Codec) -> (tempfile::NamedTempFile, TextBuf) {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(data).unwrap();
        f.flush().unwrap();
        let src = Arc::new(Source::open(f.path(), OpenMode::Auto).unwrap());
        let idx = index::build_blocking(&src, codec);
        let mut t = TextBuf::new(src, idx, codec);
        t.blocking = true;
        (f, t)
    }

    fn replaced(t: &TextBuf, pattern: &str, regex: bool, tpl: &Template, r: Range<u64>) -> Vec<u8> {
        let q = Query {
            pattern: pattern.into(),
            regex,
            case_sensitive: true,
            ..Default::default()
        };
        let m = Matcher::new(&q, t.codec).unwrap();
        let mut out = Vec::new();
        replace_all_to(t, &m, tpl, r, &mut out, &CancelToken::new(), None)
            .unwrap()
            .unwrap();
        out
    }

    /// The byte order mark is not text: `^` matches at the start of line 1, after it, and
    /// nothing is written in front of it.
    #[test]
    fn a_byte_order_mark_stays_in_front() {
        let codec = Codec::new(
            crate::encoding::Encoding::Utf8,
            crate::encoding::NlByte::Lf,
            3,
        );
        let (_f, t) = buf_in(b"\xEF\xBB\xBFid,x\nb\n", codec);
        let tpl = Template::new("> ", true);
        assert_eq!(
            replaced(&t, "^", true, &tpl, 0..t.len()),
            b"\xEF\xBB\xBF> id,x\n> b\n> "
        );
        let tpl = Template::new("ID,", true);
        assert_eq!(
            replaced(&t, "^id,", true, &tpl, 0..t.len()),
            b"\xEF\xBB\xBFID,x\nb\n"
        );
    }

    /// In a selection that ends at the start of a line, that line is not part of it.
    #[test]
    fn no_empty_match_at_the_end_of_a_selection() {
        let (_f, t) = buf_with(b"a\nb\nc\n");
        let tpl = Template::new("// ", true);
        assert_eq!(replaced(&t, "^", true, &tpl, 0..4), b"// a\n// b\nc\n");
    }

    /// A UTF-16 file with an odd number of bytes (cut off) is searched to its end instead of
    /// looping on the half code unit.
    #[test]
    fn an_odd_length_utf16_file_is_searched_to_the_end() {
        let codec = Codec::new(
            crate::encoding::Encoding::Utf16Le,
            crate::encoding::NlByte::Lf,
            2,
        );
        let (_f, t) = buf_in(b"\xFF\xFEa\x00b", codec);
        let q = Query {
            pattern: "z".into(),
            ..Default::default()
        };
        let m = Matcher::new(&q, codec).unwrap();
        let cancel = CancelToken::new();
        let watchdog = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(10));
            watchdog.cancel();
        });
        let mut n = 0;
        let end = search::search_forward(&t, &m, 0..t.len(), &cancel, None, |_, _| {
            n += 1;
            true
        });
        assert_eq!((end, n), (SearchEnd::Completed, 0));
    }

    #[test]
    fn a_verbatim_replacement_keeps_backslashes_and_dollars() {
        let (_f, t) = buf_with(br"path C:\old here");
        let tpl = Template::verbatim(r"C:\new $1");
        assert_eq!(
            replaced(&t, r"C:\old", false, &tpl, 0..t.len()),
            br"path C:\new $1 here"
        );
    }

    fn run(data: &[u8], pattern: &str, regex: bool, rep: &str) -> String {
        let (_f, t) = buf_with(data);
        let q = Query {
            pattern: pattern.into(),
            regex,
            case_sensitive: true,
            ..Default::default()
        };
        let m = Matcher::new(&q, Codec::utf8()).unwrap();
        let tpl = Template::new(rep, regex);
        let mut out = Vec::new();
        replace_all_to(
            &t,
            &m,
            &tpl,
            0..t.len(),
            &mut out,
            &CancelToken::new(),
            None,
        )
        .unwrap()
        .unwrap();
        // The batch path must agree with the streaming path.
        if let Collected::Edits(edits) = collect(
            &t,
            &m,
            &tpl,
            0..t.len(),
            1_000_000,
            &CancelToken::new(),
            None,
        ) {
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
        assert_eq!(
            run(b"2026-10-24", r"(\d+)-(\d+)-(\d+)", true, "$3.$2.$1"),
            "24.10.2026"
        );
        assert_eq!(
            run(b"key=value", r"(?P<k>\w+)=(?P<v>\w+)", true, "${v}=${k}"),
            "value=key"
        );
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
        let q = Query {
            pattern: "a".into(),
            case_sensitive: true,
            ..Default::default()
        };
        let m = Matcher::new(&q, Codec::utf8()).unwrap();
        let tpl = Template::new("b", false);
        assert!(matches!(
            collect(&t, &m, &tpl, 0..t.len(), 10, &CancelToken::new(), None),
            Collected::TooMany
        ));
    }

    #[test]
    fn cancel_leaves_no_result() {
        let (_f, t) = buf_with(b"aaaa");
        let q = Query {
            pattern: "a".into(),
            case_sensitive: true,
            ..Default::default()
        };
        let m = Matcher::new(&q, Codec::utf8()).unwrap();
        let c = CancelToken::new();
        c.cancel();
        let mut out = Vec::new();
        assert!(
            replace_all_to(&t, &m, &Template::new("b", false), 0..4, &mut out, &c, None)
                .unwrap()
                .is_none()
        );
    }
}
