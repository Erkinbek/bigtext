//! Sparse, incremental line index over the *original* file.
//!
//! `cp[k]` is the number of newline units in `[0, min(k*BLOCK, len))`. Only one `u64` per 64 KB is
//! stored (50 GB → 6.4 MB). Exact lookups scan at most one block. Positions beyond the indexed
//! frontier are estimated from the average line length seen so far.

use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
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
            (
                ((cp.len() as u64 - 1) * BLOCK).min(self.len),
                *cp.last().unwrap(),
            )
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
    pub fn nl_before_est(
        &self,
        src: &Source,
        codec: &Codec,
        blocking: bool,
        off: u64,
    ) -> (u64, bool) {
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
    pub fn line_start(
        &self,
        src: &Source,
        codec: &Codec,
        blocking: bool,
        line: u64,
    ) -> Option<u64> {
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
    pub fn line_start_est(
        &self,
        src: &Source,
        codec: &Codec,
        blocking: bool,
        line: u64,
    ) -> (u64, bool) {
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
///
/// A large local file (mapped or read with pread) is counted by several threads; chunked
/// sources (network and removable drives, remote files) keep one sequential reader, where
/// parallel reads would only compete for the same slow device or server.
pub fn spawn_indexer(
    src: Arc<Source>,
    codec: Codec,
    index: Arc<LineIndex>,
    cancel: CancelToken,
    waker: Waker,
) -> std::thread::JoinHandle<()> {
    jobs::spawn("indexer", move || {
        let threads = indexer_threads(&src, index.len - index.indexed_bytes());
        run_indexer(&src, codec, &index, &cancel, &waker, threads, BATCH);
    })
}

/// Most threads one indexer uses: newline counting runs at memory speed long before this.
const MAX_INDEX_THREADS: usize = 8;
/// Below this many batches left the file is counted by one thread.
const MIN_PARALLEL_BATCHES: u64 = 4;
/// A worker reads its batch in pieces this large, so that the bytes it counts are still in its
/// core's cache (several times faster than counting 4 MB read in one go, measured).
const PIECE: u64 = 4 * BLOCK;

fn indexer_threads(src: &Source, remaining: u64) -> usize {
    if !(src.is_mmap() || src.is_pread()) {
        return 1;
    }
    let batches = remaining.div_ceil(BATCH);
    if batches < MIN_PARALLEL_BATCHES {
        return 1;
    }
    std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(MAX_INDEX_THREADS)
        .min(batches as usize)
}

/// Indexes from the end of the existing prefix to the end of the file with `threads` threads,
/// reading `batch` bytes (a multiple of [`BLOCK`]) per step.
fn run_indexer(
    src: &Source,
    codec: Codec,
    index: &LineIndex,
    cancel: &CancelToken,
    waker: &Waker,
    threads: usize,
    batch: u64,
) {
    debug_assert!(batch > 0 && batch.is_multiple_of(BLOCK));
    let finished = if threads > 1 {
        index_parallel(src, codec, index, cancel, waker, threads, batch)
    } else {
        index_serial(src, codec, index, cancel, waker, batch)
    };
    match finished {
        Ok(true) => {
            *index.finished_after.write().unwrap() = Some(index.started.elapsed());
            index.done.store(true, Ordering::Release);
            waker();
        }
        Ok(false) => {}
        Err(_) => {
            index.failed.store(true, Ordering::Relaxed);
            waker();
        }
    }
}

/// Appends the newline count of each block of `bytes` (whose first byte is at absolute offset
/// `base`, a multiple of [`BLOCK`]) to `out`.
fn count_blocks(codec: &Codec, bytes: &[u8], base: u64, out: &mut Vec<u64>) {
    for (i, block) in bytes.chunks(BLOCK as usize).enumerate() {
        out.push(codec.count_nl(block, base + i as u64 * BLOCK));
    }
}

/// Appends `counts` (per block) to the index as running totals.
fn publish(index: &LineIndex, counts: &mut [u64]) {
    let mut total = index.indexed_newlines();
    for c in counts.iter_mut() {
        total += *c;
        *c = total;
    }
    index.push(counts);
}

/// One thread reading the file front to back. `Ok(false)`: cancelled.
fn index_serial(
    src: &Source,
    codec: Codec,
    index: &LineIndex,
    cancel: &CancelToken,
    waker: &Waker,
    batch: u64,
) -> std::io::Result<bool> {
    let len = index.len;
    let mut pos = index.indexed_bytes();
    let mut throttle = Throttle::new(Duration::from_millis(100));
    let mut counts = Vec::with_capacity((batch / BLOCK) as usize);
    let mut buf = vec![0u8; batch.min(len - pos) as usize];
    while pos < len {
        if cancel.is_cancelled() {
            return Ok(false);
        }
        let end = (pos + batch).min(len);
        let bytes = &mut buf[..(end - pos) as usize];
        src.read_into(pos, bytes)?;
        counts.clear();
        count_blocks(&codec, bytes, pos, &mut counts);
        publish(index, &mut counts);
        pos = end;
        if throttle.ready() {
            waker();
        }
    }
    Ok(true)
}

/// State shared by the counting workers and the thread that publishes their results.
struct Pipeline {
    /// Next batch (0 = the first after the existing prefix) a worker may claim.
    next: u64,
    /// Batches published so far; workers stay within `window` batches of it.
    published: u64,
    /// Per-block newline counts of finished batches, at `batch % window`.
    slots: Vec<Option<Vec<u64>>>,
    /// First batch whose read failed.
    failed_at: Option<u64>,
    /// Set on cancellation: workers claim nothing more.
    stop: bool,
}

/// Workers claim successive batches, each reads its batch (in [`PIECE`]s) and counts the
/// newlines of every block; this thread adds the counts up and publishes them in file order, so
/// the indexed part is always a prefix of the file. At most `2 * threads` batches are claimed
/// ahead of the published prefix. `Ok(false)`: cancelled.
fn index_parallel(
    src: &Source,
    codec: Codec,
    index: &LineIndex,
    cancel: &CancelToken,
    waker: &Waker,
    threads: usize,
    batch: u64,
) -> std::io::Result<bool> {
    let start = index.indexed_bytes();
    let len = index.len;
    let batches = (len - start).div_ceil(batch);
    let window = 2 * threads;
    let shared = (
        Mutex::new(Pipeline {
            next: 0,
            published: 0,
            slots: vec![None; window],
            failed_at: None,
            stop: false,
        }),
        Condvar::new(),
    );
    let (state, cv) = &shared;
    let mut error = None;

    let piece = PIECE.min(batch);
    let worker = || {
        let mut buf = vec![0u8; piece as usize];
        loop {
            let b = {
                let mut st = state.lock().unwrap();
                loop {
                    if st.stop || st.failed_at.is_some() || st.next >= batches {
                        return None;
                    }
                    if cancel.is_cancelled() {
                        return None;
                    }
                    if st.next < st.published + window as u64 {
                        break;
                    }
                    st = cv.wait(st).unwrap();
                }
                st.next += 1;
                st.next - 1
            };
            let pos = start + b * batch;
            let end = (pos + batch).min(len);
            let mut counts = Vec::with_capacity((batch / BLOCK) as usize);
            let mut result = Ok(());
            let mut p = pos;
            while p < end {
                let e = (p + piece).min(end);
                let bytes = &mut buf[..(e - p) as usize];
                result = src.read_into(p, bytes);
                if result.is_err() {
                    break;
                }
                count_blocks(&codec, bytes, p, &mut counts);
                p = e;
            }
            let result = result.map(|()| counts);
            let mut st = state.lock().unwrap();
            match result {
                Ok(counts) => st.slots[(b % window as u64) as usize] = Some(counts),
                Err(e) => {
                    if st.failed_at.is_none_or(|f| b < f) {
                        st.failed_at = Some(b);
                        drop(st);
                        cv.notify_all();
                        return Some(e);
                    }
                }
            }
            drop(st);
            cv.notify_all();
        }
    };

    let finished = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads)
            .map(|i| {
                std::thread::Builder::new()
                    .name(format!("indexer-{i}"))
                    .spawn_scoped(scope, worker)
                    .expect("failed to spawn worker thread")
            })
            .collect();
        let mut throttle = Throttle::new(Duration::from_millis(100));
        let finished = loop {
            let mut counts = {
                let mut st = state.lock().unwrap();
                loop {
                    if st.published == batches {
                        break None;
                    }
                    let slot = (st.published % window as u64) as usize;
                    if let Some(c) = st.slots[slot].take() {
                        break Some(c);
                    }
                    if cancel.is_cancelled() || st.failed_at == Some(st.published) {
                        st.stop = true;
                        break None;
                    }
                    // Woken by every finished batch; the timeout notices a cancellation.
                    st = cv.wait_timeout(st, Duration::from_millis(20)).unwrap().0;
                }
            };
            let Some(counts) = counts.as_mut() else {
                cv.notify_all();
                break state.lock().unwrap().published == batches;
            };
            publish(index, counts);
            state.lock().unwrap().published += 1;
            cv.notify_all();
            if throttle.ready() {
                waker();
            }
        };
        for h in handles {
            if let Ok(Some(e)) = h.join() {
                error.get_or_insert(e);
            }
        }
        finished
    });
    match error {
        Some(e) if !finished && !cancel.is_cancelled() => Err(e),
        _ => Ok(finished),
    }
}

