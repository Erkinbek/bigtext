//! Simple syntax highlighting for common formats and programming languages ([`code`]).
//!
//! Highlighting runs only on the visible slice of each visible row, so it costs nothing for the
//! rest of the file. What a row starts inside of — a block comment, a string, an open tag — is a
//! [`State`], carried down from the row above (see [`super::hl_rows`]); nothing reads the file
//! from its start.

mod code;
mod words;

use std::path::Path;

use egui::Color32;

/// The longest delimiter a raw string can end with (`)delim"`).
const DELIM: usize = 20;

/// What the text at a point is inside of, for constructs that span lines.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct State {
    mode: Mode,
    /// Something other than blanks came since the line started (`#include` must come first).
    inked: bool,
    /// What closes a [`Mode::Delim`] string or a heredoc.
    delim: [u8; DELIM],
    delim_len: u8,
    /// A heredoc opened on this line: its text starts on the next one.
    pending: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum Mode {
    #[default]
    Code,
    /// A block comment (`/* */`, `<!-- -->`), nested this deep.
    Comment(u8),
    /// A `//` comment on a line that wraps onto the next row.
    LineComment,
    /// A string.
    Str(Str),
    /// A raw string closed by `State::delim`.
    Delim,
    /// A heredoc's lines, up to one that starts with `State::delim`.
    Heredoc,
    /// HTML around PHP's `<?php … ?>`.
    Html,
    /// An XML tag's attributes.
    Tag,
    /// An XML `<![CDATA[` section.
    CData,
}

/// How a string ends: `n` quote characters `q` (`"` or `"""`), skipping backslash escapes when
/// `esc` and doubled quotes when `dbl`; `ml`: it may go on over lines.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Str {
    q: u8,
    n: u8,
    esc: bool,
    ml: bool,
    dbl: bool,
}

impl State {
    /// The state at the start of the file (`file_start`), or at a point after a jump: PHP files
    /// start as HTML, though a point inside one is more likely code.
    pub fn start(lang: Lang, file_start: bool) -> State {
        let mode = if lang == Lang::Php && file_start { Mode::Html } else { Mode::Code };
        State { mode, ..State::default() }
    }

    /// Inside a block comment.
    pub fn in_comment(self) -> bool {
        matches!(self.mode, Mode::Comment(_))
    }

    /// Inside a string.
    pub fn in_string(self) -> bool {
        matches!(self.mode, Mode::Str(_) | Mode::Delim | Mode::Heredoc)
    }

    /// In the HTML of a PHP file.
    pub fn in_html(self) -> bool {
        self.mode == Mode::Html
    }

    fn set_delim(&mut self, d: &[u8]) {
        let n = d.len().min(DELIM);
        self.delim[..n].copy_from_slice(&d[..n]);
        self.delim_len = n as u8;
    }

    fn delim(&self) -> &[u8] {
        &self.delim[..self.delim_len as usize]
    }

    /// At a line break: line comments and single-line strings end (unless the line ends with a
    /// backslash, which continues a C string), and a heredoc opened on the line begins.
    fn end_line(&mut self, t: &[u8]) {
        self.inked = false;
        match self.mode {
            Mode::LineComment => self.mode = Mode::Code,
            Mode::Str(s) if !s.ml && !t.ends_with(b"\\") => self.mode = Mode::Code,
            _ => {}
        }
        if self.pending && self.mode == Mode::Code {
            self.mode = Mode::Heredoc;
        }
        self.pending = false;
    }
}

/// Where a tokenizer puts its spans: nowhere when only the state after the text is wanted.
struct Sink<'a>(Option<&'a mut Vec<(usize, usize, Kind)>>);

