//! Sparse, incremental line index over the *original* file.
//!
//! `cp[k]` is the number of newline units in `[0, min(k*BLOCK, len))`. Only one `u64` per 64 KB is
//! stored (50 GB → 6.4 MB). Exact lookups scan at most one block. Positions beyond the indexed
//! frontier are estimated from the average line length seen so far.

use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use crate::encoding::Codec;
use crate::file::Source;
use crate::jobs::{self, CancelToken, Throttle, Waker};

pub const BLOCK: u64 = 64 * 1024;
/// Bytes read per indexer step.
const BATCH: u64 = 64 * BLOCK;

pub struct LineIndex {
    len: u64,
    cp: RwLock<Vec<u64>>,
    done: AtomicBool,
    failed: AtomicBool,
    started: Instant,
    finished_after: RwLock<Option<Duration>>,
}

impl LineIndex {
    pub fn new(len: u64) -> Arc<LineIndex> {
        Self::with_prefix(len, vec![0])
    }

    /// Starts from previously computed checkpoints (e.g. the unchanged prefix after a save).
    pub fn with_prefix(len: u64, mut cp: Vec<u64>) -> Arc<LineIndex> {
        if cp.is_empty() {
            cp.push(0);
        }
        let max_entries = len.div_ceil(BLOCK) as usize + 1;
        cp.truncate(max_entries.max(1));
        let done = (cp.len() as u64 - 1) * BLOCK >= len;
        Arc::new(LineIndex {
            len,
            cp: RwLock::new(cp),
            done: AtomicBool::new(done),
            failed: AtomicBool::new(false),
            started: Instant::now(),
            finished_after: RwLock::new(if done { Some(Duration::ZERO) } else { None }),
        })
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn is_done(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }

    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }

    pub fn finished_after(&self) -> Option<Duration> {
        *self.finished_after.read().unwrap()
    }

    /// Bytes from the start of the file that are fully indexed.
    pub fn indexed_bytes(&self) -> u64 {
        let n = self.cp.read().unwrap().len() as u64 - 1;
        (n * BLOCK).min(self.len)
    }

    pub fn progress(&self) -> f32 {
        if self.len == 0 {
            1.0
        } else {
            self.indexed_bytes() as f32 / self.len as f32
        }
    }

    /// Total number of lines (newlines + 1) once indexing has finished.
    pub fn total_lines(&self) -> Option<u64> {
        if self.is_done() {
            Some(self.cp.read().unwrap().last().copied().unwrap_or(0) + 1)
        } else {
            None
        }
    }

    /// Newlines in the indexed prefix.
    pub fn indexed_newlines(&self) -> u64 {
        self.cp.read().unwrap().last().copied().unwrap_or(0)
    }

    pub fn memory_bytes(&self) -> usize {
        self.cp.read().unwrap().capacity() * 8
    }

    /// Checkpoints whose blocks end at or before `limit` (to carry over after a save).
    pub fn prefix_checkpoints(&self, limit: u64) -> Vec<u64> {
        let cp = self.cp.read().unwrap();
        let n = ((limit / BLOCK) as usize + 1).min(cp.len());
        cp[..n].to_vec()
    }

    /// Average bytes per line in the indexed prefix.
    pub fn avg_line_len(&self) -> f64 {
        let (bytes, lines) = {
            let cp = self.cp.read().unwrap();
            (((cp.len() as u64 - 1) * BLOCK).min(self.len), *cp.last().unwrap())
        };
        if bytes == 0 {
            80.0
        } else if lines == 0 {
            bytes.max(1) as f64
        } else {
            bytes as f64 / lines as f64
        }
    }

    /// Exact number of newlines in `[0, off)`, if `off` lies in the indexed region.
    pub fn nl_before(&self, src: &Source, codec: &Codec, blocking: bool, off: u64) -> Option<u64> {
        let off = off.min(self.len);
        let k = (off / BLOCK) as usize;
        let base = {
            let cp = self.cp.read().unwrap();
            *cp.get(k)?
        };
        let bstart = k as u64 * BLOCK;
        if off == bstart {
            return Some(base);
        }
        let bytes = src.get(bstart..off, blocking)?;
        Some(base + codec.count_nl(&bytes, bstart))
    }

