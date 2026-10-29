//! Joining two tables by a key column: the right ("lookup") table is read into memory, keyed by
//! its column, and the left table streams past it, each row followed by the other columns of
//! every right row with the same key (a left or an inner join). The result is UTF-8 CSV.

use std::collections::HashMap;
use std::io::{self, Write};

use crate::csv::{self, Dialect, ScanEnd};
use crate::jobs::{CancelToken, Progress};
use crate::text::TextBuf;

/// Most bytes of the right table's values held in memory.
pub const JOIN_MAX: u64 = 256 << 20;
/// Most rows of the right table.
pub const JOIN_ROWS_MAX: usize = 5_000_000;

#[derive(Clone, Copy, Debug)]
pub struct JoinSpec {
    /// Key columns (0-based) of the left and the right table.
    pub left_key: usize,
    pub right_key: usize,
    /// Leave out left rows without a match (otherwise they get empty fields).
    pub inner: bool,
    /// Both tables start with a header row.
    pub header: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JoinStats {
    pub left_rows: u64,
    pub right_rows: u64,
    /// Left rows with at least one match.
    pub matched: u64,
    pub out_rows: u64,
}

fn quote(v: &str) -> String {
    if v.contains([',', '"', '\n', '\r']) || v.trim() != v {
        format!("\"{}\"", v.replace('"', "\"\""))
    } else {
        v.to_string()
    }
}

/// The values of a record's fields.
fn values(buf: &TextBuf, d: Dialect, r: &csv::Record) -> Option<Vec<String>> {
    r.fields.iter().map(|f| buf.read(f.clone()).map(|b| csv::field_value(&buf.codec, d, &b))).collect()
}

/// Writes the joined table to `out`. Returns `None` when cancelled; an error when the right
/// table does not fit in memory.
pub fn join_to<W: Write>((left, ld): (&TextBuf, Dialect), (right, rd): (&TextBuf, Dialect), spec: JoinSpec, out: &mut W, cancel: &CancelToken, progress: Option<&Progress>) -> io::Result<Option<JoinStats>> {
    let read_err = || io::Error::other("Could not read the document");
    let mut stats = JoinStats::default();
    // Pass 1: the right table's other columns, rendered once, by key.
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    let mut right_head: Option<Vec<String>> = None;
    let mut right_cols = 0usize;
    let (mut bytes, mut failed, mut too_big) = (0u64, false, false);
    let end = csv::for_each_record(right, rd, cancel, None, |r| {
        let Some(v) = values(right, rd, r) else {
            failed = true;
            return false;
        };
        if spec.header && right_head.is_none() {
            right_cols = v.len();
            right_head = Some(v);
            return true;
        }
        if r.content.is_empty() && v.len() == 1 {
            return true;
        }
        stats.right_rows += 1;
        right_cols = right_cols.max(v.len());
        let key = v.get(spec.right_key).map(|k| k.trim().to_string()).unwrap_or_default();
        let suffix: String = v.iter().enumerate().filter(|&(i, _)| i != spec.right_key).map(|(_, x)| format!(",{}", quote(x))).collect();
        bytes += (key.len() + suffix.len() + 32) as u64;
        if bytes > JOIN_MAX || stats.right_rows as usize > JOIN_ROWS_MAX {
            too_big = true;
            return false;
        }
        map.entry(key).or_default().push(suffix);
        true
    });
    if too_big {
        return Err(io::Error::other(format!("The lookup table is too large to hold in memory (more than {} MB of values or {} rows); join the other way round.", JOIN_MAX >> 20, JOIN_ROWS_MAX)));
    }
    match end {
        ScanEnd::Cancelled => return Ok(None),
        _ if failed => return Err(read_err()),
        ScanEnd::Failed => return Err(read_err()),
        _ => {}
    }
    let empty = ",".repeat(right_cols.saturating_sub(1));
    // Pass 2: the left table, streamed.
    let mut left_head = spec.header;
    let mut err: Option<io::Error> = None;
    let end = csv::for_each_record(left, ld, cancel, progress, |r| {
        let res = (|| -> io::Result<()> {
            let v = values(left, ld, r).ok_or_else(read_err)?;
            if r.content.is_empty() && v.len() == 1 {
                return Ok(());
            }
            let row = v.iter().map(|x| quote(x)).collect::<Vec<_>>().join(",");
            if left_head {
                left_head = false;
                let names: String = right_head.iter().flatten().enumerate().filter(|&(i, _)| i != spec.right_key).map(|(_, x)| format!(",{}", quote(x))).collect();
                return writeln!(out, "{row}{names}");
            }
            stats.left_rows += 1;
            let key = v.get(spec.left_key).map(|k| k.trim()).unwrap_or("");
            match map.get(key) {
                Some(list) => {
                    stats.matched += 1;
                    for suffix in list {
                        writeln!(out, "{row}{suffix}")?;
                        stats.out_rows += 1;
                    }
                }
                None if !spec.inner => {
                    writeln!(out, "{row}{empty}")?;
                    stats.out_rows += 1;
                }
                None => {}
            }
            Ok(())
        })();
        match res {
            Ok(()) => true,
            Err(e) => {
                err = Some(e);
                false
            }
        }
    });
    if let Some(e) = err {
        return Err(e);
    }
    match end {
        ScanEnd::Cancelled => Ok(None),
        ScanEnd::Failed | ScanEnd::Stopped => Err(read_err()),
        ScanEnd::Done => Ok(Some(stats)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::{Codec, Encoding, NlByte};
    use crate::file::{OpenMode, Source};
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

    fn join(l: &TextBuf, r: &TextBuf, ld: Dialect, rd: Dialect, spec: JoinSpec) -> (String, JoinStats) {
        let mut out = Vec::new();
        let st = join_to((l, ld), (r, rd), spec, &mut out, &CancelToken::new(), None).unwrap().unwrap();
        (String::from_utf8(out).unwrap(), st)
    }

    #[test]
    fn left_and_inner() {
        let (_a, orders) = buf(b"order,user,total\r\n1,u1,10\r\n2,u2,5\r\n3,u9,7\r\n4,u1,\"1,5\"\r\n", Codec::utf8());
        let (_b, users) = buf(b"id;name;city\nu1;Ann;Paris\nu2;\"Bob; Jr\";Rome\nu1;Ann2;Oslo\n", Codec::utf8());
        let spec = JoinSpec { left_key: 1, right_key: 0, inner: false, header: true };
        let (out, st) = join(&orders, &users, Dialect::new(b','), Dialect::new(b';'), spec);
        assert_eq!(out, "order,user,total,name,city\n1,u1,10,Ann,Paris\n1,u1,10,Ann2,Oslo\n2,u2,5,Bob; Jr,Rome\n3,u9,7,,\n4,u1,\"1,5\",Ann,Paris\n4,u1,\"1,5\",Ann2,Oslo\n");
        assert_eq!(st, JoinStats { left_rows: 4, right_rows: 3, matched: 3, out_rows: 6 });
        let (out, st) = join(&orders, &users, Dialect::new(b','), Dialect::new(b';'), JoinSpec { inner: true, ..spec });
        assert_eq!(out.lines().count(), 6);
        assert_eq!(st.out_rows, 5);
    }

    #[test]
    fn other_encodings_and_no_header() {
        let codec = Codec::new(Encoding::Utf16Le, NlByte::Lf, 2);
        let mut data = vec![0xFF, 0xFE];
        data.extend(codec.encode("k,ä\nb,β\n"));
        let (_a, right) = buf(&data, codec);
        let (_b, left) = buf(b"a,1\nb,2\n", Codec::utf8());
        let spec = JoinSpec { left_key: 0, right_key: 0, inner: false, header: false };
        let (out, _) = join(&left, &right, Dialect::new(b','), Dialect::new(b','), spec);
        assert_eq!(out, "a,1,\nb,2,β\n");
    }
}
