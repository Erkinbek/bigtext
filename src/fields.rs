//! The fields of one structured log line — a JSON object (JSON Lines) or logfmt `key=value`
//! pairs — and a streaming count of one field's values over a whole log.

use std::collections::HashMap;
use std::io::{self, Write};

use crate::jobs::{CancelToken, Progress};
use crate::json;
use crate::lines::LineReader;
use crate::text::TextBuf;
use crate::timestamp::{self, Clock};

/// Nesting levels of JSON objects that are flattened into dotted keys.
const DEPTH: usize = 4;
/// Most different values counted; the lines of other values are counted together.
pub const VALUES_MAX: usize = 100_000;
/// Characters of a value kept as its key in the count.
const VALUE_KEY_MAX: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Syntax {
    Json,
    Logfmt,
}

/// A field of a line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    /// The key; nested JSON keys are joined with dots (`user.id`).
    pub key: String,
    /// The value as text (JSON strings unescaped, logfmt quotes removed).
    pub value: String,
    /// The value as written in the line.
    pub raw: String,
}

/// The fields of `line`: a JSON object's (nested objects flattened, arrays kept as written), or
/// two or more logfmt `key=value` pairs.
pub fn line_fields(line: &str) -> Option<(Syntax, Vec<Field>)> {
    let t = line.trim();
    if t.starts_with('{') {
        let mut out = Vec::new();
        json_flat(t.as_bytes(), "", 0, &mut out)?;
        return Some((Syntax::Json, out));
    }
    let f = logfmt(line);
    (f.len() >= 2).then_some((Syntax::Logfmt, f))
}

fn json_flat(b: &[u8], prefix: &str, depth: usize, out: &mut Vec<Field>) -> Option<()> {
    for (k, r) in json::object_fields(b)? {
        let v = &b[r];
        let key = format!("{prefix}{k}");
        if v.first() == Some(&b'{') && depth + 1 < DEPTH && json_flat(v, &format!("{key}."), depth + 1, out).is_some() {
            continue;
        }
        let raw = String::from_utf8_lossy(v).into_owned();
        let value = if v.first() == Some(&b'"') { json::unescape(v) } else { raw.clone() };
        out.push(Field { key, value, raw });
    }
    Some(())
}

/// logfmt pairs: `key=value`, `key="quoted value"`, separated by spaces.
pub fn logfmt(line: &str) -> Vec<Field> {
    let b = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        // A key starts at the line start or after a space.
        if i > 0 && !b[i - 1].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let ks = i;
        while i < b.len() && is_key_byte(b[i]) {
            i += 1;
        }
        if i == ks || i >= b.len() || b[i] != b'=' {
            i = (i + 1).max(ks + 1);
            continue;
        }
        let key = &line[ks..i];
        i += 1;
        let vs = i;
        if b.get(i) == Some(&b'"') {
            i += 1;
            while i < b.len() && b[i] != b'"' {
                i += if b[i] == b'\\' { 2 } else { 1 };
            }
            i = (i + 1).min(b.len());
            let raw = &line[vs..i];
            out.push(Field { key: key.into(), value: unquote(raw), raw: raw.into() });
        } else {
            while i < b.len() && !b[i].is_ascii_whitespace() {
                i += 1;
            }
            let raw = &line[vs..i];
            out.push(Field { key: key.into(), value: raw.into(), raw: raw.into() });
        }
    }
    out
}

fn is_key_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-' | b'@' | b'/')
}

