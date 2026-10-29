//! Message patterns: the lines of a log grouped by template — the line with its leading
//! timestamp dropped and numbers, hex ids, UUIDs, IP addresses and paths replaced by
//! placeholders — so the noisiest messages and the rarest ones stand out.

use std::collections::HashMap;
use std::io::{self, Write};

use crate::jobs::{CancelToken, Progress, Throttle};
use crate::lines::LineReader;
use crate::loglevel::Level;
use crate::text::TextBuf;

/// Most different templates kept; the lines of others are counted together.
pub const TEMPLATES_MAX: usize = 200_000;
/// Bytes of a template kept.
const TEMPLATE_MAX: usize = 300;
/// Bytes of a line looked at.
const LINE_MAX: usize = 4096;

/// One template and its lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Template {
    pub text: String,
    pub count: u64,
    pub level: Option<Level>,
    /// Starts of its first and last lines.
    pub first: u64,
    pub last: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Patterns {
    /// Most lines first.
    pub templates: Vec<Template>,
    pub lines: u64,
    /// Lines of templates beyond [`TEMPLATES_MAX`].
    pub other: u64,
}

fn is_hex(c: u8) -> bool {
    c.is_ascii_hexdigit()
}

fn run(b: &[u8], i: usize, f: impl Fn(u8) -> bool) -> usize {
    b[i..].iter().position(|&c| !f(c)).map_or(b.len(), |n| i + n)
}

/// A UUID at `i`: 8-4-4-4-12 hex digits.
fn uuid_at(b: &[u8], i: usize) -> Option<usize> {
    let mut k = i;
    for (n, len) in [8, 4, 4, 4, 12].into_iter().enumerate() {
        if n > 0 {
            if b.get(k) != Some(&b'-') {
                return None;
            }
            k += 1;
        }
        let e = run(b, k, is_hex);
        if e - k != len {
            return None;
        }
        k = e;
    }
    Some(k)
}

/// An IPv4 address at `i` (with an optional :port).
fn ip_at(b: &[u8], i: usize) -> Option<usize> {
    let mut k = i;
    for n in 0..4 {
        if n > 0 {
            if b.get(k) != Some(&b'.') {
                return None;
            }
            k += 1;
        }
        let e = run(b, k, |c| c.is_ascii_digit());
        if e == k || e - k > 3 {
            return None;
        }
        k = e;
    }
    if b.get(k) == Some(&b':') && b.get(k + 1).is_some_and(u8::is_ascii_digit) {
        k = run(b, k + 1, |c| c.is_ascii_digit());
    }
    Some(k)
}

fn level_word(w: &[u8], before: Option<u8>) -> Option<Level> {
    let upper = match w {
        b"ERROR" | b"ERR" | b"FATAL" | b"CRITICAL" | b"SEVERE" => Some(Level::Error),
        b"WARN" | b"WARNING" => Some(Level::Warn),
        b"INFO" => Some(Level::Info),
        b"DEBUG" | b"TRACE" => Some(Level::Debug),
        _ => None,
    };
    if upper.is_some() {
        return upper;
    }
    // level=error, "level":"error", [error].
    if !matches!(before, Some(b'=') | Some(b'"') | Some(b'[')) {
        return None;
    }
    match w.to_ascii_lowercase().as_slice() {
        b"error" | b"err" | b"fatal" | b"critical" => Some(Level::Error),
        b"warn" | b"warning" => Some(Level::Warn),
        b"info" => Some(Level::Info),
        b"debug" | b"trace" => Some(Level::Debug),
        _ => None,
    }
}

/// Length of a leading timestamp: a run of digits and date punctuation with a ':' in it.
fn time_prefix(b: &[u8]) -> usize {
    let e = run(b, 0, |c| c.is_ascii_digit() || matches!(c, b'-' | b':' | b'.' | b'/' | b' ' | b'T' | b'Z' | b'+' | b',' | b'[' | b']'));
    if e >= 8 && b[..e].contains(&b':') {
        e
    } else {
        0
    }
}

