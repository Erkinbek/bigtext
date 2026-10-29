//! The lines of one text that occur nowhere in another, whatever their order (a set difference
//! by line), for texts of any size. The other text's line hashes are held in memory up to a
//! budget; past it both sides are split by hash into partition files on disk and compared one
//! partition at a time.
//!
//! Lines compare without their line breaks (CRLF and LF lines are the same), as UTF-8 whatever
//! the encodings, and after the [`DiffOptions`] normalisation (white space, case, timestamps,
//! a mask). A line counts as present when it occurs at least once (sets, not counts).

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashSet};
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Write};

use crate::diff::{line_hash, DiffOptions};
use crate::encoding::Encoding;
use crate::jobs::{CancelToken, Progress};
use crate::lines::LineReader;
use crate::spill::{read_record, Spill};
use crate::text::TextBuf;

/// Distinct line hashes of the other text kept in memory (8 bytes each, plus the table).
pub const BUDGET: usize = 25_000_000;

const PARTS: usize = 256;

/// How many of the text's lines are only in it and how many also in the other.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub only: u64,
    pub common: u64,
}

/// Hashes each line of `buf` as it compares; `f` gets the line's offset and hash, and stops
/// the walk by returning false. `None` when cancelled.
fn each_line(buf: &TextBuf, opts: &DiffOptions, cancel: &CancelToken, progress: Option<(&Progress, u64)>, mut f: impl FnMut(u64, u64) -> io::Result<bool>) -> io::Result<Option<()>> {
    let mut r = LineReader::new(buf);
    let utf8 = buf.codec.enc == Encoding::Utf8;
    let mut n = 0u64;
    while let Some((off, line)) = r.next()? {
        let decoded;
        let text: &[u8] = if utf8 {
            line
        } else {
            decoded = buf.codec.decode_lossy(line);
            decoded.as_bytes()
        };
        let hash = if opts.plain() { line_hash(text) } else { line_hash(&opts.normalise(text)) };
        if !f(off, hash)? {
            break;
        }
        n += 1;
        if n.is_multiple_of(16_384) {
            if cancel.is_cancelled() {
                return Ok(None);
            }
            if let Some((p, base)) = progress {
                p.set_done(base + off);
            }
        }
    }
    Ok(Some(()))
}

fn writers(spill: &Spill, side: &str) -> io::Result<Vec<BufWriter<File>>> {
    (0..PARTS).map(|i| spill.create(&format!("{side}-{i:03}"))).collect()
}

fn part(hash: u64) -> usize {
    (hash >> 56) as usize % PARTS
}

/// Reads 8-byte little-endian numbers until the end.
fn read_u64s(spill: &Spill, name: &str, mut f: impl FnMut(u64) -> io::Result<()>) -> io::Result<()> {
    let mut r = spill.open(name)?;
    let mut b = [0u8; 8];
    while read_record(&mut r, &mut b)? {
        f(u64::from_le_bytes(b))?;
    }
    Ok(())
}

/// Calls `on_line` with the offset of each line of `a` that is not in `b`, in order (return
/// false to stop). `budget` is the number of distinct line hashes of `b` held in memory before
/// spilling to disk. `None` when cancelled.
pub fn only_in(a: &TextBuf, b: &TextBuf, opts: &DiffOptions, budget: usize, cancel: &CancelToken, progress: Option<&Progress>, mut on_line: impl FnMut(u64) -> bool) -> io::Result<Option<Counts>> {
    if let Some(p) = progress {
        p.set_total(a.len() + b.len());
    }
    // Pass 1: the other text's lines.
    let mut set: HashSet<u64> = HashSet::new();
    let mut spill: Option<(Spill, Vec<BufWriter<File>>)> = None;
    let walked = each_line(b, opts, cancel, progress.map(|p| (p, 0)), |_, h| {
        match &mut spill {
            Some((_, w)) => w[part(h)].write_all(&h.to_le_bytes())?,
            None => {
                set.insert(h);
                if set.len() > budget {
                    let s = Spill::new("only-in")?;
                    let mut w = writers(&s, "b")?;
                    for &h in &set {
                        w[part(h)].write_all(&h.to_le_bytes())?;
                    }
                    set = HashSet::new();
                    spill = Some((s, w));
                }
            }
        }
        Ok(true)
    })?;
    if walked.is_none() {
        return Ok(None);
    }
    let mut counts = Counts::default();
    let Some((spill, mut bw)) = spill else {
        // Pass 2, all in memory: this text's lines in order.
        let walked = each_line(a, opts, cancel, progress.map(|p| (p, b.len())), |off, h| {
            if set.contains(&h) {
                counts.common += 1;
                Ok(true)
            } else {
                counts.only += 1;
                Ok(on_line(off))
            }
        })?;
        return Ok(walked.map(|_| counts));
    };
    for w in &mut bw {
        w.flush()?;
    }
    drop(bw);
    // Pass 2 on disk: (hash, offset) of this text's lines by partition.
    let mut aw = writers(&spill, "a")?;
    let walked = each_line(a, opts, cancel, progress.map(|p| (p, b.len())), |off, h| {
        let w = &mut aw[part(h)];
        w.write_all(&h.to_le_bytes())?;
        w.write_all(&off.to_le_bytes())?;
        Ok(true)
    })?;
    if walked.is_none() {
        return Ok(None);
    }
    for w in &mut aw {
        w.flush()?;
    }
    drop(aw);
    // Each partition: the offsets of the lines not in the other, still in order.
    for i in 0..PARTS {
        if cancel.is_cancelled() {
            return Ok(None);
        }
        let mut set = HashSet::new();
        read_u64s(&spill, &format!("b-{i:03}"), |h| {
            set.insert(h);
            Ok(())
        })?;
        let mut out = spill.create(&format!("o-{i:03}"))?;
        let mut hash = None;
        read_u64s(&spill, &format!("a-{i:03}"), |x| {
            match hash.take() {
                None => hash = Some(x),
                Some(h) if set.contains(&h) => counts.common += 1,
                Some(_) => {
                    counts.only += 1;
                    out.write_all(&x.to_le_bytes())?;
                }
            }
            Ok(())
        })?;
        out.flush()?;
    }
    // Merge the partitions back into the text's order.
    let mut readers: Vec<BufReader<File>> = (0..PARTS).map(|i| spill.open(&format!("o-{i:03}"))).collect::<io::Result<_>>()?;
    let next = |r: &mut BufReader<File>| -> io::Result<Option<u64>> {
        let mut b = [0u8; 8];
        Ok(read_record(r, &mut b)?.then(|| u64::from_le_bytes(b)))
    };
    let mut heap = BinaryHeap::new();
    for (i, r) in readers.iter_mut().enumerate() {
        if let Some(off) = next(r)? {
            heap.push(Reverse((off, i)));
        }
    }
    while let Some(Reverse((off, i))) = heap.pop() {
        if !on_line(off) {
            break;
        }
        if let Some(o) = next(&mut readers[i])? {
            heap.push(Reverse((o, i)));
        }
    }
    Ok(Some(counts))
}

