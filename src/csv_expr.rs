//! Expressions over a table row, for derived columns: `{price} * {qty}`,
//! `substr({ts}, 0, 10)`, `regex({url}, "id=([0-9]+)")`, `if({status} = "ok", 1, 0)`.
//!
//! `{name}` is the value of the column with that header name, `{3}` the third column. Values
//! are text; `+ - * /` read both sides as numbers (a side that is not a number makes the result
//! empty), `&` joins text, `= <> < > <= >=` compare as numbers when both sides are numbers and as
//! text otherwise, giving 1 or 0, and `and`, `or`, `not` take 0 and empty text as false.
//! Functions: upper, lower, trim, len, substr(text, start[, length]) (characters, from 0),
//! replace(text, from, to), regex(text, pattern[, group]) (group 1 by default, 0 the whole
//! match), if(condition, then, else), num, abs, round(x[, digits]).

use regex_automata::meta::Regex;

use crate::csv::parse_number;

#[derive(Clone, Debug)]
enum Node {
    Col(usize),
    Text(String),
    Num(f64),
    Neg(Box<Node>),
    Not(Box<Node>),
    Bin(Op, Box<Node>, Box<Node>),
    Call(Func, Vec<Node>),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Op {
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    Cat,
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Clone, Debug)]
enum Func {
    Upper,
    Lower,
    Trim,
    Len,
    Substr,
    Replace,
    /// The pattern compiled once, when it is a literal.
    Regex(Option<Regex>),
    If,
    Num,
    Abs,
    Round,
}

/// A compiled expression.
#[derive(Clone, Debug)]
pub struct Expr(Node);

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Col(String),
    Text(String),
    Num(f64),
    Ident(String),
    Sym(&'static str),
}

