//! Field queries over JSON Lines: `.level == "error" and .duration > 500`, `.user.id = 42`,
//! `.msg contains timeout`, `.err exists`, `.path ~ "^/api/"`. A line matches when it is a JSON
//! object meeting every condition.
//!
//! Operators: `==` (or `=`), `!=`, `>`, `<`, `>=`, `<=` (as numbers when both sides are numbers,
//! otherwise as text), `contains`, `~` (a regular expression), `exists` and `missing`. Values are
//! numbers, "quoted text", true, false, null, or a bare word.

use regex_automata::meta::Regex;

use crate::fields::{self, Syntax};

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
}

#[derive(Clone, Debug)]
struct Cond {
    /// Dotted key path: `user.id`.
    key: String,
    op: Op,
    value: String,
}

/// A parsed field query.
#[derive(Clone, Debug)]
pub struct FieldQuery {
    conds: Vec<Cond>,
}

/// Whether `text` looks like a field query rather than a search pattern.
pub fn looks_like_query(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with('.') && t[1..].chars().next().is_some_and(|c| c.is_alphabetic() || c == '_' || c == '@' || c == '"')
}

fn words(src: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let b: Vec<char> = src.chars().collect();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '"' {
            let mut s = String::from("\"");
            i += 1;
            loop {
                match b.get(i) {
                    None => return Err("text without a closing \"".into()),
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
            out.push(s);
        } else if "=!<>~".contains(c) {
            let two: String = b[i..(i + 2).min(b.len())].iter().collect();
            let op = ["==", "!=", ">=", "<="].into_iter().find(|o| *o == two).map(str::to_string).unwrap_or_else(|| c.to_string());
            i += op.chars().count();
            out.push(op);
        } else {
            let end = b[i..].iter().position(|x| x.is_whitespace() || "=!<>~\"".contains(*x)).map_or(b.len(), |n| i + n);
            out.push(b[i..end].iter().collect());
            i = end;
        }
    }
    Ok(out)
}

impl FieldQuery {
    pub fn parse(src: &str) -> Result<FieldQuery, String> {
        let w = words(src)?;
        let mut conds = Vec::new();
        let mut i = 0;
        while i < w.len() {
            let key = w[i].strip_prefix('.').ok_or_else(|| format!("a field starts with a dot: .{}", w[i]))?;
            let key = key.trim_matches('"').to_string();
            if key.is_empty() {
                return Err("a field name is missing after the dot".into());
            }
            let op_word = w.get(i + 1).ok_or_else(|| format!("what about .{key}? (==, !=, >, <, contains, ~, exists)"))?;
            let (op, takes) = match op_word.to_ascii_lowercase().as_str() {
                "==" | "=" => (Op::Eq, true),
                "!=" => (Op::Ne, true),
                ">" => (Op::Gt, true),
                "<" => (Op::Lt, true),
                ">=" => (Op::Ge, true),
                "<=" => (Op::Le, true),
                "contains" => (Op::Contains, true),
                "~" => (Op::Exists, true),
                "exists" => (Op::Exists, false),
                "missing" => (Op::Missing, false),
                other => return Err(format!("unknown operator {other}")),
            };
            let mut value = String::new();
            let mut next = i + 2;
            if takes {
                let v = w.get(i + 2).ok_or_else(|| format!("a value is missing after .{key} {op_word}"))?;
                value = v.strip_prefix('"').map(str::to_string).unwrap_or_else(|| v.clone());
                next = i + 3;
            }
            let op = if op_word == "~" { Op::Regex(Regex::new(&value).map_err(|e| format!("regex: {e}"))?) } else { op };
            conds.push(Cond { key, op, value });
            match w.get(next).map(|s| s.to_ascii_lowercase()) {
                None => break,
                Some(a) if a == "and" || a == "&&" => i = next + 1,
                Some(other) => return Err(format!("expected “and”, not {other}")),
            }
            if i >= w.len() {
                return Err("the query ends after “and”".into());
            }
        }
        if conds.is_empty() {
            return Err("the query is empty".into());
        }
        Ok(FieldQuery { conds })
    }

    /// Whether the JSON line (UTF-8 bytes) meets every condition.
    pub fn matches(&self, line: &[u8]) -> bool {
        let t = line.trim_ascii_start();
        if t.first() != Some(&b'{') {
            return false;
        }
        self.conds.iter().all(|c| {
            let v = fields::find_value(t, &c.key, Syntax::Json);
            match (&c.op, v) {
                (Op::Exists, v) => v.is_some(),
                (Op::Missing, v) => v.is_none(),
                (_, None) => false,
                (Op::Contains, Some(v)) => v.contains(&c.value),
                (Op::Regex(re), Some(v)) => re.is_match(v.as_bytes()),
                (op, Some(v)) => {
                    let ord = match (v.trim().parse::<f64>(), c.value.parse::<f64>()) {
                        (Ok(a), Ok(b)) => a.partial_cmp(&b),
                        _ => Some(v.as_str().cmp(c.value.as_str())),
                    };
                    let Some(ord) = ord else { return false };
                    use std::cmp::Ordering::*;
                    match op {
                        Op::Eq => ord == Equal,
                        Op::Ne => ord != Equal,
                        Op::Gt => ord == Greater,
                        Op::Lt => ord == Less,
                        Op::Ge => ord != Less,
                        Op::Le => ord != Greater,
                        _ => false,
                    }
                }
            }
        })
    }

    /// Text to highlight in the matching lines: the fields' names.
    pub fn highlight(&self) -> String {
        self.conds.first().map(|c| format!("\"{}\"", c.key.rsplit('.').next().unwrap_or(&c.key))).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(src: &str, line: &str) -> bool {
        FieldQuery::parse(src).unwrap_or_else(|e| panic!("{src}: {e}")).matches(line.as_bytes())
    }

    const L: &str = r#"{"level":"error","duration":750,"user":{"id":42,"name":"Ann"},"msg":"db timeout after 3 tries","path":"/api/v1/x","ok":false}"#;

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
        // Not an object.
        assert!(!q(".level exists", "plain text line"));
    }

    #[test]
    fn errors_and_detection() {
        for bad in ["level == x", ".level", ".level === x", ".level == x or .a == b", ".x ~ \"(\"", ".a == 1 and", ""] {
            assert!(FieldQuery::parse(bad).is_err(), "{bad} parsed");
        }
        assert!(looks_like_query(".level == error"));
        assert!(!looks_like_query(".*error"));
        assert!(!looks_like_query("error"));
    }
}
