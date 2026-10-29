//! The fields of JSON records — JSON Lines, or the items of a JSON array — in one pass: every key
//! path to depth 3 (`user.address.city`, array items as `tags[]`), with its types, how often it
//! appears, nulls, numbers' minimum, maximum and mean, and some distinct values and examples.

use std::collections::{BTreeMap, HashMap};
use std::io::{self, Write};
use std::ops::Range;

use crate::jobs::{CancelToken, Progress};
use crate::json;
use crate::lines::LineReader;
use crate::text::TextBuf;

/// Levels of nesting looked into.
const DEPTH: usize = 3;
/// Most paths kept.
pub const PATHS_MAX: usize = 10_000;
/// Distinct values kept per path.
const DISTINCT_MAX: usize = 64;
/// Characters of a value kept as a distinct value or an example.
const VALUE_MAX: usize = 60;

/// Kinds of JSON values, in the order of [`FieldStat::types`].
pub const TYPES: [&str; 6] = ["object", "array", "string", "number", "boolean", "null"];

#[derive(Clone, Debug, Default)]
pub struct FieldStat {
    pub path: String,
    /// Values seen (several per record for paths under arrays).
    pub count: u64,
    /// Values of each kind of [`TYPES`].
    pub types: [u64; 6],
    pub numbers: u64,
    pub sum: f64,
    pub min: Option<f64>,
    pub max: Option<f64>,
    /// Up to 64 distinct values (strings, numbers, booleans) and how often each came.
    pub distinct: BTreeMap<String, u64>,
    /// More than 64 distinct values.
    pub many: bool,
    pub examples: Vec<String>,
}

impl FieldStat {
    pub fn mean(&self) -> Option<f64> {
        (self.numbers > 0).then(|| self.sum / self.numbers as f64)
    }

    /// The kinds seen: "string", "number | null"…
    pub fn type_names(&self) -> String {
        TYPES.iter().zip(self.types).filter(|(_, n)| *n > 0).map(|(t, _)| *t).collect::<Vec<_>>().join(" | ")
    }
}

#[derive(Clone, Debug, Default)]
pub struct Fields {
    /// In the order first seen.
    pub stats: Vec<FieldStat>,
    pub records: u64,
    /// Records that were not JSON objects.
    pub skipped: u64,
    /// More than [`PATHS_MAX`] paths.
    pub capped: bool,
}

/// The end of the JSON value starting at `i` in `b`.
fn value_end(b: &[u8], i: usize) -> Option<usize> {
    match *b.get(i)? {
        b'"' => {
            let mut k = i + 1;
            while k < b.len() {
                match b[k] {
                    b'\\' => k += 2,
                    b'"' => return Some(k + 1),
                    _ => k += 1,
                }
            }
            None
        }
        b'{' | b'[' => {
            let (mut depth, mut k, mut s) = (0i64, i, false);
            while k < b.len() {
                let c = b[k];
                if s {
                    match c {
                        b'\\' => k += 1,
                        b'"' => s = false,
                        _ => {}
                    }
                } else {
                    match c {
                        b'"' => s = true,
                        b'{' | b'[' => depth += 1,
                        b'}' | b']' => {
                            depth -= 1;
                            if depth == 0 {
                                return Some(k + 1);
                            }
                        }
                        _ => {}
                    }
                }
                k += 1;
            }
            None
        }
        _ => {
            let k = b[i..].iter().position(|c| matches!(c, b',' | b'}' | b']') || c.is_ascii_whitespace()).map_or(b.len(), |n| i + n);
            (k > i).then_some(k)
        }
    }
}

/// The items of a JSON array held in `b`.
fn array_items(b: &[u8]) -> Vec<Range<usize>> {
    let ws = |b: &[u8], mut i: usize| {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        i
    };
    let mut out = Vec::new();
    let mut i = ws(b, 0);
    if b.get(i) != Some(&b'[') {
        return out;
    }
    i = ws(b, i + 1);
    while i < b.len() && b[i] != b']' {
        let Some(e) = value_end(b, i) else { break };
        out.push(i..e);
        i = ws(b, e);
        if b.get(i) == Some(&b',') {
            i = ws(b, i + 1);
        }
    }
    out
}

struct Collector {
    index: HashMap<String, usize>,
    out: Fields,
}

impl Collector {
    fn stat(&mut self, path: &str) -> Option<&mut FieldStat> {
        if let Some(&i) = self.index.get(path) {
            return self.out.stats.get_mut(i);
        }
        if self.out.stats.len() >= PATHS_MAX {
            self.out.capped = true;
            return None;
        }
        self.index.insert(path.to_string(), self.out.stats.len());
        self.out.stats.push(FieldStat { path: path.to_string(), ..Default::default() });
        self.out.stats.last_mut()
    }

