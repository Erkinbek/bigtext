//! Streaming JSON for files of any size: the direct children of an object or array, page by
//! page; the value at a path such as `$.items[1234].id`; the path of the value at an offset;
//! and JSON Lines (one object per line) to CSV. Values are skipped with a byte loop that
//! tracks only strings and nesting depth, so nothing is ever held in memory as a tree.
//! UTF-8 (or another ASCII-compatible encoding) only.

use std::io::{self, Write};
use std::ops::Range;

use crate::jobs::{CancelToken, Progress};
use crate::lines::LineReader;
use crate::text::TextBuf;

/// Bytes read at a time.
const CHUNK: u64 = 1 << 20;
/// Object keys longer than this are cut (they are only shown and compared).
const KEY_MAX: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Object,
    Array,
    String,
    Other,
}

impl Kind {
    fn of(b: u8) -> Kind {
        match b {
            b'{' => Kind::Object,
            b'[' => Kind::Array,
            b'"' => Kind::String,
            _ => Kind::Other,
        }
    }
}

/// A direct child of an object or array.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Child {
    pub key: Option<String>,
    pub index: usize,
    /// The value's byte range.
    pub start: u64,
    pub end: u64,
    pub kind: Kind,
}

/// Where to go on listing a container's children.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resume {
    pub(crate) at: u64,
    pub(crate) index: usize,
    pub(crate) object: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Page {
    pub items: Vec<Child>,
    /// Set when there are more children.
    pub resume: Option<Resume>,
}

/// A step of a path: `.key` or `[index]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Key(String),
    Index(usize),
}

