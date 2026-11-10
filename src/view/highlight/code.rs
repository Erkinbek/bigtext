//! Programming languages: one tokenizer for the C family (`//` and `/* */` comments) and the `#`
//! family, with each language's strings, keywords and quirks in [`Spec`] and in the few places
//! below that ask which language this is.
//!
//! Names are coloured by a few rules instead of a parser: keywords; the name after `class` or
//! `new` is a type, the name after `def` or `fn` a function; a name followed by `(` is a call;
//! known built-in types and names in PascalCase are types; names in UPPER_CASE are constants.

use super::{is_ident, Kind, Lang, Mode, Sink, State, Str, DELIM};

/// A language's comments and words. Word lists are sorted (binary search).
pub(super) struct Spec {
    /// Line comment openers.
    pub line: &'static [&'static str],
    /// `/* */` comments, nested when `nest`.
    pub block: bool,
    pub nest: bool,
    pub keywords: &'static [&'static str],
    /// Built-in type names.
    pub types: &'static [&'static str],
    /// After one of these the next name is a type (`class Foo`, `new Foo`).
    pub type_kw: &'static [&'static str],
    /// After one of these the next name is a function (`def foo`, `fn foo`).
    pub fn_kw: &'static [&'static str],
    /// Names in PascalCase are types.
    pub pascal: bool,
    /// `@Name` annotations and decorators.
    pub at_attr: bool,
    /// Words that begin a declaration at the start of a line: after a jump, highlighting trusts
    /// such a line to be outside comments and strings.
    pub sync: &'static [&'static str],
}

pub(super) fn spec(lang: Lang) -> Option<&'static Spec> {
    use super::words::*;
    Some(match lang {
        Lang::C => &C,
        Lang::CSharp => &CSHARP,
        Lang::Java => &JAVA,
        Lang::JavaScript => &JS,
        Lang::TypeScript => &TS,
        Lang::Go => &GO,
        Lang::Rust => &RUST,
        _ => return None,
    })
}

fn has(list: &[&str], w: &[u8]) -> bool {
    list.binary_search_by(|k| k.as_bytes().cmp(w)).is_ok()
}

/// True if `line` (from its start) begins with one of the language's declaration words.
pub(super) fn sync_line(lang: Lang, line: &str) -> bool {
    let Some(spec) = spec(lang) else { return false };
    let t = line.as_bytes();
    spec.sync.iter().any(|w| {
        let w = w.as_bytes();
        t.starts_with(w) && (!is_ident(w[w.len() - 1]) || t.get(w.len()).is_none_or(|&b| !is_ident(b)))
    })
}

/// What the previous token was, for telling `a.b` (a member) from `b` and so on.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Prev {
    Start,
    /// `.` or `->`: the next name is a member, never a keyword.
    Dot,
    /// `::`
    Path,
    Name,
    Value,
    /// `)` or `]`: what follows is an operator, as after a value.
    Close,
    Punct,
}

fn is_js(lang: Lang) -> bool {
    matches!(lang, Lang::JavaScript | Lang::TypeScript)
}

/// A JavaScript regular expression at `t[i]` (a `/`, where an operand may start): its end, past
/// the flags. `None` when the line ends first (then it was a division after all).
fn regex_end(t: &[u8], i: usize) -> Option<usize> {
    let mut j = i + 1;
    let mut class = false;
    while j < t.len() {
        match t[j] {
            b'\\' => j += 1,
            b'[' => class = true,
            b']' => class = false,
            b'/' if !class => {
                j += 1;
                while j < t.len() && t[j].is_ascii_alphabetic() {
                    j += 1;
                }
                return Some(j);
            }
            _ => {}
        }
        j += 1;
    }
    None
}

