//! Field queries over structured log lines: JSON Lines (`.level == "error" and .duration > 500`,
//! `.user.id = 42`), logfmt (`level=error duration=750`), and, given the log's [`Format`], web
//! server and syslog logs (`.status >= 500 and .method = POST`). A line matches when it has fields
//! and they meet the query; a line without any never does.
//!
//! Conditions: `==` (or `=`), `!=`, `>`, `<`, `>=`, `<=` (as numbers when both sides are numbers,
//! as times when the value is a time — one without a date compares with the time of day, so
//! `.time >= 10:00 and .time < 11:00` is an hour — and otherwise as text), `in (a, b, "c d")`,
//! `contains`, `~` (a regular expression), `exists` and `missing`. They combine with `and`, `or`,
//! `not` and parentheses; `and` binds tighter than `or`. Values are numbers, "quoted text", true,
//! false, null, or a bare word. A mistake is reported with the character it is at.

use std::cmp::Ordering;

use regex_automata::meta::Regex;

use crate::fields::{self, Syntax};
use crate::logformat::{Format, Split, Splitter};
use crate::timestamp::{self, Stamp};

#[derive(Clone, Debug)]
enum Op {
    Eq,
    Ne,
    Gt,
    Lt,
    Ge,
    Le,
    Contains,
    Regex(Regex),
    Exists,
    Missing,
    In(Vec<Value>),
}

/// A value of the query, read once: as text, as a number and as a time when it is one.
#[derive(Clone, Debug)]
struct Value {
    text: String,
    num: Option<f64>,
    time: Option<Stamp>,
}

impl Value {
    fn new(text: String) -> Value {
        let num = text.trim().parse::<f64>().ok();
        let time = if num.is_none() {
            timestamp::parse_input(&text)
        } else {
            None
        };
        Value { text, num, time }
    }

    /// How a field's value `v` compares with this one.
    fn cmp_field(&self, v: &str) -> Option<Ordering> {
        if let Some(b) = self.num {
            if let Ok(a) = v.trim().parse::<f64>() {
                return a.partial_cmp(&b);
            }
        }
        if let Some(b) = self.time {
            if let Some(a) = timestamp::parse(v.trim()) {
                return Some(if b.has_date {
                    a.ms.cmp(&b.ms)
                } else {
                    a.time_of_day().cmp(&b.time_of_day())
                });
            }
        }
        Some(v.cmp(self.text.as_str()))
    }
}

#[derive(Clone, Debug)]
struct Cond {
    /// Dotted key path: `user.id`.
    key: String,
    op: Op,
    value: Value,
}

#[derive(Clone, Debug)]
enum Node {
    Cond(Cond),
    And(Vec<Node>),
    Or(Vec<Node>),
    Not(Box<Node>),
}

/// A parsed field query.
#[derive(Clone, Debug)]
pub struct FieldQuery {
    root: Node,
    /// The first condition's field (highlighted in the kept lines).
    first_key: String,
    /// The first value a condition looks for with `==` or `in` (highlighted in log rows).
    first_value: Option<String>,
    /// Web server or syslog lines: split into these columns.
    format: Option<Format>,
    /// The format's column names, lowercase.
    columns: Vec<String>,
    /// The document is logfmt: its field names are highlighted as `key=`.
    logfmt: bool,
}

/// Whether `text` looks like a field query rather than a search pattern.
pub fn looks_like_query(text: &str) -> bool {
    let t = text.trim_start();
    let t = t
        .strip_prefix('(')
        .or_else(|| t.strip_prefix("not "))
        .map_or(t, str::trim_start);
    t.starts_with('.')
        && t[1..]
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '@' || c == '"')
}

/// The query's words with the character each starts at.
fn words(src: &str) -> Result<Vec<(usize, String)>, String> {
    let mut out = Vec::new();
    let b: Vec<char> = src.chars().collect();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '"' {
            let start = i;
            let mut s = String::from("\"");
            i += 1;
            loop {
                match b.get(i) {
                    None => return Err(format!("text without a closing \" (at {})", start + 1)),
                    Some('\\') if i + 1 < b.len() => {
                        s.push(b[i + 1]);
                        i += 2;
                    }
                    Some('"') => {
                        i += 1;
                        break;
                    }
                    Some(&ch) => {
                        s.push(ch);
                        i += 1;
                    }
                }
            }
            out.push((start, s));
        } else if "(),".contains(c) {
            out.push((i, c.to_string()));
            i += 1;
        } else if "=!<>~".contains(c) {
            let two: String = b[i..(i + 2).min(b.len())].iter().collect();
            let op = ["==", "!=", ">=", "<="]
                .into_iter()
                .find(|o| *o == two)
                .map(str::to_string)
                .unwrap_or_else(|| c.to_string());
            out.push((i, op.clone()));
            i += op.chars().count();
        } else {
            let end = b[i..]
                .iter()
                .position(|x| x.is_whitespace() || "=!<>~\"(),".contains(*x))
                .map_or(b.len(), |n| i + n);
            out.push((i, b[i..end].iter().collect()));
            i = end;
        }
    }
    Ok(out)
}

