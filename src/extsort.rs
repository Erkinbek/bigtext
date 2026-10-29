//! Sorting the lines of a file of any size: runs of up to [`RUN_BYTES`] are sorted in memory
//! and written to temporary files, then merged (a k-way merge, in two levels past
//! [`MERGE_WAYS`] runs). Equal lines can be dropped (unique) or counted (most frequent first).

use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};

use crate::encoding::Codec;
use crate::jobs::{CancelToken, Progress};
use crate::lines::LineReader;
use crate::spill::{read_record, Spill};
use crate::text::TextBuf;

/// Bytes of lines sorted in memory at a time.
pub const RUN_BYTES: usize = 64 << 20;
/// Runs merged at once.
const MERGE_WAYS: usize = 256;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SortMode {
    #[default]
    Sort,
    /// Sorted, with each line once.
    Unique,
    /// Each line once with how many times it occurs (`count<TAB>line`), most frequent first.
    Count,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SortSpec {
    pub reverse: bool,
    /// By the number at the start of the line (lines without one go last).
    pub numeric: bool,
    pub ignore_case: bool,
    pub mode: SortMode,
    /// Counts as UTF-8 CSV (`count,line` with a header) rather than `count<TAB>line`.
    pub counts_csv: bool,
}

fn leading_number(s: &str) -> Option<f64> {
    let t = s.trim_start();
    let end = t.char_indices().find(|&(i, c)| !(c.is_ascii_digit() || c == '.' || (i == 0 && (c == '-' || c == '+')))).map(|x| x.0).unwrap_or(t.len());
    t[..end].parse().ok()
}

/// How two lines (raw bytes in `codec`) compare under `spec`.
fn compare(codec: &Codec, spec: &SortSpec, a: &[u8], b: &[u8]) -> Ordering {
    let rev = |o: Ordering| if spec.reverse { o.reverse() } else { o };
    if codec.enc == crate::encoding::Encoding::Utf8 && !spec.numeric && !spec.ignore_case {
        return rev(a.cmp(b));
    }
    let (x, y) = (codec.decode_lossy(a), codec.decode_lossy(b));
    let by_text = || if spec.ignore_case { x.to_lowercase().cmp(&y.to_lowercase()) } else { x.cmp(&y) };
    if !spec.numeric {
        return rev(by_text());
    }
    // Lines without a number go last, reversed or not.
    match (leading_number(&x), leading_number(&y)) {
        (Some(p), Some(q)) => rev(p.total_cmp(&q).then_with(by_text)),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => rev(by_text()),
    }
}

fn write_record(w: &mut impl Write, b: &[u8]) -> io::Result<()> {
    w.write_all(&(b.len() as u32).to_le_bytes())?;
    w.write_all(b)
}

fn next_record(r: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut n = [0u8; 4];
    if !read_record(r, &mut n)? {
        return Ok(None);
    }
    let mut b = vec![0u8; u32::from_le_bytes(n) as usize];
    r.read_exact(&mut b)?;
    Ok(Some(b))
}

/// Sorts records (stable) with `cmp`, through temporary runs when they do not fit in
/// `run_bytes`; `emit` gets them in order. `None` when cancelled.
fn external_sort(
    mut records: impl FnMut() -> io::Result<Option<Vec<u8>>>,
    cmp: &dyn Fn(&[u8], &[u8]) -> Ordering,
    run_bytes: usize,
    cancel: &CancelToken,
    emit: &mut dyn FnMut(&[u8]) -> io::Result<()>,
) -> io::Result<Option<()>> {
    let mut spill: Option<Spill> = None;
    let mut runs: Vec<String> = Vec::new();
    let mut batch: Vec<Vec<u8>> = Vec::new();
    let mut size = 0usize;
    let mut done = false;
    while !done {
        match records()? {
            Some(r) => {
                size += r.len() + 24;
                batch.push(r);
            }
            None => done = true,
        }
        if cancel.is_cancelled() {
            return Ok(None);
        }
        if size >= run_bytes || (done && !runs.is_empty() && !batch.is_empty()) {
            batch.sort_by(|a, b| cmp(a, b));
            let s = match &spill {
                Some(s) => s,
                None => spill.insert(Spill::new("sort")?),
            };
            let name = format!("run-{}", runs.len());
            let mut w = s.create(&name)?;
            for r in batch.drain(..) {
                write_record(&mut w, &r)?;
            }
            w.flush()?;
            runs.push(name);
            size = 0;
        }
    }
    let Some(spill) = spill else {
        // All in memory.
        batch.sort_by(|a, b| cmp(a, b));
        for r in &batch {
            emit(r)?;
        }
        return Ok(Some(()));
    };
    // Merge in levels of at most MERGE_WAYS runs, keeping runs in input order (stable).
    let mut level = 0;
    while runs.len() > MERGE_WAYS {
        let mut next = Vec::new();
        for (k, group) in runs.chunks(MERGE_WAYS).enumerate() {
            let name = format!("merge-{level}-{k}");
            let mut w = spill.create(&name)?;
            merge(&spill, group, cmp, cancel, &mut |r| write_record(&mut w, r))?;
            w.flush()?;
            next.push(name);
        }
        runs = next;
        level += 1;
    }
    merge(&spill, &runs, cmp, cancel, emit)
}

/// One record at the head of a run, ordered for a min-heap (ties go to the earlier run).
struct Head<'a> {
    rec: Vec<u8>,
    run: usize,
    cmp: &'a dyn Fn(&[u8], &[u8]) -> Ordering,
}