/// Writes the line at `off` (with its line break) from `buf`.
pub fn write_line_at(buf: &TextBuf, off: u64, w: &mut dyn Write) -> io::Result<()> {
    let len = buf.len();
    let mut pos = off;
    let mut chunk = Vec::new();
    while pos < len {
        let end = (pos + (64 << 10)).min(len);
        let n = buf.read_to(pos..end, &mut chunk).ok_or_else(|| io::Error::other("reading the text failed"))?;
        let part = &chunk[..n];
        if let Some(i) = buf.codec.find_nl(part, pos) {
            let stop = (i + buf.codec.unit()).min(n);
            return w.write_all(&part[..stop]);
        }
        w.write_all(part)?;
        pos = end;
    }
    w.write_all(buf.codec.nl_unit())
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
        let b = TextBuf::new(s.clone(), LineIndex::new(s.len()), det.codec);
        (f, b)
    }

    /// The lines only in `a` (as text) and the counts.
    fn run(a: &TextBuf, b: &TextBuf, opts: &DiffOptions, budget: usize) -> (Vec<String>, Counts) {
        let mut offs = Vec::new();
        let c = only_in(a, b, opts, budget, &CancelToken::new(), None, |o| {
            offs.push(o);
            true
        })
        .unwrap()
        .unwrap();
        let lines = offs
            .iter()
            .map(|&o| {
                let mut v = Vec::new();
                write_line_at(a, o, &mut v).unwrap();
                a.codec.decode_lossy(&v).trim_end().to_string()
            })
            .collect();
        (lines, c)
    }

    #[test]
    fn lines_missing_from_the_other_in_order() {
        let (_a, a) = buf(b"apple\nbanana\ncherry\nbanana\ndate\n");
        let (_b, b) = buf(b"date\r\nbanana\r\nfig\r\n");
        let (lines, c) = run(&a, &b, &DiffOptions::default(), BUDGET);
        assert_eq!(lines, vec!["apple", "cherry"]);
        assert_eq!(c, Counts { only: 2, common: 3 });
        // The other way round.
        let (lines, c) = run(&b, &a, &DiffOptions::default(), BUDGET);
        assert_eq!((lines, c.only), (vec!["fig".to_string()], 1));
    }

    #[test]
    fn spilling_to_disk_gives_the_same_answer() {
        let a: String = (0..5000).map(|i| format!("line {}\n", i * 7 % 5000)).collect();
        let b: String = (0..5000).filter(|i| i % 3 != 0).map(|i| format!("line {i}\n")).collect();
        let (_a, a) = buf(a.as_bytes());
        let (_b, b) = buf(b.as_bytes());
        let mem = run(&a, &b, &DiffOptions::default(), BUDGET);
        let disk = run(&a, &b, &DiffOptions::default(), 10);
        assert_eq!(mem.1.only, 1667);
        assert_eq!(mem, disk);
    }

    #[test]
    fn options_encodings_and_masks() {
        let (_a, a) = buf(b"10:00:01 start job=17\n10:00:02 Working\n10:00:03 done\n");
        let (_b, b) = buf(b"11:30:00 start job=99\n11:30:05 working\n");
        let opts = DiffOptions { timestamps: true, case: true, mask: Some(regex_automata::meta::Regex::new("job=[0-9]+").unwrap()), ..Default::default() };
        assert_eq!(run(&a, &b, &opts, BUDGET).0, vec!["10:00:03 done"]);
        assert_eq!(run(&a, &b, &DiffOptions::default(), BUDGET).1.only, 3);
        // UTF-16 against UTF-8.
        let utf16: Vec<u8> = [0xFF, 0xFE].into_iter().chain("один\nдва\n".encode_utf16().flat_map(|u| u.to_le_bytes())).collect();
        let (_c, c) = buf(&utf16);
        let (_d, d) = buf("два\n".as_bytes());
        assert_eq!(run(&c, &d, &DiffOptions::default(), BUDGET).0, vec!["один"]);
    }
}