/// A recursive-descent parser over the words.
struct Parser {
    w: Vec<(usize, String)>,
    i: usize,
    len: usize,
    first_key: Option<String>,
    first_value: Option<String>,
}

impl Parser {
    fn peek(&self) -> Option<&str> {
        self.w.get(self.i).map(|t| t.1.as_str())
    }

    fn lower(&self) -> Option<String> {
        self.peek().map(str::to_ascii_lowercase)
    }

    /// `msg`, with the character the current word starts at (or the end).
    fn err(&self, msg: impl std::fmt::Display) -> String {
        let at = self.w.get(self.i).map_or(self.len, |t| t.0) + 1;
        format!("{msg} (at {at})")
    }

    fn expr(&mut self) -> Result<Node, String> {
        let mut v = vec![self.and()?];
        while matches!(self.lower().as_deref(), Some("or" | "||")) {
            self.i += 1;
            if self.peek().is_none() {
                return Err(self.err("the query ends after “or”"));
            }
            v.push(self.and()?);
        }
        Ok(if v.len() == 1 {
            v.remove(0)
        } else {
            Node::Or(v)
        })
    }

    fn and(&mut self) -> Result<Node, String> {
        let mut v = vec![self.unary()?];
        while matches!(self.lower().as_deref(), Some("and" | "&&")) {
            self.i += 1;
            if self.peek().is_none() {
                return Err(self.err("the query ends after “and”"));
            }
            v.push(self.unary()?);
        }
        Ok(if v.len() == 1 {
            v.remove(0)
        } else {
            Node::And(v)
        })
    }

    fn unary(&mut self) -> Result<Node, String> {
        match self.lower().as_deref() {
            Some("not" | "!") => {
                self.i += 1;
                Ok(Node::Not(Box::new(self.unary()?)))
            }
            Some("(") => {
                self.i += 1;
                let n = self.expr()?;
                if self.peek() != Some(")") {
                    return Err(self.err("a “)” is missing"));
                }
                self.i += 1;
                Ok(n)
            }
            Some(_) => self.cond(),
            None => Err(self.err("a condition is missing")),
        }
    }

    /// A word as a value: quotes taken off.
    fn value(&mut self, after: &str) -> Result<String, String> {
        let Some(v) = self.peek().map(str::to_string) else {
            return Err(self.err(format!("a value is missing after {after}")));
        };
        if v == "(" || v == ")" || v == "," {
            return Err(self.err(format!("a value is missing after {after}")));
        }
        self.i += 1;
        Ok(v.strip_prefix('"').map(str::to_string).unwrap_or(v))
    }

    fn cond(&mut self) -> Result<Node, String> {
        let word = self.peek().unwrap_or_default().to_string();
        let Some(key) = word.strip_prefix('.') else {
            return Err(self.err(format!("a field starts with a dot: .{word}")));
        };
        let key = key.trim_matches('"').to_string();
        if key.is_empty() {
            return Err(self.err("a field name is missing after the dot"));
        }
        self.i += 1;
        let Some(op_word) = self.peek().map(str::to_string) else {
            return Err(self.err(format!(
                "what about .{key}? (==, !=, >, <, in, contains, ~, exists)"
            )));
        };
        let at = self.i;
        self.i += 1;
        let after = format!(".{key} {op_word}");
        let (op, value) = match op_word.to_ascii_lowercase().as_str() {
            "==" | "=" => (Op::Eq, self.value(&after)?),
            "!=" => (Op::Ne, self.value(&after)?),
            ">" => (Op::Gt, self.value(&after)?),
            "<" => (Op::Lt, self.value(&after)?),
            ">=" => (Op::Ge, self.value(&after)?),
            "<=" => (Op::Le, self.value(&after)?),
            "contains" => (Op::Contains, self.value(&after)?),
            "~" => {
                let v = self.value(&after)?;
                let re = Regex::new(&v).map_err(|e| format!("regex: {e}"))?;
                (Op::Regex(re), v)
            }
            "exists" => (Op::Exists, String::new()),
            "missing" => (Op::Missing, String::new()),
            "in" => {
                if self.peek() != Some("(") {
                    return Err(self.err(format!("a “(” is missing after .{key} in")));
                }
                self.i += 1;
                let mut list = Vec::new();
                loop {
                    list.push(Value::new(self.value(&after)?));
                    match self.peek() {
                        Some(",") => self.i += 1,
                        Some(")") => {
                            self.i += 1;
                            break;
                        }
                        _ => return Err(self.err("a “,” or “)” is missing")),
                    }
                }
                (Op::In(list), String::new())
            }
            other => {
                self.i = at;
                return Err(self.err(format!("unknown operator {other}")));
            }
        };
        self.first_key.get_or_insert_with(|| key.clone());
        if self.first_value.is_none() {
            self.first_value = match &op {
                Op::Eq => Some(value.clone()),
                Op::In(list) => list.first().map(|v| v.text.clone()),
                _ => None,
            };
        }
        Ok(Node::Cond(Cond {
            key,
            op,
            value: Value::new(value),
        }))
    }
}

