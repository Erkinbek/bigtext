//! Matching brackets and tags: the partner of `(`, `[`, `{` (and back), the bracket that encloses
//! a position, and for XML / HTML the closing tag of an opening one (and back).
//!
//! Brackets inside double-quoted strings do not count. Strings end at line breaks (as in JSON),
//! so scanning backwards stays correct: a quote is a string's end going backwards, unless it is
//! escaped with a backslash. Only for encodings where ASCII is one byte (UTF-8, code pages).

use crate::jobs::CancelToken;
use crate::text::TextBuf;

/// Bytes read at a time.
const CHUNK: u64 = 1 << 20;
/// Bytes of a tag read to parse it.
const TAG_MAX: u64 = 4096;

fn partner_of(b: u8) -> Option<(u8, bool)> {
    match b {
        b'(' => Some((b')', true)),
        b'[' => Some((b']', true)),
        b'{' => Some((b'}', true)),
        b')' => Some((b'(', false)),
        b']' => Some((b'[', false)),
        b'}' => Some((b'{', false)),
        _ => None,
    }
}

fn byte_at(buf: &TextBuf, off: u64) -> Option<u8> {
    (off < buf.len()).then(|| buf.read(off..off + 1).map(|b| b[0])).flatten()
}

/// The bracket at `off` or just before it (outside a string).
pub fn at_caret(buf: &TextBuf, off: u64) -> Option<u64> {
    if buf.codec.unit() != 1 {
        return None;
    }
    [Some(off), off.checked_sub(1)].into_iter().flatten().find(|&p| byte_at(buf, p).and_then(partner_of).is_some() && !in_string(buf, p))
}

/// Whether `off` is inside a double-quoted string of its line (lines longer than 64 KB before
/// `off` are taken as code).
fn in_string(buf: &TextBuf, off: u64) -> bool {
    let from = off.saturating_sub(64 * 1024);
    let Some(b) = buf.read(from..off) else { return false };
    let start = b.iter().rposition(|&c| c == b'\n').map(|i| i + 1).unwrap_or(0);
    let (mut s, mut esc) = (false, false);
    for &c in &b[start..] {
        if s {
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                s = false;
            }
        } else if c == b'"' {
            s = true;
        }
    }
    s
}

/// The partner of the bracket at `pos`, scanning at most `limit` bytes.
pub fn partner(buf: &TextBuf, pos: u64, limit: u64, cancel: &CancelToken) -> Option<u64> {
    let (other, forward) = partner_of(byte_at(buf, pos)?)?;
    let me = byte_at(buf, pos)?;
    if forward {
        scan_forward(buf, pos + 1, limit, cancel, |c, depth| {
            if c == me {
                *depth += 1;
            } else if c == other {
                if *depth == 0 {
                    return true;
                }
                *depth -= 1;
            }
            false
        })
    } else {
        scan_backward(buf, pos, false, limit, cancel, |c, depth| {
            if c == me {
                *depth += 1;
            } else if c == other {
                if *depth == 0 {
                    return true;
                }
                *depth -= 1;
            }
            false
        })
    }
}

/// The opening bracket of the innermost brackets around `off`.
pub fn enclosing(buf: &TextBuf, off: u64, limit: u64, cancel: &CancelToken) -> Option<u64> {
    if buf.codec.unit() != 1 {
        return None;
    }
    let mut depths = [0u64; 3];
    scan_backward(buf, off, in_string(buf, off), limit, cancel, |c, _| {
        let i = match c {
            b'(' | b')' => 0,
            b'[' | b']' => 1,
            b'{' | b'}' => 2,
            _ => return false,
        };
        if matches!(c, b')' | b']' | b'}') {
            depths[i] += 1;
            false
        } else if depths[i] == 0 {
            true
        } else {
            depths[i] -= 1;
            false
        }
    })
}