/// The template of a line (ASCII-compatible bytes, without its line break) and its log level.
pub fn template(line: &[u8]) -> (String, Option<Level>) {
    let b = &line[..line.len().min(LINE_MAX)];
    let mut out = String::with_capacity(b.len().min(TEMPLATE_MAX) + 8);
    let mut level = None;
    let mut i = time_prefix(b);
    let mut space = false;
    while i < b.len() && out.len() < TEMPLATE_MAX {
        let c = b[i];
        if c == b' ' || c == b'\t' || c == b'\r' {
            space = !out.is_empty();
            i += 1;
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        let word_start = i == 0 || !b[i - 1].is_ascii_alphanumeric();
        if word_start && is_hex(c) {
            if let Some(e) = uuid_at(b, i) {
                out.push_str("<uuid>");
                i = e;
                continue;
            }
        }
        if c.is_ascii_digit() {
            if let Some(e) = ip_at(b, i).filter(|_| word_start) {
                out.push_str("<ip>");
                i = e;
                continue;
            }
            if c == b'0' && matches!(b.get(i + 1), Some(b'x') | Some(b'X')) && b.get(i + 2).is_some_and(|&c| is_hex(c)) {
                out.push_str("<hex>");
                i = run(b, i + 2, is_hex);
                continue;
            }
        }
        if word_start && is_hex(c) {
            let e = run(b, i, is_hex);
            let tail_ok = e == b.len() || !b[e].is_ascii_alphanumeric();
            if e - i >= 8 && tail_ok && b[i..e].iter().any(u8::is_ascii_digit) {
                out.push_str("<hex>");
                i = e;
                continue;
            }
        }
        if c.is_ascii_digit() {
            let mut e = run(b, i, |c| c.is_ascii_digit());
            if b.get(e) == Some(&b'.') && b.get(e + 1).is_some_and(u8::is_ascii_digit) {
                e = run(b, e + 1, |c| c.is_ascii_digit());
            }
            out.push_str("<n>");
            i = e;
            continue;
        }
        if c == b'/' && word_start {
            let e = run(b, i, |c| c.is_ascii_alphanumeric() || matches!(c, b'/' | b'_' | b'.' | b'-' | b'~' | b'%'));
            if b[i..e].iter().filter(|&&c| c == b'/').count() >= 2 {
                out.push_str("<path>");
                i = e;
                continue;
            }
        }
        if c.is_ascii_alphabetic() {
            let e = run(b, i, |c| c.is_ascii_alphabetic());
            if level.is_none() {
                level = level_word(&b[i..e], i.checked_sub(1).map(|k| b[k]));
            }
            out.push_str(std::str::from_utf8(&b[i..e]).unwrap_or(""));
            i = e;
            continue;
        }
        // Any other character, as text.
        let n = crate::encoding::utf8_char(&b[i..]).1.max(1);
        out.push_str(&String::from_utf8_lossy(&b[i..(i + n).min(b.len())]));
        i += n;
    }
    let mut cut = i < b.len() || line.len() > LINE_MAX;
    if out.len() > TEMPLATE_MAX {
        let mut k = TEMPLATE_MAX;
        while !out.is_char_boundary(k) {
            k -= 1;
        }
        out.truncate(k);
        // Not in the middle of a placeholder.
        if let Some(lt) = out.rfind('<').filter(|&p| !out[p..].contains('>')) {
            out.truncate(lt);
        }
        cut = true;
    }
    if cut {
        out.push('…');
    }
    (out, level)
}

/// A regular expression for the lines of `template`.
pub fn regex_for(template: &str) -> String {
    let (body, cut) = match template.strip_suffix('…') {
        Some(b) => (b, true),
        None => (template, false),
    };
    let mut out = String::new();
    let mut rest = body;
    while !rest.is_empty() {
        let holes = [
            ("<n>", r"[0-9]+(?:\.[0-9]+)?"),
            ("<ip>", r"[0-9]{1,3}(?:\.[0-9]{1,3}){3}(?::[0-9]+)?"),
            ("<hex>", r"(?:0[xX])?[0-9a-fA-F]+"),
            ("<uuid>", r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}"),
            ("<path>", r"/[A-Za-z0-9/_.\-~%]*"),
        ];
        if let Some((h, re)) = holes.iter().find(|(h, _)| rest.starts_with(h)) {
            out.push_str(re);
            rest = &rest[h.len()..];
        } else if let Some(r) = rest.strip_prefix(' ') {
            out.push_str(r"[ \t]+");
            rest = r;
        } else {
            let c = rest.chars().next().expect("not empty");
            out.push_str(&regex_syntax::escape(&c.to_string()));
            rest = &rest[c.len_utf8()..];
        }
    }
    if !cut {
        out.push_str(r"[ \t\r]*$");
    }
    out
}

/// Groups the lines of the document (or, with `only`, those starting at these offsets, in order)
/// by template in one pass. `partial` gets the most frequent templates so far now and then.
/// Returns `None` when cancelled.
pub fn build(buf: &TextBuf, only: Option<&[u64]>, cancel: &CancelToken, progress: Option<&Progress>, mut partial: impl FnMut(&Patterns)) -> io::Result<Option<Patterns>> {
    if buf.codec.unit() != 1 {
        return Err(io::Error::other("Message patterns need a UTF-8 file"));
    }
    let mut map: HashMap<String, Template> = HashMap::new();
    let mut out = Patterns::default();
    let mut throttle = Throttle::new(std::time::Duration::from_millis(500));
    let add = |out: &mut Patterns, map: &mut HashMap<String, Template>, off: u64, line: &[u8]| {
        out.lines += 1;
        let line = line.strip_suffix(b"\n").unwrap_or(line);
        let (text, level) = template(line);
        if let Some(t) = map.get_mut(&text) {
            t.count += 1;
            t.last = off;
        } else if map.len() < TEMPLATES_MAX {
            map.insert(text.clone(), Template { text, count: 1, level, first: off, last: off });
        } else {
            out.other += 1;
        }
    };
    let snapshot = |map: &HashMap<String, Template>, out: &Patterns| {
        let mut p = Patterns { templates: map.values().cloned().collect(), lines: out.lines, other: out.other };
        sort(&mut p.templates, false);
        p.templates.truncate(1000);
        p
    };
    let bom = buf.codec.bom_len as u64;
    let mut n = 0u64;
    match only {
        Some(starts) => {
            if let Some(p) = progress {
                p.set_total(starts.len() as u64);
            }
            for (k, &ls) in starts.iter().enumerate() {
                let Some(le) = crate::filter::line_end_at(buf, ls) else { return Err(io::Error::other("Could not read the document")) };
                let from = ls.max(bom);
                let Some(b) = buf.read(from..le.min(from + LINE_MAX as u64 + 1)) else { return Err(io::Error::other("Could not read the document")) };
                add(&mut out, &mut map, ls, &b);
                if k % 4096 == 0 {
                    if cancel.is_cancelled() {
                        return Ok(None);
                    }
                    if let Some(p) = progress {
                        p.set_done(k as u64);
                    }
                    if throttle.ready() {
                        partial(&snapshot(&map, &out));
                    }
                }
            }
        }
        None => {
            if let Some(p) = progress {
                p.set_total(buf.len());
            }
            let mut lines = LineReader::new(buf);
            while let Some((off, line)) = lines.next()? {
                let line = if off == 0 { &line[(bom as usize).min(line.len())..] } else { line };
                add(&mut out, &mut map, off, line);
                n += 1;
                if n.is_multiple_of(4096) {
                    if cancel.is_cancelled() {
                        return Ok(None);
                    }
                    if let Some(p) = progress {
                        p.set_done(lines.taken());
                    }
                    if throttle.ready() {
                        partial(&snapshot(&map, &out));
                    }
                }
            }
        }
    }
    out.templates = map.into_values().collect();
    sort(&mut out.templates, false);
    Ok(Some(out))
}

/// Most lines first, or (`rare`) fewest first; then in file order.
pub fn sort(t: &mut [Template], rare: bool) {
    if rare {
        t.sort_by(|a, b| a.count.cmp(&b.count).then(a.first.cmp(&b.first)));
    } else {
        t.sort_by(|a, b| b.count.cmp(&a.count).then(a.first.cmp(&b.first)));
    }
}

/// The templates as CSV: count, level, template, and the first and last line's offsets.
pub fn to_csv<W: Write>(p: &[Template], out: &mut W) -> io::Result<()> {
    let q = |s: &str| if s.contains([',', '"', '\n', '\r']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_string() };
    writeln!(out, "lines,level,template,first offset,last offset")?;
    for t in p {
        writeln!(out, "{},{},{},{},{}", t.count, t.level.map(|l| l.name()).unwrap_or(""), q(&t.text), t.first, t.last)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::Codec;
    use crate::search::{Matcher, Query};

    fn t(s: &str) -> String {
        template(s.as_bytes()).0
    }

    #[test]
    fn templates() {
        assert_eq!(t("2024-05-01 10:00:00.123 ERROR db timeout after 30.5 s (attempt 3)"), "ERROR db timeout after <n> s (attempt <n>)");
        assert_eq!(t("[2024-05-01 10:00:01] INFO GET /api/v2/users/42 from 10.0.0.7:5513"), "INFO GET <path> from <ip>");
        assert_eq!(t("WARN  job 123e4567-e89b-12d3-a456-426614174000 slow, id=0x1F3a"), "WARN job <uuid> slow, id=<hex>");
        assert_eq!(t("session 4bf92f3577b34da6 closed\tuser42"), "session <hex> closed user<n>");
        assert_eq!(t("deadbeef stays a word"), "deadbeef stays a word");
        assert_eq!(template(b"ts=1 level=error msg=x").1, Some(Level::Error));
        assert_eq!(template(b"10:00:00 WARNING disk").1, Some(Level::Warn));
        assert_eq!(template(b"no error here").1, None);
        let long = format!("x {}", "y".repeat(400));
        assert!(t(&long).ends_with('…'));
    }

    #[test]
    fn every_line_matches_its_template() {
        let lines = [
            "2024-05-01 10:00:00.123 ERROR db timeout after 30.5 s (attempt 3)",
            "[2024-05-01 10:00:01] INFO GET /api/v2/users/42 from 10.0.0.7:5513",
            "WARN  job 123e4567-e89b-12d3-a456-426614174000 slow, id=0x1F3a",
            "session 4bf92f3577b34da6 closed\tuser42",
            "price is $5.00 (a+b)*c? [x]",
            "plain text line",
        ];
        for l in lines {
            let re = regex_for(&t(l));
            let m = Matcher::new(&Query { pattern: re.clone(), regex: true, case_sensitive: true, ..Default::default() }, Codec::utf8()).unwrap();
            let mut hit = false;
            m.find_in(l.as_bytes(), 0, 0, l.len(), l.len(), &mut None, &mut |_, _| {
                hit = true;
                false
            });
            assert!(hit, "{l:?} does not match {re:?}");
        }
        // A longer line of another template does not.
        let re = regex_for(&t("connection reset"));
        let m = Matcher::new(&Query { pattern: re, regex: true, case_sensitive: true, ..Default::default() }, Codec::utf8()).unwrap();
        let l = "connection reset by peer";
        let mut hit = false;
        m.find_in(l.as_bytes(), 0, 0, l.len(), l.len(), &mut None, &mut |_, _| {
            hit = true;
            false
        });
        assert!(!hit);
    }

    #[test]
    fn groups_a_log() {
        use crate::file::{OpenMode, Source};
        use std::sync::Arc;
        let mut data = String::new();
        for i in 0..5 {
            data.push_str(&format!("2024-05-01 10:00:0{i} INFO request {i} done in {} ms\n", i * 7));
        }
        data.push_str("2024-05-01 10:00:09 ERROR disk /var/data/x full\n");
        data.push_str("2024-05-01 10:00:10 INFO request 9 done in 3 ms\n");
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(data.as_bytes()).unwrap();
        f.flush().unwrap();
        let src = Arc::new(Source::open(f.path(), OpenMode::Auto).unwrap());
        let idx = crate::index::build_blocking(&src, Codec::utf8());
        let mut buf = TextBuf::new(src, idx, Codec::utf8());
        buf.blocking = true;
        let p = build(&buf, None, &CancelToken::new(), None, |_| {}).unwrap().unwrap();
        assert_eq!(p.lines, 7);
        assert_eq!(p.templates.len(), 2);
        assert_eq!((p.templates[0].text.as_str(), p.templates[0].count, p.templates[0].level), ("INFO request <n> done in <n> ms", 6, Some(Level::Info)));
        assert_eq!((p.templates[1].text.as_str(), p.templates[1].level), ("ERROR disk <path> full", Some(Level::Error)));
        let mut rare = p.templates.clone();
        sort(&mut rare, true);
        assert_eq!(rare[0].count, 1);
        // Only some lines.
        let only = build(&buf, Some(&[p.templates[1].first]), &CancelToken::new(), None, |_| {}).unwrap().unwrap();
        assert_eq!((only.lines, only.templates.len()), (1, 1));
        let mut csv = Vec::new();
        to_csv(&p.templates, &mut csv).unwrap();
        assert!(String::from_utf8(csv).unwrap().starts_with("lines,level,template,first offset,last offset\n6,INFO,INFO request <n> done in <n> ms,0,"));
    }
}