fn holds(c: &Cond, v: Option<String>) -> bool {
    match (&c.op, v) {
        (Op::Exists, v) => v.is_some(),
        (Op::Missing, v) => v.is_none(),
        (_, None) => false,
        (Op::Contains, Some(v)) => v.contains(&c.value.text),
        (Op::Regex(re), Some(v)) => re.is_match(v.as_bytes()),
        (Op::In(list), Some(v)) => list
            .iter()
            .any(|x| x.cmp_field(&v) == Some(Ordering::Equal)),
        (op, Some(v)) => {
            let Some(ord) = c.value.cmp_field(&v) else {
                return false;
            };
            match op {
                Op::Eq => ord == Ordering::Equal,
                Op::Ne => ord != Ordering::Equal,
                Op::Gt => ord == Ordering::Greater,
                Op::Lt => ord == Ordering::Less,
                Op::Ge => ord != Ordering::Less,
                Op::Le => ord != Ordering::Greater,
                _ => false,
            }
        }
    }
}

fn eval(n: &Node, get: &dyn Fn(&str) -> Option<String>) -> bool {
    match n {
        Node::Cond(c) => holds(c, get(&c.key)),
        Node::And(v) => v.iter().all(|x| eval(x, get)),
        Node::Or(v) => v.iter().any(|x| eval(x, get)),
        Node::Not(x) => !eval(x, get),
    }
}

impl FieldQuery {
    pub fn parse(src: &str) -> Result<FieldQuery, String> {
        let w = words(src)?;
        if w.is_empty() {
            return Err("the query is empty".into());
        }
        let mut p = Parser {
            w,
            i: 0,
            len: src.chars().count(),
            first_key: None,
            first_value: None,
        };
        let root = p.expr()?;
        if let Some(extra) = p.peek() {
            let msg = format!("expected “and” or “or”, not {extra}");
            return Err(p.err(msg));
        }
        Ok(FieldQuery {
            root,
            first_key: p.first_key.unwrap_or_default(),
            first_value: p.first_value.filter(|v| !v.is_empty()),
            format: None,
            columns: Vec::new(),
            logfmt: false,
        })
    }

    /// Reads lines as rows of `format` (a web server or syslog log), its columns being the
    /// fields; `None` keeps JSON Lines and logfmt (`logfmt`: the document is logfmt, for the
    /// highlight).
    pub fn for_document(mut self, format: Option<Format>, logfmt: bool) -> FieldQuery {
        self.logfmt = logfmt && format.is_none();
        self.columns = format
            .as_ref()
            .map(|f| f.columns.iter().map(|c| c.to_ascii_lowercase()).collect())
            .unwrap_or_default();
        self.format = format;
        self
    }

    /// Whether the line (UTF-8 bytes) has fields that meet the query.
    pub fn matches(&self, line: &[u8]) -> bool {
        if let Some(f) = &self.format {
            let mut s = Splitter::new(f);
            if s.split(line) != Split::Row {
                return false;
            }
            let values = s.values(line);
            return eval(&self.root, &|key| {
                let key = key.to_ascii_lowercase();
                let i = self.columns.iter().position(|c| *c == key)?;
                let v = values.get(i)?;
                (!v.is_empty()).then(|| String::from_utf8_lossy(v).into_owned())
            });
        }
        let t = line.trim_ascii_start();
        if t.first() == Some(&b'{') {
            return eval(&self.root, &|key| fields::find_value(t, key, Syntax::Json));
        }
        if memchr::memchr(b'=', line).is_none() {
            return false;
        }
        eval(&self.root, &|key| {
            fields::find_value(line, key, Syntax::Logfmt)
        })
    }