fn tokens(src: &str) -> Result<Vec<(Tok, usize)>, String> {
    let mut out = Vec::new();
    let b: Vec<char> = src.chars().collect();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        let at = i;
        if c.is_whitespace() {
            i += 1;
        } else if c == '{' {
            let end = b[i..].iter().position(|&x| x == '}').ok_or(format!("a {{ without }} at {}", at + 1))? + i;
            out.push((Tok::Col(b[i + 1..end].iter().collect()), at));
            i = end + 1;
        } else if c == '"' {
            let mut s = String::new();
            i += 1;
            loop {
                match b.get(i) {
                    None => return Err(format!("text without a closing \" at {}", at + 1)),
                    Some('"') if b.get(i + 1) == Some(&'"') => {
                        s.push('"');
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
            out.push((Tok::Text(s), at));
        } else if c.is_ascii_digit() || (c == '.' && b.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            let end = b[i..].iter().position(|x| !(x.is_ascii_digit() || *x == '.')).map_or(b.len(), |n| i + n);
            let t: String = b[i..end].iter().collect();
            out.push((Tok::Num(t.parse().map_err(|_| format!("not a number: {t}"))?), at));
            i = end;
        } else if c.is_alphabetic() || c == '_' {
            let end = b[i..].iter().position(|x| !(x.is_alphanumeric() || *x == '_')).map_or(b.len(), |n| i + n);
            out.push((Tok::Ident(b[i..end].iter().collect::<String>().to_lowercase()), at));
            i = end;
        } else {
            let two: String = b[i..(i + 2).min(b.len())].iter().collect();
            let sym = ["<>", "<=", ">=", "!="].into_iter().find(|s| *s == two).or_else(|| ["+", "-", "*", "/", "&", "=", "<", ">", "(", ")", ","].into_iter().find(|s| s.starts_with(c)));
            let Some(s) = sym else { return Err(format!("unexpected {c} at {}", at + 1)) };
            out.push((Tok::Sym(if s == "!=" { "<>" } else { s }), at));
            i += s.chars().count();
        }
    }
    Ok(out)
}

struct Parser<'a> {
    toks: Vec<(Tok, usize)>,
    pos: usize,
    names: &'a [String],
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos).map(|t| &t.0)
    }

    fn eat(&mut self, sym: &str) -> bool {
        if self.peek() == Some(&Tok::Sym(match sym {
            "(" => "(",
            ")" => ")",
            "," => ",",
            _ => return false,
        })) {
            self.pos += 1;
            return true;
        }
        false
    }

    fn binop(&self) -> Option<(Op, u8)> {
        Some(match self.peek()? {
            Tok::Ident(w) if w == "or" => (Op::Or, 1),
            Tok::Ident(w) if w == "and" => (Op::And, 2),
            Tok::Sym("=") => (Op::Eq, 4),
            Tok::Sym("<>") => (Op::Ne, 4),
            Tok::Sym("<") => (Op::Lt, 4),
            Tok::Sym(">") => (Op::Gt, 4),
            Tok::Sym("<=") => (Op::Le, 4),
            Tok::Sym(">=") => (Op::Ge, 4),
            Tok::Sym("&") => (Op::Cat, 5),
            Tok::Sym("+") => (Op::Add, 6),
            Tok::Sym("-") => (Op::Sub, 6),
            Tok::Sym("*") => (Op::Mul, 7),
            Tok::Sym("/") => (Op::Div, 7),
            _ => return None,
        })
    }

    fn expr(&mut self, min: u8) -> Result<Node, String> {
        let mut left = self.unary()?;
        while let Some((op, prec)) = self.binop() {
            if prec < min {
                break;
            }
            self.pos += 1;
            let right = self.expr(prec + 1)?;
            left = Node::Bin(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<Node, String> {
        match self.peek() {
            Some(Tok::Sym("-")) => {
                self.pos += 1;
                Ok(Node::Neg(Box::new(self.unary()?)))
            }
            Some(Tok::Ident(w)) if w == "not" => {
                self.pos += 1;
                Ok(Node::Not(Box::new(self.expr(3)?)))
            }
            _ => self.atom(),
        }
    }

    fn atom(&mut self) -> Result<Node, String> {
        let Some((tok, at)) = self.toks.get(self.pos).cloned() else { return Err("the expression ends too early".into()) };
        self.pos += 1;
        match tok {
            Tok::Num(n) => Ok(Node::Num(n)),
            Tok::Text(s) => Ok(Node::Text(s)),
            Tok::Col(name) => {
                if let Some(i) = self.names.iter().position(|n| n.trim() == name.trim()) {
                    return Ok(Node::Col(i));
                }
                match name.trim().parse::<usize>() {
                    Ok(n) if n > 0 => Ok(Node::Col(n - 1)),
                    _ => Err(format!("no column named {{{name}}}")),
                }
            }
            Tok::Sym("(") => {
                let e = self.expr(0)?;
                if !self.eat(")") {
                    return Err(format!("a ( at {} without )", at + 1));
                }
                Ok(e)
            }
            Tok::Ident(f) => {
                let func = match f.as_str() {
                    "upper" => Func::Upper,
                    "lower" => Func::Lower,
                    "trim" => Func::Trim,
                    "len" => Func::Len,
                    "substr" => Func::Substr,
                    "replace" => Func::Replace,
                    "regex" => Func::Regex(None),
                    "if" => Func::If,
                    "num" => Func::Num,
                    "abs" => Func::Abs,
                    "round" => Func::Round,
                    _ => return Err(format!("unknown function {f}")),
                };
                if !self.eat("(") {
                    return Err(format!("{f} needs ( after it"));
                }
                let mut args = Vec::new();
                if !self.eat(")") {
                    loop {
                        args.push(self.expr(0)?);
                        if self.eat(")") {
                            break;
                        }
                        if !self.eat(",") {
                            return Err(format!("expected , or ) in {f}(…)"));
                        }
                    }
                }
                let (lo, hi) = match func {
                    Func::Upper | Func::Lower | Func::Trim | Func::Len | Func::Num | Func::Abs => (1, 1),
                    Func::Substr | Func::Regex(_) => (2, 3),
                    Func::Round => (1, 2),
                    Func::Replace | Func::If => (3, 3),
                };
                if args.len() < lo || args.len() > hi {
                    return Err(format!("{f} takes {lo}{} arguments", if hi > lo { format!(" to {hi}") } else { String::new() }));
                }
                let func = match (func, args.get(1)) {
                    (Func::Regex(_), Some(Node::Text(p))) => Func::Regex(Some(Regex::new(p).map_err(|e| format!("regex: {e}"))?)),
                    (f, _) => f,
                };
                Ok(Node::Call(func, args))
            }
            Tok::Sym(s) => Err(format!("unexpected {s} at {}", at + 1)),
        }
    }
}

impl Expr {
    /// Compiles `src`; `names` are the header's column names.
    pub fn compile(src: &str, names: &[String]) -> Result<Expr, String> {
        let toks = tokens(src)?;
        if toks.is_empty() {
            return Err("the expression is empty".into());
        }
        let mut p = Parser { toks, pos: 0, names };
        let e = p.expr(0)?;
        if p.pos < p.toks.len() {
            return Err(format!("unexpected text at {}", p.toks[p.pos].1 + 1));
        }
        Ok(Expr(e))
    }

    /// The value for a row of `fields`.
    pub fn eval(&self, fields: &[String]) -> String {
        eval(&self.0, fields)
    }
}

fn num(s: &str) -> Option<f64> {
    parse_number(s)
}

fn show(x: f64) -> String {
    if !x.is_finite() {
        String::new()
    } else if x.fract() == 0.0 && x.abs() < 1e15 {
        format!("{}", x as i64)
    } else {
        let s = format!("{x:.10}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

fn truthy(s: &str) -> bool {
    match num(s) {
        Some(x) => x != 0.0,
        None => !s.is_empty(),
    }
}

fn eval(n: &Node, f: &[String]) -> String {
    match n {
        Node::Col(i) => f.get(*i).cloned().unwrap_or_default(),
        Node::Text(s) => s.clone(),
        Node::Num(x) => show(*x),
        Node::Neg(a) => num(&eval(a, f)).map(|x| show(-x)).unwrap_or_default(),
        Node::Not(a) => (if truthy(&eval(a, f)) { "0" } else { "1" }).into(),
        Node::Bin(op, a, b) => {
            let (x, y) = (eval(a, f), eval(b, f));
            let both = || Some((num(&x)?, num(&y)?));
            let bool_s = |v: bool| (if v { "1" } else { "0" }).to_string();
            match op {
                Op::Or => bool_s(truthy(&x) || truthy(&y)),
                Op::And => bool_s(truthy(&x) && truthy(&y)),
                Op::Cat => x + &y,
                Op::Add => both().map(|(p, q)| show(p + q)).unwrap_or_default(),
                Op::Sub => both().map(|(p, q)| show(p - q)).unwrap_or_default(),
                Op::Mul => both().map(|(p, q)| show(p * q)).unwrap_or_default(),
                Op::Div => both().filter(|&(_, q)| q != 0.0).map(|(p, q)| show(p / q)).unwrap_or_default(),
                _ => {
                    let ord = match both() {
                        Some((p, q)) => p.partial_cmp(&q).unwrap_or(std::cmp::Ordering::Equal),
                        None => x.cmp(&y),
                    };
                    use std::cmp::Ordering::*;
                    bool_s(match op {
                        Op::Eq => ord == Equal,
                        Op::Ne => ord != Equal,
                        Op::Lt => ord == Less,
                        Op::Gt => ord == Greater,
                        Op::Le => ord != Greater,
                        _ => ord != Less,
                    })
                }
            }
        }
        Node::Call(func, args) => {
            let arg = |i: usize| args.get(i).map(|a| eval(a, f)).unwrap_or_default();
            match func {
                Func::Upper => arg(0).to_uppercase(),
                Func::Lower => arg(0).to_lowercase(),
                Func::Trim => arg(0).trim().to_string(),
                Func::Len => arg(0).chars().count().to_string(),
                Func::Substr => {
                    let s = arg(0);
                    let start = num(&arg(1)).unwrap_or(0.0).max(0.0) as usize;
                    let taken: Box<dyn Iterator<Item = char>> = Box::new(s.chars().skip(start));
                    match args.get(2).map(|_| num(&arg(2)).unwrap_or(0.0).max(0.0) as usize) {
                        Some(n) => taken.take(n).collect(),
                        None => taken.collect(),
                    }
                }
                Func::Replace => {
                    let from = arg(1);
                    if from.is_empty() { arg(0) } else { arg(0).replace(&from, &arg(2)) }
                }
                Func::Regex(re) => {
                    let s = arg(0);
                    let compiled;
                    let re = match re {
                        Some(r) => r,
                        None => match Regex::new(&arg(1)) {
                            Ok(r) => {
                                compiled = r;
                                &compiled
                            }
                            Err(_) => return String::new(),
                        },
                    };
                    let g = args.get(2).map(|_| num(&arg(2)).unwrap_or(1.0) as usize).unwrap_or(if re.group_info().group_len(regex_automata::PatternID::ZERO) > 1 { 1 } else { 0 });
                    let mut caps = re.create_captures();
                    re.captures(s.as_bytes(), &mut caps);
                    caps.get_group(g).map(|sp| s[sp.range()].to_string()).unwrap_or_default()
                }
                Func::If => {
                    if truthy(&arg(0)) {
                        arg(1)
                    } else {
                        arg(2)
                    }
                }
                Func::Num => num(&arg(0)).map(show).unwrap_or_default(),
                Func::Abs => num(&arg(0)).map(|x| show(x.abs())).unwrap_or_default(),
                Func::Round => {
                    let digits = args.get(1).map(|_| num(&arg(1)).unwrap_or(0.0)).unwrap_or(0.0).clamp(0.0, 12.0) as i32;
                    let m = 10f64.powi(digits);
                    num(&arg(0)).map(|x| show((x * m).round() / m)).unwrap_or_default()
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(src: &str, row: &[&str]) -> String {
        let names: Vec<String> = ["price", "qty", "status", "url", "ts"].iter().map(|s| s.to_string()).collect();
        let row: Vec<String> = row.iter().map(|s| s.to_string()).collect();
        Expr::compile(src, &names).unwrap_or_else(|e| panic!("{src}: {e}")).eval(&row)
    }

    const ROW: [&str; 5] = ["2.5", "4", "ok", "/a?id=42&x=1", "2024-05-01 10:00:00"];

    #[test]
    fn arithmetic_and_text() {
        assert_eq!(run("{price} * {qty}", &ROW), "10");
        assert_eq!(run("{price} * {qty} + 1 / 4", &ROW), "10.25");
        assert_eq!(run("({price} + 1.5) * -{qty}", &ROW), "-16");
        assert_eq!(run("{status} * 2", &ROW), "");
        assert_eq!(run("{qty} / 0", &ROW), "");
        assert_eq!(run("upper({status}) & \"-\" & {2}", &ROW), "OK-4");
        assert_eq!(run("\"say \"\"hi\"\"\"", &ROW), "say \"hi\"");
        assert_eq!(run("substr({ts}, 0, 10)", &ROW), "2024-05-01");
        assert_eq!(run("substr({ts}, 11)", &ROW), "10:00:00");
        assert_eq!(run("len(trim(\"  ab \"))", &ROW), "2");
        assert_eq!(run("replace({url}, \"&\", \";\")", &ROW), "/a?id=42;x=1");
        assert_eq!(run("round(10 / 3, 2)", &ROW), "3.33");
        assert_eq!(run("abs(-3) & num(\"3,5\")", &ROW), "33.5");
    }

    #[test]
    fn logic_and_regex() {
        assert_eq!(run("if({status} = \"ok\", 1, 0)", &ROW), "1");
        assert_eq!(run("if({qty} > 10 or {status} <> \"ok\", \"big\", \"small\")", &ROW), "small");
        assert_eq!(run("{qty} >= 4 and not {qty} > 4", &ROW), "1");
        assert_eq!(run("\"10\" > \"9\"", &ROW), "1");
        assert_eq!(run("\"b\" > \"a\"", &ROW), "1");
        assert_eq!(run("regex({url}, \"id=([0-9]+)\")", &ROW), "42");
        assert_eq!(run("regex({url}, \"x=[0-9]\")", &ROW), "x=1");
        assert_eq!(run("regex({url}, \"zz(z)\")", &ROW), "");
    }

    #[test]
    fn errors() {
        let names = vec!["a".to_string()];
        for bad in ["{nope} + 1", "1 +", "(1", "frob(1)", "upper(1, 2)", "\"open", "{a} $ 2", "regex({a}, \"(\")", ""] {
            assert!(Expr::compile(bad, &names).is_err(), "{bad} compiled");
        }
        assert_eq!(Expr::compile("{1} & \"x\"", &names).unwrap().eval(&["v".into()]), "vx");
    }
}