/// The text read byte by byte, a chunk at a time.
struct Reader<'a> {
    buf: &'a TextBuf,
    data: Vec<u8>,
    start: u64,
    i: usize,
    cancel: &'a CancelToken,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a TextBuf, at: u64, cancel: &'a CancelToken) -> Reader<'a> {
        Reader { buf, data: Vec::new(), start: at, i: 0, cancel }
    }

    fn pos(&self) -> u64 {
        self.start + self.i as u64
    }

    fn fill(&mut self) -> Result<bool, String> {
        if self.i < self.data.len() {
            return Ok(true);
        }
        if self.cancel.is_cancelled() {
            return Err(CANCELLED.into());
        }
        let at = self.pos();
        if at >= self.buf.len() {
            return Ok(false);
        }
        let to = (at + CHUNK).min(self.buf.len());
        self.data = self.buf.read(at..to).ok_or("could not read the file")?.to_vec();
        self.start = at;
        self.i = 0;
        Ok(true)
    }

    fn peek(&mut self) -> Result<Option<u8>, String> {
        Ok(if self.fill()? { Some(self.data[self.i]) } else { None })
    }

    fn bump(&mut self) {
        self.i += 1;
    }

    fn skip_ws(&mut self) -> Result<(), String> {
        while let Some(b) = self.peek()? {
            if !matches!(b, b' ' | b'\t' | b'\r' | b'\n') {
                break;
            }
            self.bump();
        }
        Ok(())
    }

    fn expect(&mut self, b: u8) -> Result<(), String> {
        self.skip_ws()?;
        match self.peek()? {
            Some(x) if x == b => {
                self.bump();
                Ok(())
            }
            Some(x) => Err(format!("expected '{}' at byte {}, found '{}'", b as char, self.pos(), x as char)),
            None => Err(format!("expected '{}' at the end of the file", b as char)),
        }
    }

    /// Reads a string (at its opening quote) and returns it unescaped (cut after KEY_MAX bytes).
    fn string(&mut self) -> Result<String, String> {
        self.expect(b'"')?;
        let mut raw = Vec::new();
        let mut esc = false;
        loop {
            let Some(b) = self.peek()? else { return Err("unterminated string".into()) };
            self.bump();
            if esc {
                esc = false;
            } else if b == b'\\' {
                esc = true;
            } else if b == b'"' {
                break;
            }
            if raw.len() < KEY_MAX + 8 {
                raw.push(b);
            }
        }
        let quoted = [&b"\""[..], &raw[..], b"\""].concat();
        Ok(unescape(&quoted))
    }

    /// Skips one value; stops early (returning false) once past `stop`.
    fn skip_value(&mut self, stop: u64) -> Result<bool, String> {
        self.skip_ws()?;
        let Some(first) = self.peek()? else { return Err("expected a value at the end of the file".into()) };
        match first {
            b'{' | b'[' => {
                let (mut depth, mut s, mut esc) = (0u64, false, false);
                loop {
                    if !self.fill()? {
                        return Err("unexpected end of the file inside a value".into());
                    }
                    if self.pos() > stop {
                        return Ok(false);
                    }
                    // A tight loop over the chunk.
                    let data = &self.data[self.i..];
                    let mut k = 0;
                    while k < data.len() {
                        let c = data[k];
                        k += 1;
                        if s {
                            if esc {
                                esc = false;
                            } else if c == b'\\' {
                                esc = true;
                            } else if c == b'"' {
                                s = false;
                            }
                        } else {
                            match c {
                                b'"' => s = true,
                                b'{' | b'[' => depth += 1,
                                b'}' | b']' => {
                                    depth -= 1;
                                    if depth == 0 {
                                        self.i += k;
                                        return Ok(true);
                                    }
                                }
                                _ => {}
                            }
                        }
                        if self.start + (self.i + k) as u64 > stop {
                            self.i += k;
                            return Ok(false);
                        }
                    }
                    self.i += k;
                }
            }
            b'"' => {
                self.bump();
                let mut esc = false;
                loop {
                    let Some(c) = self.peek()? else { return Err("unterminated string".into()) };
                    self.bump();
                    if esc {
                        esc = false;
                    } else if c == b'\\' {
                        esc = true;
                    } else if c == b'"' {
                        return Ok(true);
                    }
                    if self.pos() > stop {
                        return Ok(false);
                    }
                }
            }
            _ => {
                let mut n = 0;
                while let Some(c) = self.peek()? {
                    if matches!(c, b',' | b'}' | b']' | b' ' | b'\t' | b'\r' | b'\n') {
                        break;
                    }
                    self.bump();
                    n += 1;
                }
                if n == 0 {
                    return Err(format!("unexpected '{}' at byte {}", first as char, self.pos()));
                }
                Ok(true)
            }
        }
    }

    /// Copies one value to `out` without the white space between its parts (strings as they
    /// are).
    fn copy_value(&mut self, out: &mut dyn std::io::Write) -> Result<(), String> {
        let werr = |e: std::io::Error| e.to_string();
        self.skip_ws()?;
        let Some(first) = self.peek()? else { return Err("expected a value at the end of the file".into()) };
        let (mut depth, mut s, mut esc) = (0u64, false, false);
        let scalar = !matches!(first, b'{' | b'[' | b'"');
        loop {
            if !self.fill()? {
                return if scalar { Ok(()) } else { Err("unexpected end of the file inside a value".into()) };
            }
            let data = &self.data[self.i..];
            let (mut k, mut seg) = (0, 0);
            while k < data.len() {
                let c = data[k];
                if s {
                    if esc {
                        esc = false;
                    } else if c == b'\\' {
                        esc = true;
                    } else if c == b'"' {
                        s = false;
                        if depth == 0 {
                            out.write_all(&data[seg..k + 1]).map_err(werr)?;
                            self.i += k + 1;
                            return Ok(());
                        }
                    }
                    k += 1;
                    continue;
                }
                if scalar {
                    if matches!(c, b',' | b'}' | b']' | b' ' | b'\t' | b'\r' | b'\n') {
                        out.write_all(&data[seg..k]).map_err(werr)?;
                        self.i += k;
                        return Ok(());
                    }
                    k += 1;
                    continue;
                }
                match c {
                    b' ' | b'\t' | b'\r' | b'\n' => {
                        out.write_all(&data[seg..k]).map_err(werr)?;
                        seg = k + 1;
                    }
                    b'"' => s = true,
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth = depth.saturating_sub(1);
                        if depth == 0 {
                            out.write_all(&data[seg..k + 1]).map_err(werr)?;
                            self.i += k + 1;
                            return Ok(());
                        }
                    }
                    _ => {}
                }
                k += 1;
            }
            out.write_all(&data[seg..]).map_err(werr)?;
            self.i += k;
        }
    }
}