/// Indexes synchronously (tests and benchmarks).
pub fn build_blocking(src: &Arc<Source>, codec: Codec) -> Arc<LineIndex> {
    let index = LineIndex::new(src.len());
    let h = spawn_indexer(
        src.clone(),
        codec,
        index.clone(),
        CancelToken::new(),
        jobs::noop_waker(),
    );
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
    use std::sync::atomic::AtomicUsize;

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
            assert_eq!(
                idx.line_start(&src, &codec, true, line as u64),
                Some(s),
                "line {line}"
            );
            assert_eq!(idx.nl_before(&src, &codec, true, s), Some(line as u64));
        }
        assert_eq!(
            idx.line_start(&src, &codec, true, starts.len() as u64),
            None
        );
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
        assert!(codec
            .decode_lossy(&line_bytes)
            .starts_with("строка 12345\n"));
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
        assert!(
            (est as i64 - real as i64).abs() < 1000,
            "estimate {est} vs {real}"
        );
    }

    /// Random bytes with newline units at random places (and some lone halves of UTF-16 ones).
    fn random_text(len: usize, codec: &Codec, seed: u64) -> Vec<u8> {
        let unit = codec.nl_unit();
        let mut rng = seed | 1;
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        let mut data = Vec::with_capacity(len + 2);
        while data.len() < len {
            let r = next();
            match r % 40 {
                0..=2 => data.extend_from_slice(unit),
                3 => data.push(unit[0]),
                _ => data.push((r >> 8) as u8),
            }
        }
        data.truncate(len);
        data
    }

    /// Checkpoints counted byte by byte, without [`Codec`].
    fn naive_checkpoints(data: &[u8], codec: &Codec) -> Vec<u64> {
        let unit = codec.nl_unit();
        let mut cp = vec![0u64];
        for block in data.chunks(BLOCK as usize) {
            // Blocks start at even offsets, so a UTF-16 unit is one at an even index here.
            let n = if unit.len() == 2 {
                block.chunks_exact(2).filter(|p| *p == unit).count()
            } else {
                block.iter().filter(|&&b| b == unit[0]).count()
            };
            cp.push(cp.last().unwrap() + n as u64);
        }
        cp
    }

    fn checkpoints(idx: &LineIndex) -> Vec<u64> {
        idx.cp.read().unwrap().clone()
    }

    fn run(src: &Source, codec: Codec, idx: &LineIndex, threads: usize, batch: u64) {
        run_indexer(
            src,
            codec,
            idx,
            &CancelToken::new(),
            &jobs::noop_waker(),
            threads,
            batch,
        );
    }

    #[test]
    fn parallel_index_matches_naive_count() {
        let codecs = [
            Codec::utf8(),
            Codec::new(Encoding::Utf8, NlByte::Cr, 0),
            Codec::new(Encoding::Utf16Le, NlByte::Lf, 2),
            Codec::new(Encoding::Utf16Be, NlByte::Lf, 2),
            Codec::new(Encoding::Utf16Le, NlByte::Cr, 2),
        ];
        let b = BLOCK as usize;
        let sizes = [
            1,
            3,
            b - 1,
            b,
            b + 1,
            3 * b + 7,
            7 * b,
            9 * b + 12_345,
            1_000_003,
        ];
        let runs = [
            (1, BATCH),
            (4, BATCH),
            (3, BLOCK),
            (4, 2 * BLOCK),
            (8, 3 * BLOCK),
            (5, 6 * BLOCK),
        ];
        for (ci, codec) in codecs.iter().enumerate() {
            for (si, &size) in sizes.iter().enumerate() {
                let data = random_text(size, codec, (ci * 100 + si) as u64 + 7);
                let want = naive_checkpoints(&data, codec);
                let (_f, src) = source_with(&data);
                for (threads, batch) in runs {
                    let idx = LineIndex::new(src.len());
                    run(&src, *codec, &idx, threads, batch);
                    let what = format!("{codec:?} size {size} threads {threads} batch {batch}");
                    assert!(idx.is_done() && !idx.failed(), "{what}");
                    assert_eq!(checkpoints(&idx), want, "{what}");
                    assert!(idx.finished_after().is_some(), "{what}");
                }
            }
        }
    }

    #[test]
    fn parallel_index_resumes_from_a_prefix() {
        let codec = Codec::new(Encoding::Utf16Be, NlByte::Lf, 2);
        let data = random_text(20 * BLOCK as usize + 333, &codec, 99);
        let want = naive_checkpoints(&data, &codec);
        let (_f, src) = source_with(&data);
        for keep in [1, 2, 5, 17, 21, want.len()] {
            let idx = LineIndex::with_prefix(src.len(), want[..keep].to_vec());
            run(&src, codec, &idx, 4, 2 * BLOCK);
            assert!(idx.is_done() && !idx.failed(), "keep {keep}");
            assert_eq!(checkpoints(&idx), want, "keep {keep}");
        }
        // The real entry point, on a file large enough for several threads.
        let codec = Codec::utf8();
        let data = random_text(5 * BATCH as usize + 1, &codec, 5);
        let want = naive_checkpoints(&data, &codec);
        let (_f, src) = source_with(&data);
        let one_cpu = std::thread::available_parallelism().map_or(true, |n| n.get() == 1);
        assert!(one_cpu || indexer_threads(&src, src.len()) > 1);
        let idx = build_blocking(&src, codec);
        assert_eq!(checkpoints(&idx), want);
        let idx = LineIndex::with_prefix(src.len(), want[..70].to_vec());
        spawn_indexer(
            src.clone(),
            codec,
            idx.clone(),
            CancelToken::new(),
            jobs::noop_waker(),
        )
        .join()
        .unwrap();
        assert!(idx.is_done());
        assert_eq!(checkpoints(&idx), want);
    }

    #[test]
    fn small_and_chunked_sources_use_one_thread() {
        let data = random_text(3 * BATCH as usize, &Codec::utf8(), 3);
        let (_f, src) = source_with(&data);
        assert_eq!(indexer_threads(&src, src.len()), 1);
        let remote = Source::remote(Box::new(Slow::new(data, u64::MAX, None)), 9 * BATCH, None);
        assert_eq!(indexer_threads(&remote, remote.len()), 1);
    }

    /// A remote file in memory that is slow to read, can cancel the indexer after some reads and
    /// fails from a given offset on.
    struct Slow {
        data: Vec<u8>,
        fail_from: u64,
        cancel_after: Option<(CancelToken, usize)>,
        reads: AtomicUsize,
    }

    impl Slow {
        fn new(data: Vec<u8>, fail_from: u64, cancel_after: Option<(CancelToken, usize)>) -> Slow {
            Slow {
                data,
                fail_from,
                cancel_after,
                reads: AtomicUsize::new(0),
            }
        }
    }

    impl crate::file::RangeRead for Slow {
        fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
            std::thread::sleep(Duration::from_millis(2));
            let n = self.reads.fetch_add(1, Ordering::Relaxed) + 1;
            if let Some((cancel, after)) = &self.cancel_after {
                if n >= *after {
                    cancel.cancel();
                }
            }
            if offset + buf.len() as u64 > self.fail_from {
                return Err(std::io::Error::other("gone"));
            }
            buf.copy_from_slice(&self.data[offset as usize..offset as usize + buf.len()]);
            Ok(())
        }
    }

    #[test]
    fn parallel_index_stops_when_cancelled() {
        let codec = Codec::utf8();
        let data = random_text(200 * BLOCK as usize, &codec, 11);
        let want = naive_checkpoints(&data, &codec);
        let len = data.len() as u64;
        for threads in [1, 4] {
            // Before the start: nothing is read.
            let cancel = CancelToken::new();
            cancel.cancel();
            let src = Source::remote(Box::new(Slow::new(data.clone(), u64::MAX, None)), len, None);
            let idx = LineIndex::new(src.len());
            run_indexer(
                &src,
                codec,
                &idx,
                &cancel,
                &jobs::noop_waker(),
                threads,
                BLOCK,
            );
            assert_eq!(checkpoints(&idx), vec![0]);
            assert!(!idx.is_done() && !idx.failed() && idx.finished_after().is_none());
            // Part way: stops soon after, with a correct prefix.
            let cancel = CancelToken::new();
            let slow = Slow::new(data.clone(), u64::MAX, Some((cancel.clone(), 10)));
            let src = Source::remote(Box::new(slow), len, None);
            let idx = LineIndex::new(src.len());
            let t = Instant::now();
            run_indexer(
                &src,
                codec,
                &idx,
                &cancel,
                &jobs::noop_waker(),
                threads,
                BLOCK,
            );
            assert!(t.elapsed() < Duration::from_secs(2));
            let got = checkpoints(&idx);
            assert!(!idx.is_done() && !idx.failed(), "threads {threads}");
            assert!(got.len() < 12 + threads, "threads {threads}: {}", got.len());
            assert_eq!(got, want[..got.len()], "threads {threads}");
        }
    }

    #[test]
    fn parallel_index_keeps_the_prefix_before_a_read_error() {
        let codec = Codec::utf8();
        let data = random_text(40 * BLOCK as usize + 5, &codec, 13);
        let want = naive_checkpoints(&data, &codec);
        let fail_from = 23 * BLOCK + 100;
        for threads in [1, 2, 4] {
            let slow = Slow::new(data.clone(), fail_from, None);
            let src = Source::remote(Box::new(slow), data.len() as u64, None);
            let idx = LineIndex::new(src.len());
            run(&src, codec, &idx, threads, 2 * BLOCK);
            assert!(idx.failed() && !idx.is_done(), "threads {threads}");
            // Batches of two blocks: everything before the batch holding `fail_from`.
            assert_eq!(checkpoints(&idx), want[..23], "threads {threads}");
        }
    }
}