/// A logfmt "quoted value" without its quotes and escapes.
fn unquote(raw: &str) -> String {
    let inner = raw.strip_prefix('"').map(|s| s.strip_suffix('"').unwrap_or(s)).unwrap_or(raw);
    let mut out = String::with_capacity(inner.len());
    let mut it = inner.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(o) => out.push(o),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The value of `key` in `line` without parsing the whole line: a search for `"leaf":` (JSON)
/// or `key=` (logfmt) checked at the key's position. Nested JSON keys (`a.b`) are looked up
/// through their objects.
pub fn find_value(line: &[u8], key: &str, syntax: Syntax) -> Option<String> {
    match syntax {
        Syntax::Logfmt => {
            let needle = format!("{key}=");
            let mut from = 0;
            while let Some(i) = memchr::memmem::find(&line[from..], needle.as_bytes()) {
                let at = from + i;
                if at == 0 || line[at - 1].is_ascii_whitespace() {
                    let text = std::str::from_utf8(&line[at..]).ok().map(str::to_string).unwrap_or_else(|| String::from_utf8_lossy(&line[at..]).into_owned());
                    return logfmt(&text).into_iter().next().filter(|f| f.key == key).map(|f| f.value);
                }
                from = at + 1;
            }
            None
        }
        Syntax::Json if key.contains('.') => {
            let mut obj = line;
            let mut parts = key.split('.').peekable();
            while let Some(p) = parts.next() {
                let (_, r) = json::object_fields(obj)?.into_iter().find(|(k, _)| k == p)?;
                let v = &obj[r];
                if parts.peek().is_none() {
                    return Some(json_text(v));
                }
                obj = v;
            }
            None
        }
        Syntax::Json => {
            let needle = format!("\"{key}\"");
            let mut from = 0;
            while let Some(i) = memchr::memmem::find(&line[from..], needle.as_bytes()) {
                let at = from + i;
                let mut k = at + needle.len();
                while k < line.len() && line[k].is_ascii_whitespace() {
                    k += 1;
                }
                let before = line[..at].iter().rev().find(|c| !c.is_ascii_whitespace());
                if line.get(k) == Some(&b':') && matches!(before, Some(b'{') | Some(b',')) {
                    let rest = &line[k + 1..];
                    let s = rest.iter().position(|c| !c.is_ascii_whitespace())?;
                    return value_at(&rest[s..]).map(json_text);
                }
                from = at + 1;
            }
            None
        }
    }
}

/// The JSON value at the start of `b`: a string, an object or array, or a number or literal.
fn value_at(b: &[u8]) -> Option<&[u8]> {
    match *b.first()? {
        b'"' => {
            let mut k = 1;
            while k < b.len() {
                match b[k] {
                    b'\\' => k += 2,
                    b'"' => return Some(&b[..k + 1]),
                    _ => k += 1,
                }
            }
            None
        }
        b'{' | b'[' => {
            let (mut depth, mut k) = (0i64, 0);
            while k < b.len() {
                match b[k] {
                    b'"' => {
                        k += 1;
                        while k < b.len() && b[k] != b'"' {
                            k += if b[k] == b'\\' { 2 } else { 1 };
                        }
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(&b[..k + 1]);
                        }
                    }
                    _ => {}
                }
                k += 1;
            }
            None
        }
        _ => {
            let k = b.iter().position(|&c| matches!(c, b',' | b'}' | b']') || c.is_ascii_whitespace()).unwrap_or(b.len());
            (k > 0).then(|| &b[..k])
        }
    }
}

/// A JSON value as text: strings unescaped, the rest as written.
fn json_text(v: &[u8]) -> String {
    if v.first() == Some(&b'"') {
        json::unescape(v)
    } else {
        String::from_utf8_lossy(v).into_owned()
    }
}

/// One value of a field over a log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValueCount {
    pub value: String,
    pub lines: u64,
    /// Time keys ([`timestamp::format_key`]) of its first and last line with a timestamp.
    pub first: Option<i64>,
    pub last: Option<i64>,
    /// Offset of its first line.
    pub first_off: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ValueCounts {
    /// Most frequent first.
    pub values: Vec<ValueCount>,
    /// Lines with the field.
    pub lines: u64,
    /// Lines without it.
    pub without: u64,
    /// Lines of values beyond [`VALUES_MAX`] different ones.
    pub other: u64,
    /// Whether the times have dates.
    pub dated: bool,
}

/// Counts the values of `key` over the document in one streaming pass. UTF-8 (or another
/// ASCII-compatible encoding) only. Returns `None` when cancelled.
pub fn value_counts(buf: &TextBuf, key: &str, syntax: Syntax, cancel: &CancelToken, progress: Option<&Progress>) -> io::Result<Option<ValueCounts>> {
    if buf.codec.unit() != 1 {
        return Err(io::Error::other("Counting values needs a UTF-8 file"));
    }
    if let Some(p) = progress {
        p.set_total(buf.len());
    }
    let mut map: HashMap<String, ValueCount> = HashMap::new();
    let mut out = ValueCounts::default();
    let mut clock = Clock::default();
    let mut dated: Option<bool> = None;
    let mut r = LineReader::new(buf);
    let mut n = 0u64;
    let mut done = 0;
    loop {
        n += 1;
        if n.is_multiple_of(4096) {
            if cancel.is_cancelled() {
                return Ok(None);
            }
            if let Some(p) = progress {
                p.add(r.taken() - done);
            }
            done = r.taken();
        }
        let Some((off, line)) = r.next()? else { break };
        let Some(v) = find_value(line, key, syntax) else {
            out.without += 1;
            continue;
        };
        out.lines += 1;
        let t = timestamp::parse_bytes(line).map(|st| {
            let d = *dated.get_or_insert(st.has_date);
            clock.key(st, d)
        });
        let v: String = if v.chars().count() > VALUE_KEY_MAX { v.chars().take(VALUE_KEY_MAX).collect() } else { v };
        if let Some(c) = map.get_mut(&v) {
            c.lines += 1;
            if t.is_some() {
                c.first = c.first.or(t);
                c.last = t;
            }
        } else if map.len() < VALUES_MAX {
            map.insert(v.clone(), ValueCount { value: v, lines: 1, first: t, last: t, first_off: off });
        } else {
            out.other += 1;
        }
    }
    out.dated = dated.unwrap_or(false);
    out.values = map.into_values().collect();
    out.values.sort_by(|a, b| b.lines.cmp(&a.lines).then_with(|| a.first_off.cmp(&b.first_off)));
    Ok(Some(out))
}