/// Writes each item of the array at `at` (an offset from [`root`] or [`resolve`]) on a line of
/// its own, without white space (JSON Lines). Returns the number of items, or `None` when
/// cancelled.
pub fn array_to_lines<W: std::io::Write>(buf: &TextBuf, at: u64, out: &mut W, cancel: &CancelToken, progress: Option<&Progress>) -> Result<Option<u64>, String> {
    if buf.codec.unit() != 1 {
        return Err("JSON must be UTF-8".into());
    }
    if let Some(p) = progress {
        p.set_total(buf.len());
    }
    let mut r = Reader::new(buf, at, cancel);
    let wrap = |e: String| if e == CANCELLED { None } else { Some(e) };
    let run = (|| -> Result<u64, String> {
        r.expect(b'[')?;
        let mut n = 0u64;
        r.skip_ws()?;
        if r.peek()? == Some(b']') {
            return Ok(0);
        }
        loop {
            r.copy_value(out)?;
            out.write_all(b"\n").map_err(|e| e.to_string())?;
            n += 1;
            if let Some(p) = progress {
                p.set_done(r.pos());
            }
            r.skip_ws()?;
            match r.peek()? {
                Some(b',') => r.bump(),
                Some(b']') => return Ok(n),
                Some(c) => return Err(format!("expected ',' or ']' at byte {}, found '{}'", r.pos(), c as char)),
                None => return Err("the array does not end".into()),
            }
        }
    })();
    match run {
        Ok(n) => Ok(Some(n)),
        Err(e) => match wrap(e) {
            Some(e) => Err(e),
            None => Ok(None),
        },
    }
}

/// The array to turn into lines: the top-level value when it is an array, otherwise the first
/// array under a key of the top-level object (`{"events": [...]}`), with its path.
pub fn find_array(buf: &TextBuf, cancel: &CancelToken) -> Result<(u64, String), String> {
    let at = root(buf, cancel)?;
    let mut r = Reader::new(buf, at, cancel);
    match r.peek()? {
        Some(b'[') => return Ok((at, "$".into())),
        Some(b'{') => {}
        _ => return Err("the file is not a JSON array or object".into()),
    }
    let page = children(buf, at, None, 1000, cancel)?;
    for c in page.items {
        if c.kind == Kind::Array {
            let name = c.key.clone().unwrap_or_else(|| c.index.to_string());
            return Ok((c.start, format!("$.{name}")));
        }
    }
    Err("no array in the file's top-level object".into())
}

/// Writes JSON Lines as one JSON array, an item per line. Returns the number of items, or
/// `None` when cancelled.
pub fn lines_to_array<W: std::io::Write>(buf: &TextBuf, out: &mut W, cancel: &CancelToken, progress: Option<&Progress>) -> std::io::Result<Option<u64>> {
    if let Some(p) = progress {
        p.set_total(buf.len());
    }
    let mut r = crate::lines::LineReader::new(buf);
    let mut n = 0u64;
    out.write_all(b"[")?;
    while let Some((_, line)) = r.next()? {
        let line = line.trim_ascii();
        if line.is_empty() {
            continue;
        }
        out.write_all(if n == 0 { b"\n" } else { b",\n" })?;
        out.write_all(line)?;
        n += 1;
        if n.is_multiple_of(4096) {
            if cancel.is_cancelled() {
                return Ok(None);
            }
            if let Some(p) = progress {
                p.set_done(r.taken());
            }
        }
    }
    out.write_all(if n == 0 { b"]\n" } else { b"\n]\n" })?;
    Ok(Some(n))
}

const CANCELLED: &str = "cancelled";