    /// Text to highlight in the matching lines (never empty): the first field's name as JSON
    /// writes it (`"level"`), or as logfmt does (`level=`); in web server and syslog rows, which
    /// have no names, the first value looked for.
    pub fn highlight(&self) -> String {
        let leaf = self.first_key.rsplit('.').next().unwrap_or(&self.first_key);
        if self.format.is_some() {
            return self.first_value.clone().unwrap_or_else(|| leaf.to_string());
        }
        if self.logfmt {
            format!("{leaf}=")
        } else {
            format!("\"{leaf}\"")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(src: &str, line: &str) -> bool {
        FieldQuery::parse(src)
            .unwrap_or_else(|e| panic!("{src}: {e}"))
            .matches(line.as_bytes())
    }

    const L: &str = r#"{"level":"error","duration":750,"user":{"id":42,"name":"Ann"},"msg":"db timeout after 3 tries","path":"/api/v1/x","ok":false,"time":"2026-11-13T10:42:07Z"}"#;

    #[test]
    fn conditions() {
        assert!(q(r#".level == "error""#, L));
        assert!(q(".level = error", L));
        assert!(!q(".level != error", L));
        assert!(q(".duration > 500 and .duration <= 750", L));
        assert!(!q(".duration < 500", L));
        assert!(q(".user.id = 42", L));
        assert!(q(".user.name contains An", L));
        assert!(q(".msg contains timeout", L));
        assert!(q(r#".path ~ "^/api/v[0-9]""#, L));
        assert!(q(".ok == false", L));
        assert!(q(".msg exists and .err missing", L));
        assert!(!q(".err exists", L));
        // Numbers compare as numbers, text as text.
        assert!(q(".duration > 80", L));
        assert!(!q(".level > x", L));
        // Not a line with fields.
        assert!(!q(".level exists", "plain text line"));
        assert!(!q(".err missing", "plain text line"));
    }

    #[test]
    fn or_not_parentheses_in_and_times() {
        assert!(q(".level == warn or .duration > 500", L));
        assert!(!q(".level == warn or .duration > 900", L));
        assert!(q("not .level == warn", L));
        assert!(q(
            "(.level == warn or .level == error) and .user.id = 42",
            L
        ));
        assert!(
            !q(".level == warn or .level == error and .user.id = 7", L),
            "and binds tighter"
        );
        assert!(q(".level in (warn, error)", L));
        assert!(q(".user.id in (7, 42.0)", L));
        assert!(!q(r#".level in ("info", "debug")"#, L));
        assert!(q(".time >= 10:00 and .time < 11:00", L));
        assert!(!q(".time >= 11:00", L));
        assert!(q(r#".time > "2026-11-13 10:00""#, L));
        assert!(!q(r#".time > "2026-11-14""#, L));
    }

    #[test]
    fn logfmt_lines() {
        let line =
            r#"time=2026-11-13T10:42:07Z level=error duration=750 msg="db timeout" path=/api/v1/x"#;
        assert!(q(".level = error and .duration > 500", line));
        assert!(q(".msg contains timeout", line));
        assert!(q(".path ~ ^/api", line));
        assert!(!q(".level = info", line));
    }

    #[test]
    fn web_server_rows_by_column() {
        let lines = [
            r#"10.0.0.1 - - [13/Nov/2026:10:00:00 +0000] "GET /a HTTP/1.1" 200 512 "-" "curl/8""#,
            r#"10.0.0.2 - - [13/Nov/2026:10:30:00 +0000] "POST /api/pay HTTP/1.1" 502 0 "-" "curl/8""#,
        ];
        let f = Format::guess(&lines).expect("an access log");
        let q = FieldQuery::parse(".status >= 500 and .method = POST")
            .unwrap()
            .for_document(Some(f.clone()), false);
        assert!(!q.matches(lines[0].as_bytes()));
        assert!(q.matches(lines[1].as_bytes()));
        let q = FieldQuery::parse(".url in (/a, /b) or .time >= 10:15")
            .unwrap()
            .for_document(Some(f), false);
        assert!(q.matches(lines[0].as_bytes()) && q.matches(lines[1].as_bytes()));
        assert!(!q.matches(b"java.lang.Exception: not a row"));
    }

    #[test]
    fn errors_and_detection() {
        for bad in [
            "level == x",
            ".level",
            ".level === x",
            ".x ~ \"(\"",
            ".a == 1 and",
            ".a == 1 or",
            "(.a == 1",
            ".a in (1, 2",
            ".a in 1",
            "",
        ] {
            assert!(FieldQuery::parse(bad).is_err(), "{bad} parsed");
        }
        let e = FieldQuery::parse(".a == 1 xor .b == 2").unwrap_err();
        assert!(e.contains("(at 9)"), "{e}");
        assert!(looks_like_query(".level == error"));
        assert!(looks_like_query("(.a == 1 or .b == 2)"));
        assert!(looks_like_query("not .a exists"));
        assert!(!looks_like_query(".*error"));
        assert!(!looks_like_query("error"));
    }
}