pub(super) fn spans(lang: Lang, st: &mut State, t: &[u8], out: &mut Sink) {
    let Some(spec) = spec(lang) else { return };
    let colour = out.0.is_some();
    let mut i = 0;
    let mut prev = Prev::Start;
    // The kind of the next name, after `class`, `def` and the like.
    let mut next: Option<Kind> = None;
    // The last name, for `return /re/`.
    let mut last_name = (0, 0);
    while i < t.len() {
        // Finish what an earlier row or token left open.
        match st.mode {
            Mode::Code => {}
            Mode::LineComment => {
                out.push(i, t.len(), Kind::Comment);
                return;
            }
            Mode::Comment(_) => {
                i = block_comment(st, t, i, i, spec.nest, out);
                continue;
            }
            Mode::Str(_) => {
                i = string(st, t, i, i, out);
                prev = Prev::Value;
                continue;
            }
            Mode::Delim => {
                i = delimited(st, t, i, i, out);
                prev = Prev::Value;
                continue;
            }
            _ => st.mode = Mode::Code,
        }
        let c = t[i];
        if c == b' ' || c == b'\t' || c == b'\r' {
            i += 1;
            continue;
        }
        let first = !st.inked;
        st.inked = true;
        if spec.line.iter().any(|o| t[i..].starts_with(o.as_bytes())) {
            out.push(i, t.len(), Kind::Comment);
            st.mode = Mode::LineComment;
            return;
        }
        if spec.block && t[i..].starts_with(b"/*") {
            st.mode = Mode::Comment(1);
            i = block_comment(st, t, i, i + 2, spec.nest, out);
            continue;
        }
        if first && c == b'#' && matches!(lang, Lang::C | Lang::CSharp) {
            i = directive(t, i, out);
            prev = Prev::Punct;
            continue;
        }
        if lang == Lang::Rust {
            if c == b'\'' {
                i = rust_quote(t, i, out);
                prev = Prev::Value;
                continue;
            }
            if c == b'#' && (t[i + 1..].starts_with(b"[") || t[i + 1..].starts_with(b"![")) {
                // #[derive(Debug)], #![allow(x)]: up to the matching bracket on this row.
                let mut depth = 0;
                let mut e = i;
                while e < t.len() {
                    match t[e] {
                        b'[' => depth += 1,
                        b']' => {
                            depth -= 1;
                            if depth == 0 {
                                e += 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                    e += 1;
                }
                out.push(i, e, Kind::Attr);
                i = e;
                prev = Prev::Punct;
                continue;
            }
        }
        if let Some((open, body)) = open_string(lang, t, i) {
            st.mode = open;
            i = match open {
                Mode::Delim => delimited(st, t, i, body, out),
                _ => string(st, t, i, body, out),
            };
            prev = Prev::Value;
            next = None;
            continue;
        }
        if c == b'/' && is_js(lang) {
            let operand = match prev {
                Prev::Start | Prev::Punct => true,
                Prev::Name => super::words::JS_BEFORE_REGEX.iter().any(|k| k.as_bytes() == &t[last_name.0..last_name.1]),
                _ => false,
            };
            if let Some(e) = regex_end(t, i).filter(|_| operand) {
                out.push(i, e, Kind::Str);
                i = e;
                prev = Prev::Value;
                continue;
            }
        }
        if c.is_ascii_digit() {
            let e = number_end(lang, t, i);
            out.push(i, e, Kind::Number);
            i = e;
            prev = Prev::Value;
            continue;
        }
        if is_ident(c) {
            let e = super::word_end(t, i);
            // A string with a prefix: L"wide", u8"text", R"(raw)".
            if let Some((open, body)) = prefixed_string(lang, st, &t[i..e], t, e) {
                st.mode = open;
                i = match open {
                    Mode::Delim => delimited(st, t, i, body, out),
                    _ => string(st, t, i, body, out),
                };
                prev = Prev::Value;
                next = None;
                continue;
            }
            // A Rust macro: println!(…), vec![…], macro_rules! name.
            if lang == Lang::Rust && t.get(e) == Some(&b'!') && t.get(e + 1) != Some(&b'=') {
                out.push(i, e + 1, Kind::Function);
                i = e + 1;
                prev = Prev::Name;
                next = None;
                continue;
            }
            if colour {
                let w = &t[i..e];
                let kind = if prev != Prev::Dot && has(spec.keywords, w) {
                    next = if spec.type_kw.iter().any(|k| k.as_bytes() == w) {
                        Some(Kind::Type)
                    } else if spec.fn_kw.iter().any(|k| k.as_bytes() == w) {
                        Some(Kind::Function)
                    } else {
                        None
                    };
                    Some(Kind::Keyword)
                } else {
                    next.take().or_else(|| name_kind(spec, w, t, e, prev))
                };
                if let Some(k) = kind {
                    out.push(i, e, k);
                }
            }
            last_name = (i, e);
            i = e;
            prev = Prev::Name;
            continue;
        }
        if c == b'@' && spec.at_attr && t.get(i + 1).is_some_and(|&b| is_ident(b)) {
            let mut e = i + 1;
            while e < t.len() && (is_ident(t[e]) || t[e] == b'.') {
                e += 1;
            }
            out.push(i, e, Kind::Attr);
            i = e;
            prev = Prev::Name;
            continue;
        }
        prev = match c {
            b'.' => Prev::Dot,
            // `->` reaches a member only in C++ (in Rust and TypeScript it comes before a type).
            b'>' if i > 0 && t[i - 1] == b'-' && lang == Lang::C => Prev::Dot,
            b':' if i > 0 && t[i - 1] == b':' => Prev::Path,
            b')' | b']' => Prev::Close,
            _ => Prev::Punct,
        };
        next = None;
        i += 1;
    }
}

/// The colour of a name that is not a keyword.
fn name_kind(spec: &Spec, w: &[u8], t: &[u8], e: usize, prev: Prev) -> Option<Kind> {
    let mut j = e;
    while j < t.len() && (t[j] == b' ' || t[j] == b'\t') {
        j += 1;
    }
    if t.get(j) == Some(&b'(') {
        return Some(Kind::Function);
    }
    if prev != Prev::Dot && has(spec.types, w) {
        return Some(Kind::Type);
    }
    let upper = w.iter().filter(|b| b.is_ascii_uppercase()).count();
    let lower = w.iter().filter(|b| b.is_ascii_lowercase()).count();
    if spec.pascal && w[0].is_ascii_uppercase() && lower > 0 && w.len() > 1 {
        return Some(Kind::Type);
    }
    if upper >= 2 && lower == 0 {
        return Some(Kind::Number);
    }
    None
}

fn number_end(lang: Lang, t: &[u8], i: usize) -> usize {
    let hex = t[i..].starts_with(b"0x") || t[i..].starts_with(b"0X");
    let mut j = i;
    while j < t.len() {
        let b = t[j];
        if b.is_ascii_alphanumeric() || b == b'_' || (b == b'.' && t.get(j + 1).is_some_and(u8::is_ascii_digit)) {
            j += 1;
        } else if (b == b'+' || b == b'-') && !hex && matches!(t[j - 1], b'e' | b'E') {
            j += 1;
        } else if b == b'\'' && lang == Lang::C && t.get(j + 1).is_some_and(u8::is_ascii_alphanumeric) {
            // 1'000'000
            j += 1;
        } else {
            break;
        }
    }
    j
}

/// `#include <x.h>`, `#define`, `#region`: the directive as a keyword, an included header as a
/// string.
fn directive(t: &[u8], i: usize, out: &mut Sink) -> usize {
    let mut e = i + 1;
    while e < t.len() && (t[e] == b' ' || t[e] == b'\t') {
        e += 1;
    }
    let w = e;
    let e = super::word_end(t, e);
    out.push(i, e, Kind::Keyword);
    if matches!(&t[w..e], b"include" | b"import") {
        let mut j = e;
        while j < t.len() && t[j] == b' ' {
            j += 1;
        }
        if t.get(j) == Some(&b'<') {
            if let Some(p) = memchr::memchr(b'>', &t[j..]) {
                out.push(j, j + p + 1, Kind::Str);
                return j + p + 1;
            }
        }
    }
    e
}

/// A string opening at `t[i]`: its mode and where its text starts.
fn open_string(lang: Lang, t: &[u8], i: usize) -> Option<(Mode, usize)> {
    let c = t[i];
    let three = t[i..].starts_with(b"\"\"\"");
    match lang {
        Lang::Java if three => Some((Mode::Str(Str { q: b'"', n: 3, esc: true, ml: true, dbl: false }), i + 3)),
        // A template literal; what is inside `${}` is coloured as the string.
        Lang::JavaScript | Lang::TypeScript if c == b'`' => Some((Mode::Str(Str { q: c, n: 1, esc: true, ml: true, dbl: false }), i + 1)),
        // Go's raw strings; Rust's strings may span lines.
        Lang::Go if c == b'`' => Some((Mode::Str(Str { q: c, n: 1, esc: false, ml: true, dbl: false }), i + 1)),
        Lang::Rust if c == b'"' => Some((Mode::Str(Str { q: c, n: 1, esc: true, ml: true, dbl: false }), i + 1)),
        Lang::CSharp if c == b'@' || c == b'$' || three => {
            let mut j = i;
            let mut verbatim = false;
            while j < t.len() && j < i + 3 && (t[j] == b'@' || t[j] == b'$') {
                verbatim |= t[j] == b'@';
                j += 1;
            }
            if t.get(j) != Some(&b'"') {
                return None;
            }
            Some(if t[j..].starts_with(b"\"\"\"") {
                (Mode::Str(Str { q: b'"', n: 3, esc: false, ml: true, dbl: false }), j + 3)
            } else if verbatim {
                (Mode::Str(Str { q: b'"', n: 1, esc: false, ml: true, dbl: true }), j + 1)
            } else {
                (Mode::Str(Str { q: b'"', n: 1, esc: true, ml: false, dbl: false }), j + 1)
            })
        }
        _ if c == b'"' || c == b'\'' => Some((Mode::Str(Str { q: c, n: 1, esc: true, ml: false, dbl: false }), i + 1)),
        _ => None,
    }
}

/// A string whose prefix is the name `w` (ending at `e`): C's `L"…"`, `u8"…"` and raw `R"x(…)x"`;
/// Rust's `b"…"`, `b'x'` and raw `r#"…"#`.
fn prefixed_string(lang: Lang, st: &mut State, w: &[u8], t: &[u8], e: usize) -> Option<(Mode, usize)> {
    let q = *t.get(e)?;
    if q != b'"' && q != b'\'' && !(lang == Lang::Rust && q == b'#') {
        return None;
    }
    match lang {
        Lang::Rust => match (w, q) {
            (b"b" | b"c", b'"') => Some((Mode::Str(Str { q, n: 1, esc: true, ml: true, dbl: false }), e + 1)),
            (b"b", b'\'') => Some((Mode::Str(Str { q, n: 1, esc: true, ml: false, dbl: false }), e + 1)),
            (b"r" | b"br" | b"cr", _) => {
                let hashes = t[e..].iter().take_while(|&&b| b == b'#').count();
                if t.get(e + hashes) != Some(&b'"') || hashes + 1 > DELIM {
                    return None;
                }
                st.set_delim(&[&b"\""[..], &[b'#'; DELIM][..hashes]].concat());
                Some((Mode::Delim, e + hashes + 1))
            }
            _ => None,
        },
        Lang::C if q == b'"' && matches!(w, b"R" | b"LR" | b"uR" | b"UR" | b"u8R") => {
            // R"delim( … )delim"
            let d = &t[e + 1..];
            let p = d.iter().take(17).position(|&b| b == b'(')?;
            if p + 2 > DELIM || d[..p].iter().any(|&b| matches!(b, b' ' | b')' | b'\\' | b'\t')) {
                return None;
            }
            st.set_delim(&[b")", &d[..p], b"\""].concat());
            Some((Mode::Delim, e + 1 + p + 1))
        }
        Lang::C if matches!(w, b"L" | b"u" | b"U" | b"u8") => Some((Mode::Str(Str { q, n: 1, esc: true, ml: false, dbl: false }), e + 1)),
        _ => None,
    }
}

/// Rust's `'`: a character (`'a'`, `'\n'`, `'é'`) or a lifetime or label (`'a`, `'static`).
fn rust_quote(t: &[u8], i: usize, out: &mut Sink) -> usize {
    let rest = &t[i + 1..];
    if rest.first() == Some(&b'\\') {
        let e = rest.iter().skip(2).position(|&b| b == b'\'').map_or(t.len(), |p| i + 1 + 2 + p + 1);
        out.push(i, e, Kind::Str);
        return e;
    }
    if let Some(&b) = rest.first() {
        let len = match b {
            0xF0.. => 4,
            0xE0.. => 3,
            0xC0.. => 2,
            _ => 1,
        };
        if rest.get(len) == Some(&b'\'') {
            out.push(i, i + len + 2, Kind::Str);
            return i + len + 2;
        }
        if is_ident(b) {
            let e = super::word_end(t, i + 1);
            out.push(i, e, Kind::Tag);
            return e;
        }
    }
    i + 1
}

/// A block comment from `from` (the comment's text pushed from `start`); returns where it ends.
fn block_comment(st: &mut State, t: &[u8], start: usize, from: usize, nest: bool, out: &mut Sink) -> usize {
    let Mode::Comment(mut depth) = st.mode else { return from };
    let mut j = from;
    while j + 1 < t.len() {
        if t[j] == b'*' && t[j + 1] == b'/' {
            depth -= 1;
            j += 2;
            if depth == 0 {
                st.mode = Mode::Code;
                out.push(start, j, Kind::Comment);
                return j;
            }
        } else if nest && t[j] == b'/' && t[j + 1] == b'*' {
            depth = depth.saturating_add(1);
            j += 2;
        } else {
            j += 1;
        }
    }
    st.mode = Mode::Comment(depth);
    out.push(start, t.len(), Kind::Comment);
    t.len()
}

/// A string from `from` (pushed from `start`); returns where it ends.
fn string(st: &mut State, t: &[u8], start: usize, from: usize, out: &mut Sink) -> usize {
    let Mode::Str(s) = st.mode else { return from };
    let mut j = from;
    while j < t.len() {
        let b = t[j];
        if s.esc && b == b'\\' {
            j += 2;
            continue;
        }
        if b == s.q {
            if s.n == 1 {
                if s.dbl && t.get(j + 1) == Some(&s.q) {
                    j += 2;
                    continue;
                }
                j += 1;
            } else if t[j..].starts_with(&[s.q; 3]) {
                j += 3;
                while j < t.len() && t[j] == s.q {
                    j += 1;
                }
            } else {
                j += 1;
                continue;
            }
            st.mode = Mode::Code;
            out.push(start, j, Kind::Str);
            return j;
        }
        j += 1;
    }
    out.push(start, t.len(), Kind::Str);
    t.len()
}

/// A raw string closed by the state's delimiter, from `from` (pushed from `start`).
fn delimited(st: &mut State, t: &[u8], start: usize, from: usize, out: &mut Sink) -> usize {
    let (e, closed) = super::close(t, from, st.delim());
    if closed {
        st.mode = Mode::Code;
    }
    out.push(start, e, Kind::Str);
    e
}