impl Sink<'_> {
    fn push(&mut self, a: usize, b: usize, k: Kind) {
        if let Some(o) = self.0.as_mut() {
            o.push((a, b, k));
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    Plain,
    Log,
    Json,
    Xml,
    Csv(u8),
    Sql,
    Ini,
    /// C and C++.
    C,
    CSharp,
    Java,
    JavaScript,
    TypeScript,
    Go,
    Rust,
    Php,
    Python,
    /// Bash and other POSIX shells.
    Shell,
    Kotlin,
    Swift,
    Dart,
    Ruby,
    Lua,
    PowerShell,
}

impl Lang {
    pub const ALL: [Lang; 24] = [
        Lang::Plain,
        Lang::Log,
        Lang::Json,
        Lang::Xml,
        Lang::Csv(b','),
        Lang::Csv(b'\t'),
        Lang::Sql,
        Lang::Ini,
        Lang::C,
        Lang::CSharp,
        Lang::Java,
        Lang::JavaScript,
        Lang::TypeScript,
        Lang::Go,
        Lang::Rust,
        Lang::Php,
        Lang::Python,
        Lang::Shell,
        Lang::Kotlin,
        Lang::Swift,
        Lang::Dart,
        Lang::Ruby,
        Lang::Lua,
        Lang::PowerShell,
    ];

    pub fn from_path(p: Option<&Path>) -> Lang {
        let name = p.and_then(|p| p.file_name()).map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        if matches!(name.as_str(), ".bashrc" | ".bash_profile" | ".bash_aliases" | ".bash_logout" | ".profile" | ".zshrc" | ".zprofile" | ".zshenv" | "pkgbuild") {
            return Lang::Shell;
        }
        if matches!(name.as_str(), "gemfile" | "rakefile" | "podfile" | "vagrantfile" | "guardfile" | "brewfile") {
            return Lang::Ruby;
        }
        let ext = p.and_then(|p| p.extension()).map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        match ext.as_str() {
            "log" | "out" | "err" => Lang::Log,
            "json" | "jsonl" | "ndjson" | "geojson" | "har" => Lang::Json,
            "xml" | "html" | "htm" | "xhtml" | "svg" | "xaml" | "csproj" | "vcxproj" | "props" | "config" | "plist" | "xsd" | "wsdl" => Lang::Xml,
            "csv" => Lang::Csv(b','),
            "tsv" | "tab" => Lang::Csv(b'\t'),
            "sql" | "ddl" | "dml" => Lang::Sql,
            "ini" | "toml" | "cfg" | "conf" | "yaml" | "yml" | "properties" | "env" | "inf" => Lang::Ini,
            "c" | "h" | "cpp" | "cc" | "cxx" | "c++" | "hpp" | "hh" | "hxx" | "h++" | "inl" | "ipp" | "ino" => Lang::C,
            "cs" | "csx" => Lang::CSharp,
            "java" => Lang::Java,
            "js" | "mjs" | "cjs" | "jsx" => Lang::JavaScript,
            "ts" | "tsx" | "mts" | "cts" => Lang::TypeScript,
            "go" => Lang::Go,
            "rs" => Lang::Rust,
            "php" | "phtml" | "php3" | "php4" | "php5" | "php7" | "php8" | "phps" => Lang::Php,
            "py" | "pyw" | "pyi" => Lang::Python,
            "sh" | "bash" | "zsh" | "ksh" => Lang::Shell,
            "kt" | "kts" => Lang::Kotlin,
            "swift" => Lang::Swift,
            "dart" => Lang::Dart,
            "rb" | "rake" | "gemspec" | "ru" | "erb" => Lang::Ruby,
            "lua" => Lang::Lua,
            "ps1" | "psm1" | "psd1" => Lang::PowerShell,
            _ => Lang::Plain,
        }
    }

    /// Guesses the format of a file whose name says nothing from its first lines: a `#!` line
    /// names a script's language, most lines starting with a time make a log, a `{` or `[` JSON,
    /// `<?xml` or a tag XML.
    pub fn sniff(text: &str) -> Lang {
        if let Some(bang) = text.trim_start_matches('\u{FEFF}').strip_prefix("#!").and_then(|t| t.lines().next()) {
            // #!/usr/bin/env python3, #!/bin/bash -e: the interpreter's name.
            let mut words = bang.split_whitespace();
            let first = words.next().unwrap_or_default().rsplit('/').next().unwrap_or_default();
            let prog = if first == "env" { words.find(|w| !w.starts_with('-')).unwrap_or_default() } else { first };
            let prog = prog.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
            match prog {
                "python" | "pypy" => return Lang::Python,
                "sh" | "bash" | "zsh" | "ksh" | "dash" | "ash" => return Lang::Shell,
                "node" | "deno" | "bun" => return Lang::JavaScript,
                "php" => return Lang::Php,
                "ruby" => return Lang::Ruby,
                "lua" | "luajit" => return Lang::Lua,
                "pwsh" | "powershell" => return Lang::PowerShell,
                _ => {}
            }
        }
        let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).take(8).collect();
        let Some(first) = lines.first().map(|l| l.trim_start()) else { return Lang::Plain };
        let stamped = lines.iter().filter(|l| bigtext::timestamp::parse(&l.chars().take(40).collect::<String>()).is_some()).count();
        if stamped > 0 && stamped * 2 >= lines.len() {
            Lang::Log
        } else if first.starts_with("<?xml") || first.starts_with("<!DOCTYPE") || first.starts_with("<html") {
            Lang::Xml
        } else if first.starts_with('{') || first.starts_with("[{") || first == "[" {
            Lang::Json
        } else {
            Lang::Plain
        }
    }

    /// True if a row's colours depend on the rows above it (block comments, tags over lines).
    pub fn stateful(self) -> bool {
        matches!(self, Lang::Xml | Lang::Sql) || self.is_code()
    }

    /// How a line is commented out (Edit → Toggle comment): a line comment, or a block comment
    /// around the lines when the language has none.
    pub fn comment(self) -> Option<Comment> {
        if let Some(s) = code::spec(self) {
            return match (s.line.first(), s.block) {
                (Some(t), _) => Some(Comment::Line(t)),
                (None, Some((open, close))) => Some(Comment::Block(open, close)),
                (None, None) => None,
            };
        }
        match self {
            Lang::Sql => Some(Comment::Line("--")),
            Lang::Ini => Some(Comment::Line("#")),
            Lang::Xml => Some(Comment::Block("<!--", "-->")),
            _ => None,
        }
    }

    /// A programming language (not a data or log format).
    pub fn is_code(self) -> bool {
        code::spec(self).is_some()
    }

    /// True if `line`, at the start of a line, almost surely begins outside any comment or string
    /// (`class Foo`, `#include`): highlighting after a jump starts there.
    pub fn sync_line(self, line: &str) -> bool {
        code::sync_line(self, line)
    }

    pub fn name(self) -> &'static str {
        match self {
            Lang::Plain => "Plain text",
            Lang::Log => "Log",
            Lang::Json => "JSON",
            Lang::Xml => "XML / HTML",
            Lang::Csv(b'\t') => "TSV",
            Lang::Csv(_) => "CSV",
            Lang::Sql => "SQL",
            Lang::Ini => "INI / TOML / YAML",
            Lang::C => "C / C++",
            Lang::CSharp => "C#",
            Lang::Java => "Java",
            Lang::JavaScript => "JavaScript",
            Lang::TypeScript => "TypeScript",
            Lang::Go => "Go",
            Lang::Rust => "Rust",
            Lang::Php => "PHP",
            Lang::Python => "Python",
            Lang::Shell => "Shell (Bash)",
            Lang::Kotlin => "Kotlin",
            Lang::Swift => "Swift",
            Lang::Dart => "Dart",
            Lang::Ruby => "Ruby",
            Lang::Lua => "Lua",
            Lang::PowerShell => "PowerShell",
        }
    }
}

/// How a language comments a line out.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Comment {
    /// `//`, `#`, `--`
    Line(&'static str),
    /// `/* … */`, `<!-- … -->`
    Block(&'static str, &'static str),
}