impl PartialEq for Head<'_> {
    fn eq(&self, o: &Self) -> bool {
        self.cmp(o) == Ordering::Equal
    }
}
impl Eq for Head<'_> {}
impl PartialOrd for Head<'_> {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Head<'_> {
    fn cmp(&self, o: &Self) -> Ordering {
        (self.cmp)(&self.rec, &o.rec).then(self.run.cmp(&o.run))
    }
}

fn merge(spill: &Spill, runs: &[String], cmp: &dyn Fn(&[u8], &[u8]) -> Ordering, cancel: &CancelToken, emit: &mut dyn FnMut(&[u8]) -> io::Result<()>) -> io::Result<Option<()>> {
    let mut readers: Vec<BufReader<File>> = runs.iter().map(|r| spill.open(r)).collect::<io::Result<_>>()?;
    let mut heap = BinaryHeap::new();
    for (i, r) in readers.iter_mut().enumerate() {
        if let Some(rec) = next_record(r)? {
            heap.push(Reverse(Head { rec, run: i, cmp }));
        }
    }
    let mut n = 0u64;
    while let Some(Reverse(h)) = heap.pop() {
        emit(&h.rec)?;
        if let Some(rec) = next_record(&mut readers[h.run])? {
            heap.push(Reverse(Head { rec, run: h.run, cmp }));
        }
        n += 1;
        if n.is_multiple_of(65_536) && cancel.is_cancelled() {
            return Ok(None);
        }
    }
    Ok(Some(()))
}

/// Writes the document's lines sorted (unique, or counted) by `spec`, each ending with `eol`
/// (encoded), keeping a byte order mark. Returns the lines written, or `None` when cancelled.
pub fn sort_to(buf: &TextBuf, spec: SortSpec, eol: &[u8], out: &mut dyn Write, cancel: &CancelToken, progress: Option<&Progress>) -> io::Result<Option<u64>> {
    sort_runs(buf, spec, eol, out, cancel, progress, RUN_BYTES)
}

