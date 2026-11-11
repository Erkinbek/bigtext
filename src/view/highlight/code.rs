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
    /// Block comments (`/* */`, `<# #>`, `--[[ ]]`; Ruby's `=begin`/`=end` at line starts),
    /// nested when `nest`.
    pub block: Option<(&'static str, &'static str)>,
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
    /// Keywords ignore case (the list is in lower case).
    pub ci: bool,
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
        Lang::Php => &PHP,
        Lang::Python => &PYTHON,
        Lang::Shell => &SHELL,
        Lang::Kotlin => &KOTLIN,
        Lang::Swift => &SWIFT,
        Lang::Dart => &DART,
        Lang::Ruby => &RUBY,
        Lang::Lua => &LUA,
        Lang::PowerShell => &POWERSHELL,
        _ => return None,
    })
}

fn has(list: &[&str], w: &[u8]) -> bool {
    list.binary_search_by(|k| k.as_bytes().cmp(w)).is_ok()
}

/// Like [`has`], ignoring case when `ci` (for a list in lower case).
fn has_word(list: &[&str], w: &[u8], ci: bool) -> bool {
    if !ci {
        return has(list, w);
    }
    let mut buf = [0u8; 32];
    if w.len() > buf.len() {
        return false;
    }
    buf[..w.len()].copy_from_slice(w);
    buf[..w.len()].make_ascii_lowercase();
    has(list, &buf[..w.len()])
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
                i = block_comment(st, t, i, i, spec, out);
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
            Mode::Heredoc => {
                i = heredoc(st, t, i, out);
                prev = Prev::Value;
                continue;
            }
            Mode::Html => {
                i = php_html(st, t, i, out);
                prev = Prev::Start;
                continue;
            }
            Mode::Tag | Mode::CData => st.mode = Mode::Code,
        }
        let c = t[i];
        if c == b' ' || c == b'\t' || c == b'\r' {
            i += 1;
            continue;
        }
        let first = !st.inked;
        st.inked = true;
        // In a shell `#` starts a comment only at the start of a word (not in `a#b` or `${#x}`).
        let shell_word = lang != Lang::Shell || i == 0 || matches!(t[i - 1], b' ' | b'\t' | b';' | b'|' | b'&' | b'(' | b')');
        // A block comment first: Lua's `--[[` starts like its line comment.
        if let Some((open, _)) = spec.block.filter(|(o, _)| t[i..].starts_with(o.as_bytes()) && (!o.starts_with('=') || i == 0)) {
            st.mode = Mode::Comment(1);
            i = block_comment(st, t, i, i + open.len(), spec, out);
            continue;
        }
        if spec.line.iter().any(|o| t[i..].starts_with(o.as_bytes())) && !(lang == Lang::Php && t[i..].starts_with(b"#[")) && shell_word {
            // In PHP a line comment ends at `?>` too.
            if let Some(p) = memchr::memmem::find(&t[i..], b"?>").filter(|_| lang == Lang::Php) {
                out.push(i, i + p, Kind::Comment);
                i += p;
                continue;
            }
            out.push(i, t.len(), Kind::Comment);
            st.mode = Mode::LineComment;
            return;
        }
        if first && c == b'#' && matches!(lang, Lang::C | Lang::CSharp | Lang::Swift) {
            i = directive(t, i, out);
            prev = Prev::Punct;
            continue;
        }
        if lang == Lang::Php {
            if t[i..].starts_with(b"?>") {
                out.push(i, i + 2, Kind::Tag);
                st.mode = Mode::Html;
                i += 2;
                continue;
            }
            if let Some(len) = php_open(t, i).filter(|&n| n > 2) {
                out.push(i, i + len, Kind::Tag);
                i += len;
                continue;
            }
            if c == b'$' && t.get(i + 1).is_some_and(|&b| is_ident(b) || b == b'$') {
                let mut e = i + 1;
                while e < t.len() && t[e] == b'$' {
                    e += 1;
                }
                let e = super::word_end(t, e);
                out.push(i, e, Kind::Var);
                i = e;
                prev = Prev::Name;
                continue;
            }
        }
        if lang == Lang::Shell && c == b'$' {
            // $name, ${name…}, $1, $?, $@ and $'…' (a string with escapes).
            let e = match t.get(i + 1) {
                Some(b'{') => Some(memchr::memchr(b'}', &t[i..]).map_or(t.len(), |p| i + p + 1)),
                Some(&b) if is_ident(b) => Some(super::word_end(t, i + 1)),
                Some(b'@' | b'?' | b'#' | b'$' | b'!' | b'*' | b'-') => Some(i + 2),
                _ => None,
            };
            if let Some(e) = e {
                out.push(i, e, Kind::Var);
                i = e;
                prev = Prev::Name;
                continue;
            }
            if t.get(i + 1) == Some(&b'\'') {
                st.mode = Mode::Str(Str { q: b'\'', n: 1, esc: true, ml: true, dbl: false });
                i = string(st, t, i, i + 2, out);
                prev = Prev::Value;
                continue;
            }
        }
        // Ruby's @instance, @@class and $global variables, and :symbols.
        if lang == Lang::Ruby {
            if (c == b'@' || c == b'$') && t.get(i + 1).is_some_and(|&b| is_ident(b) || b == b'@') {
                let mut e = i + 1;
                while t.get(e) == Some(&b'@') {
                    e += 1;
                }
                let e = super::word_end(t, e);
                out.push(i, e, Kind::Var);
                i = e;
                prev = Prev::Name;
                continue;
            }
            if c == b':' && t.get(i + 1).is_some_and(|&b| is_ident(b) && !b.is_ascii_digit()) && (i == 0 || (t[i - 1] != b':' && !is_ident(t[i - 1]))) {
                let mut e = super::word_end(t, i + 1);
                if matches!(t.get(e), Some(b'?' | b'!')) {
                    e += 1;
                }
                out.push(i, e, Kind::Number);
                i = e;
                prev = Prev::Value;
                continue;
            }
        }
        if lang == Lang::PowerShell {
            // $name, ${any name}, $env:PATH, $script:x, $_, $?
            if c == b'$' {
                let e = match t.get(i + 1) {
                    Some(b'{') => Some(memchr::memchr(b'}', &t[i..]).map_or(t.len(), |p| i + p + 1)),
                    Some(&b) if is_ident(b) => {
                        let e = super::word_end(t, i + 1);
                        Some(if t.get(e) == Some(&b':') && t.get(e + 1).is_some_and(|&b| is_ident(b)) { super::word_end(t, e + 1) } else { e })
                    }
                    Some(b'?' | b'$' | b'^') => Some(i + 2),
                    _ => None,
                };
                if let Some(e) = e {
                    out.push(i, e, Kind::Var);
                    i = e;
                    prev = Prev::Name;
                    continue;
                }
            }
            // -eq, -like, -and…
            if c == b'-' && t.get(i + 1).is_some_and(u8::is_ascii_alphabetic) {
                let e = super::word_end(t, i + 1);
                if has_word(super::words::PS_OPERATORS, &t[i + 1..e], true) {
                    out.push(i, e, Kind::Keyword);
                    i = e;
                    prev = Prev::Punct;
                    continue;
                }
            }
            // @"…"@ and @'…'@ here-strings.
            if c == b'@' && matches!(t.get(i + 1), Some(b'"' | b'\'')) {
                st.set_delim(&[t[i + 1], b'@']);
                st.mode = Mode::Delim;
                i = delimited(st, t, i, i + 2, out);
                prev = Prev::Value;
                continue;
            }
        }
        // Lua's long strings: [[…]], [==[…]==].
        if lang == Lang::Lua && c == b'[' {
            let n = t[i + 1..].iter().take_while(|&&b| b == b'=').count();
            if t.get(i + 1 + n) == Some(&b'[') && n + 2 <= DELIM {
                st.set_delim(&[&b"]"[..], &[b'='; DELIM][..n], &b"]"[..]].concat());
                st.mode = Mode::Delim;
                i = delimited(st, t, i, i + 2 + n, out);
                prev = Prev::Value;
                continue;
            }
        }
        if matches!(lang, Lang::Php | Lang::Shell | Lang::Ruby) {
            if let Some(e) = heredoc_open(lang, st, t, i) {
                out.push(i, e, Kind::Str);
                i = e;
                prev = Prev::Value;
                continue;
            }
        }
        if lang == Lang::Rust && c == b'\'' {
            i = rust_quote(t, i, out);
            prev = Prev::Value;
            continue;
        }
        if matches!(lang, Lang::Rust | Lang::Php) && c == b'#' && (t[i + 1..].starts_with(b"[") || t[i + 1..].starts_with(b"![")) {
            // #[derive(Debug)], #![allow(x)], #[Route('/')]: up to the matching bracket on this row.
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
            out.push(i, e, Kind::Decorator);
            i = e;
            prev = Prev::Punct;
            continue;
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
            let mut e = super::word_end(t, i);
            if lang == Lang::Shell {
                // Shell words go on over `-` and `.`: apt-get, my-script.sh.
                while e + 1 < t.len() && matches!(t[e], b'-' | b'.') && is_ident(t[e + 1]) {
                    e = super::word_end(t, e + 1);
                }
                // NAME=value, local name=value
                let declared = prev == Prev::Name && matches!(&t[last_name.0..last_name.1], b"local" | b"export" | b"readonly" | b"declare" | b"typeset");
                if t.get(e) == Some(&b'=') && (matches!(prev, Prev::Start | Prev::Punct) || declared) {
                    out.push(i, e, Kind::Var);
                    i = e;
                    prev = Prev::Name;
                    continue;
                }
            }
            // Ruby's predicate and bang methods: empty?, save!, defined?
            if lang == Lang::Ruby && matches!(t.get(e), Some(b'?' | b'!')) && t.get(e + 1) != Some(&b'=') {
                e += 1;
            }
            // PowerShell's Verb-Noun commands: Get-ChildItem.
            if lang == Lang::PowerShell && t.get(e) == Some(&b'-') && t.get(e + 1).is_some_and(u8::is_ascii_alphabetic) {
                while t.get(e) == Some(&b'-') && t.get(e + 1).is_some_and(u8::is_ascii_alphabetic) {
                    e = super::word_end(t, e + 1);
                }
                out.push(i, e, Kind::Function);
                i = e;
                prev = Prev::Name;
                continue;
            }
            // A string with a prefix: L"wide", u8"text", R"(raw)", f"{x}".
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
                let kind = if prev != Prev::Dot && has_word(spec.keywords, w, spec.ci) {
                    next = if spec.type_kw.iter().any(|k| k.as_bytes().eq_ignore_ascii_case(w)) {
                        Some(Kind::Type)
                    } else if spec.fn_kw.iter().any(|k| k.as_bytes().eq_ignore_ascii_case(w)) {
                        Some(Kind::Function)
                    } else {
                        None
                    };
                    Some(if has_word(super::words::LITERALS, w, spec.ci) {
                        Kind::Number
                    } else if matches!(w, b"self" | b"this") {
                        Kind::Var
                    } else {
                        Kind::Keyword
                    })
                } else if lang == Lang::Shell && has(super::words::SH_BUILTINS, w) {
                    Some(Kind::Function)
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
            out.push(i, e, Kind::Decorator);
            i = e;
            prev = Prev::Name;
            continue;
        }
        prev = match c {
            b'.' => Prev::Dot,
            // `->` reaches a member only in C++ and PHP (in Rust and TypeScript a type follows).
            b'>' if i > 0 && t[i - 1] == b'-' && matches!(lang, Lang::C | Lang::Php) => Prev::Dot,
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
    if prev != Prev::Dot && has_word(spec.types, w, spec.ci) {
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
        let next = t.get(j + 1);
        let more = b.is_ascii_alphanumeric()
            || b == b'_'
            || (b == b'.' && next.is_some_and(u8::is_ascii_digit))
            // 1e-5
            || ((b == b'+' || b == b'-') && !hex && matches!(t[j - 1], b'e' | b'E'))
            // 1'000'000
            || (b == b'\'' && lang == Lang::C && next.is_some_and(u8::is_ascii_alphanumeric));
        if !more {
            break;
        }
        j += 1;
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
        // Kotlin's """raw strings""" have no escapes; Swift's and Dart's multi-line ones do.
        // Ruby's strings may span lines; PowerShell's too, where a backtick escapes, not a
        // backslash, and '' is a quote inside '…'.
        Lang::Ruby if matches!(c, b'"' | b'\'' | b'`') => Some((Mode::Str(Str { q: c, n: 1, esc: true, ml: true, dbl: false }), i + 1)),
        Lang::PowerShell if c == b'"' || c == b'\'' => Some((Mode::Str(Str { q: c, n: 1, esc: false, ml: true, dbl: true }), i + 1)),
        Lang::Kotlin if three =>Some((Mode::Str(Str { q: b'"', n: 3, esc: false, ml: true, dbl: false }), i + 3)),
        Lang::Swift if three => Some((Mode::Str(Str { q: b'"', n: 3, esc: true, ml: true, dbl: false }), i + 3)),
        Lang::Dart if three || t[i..].starts_with(b"'''") => Some((Mode::Str(Str { q: c, n: 3, esc: true, ml: true, dbl: false }), i + 3)),
        // A template literal; what is inside `${}` is coloured as the string.
        Lang::JavaScript | Lang::TypeScript if c == b'`' => Some((Mode::Str(Str { q: c, n: 1, esc: true, ml: true, dbl: false }), i + 1)),
        // Go's raw strings; Rust's strings may span lines.
        Lang::Go if c == b'`' => Some((Mode::Str(Str { q: c, n: 1, esc: false, ml: true, dbl: false }), i + 1)),
        Lang::Rust if c == b'"' => Some((Mode::Str(Str { q: c, n: 1, esc: true, ml: true, dbl: false }), i + 1)),
        // PHP's strings (and `shell` commands) may span lines.
        Lang::Php if matches!(c, b'"' | b'\'' | b'`') => Some((Mode::Str(Str { q: c, n: 1, esc: true, ml: true, dbl: false }), i + 1)),
        Lang::Python if t[i..].starts_with(b"\"\"\"") || t[i..].starts_with(b"'''") => Some((Mode::Str(Str { q: c, n: 3, esc: true, ml: true, dbl: false }), i + 3)),
        // A shell's quotes may span lines; '…' has no escapes.
        Lang::Shell if c == b'"' || c == b'\'' => Some((Mode::Str(Str { q: c, n: 1, esc: c == b'"', ml: true, dbl: false }), i + 1)),
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
        // r"…", b'…', f"""…""", rb"…"
        Lang::Python if q != b'#' && w.len() <= 2 && w.iter().all(|b| b"rRbBuUfF".contains(b)) => {
            let n = if t[e..].starts_with(&[q; 3]) { 3 } else { 1 };
            Some((Mode::Str(Str { q, n, esc: true, ml: n == 3, dbl: false }), e + n as usize))
        }
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
        // Dart's raw strings: r'…', r"""…""".
        Lang::Dart if w == b"r" && q != b'#' => {
            let n = if t[e..].starts_with(&[q; 3]) { 3 } else { 1 };
            Some((Mode::Str(Str { q, n, esc: false, ml: n == 3, dbl: false }), e + n as usize))
        }
        _ => None,
    }
}

/// The length of a PHP opening tag at `t[p]`: `<?php`, `<?=` or a short `<? `. (`<?xml` is not
/// one.)
fn php_open(t: &[u8], p: usize) -> Option<usize> {
    let r = &t[p..];
    if !r.starts_with(b"<?") {
        return None;
    }
    if r.len() >= 5 && r[2..5].eq_ignore_ascii_case(b"php") && r.get(5).is_none_or(|&b| !is_ident(b)) {
        Some(5)
    } else if r.starts_with(b"<?=") {
        Some(3)
    } else if r.get(2).is_none_or(|&b| matches!(b, b' ' | b'\t' | b'\r')) {
        Some(2)
    } else {
        None
    }
}

/// HTML around PHP: highlighted as XML up to the next opening tag.
fn php_html(st: &mut State, t: &[u8], i: usize, out: &mut Sink) -> usize {
    let mut from = i;
    loop {
        let Some(p) = memchr::memmem::find(&t[from..], b"<?").map(|p| from + p) else {
            html(t, i, t.len(), out);
            return t.len();
        };
        if let Some(len) = php_open(t, p) {
            html(t, i, p, out);
            out.push(p, p + len, Kind::Tag);
            st.mode = Mode::Code;
            return p + len;
        }
        from = p + 2;
    }
}

/// `t[from..to]` as HTML (tags and comments on this row only).
fn html(t: &[u8], from: usize, to: usize, out: &mut Sink) {
    if let Some(v) = out.0.as_deref_mut() {
        let n = v.len();
        super::xml_spans(&mut State::default(), &t[from..to], &mut Sink(Some(&mut *v)));
        for s in &mut v[n..] {
            s.0 += from;
            s.1 += from;
        }
    }
}

/// A heredoc's opening at `t[i]` (PHP `<<<ID`, `<<<'ID'`; shell `<<EOF`, `<<-'EOF'`): its end.
/// The text starts on the next line and runs to a line that begins with `ID`.
fn heredoc_open(lang: Lang, st: &mut State, t: &[u8], i: usize) -> Option<usize> {
    let mut j = match lang {
        Lang::Php if t[i..].starts_with(b"<<<") => i + 3,
        // `<<<` is a shell's here-string, not a heredoc.
        Lang::Shell if t[i..].starts_with(b"<<") && !t[i..].starts_with(b"<<<") => i + 2 + t[i + 2..].starts_with(b"-") as usize,
        // <<~EOS, <<-EOS, <<EOS (a capital or a quote right after: `a << b` appends).
        Lang::Ruby if t[i..].starts_with(b"<<~") || t[i..].starts_with(b"<<-") => i + 3,
        Lang::Ruby if t[i..].starts_with(b"<<") && t.get(i + 2).is_some_and(|&b| b.is_ascii_uppercase() || b == b'"' || b == b'\'') => i + 2,
        _ => return None,
    };
    while j < t.len() && (t[j] == b' ' || t[j] == b'\t') {
        j += 1;
    }
    let quote = t.get(j).filter(|&&b| b == b'\'' || b == b'"').is_some();
    let w = j + quote as usize;
    if !t.get(w).is_some_and(|&b| is_ident(b) && !b.is_ascii_digit()) {
        return None;
    }
    let e = super::word_end(t, w);
    if e - w > DELIM {
        return None;
    }
    st.set_delim(&t[w..e]);
    st.pending = true;
    Some(if quote && e < t.len() { e + 1 } else { e })
}

/// A heredoc's text from `t[i]` (the start of a row): up to its closing word at the start of a
/// line, after which the code goes on.
fn heredoc(st: &mut State, t: &[u8], i: usize, out: &mut Sink) -> usize {
    if !st.inked {
        let at = i + t[i..].iter().take_while(|&&b| b == b' ' || b == b'\t').count();
        let d = st.delim().len();
        if t[at..].starts_with(st.delim()) && t.get(at + d).is_none_or(|&b| !is_ident(b)) {
            st.inked = true;
            st.mode = Mode::Code;
            out.push(at, at + d, Kind::Str);
            return at + d;
        }
    }
    if t[i..].iter().any(|&b| b != b' ' && b != b'\t') {
        st.inked = true;
    }
    out.push(i, t.len(), Kind::Str);
    t.len()
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
fn block_comment(st: &mut State, t: &[u8], start: usize, from: usize, spec: &Spec, out: &mut Sink) -> usize {
    let Mode::Comment(mut depth) = st.mode else { return from };
    let Some((open, close)) = spec.block else { return from };
    let (open, close) = (open.as_bytes(), close.as_bytes());
    // Ruby's `=end` counts only at the start of a line.
    let line_start = close.starts_with(b"=");
    let mut j = from;
    while j < t.len() {
        if t[j..].starts_with(close) && (!line_start || j == 0) {
            depth -= 1;
            j += close.len();
            if depth == 0 {
                st.mode = Mode::Code;
                out.push(start, j, Kind::Comment);
                return j;
            }
        } else if spec.nest && t[j..].starts_with(open) {
            depth = depth.saturating_add(1);
            j += open.len();
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
