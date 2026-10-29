//! Comparing two JSON documents by structure: each is flattened into `$.path = value` lines (keys
//! in sorted order, array items by index), the flat forms are compared with the line diff, and
//! the differing lines become changes — a value changed, added or removed — so key order and
//! formatting do not count.

use std::io::{self, BufWriter, Write};
use std::path::Path;

use crate::diff::{self, DiffEnd};
use crate::file::{OpenMode, Source};
use crate::jobs::{CancelToken, Progress};
use crate::json::{self, Kind, Step};
use crate::text::TextBuf;

/// Children of one object or array read before it is written as one value instead.
const LEVEL_MAX: usize = 100_000;
/// Most changes listed.
pub const CHANGES_MAX: usize = 100_000;

/// Writes the document as `$.path = value` lines, one per leaf value (and per empty object or
/// array). Returns the number of lines, or `None` when cancelled.
pub fn flatten<W: Write>(buf: &TextBuf, out: &mut W, cancel: &CancelToken, progress: Option<&Progress>) -> Result<Option<u64>, String> {
    if buf.codec.unit() != 1 {
        return Err("JSON must be UTF-8".into());
    }
    if let Some(p) = progress {
        p.set_total(buf.len());
    }
    let root = json::root(buf, cancel)?;
    let first = buf.read(root..root + 1).ok_or("could not read the file")?[0];
    let kind = match first {
        b'{' => Kind::Object,
        b'[' => Kind::Array,
        b'"' => Kind::String,
        _ => Kind::Other,
    };
    // (path steps, start, end, kind); a leaf's end is known, a container's is found by listing.
    let mut stack: Vec<(Vec<Step>, u64, u64, Kind)> = vec![(Vec::new(), root, buf.len(), kind)];
    let mut lines = 0u64;
    let werr = |e: io::Error| e.to_string();
    while let Some((path, start, end, kind)) = stack.pop() {
        if cancel.is_cancelled() {
            return Ok(None);
        }
        if let Some(p) = progress {
            p.set_done(start);
        }
        let leaf = |out: &mut W, raw: &[u8]| -> Result<(), String> {
            let minified: Vec<u8> = minify(raw);
            out.write_all(json::format_path(&path).as_bytes()).map_err(werr)?;
            out.write_all(b" = ").map_err(werr)?;
            out.write_all(&minified).map_err(werr)?;
            out.write_all(b"\n").map_err(werr)
        };
        if !matches!(kind, Kind::Object | Kind::Array) {
            let raw = buf.read(start..end).ok_or("could not read the file")?;
            leaf(out, raw.trim_ascii())?;
            lines += 1;
            continue;
        }
        let mut items = Vec::new();
        let mut resume = None;
        loop {
            let page = json::children(buf, start, resume, 10_000, cancel)?;
            items.extend(page.items);
            if page.resume.is_none() || items.len() > LEVEL_MAX {
                resume = page.resume;
                break;
            }
            resume = page.resume;
        }
        if resume.is_some() {
            // Too many to sort: the whole value on one line.
            let mut r = Vec::new();
            let last = items.last().map(|c| c.end).unwrap_or(start);
            r.extend_from_slice(&buf.read(start..last).ok_or("could not read the file")?);
            leaf(out, &r)?;
            lines += 1;
            continue;
        }
        if items.is_empty() {
            leaf(out, if kind == Kind::Object { b"{}" } else { b"[]" })?;
            lines += 1;
            continue;
        }
        if kind == Kind::Object {
            items.sort_by(|a, b| a.key.cmp(&b.key));
        }
        for c in items.into_iter().rev() {
            let mut p = path.clone();
            p.push(match &c.key {
                Some(k) => Step::Key(k.clone()),
                None => Step::Index(c.index),
            });
            stack.push((p, c.start, c.end, c.kind));
        }
    }
    Ok(Some(lines))
}

/// A JSON value without the white space between its parts.
fn minify(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    let (mut s, mut esc) = (false, false);
    for &c in raw {
        if s {
            out.push(c);
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                s = false;
            }
        } else if !c.is_ascii_whitespace() {
            if c == b'"' {
                s = true;
            }
            out.push(c);
        }
    }
    out
}

/// A difference at a path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub path: String,
    /// The value in A (none: added in B) and in B (none: removed from A).
    pub old: Option<String>,
    pub new: Option<String>,
}

fn split_line(l: &str) -> (String, String) {
    match l.split_once(" = ") {
        Some((p, v)) => (p.to_string(), v.to_string()),
        None => (l.to_string(), String::new()),
    }
}

fn read_lines(src: &Source, off: u64, count: u64) -> io::Result<Vec<String>> {
    let mut out = Vec::new();
    let mut at = off;
    while (out.len() as u64) < count && at < src.len() {
        let b = src.read(at..(at + (1 << 20)).min(src.len()))?;
        let mut consumed = 0;
        for line in b.split_inclusive(|&c| c == b'\n') {
            if !line.ends_with(b"\n") && at + (consumed + line.len()) as u64 != src.len() {
                break;
            }
            out.push(String::from_utf8_lossy(line.strip_suffix(b"\n").unwrap_or(line)).into_owned());
            consumed += line.len();
            if out.len() as u64 >= count {
                break;
            }
        }
        if consumed == 0 {
            break;
        }
        at += consumed as u64;
    }
    Ok(out)
}

