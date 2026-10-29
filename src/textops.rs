//! Text operations on a selection: sorting and de-duplicating lines, changing case, trimming,
//! converting indentation, and re-formatting JSON. They work on decoded text and keep the
//! text's own line breaks; the caller applies the result as one undoable edit, or, for a
//! document of any size, [`text_op_to`] streams it to a file.

use std::collections::HashSet;
use std::io::{self, Write};
use std::ops::Range;

use crate::jobs::{CancelToken, Progress};
use crate::text::TextBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Op {
    SortAsc,
    SortDesc,
    /// By the number at the start of each line (lines without one go last, in order).
    SortNumeric,
    Reverse,
    /// Removes repeated lines, keeping the first of each.
    Unique,
    Upper,
    Lower,
    TrimTrailing,
    TabsToSpaces,
    /// Leading spaces only (indentation).
    SpacesToTabs,
    JsonPretty,
    JsonMinify,
    B64Decode,
    B64Encode,
    UrlDecode,
    UrlEncode,
    HexDecode,
    HexEncode,
    HtmlUnescape,
    HtmlEscape,
    JsonUnescape,
    JsonEscape,
}

impl Op {
    pub const ALL: [Op; 22] = [
        Op::SortAsc,
        Op::SortDesc,
        Op::SortNumeric,
        Op::Reverse,
        Op::Unique,
        Op::Upper,
        Op::Lower,
        Op::TrimTrailing,
        Op::TabsToSpaces,
        Op::SpacesToTabs,
        Op::JsonPretty,
        Op::JsonMinify,
        Op::B64Decode,
        Op::B64Encode,
        Op::UrlDecode,
        Op::UrlEncode,
        Op::HexDecode,
        Op::HexEncode,
        Op::HtmlUnescape,
        Op::HtmlEscape,
        Op::JsonUnescape,
        Op::JsonEscape,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Op::SortAsc => "Sort lines A → Z",
            Op::SortDesc => "Sort lines Z → A",
            Op::SortNumeric => "Sort lines by number",
            Op::Reverse => "Reverse line order",
            Op::Unique => "Remove duplicate lines",
            Op::Upper => "UPPERCASE",
            Op::Lower => "lowercase",
            Op::TrimTrailing => "Trim trailing whitespace",
            Op::TabsToSpaces => "Tabs to spaces",
            Op::SpacesToTabs => "Indentation spaces to tabs",
            Op::JsonPretty => "Format JSON (pretty)",
            Op::JsonMinify => "Minify JSON",
            Op::B64Decode => "Base64 decode",
            Op::B64Encode => "Base64 encode",
            Op::UrlDecode => "URL decode",
            Op::UrlEncode => "URL encode",
            Op::HexDecode => "Hex decode",
            Op::HexEncode => "Hex encode",
            Op::HtmlUnescape => "HTML unescape",
            Op::HtmlEscape => "HTML escape",
            Op::JsonUnescape => "JSON string unescape",
            Op::JsonEscape => "JSON string escape",
        }
    }

    /// Decoding or encoding (Base64, URL, hex, HTML, JSON strings): with nothing selected,
    /// they work on the word at the caret.
    pub fn is_codec_op(self) -> bool {
        matches!(
            self,
            Op::B64Decode | Op::B64Encode | Op::UrlDecode | Op::UrlEncode | Op::HexDecode | Op::HexEncode | Op::HtmlUnescape | Op::HtmlEscape | Op::JsonUnescape | Op::JsonEscape
        )
    }

    /// Operations that work line by line (separators between groups in menus).
    pub fn is_line_op(self) -> bool {
        matches!(self, Op::SortAsc | Op::SortDesc | Op::SortNumeric | Op::Reverse | Op::Unique)
    }
}

