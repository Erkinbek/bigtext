//! Comparing two tables by a key column, whatever their row order: each row is paired with the
//! other table's row with the same key (the n-th duplicate with the n-th), and the result is a
//! table of the rows changed (with old → new values and the names of the changed columns),
//! removed and added. Columns pair up by header name, or by position without headers; the
//! columns only one table has are not compared.
//!
//! Any size: both tables are split by key hash into bucket files on disk (32 bytes a row), each
//! bucket is paired in memory, and only the differing rows are read again for the output.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, VecDeque};
use std::io::{self, BufWriter, Write};
use std::fs::File;
use std::ops::Range;

use crate::csv::{self, Dialect, ScanEnd};
use crate::diff::line_hash;
use crate::jobs::{CancelToken, Progress};
use crate::spill::{read_record, Spill};
use crate::text::TextBuf;

const BUCKETS: usize = 64;
/// Most rows of one table in one bucket (rows with the same key share a bucket).
pub const BUCKET_ROWS_MAX: usize = 20_000_000;

#[derive(Clone, Copy, Debug)]
pub struct KeyedSpec {
    /// The key columns (0-based) of the two tables.
    pub key_a: usize,
    pub key_b: usize,
    /// Both tables start with a header row.
    pub header: bool,
    /// List the rows that are the same too.
    pub all: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TableDiffStats {
    pub same: u64,
    pub changed: u64,
    pub removed: u64,
    pub added: u64,
}

fn read_err() -> io::Error {
    io::Error::other("Could not read the document")
}

/// A row in a bucket file: the hash of its key, of its compared values, and where it is.
#[derive(Clone, Copy)]
struct Row {
    key: u64,
    values: u64,
    off: u64,
    len: u64,
}

impl Row {
    fn bytes(self) -> [u8; 32] {
        let mut b = [0u8; 32];
        for (i, x) in [self.key, self.values, self.off, self.len].into_iter().enumerate() {
            b[i * 8..i * 8 + 8].copy_from_slice(&x.to_le_bytes());
        }
        b
    }

    fn from(b: &[u8; 32]) -> Row {
        let x = |i: usize| u64::from_le_bytes(b[i * 8..i * 8 + 8].try_into().unwrap());
        Row { key: x(0), values: x(1), off: x(2), len: x(3) }
    }
}

const SAME: u8 = 0;
const CHANGED: u8 = 1;
const REMOVED: u8 = 2;
const ADDED: u8 = 3;

/// A paired result: what it is, and the records in A and B (offset, length).
#[derive(Clone, Copy)]
struct Pair {
    kind: u8,
    a: (u64, u64),
    b: (u64, u64),
}

impl Pair {
    fn bytes(self) -> [u8; 33] {
        let mut out = [0u8; 33];
        out[0] = self.kind;
        for (i, x) in [self.a.0, self.a.1, self.b.0, self.b.1].into_iter().enumerate() {
            out[1 + i * 8..9 + i * 8].copy_from_slice(&x.to_le_bytes());
        }
        out
    }