    /// Estimated newlines in `[0, off)` (exact when indexed).
    pub fn nl_before_est(&self, src: &Source, codec: &Codec, blocking: bool, off: u64) -> (u64, bool) {
        if let Some(n) = self.nl_before(src, codec, blocking, off) {
            return (n, true);
        }
        let idx = self.indexed_bytes();
        let base = self.indexed_newlines();
        let extra = off.saturating_sub(idx) as f64 / self.avg_line_len();
        (base + extra as u64, false)
    }

    /// Exact start offset of 0-based `line`, if the index covers it. `None` also when the line
    /// does not exist (beyond the last line of a fully indexed file).
    pub fn line_start(&self, src: &Source, codec: &Codec, blocking: bool, line: u64) -> Option<u64> {
        if line == 0 {
            return Some(0);
        }
        let (k, need) = {
            let cp = self.cp.read().unwrap();
            let pp = cp.partition_point(|&c| c < line);
            if pp == cp.len() {
                return None;
            }
            (pp - 1, line - cp[pp - 1])
        };
        let bstart = k as u64 * BLOCK;
        let bend = (bstart + BLOCK).min(self.len);
        let bytes = src.get(bstart..bend, blocking)?;
        let p = codec.nth_nl(&bytes, bstart, need)?;
        Some(bstart + p as u64 + codec.unit() as u64)
    }

    /// Start offset of `line`: exact if indexed, otherwise an estimate (not necessarily a line
    /// start; callers snap it).
    pub fn line_start_est(&self, src: &Source, codec: &Codec, blocking: bool, line: u64) -> (u64, bool) {
        if let Some(off) = self.line_start(src, codec, blocking, line) {
            return (off, true);
        }
        if self.is_done() {
            return (self.len, false);
        }
        let idx = self.indexed_bytes();
        let base = self.indexed_newlines();
        let est = idx as f64 + line.saturating_sub(base) as f64 * self.avg_line_len();
        ((est as u64).min(self.len), false)
    }

    fn push(&self, entries: &[u64]) {
        self.cp.write().unwrap().extend_from_slice(entries);
    }
}

/// Builds the index in the background, from the start of the file (so the top is navigable at
/// once). Cancelled via `cancel`; wakes the UI periodically.
pub fn spawn_indexer(
    src: Arc<Source>,
    codec: Codec,
    index: Arc<LineIndex>,
    cancel: CancelToken,
    waker: Waker,
) -> std::thread::JoinHandle<()> {
    jobs::spawn("indexer", move || {
        let len = index.len;
        let (mut pos, mut count) = {
            let cp = index.cp.read().unwrap();
            ((cp.len() as u64 - 1) * BLOCK, *cp.last().unwrap())
        };
        let mut throttle = Throttle::new(Duration::from_millis(100));
        let mut entries = Vec::with_capacity((BATCH / BLOCK) as usize);
        let mut buf = vec![0u8; BATCH as usize];
        while pos < len {
            if cancel.is_cancelled() {
                return;
            }
            let end = (pos + BATCH).min(len);
            let bytes = &mut buf[..(end - pos) as usize];
            if src.read_into(pos, bytes).is_err() {
                index.failed.store(true, Ordering::Relaxed);
                waker();
                return;
            }
            let bytes = &*bytes;
            entries.clear();
            let mut c = pos;
            while c < end {
                let ce = (c + BLOCK).min(end);
                count += codec.count_nl(&bytes[(c - pos) as usize..(ce - pos) as usize], c);
                entries.push(count);
                c = ce;
            }
            index.push(&entries);
            pos = end;
            if throttle.ready() {
                waker();
            }
        }
        *index.finished_after.write().unwrap() = Some(index.started.elapsed());
        index.done.store(true, Ordering::Release);
        waker();
    })
}