/// Unescapes a JSON string with its quotes; other values come back as written.
pub fn unescape(raw: &[u8]) -> String {
    if raw.len() < 2 || raw[0] != b'"' {
        return String::from_utf8_lossy(raw).into_owned();
    }
    let body = &raw[1..raw.len() - 1];
    if !body.contains(&b'\\') {
        return String::from_utf8_lossy(body).into_owned();
    }
    let s = String::from_utf8_lossy(body);
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('b') => out.push('\u{8}'),
            Some('f') => out.push('\u{c}'),
            Some('u') => {
                let hex: String = it.by_ref().take(4).collect();
                let mut code = u32::from_str_radix(&hex, 16).unwrap_or(0xFFFD);
                // A surrogate pair.
                if (0xD800..0xDC00).contains(&code) {
                    let rest: String = it.clone().take(6).collect();
                    if let Some(lo) = rest.strip_prefix("\\u").and_then(|h| u32::from_str_radix(h, 16).ok()).filter(|l| (0xDC00..0xE000).contains(l)) {
                        it.by_ref().take(6).for_each(drop);
                        code = 0x10000 + ((code - 0xD800) << 10) + (lo - 0xDC00);
                    }
                }
                out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
            }
            Some(o) => out.push(o),
            None => {}
        }
    }
    out
}

/// Offset of the top-level value (after a byte order mark and white space).
pub fn root(buf: &TextBuf, cancel: &CancelToken) -> Result<u64, String> {
    let mut r = Reader::new(buf, buf.codec.bom_len as u64, cancel);
    r.skip_ws()?;
    match r.peek()? {
        Some(_) => Ok(r.pos()),
        None => Err("the file is empty".into()),
    }
}

/// Up to `max` children of the object or array at `at` (or from where `resume` left off).
pub fn children(buf: &TextBuf, at: u64, resume: Option<Resume>, max: usize, cancel: &CancelToken) -> Result<Page, String> {
    let mut page = Page::default();
    let (mut r, object, mut index) = match resume {
        Some(x) => (Reader::new(buf, x.at, cancel), x.object, x.index),
        None => {
            let mut r = Reader::new(buf, at, cancel);
            let object = match r.peek()? {
                Some(b'{') => true,
                Some(b'[') => false,
                _ => return Ok(page),
            };
            r.bump();
            (r, object, 0)
        }
    };
    loop {
        r.skip_ws()?;
        match r.peek()? {
            Some(b'}' | b']') | None => return Ok(page),
            Some(b',') => {
                r.bump();
                r.skip_ws()?;
            }
            _ => {}
        }
        if page.items.len() == max {
            page.resume = Some(Resume { at: r.pos(), index, object });
            return Ok(page);
        }
        let key = if object {
            let k = r.string()?;
            r.expect(b':')?;
            Some(k)
        } else {
            None
        };
        r.skip_ws()?;
        let start = r.pos();
        let kind = Kind::of(r.peek()?.unwrap_or(b' '));
        r.skip_value(u64::MAX)?;
        page.items.push(Child { key, index, start, end: r.pos(), kind });
        index += 1;
    }
}

/// Parses `$.items[3].id`, `items[3]["a b"]`, `.a.b` into steps.
pub fn parse_path(text: &str) -> Result<Vec<Step>, String> {
    let t = text.trim();
    let t = t.strip_prefix('$').unwrap_or(t);
    let b = t.as_bytes();
    let mut steps = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'.' => i += 1,
            b'[' => {
                let close = t[i..].find(']').ok_or_else(|| format!("missing ] after {}", &t[..i]))? + i;
                let inner = t[i + 1..close].trim();
                if let Some(q) = inner.strip_prefix('"').and_then(|x| x.strip_suffix('"')).or_else(|| inner.strip_prefix('\'').and_then(|x| x.strip_suffix('\''))) {
                    steps.push(Step::Key(q.to_string()));
                } else {
                    steps.push(Step::Index(inner.parse().map_err(|_| format!("not an index: [{inner}]"))?));
                }
                i = close + 1;
                continue;
            }
            _ => {}
        }
        if i < b.len() && b[i] != b'[' {
            let end = t[i..].find(['.', '[']).map(|k| k + i).unwrap_or(t.len());
            let key = &t[i..end];
            if key.is_empty() {
                return Err("empty key".into());
            }
            steps.push(Step::Key(key.to_string()));
            i = end;
        }
    }
    Ok(steps)
}

