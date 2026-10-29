//! Checking JSON strictly: bad escapes, control characters in strings, trailing commas, NaN and
//! other words, numbers like 01 or 1., brackets that do not match, invalid UTF-8, and text after
//! the value. A whole document stops at its first problem; JSON Lines are checked line by line,
//! and every bad line is listed.

use crate::jobs::{CancelToken, Progress};
use crate::lines::LineReader;
use crate::text::TextBuf;

/// Bytes read at a time.
const CHUNK: u64 = 1 << 20;
/// Most problems listed.
pub const PROBLEMS_MAX: usize = 10_000;

/// A problem and where it is (a byte offset).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsonProblem {
    pub at: u64,
    pub message: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JsonChecked {
    pub problems: Vec<JsonProblem>,
    /// Values read (objects, arrays, strings, numbers, true, false, null).
    pub values: u64,
    /// JSON Lines: lines checked.
    pub lines: u64,
    pub capped: bool,
}

/// The bytes of `buf` from `at` to `end`, one at a time.
struct Src<'a> {
    buf: &'a TextBuf,
    data: Vec<u8>,
    start: u64,
    i: usize,
    end: u64,
    cancel: &'a CancelToken,
}

type Res<T> = Result<T, (u64, String)>;

impl<'a> Src<'a> {
    fn pos(&self) -> u64 {
        self.start + self.i as u64
    }

    fn peek(&mut self) -> Res<Option<u8>> {
        if self.i >= self.data.len() {
            let at = self.pos();
            if at >= self.end {
                return Ok(None);
            }
            if self.cancel.is_cancelled() {
                return Err((at, String::new()));
            }
            let to = (at + CHUNK).min(self.end);
            self.data = self.buf.read(at..to).ok_or((at, "could not read the file".to_string()))?.to_vec();
            self.start = at;
            self.i = 0;
        }
        Ok(Some(self.data[self.i]))
    }

    fn next(&mut self) -> Res<Option<u8>> {
        let c = self.peek()?;
        if c.is_some() {
            self.i += 1;
        }
        Ok(c)
    }

    fn ws(&mut self) -> Res<()> {
        while let Some(c) = self.peek()? {
            if !matches!(c, b' ' | b'\t' | b'\r' | b'\n') {
                break;
            }
            self.i += 1;
        }
        Ok(())
    }

    fn fail<T>(&self, at: u64, msg: impl Into<String>) -> Res<T> {
        Err((at, msg.into()))
    }

    fn string(&mut self) -> Res<()> {
        let open = self.pos();
        self.i += 1;
        // Continuation bytes still to come in a UTF-8 character.
        let mut more = 0u8;
        loop {
            let at = self.pos();
            let Some(c) = self.next()? else { return self.fail(open, "a string that is never closed") };
            if more > 0 {
                if c & 0xC0 != 0x80 {
                    return self.fail(at, "invalid UTF-8");
                }
                more -= 1;
                continue;
            }
            match c {
                b'"' => return Ok(()),
                b'\\' => match self.next()? {
                    Some(b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') => {}
                    Some(b'u') => {
                        for _ in 0..4 {
                            let a = self.pos();
                            match self.next()? {
                                Some(h) if h.is_ascii_hexdigit() => {}
                                _ => return self.fail(a, "\\u needs four hex digits"),
                            }
                        }
                    }
                    Some(x) => return self.fail(at, format!("a bad escape \\{}", x as char)),
                    None => return self.fail(open, "a string that is never closed"),
                },
                0..=0x1F => return self.fail(at, "a control character in a string (write it as \\n, \\t or \\u00XX)"),
                0x80..=0xBF | 0xF8..=0xFF => return self.fail(at, "invalid UTF-8"),
                0xC0..=0xDF => more = 1,
                0xE0..=0xEF => more = 2,
                0xF0..=0xF7 => more = 3,
                _ => {}
            }
        }
    }

    fn digits(&mut self) -> Res<usize> {
        let mut n = 0;
        while let Some(c) = self.peek()? {
            if !c.is_ascii_digit() {
                break;
            }
            self.i += 1;
            n += 1;
        }
        Ok(n)
    }

    fn number(&mut self) -> Res<()> {
        let at = self.pos();
        if self.peek()? == Some(b'-') {
            self.i += 1;
        }
        let first = self.peek()?;
        let n = self.digits()?;
        if n == 0 {
            return self.fail(at, "a number needs digits");
        }
        if first == Some(b'0') && n > 1 {
            return self.fail(at, "a number cannot start with 0");
        }
        if self.peek()? == Some(b'.') {
            self.i += 1;
            if self.digits()? == 0 {
                return self.fail(at, "digits must follow the decimal point");
            }
        }
        if matches!(self.peek()?, Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.peek()?, Some(b'+' | b'-')) {
                self.i += 1;
            }
            if self.digits()? == 0 {
                return self.fail(at, "digits must follow the exponent");
            }
        }
        Ok(())
    }