    fn value(&mut self, path: &str, v: &[u8], depth: usize) {
        let kind = match v.first() {
            Some(b'{') => 0,
            Some(b'[') => 1,
            Some(b'"') => 2,
            Some(b't' | b'f') => 4,
            Some(b'n') => 5,
            _ => 3,
        };
        let Some(s) = self.stat(path) else { return };
        s.count += 1;
        s.types[kind] += 1;
        if kind >= 2 && kind != 5 {
            let text = if kind == 2 { json::unescape(v) } else { String::from_utf8_lossy(v).into_owned() };
            let text: String = text.chars().take(VALUE_MAX).collect();
            if kind == 3 {
                if let Ok(x) = text.parse::<f64>() {
                    s.numbers += 1;
                    s.sum += x;
                    s.min = Some(s.min.map_or(x, |m| m.min(x)));
                    s.max = Some(s.max.map_or(x, |m| m.max(x)));
                }
            }
            if s.examples.len() < 3 && !s.examples.contains(&text) {
                s.examples.push(text.clone());
            }
            if let Some(n) = s.distinct.get_mut(&text) {
                *n += 1;
            } else if s.distinct.len() < DISTINCT_MAX {
                s.distinct.insert(text, 1);
            } else {
                s.many = true;
            }
        }
        if depth + 1 >= DEPTH {
            return;
        }
        match kind {
            0 => self.object(v, &format!("{path}."), depth + 1),
            1 => {
                for r in array_items(v) {
                    self.value(&format!("{path}[]"), &v[r], depth + 1);
                }
            }
            _ => {}
        }
    }

    fn object(&mut self, b: &[u8], prefix: &str, depth: usize) {
        let Some(fields) = json::object_fields(b) else { return };
        for (k, r) in fields {
            self.value(&format!("{prefix}{k}"), &b[r], depth);
        }
    }

    fn record(&mut self, b: &[u8]) {
        let t = b.trim_ascii();
        if t.is_empty() {
            return;
        }
        if t.first() == Some(&b'{') && json::object_fields(t).is_some() {
            self.out.records += 1;
            self.object(t, "", 0);
        } else {
            self.out.skipped += 1;
        }
    }
}