/// Lines of `text` without their breaks, the break used, and whether the text ended with one.
fn split_lines(text: &str) -> (Vec<&str>, &'static str, bool) {
    let eol = if text.contains("\r\n") {
        "\r\n"
    } else if text.contains('\n') {
        "\n"
    } else if text.contains('\r') {
        "\r"
    } else {
        "\n"
    };
    let body = text.strip_suffix(eol);
    let trailing = body.is_some();
    let body = body.unwrap_or(text);
    let lines = if eol == "\r\n" { body.split("\r\n").collect() } else { body.split(eol).collect() };
    (lines, eol, trailing)
}

fn join(lines: &[impl AsRef<str>], eol: &str, trailing: bool) -> String {
    let mut out = lines.iter().map(|l| l.as_ref()).collect::<Vec<_>>().join(eol);
    if trailing {
        out.push_str(eol);
    }
    out
}

fn leading_number(s: &str) -> Option<f64> {
    let t = s.trim_start();
    let end = t.char_indices().find(|&(i, c)| !(c.is_ascii_digit() || c == '.' || (i == 0 && (c == '-' || c == '+')))).map(|x| x.0).unwrap_or(t.len());
    t[..end].parse().ok()
}

pub fn apply(op: Op, text: &str, tab_width: usize) -> Result<String, String> {
    let (lines, eol, trailing) = split_lines(text);
    Ok(match op {
        Op::SortAsc | Op::SortDesc => {
            let mut v = lines;
            v.sort();
            if op == Op::SortDesc {
                v.reverse();
            }
            join(&v, eol, trailing)
        }
        Op::SortNumeric => {
            let mut v: Vec<(Option<f64>, &str)> = lines.into_iter().map(|l| (leading_number(l), l)).collect();
            v.sort_by(|a, b| match (a.0, b.0) {
                (Some(x), Some(y)) => x.total_cmp(&y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            });
            join(&v.into_iter().map(|x| x.1).collect::<Vec<_>>(), eol, trailing)
        }
        Op::Reverse => {
            let mut v = lines;
            v.reverse();
            join(&v, eol, trailing)
        }
        Op::Unique => {
            let mut seen = HashSet::new();
            let v: Vec<&str> = lines.into_iter().filter(|l| seen.insert(*l)).collect();
            join(&v, eol, trailing)
        }
        Op::Upper => text.to_uppercase(),
        Op::Lower => text.to_lowercase(),
        Op::TrimTrailing => join(&lines.iter().map(|l| l.trim_end_matches([' ', '\t'])).collect::<Vec<_>>(), eol, trailing),
        Op::TabsToSpaces => {
            let v: Vec<String> = lines
                .iter()
                .map(|l| {
                    let mut out = String::with_capacity(l.len());
                    let mut col = 0;
                    for c in l.chars() {
                        if c == '\t' {
                            let n = tab_width - col % tab_width;
                            out.extend(std::iter::repeat_n(' ', n));
                            col += n;
                        } else {
                            out.push(c);
                            col += 1;
                        }
                    }
                    out
                })
                .collect();
            join(&v, eol, trailing)
        }
        Op::SpacesToTabs => {
            let v: Vec<String> = lines
                .iter()
                .map(|l| {
                    let indent = l.len() - l.trim_start_matches(' ').len();
                    format!("{}{}{}", "\t".repeat(indent / tab_width), " ".repeat(indent % tab_width), &l[indent..])
                })
                .collect();
            join(&v, eol, trailing)
        }
        Op::JsonPretty => json_format(text, Some("  "), eol)?,
        Op::JsonMinify => json_format(text, None, eol)?,
        _ => String::from_utf8(codec_apply(op, text)?).map_err(|_| "The result is not text.".to_string())?,
    })
}

/// A decoding or encoding of `text`: the bytes it gives (decoded bytes need not be text).
pub fn codec_apply(op: Op, text: &str) -> Result<Vec<u8>, String> {
    use crate::codec;
    Ok(match op {
        Op::B64Decode => codec::b64_decode(text.trim())?,
        Op::B64Encode => codec::b64_encode(text.as_bytes()).into_bytes(),
        Op::UrlDecode => codec::url_decode(text.trim())?,
        Op::UrlEncode => codec::url_encode(text).into_bytes(),
        Op::HexDecode => codec::hex_decode(text)?,
        Op::HexEncode => codec::hex_encode(text.as_bytes()).into_bytes(),
        Op::HtmlUnescape => codec::html_unescape(text).into_bytes(),
        Op::HtmlEscape => codec::html_escape(text).into_bytes(),
        Op::JsonUnescape => codec::json_unescape(text)?.into_bytes(),
        Op::JsonEscape => codec::json_escape(text).into_bytes(),
        _ => return Err("not a decoding".into()),
    })
}

/// Re-formats JSON token by token: key order, numbers and string escapes stay exactly as
/// written. Several top-level values (JSON Lines) stay on separate lines. With `indent` the
/// output is indented, otherwise all insignificant whitespace is removed.
pub fn json_format(text: &str, indent: Option<&str>, eol: &str) -> Result<String, String> {
    let mut out = String::with_capacity(text.len() + text.len() / 2);
    let mut f = JsonFormatter::new(indent, eol);
    f.feed(text, &mut out)?;
    f.finish(&mut out)?;
    Ok(out)
}

/// [`json_format`] a piece at a time: what it has seen is carried from one piece to the next.
pub struct JsonFormatter<'a> {
    indent: Option<&'a str>,
    eol: &'a str,
    stack: Vec<char>,
    in_str: bool,
    escaped: bool,
    /// A line break (and indentation) is due before the next token.
    pending: bool,
    /// The last token was an opening bracket (so a closing one makes `{}` / `[]`).
    after_open: bool,
    /// A line break between top-level values.
    top_break: bool,
    started: bool,
    line: usize,
    /// The last two characters seen (does the text end with a line break?).
    tail: String,
}