    fn word(&mut self) -> Res<()> {
        let at = self.pos();
        let mut w = Vec::new();
        while let Some(c) = self.peek()? {
            if !c.is_ascii_alphanumeric() && c != b'_' {
                break;
            }
            w.push(c);
            self.i += 1;
            if w.len() > 16 {
                break;
            }
        }
        match &w[..] {
            b"true" | b"false" | b"null" => Ok(()),
            b"" => {
                let c = self.peek()?.map(|c| c as char).unwrap_or(' ');
                self.fail(at, format!("unexpected {c}"))
            }
            other => self.fail(at, format!("{} is not a JSON value (true, false, null, a number or \"text\")", String::from_utf8_lossy(other))),
        }
    }

    /// One value and everything in it; returns how many values that was.
    fn value(&mut self) -> Res<u64> {
        #[derive(PartialEq)]
        enum Want {
            Value,
            KeyOrEnd,
            Key,
            Colon,
            CommaOrEnd,
            ValueOrEnd,
        }
        let mut stack: Vec<(u8, u64)> = Vec::new();
        let mut want = Want::Value;
        let mut values = 0u64;
        let mut after_colon = false;
        loop {
            self.ws()?;
            let at = self.pos();
            let Some(c) = self.peek()? else {
                return match stack.last() {
                    Some(&(_, open)) => self.fail(open, "a bracket that is never closed"),
                    None => self.fail(at, "a value is missing"),
                };
            };
            match want {
                Want::Value | Want::ValueOrEnd => {
                    if want == Want::ValueOrEnd && c == b']' {
                        self.i += 1;
                        stack.pop();
                        want = Want::CommaOrEnd;
                    } else {
                        if c == b']' || c == b'}' {
                            return self.fail(at, if after_colon { "a value is missing after the key" } else { "a comma before a closing bracket (a trailing comma)" });
                        }
                        after_colon = false;
                        values += 1;
                        match c {
                            b'{' => {
                                self.i += 1;
                                stack.push((b'{', at));
                                want = Want::KeyOrEnd;
                                continue;
                            }
                            b'[' => {
                                self.i += 1;
                                stack.push((b'[', at));
                                want = Want::ValueOrEnd;
                                continue;
                            }
                            b'"' => self.string()?,
                            b'-' | b'0'..=b'9' => self.number()?,
                            _ => self.word()?,
                        }
                        want = Want::CommaOrEnd;
                    }
                }
                Want::KeyOrEnd | Want::Key => {
                    if c == b'}' && want == Want::KeyOrEnd {
                        self.i += 1;
                        stack.pop();
                        want = Want::CommaOrEnd;
                    } else if c == b'"' {
                        self.string()?;
                        want = Want::Colon;
                    } else if c == b'}' {
                        return self.fail(at, "a comma before a closing bracket (a trailing comma)");
                    } else {
                        return self.fail(at, "a key must be a \"quoted\" string");
                    }
                }
                Want::Colon => {
                    if c != b':' {
                        return self.fail(at, "a : is missing after the key");
                    }
                    self.i += 1;
                    want = Want::Value;
                    after_colon = true;
                }
                Want::CommaOrEnd => {
                    let Some(&(open, _)) = stack.last() else { return Ok(values) };
                    match (open, c) {
                        (_, b',') => {
                            self.i += 1;
                            want = if open == b'{' { Want::Key } else { Want::Value };
                        }
                        (b'{', b'}') | (b'[', b']') => {
                            self.i += 1;
                            stack.pop();
                        }
                        (b'{', b']') | (b'[', b'}') => return self.fail(at, format!("{} does not match the bracket it closes", c as char)),
                        _ => return self.fail(at, format!("a comma or {} is missing", if open == b'{' { '}' } else { ']' })),
                    }
                }
            }
            if stack.is_empty() && want == Want::CommaOrEnd {
                return Ok(values);
            }
        }
    }
}

/// Checks the document as one JSON value. Returns `None` when cancelled.
pub fn check_document(buf: &TextBuf, cancel: &CancelToken, progress: Option<&Progress>) -> Option<JsonChecked> {
    if let Some(p) = progress {
        p.set_total(buf.len());
    }
    let start = buf.codec.bom_len as u64;
    let mut src = Src { buf, data: Vec::new(), start, i: 0, end: buf.len(), cancel };
    let mut out = JsonChecked::default();
    let r = (|| -> Res<u64> {
        let n = src.value()?;
        src.ws()?;
        if src.peek()?.is_some() {
            return src.fail(src.pos(), "more text after the JSON value (JSON Lines? check them as lines)");
        }
        Ok(n)
    })();
    if let Some(p) = progress {
        p.set_done(buf.len());
    }
    match r {
        Ok(n) => out.values = n,
        Err(_) if cancel.is_cancelled() => return None,
        Err((at, message)) => out.problems.push(JsonProblem { at, message }),
    }
    Some(out)
}