/// Calls `f` with each byte from `from` on that is outside strings until it returns true.
fn scan_forward(buf: &TextBuf, from: u64, limit: u64, cancel: &CancelToken, mut f: impl FnMut(u8, &mut u64) -> bool) -> Option<u64> {
    let end = from.saturating_add(limit).min(buf.len());
    let (mut s, mut esc, mut depth) = (false, false, 0u64);
    let mut pos = from;
    while pos < end {
        if cancel.is_cancelled() {
            return None;
        }
        let to = (pos + CHUNK).min(end);
        let b = buf.read(pos..to)?;
        for (i, &c) in b.iter().enumerate() {
            if c == b'\n' {
                s = false;
                esc = false;
            } else if s {
                if esc {
                    esc = false;
                } else if c == b'\\' {
                    esc = true;
                } else if c == b'"' {
                    s = false;
                }
            } else if c == b'"' {
                s = true;
            } else if f(c, &mut depth) {
                return Some(pos + i as u64);
            }
        }
        pos = to;
    }
    None
}

/// Calls `f` with each byte before `before`, going backwards, that is outside strings, until
/// it returns true. `in_str`: `before` is inside a string.
fn scan_backward(buf: &TextBuf, before: u64, in_str: bool, limit: u64, cancel: &CancelToken, mut f: impl FnMut(u8, &mut u64) -> bool) -> Option<u64> {
    let floor = before.saturating_sub(limit);
    let (mut s, mut depth) = (in_str, 0u64);
    let mut hi = before.min(buf.len());
    while hi > floor {
        if cancel.is_cancelled() {
            return None;
        }
        let lo = hi.saturating_sub(CHUNK).max(floor);
        // A little more before the chunk, to see the backslashes in front of a quote.
        let look = lo.saturating_sub(64);
        let b = buf.read(look..hi)?;
        let base = (lo - look) as usize;
        for i in (base..b.len()).rev() {
            let c = b[i];
            if c == b'\n' {
                s = false;
            } else if c == b'"' {
                let slashes = b[..i].iter().rev().take_while(|&&x| x == b'\\').count();
                if slashes % 2 == 0 {
                    s = !s;
                }
            } else if !s && f(c, &mut depth) {
                return Some(look + i as u64);
            }
        }
        hi = lo;
    }
    None
}

/// An XML / HTML tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tag {
    /// The `<`.
    pub start: u64,
    /// After the `>`.
    pub end: u64,
    /// The name's byte range.
    pub name: (u64, u64),
    pub closing: bool,
    pub self_closing: bool,
}

/// Parses the tag whose `<` is at `start` (not comments, declarations or instructions).
fn tag_at_lt(buf: &TextBuf, start: u64) -> Option<Tag> {
    let b = buf.read(start..(start + TAG_MAX).min(buf.len()))?;
    if b.first() != Some(&b'<') {
        return None;
    }
    let closing = b.get(1) == Some(&b'/');
    let ns = 1 + closing as usize;
    let name_len = b[ns..].iter().take_while(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b':' | b'.')).count();
    if name_len == 0 || !b[ns].is_ascii_alphabetic() && b[ns] != b'_' {
        return None;
    }
    // The end of the tag, skipping quoted attribute values.
    let mut q = None;
    let mut i = ns + name_len;
    while i < b.len() {
        let c = b[i];
        match q {
            Some(x) if c == x => q = None,
            Some(_) => {}
            None if c == b'"' || c == b'\'' => q = Some(c),
            None if c == b'<' => return None,
            None if c == b'>' => {
                let self_closing = !closing && b[..i].ends_with(b"/");
                return Some(Tag { start, end: start + i as u64 + 1, name: (start + ns as u64, start + (ns + name_len) as u64), closing, self_closing });
            }
            None => {}
        }
        i += 1;
    }
    None
}