/// Indexes synchronously (tests and benchmarks).
pub fn build_blocking(src: &Arc<Source>, codec: Codec) -> Arc<LineIndex> {
    let index = LineIndex::new(src.len());
    let h = spawn_indexer(src.clone(), codec, index.clone(), CancelToken::new(), jobs::noop_waker());
    let _ = h.join();
    index
}

/// Byte range helper used by tests.
pub fn block_range(k: u64, len: u64) -> Range<u64> {
    let s = k * BLOCK;
    s..(s + BLOCK).min(len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::{Encoding, NlByte};
    use crate::file::OpenMode;
    use std::io::Write;

    fn source_with(data: &[u8]) -> (tempfile::NamedTempFile, Arc<Source>) {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(data).unwrap();
        f.flush().unwrap();
        let s = Arc::new(Source::open(f.path(), OpenMode::Auto).unwrap());
        (f, s)
    }

    fn naive_line_starts(data: &[u8]) -> Vec<u64> {
        let mut v = vec![0];
        for (i, &b) in data.iter().enumerate() {
            if b == b'\n' {
                v.push(i as u64 + 1);
            }
        }
        v
    }

    #[test]
    fn index_matches_naive() {
        let mut data = Vec::new();
        let mut rng = 12345u64;
        while data.len() < 1_000_000 {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            let n = (rng % 300) as usize;
            data.extend(std::iter::repeat_n(b'x', n));
            data.push(b'\n');
        }
        data.extend_from_slice(b"tail without newline");
        let (_f, src) = source_with(&data);
        let codec = Codec::utf8();
        let idx = build_blocking(&src, codec);
        assert!(idx.is_done());
        let starts = naive_line_starts(&data);
        assert_eq!(idx.total_lines(), Some(starts.len() as u64));
        for (line, &s) in starts.iter().enumerate().step_by(97) {
            assert_eq!(idx.line_start(&src, &codec, true, line as u64), Some(s), "line {line}");
            assert_eq!(idx.nl_before(&src, &codec, true, s), Some(line as u64));
        }
        assert_eq!(idx.line_start(&src, &codec, true, starts.len() as u64), None);
        assert!(idx.memory_bytes() < 1024);
    }

    #[test]
    fn index_utf16() {
        let text: String = (0..20_000).map(|i| format!("строка {i}\n")).collect();
        let mut data = vec![0xFF, 0xFE];
        data.extend(text.encode_utf16().flat_map(|u| u.to_le_bytes()));
        let (_f, src) = source_with(&data);
        let codec = Codec::new(Encoding::Utf16Le, NlByte::Lf, 2);
        let idx = build_blocking(&src, codec);
        assert_eq!(idx.total_lines(), Some(20_001));
        let off = idx.line_start(&src, &codec, true, 12_345).unwrap();
        let line_bytes = src.read(off..off + 40).unwrap();
        assert!(codec.decode_lossy(&line_bytes).starts_with("строка 12345\n"));
    }

    #[test]
    fn empty_and_no_newline() {
        let (_f, src) = source_with(b"");
        let idx = build_blocking(&src, Codec::utf8());
        assert_eq!(idx.total_lines(), Some(1));
        let (_g, src) = source_with(b"single line");
        let idx = build_blocking(&src, Codec::utf8());
        assert_eq!(idx.total_lines(), Some(1));
        assert_eq!(idx.line_start(&src, &Codec::utf8(), true, 1), None);
    }

    #[test]
    fn estimates_before_indexing() {
        let data: Vec<u8> = b"0123456789\n".repeat(100_000);
        let (_f, src) = source_with(&data);
        let idx = LineIndex::with_prefix(src.len(), vec![0, BLOCK / 11]);
        let (est, exact) = idx.line_start_est(&src, &Codec::utf8(), true, 50_000);
        assert!(!exact);
        let real = 50_000u64 * 11;
        assert!((est as i64 - real as i64).abs() < 1000, "estimate {est} vs {real}");
    }
}
