//! Text operations on a selection: sorting and de-duplicating lines, changing case, trimming,
//! converting indentation, and re-formatting JSON. They work on decoded text and keep the
//! text's own line breaks; the caller applies the result as one undoable edit.

use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
}

impl Op {
    pub const ALL: [Op; 12] = [
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
        }
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
    })
}

/// Re-formats JSON token by token: key order, numbers and string escapes stay exactly as
/// written. Several top-level values (JSON Lines) stay on separate lines. With `indent` the
/// output is indented, otherwise all insignificant whitespace is removed.
pub fn json_format(text: &str, indent: Option<&str>, eol: &str) -> Result<String, String> {
    let mut out = String::with_capacity(text.len() + text.len() / 2);
    let mut stack: Vec<char> = Vec::new();
    let mut in_str = false;
    let mut escaped = false;
    // A line break (and indentation) is due before the next token.
    let mut pending = false;
    // The last token was an opening bracket (so a closing one makes `{}` / `[]`).
    let mut after_open = false;
    // A line break between top-level values.
    let mut top_break = false;
    let mut started = false;
    let newline = |out: &mut String, depth: usize| {
        if let Some(ind) = indent {
            out.push_str(eol);
            for _ in 0..depth {
                out.push_str(ind);
            }
        }
    };
    let mut line = 1usize;
    for c in text.chars() {
        if in_str {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            } else if c == '\n' {
                return Err(format!("Line break inside a JSON string (line {line})."));
            }
            continue;
        }
        if c.is_whitespace() {
            if c == '\n' {
                line += 1;
                if stack.is_empty() && started {
                    top_break = true;
                }
            }
            continue;
        }
        let closing = matches!(c, '}' | ']');
        if closing {
            let open = stack.pop().ok_or_else(|| format!("Unexpected “{c}” (line {line})."))?;
            if (open == '{') != (c == '}') {
                return Err(format!("“{c}” does not close “{open}” (line {line})."));
            }
            if !after_open {
                newline(&mut out, stack.len());
            }
            out.push(c);
            pending = false;
            after_open = false;
            continue;
        }
        if stack.is_empty() && started && top_break {
            out.push_str(eol);
        }
        top_break = false;
        if pending {
            newline(&mut out, stack.len());
            pending = false;
        }
        after_open = false;
        started = true;
        match c {
            '"' => {
                in_str = true;
                out.push(c);
            }
            '{' | '[' => {
                out.push(c);
                stack.push(c);
                pending = true;
                after_open = true;
            }
            ',' => {
                if stack.is_empty() {
                    return Err(format!("Unexpected “,” outside an object or array (line {line})."));
                }
                out.push(c);
                pending = true;
            }
            ':' => {
                out.push(':');
                if indent.is_some() {
                    out.push(' ');
                }
            }
            _ => out.push(c),
        }
    }
    if in_str {
        return Err("A JSON string is not closed.".into());
    }
    if let Some(open) = stack.last() {
        return Err(format!("“{open}” is not closed."));
    }
    if text.ends_with(eol) {
        out.push_str(eol);
    }
    Ok(out)
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