/// The counts as CSV: value, lines, percent, first and last time.
pub fn counts_to_csv<W: Write>(c: &ValueCounts, key: &str, out: &mut W) -> io::Result<()> {
    let q = |s: &str| if s.contains([',', '"', '\n', '\r']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_string() };
    let time = |k: Option<i64>| k.map(|k| timestamp::format_key(k, c.dated, true)).unwrap_or_default();
    writeln!(out, "{},lines,percent,first,last", q(key))?;
    for v in &c.values {
        let pct = if c.lines > 0 { v.lines as f64 * 100.0 / c.lines as f64 } else { 0.0 };
        writeln!(out, "{},{},{pct:.2},{},{}", q(&v.value), v.lines, time(v.first), time(v.last))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::Codec;
    use crate::file::{OpenMode, Source};
    use crate::index;
    use std::sync::Arc;

    fn keys(fs: &[Field]) -> Vec<(&str, &str)> {
        fs.iter().map(|f| (f.key.as_str(), f.value.as_str())).collect()
    }

    #[test]
    fn json_lines_flattened() {
        let (s, f) = line_fields(r#"{"ts":"2024-05-01T10:00:00Z","level":"error","user":{"id":42,"name":"A\"B"},"tags":["x","y"],"err":"line1\nline2"}"#).unwrap();
        assert_eq!(s, Syntax::Json);
        assert_eq!(keys(&f), [("ts", "2024-05-01T10:00:00Z"), ("level", "error"), ("user.id", "42"), ("user.name", "A\"B"), ("tags", r#"["x","y"]"#), ("err", "line1\nline2")]);
        assert_eq!(f[2].raw, "42");
        assert_eq!(f[1].raw, "\"error\"");
        assert!(line_fields("{broken").is_none());
    }

    #[test]
    fn logfmt_pairs() {
        let (s, f) = line_fields(r#"time=2024-05-01T10:00:00Z level=info msg="user logged in" user=42 empty= path=/a/b"#).unwrap();
        assert_eq!(s, Syntax::Logfmt);
        assert_eq!(keys(&f), [("time", "2024-05-01T10:00:00Z"), ("level", "info"), ("msg", "user logged in"), ("user", "42"), ("empty", ""), ("path", "/a/b")]);
        assert_eq!(f[2].raw, "\"user logged in\"");
        // One pair is not a structured line; a = inside a value is not a key.
        assert!(line_fields("2024-05-01 INFO x=1 done").is_none());
        assert_eq!(keys(&logfmt("q=a=b k=\"say \\\"hi\\\"\"")), [("q", "a=b"), ("k", "say \"hi\"")]);
    }

    #[test]
    fn find_value_without_parsing() {
        let j = br#"{"level":"warn", "msg": "a \"level\": x", "n": 12, "o": {"level": "inner"}, "arr": [1, {"x": 2}]}"#;
        assert_eq!(find_value(j, "level", Syntax::Json).as_deref(), Some("warn"));
        assert_eq!(find_value(j, "n", Syntax::Json).as_deref(), Some("12"));
        assert_eq!(find_value(j, "arr", Syntax::Json).as_deref(), Some(r#"[1, {"x": 2}]"#));
        assert_eq!(find_value(j, "o.level", Syntax::Json).as_deref(), Some("inner"));
        assert_eq!(find_value(j, "missing", Syntax::Json), None);
        let l = b"level=info msg=\"x level=fake\" xlevel=no";
        assert_eq!(find_value(l, "level", Syntax::Logfmt).as_deref(), Some("info"));
        assert_eq!(find_value(b"a=1 xlevel=2", "level", Syntax::Logfmt), None);
        assert_eq!(find_value(b"msg=\"a b\"", "msg", Syntax::Logfmt).as_deref(), Some("a b"));
    }

    #[test]
    fn counts_values_over_a_file() {
        let data = b"{\"ts\":\"2024-05-01 10:00:00\",\"status\":200}\n{\"ts\":\"2024-05-01 10:00:01\",\"status\":500}\nnot json\n{\"ts\":\"2024-05-01 10:00:02\",\"status\":200}\n{\"other\":1}\n";
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(data).unwrap();
        f.flush().unwrap();
        let src = Arc::new(Source::open(f.path(), OpenMode::Auto).unwrap());
        let idx = index::build_blocking(&src, Codec::utf8());
        let mut t = TextBuf::new(src, idx, Codec::utf8());
        t.blocking = true;
        let c = value_counts(&t, "status", Syntax::Json, &CancelToken::new(), None).unwrap().unwrap();
        assert_eq!((c.lines, c.without, c.other), (3, 2, 0));
        assert_eq!(c.values[0].value, "200");
        assert_eq!(c.values[0].lines, 2);
        assert!(c.dated);
        assert_eq!(timestamp::format_key(c.values[0].last.unwrap(), true, true), "2024-05-01 10:00:02");
        let mut csv = Vec::new();
        counts_to_csv(&c, "status", &mut csv).unwrap();
        assert_eq!(String::from_utf8(csv).unwrap(), "status,lines,percent,first,last\n200,2,66.67,2024-05-01 10:00:00,2024-05-01 10:00:02\n500,1,33.33,2024-05-01 10:00:01,2024-05-01 10:00:01\n");
    }
}