impl<'a> JsonFormatter<'a> {
    pub fn new(indent: Option<&'a str>, eol: &'a str) -> Self {
        JsonFormatter { indent, eol, stack: Vec::new(), in_str: false, escaped: false, pending: false, after_open: false, top_break: false, started: false, line: 1, tail: String::new() }
    }

    fn newline(&self, out: &mut String, depth: usize) {
        if let Some(ind) = self.indent {
            out.push_str(self.eol);
            for _ in 0..depth {
                out.push_str(ind);
            }
        }
    }

    pub fn feed(&mut self, text: &str, out: &mut String) -> Result<(), String> {
        let last: Vec<char> = text.chars().rev().take(2).collect();
        if last.len() == 2 {
            self.tail = last.iter().rev().collect();
        } else {
            self.tail.extend(last.iter().rev());
            let skip = self.tail.chars().count().saturating_sub(2);
            self.tail = self.tail.chars().skip(skip).collect();
        }
        for c in text.chars() {
            if self.in_str {
                out.push(c);
                if self.escaped {
                    self.escaped = false;
                } else if c == '\\' {
                    self.escaped = true;
                } else if c == '"' {
                    self.in_str = false;
                } else if c == '\n' {
                    return Err(format!("Line break inside a JSON string (line {}).", self.line));
                }
                continue;
            }
            if c.is_whitespace() {
                if c == '\n' {
                    self.line += 1;
                    if self.stack.is_empty() && self.started {
                        self.top_break = true;
                    }
                }
                continue;
            }
            let line = self.line;
            if matches!(c, '}' | ']') {
                let open = self.stack.pop().ok_or_else(|| format!("Unexpected “{c}” (line {line})."))?;
                if (open == '{') != (c == '}') {
                    return Err(format!("“{c}” does not close “{open}” (line {line})."));
                }
                if !self.after_open {
                    self.newline(out, self.stack.len());
                }
                out.push(c);
                self.pending = false;
                self.after_open = false;
                continue;
            }
            if self.stack.is_empty() && self.started && self.top_break {
                out.push_str(self.eol);
            }
            self.top_break = false;
            if self.pending {
                self.newline(out, self.stack.len());
                self.pending = false;
            }
            self.after_open = false;
            self.started = true;
            match c {
                '"' => {
                    self.in_str = true;
                    out.push(c);
                }
                '{' | '[' => {
                    out.push(c);
                    self.stack.push(c);
                    self.pending = true;
                    self.after_open = true;
                }
                ',' => {
                    if self.stack.is_empty() {
                        return Err(format!("Unexpected “,” outside an object or array (line {line})."));
                    }
                    out.push(c);
                    self.pending = true;
                }
                ':' => {
                    out.push(':');
                    if self.indent.is_some() {
                        out.push(' ');
                    }
                }
                _ => out.push(c),
            }
        }
        Ok(())
    }