/// Lines with `c` toggled: commented out at their common indentation (blank lines left alone,
/// unless all are blank), or uncommented when every line with text already is. Returns the new
/// text and, per line, where the change is (byte in the old line, bytes removed, inserted).
pub fn toggle_comment(text: &str, c: Comment) -> (String, Vec<(usize, usize, usize)>) {
    let lines: Vec<&str> = text.split('\n').collect();
    // Each line without its CR (the CR goes back after it).
    let bodies: Vec<&str> = lines.iter().map(|l| l.strip_suffix('\r').unwrap_or(l)).collect();
    let all_blank = bodies.iter().all(|l| l.trim().is_empty());
    let has_text = |l: &str| all_blank || !l.trim().is_empty();
    let indent = |l: &str| l.len() - l.trim_start_matches([' ', '\t']).len();
    match c {
        Comment::Line(t) => {
            let commented = !all_blank && bodies.iter().filter(|l| has_text(l)).all(|l| l.trim_start().starts_with(t));
            let col = bodies.iter().filter(|l| has_text(l)).map(|l| indent(l)).min().unwrap_or(0);
            let mut out = Vec::with_capacity(lines.len());
            let mut changes = Vec::with_capacity(lines.len());
            for (l, b) in lines.iter().zip(&bodies) {
                let cr = &l[b.len()..];
                if !has_text(b) {
                    out.push(l.to_string());
                    changes.push((0, 0, 0));
                } else if commented {
                    let at = indent(b);
                    let mut n = t.len();
                    if b[at + n..].starts_with(' ') {
                        n += 1;
                    }
                    out.push(format!("{}{}{cr}", &b[..at], &b[at + n..]));
                    changes.push((at, n, 0));
                } else {
                    let at = col.min(b.len());
                    out.push(format!("{}{t} {}{cr}", &b[..at], &b[at..]));
                    changes.push((at, 0, t.len() + 1));
                }
            }
            (out.join("\n"), changes)
        }
        Comment::Block(open, close) => {
            let joined = bodies.join("\n");
            let trimmed = joined.trim();
            let new = if trimmed.len() >= open.len() + close.len() && trimmed.starts_with(open) && trimmed.ends_with(close) {
                let s = joined.find(open).unwrap_or(0);
                let e = joined.rfind(close).unwrap_or(joined.len());
                let inner = &joined[s + open.len()..e];
                let inner = inner.strip_prefix(' ').unwrap_or(inner);
                let inner = inner.strip_suffix(' ').unwrap_or(inner);
                format!("{}{inner}{}", &joined[..s], &joined[e + close.len()..])
            } else {
                let s = joined.len() - joined.trim_start().len();
                let e = joined.trim_end().len().max(s);
                format!("{}{open} {} {close}{}", &joined[..s], &joined[s..e], &joined[e..])
            };
            // CR LF line breaks stay CR LF.
            let crlf = lines.len() > 1 && lines[..lines.len() - 1].iter().all(|l| l.ends_with('\r'));
            (if crlf { new.replace('\n', "\r\n") } else { new }, Vec::new())
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Keyword,
    Str,
    Number,
    Comment,
    Tag,
    Attr,
    Key,
    Error,
    Warn,
    Info,
    Debug,
    Time,
    Punct,
    Col(u8),
    /// A type name (`int`, `String`, `class Foo`).
    Type,
    /// A function being called or defined.
    Function,
    /// A variable that says it is one: `$name` in PHP and shell scripts.
    Var,
    /// `@decorator`, `@Annotation`, `#[attribute]`.
    Decorator,
}

/// Colours on dark and light backgrounds. Code takes One Dark's (One Light's darkened to read well
/// on white): purple keywords, green strings, orange numbers and constants, yellow types, blue
/// functions, red variables, cyan decorators.
pub fn color(kind: Kind, dark: bool) -> Color32 {
    let (d, l) = match kind {
        Kind::Keyword => ((198, 120, 221), (152, 24, 160)),
        Kind::Str => ((152, 195, 121), (40, 120, 30)),
        Kind::Number => ((209, 154, 102), (170, 90, 0)),
        Kind::Comment => ((127, 132, 142), (120, 120, 120)),
        Kind::Tag => ((86, 156, 214), (20, 70, 180)),
        Kind::Attr => ((156, 220, 254), (150, 60, 30)),
        Kind::Key => ((97, 175, 239), (10, 90, 170)),
        Kind::Error => ((240, 90, 90), (200, 0, 0)),
        Kind::Warn => ((229, 192, 123), (170, 110, 0)),
        Kind::Info => ((120, 200, 120), (0, 120, 50)),
        Kind::Debug => ((128, 128, 128), (130, 130, 130)),
        Kind::Time => ((86, 182, 194), (0, 110, 130)),
        Kind::Punct => ((171, 178, 191), (90, 90, 90)),
        Kind::Type => ((229, 192, 123), (150, 100, 0)),
        Kind::Function => ((97, 175, 239), (40, 95, 210)),
        Kind::Var => ((224, 108, 117), (200, 50, 40)),
        Kind::Decorator => ((86, 182, 194), (0, 120, 160)),
        Kind::Col(i) => {
            const DARK: [(u8, u8, u8); 6] = [(229, 192, 123), (97, 175, 239), (152, 195, 121), (198, 120, 221), (86, 182, 194), (224, 108, 117)];
            const LIGHT: [(u8, u8, u8); 6] = [(150, 100, 0), (10, 90, 170), (40, 120, 30), (152, 24, 160), (0, 110, 130), (180, 30, 50)];
            (DARK[i as usize % 6], LIGHT[i as usize % 6])
        }
    };
    let theme = crate::app::theme::cvd().then(|| crate::app::theme::cvd_syntax(kind, dark));
    let (r, g, b) = theme.unwrap_or(if dark { d } else { l });
    let c = Color32::from_rgb(r, g, b);
    if crate::app::theme::high_contrast_on() {
        crate::app::theme::contrast(c, dark)
    } else {
        c
    }
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// End of a quoted string starting at `i` (which holds the quote), honouring backslash escapes.
fn string_end(t: &[u8], i: usize, q: u8, backslash: bool) -> usize {
    let mut j = i + 1;
    while j < t.len() {
        if backslash && t[j] == b'\\' {
            j += 2;
            continue;
        }
        if t[j] == q {
            return j + 1;
        }
        j += 1;
    }
    t.len()
}

fn number_end(t: &[u8], i: usize) -> usize {
    let mut j = i;
    while j < t.len() && (t[j].is_ascii_alphanumeric() || t[j] == b'.' || t[j] == b'_') {
        j += 1;
    }
    j
}

fn word_end(t: &[u8], i: usize) -> usize {
    let mut j = i;
    while j < t.len() && is_ident(t[j]) {
        j += 1;
    }
    j
}

const SQL_KEYWORDS: &[&str] = &[
    "select", "from", "where", "insert", "into", "values", "update", "set", "delete", "create", "table", "drop", "alter", "add", "index", "primary",
    "key", "foreign", "references", "not", "null", "and", "or", "in", "is", "like", "between", "join", "left", "right", "inner", "outer", "on",
    "as", "group", "by", "order", "having", "limit", "offset", "union", "all", "distinct", "case", "when", "then", "else", "end", "begin",
    "commit", "rollback", "transaction", "default", "unique", "constraint", "exists", "if", "view", "database", "use", "lock", "tables",
    "unlock", "engine", "charset", "int", "integer", "bigint", "smallint", "varchar", "char", "text", "date", "datetime", "timestamp",
    "decimal", "float", "double", "boolean", "true", "false", "auto_increment", "with", "returning", "grant", "revoke", "desc", "asc",
];

/// Byte spans (into `text`, one line or the visible slice of one row) with their kinds, for text
/// that starts outside any comment or tag.
#[cfg(test)]
pub fn spans(lang: Lang, text: &str, out: &mut Vec<(usize, usize, Kind)>) {
    spans_from(lang, State::default(), text, true, out);
}

/// Spans of `text` starting in state `st`; returns the state after it. `ends_line`: the text
/// ends at a line break (not at a wrap or the edge of the screen).
pub fn spans_from(lang: Lang, st: State, text: &str, ends_line: bool, out: &mut Vec<(usize, usize, Kind)>) -> State {
    run(lang, st, text.as_bytes(), ends_line, Sink(Some(out)))
}

/// The state after `text` starting in state `st`, without the spans.
pub fn scan(lang: Lang, st: State, text: &str, ends_line: bool) -> State {
    if !lang.stateful() {
        return st;
    }
    run(lang, st, text.as_bytes(), ends_line, Sink(None))
}

fn run(lang: Lang, mut st: State, t: &[u8], ends_line: bool, mut sink: Sink) -> State {
    match (lang, sink.0.as_deref_mut()) {
        (Lang::Xml, _) => xml_spans(&mut st, t, &mut sink),
        (Lang::Sql, _) => sql_spans(&mut st, t, &mut sink),
        (l, _) if l.is_code() => code::spans(lang, &mut st, t, &mut sink),
        (Lang::Log, Some(out)) => log_spans(t, out),
        (Lang::Json, Some(out)) => json_spans(t, out),
        (Lang::Csv(sep), Some(out)) => csv_spans(t, sep, out),
        (Lang::Ini, Some(out)) => ini_spans(t, out),
        // Plain text, or only the state wanted of a format without one.
        _ => {}
    }
    if ends_line {
        st.end_line(t);
    }
    st
}

/// End of a construct closed by `pat`, searched from `from`: past `pat`, or the end of the text
/// (false) when it goes on.
fn close(t: &[u8], from: usize, pat: &[u8]) -> (usize, bool) {
    match memchr::memmem::find(&t[from.min(t.len())..], pat) {
        Some(p) => (from + p + pat.len(), true),
        None => (t.len(), false),
    }
}

fn log_spans(t: &[u8], out: &mut Vec<(usize, usize, Kind)>) {
    // Leading timestamp: digits and date/time punctuation.
    let mut i = 0;
    if t.len() >= 8 && t[..4].iter().all(u8::is_ascii_digit) {
        while i < t.len() && i < 40 && (t[i].is_ascii_digit() || b"-:./T ,+Z".contains(&t[i])) {
            i += 1;
        }
        while i > 0 && t[i - 1] == b' ' {
            i -= 1;
        }
        out.push((0, i, Kind::Time));
    }
    while i < t.len() {
        let c = t[i];
        if c == b'"' {
            let e = string_end(t, i, b'"', true);
            out.push((i, e, Kind::Str));
            i = e;
        } else if is_ident(c) {
            let e = word_end(t, i);
            let w = &t[i..e];
            let kind = if w.eq_ignore_ascii_case(b"ERROR") || w.eq_ignore_ascii_case(b"FATAL") || w.eq_ignore_ascii_case(b"CRITICAL") || w.eq_ignore_ascii_case(b"SEVERE") || w.eq_ignore_ascii_case(b"EXCEPTION") {
                Some(Kind::Error)
            } else if w.eq_ignore_ascii_case(b"WARN") || w.eq_ignore_ascii_case(b"WARNING") {
                Some(Kind::Warn)
            } else if w.eq_ignore_ascii_case(b"INFO") || w.eq_ignore_ascii_case(b"NOTICE") {
                Some(Kind::Info)
            } else if w.eq_ignore_ascii_case(b"DEBUG") || w.eq_ignore_ascii_case(b"TRACE") || w.eq_ignore_ascii_case(b"VERBOSE") {
                Some(Kind::Debug)
            } else if c.is_ascii_digit() {
                Some(Kind::Number)
            } else {
                None
            };
            if let Some(k) = kind {
                out.push((i, e, k));
            }
            i = e;
        } else {
            i += 1;
        }
    }
}

fn json_spans(t: &[u8], out: &mut Vec<(usize, usize, Kind)>) {
    let mut i = 0;
    while i < t.len() {
        let c = t[i];
        if c == b'"' {
            let e = string_end(t, i, b'"', true);
            let mut k = e;
            while k < t.len() && t[k] == b' ' {
                k += 1;
            }
            out.push((i, e, if k < t.len() && t[k] == b':' { Kind::Key } else { Kind::Str }));
            i = e;
        } else if c.is_ascii_digit() || (c == b'-' && t.get(i + 1).is_some_and(u8::is_ascii_digit)) {
            let e = number_end(t, i + 1);
            out.push((i, e, Kind::Number));
            i = e;
        } else if is_ident(c) {
            let e = word_end(t, i);
            if matches!(&t[i..e], b"true" | b"false" | b"null") {
                out.push((i, e, Kind::Keyword));
            }
            i = e;
        } else {
            if b"{}[]:,".contains(&c) {
                out.push((i, i + 1, Kind::Punct));
            }
            i += 1;
        }
    }
}

fn xml_spans(st: &mut State, t: &[u8], out: &mut Sink) {
    let mut i = 0;
    while i < t.len() {
        match st.mode {
            Mode::Comment(_) | Mode::CData => {
                let (kind, pat) = if st.mode == Mode::CData { (Kind::Str, &b"]]>"[..]) } else { (Kind::Comment, &b"-->"[..]) };
                let (e, closed) = close(t, i, pat);
                out.push(i, e, kind);
                if closed {
                    st.mode = Mode::Code;
                }
                i = e;
            }
            Mode::Code | Mode::LineComment | Mode::Str(_) | Mode::Delim | Mode::Heredoc | Mode::Html => {
                if t[i..].starts_with(b"<!--") {
                    let (e, closed) = close(t, i + 4, b"-->");
                    out.push(i, e, Kind::Comment);
                    if !closed {
                        st.mode = Mode::Comment(1);
                    }
                    i = e;
                } else if t[i..].starts_with(b"<![CDATA[") {
                    out.push(i, i + 9, Kind::Tag);
                    st.mode = Mode::CData;
                    i += 9;
                } else if t[i] == b'<' {
                    let mut e = i + 1;
                    while e < t.len() && (t[e] == b'/' || t[e] == b'?' || t[e] == b'!') {
                        e += 1;
                    }
                    while e < t.len() && (is_ident(t[e]) || t[e] == b':' || t[e] == b'-' || t[e] == b'.') {
                        e += 1;
                    }
                    out.push(i, e, Kind::Tag);
                    st.mode = Mode::Tag;
                    i = e;
                } else if t[i] == b'&' {
                    let e = t[i..].iter().position(|&b| b == b';' || b == b' ').map(|p| i + p + 1).unwrap_or(t.len()).min(i + 12);
                    out.push(i, e, Kind::Keyword);
                    i = e;
                } else {
                    i += 1;
                }
            }
            Mode::Tag => {
                let c = t[i];
                if c == b'>' || (c == b'/' && t.get(i + 1) == Some(&b'>')) || (c == b'?' && t.get(i + 1) == Some(&b'>')) {
                    let e = if c == b'>' { i + 1 } else { i + 2 };
                    out.push(i, e, Kind::Tag);
                    st.mode = Mode::Code;
                    i = e;
                } else if c == b'"' || c == b'\'' {
                    let e = string_end(t, i, c, false);
                    out.push(i, e, Kind::Str);
                    i = e;
                } else if is_ident(c) {
                    let mut e = i;
                    while e < t.len() && (is_ident(t[e]) || t[e] == b':' || t[e] == b'-' || t[e] == b'.') {
                        e += 1;
                    }
                    out.push(i, e, Kind::Attr);
                    i = e;
                } else {
                    i += 1;
                }
            }
        }
    }
}

fn csv_spans(t: &[u8], sep: u8, out: &mut Vec<(usize, usize, Kind)>) {
    let mut col = 0u8;
    let mut start = 0;
    let mut i = 0;
    while i < t.len() {
        if t[i] == b'"' {
            // Quoted field with "" escapes.
            let mut j = i + 1;
            while j < t.len() {
                if t[j] == b'"' {
                    if t.get(j + 1) == Some(&b'"') {
                        j += 2;
                        continue;
                    }
                    break;
                }
                j += 1;
            }
            i = j + 1;
            continue;
        }
        if t[i] == sep {
            out.push((start, i, Kind::Col(col)));
            out.push((i, i + 1, Kind::Punct));
            col = col.wrapping_add(1);
            start = i + 1;
        }
        i += 1;
    }
    out.push((start, t.len(), Kind::Col(col)));
}

fn sql_spans(st: &mut State, t: &[u8], out: &mut Sink) {
    let mut i = 0;
    if st.in_comment() {
        let (e, closed) = close(t, 0, b"*/");
        out.push(0, e, Kind::Comment);
        if !closed {
            return;
        }
        st.mode = Mode::Code;
        i = e;
    }
    let colour = out.0.is_some();
    while i < t.len() {
        let c = t[i];
        if t[i..].starts_with(b"--") || c == b'#' {
            out.push(i, t.len(), Kind::Comment);
            return;
        }
        if t[i..].starts_with(b"/*") {
            let (e, closed) = close(t, i + 2, b"*/");
            out.push(i, e, Kind::Comment);
            if !closed {
                st.mode = Mode::Comment(1);
            }
            i = e;
        } else if c == b'\'' || c == b'"' || c == b'`' {
            let e = string_end(t, i, c, true);
            out.push(i, e, if c == b'`' { Kind::Key } else { Kind::Str });
            i = e;
        } else if c.is_ascii_digit() {
            let e = number_end(t, i);
            out.push(i, e, Kind::Number);
            i = e;
        } else if is_ident(c) {
            let e = word_end(t, i);
            if colour {
                let w = String::from_utf8_lossy(&t[i..e]).to_ascii_lowercase();
                if SQL_KEYWORDS.contains(&w.as_str()) {
                    out.push(i, e, Kind::Keyword);
                }
            }
            i = e;
        } else {
            i += 1;
        }
    }
}

fn ini_spans(t: &[u8], out: &mut Vec<(usize, usize, Kind)>) {
    let lead = t.iter().take_while(|&&b| b == b' ' || b == b'\t').count();
    let rest = &t[lead..];
    if rest.starts_with(b"#") || rest.starts_with(b";") || rest.starts_with(b"//") {
        out.push((lead, t.len(), Kind::Comment));
        return;
    }
    if rest.starts_with(b"[") {
        out.push((lead, t.len(), Kind::Tag));
        return;
    }
    let sep = rest.iter().position(|&b| b == b'=' || b == b':');
    let mut i = lead;
    if let Some(p) = sep {
        let key_end = lead + p;
        out.push((lead, key_end, Kind::Key));
        out.push((key_end, key_end + 1, Kind::Punct));
        i = key_end + 1;
    }
    while i < t.len() {
        let c = t[i];
        if c == b'"' || c == b'\'' {
            let e = string_end(t, i, c, c == b'"');
            out.push((i, e, Kind::Str));
            i = e;
        } else if c == b'#' && i > 0 && t[i - 1] == b' ' {
            out.push((i, t.len(), Kind::Comment));
            return;
        } else if c.is_ascii_digit() || (c == b'-' && t.get(i + 1).is_some_and(u8::is_ascii_digit)) {
            let e = number_end(t, i + 1);
            out.push((i, e, Kind::Number));
            i = e;
        } else if is_ident(c) {
            let e = word_end(t, i);
            if matches!(&t[i..e], b"true" | b"false" | b"yes" | b"no" | b"on" | b"off" | b"null") {
                out.push((i, e, Kind::Keyword));
            }
            i = e;
        } else {
            i += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(lang: Lang, s: &str) -> Vec<(String, Kind)> {
        let mut v = Vec::new();
        spans(lang, s, &mut v);
        v.into_iter().map(|(a, b, k)| (s[a..b].to_string(), k)).collect()
    }

    #[test]
    fn json() {
        let k = kinds(Lang::Json, r#"{"key": "value", "n": -1.5e3, "ok": true}"#);
        assert!(k.contains(&("\"key\"".into(), Kind::Key)));
        assert!(k.contains(&("\"value\"".into(), Kind::Str)));
        assert!(k.contains(&("-1.5e3".into(), Kind::Number)));
        assert!(k.contains(&("true".into(), Kind::Keyword)));
    }

    #[test]
    fn log_levels_and_time() {
        let k = kinds(Lang::Log, "2026-10-24 12:00:01.123 [ERROR] id=5 failed");
        assert_eq!(k[0], ("2026-10-24 12:00:01.123".into(), Kind::Time));
        assert!(k.contains(&("ERROR".into(), Kind::Error)));
    }

    #[test]
    fn xml_sql_csv_ini() {
        let k = kinds(Lang::Xml, r#"<a href="x">text</a><!-- c -->"#);
        assert!(k.contains(&("<a".into(), Kind::Tag)));
        assert!(k.contains(&("href".into(), Kind::Attr)));
        assert!(k.contains(&("\"x\"".into(), Kind::Str)));
        assert!(k.contains(&("<!-- c -->".into(), Kind::Comment)));
        let k = kinds(Lang::Sql, "SELECT name FROM t WHERE id = 'x' -- note");
        assert!(k.contains(&("SELECT".into(), Kind::Keyword)));
        assert!(k.contains(&("'x'".into(), Kind::Str)));
        assert!(k.contains(&("-- note".into(), Kind::Comment)));
        let k = kinds(Lang::Csv(b','), r#"a,"b,c",d"#);
        assert_eq!(k.iter().filter(|x| matches!(x.1, Kind::Col(_))).count(), 3);
        let k = kinds(Lang::Ini, "port = 8080 # comment");
        assert!(k.contains(&("port ".into(), Kind::Key)));
        assert!(k.contains(&("8080".into(), Kind::Number)));
    }

    /// Each line's spans, the state carried from line to line.
    fn lines(lang: Lang, text: &str) -> Vec<Vec<(String, Kind)>> {
        lines_from(lang, State::default(), text)
    }

    fn lines_from(lang: Lang, mut st: State, text: &str) -> Vec<Vec<(String, Kind)>> {
        text.lines()
            .map(|l| {
                let mut v = Vec::new();
                st = spans_from(lang, st, l, true, &mut v);
                v.into_iter().map(|(a, b, k)| (l[a..b].to_string(), k)).collect()
            })
            .collect()
    }

    #[test]
    fn comments_and_tags_over_lines() {
        let x = lines(Lang::Xml, "<a href=\"x\"\n   id=\"y\">t</a>\n<!-- one\ntwo -->\n<b/>\n<![CDATA[\n<raw>\n]]>");
        assert!(x[1].contains(&("id".into(), Kind::Attr)) && x[1].contains(&("\"y\"".into(), Kind::Str)), "{:?}", x[1]);
        assert_eq!(x[3], vec![("two -->".into(), Kind::Comment)]);
        assert!(x[4].contains(&("<b".into(), Kind::Tag)));
        assert_eq!(x[6], vec![("<raw>".into(), Kind::Str)]);
        assert!(x[7].iter().all(|s| s.1 == Kind::Str));
        let s = lines(Lang::Sql, "SELECT 1; /* a\nselect\n*/ SELECT 2");
        assert_eq!(s[1], vec![("select".into(), Kind::Comment)]);
        assert!(s[2].contains(&("SELECT".into(), Kind::Keyword)));
        // The state alone, without spans.
        let st = scan(Lang::Sql, State::default(), "x /* open", true);
        assert!(st.in_comment() && !scan(Lang::Sql, st, "*/", true).in_comment());
        assert_eq!(scan(Lang::Json, st, "{", true), st);
    }

    #[test]
    fn c_cpp_csharp_java() {
        let k = kinds(Lang::C, "#include <stdio.h>");
        assert_eq!(k, vec![("#include".into(), Kind::Keyword), ("<stdio.h>".into(), Kind::Str)]);
        let k = kinds(Lang::C, "static int count(const char *s) { return MAX_LEN + 1'000; } // done");
        for want in [("static", Kind::Keyword), ("int", Kind::Type), ("count", Kind::Function), ("char", Kind::Type), ("MAX_LEN", Kind::Number), ("1'000", Kind::Number), ("// done", Kind::Comment)] {
            assert!(k.contains(&(want.0.into(), want.1)), "{want:?} in {k:?}");
        }
        assert!(!k.iter().any(|(s, _)| s == "s"));
        let k = kinds(Lang::C, r#"auto s = R"x(a "quoted" )x" + L"wide";"#);
        assert!(k.contains(&(r#"R"x(a "quoted" )x""#.into(), Kind::Str)) && k.contains(&("L\"wide\"".into(), Kind::Str)), "{k:?}");
        let l = lines(Lang::C, "a /* one\ntwo */ b\nstd::string s = R\"(raw\nstill)\";");
        assert_eq!(l[1][0], ("two */".into(), Kind::Comment));
        assert!(l[2].contains(&("string".into(), Kind::Type)));
        assert_eq!(l[3], vec![("still)\"".into(), Kind::Str)]);
        // A line comment goes on over a wrapped row, and ends with the line.
        let st = scan(Lang::C, State::default(), "x // comment", false);
        let mut v = Vec::new();
        let after = spans_from(Lang::C, st, "goes on", true, &mut v);
        assert_eq!((v, after), (vec![(0, 7, Kind::Comment)], State::default()));

        let k = kinds(Lang::CSharp, r#"var p = @"C:\dir\"; var s = $"{x}\n"; public class Foo : IBar { }"#);
        for want in [("var", Kind::Keyword), (r#"@"C:\dir\""#, Kind::Str), (r#"$"{x}\n""#, Kind::Str), ("Foo", Kind::Type), ("IBar", Kind::Type)] {
            assert!(k.contains(&(want.0.into(), want.1)), "{want:?} in {k:?}");
        }
        let l = lines(Lang::CSharp, "var q = \"\"\"\nraw \" text\n\"\"\";");
        assert_eq!(l[1], vec![("raw \" text".into(), Kind::Str)]);
        assert_eq!(l[2][0], ("\"\"\"".into(), Kind::Str));

        let k = kinds(Lang::Java, "@Override public String toString() { return new StringBuilder().toString(); }");
        assert_eq!(k[..4], [("@Override".into(), Kind::Decorator), ("public".into(), Kind::Keyword), ("String".into(), Kind::Type), ("toString".into(), Kind::Function)]);
        assert!(k.contains(&("StringBuilder".into(), Kind::Type)) && k.last() == Some(&("toString".into(), Kind::Function)), "{k:?}");
        let l = lines(Lang::Java, "String s = \"\"\"\n  text \"block\"\n  \"\"\";\n/**\n * Docs.\n */\nclass A {}");
        assert_eq!(l[1], vec![("  text \"block\"".into(), Kind::Str)]);
        assert_eq!(l[4], vec![(" * Docs.".into(), Kind::Comment)]);
        assert_eq!(l[6][..2], [("class".into(), Kind::Keyword), ("A".into(), Kind::Type)]);
    }

    #[test]
    fn javascript_typescript() {
        let k = kinds(Lang::JavaScript, r#"export async function load(url) { const re = /"[a-z/]+"/gi; return x / 2 / y; }"#);
        for want in [("export", Kind::Keyword), ("async", Kind::Keyword), ("load", Kind::Function), (r#"/"[a-z/]+"/gi"#, Kind::Str), ("2", Kind::Number)] {
            assert!(k.contains(&(want.0.into(), want.1)), "{want:?} in {k:?}");
        }
        assert_eq!(k.iter().filter(|s| s.1 == Kind::Str).count(), 1, "{k:?}");
        // Values among the keywords are constants, `this` a variable.
        let k = kinds(Lang::JavaScript, "return this.ok ?? null;");
        assert_eq!(k, vec![("return".into(), Kind::Keyword), ("this".into(), Kind::Var), ("null".into(), Kind::Number)]);
        // A regular expression with a backtick does not open a template literal.
        let k = kinds(Lang::JavaScript, "if (ok) return /`/.test(s);");
        assert!(k.contains(&("/`/".into(), Kind::Str)) && k.contains(&("test".into(), Kind::Function)), "{k:?}");
        assert_eq!(scan(Lang::JavaScript, State::default(), "if (ok) return /`/.test(s);", true), State::default());
        let l = lines(Lang::TypeScript, "const html = `<p>\n  ${name}\n</p>`;\ninterface User { id: number }");
        assert_eq!(l[1], vec![("  ${name}".into(), Kind::Str)]);
        assert_eq!(l[2][0], ("</p>`".into(), Kind::Str));
        assert_eq!(l[3], vec![("interface".into(), Kind::Keyword), ("User".into(), Kind::Type), ("number".into(), Kind::Type)]);
        let k = kinds(Lang::TypeScript, "@Component({}) class A { private x: string = 'a'; }");
        for want in [("@Component", Kind::Decorator), ("private", Kind::Keyword), ("string", Kind::Type), ("'a'", Kind::Str)] {
            assert!(k.contains(&(want.0.into(), want.1)), "{want:?} in {k:?}");
        }
        assert_eq!((Lang::from_path(Some(Path::new("a.tsx"))), Lang::from_path(Some(Path::new("b.mjs")))), (Lang::TypeScript, Lang::JavaScript));
    }

    #[test]
    fn go_rust() {
        let k = kinds(Lang::Go, r#"func (s *Server) Handle(w http.ResponseWriter) error { return fmt.Errorf(`bad "%s"`, nil) }"#);
        for want in [("func", Kind::Keyword), ("Handle", Kind::Function), ("error", Kind::Type), ("Errorf", Kind::Function), ("`bad \"%s\"`", Kind::Str), ("nil", Kind::Number)] {
            assert!(k.contains(&(want.0.into(), want.1)), "{want:?} in {k:?}");
        }
        // Capitalised names are exported, not types, in Go.
        assert!(!k.iter().any(|(s, _)| s == "Server" || s == "ResponseWriter"), "{k:?}");
        let l = lines(Lang::Go, "q := `SELECT\n  *\nFROM t`\nvar n int");
        assert_eq!(l[1], vec![("  *".into(), Kind::Str)]);
        assert_eq!(l[3], vec![("var".into(), Kind::Keyword), ("int".into(), Kind::Type)]);

        let k = kinds(Lang::Rust, r#"#[derive(Debug)] pub fn get<'a>(s: &'a str) -> Option<i32> { println!("{}", 'x'); Some('\n') }"#);
        for want in [
            ("#[derive(Debug)]", Kind::Decorator),
            ("fn", Kind::Keyword),
            ("get", Kind::Function),
            ("'a", Kind::Tag),
            ("str", Kind::Type),
            ("Option", Kind::Type),
            ("i32", Kind::Type),
            ("println!", Kind::Function),
            ("'x'", Kind::Str),
            ("'\\n'", Kind::Str),
        ] {
            assert!(k.contains(&(want.0.into(), want.1)), "{want:?} in {k:?}");
        }
        let l = lines(Lang::Rust, "let s = r#\"raw \"quoted\"\nstill\"#;\n/* outer /* inner */ still\n*/ let x = 1;\nlet t = \"a\nb\";");
        assert_eq!(l[1], vec![("still\"#".into(), Kind::Str)]);
        assert_eq!(l[2], vec![("/* outer /* inner */ still".into(), Kind::Comment)]);
        assert_eq!(l[3][..2], [("*/".into(), Kind::Comment), ("let".into(), Kind::Keyword)]);
        assert_eq!(l[5][0], ("b\"".into(), Kind::Str));
    }

    #[test]
    fn php() {
        let text = "<h1 class=\"t\">Hi</h1>\n<?php\n$name = 'Ann'; // greet ?> <b>x</b>\n<?= strtoupper($name) ?>\n<?php\n$t = <<<EOT\n  Hello $name\n  EOT;\necho TRUE; #[Route('/')]\nfunction show(int $id): string {}";
        let l = lines_from(Lang::Php, State::start(Lang::Php, true), text);
        assert_eq!(l[0][..3], [("<h1".into(), Kind::Tag), ("class".into(), Kind::Attr), ("\"t\"".into(), Kind::Str)]);
        assert_eq!(l[1], vec![("<?php".into(), Kind::Tag)]);
        assert_eq!(l[2][..4], [("$name".into(), Kind::Var), ("'Ann'".into(), Kind::Str), ("// greet ".into(), Kind::Comment), ("?>".into(), Kind::Tag)]);
        assert!(l[2].contains(&("<b".into(), Kind::Tag)), "{:?}", l[2]);
        assert_eq!(l[3], vec![("<?=".into(), Kind::Tag), ("strtoupper".into(), Kind::Function), ("$name".into(), Kind::Var), ("?>".into(), Kind::Tag)]);
        assert_eq!(l[5], vec![("$t".into(), Kind::Var), ("<<<EOT".into(), Kind::Str)]);
        assert_eq!(l[6], vec![("  Hello $name".into(), Kind::Str)]);
        assert_eq!(l[7], vec![("EOT".into(), Kind::Str)]);
        assert_eq!(l[8], vec![("echo".into(), Kind::Keyword), ("TRUE".into(), Kind::Number), ("#[Route('/')]".into(), Kind::Decorator)]);
        for want in [("function", Kind::Keyword), ("show", Kind::Function), ("int", Kind::Type), ("$id", Kind::Var), ("string", Kind::Type)] {
            assert!(l[9].contains(&(want.0.into(), want.1)), "{want:?} in {:?}", l[9]);
        }
        // <?xml is not PHP; after a jump a PHP file is assumed to be in code.
        let l = lines_from(Lang::Php, State::start(Lang::Php, true), "<?xml version=\"1.0\"?>\n$x");
        assert!(l[1].is_empty(), "{:?}", l[1]);
        assert_eq!(kinds(Lang::Php, "$x"), vec![("$x".into(), Kind::Var)]);
    }

    #[test]
    fn python_shell() {
        let l = lines(
            Lang::Python,
            "@dataclass\nclass User(Base):\n    \"\"\"A user.\n\n    More text.\n    \"\"\"\n    def greet(self, name: str) -> str:\n        return f\"hi {name}\"  # say hi\nMAX = 3\nx = 'open\ny = 1",
        );
        assert_eq!(l[0], vec![("@dataclass".into(), Kind::Decorator)]);
        assert_eq!(l[1], vec![("class".into(), Kind::Keyword), ("User".into(), Kind::Type), ("Base".into(), Kind::Type)]);
        assert_eq!(l[2], vec![("\"\"\"A user.".into(), Kind::Str)]);
        assert_eq!(l[4], vec![("    More text.".into(), Kind::Str)]);
        assert_eq!(l[6], vec![("def".into(), Kind::Keyword), ("greet".into(), Kind::Function), ("self".into(), Kind::Var), ("str".into(), Kind::Type), ("str".into(), Kind::Type)]);
        assert_eq!(l[7], vec![("return".into(), Kind::Keyword), ("f\"hi {name}\"".into(), Kind::Str), ("# say hi".into(), Kind::Comment)]);
        assert_eq!(l[8], vec![("MAX".into(), Kind::Number), ("3".into(), Kind::Number)]);
        // A '…' string ends with its line.
        assert_eq!(l[10], vec![("1".into(), Kind::Number)]);

        let l = lines(
            Lang::Shell,
            "#!/bin/bash\nNAME=\"world\"  # greet\nif [ -n \"$NAME\" ]; then echo \"hi ${NAME}\"; fi\ncat <<-'EOF' | grep x\n  body $NAME\n\tEOF\necho done a#b $# my-done.sh",
        );
        assert_eq!(l[0], vec![("#!/bin/bash".into(), Kind::Comment)]);
        assert_eq!(l[1], vec![("NAME".into(), Kind::Var), ("\"world\"".into(), Kind::Str), ("# greet".into(), Kind::Comment)]);
        assert_eq!(
            l[2],
            vec![("if".into(), Kind::Keyword), ("\"$NAME\"".into(), Kind::Str), ("then".into(), Kind::Keyword), ("echo".into(), Kind::Function), ("\"hi ${NAME}\"".into(), Kind::Str), ("fi".into(), Kind::Keyword)]
        );
        assert_eq!(l[3], vec![("<<-'EOF'".into(), Kind::Str)]);
        assert_eq!(l[4], vec![("  body $NAME".into(), Kind::Str)]);
        assert_eq!(l[5], vec![("EOF".into(), Kind::Str)]);
        assert_eq!(l[6], vec![("echo".into(), Kind::Function), ("done".into(), Kind::Keyword), ("$#".into(), Kind::Var)]);
        assert_eq!(kinds(Lang::Shell, "local dir=/tmp"), vec![("local".into(), Kind::Keyword), ("dir".into(), Kind::Var)]);

        assert_eq!(Lang::sniff("#!/usr/bin/env python3\nprint(1)\n"), Lang::Python);
        assert_eq!(Lang::sniff("\u{FEFF}#!/bin/bash -e\n"), Lang::Shell);
        assert_eq!(Lang::sniff("#!/usr/bin/env -S node --x\n"), Lang::JavaScript);
        assert_eq!(Lang::sniff("#!/usr/bin/perl\n"), Lang::Plain);
        assert_eq!((Lang::from_path(Some(Path::new("/home/a/.bashrc"))), Lang::from_path(Some(Path::new("t.pyi")))), (Lang::Shell, Lang::Python));
    }

    #[test]
    fn kotlin_swift_dart() {
        let k = kinds(Lang::Kotlin, "@JvmStatic suspend fun load(id: Int): String? = null");
        for want in [("@JvmStatic", Kind::Decorator), ("suspend", Kind::Keyword), ("fun", Kind::Keyword), ("load", Kind::Function), ("Int", Kind::Type), ("String", Kind::Type), ("null", Kind::Number)] {
            assert!(k.contains(&(want.0.into(), want.1)), "{want:?} in {k:?}");
        }
        let l = lines(Lang::Kotlin, "val s = \"\"\"\n  raw \\n $x\n\"\"\"\n/* a /* b */ c */ val y = 1");
        assert_eq!(l[1], vec![("  raw \\n $x".into(), Kind::Str)]);
        assert_eq!(l[3][..2], [("/* a /* b */ c */".into(), Kind::Comment), ("val".into(), Kind::Keyword)]);

        let k = kinds(Lang::Swift, "#if DEBUG\nstruct Point: Codable { let x: Int; func move() { self.x } }");
        assert_eq!(k[0], ("#if".into(), Kind::Keyword));
        for want in [("struct", Kind::Keyword), ("Point", Kind::Type), ("let", Kind::Keyword), ("Int", Kind::Type), ("move", Kind::Function), ("self", Kind::Var)] {
            assert!(k.contains(&(want.0.into(), want.1)), "{want:?} in {k:?}");
        }
        let l = lines(Lang::Swift, "let t = \"\"\"\n  line \\(x)\n  \"\"\"");
        assert_eq!(l[1], vec![("  line \\(x)".into(), Kind::Str)]);

        let k = kinds(Lang::Dart, "Future<void> main() async { final s = r'\\d+'; }");
        for want in [("Future", Kind::Type), ("void", Kind::Type), ("main", Kind::Function), ("async", Kind::Keyword), ("final", Kind::Keyword), ("r'\\d+'", Kind::Str)] {
            assert!(k.contains(&(want.0.into(), want.1)), "{want:?} in {k:?}");
        }
        let l = lines(Lang::Dart, "var m = '''\nmulti\n''';");
        assert_eq!(l[1], vec![("multi".into(), Kind::Str)]);
        assert_eq!(
            (Lang::from_path(Some(Path::new("App.kt"))), Lang::from_path(Some(Path::new("main.swift"))), Lang::from_path(Some(Path::new("lib.dart")))),
            (Lang::Kotlin, Lang::Swift, Lang::Dart)
        );
    }

    #[test]
    fn ruby_lua_powershell() {
        let k = kinds(Lang::Ruby, "def valid?(x) = @items.empty? && $debug || :ok if defined?(Foo) # done");
        for want in [("def", Kind::Keyword), ("valid?", Kind::Function), ("@items", Kind::Var), ("$debug", Kind::Var), (":ok", Kind::Number), ("defined?", Kind::Keyword), ("Foo", Kind::Type), ("# done", Kind::Comment)] {
            assert!(k.contains(&(want.0.into(), want.1)), "{want:?} in {k:?}");
        }
        let l = lines(Lang::Ruby, "=begin\ndef hidden\n=end\nsql = <<~SQL\n  SELECT 1\nSQL\nx = A::B");
        assert_eq!(l[1], vec![("def hidden".into(), Kind::Comment)]);
        assert_eq!(l[2], vec![("=end".into(), Kind::Comment)]);
        assert_eq!(l[4], vec![("  SELECT 1".into(), Kind::Str)]);
        assert!(!l[6].iter().any(|s| s.1 == Kind::Number), "{:?}", l[6]);

        let l = lines(Lang::Lua, "--[[ block\nstill ]] local s = [==[\nraw ]] here\n]==] -- end\nprint(#t)");
        assert_eq!(l[1][..2], [("still ]]".into(), Kind::Comment), ("local".into(), Kind::Keyword)]);
        assert_eq!(l[2], vec![("raw ]] here".into(), Kind::Str)]);
        assert_eq!(l[3], vec![("]==]".into(), Kind::Str), ("-- end".into(), Kind::Comment)]);
        assert_eq!(l[4], vec![("print".into(), Kind::Function)]);

        let k = kinds(Lang::PowerShell, "Function Get-Info([string]$Path) { if ($env:DEBUG -eq $true) { Write-Host \"a`\"b\" } }");
        for want in [("Function", Kind::Keyword), ("Get-Info", Kind::Function), ("string", Kind::Type), ("$Path", Kind::Var), ("$env:DEBUG", Kind::Var), ("-eq", Kind::Keyword), ("$true", Kind::Var), ("Write-Host", Kind::Function)] {
            assert!(k.contains(&(want.0.into(), want.1)), "{want:?} in {k:?}");
        }
        let l = lines(Lang::PowerShell, "<# help\n#> $t = @\"\nhere $x\n\"@");
        assert_eq!(l[1][0], ("#>".into(), Kind::Comment));
        assert_eq!(l[2], vec![("here $x".into(), Kind::Str)]);
        assert_eq!(
            (Lang::from_path(Some(Path::new("Gemfile"))), Lang::from_path(Some(Path::new("init.lua"))), Lang::from_path(Some(Path::new("build.ps1"))), Lang::sniff("#!/usr/bin/env ruby\n")),
            (Lang::Ruby, Lang::Lua, Lang::PowerShell, Lang::Ruby)
        );
        assert_eq!((Lang::Lua.comment(), Lang::PowerShell.comment()), (Some(Comment::Line("--")), Some(Comment::Line("#"))));
    }

    #[test]
    fn toggling_comments() {
        let c = Lang::Python.comment().unwrap();
        assert_eq!(c, Comment::Line("#"));
        // At the common indentation, blank lines left alone; back again.
        let (on, ch) = toggle_comment("    a = 1\n\n        b = 2", c);
        assert_eq!(on, "    # a = 1\n\n    #     b = 2");
        assert_eq!(ch, vec![(4, 0, 2), (0, 0, 0), (4, 0, 2)]);
        assert_eq!(toggle_comment(&on, c).0, "    a = 1\n\n        b = 2");
        // Partly commented lines are all commented; CR LF kept; an empty line gets one.
        assert_eq!(toggle_comment("// x\r\ny\r\n", Comment::Line("//")).0, "// // x\r\n// y\r\n");
        assert_eq!(toggle_comment("", Comment::Line("//")).0, "// ");
        assert_eq!(toggle_comment("  //x", Comment::Line("//")).0, "  x");
        // A block comment around the lines, and off again.
        let x = Lang::Xml.comment().unwrap();
        let (on, _) = toggle_comment("  <a/>\r\n  <b/>", x);
        assert_eq!(on, "  <!-- <a/>\r\n  <b/> -->");
        assert_eq!(toggle_comment(&on, x).0, "  <a/>\r\n  <b/>");
        assert_eq!((Lang::Sql.comment(), Lang::Json.comment(), Lang::C.comment()), (Some(Comment::Line("--")), None, Some(Comment::Line("//"))));
    }

    #[test]
    fn word_lists_are_sorted_and_sync_lines() {
        for s in words::ALL {
            for l in [s.keywords, s.types] {
                assert!(l.windows(2).all(|w| w[0] < w[1]), "{l:?}");
            }
        }
        assert!(words::SH_BUILTINS.windows(2).all(|w| w[0] < w[1]));
        assert!(words::LITERALS.windows(2).all(|w| w[0] < w[1]));
        assert!(Lang::C.sync_line("#include <x>") && Lang::C.sync_line("int main(void)"));
        assert!(!Lang::C.sync_line(" * int") && !Lang::C.sync_line("integer = 1"));
        assert!(Lang::Java.sync_line("@Override") && Lang::Java.sync_line("public class A") && !Lang::Json.sync_line("{"));
    }

    #[test]
    fn never_panics_on_odd_input() {
        for lang in Lang::ALL {
            for s in ["\"", "<", "<!--", "'", "\"\\", "-", "a,\"", "[", "=", "/*", "R\"", "R\"(", "@\"", "$@", "\"\"\"", "#", "u8'", "/*/", "0x", "1e", "r#", "r#\"", "'\\", "#[", "b'", "x!", "'é", "<<<", "<<<'", "<<<\"A", "<?", "?>", "$", "$$"] {
                let mut v = Vec::new();
                spans(lang, s, &mut v);
                assert!(v.iter().all(|&(a, b, _)| a <= b && b <= s.len()));
            }
        }
    }
}
