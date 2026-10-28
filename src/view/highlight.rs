//! Simple syntax highlighting for common formats.
//!
//! Highlighting is row-local and runs only on the visible slice of each visible row, so it costs
//! nothing for the rest of the file. Constructs spanning rows (multi-line strings or comments)
//! are not tracked — this is a lightweight editor, not an IDE.

use std::path::Path;

use egui::Color32;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    Plain,
    Log,
    Json,
    Xml,
    Csv(u8),
    Sql,
    Ini,
}

impl Lang {
    pub const ALL: [Lang; 8] = [Lang::Plain, Lang::Log, Lang::Json, Lang::Xml, Lang::Csv(b','), Lang::Csv(b'\t'), Lang::Sql, Lang::Ini];

    pub fn from_path(p: Option<&Path>) -> Lang {
        let ext = p.and_then(|p| p.extension()).map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        match ext.as_str() {
            "log" | "out" | "err" => Lang::Log,
            "json" | "jsonl" | "ndjson" | "geojson" | "har" => Lang::Json,
            "xml" | "html" | "htm" | "xhtml" | "svg" | "xaml" | "csproj" | "vcxproj" | "props" | "config" | "plist" | "xsd" | "wsdl" => Lang::Xml,
            "csv" => Lang::Csv(b','),
            "tsv" | "tab" => Lang::Csv(b'\t'),
            "sql" | "ddl" | "dml" => Lang::Sql,
            "ini" | "toml" | "cfg" | "conf" | "yaml" | "yml" | "properties" | "env" | "inf" => Lang::Ini,
            _ => Lang::Plain,
        }
    }

    /// Guesses the format of a file whose name says nothing from its first lines: most lines
    /// starting with a time make a log, a `{` or `[` JSON, `<?xml` or a tag XML.
    pub fn sniff(text: &str) -> Lang {
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
}

pub fn color(kind: Kind, dark: bool) -> Color32 {
    let (d, l) = match kind {
        Kind::Keyword => ((198, 120, 221), (152, 24, 160)),
        Kind::Str => ((152, 195, 121), (40, 120, 30)),
        Kind::Number => ((209, 154, 102), (170, 90, 0)),
        Kind::Comment => ((110, 118, 129), (120, 120, 120)),
        Kind::Tag => ((86, 156, 214), (20, 70, 180)),
        Kind::Attr => ((156, 220, 254), (150, 60, 30)),
        Kind::Key => ((97, 175, 239), (10, 90, 170)),
        Kind::Error => ((240, 90, 90), (200, 0, 0)),
        Kind::Warn => ((229, 192, 123), (170, 110, 0)),
        Kind::Info => ((120, 200, 120), (0, 120, 50)),
        Kind::Debug => ((128, 128, 128), (130, 130, 130)),
        Kind::Time => ((86, 182, 194), (0, 110, 130)),
        Kind::Punct => ((171, 178, 191), (90, 90, 90)),
        Kind::Col(i) => {
            const DARK: [(u8, u8, u8); 6] = [(229, 192, 123), (97, 175, 239), (152, 195, 121), (198, 120, 221), (86, 182, 194), (224, 108, 117)];
            const LIGHT: [(u8, u8, u8); 6] = [(150, 100, 0), (10, 90, 170), (40, 120, 30), (152, 24, 160), (0, 110, 130), (180, 30, 50)];
            (DARK[i as usize % 6], LIGHT[i as usize % 6])
        }
    };
    let (r, g, b) = if dark { d } else { l };
    Color32::from_rgb(r, g, b)
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

/// Byte spans (into `text`, the visible slice of one row) with their kinds.
pub fn spans(lang: Lang, text: &str, out: &mut Vec<(usize, usize, Kind)>) {
    let t = text.as_bytes();
    match lang {
        Lang::Plain => {}
        Lang::Log => log_spans(t, out),
        Lang::Json => json_spans(t, out),
        Lang::Xml => xml_spans(t, out),
        Lang::Csv(sep) => csv_spans(t, sep, out),
        Lang::Sql => sql_spans(t, out),
        Lang::Ini => ini_spans(t, out),
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

fn xml_spans(t: &[u8], out: &mut Vec<(usize, usize, Kind)>) {
    let mut i = 0;
    let mut in_tag = false;
    while i < t.len() {
        if !in_tag {
            if t[i..].starts_with(b"<!--") {
                let e = memchr::memmem::find(&t[i..], b"-->").map(|p| i + p + 3).unwrap_or(t.len());
                out.push((i, e, Kind::Comment));
                i = e;
            } else if t[i] == b'<' {
                let mut e = i + 1;
                while e < t.len() && (t[e] == b'/' || t[e] == b'?' || t[e] == b'!') {
                    e += 1;
                }
                while e < t.len() && (is_ident(t[e]) || t[e] == b':' || t[e] == b'-' || t[e] == b'.') {
                    e += 1;
                }
                out.push((i, e, Kind::Tag));
                in_tag = true;
                i = e;
            } else if t[i] == b'&' {
                let e = t[i..].iter().position(|&b| b == b';' || b == b' ').map(|p| i + p + 1).unwrap_or(t.len()).min(i + 12);
                out.push((i, e, Kind::Keyword));
                i = e;
            } else {
                i += 1;
            }
        } else {
            let c = t[i];
            if c == b'>' || (c == b'/' && t.get(i + 1) == Some(&b'>')) || (c == b'?' && t.get(i + 1) == Some(&b'>')) {
                let e = if c == b'>' { i + 1 } else { i + 2 };
                out.push((i, e, Kind::Tag));
                in_tag = false;
                i = e;
            } else if c == b'"' || c == b'\'' {
                let e = string_end(t, i, c, false);
                out.push((i, e, Kind::Str));
                i = e;
            } else if is_ident(c) {
                let mut e = i;
                while e < t.len() && (is_ident(t[e]) || t[e] == b':' || t[e] == b'-' || t[e] == b'.') {
                    e += 1;
                }
                out.push((i, e, Kind::Attr));
                i = e;
            } else {
                i += 1;
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

fn sql_spans(t: &[u8], out: &mut Vec<(usize, usize, Kind)>) {
    let mut i = 0;
    while i < t.len() {
        let c = t[i];
        if t[i..].starts_with(b"--") || c == b'#' {
            out.push((i, t.len(), Kind::Comment));
            return;
        }
        if t[i..].starts_with(b"/*") {
            let e = memchr::memmem::find(&t[i..], b"*/").map(|p| i + p + 2).unwrap_or(t.len());
            out.push((i, e, Kind::Comment));
            i = e;
        } else if c == b'\'' || c == b'"' || c == b'`' {
            let e = string_end(t, i, c, true);
            out.push((i, e, if c == b'`' { Kind::Key } else { Kind::Str }));
            i = e;
        } else if c.is_ascii_digit() {
            let e = number_end(t, i);
            out.push((i, e, Kind::Number));
            i = e;
        } else if is_ident(c) {
            let e = word_end(t, i);
            let w = String::from_utf8_lossy(&t[i..e]).to_ascii_lowercase();
            if SQL_KEYWORDS.contains(&w.as_str()) {
                out.push((i, e, Kind::Keyword));
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

    #[test]
    fn never_panics_on_odd_input() {
        for lang in Lang::ALL {
            for s in ["\"", "<", "<!--", "'", "\"\\", "-", "a,\"", "[", "=", "/*"] {
                let mut v = Vec::new();
                spans(lang, s, &mut v);
                assert!(v.iter().all(|&(a, b, _)| a <= b && b <= s.len()));
            }
        }
    }
}