    pub fn finish(self, out: &mut String) -> Result<(), String> {
        if self.in_str {
            return Err("A JSON string is not closed.".into());
        }
        if let Some(open) = self.stack.last() {
            return Err(format!("“{open}” is not closed."));
        }
        if self.tail.ends_with(self.eol) {
            out.push_str(self.eol);
        }
        Ok(())
    }
}

impl Op {
    /// Whether it can work through a document a piece at a time (sorting cannot).
    pub fn streams(self) -> bool {
        matches!(self, Op::Upper | Op::Lower | Op::TrimTrailing | Op::TabsToSpaces | Op::SpacesToTabs | Op::JsonPretty | Op::JsonMinify)
    }
}

/// Bytes decoded, changed and written at a time (whole lines).
pub const WINDOW: u64 = 4 << 20;
/// The longest line trimming and indentation changes work on.
const LINE_MAX: u64 = 64 << 20;

fn copy_range(buf: &TextBuf, r: Range<u64>, out: &mut dyn Write) -> io::Result<u64> {
    let mut err = None;
    buf.for_each_chunk(r.clone(), |_, b| match out.write_all(b) {
        Ok(()) => true,
        Err(e) => {
            err = Some(e);
            false
        }
    })
    .ok_or_else(|| io::Error::other("Could not read the document"))?;
    match err {
        Some(e) => Err(e),
        None => Ok(r.end - r.start),
    }
}

/// Writes `buf` with `op` applied to `range` (the rest as it is) to `out`, whole lines at a
/// time, for documents of any size. Returns the bytes written, or `None` when cancelled.
pub fn text_op_to(buf: &TextBuf, range: Range<u64>, op: Op, tab_width: usize, out: &mut dyn Write, cancel: &CancelToken, progress: Option<&Progress>) -> io::Result<Option<u64>> {
    text_op_window(buf, range, op, tab_width, out, cancel, progress, WINDOW)
}