/// The changes from the flat form of A to that of B (files written by [`flatten`]).
pub fn changes(a: &Path, b: &Path, cancel: &CancelToken, progress: Option<&Progress>, mut on_change: impl FnMut(Change) -> bool) -> Result<Option<bool>, String> {
    let sa = Source::open(a, OpenMode::Auto).map_err(|e| e.to_string())?;
    let sb = Source::open(b, OpenMode::Auto).map_err(|e| e.to_string())?;
    let mut err = None;
    let mut any = false;
    let end = diff::diff(&sa, &sb, cancel, progress, |h| {
        let res = (|| -> io::Result<bool> {
            let old = read_lines(&sa, h.a_off, h.a_count)?;
            let new = read_lines(&sb, h.b_off, h.b_count)?;
            let mut newer: Vec<(String, String)> = new.iter().map(|l| split_line(l)).collect();
            for l in &old {
                let (p, v) = split_line(l);
                let change = match newer.iter().position(|x| x.0 == p) {
                    // A hunk can hold lines that did not change.
                    Some(i) if newer[i].1 == v => {
                        newer.remove(i);
                        continue;
                    }
                    Some(i) => {
                        let (_, nv) = newer.remove(i);
                        Change { path: p, old: Some(v), new: Some(nv) }
                    }
                    None => Change { path: p, old: Some(v), new: None },
                };
                any = true;
                if !on_change(change) {
                    return Ok(false);
                }
            }
            for (p, v) in newer {
                any = true;
                if !on_change(Change { path: p, old: None, new: Some(v) }) {
                    return Ok(false);
                }
            }
            Ok(true)
        })();
        match res {
            Ok(go) => go,
            Err(e) => {
                err = Some(e.to_string());
                false
            }
        }
    });
    if let Some(e) = err {
        return Err(e);
    }
    match end {
        DiffEnd::Cancelled => Ok(None),
        DiffEnd::Failed => Err("could not read the flattened files".into()),
        _ => Ok(Some(any)),
    }
}

/// Flattens `buf` into a new temporary file.
pub fn flatten_to_temp(buf: &TextBuf, name: &str, cancel: &CancelToken, progress: Option<&Progress>) -> Result<Option<std::path::PathBuf>, String> {
    let path = crate::decompress::temp_file(name);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let file = std::fs::File::options().write(true).create_new(true).open(&path).map_err(|e| e.to_string())?;
    let mut w = BufWriter::with_capacity(8 << 20, file);
    let r = flatten(buf, &mut w, cancel, progress).and_then(|n| w.flush().map(|_| n).map_err(|e| e.to_string()));
    drop(w);
    match r {
        Ok(Some(_)) => Ok(Some(path)),
        Ok(None) => {
            crate::decompress::remove_temp(&path);
            Ok(None)
        }
        Err(e) => {
            crate::decompress::remove_temp(&path);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::Codec;
    use std::sync::Arc;

    fn buf(data: &str) -> (tempfile::NamedTempFile, TextBuf) {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(data.as_bytes()).unwrap();
        f.flush().unwrap();
        let src = Arc::new(Source::open(f.path(), OpenMode::Auto).unwrap());
        let idx = crate::index::build_blocking(&src, Codec::utf8());
        let mut t = TextBuf::new(src, idx, Codec::utf8());
        t.blocking = true;
        (f, t)
    }

    fn flat(data: &str) -> String {
        let (_f, t) = buf(data);
        let mut out = Vec::new();
        flatten(&t, &mut out, &CancelToken::new(), None).unwrap().unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn flat_forms() {
        assert_eq!(flat("{\"b\": [1, {\"x\": \"a b\"}], \"a\": {}, \"c d\": null}"), "$.a = {}\n$.b[0] = 1\n$.b[1].x = \"a b\"\n$[\"c d\"] = null\n");
        // Formatting and key order do not change the flat form.
        assert_eq!(flat("{\n  \"x\": 1,\n  \"y\": [ 2, 3 ]\n}"), flat("{\"y\":[2,3],\"x\":1}"));
        assert_eq!(flat("[]"), "$ = []\n");
        assert_eq!(flat("\"s\""), "$ = \"s\"\n");
    }

    #[test]
    fn changes_between_two_documents() {
        let (_a, a) = buf("{\"items\": [{\"id\": 1, \"price\": 10}, {\"id\": 2, \"price\": 5}], \"name\": \"x\", \"gone\": true}");
        let (_b, b) = buf("{\n  \"name\": \"x\",\n  \"items\": [\n    {\"price\": 12, \"id\": 1},\n    {\"id\": 2, \"price\": 5}\n  ],\n  \"new\": [1]\n}");
        let c = CancelToken::new();
        let pa = flatten_to_temp(&a, "a.flat", &c, None).unwrap().unwrap();
        let pb = flatten_to_temp(&b, "b.flat", &c, None).unwrap().unwrap();
        let mut got = Vec::new();
        let any = changes(&pa, &pb, &c, None, |ch| {
            got.push(ch);
            true
        })
        .unwrap()
        .unwrap();
        crate::decompress::remove_temp(&pa);
        crate::decompress::remove_temp(&pb);
        assert!(any);
        let s = |x: &str| Some(x.to_string());
        assert_eq!(got, [
            Change { path: "$.gone".into(), old: s("true"), new: None },
            Change { path: "$.items[0].price".into(), old: s("10"), new: s("12") },
            Change { path: "$.new[0]".into(), old: None, new: s("1") },
        ]);
    }
}