/// `$.items[3].id`; keys that are not plain words in brackets: `$["a b"]`.
pub fn format_path(steps: &[Step]) -> String {
    let mut s = String::from("$");
    for st in steps {
        match st {
            Step::Index(i) => s.push_str(&format!("[{i}]")),
            Step::Key(k) if !k.is_empty() && k.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-') => {
                s.push('.');
                s.push_str(k);
            }
            Step::Key(k) => s.push_str(&format!("[\"{}\"]", k.replace('"', "\\\""))),
        }
    }
    s
}

/// The byte range of the value at `path`.
pub fn resolve(buf: &TextBuf, path: &[Step], cancel: &CancelToken) -> Result<(u64, u64), String> {
    let mut at = root(buf, cancel)?;
    for (n, step) in path.iter().enumerate() {
        let mut r = Reader::new(buf, at, cancel);
        let object = match (r.peek()?, step) {
            (Some(b'{'), Step::Key(_)) => true,
            (Some(b'['), Step::Index(_)) => false,
            _ => return Err(format!("{} is not {}", format_path(&path[..n]), if matches!(step, Step::Key(_)) { "an object" } else { "an array" })),
        };
        r.bump();
        let mut index = 0;
        let found = loop {
            r.skip_ws()?;
            match r.peek()? {
                Some(b'}' | b']') | None => break None,
                Some(b',') => {
                    r.bump();
                    r.skip_ws()?;
                }
                _ => {}
            }
            let key = if object {
                let k = r.string()?;
                r.expect(b':')?;
                Some(k)
            } else {
                None
            };
            r.skip_ws()?;
            let hit = match step {
                Step::Key(k) => key.as_deref() == Some(k.as_str()),
                Step::Index(i) => *i == index,
            };
            if hit {
                break Some(r.pos());
            }
            r.skip_value(u64::MAX)?;
            index += 1;
        };
        at = found.ok_or_else(|| format!("{} not found", format_path(&path[..=n])))?;
    }
    let mut r = Reader::new(buf, at, cancel);
    r.skip_value(u64::MAX)?;
    Ok((at, r.pos()))
}

/// The path of the value at `off` (reading the text up to it once).
pub fn path_at(buf: &TextBuf, off: u64, cancel: &CancelToken) -> Result<Vec<Step>, String> {
    struct Frame {
        object: bool,
        key: Option<String>,
        index: usize,
        expect_key: bool,
    }
    let mut stack: Vec<Frame> = Vec::new();
    let mut r = Reader::new(buf, buf.codec.bom_len as u64, cancel);
    loop {
        r.skip_ws()?;
        let Some(b) = r.peek()? else { break };
        if r.pos() > off {
            break;
        }
        match b {
            b'{' | b'[' => {
                r.bump();
                stack.push(Frame { object: b == b'{', key: None, index: 0, expect_key: b == b'{' });
            }
            b'}' | b']' => {
                r.bump();
                if r.pos() > off {
                    break;
                }
                stack.pop();
            }
            b',' => {
                r.bump();
                if let Some(f) = stack.last_mut() {
                    if f.object {
                        f.expect_key = true;
                    } else {
                        f.index += 1;
                    }
                }
            }
            b':' => {
                r.bump();
                if let Some(f) = stack.last_mut() {
                    f.expect_key = false;
                }
            }
            b'"' if stack.last().is_some_and(|f| f.object && f.expect_key) => {
                let k = r.string()?;
                if let Some(f) = stack.last_mut() {
                    f.key = Some(k);
                }
            }
            _ => {
                if !r.skip_value(off)? {
                    break;
                }
            }
        }
    }
    Ok(stack.into_iter().filter_map(|f| if f.object { f.key.map(Step::Key) } else { Some(Step::Index(f.index)) }).collect())
}