#[allow(clippy::too_many_arguments)]
fn sort_runs(buf: &TextBuf, spec: SortSpec, eol: &[u8], out: &mut dyn Write, cancel: &CancelToken, progress: Option<&Progress>, run_bytes: usize) -> io::Result<Option<u64>> {
    let codec = buf.codec;
    if let Some(p) = progress {
        p.set_total(buf.len());
    }
    if spec.counts_csv {
        out.write_all(b"count,line\n")?;
    } else {
        let bom = buf.read(0..codec.bom_len as u64).ok_or_else(|| io::Error::other("Could not read the document"))?;
        out.write_all(&bom)?;
    }
    let mut lines = LineReader::new(buf);
    let mut read = 0u64;
    let records = || -> io::Result<Option<Vec<u8>>> {
        let r = lines.next()?.map(|(_, l)| l.to_vec());
        read += 1;
        if read.is_multiple_of(65_536) {
            if let Some(p) = progress {
                p.set_done(lines.taken());
            }
        }
        Ok(r)
    };
    let line_cmp = |a: &[u8], b: &[u8]| compare(&codec, &spec, a, b);
    let mut written = 0u64;
    let mut prev: Option<Vec<u8>> = None;
    let mut count = 0u64;
    match spec.mode {
        SortMode::Sort => {
            let r = external_sort(records, &line_cmp, run_bytes, cancel, &mut |l| {
                written += 1;
                out.write_all(l)?;
                out.write_all(eol)
            })?;
            Ok(r.map(|_| written))
        }
        SortMode::Unique => {
            let r = external_sort(records, &line_cmp, run_bytes, cancel, &mut |l| {
                if prev.as_deref() == Some(l) {
                    return Ok(());
                }
                prev = Some(l.to_vec());
                written += 1;
                out.write_all(l)?;
                out.write_all(eol)
            })?;
            Ok(r.map(|_| written))
        }
        SortMode::Count => {
            // Counted runs of equal lines, then sorted by count (most first), line order kept.
            let mut counted: Vec<Vec<u8>> = Vec::new();
            let push = |prev: &Option<Vec<u8>>, count: u64, counted: &mut Vec<Vec<u8>>| {
                if let Some(p) = prev {
                    let mut rec = (u64::MAX - count).to_be_bytes().to_vec();
                    rec.extend_from_slice(p);
                    counted.push(rec);
                }
            };
            // The counted lines are themselves sorted externally (they can be as many).
            let mut spill_counts: Option<(Spill, BufWriter<File>)> = None;
            let mut kept = 0usize;
            let r = external_sort(records, &line_cmp, run_bytes, cancel, &mut |l| {
                if prev.as_deref() == Some(l) {
                    count += 1;
                    return Ok(());
                }
                push(&prev, count, &mut counted);
                kept += counted.last().map_or(0, |c| c.len());
                if kept > run_bytes {
                    let (_, w) = match &mut spill_counts {
                        Some(x) => x,
                        None => {
                            let s = Spill::new("counts")?;
                            let w = s.create("counted")?;
                            spill_counts.insert((s, w))
                        }
                    };
                    for c in counted.drain(..) {
                        write_record(w, &c)?;
                    }
                    kept = 0;
                }
                prev = Some(l.to_vec());
                count = 1;
                Ok(())
            })?;
            if r.is_none() {
                return Ok(None);
            }
            push(&prev, count, &mut counted);
            let mut from_disk = match spill_counts {
                Some((s, mut w)) => {
                    w.flush()?;
                    drop(w);
                    Some((s.open("counted")?, s))
                }
                None => None,
            };
            let mut mem = counted.into_iter();
            let records = || -> io::Result<Option<Vec<u8>>> {
                if let Some((r, _)) = &mut from_disk {
                    if let Some(rec) = next_record(r)? {
                        return Ok(Some(rec));
                    }
                }
                Ok(mem.next())
            };
            let by_count = |a: &[u8], b: &[u8]| a[..8].cmp(&b[..8]);
            let tab = codec.encode("\t");
            let r = external_sort(records, &by_count, run_bytes, cancel, &mut |rec| {
                let n = u64::MAX - u64::from_be_bytes(rec[..8].try_into().unwrap());
                written += 1;
                if spec.counts_csv {
                    let line = crate::csv::quote_field(&codec.decode_lossy(&rec[8..]), crate::csv::Dialect::new(b','));
                    return writeln!(out, "{n},{line}");
                }
                out.write_all(&codec.encode(&n.to_string()))?;
                out.write_all(&tab)?;
                out.write_all(&rec[8..])?;
                out.write_all(eol)
            })?;
            Ok(r.map(|_| written))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::{OpenMode, Source};
    use crate::index::LineIndex;
    use std::sync::Arc;

    fn buf(data: &[u8]) -> (tempfile::NamedTempFile, TextBuf) {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(data).unwrap();
        f.flush().unwrap();
        let s = Arc::new(Source::open(f.path(), OpenMode::Auto).unwrap());
        let det = crate::encoding::detect(data, true);
        (f, TextBuf::new(s.clone(), LineIndex::new(s.len()), det.codec))
    }

    fn sorted(data: &str, spec: SortSpec, run_bytes: usize) -> String {
        let (_f, b) = buf(data.as_bytes());
        let mut out = Vec::new();
        sort_runs(&b, spec, b"\n", &mut out, &CancelToken::new(), None, run_bytes).unwrap().unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn modes_in_memory_and_through_runs() {
        let text = "pear\napple\nPear\nbanana\napple\n10 x\n9 y\napple\n";
        for run in [usize::MAX, 30, 1] {
            assert_eq!(sorted(text, SortSpec::default(), run), "10 x\n9 y\nPear\napple\napple\napple\nbanana\npear\n", "run {run}");
            assert_eq!(sorted(text, SortSpec { mode: SortMode::Unique, ..Default::default() }, run), "10 x\n9 y\nPear\napple\nbanana\npear\n");
            assert_eq!(sorted(text, SortSpec { reverse: true, ..Default::default() }, run).lines().next(), Some("pear"));
            assert_eq!(sorted(text, SortSpec { numeric: true, ..Default::default() }, run).lines().take(2).collect::<Vec<_>>(), ["9 y", "10 x"]);
            // Ignoring case keeps the original order of equal keys (stable).
            assert!(sorted(text, SortSpec { ignore_case: true, ..Default::default() }, run).contains("pear\nPear\n"), "run {run}");
            assert_eq!(sorted(text, SortSpec { mode: SortMode::Count, ..Default::default() }, run), "3\tapple\n1\t10 x\n1\t9 y\n1\tPear\n1\tbanana\n1\tpear\n", "run {run}");
            assert!(sorted("b,1\na\nb,1\n", SortSpec { mode: SortMode::Count, counts_csv: true, ..Default::default() }, run).starts_with("count,line\n2,\"b,1\"\n1,a\n"));
        }
    }

    #[test]
    fn many_runs_merge_in_levels_and_keep_the_bom() {
        let text: String = (0..3000).map(|i| format!("{:04}\n", (i * 7919) % 3000)).collect();
        let out = sorted(&text, SortSpec::default(), 8);
        let want: String = (0..3000).map(|i| format!("{i:04}\n")).collect();
        assert!(out == want);
        let with_bom = "\u{feff}b\na\n".to_string();
        assert_eq!(sorted(&with_bom, SortSpec::default(), 1), "\u{feff}a\nb\n");
        // UTF-16, CRLF.
        let utf16: Vec<u8> = [0xFF, 0xFE].into_iter().chain("b\r\nä\r\na\r\n".encode_utf16().flat_map(|u| u.to_le_bytes())).collect();
        let (_f, b) = buf(&utf16);
        let mut out = Vec::new();
        let eol: Vec<u8> = "\r\n".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        sort_runs(&b, SortSpec::default(), &eol, &mut out, &CancelToken::new(), None, 1).unwrap().unwrap();
        let back = String::from_utf16(&out[2..].chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>()).unwrap();
        assert_eq!(back, "a\r\nb\r\nä\r\n");
        let c = CancelToken::new();
        c.cancel();
        let (_g, b) = buf(text.as_bytes());
        assert_eq!(sort_runs(&b, SortSpec::default(), b"\n", &mut Vec::new(), &c, None, 8).unwrap(), None);
    }
}