/// The fields of the document's records: each line of JSON Lines, or each item of a JSON array
/// at the top. Returns `None` when cancelled.
pub fn field_stats(buf: &TextBuf, cancel: &CancelToken, progress: Option<&Progress>) -> Result<Option<Fields>, String> {
    if buf.codec.unit() != 1 {
        return Err("JSON must be UTF-8".into());
    }
    if let Some(p) = progress {
        p.set_total(buf.len());
    }
    let mut c = Collector { index: HashMap::new(), out: Fields::default() };
    let at = json::root(buf, cancel)?;
    if buf.read(at..at + 1).is_some_and(|b| b[0] == b'[') {
        // The items of the array, each copied out and looked at like a line.
        let mut items: Vec<u8> = Vec::new();
        let mut sink = RecordSink { items: &mut items, c: &mut c, cancel };
        match json::array_to_lines(buf, at, &mut sink, cancel, progress) {
            Ok(Some(_)) => {}
            Ok(None) => return Ok(None),
            Err(e) => return Err(e),
        }
        let rest = std::mem::take(&mut items);
        c.record(&rest);
        return Ok(Some(c.out));
    }
    let mut r = LineReader::new(buf);
    let mut n = 0u32;
    loop {
        n += 1;
        if n.is_multiple_of(4096) {
            if cancel.is_cancelled() {
                return Ok(None);
            }
            if let Some(p) = progress {
                p.set_done(r.taken());
            }
        }
        match r.next() {
            Ok(Some((_, line))) => c.record(line),
            Ok(None) => break,
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(Some(c.out))
}

/// Takes the lines [`json::array_to_lines`] writes and looks at each one.
struct RecordSink<'a> {
    items: &'a mut Vec<u8>,
    c: &'a mut Collector,
    cancel: &'a CancelToken,
}

impl Write for RecordSink<'_> {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        let mut rest = b;
        while let Some(i) = memchr::memchr(b'\n', rest) {
            self.items.extend_from_slice(&rest[..i]);
            let item = std::mem::take(self.items);
            self.c.record(&item);
            rest = &rest[i + 1..];
        }
        self.items.extend_from_slice(rest);
        if self.cancel.is_cancelled() {
            return Err(io::Error::other("cancelled"));
        }
        Ok(b.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn show(x: f64) -> String {
    if x.fract() == 0.0 && x.abs() < 1e15 {
        format!("{}", x as i64)
    } else {
        format!("{x:.4}").trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// The fields as CSV: path, types, count, nulls, min, max, mean, distinct, examples.
pub fn fields_to_csv<W: Write>(f: &Fields, out: &mut W) -> io::Result<()> {
    let q = |s: &str| if s.contains([',', '"', '\n', '\r']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_string() };
    writeln!(out, "path,types,count,nulls,min,max,mean,distinct,examples")?;
    for s in &f.stats {
        let distinct = if s.many { format!("{}+", s.distinct.len()) } else { s.distinct.len().to_string() };
        writeln!(
            out,
            "{},{},{},{},{},{},{},{},{}",
            q(&s.path),
            q(&s.type_names()),
            s.count,
            s.types[5],
            s.min.map(show).unwrap_or_default(),
            s.max.map(show).unwrap_or_default(),
            s.mean().map(show).unwrap_or_default(),
            distinct,
            q(&s.examples.join(" | "))
        )?;
    }
    Ok(())
}

/// A JSON Schema sketch of the records: properties by path, their types, and `required` for the
/// top-level keys every record has.
pub fn fields_to_schema(f: &Fields) -> String {
    fn esc(s: &str) -> String {
        s.replace('\\', "\\\\").replace('"', "\\\"")
    }
    fn types(s: &FieldStat) -> String {
        let names: Vec<String> = TYPES.iter().zip(s.types).filter(|(_, n)| *n > 0).map(|(t, _)| format!("\"{t}\"")).collect();
        if names.len() == 1 {
            names[0].clone()
        } else {
            format!("[{}]", names.join(", "))
        }
    }
    // Children of each object path ("" for the record).
    fn node(f: &Fields, prefix: &str, indent: usize, top: bool) -> String {
        let pad = " ".repeat(indent);
        let kids: Vec<&FieldStat> = f.stats.iter().filter(|s| s.path.strip_prefix(prefix).is_some_and(|rest| !rest.is_empty() && !rest.contains('.') && !rest.contains("[]"))).collect();
        let mut out = String::new();
        out.push_str(&format!("{pad}\"type\": \"object\",\n{pad}\"properties\": {{\n"));
        let body: Vec<String> = kids
            .iter()
            .map(|s| {
                let name = &s.path[prefix.len()..];
                let mut v = format!("{pad}  \"{}\": {{\n{pad}    \"type\": {}", esc(name), types(s));
                if s.types[0] > 0 {
                    v.push_str(",\n");
                    let inner = node(f, &format!("{}.", s.path), indent + 4, false);
                    v.push_str(inner.trim_start_matches(&format!("{pad}    \"type\": \"object\",\n")));
                } else if s.types[1] > 0 && f.stats.iter().any(|x| x.path == format!("{}[]", s.path)) {
                    let item = f.stats.iter().find(|x| x.path == format!("{}[]", s.path)).expect("item");
                    v.push_str(&format!(",\n{pad}    \"items\": {{ \"type\": {} }}", types(item)));
                }
                v.push_str(&format!("\n{pad}  }}"));
                v
            })
            .collect();
        out.push_str(&body.join(",\n"));
        out.push_str(&format!("\n{pad}}}"));
        if top {
            let req: Vec<String> = kids.iter().filter(|s| s.count == f.records && f.records > 0).map(|s| format!("\"{}\"", esc(&s.path))).collect();
            if !req.is_empty() {
                out.push_str(&format!(",\n{pad}\"required\": [{}]", req.join(", ")));
            }
        }
        out
    }
    format!("{{\n  \"$schema\": \"https://json-schema.org/draft/2020-12/schema\",\n{}\n}}\n", node(f, "", 2, true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::Codec;
    use crate::file::{OpenMode, Source};
    use std::sync::Arc;

    fn stats(data: &str) -> Fields {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(data.as_bytes()).unwrap();
        f.flush().unwrap();
        let src = Arc::new(Source::open(f.path(), OpenMode::Auto).unwrap());
        let idx = crate::index::build_blocking(&src, Codec::utf8());
        let mut t = TextBuf::new(src, idx, Codec::utf8());
        t.blocking = true;
        field_stats(&t, &CancelToken::new(), None).unwrap().unwrap()
    }

    const LINES: &str = "{\"id\": 1, \"user\": {\"name\": \"Ann\", \"age\": 30}, \"tags\": [\"a\", \"b\"]}\n{\"id\": 2, \"user\": {\"name\": \"Bob\", \"age\": null}, \"tags\": []}\nnot json\n{\"id\": 3.5, \"extra\": true}\n";

    #[test]
    fn paths_types_and_numbers() {
        let f = stats(LINES);
        assert_eq!((f.records, f.skipped), (3, 1));
        let paths: Vec<&str> = f.stats.iter().map(|s| s.path.as_str()).collect();
        assert_eq!(paths, ["id", "user", "user.name", "user.age", "tags", "tags[]", "extra"]);
        let id = &f.stats[0];
        assert_eq!((id.count, id.min, id.max, id.mean()), (3, Some(1.0), Some(3.5), Some(6.5 / 3.0)));
        assert_eq!(f.stats[3].type_names(), "number | null");
        assert_eq!(f.stats[5].count, 2);
        let mut csv = Vec::new();
        fields_to_csv(&f, &mut csv).unwrap();
        assert!(String::from_utf8(csv).unwrap().contains("\nuser.age,number | null,2,1,30,30,30,1,30\n"));
        let schema = fields_to_schema(&f);
        assert!(schema.contains("\"required\": [\"id\"]") && schema.contains("\"age\": {") && schema.contains("\"items\": { \"type\": \"string\" }"), "{schema}");
    }

    #[test]
    fn items_of_an_array() {
        let f = stats("[ {\"a\": 1}, {\"a\": 2, \"b\": \"x\"} ]");
        assert_eq!(f.records, 2);
        assert_eq!(f.stats.iter().map(|s| (s.path.as_str(), s.count)).collect::<Vec<_>>(), [("a", 2), ("b", 1)]);
    }
}