    fn from(b: &[u8; 33]) -> Pair {
        let x = |i: usize| u64::from_le_bytes(b[1 + i * 8..9 + i * 8].try_into().unwrap());
        Pair { kind: b[0], a: (x(0), x(1)), b: (x(2), x(3)) }
    }
}

/// The values of the record at `content`.
fn values_at(buf: &TextBuf, d: Dialect, content: Range<u64>, fields: &mut Vec<Range<usize>>) -> io::Result<Vec<String>> {
    let raw = buf.read(content).ok_or_else(read_err)?;
    csv::split_fields(&buf.codec, d, &raw, fields);
    Ok(fields.iter().map(|f| csv::field_value(&buf.codec, d, &raw[f.clone()])).collect())
}

/// The first record's values (the header, or the first row).
fn first_values(buf: &TextBuf, d: Dialect, cancel: &CancelToken) -> io::Result<Vec<String>> {
    let mut first = None;
    let end = csv::for_each_record(buf, d, cancel, None, |r| {
        first = Some(r.content.clone());
        false
    });
    if end == ScanEnd::Failed {
        return Err(read_err());
    }
    match first {
        Some(c) => values_at(buf, d, c, &mut Vec::new()),
        None => Ok(Vec::new()),
    }
}

/// A's columns other than the key, each with B's column of the same name (or place).
struct Columns {
    names: Vec<String>,
    pairs: Vec<(usize, Option<usize>)>,
}

fn columns(a: &[String], b: &[String], spec: KeyedSpec) -> Columns {
    let norm = |s: &str| s.trim().trim_start_matches('\u{feff}').to_lowercase();
    let a_cols: Vec<usize> = (0..a.len()).filter(|&i| i != spec.key_a).collect();
    let b_cols: Vec<usize> = (0..b.len()).filter(|&j| j != spec.key_b).collect();
    let pairs = if spec.header {
        a_cols.iter().map(|&i| (i, b_cols.iter().copied().find(|&j| norm(&b[j]) == norm(&a[i])))).collect()
    } else {
        a_cols.iter().enumerate().map(|(k, &i)| (i, b_cols.get(k).copied())).collect()
    };
    let names = a_cols.iter().map(|&i| if spec.header { a[i].trim_start_matches('\u{feff}').to_string() } else { format!("column {}", i + 1) }).collect();
    Columns { names, pairs }
}

/// Writes each data row of a table into the bucket of its key.
#[allow(clippy::too_many_arguments)]
fn bucket_rows(buf: &TextBuf, d: Dialect, key: usize, cols: &[usize], header: bool, w: &mut [BufWriter<File>], cancel: &CancelToken, progress: Option<(&Progress, u64)>) -> io::Result<Option<()>> {
    let mut err = None;
    let mut first = header;
    let mut text = String::new();
    let mut n = 0u64;
    let end = csv::for_each_record(buf, d, cancel, None, |r| {
        if std::mem::take(&mut first) {
            return true;
        }
        let res = (|| {
            let raw = buf.read(r.content.clone()).ok_or_else(read_err)?;
            let value = |i: usize| -> String {
                r.fields.get(i).map(|f| csv::field_value(&buf.codec, d, &raw[(f.start - r.content.start) as usize..(f.end - r.content.start) as usize])).unwrap_or_default()
            };
            let k = line_hash(value(key).trim().as_bytes());
            text.clear();
            for &c in cols {
                text.push_str(&value(c));
                text.push('\u{1f}');
            }
            let row = Row { key: k, values: line_hash(text.as_bytes()), off: r.content.start, len: r.content.end - r.content.start };
            w[(k >> 58) as usize % BUCKETS].write_all(&row.bytes())
        })();
        n += 1;
        if n.is_multiple_of(4096) {
            if let Some((p, base)) = progress {
                p.set_done(base + r.next);
            }
        }
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
        ScanEnd::Failed => Err(read_err()),
        _ => Ok(Some(())),
    }
}

fn read_rows(spill: &Spill, name: &str, mut f: impl FnMut(Row) -> io::Result<()>) -> io::Result<()> {
    let mut r = spill.open(name)?;
    let mut b = [0u8; 32];
    while read_record(&mut r, &mut b)? {
        f(Row::from(&b))?;
    }
    Ok(())
}

/// Writes the comparison of `a` and `b` by their key columns as UTF-8 CSV: status, key, the
/// changed columns, then A's columns (changed values as "old → new"). `None` when cancelled.
pub fn keyed_diff_to(a: (&TextBuf, Dialect), b: (&TextBuf, Dialect), spec: KeyedSpec, out: &mut dyn Write, cancel: &CancelToken, progress: Option<&Progress>) -> io::Result<Option<TableDiffStats>> {
    let ((ab, ad), (bb, bd)) = (a, b);
    let (a_first, b_first) = (first_values(ab, ad, cancel)?, first_values(bb, bd, cancel)?);
    let cols = columns(&a_first, &b_first, spec);
    // Only the columns both tables have are compared.
    let a_cmp: Vec<usize> = cols.pairs.iter().filter(|p| p.1.is_some()).map(|p| p.0).collect();
    let b_cmp: Vec<usize> = cols.pairs.iter().filter_map(|p| p.1).collect();
    if let Some(p) = progress {
        p.set_total(ab.len() + bb.len());
    }
    let spill = Spill::new("table-diff")?;
    for (side, buf, d, key, cmp, base) in [("a", ab, ad, spec.key_a, &a_cmp, 0), ("b", bb, bd, spec.key_b, &b_cmp, ab.len())] {
        let mut w: Vec<BufWriter<File>> = (0..BUCKETS).map(|i| spill.create(&format!("{side}-{i:02}"))).collect::<io::Result<_>>()?;
        if bucket_rows(buf, d, key, cmp, spec.header, &mut w, cancel, progress.map(|p| (p, base)))?.is_none() {
            return Ok(None);
        }
        for x in &mut w {
            x.flush()?;
        }
    }
    // Pair each bucket's rows; the results in A's order (B's for the added rows).
    let mut stats = TableDiffStats::default();
    for i in 0..BUCKETS {
        if cancel.is_cancelled() {
            return Ok(None);
        }
        let mut rows: Vec<Row> = Vec::new();
        read_rows(&spill, &format!("a-{i:02}"), |r| {
            if rows.len() >= BUCKET_ROWS_MAX {
                return Err(io::Error::other(format!("more than {BUCKET_ROWS_MAX} rows share keys of one kind (many empty or equal keys?)")));
            }
            rows.push(r);
            Ok(())
        })?;
        let mut by_key: HashMap<u64, VecDeque<usize>> = HashMap::new();
        for (k, r) in rows.iter().enumerate() {
            by_key.entry(r.key).or_default().push_back(k);
        }
        let mut paired = vec![false; rows.len()];
        let (mut in_a, mut added) = (Vec::new(), Vec::new());
        read_rows(&spill, &format!("b-{i:02}"), |r| {
            match by_key.get_mut(&r.key).and_then(|q| q.pop_front()) {
                Some(k) => {
                    paired[k] = true;
                    let x = rows[k];
                    if x.values == r.values {
                        stats.same += 1;
                        if spec.all {
                            in_a.push(Pair { kind: SAME, a: (x.off, x.len), b: (r.off, r.len) });
                        }
                    } else {
                        in_a.push(Pair { kind: CHANGED, a: (x.off, x.len), b: (r.off, r.len) });
                    }
                }
                None => added.push(Pair { kind: ADDED, a: (0, 0), b: (r.off, r.len) }),
            }
            Ok(())
        })?;
        in_a.extend(rows.iter().zip(&paired).filter(|(_, p)| !**p).map(|(x, _)| Pair { kind: REMOVED, a: (x.off, x.len), b: (0, 0) }));
        in_a.sort_unstable_by_key(|p| p.a.0);
        added.sort_unstable_by_key(|p| p.b.0);
        for (name, v) in [(format!("x-{i:02}"), in_a), (format!("y-{i:02}"), added)] {
            let mut w = spill.create(&name)?;
            for p in v {
                w.write_all(&p.bytes())?;
            }
            w.flush()?;
        }
    }
    // The output: header, then the rows merged back into order.
    let q = |v: &str| csv::quote_field(v, Dialect::new(b','));
    let key_name = if spec.header { a_first.get(spec.key_a).map(|s| s.trim_start_matches('\u{feff}').to_string()).unwrap_or_default() } else { "key".into() };
    let head: Vec<String> = ["status".to_string(), key_name, "changed".into()].into_iter().chain(cols.names.iter().cloned()).collect();
    writeln!(out, "{}", head.iter().map(|h| q(h)).collect::<Vec<_>>().join(","))?;
    let mut fields = Vec::new();
    let mut write_row = |out: &mut dyn Write, p: Pair, stats: &mut TableDiffStats| -> io::Result<()> {
        let va = if p.kind == ADDED { Vec::new() } else { values_at(ab, ad, p.a.0..p.a.0 + p.a.1, &mut fields)? };
        let vb = if p.kind == REMOVED || p.kind == SAME { Vec::new() } else { values_at(bb, bd, p.b.0..p.b.0 + p.b.1, &mut fields)? };
        let get = |v: &[String], i: usize| v.get(i).cloned().unwrap_or_default();
        let mut row = |status: &str, key: String, changed: &str, cells: Vec<String>| -> io::Result<()> {
            let all: Vec<String> = [status.to_string(), key, changed.to_string()].into_iter().chain(cells).collect();
            writeln!(out, "{}", all.iter().map(|c| q(c)).collect::<Vec<_>>().join(","))
        };
        let a_cells = || cols.pairs.iter().map(|&(i, _)| get(&va, i)).collect::<Vec<_>>();
        let b_cells = || cols.pairs.iter().map(|&(_, j)| j.map(|j| get(&vb, j)).unwrap_or_default()).collect::<Vec<_>>();
        match p.kind {
            SAME => row("same", get(&va, spec.key_a), "", a_cells()),
            REMOVED => {
                stats.removed += 1;
                row("removed", get(&va, spec.key_a), "", a_cells())
            }
            ADDED => {
                stats.added += 1;
                row("added", get(&vb, spec.key_b), "", b_cells())
            }
            _ => {
                let (ka, kb) = (get(&va, spec.key_a), get(&vb, spec.key_b));
                if ka.trim() != kb.trim() {
                    // Two keys with the same hash: not a pair after all.
                    stats.removed += 1;
                    stats.added += 1;
                    row("removed", ka, "", a_cells())?;
                    return row("added", kb, "", b_cells());
                }
                let mut changed = Vec::new();
                let cells = cols
                    .pairs
                    .iter()
                    .zip(&cols.names)
                    .map(|(&(i, j), name)| {
                        let x = get(&va, i);
                        match j.map(|j| get(&vb, j)) {
                            Some(y) if y != x => {
                                changed.push(name.as_str());
                                format!("{x} → {y}")
                            }
                            _ => x,
                        }
                    })
                    .collect();
                if changed.is_empty() {
                    stats.same += 1;
                    return if spec.all { row("same", ka, "", cells) } else { Ok(()) };
                }
                stats.changed += 1;
                row("changed", ka, &changed.join("; "), cells)
            }
        }
    };
    for (prefix, by_b) in [("x", false), ("y", true)] {
        let mut readers: Vec<_> = (0..BUCKETS).map(|i| spill.open(&format!("{prefix}-{i:02}"))).collect::<io::Result<_>>()?;
        let next = |r: &mut io::BufReader<File>| -> io::Result<Option<Pair>> {
            let mut b = [0u8; 33];
            Ok(read_record(r, &mut b)?.then(|| Pair::from(&b)))
        };
        let mut heap = BinaryHeap::new();
        let order = |p: &Pair| if by_b { p.b.0 } else { p.a.0 };
        let mut pending: Vec<Option<Pair>> = Vec::with_capacity(BUCKETS);
        for (i, r) in readers.iter_mut().enumerate() {
            let p = next(r)?;
            if let Some(p) = &p {
                heap.push(Reverse((order(p), i)));
            }
            pending.push(p);
        }
        let mut n = 0u64;
        while let Some(Reverse((_, i))) = heap.pop() {
            let p = pending[i].take().expect("a row");
            write_row(out, p, &mut stats)?;
            if let Some(p) = next(&mut readers[i])? {
                heap.push(Reverse((order(&p), i)));
                pending[i] = Some(p);
            }
            n += 1;
            if n.is_multiple_of(4096) && cancel.is_cancelled() {
                return Ok(None);
            }
        }
    }
    Ok(Some(stats))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::{OpenMode, Source};
    use crate::index::LineIndex;
    use std::sync::Arc;

    fn table(data: &[u8]) -> (tempfile::NamedTempFile, TextBuf) {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(data).unwrap();
        f.flush().unwrap();
        let s = Arc::new(Source::open(f.path(), OpenMode::Auto).unwrap());
        let det = crate::encoding::detect(data, true);
        (f, TextBuf::new(s.clone(), LineIndex::new(s.len()), det.codec))
    }

    fn run(a: &str, b: &[u8], spec: KeyedSpec) -> (String, TableDiffStats) {
        let (_fa, ta) = table(a.as_bytes());
        let (_fb, tb) = table(b);
        let d = Dialect::new(b',');
        let mut out = Vec::new();
        let s = keyed_diff_to((&ta, d), (&tb, d), spec, &mut out, &CancelToken::new(), None).unwrap().unwrap();
        (String::from_utf8(out).unwrap(), s)
    }

    const BY_ID: KeyedSpec = KeyedSpec { key_a: 0, key_b: 0, header: true, all: false };

    #[test]
    fn reordered_rows_and_columns() {
        let a = "id,name,qty\n1,apple,5\n2,banana,7\n3,cherry,9\n";
        let b = b"id,qty,name\n3,9,cherry\n1,6,apple\n4,1,date\n";
        let (out, s) = run(a, b, BY_ID);
        assert_eq!(out, "status,id,changed,name,qty\nchanged,1,qty,apple,5 → 6\nremoved,2,,banana,7\nadded,4,,date,1\n");
        assert_eq!(s, TableDiffStats { same: 1, changed: 1, removed: 1, added: 1 });
        let (out, _) = run(a, b, KeyedSpec { all: true, ..BY_ID });
        assert!(out.contains("removed,2,,banana,7\nsame,3,,cherry,9\nadded"), "{out}");
        // The same table in another order has no differences.
        let (out, s) = run(a, b"qty,id,name\n9,3,cherry\n7,2,banana\n5,1,apple\n", KeyedSpec { key_b: 1, ..BY_ID });
        assert_eq!((out.lines().count(), s.same), (1, 3));
    }

    #[test]
    fn duplicates_quotes_line_breaks_and_encodings() {
        // The n-th row with a key pairs with the n-th.
        let (out, _) = run("k,v\nx,1\nx,2\n", b"k,v\nx,1\nx,3\n", BY_ID);
        assert_eq!(out, "status,k,changed,v\nchanged,x,v,2 → 3\n");
        // A quoted key with a comma, CRLF against LF.
        let (out, _) = run("id,v\r\n\"a,b\",1\r\nc,2\r\n", b"id,v\n\"a,b\",2\nc,2\n", BY_ID);
        assert_eq!(out, "status,id,changed,v\nchanged,\"a,b\",v,1 → 2\n");
        // UTF-16 against UTF-8.
        let utf16: Vec<u8> = [0xFF, 0xFE].into_iter().chain("id,город\n1,Ташкент\n2,Самарканд\n".encode_utf16().flat_map(|u| u.to_le_bytes())).collect();
        let (out, s) = run("id,город\n1,Ташкент\n2,Бухара\n", &utf16, BY_ID);
        assert_eq!((out.as_str(), s.same), ("status,id,changed,город\nchanged,2,город,Бухара → Самарканд\n", 1));
        // Without headers columns pair by place.
        let (out, _) = run("1,a\n2,b\n", b"2,B\n1,a\n", KeyedSpec { header: false, ..BY_ID });
        assert_eq!(out, "status,key,changed,column 2\nchanged,2,column 2,b → B\n");
    }

    #[test]
    fn many_rows_across_buckets_in_order() {
        let a: String = std::iter::once("id,v\n".to_string()).chain((0..20_000).map(|i| format!("{i},{}\n", i * 2))).collect();
        let b: String = std::iter::once("id,v\n".to_string()).chain((0..20_000).rev().filter(|i| i % 1000 != 5).map(|i| format!("{i},{}\n", if i % 997 == 0 { i * 3 } else { i * 2 }))).collect();
        let (out, s) = run(&a, b.as_bytes(), BY_ID);
        assert_eq!((s.removed, s.added), (20, 0));
        assert_eq!(s.changed, (1..20_000).filter(|i| i % 997 == 0 && i % 1000 != 5).count() as u64);
        // Changed and removed rows come in A's order.
        let keys: Vec<u64> = out.lines().skip(1).map(|l| l.split(',').nth(1).unwrap().parse().unwrap()).collect();
        assert!(keys.windows(2).all(|w| w[0] < w[1]), "out of order");
    }
}