#[allow(clippy::too_many_arguments)]
fn text_op_window(buf: &TextBuf, range: Range<u64>, op: Op, tab_width: usize, out: &mut dyn Write, cancel: &CancelToken, progress: Option<&Progress>, window: u64) -> io::Result<Option<u64>> {
    if !op.streams() {
        return Err(io::Error::new(io::ErrorKind::Unsupported, "this operation needs all the lines at once"));
    }
    let bad = |e: String| io::Error::new(io::ErrorKind::InvalidData, e);
    let codec = buf.codec;
    let unit = codec.unit();
    let range = range.start.min(buf.len())..range.end.min(buf.len());
    if let Some(p) = progress {
        p.set_total(buf.len());
    }
    let mut written = copy_range(buf, 0..range.start, out)?;
    let mut json: Option<JsonFormatter> = None;
    let whole_lines = matches!(op, Op::TrimTrailing | Op::TabsToSpaces | Op::SpacesToTabs);
    let mut text_out = String::new();
    let mut pos = range.start;
    let mut first = true;
    while pos < range.end {
        if cancel.is_cancelled() {
            return Ok(None);
        }
        // Whole lines (or whole characters, in a line longer than the window).
        let mut w = window;
        let (bytes, cut) = loop {
            let end = (pos + w).min(range.end);
            let bytes = buf.read(pos..end).ok_or_else(|| io::Error::other("Could not read the document"))?;
            if end == range.end {
                let n = bytes.len();
                break (bytes, n);
            }
            let cut = match codec.rfind_nl(&bytes, pos) {
                Some(i) => i + unit,
                // Trimming and indentation look at a whole line.
                None if whole_lines => {
                    if w >= LINE_MAX {
                        return Err(bad(format!("A line is longer than {} MB.", LINE_MAX >> 20)));
                    }
                    0
                }
                None => {
                    let mut c = bytes.len() - bytes.len() % unit;
                    if codec.enc == crate::encoding::Encoding::Utf8 {
                        // Leave out the last character: it may go on past the window.
                        while c > 0 && bytes[c - 1] & 0xC0 == 0x80 {
                            c -= 1;
                        }
                        if c > 0 && bytes[c - 1] >= 0xC0 {
                            c -= 1;
                        }
                    } else if unit == 2 && c >= 2 {
                        let u = if codec.enc == crate::encoding::Encoding::Utf16Be { u16::from_be_bytes([bytes[c - 2], bytes[c - 1]]) } else { u16::from_le_bytes([bytes[c - 2], bytes[c - 1]]) };
                        if (0xD800..0xDC00).contains(&u) {
                            c -= 2;
                        }
                    }
                    c
                }
            };
            if cut > 0 {
                break (bytes, cut);
            }
            w *= 2;
        };
        let chunk = &bytes[..cut];
        let text = codec.decode_lossy(chunk);
        if codec.encode(&text) != chunk {
            return Err(bad(format!("The text contains bytes that are not valid {}; the operation would change them.", codec.enc.name())));
        }
        if first {
            first = false;
            let eol = if text.contains("\r\n") { "\r\n" } else if text.contains('\n') || !text.contains('\r') { "\n" } else { "\r" };
            json = match op {
                Op::JsonPretty => Some(JsonFormatter::new(Some("  "), eol)),
                Op::JsonMinify => Some(JsonFormatter::new(None, eol)),
                _ => None,
            };
        }
        let changed = match &mut json {
            Some(f) => {
                text_out.clear();
                f.feed(&text, &mut text_out).map_err(bad)?;
                codec.encode(&text_out)
            }
            None => codec.encode(&apply(op, &text, tab_width).map_err(bad)?),
        };
        out.write_all(&changed)?;
        written += changed.len() as u64;
        pos += cut as u64;
        if let Some(p) = progress {
            p.set_done(pos);
        }
    }
    if let Some(f) = json {
        text_out.clear();
        f.finish(&mut text_out).map_err(bad)?;
        let b = codec.encode(&text_out);
        out.write_all(&b)?;
        written += b.len() as u64;
    }
    written += copy_range(buf, range.end..buf.len(), out)?;
    Ok(Some(written))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_ops_keep_breaks() {
        assert_eq!(apply(Op::SortAsc, "b\r\na\r\nc\r\n", 4).unwrap(), "a\r\nb\r\nc\r\n");
        assert_eq!(apply(Op::SortDesc, "b\na\nc", 4).unwrap(), "c\nb\na");
        assert_eq!(apply(Op::SortNumeric, "10 x\n9 y\nz\n-1 w\n", 4).unwrap(), "-1 w\n9 y\n10 x\nz\n");
        assert_eq!(apply(Op::Reverse, "1\n2\n3\n", 4).unwrap(), "3\n2\n1\n");
        assert_eq!(apply(Op::Unique, "a\nb\na\nc\nb\n", 4).unwrap(), "a\nb\nc\n");
        assert_eq!(apply(Op::TrimTrailing, "a  \nb\t\n", 4).unwrap(), "a\nb\n");
    }

    #[test]
    fn case_and_indentation() {
        assert_eq!(apply(Op::Upper, "straße привет", 4).unwrap(), "STRASSE ПРИВЕТ");
        assert_eq!(apply(Op::Lower, "ÄB", 4).unwrap(), "äb");
        assert_eq!(apply(Op::TabsToSpaces, "\tx\n ab\tc", 4).unwrap(), "    x\n ab c");
        assert_eq!(apply(Op::SpacesToTabs, "      x  y\n  z", 4).unwrap(), "\t  x  y\n  z");
    }

    fn buf(data: &[u8]) -> (tempfile::NamedTempFile, TextBuf) {
        use std::io::Write as _;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(data).unwrap();
        f.flush().unwrap();
        let s = std::sync::Arc::new(crate::file::Source::open(f.path(), crate::file::OpenMode::Auto).unwrap());
        let det = crate::encoding::detect(data, true);
        (f, TextBuf::new(s.clone(), crate::index::LineIndex::new(s.len()), det.codec))
    }

    #[test]
    fn streaming_matches_in_memory_at_any_window() {
        for text in ["Straße\tone  \n  two\tTWO \nпривет мир\t\n\n    four\nlast", "a\t \r\n    b  \r\nc"] {
            let (_f, b) = buf(text.as_bytes());
            for op in [Op::Upper, Op::Lower, Op::TrimTrailing, Op::TabsToSpaces, Op::SpacesToTabs] {
                let want = apply(op, text, 4).unwrap();
                for window in [1, 3, 7, 16, 1000] {
                    let mut out = Vec::new();
                    text_op_window(&b, 0..b.len(), op, 4, &mut out, &CancelToken::new(), None, window).unwrap().unwrap();
                    assert_eq!(String::from_utf8(out).unwrap(), want, "{op:?} at {window}");
                }
            }
        }
        let text = "Straße\tone  \n  two\tTWO \nпривет мир\t\n\n    four\nlast";
        let (_f, b) = buf(text.as_bytes());
        // Only the range changes.
        let mut out = Vec::new();
        text_op_window(&b, 8..13, Op::Upper, 4, &mut out, &CancelToken::new(), None, 2).unwrap().unwrap();
        assert!(String::from_utf8(out).unwrap().starts_with("Straße\tONE  \n"));
        assert!(text_op_to(&b, 0..b.len(), Op::SortAsc, 4, &mut Vec::new(), &CancelToken::new(), None).is_err());
    }

    #[test]
    fn json_streams_too() {
        let src = "{\"b\":1,\"a\":[1, 2.50,{}],\"s\":\"x, \\\"y\\\": [z]\",\"e\":[]}\n{\"n\": null}\n";
        let (_f, b) = buf(src.as_bytes());
        for (op, indent) in [(Op::JsonPretty, Some("  ")), (Op::JsonMinify, None)] {
            let want = json_format(src, indent, "\n").unwrap();
            for window in [1, 5, 13, 4096] {
                let mut out = Vec::new();
                text_op_window(&b, 0..b.len(), op, 4, &mut out, &CancelToken::new(), None, window).unwrap().unwrap();
                assert_eq!(String::from_utf8(out).unwrap(), want, "{op:?} at {window}");
            }
        }
        let (_g, broken) = buf(b"{\"a\": [1}");
        assert!(text_op_to(&broken, 0..broken.len(), Op::JsonMinify, 4, &mut Vec::new(), &CancelToken::new(), None).is_err());
    }

    #[test]
    fn json_pretty_and_minify() {
        let src = r#"{"b":1,"a":[1, 2.50,{}],"s":"x, \"y\": [z]","e":[]}"#;
        let pretty = json_format(src, Some("  "), "\n").unwrap();
        assert_eq!(pretty, "{\n  \"b\": 1,\n  \"a\": [\n    1,\n    2.50,\n    {}\n  ],\n  \"s\": \"x, \\\"y\\\": [z]\",\n  \"e\": []\n}");
        assert_eq!(json_format(&pretty, None, "\n").unwrap(), r#"{"b":1,"a":[1,2.50,{}],"s":"x, \"y\": [z]","e":[]}"#);
        // JSON Lines stay one value per line.
        assert_eq!(json_format("{\"a\": 1}\n{\"b\": [ 2 ]}\n", None, "\n").unwrap(), "{\"a\":1}\n{\"b\":[2]}\n");
        assert!(json_format("{\"a\": [1}", None, "\n").unwrap_err().contains("does not close"));
        assert!(json_format("[1", None, "\n").unwrap_err().contains("not closed"));
    }
}
