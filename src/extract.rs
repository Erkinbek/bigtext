//! The matched texts themselves, not their lines: every match (or one capture group of it) in
//! order, or counted by value, most frequent first — `grep -o | sort | uniq -c` in one pass.

use std::collections::HashMap;
use std::io::{self, Write};
use std::ops::Range;

use crate::jobs::{CancelToken, Progress};
use crate::search::{self, Matcher, SearchEnd};
use crate::text::TextBuf;

/// Most different values counted; the matches of others are counted together.
pub const VALUES_MAX: usize = 100_000;
/// Characters of a value kept as its key.
const KEY_MAX: usize = 200;

/// Calls `on_value` with the text of every match in `range` — capture group `group` of it (0:
/// the whole match) — skipping matches where the group did not take part. Stops when it
/// returns false.
pub fn extract(buf: &TextBuf, m: &Matcher, group: usize, range: Range<u64>, cancel: &CancelToken, progress: Option<&Progress>, mut on_value: impl FnMut(&str) -> bool) -> SearchEnd {
    let mut failed = false;
    let end = search::search_forward_ctx(buf, m, range, cancel, progress, |s, e, hay, base| {
        let text = if group > 0 {
            match search::group_text(buf, m, s, e, group) {
                Some(t) => t,
                None => return true,
            }
        } else if s >= base && e - base <= hay.len() as u64 {
            buf.codec.decode_lossy(&hay[(s - base) as usize..(e - base) as usize])
        } else {
            match buf.read(s..e) {
                Some(b) => buf.codec.decode_lossy(&b),
                None => {
                    failed = true;
                    return false;
                }
            }
        };
        on_value(&text)
    });
    if failed {
        SearchEnd::Unavailable
    } else {
        end
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    /// Most frequent first, then in order of first appearance.
    pub values: Vec<(String, u64)>,
    pub matches: u64,
    /// Matches of values beyond [`VALUES_MAX`] different ones.
    pub other: u64,
}

/// Counts the matched texts (or a capture group) by value. Returns `None` when cancelled.
pub fn count(buf: &TextBuf, m: &Matcher, group: usize, range: Range<u64>, cancel: &CancelToken, progress: Option<&Progress>) -> io::Result<Option<Tally>> {
    let mut map: HashMap<String, (u64, u64)> = HashMap::new();
    let mut t = Tally::default();
    let end = extract(buf, m, group, range, cancel, progress, |v| {
        let key: String = if v.chars().count() > KEY_MAX { v.chars().take(KEY_MAX).collect() } else { v.to_string() };
        let order = t.matches;
        t.matches += 1;
        if let Some(c) = map.get_mut(&key) {
            c.0 += 1;
        } else if map.len() < VALUES_MAX {
            map.insert(key, (1, order));
        } else {
            t.other += 1;
        }
        true
    });
    match end {
        SearchEnd::Completed => {}
        SearchEnd::Cancelled => return Ok(None),
        _ => return Err(io::Error::other("Could not read the document")),
    }
    let mut v: Vec<(String, (u64, u64))> = map.into_iter().collect();
    v.sort_by(|a, b| b.1 .0.cmp(&a.1 .0).then(a.1 .1.cmp(&b.1 .1)));
    t.values = v.into_iter().map(|(k, (n, _))| (k, n)).collect();
    Ok(Some(t))
}

/// Writes every matched text (or group) on a line of its own, as UTF-8. Returns how many, or
/// `None` when cancelled.
pub fn write_values_to<W: Write>(buf: &TextBuf, m: &Matcher, group: usize, range: Range<u64>, out: &mut W, cancel: &CancelToken, progress: Option<&Progress>) -> io::Result<Option<u64>> {
    let mut n = 0u64;
    let mut err = None;
    let end = extract(buf, m, group, range, cancel, progress, |v| match writeln!(out, "{v}") {
        Ok(()) => {
            n += 1;
            true
        }
        Err(e) => {
            err = Some(e);
            false
        }
    });
    if let Some(e) = err {
        return Err(e);
    }
    match end {
        SearchEnd::Completed => Ok(Some(n)),
        SearchEnd::Cancelled => Ok(None),
        _ => Err(io::Error::other("Could not read the document")),
    }
}

/// The tally as CSV: value, matches, percent.
pub fn tally_to_csv<W: Write>(t: &Tally, out: &mut W) -> io::Result<()> {
    let q = |s: &str| if s.contains([',', '"', '\n', '\r']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_string() };
    writeln!(out, "value,matches,percent")?;
    for (v, n) in &t.values {
        let pct = if t.matches > 0 { *n as f64 * 100.0 / t.matches as f64 } else { 0.0 };
        writeln!(out, "{},{n},{pct:.2}", q(v))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::{Codec, Encoding, NlByte};
    use crate::file::{OpenMode, Source};
    use crate::search::Query;
    use std::sync::Arc;

    fn buf(data: &[u8], codec: Codec) -> (tempfile::NamedTempFile, TextBuf) {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(data).unwrap();
        f.flush().unwrap();
        let src = Arc::new(Source::open(f.path(), OpenMode::Auto).unwrap());
        let idx = crate::index::build_blocking(&src, codec);
        let mut t = TextBuf::new(src, idx, codec);
        t.blocking = true;
        (f, t)
    }

    fn re(p: &str, codec: Codec) -> Matcher {
        Matcher::new(&Query { pattern: p.into(), regex: true, case_sensitive: true, ..Default::default() }, codec).unwrap()
    }

    const LOG: &[u8] = b"GET /a 200 user=ann\nGET /b 404 user=bob\nPOST /a 200 user=ann\nGET /c 200\n";

    #[test]
    fn values_and_groups() {
        let (_f, t) = buf(LOG, Codec::utf8());
        let m = re(r"user=(?P<user>\w+)", t.codec);
        let mut all = Vec::new();
        extract(&t, &m, 0, 0..t.len(), &CancelToken::new(), None, |v| {
            all.push(v.to_string());
            true
        });
        assert_eq!(all, ["user=ann", "user=bob", "user=ann"]);
        let c = count(&t, &m, 1, 0..t.len(), &CancelToken::new(), None).unwrap().unwrap();
        assert_eq!(c.values, [("ann".to_string(), 2), ("bob".to_string(), 1)]);
        assert_eq!(c.matches, 3);
        assert_eq!(search::group_names(&m), ["user"]);
        // A group that did not take part is skipped.
        let m2 = re(r" (2\d\d)|(4\d\d)", t.codec);
        let c2 = count(&t, &m2, 1, 0..t.len(), &CancelToken::new(), None).unwrap().unwrap();
        assert_eq!(c2.values, [("200".to_string(), 3)]);
        let mut out = Vec::new();
        assert_eq!(write_values_to(&t, &m, 1, 0..t.len(), &mut out, &CancelToken::new(), None).unwrap(), Some(3));
        assert_eq!(out, b"ann\nbob\nann\n");
        let mut csv = Vec::new();
        tally_to_csv(&c, &mut csv).unwrap();
        assert_eq!(String::from_utf8(csv).unwrap(), "value,matches,percent\nann,2,66.67\nbob,1,33.33\n");
    }

    #[test]
    fn other_encodings() {
        let codec = Codec::new(Encoding::Utf16Le, NlByte::Lf, 2);
        let mut data = vec![0xFF, 0xFE];
        data.extend(codec.encode("id=ä1 id=b2\nid=ä1\n"));
        let (_f, t) = buf(&data, codec);
        let m = re(r"id=(\w+)", codec);
        let c = count(&t, &m, 1, 0..t.len(), &CancelToken::new(), None).unwrap().unwrap();
        assert_eq!(c.values, [("ä1".to_string(), 2), ("b2".to_string(), 1)]);
        let lit = Matcher::new(&Query { pattern: "ä1".into(), case_sensitive: true, ..Default::default() }, codec).unwrap();
        let c = count(&t, &lit, 0, 0..t.len(), &CancelToken::new(), None).unwrap().unwrap();
        assert_eq!(c.values, [("ä1".to_string(), 2)]);
    }
}
