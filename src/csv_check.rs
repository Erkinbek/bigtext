//! Checking a CSV / TSV file's records in one pass: a field count other than the table's, a
//! stray or unclosed quote, control characters, and text in columns that should hold numbers.

use std::io;

use crate::csv::{self, Dialect, ScanEnd};
use crate::jobs::{CancelToken, Progress};
use crate::text::TextBuf;

/// Records looked at to find the usual field count when the file has no header.
const SAMPLE: usize = 1000;
/// Records longer than this are reported as such, not read field by field.
const RECORD_MAX: u64 = 64 * 1024;
/// Most problems listed.
pub const PROBLEMS_MAX: usize = 1_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Fields { got: usize, want: usize },
    /// A quote inside a field that is not quoted, or text after a field's closing quote.
    StrayQuote { col: usize },
    /// A quoted field that runs to the end of the file.
    Unclosed { col: usize },
    Control { col: usize },
    NotNumber { col: usize },
    /// Longer than [`RECORD_MAX`]: not looked into.
    Huge,
}

/// A record with a problem.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Problem {
    pub start: u64,
    /// Where the next record starts.
    pub next: u64,
    pub kind: Kind,
}

#[derive(Clone, Debug, Default)]
pub struct Checked {
    pub problems: Vec<Problem>,
    pub records: u64,
    /// The field count records should have.
    pub fields: usize,
    pub capped: bool,
}

/// The usual field count: the header's, or the most common one of the first records.
fn expected_fields(buf: &TextBuf, d: Dialect, header: bool, cancel: &CancelToken) -> Option<usize> {
    let mut counts: Vec<usize> = Vec::new();
    let end = csv::for_each_record(buf, d, cancel, None, |r| {
        counts.push(r.fields.len());
        !header && counts.len() < SAMPLE
    });
    if end == ScanEnd::Failed {
        return None;
    }
    if header {
        return counts.first().copied();
    }
    let mut tally = std::collections::HashMap::new();
    for c in &counts {
        *tally.entry(*c).or_insert(0usize) += 1;
    }
    tally.into_iter().max_by_key(|&(c, n)| (n, c)).map(|(c, _)| c)
}

/// Checks every record; `numeric` lists the (0-based) columns whose non-empty values must be
/// numbers. `on_problem` sees each problem as it is found. Returns `None` when cancelled.
pub fn check(
    buf: &TextBuf,
    d: Dialect,
    header: bool,
    numeric: &[usize],
    cancel: &CancelToken,
    progress: Option<&Progress>,
    mut on_problem: impl FnMut(&Problem),
) -> io::Result<Option<Checked>> {
    let codec = buf.codec;
    let mut out = Checked { fields: expected_fields(buf, d, header, cancel).unwrap_or(0), ..Default::default() };
    let len = buf.len();
    let q = d.quote;
    let unit = codec.unit() as u64;
    let mut failed = false;
    let end = csv::for_each_record(buf, d, cancel, progress, |r| {
        out.records += 1;
        let mut found: Option<Kind> = None;
        if r.content.end - r.content.start > RECORD_MAX {
            found = Some(Kind::Huge);
        } else if r.fields.len() != out.fields && out.fields > 0 {
            found = Some(Kind::Fields { got: r.fields.len(), want: out.fields });
        } else {
            for (col, f) in r.fields.iter().enumerate() {
                let Some(b) = buf.read(f.start..f.end) else {
                    failed = true;
                    return false;
                };
                let text = codec.decode_lossy(&b);
                let t = text.as_bytes();
                let quoted = t.first() == Some(&q);
                if quoted {
                    // "…" with doubled quotes inside; at the end of the file it may be unclosed.
                    let inner = &t[1..];
                    let closes = !inner.is_empty() && inner.last() == Some(&q) && inner[..inner.len() - 1].split(|&c| c == q).count() % 2 == 1;
                    if !closes {
                        found = Some(if r.next >= len && f.end >= len { Kind::Unclosed { col } } else { Kind::StrayQuote { col } });
                        break;
                    }
                } else if t.contains(&q) {
                    found = Some(Kind::StrayQuote { col });
                    break;
                }
                if t.iter().any(|&c| c < 0x20 && !matches!(c, b'\t' | b'\n' | b'\r')) {
                    found = Some(Kind::Control { col });
                    break;
                }
                if numeric.contains(&col) && !(header && out.records == 1) {
                    let v = if quoted { text[1..text.len() - 1].replace("\"\"", "\"") } else { text.clone() };
                    if !v.trim().is_empty() && csv::parse_number(&v).is_none() {
                        found = Some(Kind::NotNumber { col });
                        break;
                    }
                }
            }
        }
        if let Some(kind) = found {
            let p = Problem { start: r.content.start, next: r.next.max(r.content.end + unit).min(len), kind };
            on_problem(&p);
            if out.problems.len() < PROBLEMS_MAX {
                out.problems.push(p);
            } else {
                out.capped = true;
            }
        }
        true
    });
    match end {
        ScanEnd::Cancelled => Ok(None),
        _ if failed => Err(io::Error::other("Could not read the document")),
        ScanEnd::Failed | ScanEnd::Stopped => Err(io::Error::other("Could not read the document")),
        ScanEnd::Done => Ok(Some(out)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::Codec;
    use crate::file::{OpenMode, Source};
    use std::io::Write;
    use std::sync::Arc;

    fn checked(data: &[u8], header: bool, numeric: &[usize]) -> (Checked, Vec<(String, Kind)>) {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(data).unwrap();
        f.flush().unwrap();
        let src = Arc::new(Source::open(f.path(), OpenMode::Auto).unwrap());
        let idx = crate::index::build_blocking(&src, Codec::utf8());
        let mut t = TextBuf::new(src, idx, Codec::utf8());
        t.blocking = true;
        let c = check(&t, Dialect::new(b','), header, numeric, &CancelToken::new(), None, |_| {}).unwrap().unwrap();
        let rows = c.problems.iter().map(|p| (String::from_utf8_lossy(&data[p.start as usize..p.next as usize]).trim_end().to_string(), p.kind)).collect();
        (c, rows)
    }

    #[test]
    fn finds_bad_records() {
        let data = b"id,name,qty\n1,ann,3\n2,bob\n3,\"cy, jr\",4\n4,d\"x,5\n5,e,six\n6,\"multi\nline\",7\n7,f\x01,8\n";
        let (c, rows) = checked(data, true, &[2]);
        assert_eq!(c.fields, 3);
        assert_eq!(c.records, 8);
        assert_eq!(rows, [
            ("2,bob".to_string(), Kind::Fields { got: 2, want: 3 }),
            ("4,d\"x,5".to_string(), Kind::StrayQuote { col: 1 }),
            ("5,e,six".to_string(), Kind::NotNumber { col: 2 }),
            ("7,f\x01,8".to_string(), Kind::Control { col: 1 }),
        ]);
    }

    #[test]
    fn unclosed_quote_and_no_header() {
        let (c, rows) = checked(b"a,b\nc,d\ne,f,g\nh,\"open\n", false, &[]);
        assert_eq!(c.fields, 2);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].1, Kind::Fields { got: 3, want: 2 });
        assert_eq!(rows[1].1, Kind::Unclosed { col: 1 });
        // Text after a closing quote.
        let (_, rows) = checked(b"a,b\n\"x\"y,z\n", false, &[]);
        assert_eq!(rows, [("\"x\"y,z".to_string(), Kind::StrayQuote { col: 0 })]);
        // A clean file.
        assert!(checked(b"x,y\n1,2\n\"a\"\"b\",3\n", true, &[1]).1.is_empty());
    }
}