/// The tag that `off` is in (looking back at most one tag's length for its `<`).
pub fn tag_at(buf: &TextBuf, off: u64) -> Option<Tag> {
    if buf.codec.unit() != 1 {
        return None;
    }
    let from = off.saturating_sub(TAG_MAX);
    let b = buf.read(from..off.min(buf.len()))?;
    // On a `<`, that tag; right after a tag's `>`, that one (as for brackets).
    let lt = match b.iter().rposition(|&c| c == b'<' || c == b'>') {
        _ if byte_at(buf, off) == Some(b'<') => off,
        Some(i) if b[i] == b'<' => from + i as u64,
        Some(i) if i + 1 == b.len() => from + b[..i].iter().rposition(|&c| c == b'<')? as u64,
        _ => return None,
    };
    let t = tag_at_lt(buf, lt)?;
    (off >= t.start && off <= t.end && !t.self_closing).then_some(t)
}

/// The partner of a tag: the closing tag of an opening one, or the opening tag of a closing one.
pub fn tag_partner(buf: &TextBuf, t: &Tag, limit: u64, cancel: &CancelToken) -> Option<Tag> {
    let name = buf.read(t.name.0..t.name.1)?.to_vec();
    let same = |x: &Tag| buf.read(x.name.0..x.name.1).is_some_and(|n| *n == *name);
    let mut depth = 0u64;
    if !t.closing {
        let end = t.end.saturating_add(limit).min(buf.len());
        let mut pos = t.end;
        while pos < end {
            if cancel.is_cancelled() {
                return None;
            }
            let to = (pos + CHUNK).min(end);
            let b = buf.read(pos..to)?;
            let mut next = to;
            for i in memchr::memchr_iter(b'<', &b) {
                let at = pos + i as u64;
                let Some(x) = tag_at_lt(buf, at) else { continue };
                if !same(&x) || x.self_closing {
                    continue;
                }
                if x.closing {
                    if depth == 0 {
                        return Some(x);
                    }
                    depth -= 1;
                } else {
                    depth += 1;
                }
                // Tags never overlap; continue after this one.
                if x.end > to {
                    next = x.end;
                    break;
                }
            }
            pos = next;
        }
        None
    } else {
        let floor = t.start.saturating_sub(limit);
        let mut hi = t.start;
        while hi > floor {
            if cancel.is_cancelled() {
                return None;
            }
            let lo = hi.saturating_sub(CHUNK).max(floor);
            let b = buf.read(lo..hi)?;
            for i in memchr::memrchr_iter(b'<', &b) {
                let Some(x) = tag_at_lt(buf, lo + i as u64) else { continue };
                if !same(&x) || x.self_closing || x.end > t.start {
                    continue;
                }
                if !x.closing {
                    if depth == 0 {
                        return Some(x);
                    }
                    depth -= 1;
                } else {
                    depth += 1;
                }
            }
            hi = lo;
        }
        None
    }
}

/// What the caret is at: two bracket offsets, or two tags' name ranges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pair {
    Brackets(u64, u64),
    Tags((u64, u64), (u64, u64)),
}