/// The keys and value ranges of a JSON object held in `b` (one line of JSON Lines).
pub fn object_fields(b: &[u8]) -> Option<Vec<(String, Range<usize>)>> {
    let mut i = 0;
    let ws = |b: &[u8], mut i: usize| {
        while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\r' | b'\n') {
            i += 1;
        }
        i
    };
    let string_end = |b: &[u8], i: usize| -> Option<usize> {
        let mut k = i + 1;
        while k < b.len() {
            match b[k] {
                b'\\' => k += 2,
                b'"' => return Some(k + 1),
                _ => k += 1,
            }
        }
        None
    };
    let value_end = |b: &[u8], i: usize| -> Option<usize> {
        match *b.get(i)? {
            b'"' => string_end(b, i),
            b'{' | b'[' => {
                let (mut depth, mut k) = (0i64, i);
                while k < b.len() {
                    match b[k] {
                        b'"' => {
                            k = string_end(b, k)?;
                            continue;
                        }
                        b'{' | b'[' => depth += 1,
                        b'}' | b']' => {
                            depth -= 1;
                            if depth == 0 {
                                return Some(k + 1);
                            }
                        }
                        _ => {}
                    }
                    k += 1;
                }
                None
            }
            _ => {
                let mut k = i;
                while k < b.len() && !matches!(b[k], b',' | b'}' | b']' | b' ' | b'\t' | b'\r' | b'\n') {
                    k += 1;
                }
                (k > i).then_some(k)
            }
        }
    };
    i = ws(b, i);
    if b.get(i) != Some(&b'{') {
        return None;
    }
    i = ws(b, i + 1);
    let mut out = Vec::new();
    if b.get(i) == Some(&b'}') {
        return Some(out);
    }
    loop {
        if b.get(i) != Some(&b'"') {
            return None;
        }
        let ke = string_end(b, i)?;
        let key = unescape(&b[i..ke]);
        i = ws(b, ke);
        if b.get(i) != Some(&b':') {
            return None;
        }
        i = ws(b, i + 1);
        let ve = value_end(b, i)?;
        out.push((key, i..ve));
        i = ws(b, ve);
        match b.get(i) {
            Some(b',') => i = ws(b, i + 1),
            Some(b'}') => return Some(out),
            _ => return None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JsonlStats {
    pub rows: u64,
    pub columns: usize,
    /// Lines that were not JSON objects (left out).
    pub skipped: u64,
    /// Keys first seen after the sampled lines (left out).
    pub late_keys: u64,
}

/// Lines read to find the columns.
pub const JSONL_SAMPLE: usize = 1000;

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Writes JSON Lines as CSV: a column per key (from the first [`JSONL_SAMPLE`] lines, in the
/// order first seen), strings unescaped, other values as written. Returns `None` if cancelled.
pub fn jsonl_to_csv<W: Write>(buf: &TextBuf, out: &mut W, cancel: &CancelToken, progress: Option<&Progress>) -> io::Result<Option<JsonlStats>> {
    if buf.codec.unit() != 1 {
        return Err(io::Error::other("JSON Lines must be UTF-8"));
    }
    if let Some(p) = progress {
        p.set_total(buf.len());
    }
    let mut stats = JsonlStats::default();
    let mut lines = LineReader::new(buf);
    let mut sample: Vec<Vec<u8>> = Vec::new();
    let mut columns: Vec<String> = Vec::new();
    while sample.len() < JSONL_SAMPLE {
        let Some((_, l)) = lines.next()? else { break };
        if let Some(fields) = object_fields(l) {
            for (k, _) in fields {
                if !columns.contains(&k) {
                    columns.push(k);
                }
            }
            sample.push(l.to_vec());
        } else if !l.iter().all(u8::is_ascii_whitespace) {
            stats.skipped += 1;
        }
    }
    stats.columns = columns.len();
    let index: std::collections::HashMap<&str, usize> = columns.iter().enumerate().map(|(i, k)| (k.as_str(), i)).collect();
    writeln!(out, "{}", columns.iter().map(|c| csv_field(c)).collect::<Vec<_>>().join(","))?;
    let mut row = vec![String::new(); columns.len()];
    let mut write_row = |out: &mut W, l: &[u8], stats: &mut JsonlStats| -> io::Result<()> {
        let Some(fields) = object_fields(l) else {
            if !l.iter().all(u8::is_ascii_whitespace) {
                stats.skipped += 1;
            }
            return Ok(());
        };
        row.iter_mut().for_each(String::clear);
        for (k, r) in fields {
            match index.get(k.as_str()) {
                Some(&i) => row[i] = unescape(&l[r]),
                None => stats.late_keys += 1,
            }
        }
        writeln!(out, "{}", row.iter().map(|c| csv_field(c)).collect::<Vec<_>>().join(","))?;
        stats.rows += 1;
        Ok(())
    };
    for l in &sample {
        write_row(out, l, &mut stats)?;
    }
    let mut n = 0u32;
    while let Some((_, l)) = lines.next()? {
        write_row(out, l, &mut stats)?;
        n += 1;
        if n.is_multiple_of(4096) {
            if cancel.is_cancelled() {
                return Ok(None);
            }
            if let Some(p) = progress {
                p.set_done(lines.taken());
            }
        }
    }
    Ok(Some(stats))
}

#[cfg(test)]
mod convert_tests {
    use super::*;
    use crate::encoding::Codec;
    use crate::file::{OpenMode, Source};
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

    #[test]
    fn array_and_lines() {
        let (_f, t) = buf(b"[\n  {\"a\": 1, \"s\": \"x y\\\" ]\"},\n  [1, 2],\n  \"text\", 42, null\n]\n");
        let c = CancelToken::new();
        let at = root(&t, &c).unwrap();
        let mut out = Vec::new();
        assert_eq!(array_to_lines(&t, at, &mut out, &c, None).unwrap(), Some(5));
        assert_eq!(String::from_utf8(out.clone()).unwrap(), "{\"a\":1,\"s\":\"x y\\\" ]\"}\n[1,2]\n\"text\"\n42\nnull\n");
        let (_g, lines) = buf(&out);
        let mut back = Vec::new();
        assert_eq!(lines_to_array(&lines, &mut back, &c, None).unwrap(), Some(5));
        assert!(String::from_utf8(back).unwrap().starts_with("[\n{\"a\":1,"));
        // An array under a key of the top-level object.
        let (_h, o) = buf(b"{\"meta\": {\"n\": 2}, \"events\": [{\"id\": 1}, {\"id\": 2}]}");
        let (at, path) = find_array(&o, &c).unwrap();
        assert_eq!(path, "$.events");
        let mut out = Vec::new();
        assert_eq!(array_to_lines(&o, at, &mut out, &c, None).unwrap(), Some(2));
        assert_eq!(out, b"{\"id\":1}\n{\"id\":2}\n");
        let (_i, bad) = buf(b"[1 2]");
        assert!(array_to_lines(&bad, 0, &mut Vec::new(), &c, None).is_err());
    }
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
        let path = dir.path().join("t.json");
        std::fs::write(&path, text).unwrap();
        let src = Arc::new(Source::open(&path, OpenMode::Auto).unwrap());
        let det = encoding::detect(&src.read(0..src.len()).unwrap(), true);
        let len = src.len();
        (dir, TextBuf::new(src, LineIndex::new(len), det.codec))
    }

    const DOC: &str = r#"{"name": "x", "items": [{"id": 1, "tags": ["a", "b]"]}, {"id": 2}, {"id": 3, "s": "q\"}"}], "a b": null}"#;

    #[test]
    fn children_and_pages() {
        let (_d, b) = buf(DOC);
        let c = CancelToken::new();
        let root = root(&b, &c).unwrap();
        let p = children(&b, root, None, 10, &c).unwrap();
        let keys: Vec<Option<String>> = p.items.iter().map(|x| x.key.clone()).collect();
        assert_eq!(keys, vec![Some("name".into()), Some("items".into()), Some("a b".into())]);
        assert_eq!(p.items[1].kind, Kind::Array);
        assert_eq!(&DOC[p.items[0].start as usize..p.items[0].end as usize], "\"x\"");
        // The array, two items at a time.
        let arr = p.items[1].start;
        let p1 = children(&b, arr, None, 2, &c).unwrap();
        assert_eq!(p1.items.len(), 2);
        let p2 = children(&b, arr, p1.resume, 2, &c).unwrap();
        assert_eq!((p2.items.len(), p2.items[0].index, p2.resume), (1, 2, None));
        assert_eq!(&DOC[p2.items[0].start as usize..p2.items[0].end as usize], r#"{"id": 3, "s": "q\"}"}"#);
    }

    #[test]
    fn paths() {
        let (_d, b) = buf(DOC);
        let c = CancelToken::new();
        let steps = parse_path("$.items[2].s").unwrap();
        assert_eq!(steps, vec![Step::Key("items".into()), Step::Index(2), Step::Key("s".into())]);
        let (s, e) = resolve(&b, &steps, &c).unwrap();
        assert_eq!(&DOC[s as usize..e as usize], r#""q\"}""#);
        assert_eq!(parse_path("items[1][\"id\"]").unwrap(), vec![Step::Key("items".into()), Step::Index(1), Step::Key("id".into())]);
        assert_eq!(resolve(&b, &parse_path("$[\"a b\"]").unwrap(), &c).map(|r| DOC[r.0 as usize..r.1 as usize].to_string()), Ok("null".into()));
        assert!(resolve(&b, &parse_path("$.items[7]").unwrap(), &c).unwrap_err().contains("not found"));
        assert!(resolve(&b, &parse_path("$.name.x").unwrap(), &c).unwrap_err().contains("not an object"));
        // The path at an offset: inside "b]" of the first item's tags.
        let off = DOC.find("b]").unwrap() as u64;
        assert_eq!(format_path(&path_at(&b, off, &c).unwrap()), "$.items[0].tags[1]");
        let off = DOC.find("\"a b\"").unwrap() as u64 + 2;
        assert_eq!(format_path(&path_at(&b, off, &c).unwrap()), "$[\"a b\"]");
        assert_eq!(format_path(&path_at(&b, DOC.find("2}").unwrap() as u64, &c).unwrap()), "$.items[1].id");
    }

    #[test]
    fn strings_unescape() {
        // \u escapes, a surrogate pair among them (built here so the source stays ASCII).
        let u = |hex: &str| format!("{}u{hex}", '\\');
        let raw = format!(r#""a\"b\\c\n{}{}{}""#, u("00e9"), u("d83d"), u("de00"));
        assert_eq!(unescape(raw.as_bytes()), "a\"b\\c\né😀");
        assert_eq!(unescape(b"123"), "123");
    }

    #[test]
    fn json_lines_to_csv() {
        let text = "{\"id\": 1, \"msg\": \"hi, \\\"you\\\"\"}\nnot json\n{\"msg\": \"x\", \"id\": 2, \"extra\": {\"a\": [1]}}\n\n";
        let (_d, b) = buf(text);
        let mut out = Vec::new();
        let st = jsonl_to_csv(&b, &mut out, &CancelToken::new(), None).unwrap().unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "id,msg,extra\n1,\"hi, \"\"you\"\"\",\n2,x,\"{\"\"a\"\": [1]}\"\n");
        assert_eq!(st, JsonlStats { rows: 2, columns: 3, skipped: 1, late_keys: 0 });
    }

    #[test]
    fn big_arrays_page_quickly() {
        let mut text = String::from("[");
        for i in 0..300_000 {
            if i > 0 {
                text.push(',');
            }
            text.push_str(&format!("{{\"i\":{i},\"s\":\"[{{\"}}"));
        }
        text.push(']');
        let (_d, b) = buf(&text);
        let c = CancelToken::new();
        let p = children(&b, 0, None, 100, &c).unwrap();
        assert_eq!(p.items.len(), 100);
        assert!(p.resume.is_some());
        let (s, e) = resolve(&b, &parse_path("$[299999].i").unwrap(), &c).unwrap();
        assert_eq!(&text[s as usize..e as usize], "299999");
    }
}