/// Checks each line as a JSON value (blank lines are fine). Returns `None` when cancelled.
pub fn check_lines(buf: &TextBuf, cancel: &CancelToken, progress: Option<&Progress>, mut on_problem: impl FnMut(&JsonProblem)) -> std::io::Result<Option<JsonChecked>> {
    if let Some(p) = progress {
        p.set_total(buf.len());
    }
    let mut out = JsonChecked::default();
    let mut r = LineReader::new(buf);
    let mut n = 0u32;
    loop {
        n += 1;
        if n.is_multiple_of(4096) {
            if cancel.is_cancelled() {
                return Ok(None);
            }
            if let Some(p) = progress {
                p.set_done(r.taken());
            }
        }
        let Some((off, line)) = r.next()? else { break };
        if line.trim_ascii().is_empty() {
            continue;
        }
        out.lines += 1;
        let end = off + line.len() as u64;
        let mut src = Src { buf, data: line.to_vec(), start: off, i: 0, end, cancel };
        let res = (|| -> Res<u64> {
            let v = src.value()?;
            src.ws()?;
            if src.peek()?.is_some() {
                return src.fail(src.pos(), "more text after the line's JSON value");
            }
            Ok(v)
        })();
        match res {
            Ok(v) => out.values += v,
            Err(_) if cancel.is_cancelled() => return Ok(None),
            Err((at, message)) => {
                let p = JsonProblem { at, message };
                on_problem(&p);
                if out.problems.len() < PROBLEMS_MAX {
                    out.problems.push(p);
                } else {
                    out.capped = true;
                }
            }
        }
    }
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::Codec;
    use crate::file::{OpenMode, Source};
    use std::io::Write;
    use std::sync::Arc;

    fn buf(data: &[u8]) -> (tempfile::NamedTempFile, TextBuf) {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(data).unwrap();
        f.flush().unwrap();
        let src = Arc::new(Source::open(f.path(), OpenMode::Auto).unwrap());
        let idx = crate::index::build_blocking(&src, Codec::utf8());
        let mut t = TextBuf::new(src, idx, Codec::utf8());
        t.blocking = true;
        (f, t)
    }

    fn doc(s: &str) -> Option<(u64, String)> {
        let (_f, t) = buf(s.as_bytes());
        let c = check_document(&t, &CancelToken::new(), None).unwrap();
        c.problems.first().map(|p| (p.at, p.message.clone()))
    }

    #[test]
    fn valid_documents() {
        for ok in [r#"{"a": [1, -2.5e3, "x\né", true, false, null, {}, []]}"#, "  [ ]  \n", "\"just text\"", "0", "-0.5"] {
            assert_eq!(doc(ok), None, "{ok}");
        }
        let (_f, t) = buf(br#"{"a": [1, 2], "b": {"c": "d"}}"#);
        assert_eq!(check_document(&t, &CancelToken::new(), None).unwrap().values, 6);
    }

    #[test]
    fn problems_and_where_they_are() {
        let cases = [
            (r#"{"a": 1,}"#, 8, "trailing comma"),
            ("[1, 2,]", 6, "trailing comma"),
            (r#"{"a": NaN}"#, 6, "NaN is not"),
            ("[01]", 1, "start with 0"),
            ("[1.]", 1, "decimal point"),
            (r#"{"a": "x\q"}"#, 8, "bad escape"),
            ("{\"a\": \"x\ty\"}", 8, "control character"),
            ("[1, 2}", 5, "does not match"),
            ("{\"a\" 1}", 5, ": is missing"),
            ("{a: 1}", 1, "quoted"),
            ("[1, 2", 0, "never closed"),
            ("\"open", 0, "never closed"),
            ("{} {}", 3, "more text"),
            ("[1 2]", 3, "comma or ]"),
        ];
        for (src, at, msg) in cases {
            let got = doc(src).unwrap_or_else(|| panic!("{src} passed"));
            assert!(got.0 == at && got.1.contains(msg), "{src}: {got:?}");
        }
        let (_f, t) = buf(&[b'"', 0xFF, b'"']);
        assert!(check_document(&t, &CancelToken::new(), None).unwrap().problems[0].message.contains("UTF-8"));
    }

    #[test]
    fn lines_one_by_one_and_large_values() {
        let (_f, t) = buf(b"{\"a\":1}\n\n{\"a\":}\nnot json\n[1,2]\n");
        let c = check_lines(&t, &CancelToken::new(), None, |_| {}).unwrap().unwrap();
        assert_eq!(c.lines, 4);
        assert_eq!(c.problems.iter().map(|p| p.at).collect::<Vec<_>>(), [14, 16]);
        // A string across the chunk boundary of the reader.
        let mut big = b"[\"".to_vec();
        big.extend(std::iter::repeat_n(b'x', CHUNK as usize + 10));
        big.extend_from_slice(b"\", 1]");
        let (_g, t) = buf(&big);
        assert!(check_document(&t, &CancelToken::new(), None).unwrap().problems.is_empty());
    }
}