/// The brackets or tags at `off` and their partners, scanning at most `limit` bytes.
pub fn pair_at(buf: &TextBuf, off: u64, limit: u64, cancel: &CancelToken) -> Option<Pair> {
    if let Some(p) = at_caret(buf, off) {
        return partner(buf, p, limit, cancel).map(|q| Pair::Brackets(p.min(q), p.max(q)));
    }
    let t = tag_at(buf, off)?;
    let u = tag_partner(buf, &t, limit, cancel)?;
    let (a, b) = if t.closing { (u, t) } else { (t, u) };
    Some(Pair::Tags(a.name, b.name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding;
    use crate::file::{OpenMode, Source};
    use crate::index::LineIndex;
    use std::sync::Arc;

    fn buf(text: &str) -> (tempfile::TempDir, TextBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t");
        std::fs::write(&path, text).unwrap();
        let src = Arc::new(Source::open(&path, OpenMode::Auto).unwrap());
        let det = encoding::detect(&src.read(0..src.len()).unwrap(), true);
        let len = src.len();
        (dir, TextBuf::new(src, LineIndex::new(len), det.codec))
    }

    const NO: u64 = u64::MAX;

    #[test]
    fn brackets_skip_strings() {
        let text = r#"{"a": [1, "x]{", {"b": "\"}"}], "c": ()}"#;
        let (_d, b) = buf(text);
        let c = CancelToken::new();
        let close = text.len() as u64 - 1;
        assert_eq!(pair_at(&b, 0, NO, &c), Some(Pair::Brackets(0, close)));
        // Caret right after the last brace.
        assert_eq!(pair_at(&b, close + 1, NO, &c), Some(Pair::Brackets(0, close)));
        let open_arr = text.find('[').unwrap() as u64;
        let close_arr = text.rfind(']').unwrap() as u64;
        assert_eq!(pair_at(&b, open_arr, NO, &c), Some(Pair::Brackets(open_arr, close_arr)));
        assert_eq!(pair_at(&b, close_arr, NO, &c), Some(Pair::Brackets(open_arr, close_arr)));
        // A bracket inside a string is not one.
        let in_str = text.find("x]").unwrap() as u64 + 1;
        assert_eq!(at_caret(&b, in_str + 1).filter(|&p| p == in_str), None);
        // The innermost brackets around "b".
        let inner = text.find("{\"b").unwrap() as u64;
        assert_eq!(enclosing(&b, inner + 3, NO, &c), Some(inner));
        assert_eq!(pair_at(&b, 5, 2, &c), None);
    }

    #[test]
    fn brackets_across_lines_and_chunks() {
        let text = format!("[\n{}\n]", "  {\"k\": \"v\"},\n".repeat(200_000));
        let (_d, b) = buf(&text);
        let c = CancelToken::new();
        let close = text.len() as u64 - 1;
        assert_eq!(pair_at(&b, 0, NO, &c), Some(Pair::Brackets(0, close)));
        assert_eq!(pair_at(&b, close, NO, &c), Some(Pair::Brackets(0, close)));
        assert_eq!(pair_at(&b, 0, 1 << 20, &c), None);
    }

    #[test]
    fn xml_tags() {
        let text = "<a x=\"1>2\"><b/><a>inner</a></a>";
        let (_d, b) = buf(text);
        let c = CancelToken::new();
        let outer_close = text.rfind("</a>").unwrap() as u64;
        assert_eq!(pair_at(&b, 1, NO, &c), Some(Pair::Tags((1, 2), (outer_close + 2, outer_close + 3))));
        assert_eq!(pair_at(&b, outer_close, NO, &c), Some(Pair::Tags((1, 2), (outer_close + 2, outer_close + 3))));
        let inner_open = text.find("<a>").unwrap() as u64;
        let inner_close = text.find("</a>").unwrap() as u64;
        assert_eq!(pair_at(&b, inner_open + 1, NO, &c), Some(Pair::Tags((inner_open + 1, inner_open + 2), (inner_close + 2, inner_close + 3))));
        assert_eq!(pair_at(&b, inner_close + 2, NO, &c), Some(Pair::Tags((inner_open + 1, inner_open + 2), (inner_close + 2, inner_close + 3))));
        // A self-closing tag has no partner.
        assert_eq!(pair_at(&b, text.find("<b/>").unwrap() as u64 + 1, NO, &c), None);
        let t = tag_at(&b, 1).unwrap();
        assert_eq!((t.start, t.end, t.closing), (0, text.find("><b").unwrap() as u64 + 1, false));
    }

    #[test]
    fn html_nesting() {
        let text = "<div><div>x</div><p>y</p></div>";
        let (_d, b) = buf(text);
        let c = CancelToken::new();
        let close = text.rfind("</div>").unwrap() as u64;
        assert_eq!(pair_at(&b, 1, NO, &c), Some(Pair::Tags((1, 4), (close + 2, close + 5))));
        assert_eq!(pair_at(&b, close + 3, NO, &c), Some(Pair::Tags((1, 4), (close + 2, close + 5))));
    }
}
