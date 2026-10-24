//! File access: windowed memory mapping when possible, buffered chunked reads otherwise.
//!
//! * The UI thread only calls [`Source::get`] with `blocking = false`. For mapped sources the
//!   bytes are borrowed from a small cache of mapped *views* (never the whole file: on Windows
//!   mapping a view commits page-table memory proportional to its size, ~100 MB for a 50 GB
//!   file, which would make memory O(file size)). For chunked sources it serves from a block
//!   cache and, on a miss, queues the blocks for a background loader and returns `None`.
//! * Workers stream large ranges with [`Source::read_into`] (positional reads into a reused
//!   buffer). On a cached file this is faster than first-touch page faults of a mapping and keeps
//!   the process's private memory flat.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io;
use std::ops::{Deref, Range};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use crate::jobs::{self, Waker};

/// Block size of the chunked-read cache.
pub const CHUNK_BLOCK: u64 = 64 * 1024;
/// Maximum number of cached blocks (64 MB).
const CACHE_BLOCKS: usize = 1024;
/// Reads larger than this bypass the block cache (workers streaming through the file).
const DIRECT_READ_MIN: u64 = 1024 * 1024;
/// Mapped views start every `VIEW_STRIDE` bytes and extend `VIEW_OVERLAP` beyond the next start,
/// so any read of up to `VIEW_OVERLAP` bytes fits inside a single view.
const VIEW_STRIDE: u64 = 256 * 1024 * 1024;
const VIEW_OVERLAP: u64 = 32 * 1024 * 1024;
const MAX_VIEWS: usize = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenMode {
    /// Memory-mapped unless the path is on a network/removable drive or mapping fails.
    Auto,
    /// Always use chunked reads.
    Chunked,
}

/// Bytes read from a [`Source`]: borrowed, owned, or a slice of a cached mapped view.
pub struct Bytes<'a>(Repr<'a>);

enum Repr<'a> {
    Borrowed(&'a [u8]),
    Owned(Vec<u8>),
    View(Arc<memmap2::Mmap>, usize, usize),
}

impl<'a> Bytes<'a> {
    pub fn borrowed(b: &'a [u8]) -> Bytes<'a> {
        Bytes(Repr::Borrowed(b))
    }
    pub fn owned(v: Vec<u8>) -> Bytes<'a> {
        Bytes(Repr::Owned(v))
    }
    pub fn into_owned(self) -> Vec<u8> {
        match self.0 {
            Repr::Owned(v) => v,
            _ => self.to_vec(),
        }
    }
}

impl Deref for Bytes<'_> {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match &self.0 {
            Repr::Borrowed(b) => b,
            Repr::Owned(v) => v,
            Repr::View(m, s, l) => &m[*s..*s + *l],
        }
    }
}

impl AsRef<[u8]> for Bytes<'_> {
    fn as_ref(&self) -> &[u8] {
        self
    }
}

impl std::fmt::Debug for Bytes<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Bytes({} bytes)", self.len())
    }
}

impl PartialEq for Bytes<'_> {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}

/// Lazily mapped, LRU-cached views of a file.
struct Mapped {
    file: File,
    len: u64,
    views: Mutex<Vec<(u64, Arc<memmap2::Mmap>, u64)>>,
    tick: AtomicU64,
}

impl Mapped {
    fn view(&self, k: u64) -> io::Result<Arc<memmap2::Mmap>> {
        let tick = self.tick.fetch_add(1, Ordering::Relaxed);
        let mut views = self.views.lock().unwrap();
        if let Some(v) = views.iter_mut().find(|v| v.0 == k) {
            v.2 = tick;
            return Ok(v.1.clone());
        }
        let start = k * VIEW_STRIDE;
        let len = (self.len - start).min(VIEW_STRIDE + VIEW_OVERLAP);
        // SAFETY: read-only mapping. Another process truncating the file while it is mapped is
        // not possible on Windows (ERROR_USER_MAPPED_FILE); on Unix it is a known limitation of
        // every mmap-based viewer.
        let map = Arc::new(unsafe { memmap2::MmapOptions::new().offset(start).len(len as usize).map(&self.file)? });
        if views.len() >= MAX_VIEWS {
            if let Some(i) = views.iter().enumerate().min_by_key(|(_, v)| v.2).map(|(i, _)| i) {
                views.swap_remove(i);
            }
        }
        views.push((k, map.clone(), tick));
        Ok(map)
    }

    fn read(&self, r: Range<u64>) -> io::Result<Bytes<'static>> {
        let k = r.start / VIEW_STRIDE;
        let vstart = k * VIEW_STRIDE;
        if r.end <= vstart + VIEW_STRIDE + VIEW_OVERLAP {
            let v = self.view(k)?;
            Ok(Bytes(Repr::View(v, (r.start - vstart) as usize, (r.end - r.start) as usize)))
        } else {
            let mut buf = vec![0u8; (r.end - r.start) as usize];
            read_exact_at(&self.file, &mut buf, r.start)?;
            Ok(Bytes::owned(buf))
        }
    }
}

enum Backing {
    Empty,
    Mapped(Mapped),
    Chunked(Chunked),
}

/// An immutable view of a file's bytes as they were when it was opened.
pub struct Source {
    path: Option<PathBuf>,
    len: u64,
    backing: Backing,
    modified: Option<SystemTime>,
}

impl Source {
    /// An empty in-memory source (new, untitled documents).
    pub fn empty() -> Source {
        Source { path: None, len: 0, backing: Backing::Empty, modified: None }
    }

    pub fn open(path: &Path, mode: OpenMode) -> io::Result<Source> {
        let file = File::open(path)?;
        let meta = file.metadata()?;
        if meta.is_dir() {
            return Err(io::Error::other("path is a directory"));
        }
        let len = meta.len();
        let modified = meta.modified().ok();
        let path_buf = Some(path.to_path_buf());
        if len == 0 {
            return Ok(Source { path: path_buf, len, backing: Backing::Empty, modified });
        }
        let force_chunked = mode == OpenMode::Chunked
            || std::env::var_os("BIGTEXT_FORCE_CHUNKED").is_some()
            || prefers_chunked(path);
        if !force_chunked {
            let m = Mapped { file, len, views: Mutex::new(Vec::new()), tick: AtomicU64::new(0) };
            // Map the first view now so that a file which cannot be mapped falls back here.
            match m.view(0) {
                Ok(_) => return Ok(Source { path: path_buf, len, backing: Backing::Mapped(m), modified }),
                Err(_) => {
                    return Ok(Source { path: path_buf, len, backing: Backing::Chunked(Chunked::new(m.file, len)), modified })
                }
            }
        }
        Ok(Source { path: path_buf, len, backing: Backing::Chunked(Chunked::new(file, len)), modified })
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn modified(&self) -> Option<SystemTime> {
        self.modified
    }

    pub fn kind_name(&self) -> &'static str {
        match self.backing {
            Backing::Empty => "empty",
            Backing::Mapped(_) => "mmap",
            Backing::Chunked(_) => "chunked",
        }
    }

    pub fn is_mmap(&self) -> bool {
        matches!(self.backing, Backing::Mapped(_))
    }

    /// Registers the function used to wake the UI when queued chunked reads complete.
    pub fn set_waker(&self, waker: Waker) {
        if let Backing::Chunked(c) = &self.backing {
            *c.inner.waker.lock().unwrap() = Some(waker);
        }
    }

    /// True if a chunked read failed (file vanished, network error, ...).
    pub fn has_io_error(&self) -> bool {
        match &self.backing {
            Backing::Chunked(c) => c.inner.failed.load(Ordering::Relaxed),
            _ => false,
        }
    }

    fn clamp(&self, r: Range<u64>) -> Range<u64> {
        let end = r.end.min(self.len);
        r.start.min(end)..end
    }

    /// Blocking read (worker threads).
    pub fn read(&self, r: Range<u64>) -> io::Result<Bytes<'_>> {
        let r = self.clamp(r);
        if r.is_empty() {
            return Ok(Bytes::borrowed(&[]));
        }
        match &self.backing {
            Backing::Empty => Ok(Bytes::borrowed(&[])),
            Backing::Mapped(m) => m.read(r),
            Backing::Chunked(c) => c.read_blocking(r).map(Bytes::owned),
        }
    }

    /// Non-blocking read for the UI thread. `None` means "not available yet" (queued) or failed.
    pub fn try_read(&self, r: Range<u64>) -> Option<Bytes<'_>> {
        let r = self.clamp(r);
        if r.is_empty() {
            return Some(Bytes::borrowed(&[]));
        }
        match &self.backing {
            Backing::Empty => Some(Bytes::borrowed(&[])),
            Backing::Mapped(m) => m.read(r).ok(),
            Backing::Chunked(c) => c.try_read(r).map(Bytes::owned),
        }
    }

    /// Positional bulk read into a caller buffer (workers streaming through the file).
    pub fn read_into(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        if offset + buf.len() as u64 > self.len {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "read past end of source"));
        }
        match &self.backing {
            Backing::Empty => Ok(()),
            Backing::Mapped(m) => read_exact_at(&m.file, buf, offset),
            Backing::Chunked(c) => read_exact_at(&c.inner.file, buf, offset),
        }
    }

    /// Read with the given blocking behaviour; I/O errors map to `None`.
    pub fn get(&self, r: Range<u64>, blocking: bool) -> Option<Bytes<'_>> {
        if blocking {
            self.read(r).ok()
        } else {
            self.try_read(r)
        }
    }

    /// Number of currently mapped views (diagnostics).
    pub fn mapped_views(&self) -> usize {
        match &self.backing {
            Backing::Mapped(m) => m.views.lock().unwrap().len(),
            _ => 0,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Chunked (buffered) reads with a block cache and a background loader thread.

struct BlockCache {
    map: HashMap<u64, (Arc<Vec<u8>>, u64)>,
    tick: u64,
}

impl BlockCache {
    fn get(&mut self, b: u64) -> Option<Arc<Vec<u8>>> {
        self.tick += 1;
        let tick = self.tick;
        self.map.get_mut(&b).map(|e| {
            e.1 = tick;
            e.0.clone()
        })
    }
    fn insert(&mut self, b: u64, data: Arc<Vec<u8>>) {
        self.tick += 1;
        if self.map.len() >= CACHE_BLOCKS {
            if let Some((&oldest, _)) = self.map.iter().min_by_key(|(_, e)| e.1) {
                self.map.remove(&oldest);
            }
        }
        self.map.insert(b, (data, self.tick));
    }
}

struct ChunkedInner {
    file: File,
    len: u64,
    cache: Mutex<BlockCache>,
    pending: Mutex<HashSet<u64>>,
    waker: Mutex<Option<Waker>>,
    failed: AtomicBool,
    reads: AtomicU64,
}

impl ChunkedInner {
    fn block_range(&self, b: u64) -> Range<u64> {
        let s = b * CHUNK_BLOCK;
        s..(s + CHUNK_BLOCK).min(self.len)
    }

    fn load_block(&self, b: u64) -> io::Result<Arc<Vec<u8>>> {
        let r = self.block_range(b);
        let mut buf = vec![0u8; (r.end - r.start) as usize];
        read_exact_at(&self.file, &mut buf, r.start)?;
        self.reads.fetch_add(1, Ordering::Relaxed);
        let data = Arc::new(buf);
        self.cache.lock().unwrap().insert(b, data.clone());
        Ok(data)
    }
}

struct Chunked {
    inner: Arc<ChunkedInner>,
    tx: Sender<u64>,
}

impl Chunked {
    fn new(file: File, len: u64) -> Chunked {
        let inner = Arc::new(ChunkedInner {
            file,
            len,
            cache: Mutex::new(BlockCache { map: HashMap::new(), tick: 0 }),
            pending: Mutex::new(HashSet::new()),
            waker: Mutex::new(None),
            failed: AtomicBool::new(false),
            reads: AtomicU64::new(0),
        });
        let (tx, rx) = mpsc::channel::<u64>();
        let worker = inner.clone();
        jobs::spawn("chunk-loader", move || loader_loop(worker, rx));
        Chunked { inner, tx }
    }

    fn read_blocking(&self, r: Range<u64>) -> io::Result<Vec<u8>> {
        let n = (r.end - r.start) as usize;
        if r.end - r.start >= DIRECT_READ_MIN {
            let mut buf = vec![0u8; n];
            read_exact_at(&self.inner.file, &mut buf, r.start).inspect_err(|_| {
                self.inner.failed.store(true, Ordering::Relaxed);
            })?;
            return Ok(buf);
        }
        let mut out = Vec::with_capacity(n);
        if n == 0 {
            return Ok(out);
        }
        let first = r.start / CHUNK_BLOCK;
        let last = (r.end - 1) / CHUNK_BLOCK;
        for b in first..=last {
            let cached = self.inner.cache.lock().unwrap().get(b);
            let data = match cached {
                Some(d) => d,
                None => self.inner.load_block(b).inspect_err(|_| {
                    self.inner.failed.store(true, Ordering::Relaxed);
                })?,
            };
            let br = self.inner.block_range(b);
            let s = r.start.max(br.start) - br.start;
            let e = r.end.min(br.end) - br.start;
            out.extend_from_slice(&data[s as usize..e as usize]);
        }
        Ok(out)
    }

    fn try_read(&self, r: Range<u64>) -> Option<Vec<u8>> {
        let n = (r.end - r.start) as usize;
        if n == 0 {
            return Some(Vec::new());
        }
        let first = r.start / CHUNK_BLOCK;
        let last = (r.end - 1) / CHUNK_BLOCK;
        let mut blocks = Vec::with_capacity((last - first + 1) as usize);
        let mut missing = Vec::new();
        {
            let mut cache = self.inner.cache.lock().unwrap();
            for b in first..=last {
                match cache.get(b) {
                    Some(d) => blocks.push(d),
                    None => missing.push(b),
                }
            }
        }
        if !missing.is_empty() {
            let mut pending = self.inner.pending.lock().unwrap();
            for b in missing {
                if pending.insert(b) {
                    let _ = self.tx.send(b);
                }
            }
            return None;
        }
        let mut out = Vec::with_capacity(n);
        for (i, data) in blocks.iter().enumerate() {
            let br = self.inner.block_range(first + i as u64);
            let s = r.start.max(br.start) - br.start;
            let e = r.end.min(br.end) - br.start;
            out.extend_from_slice(&data[s as usize..e as usize]);
        }
        Some(out)
    }
}

fn loader_loop(inner: Arc<ChunkedInner>, rx: Receiver<u64>) {
    // Exits when the owning `Source` is dropped (sender closed), releasing the file handle.
    while let Ok(b) = rx.recv() {
        let mut batch = vec![b];
        while let Ok(more) = rx.try_recv() {
            batch.push(more);
        }
        batch.sort_unstable();
        for b in batch {
            let already = inner.cache.lock().unwrap().map.contains_key(&b);
            if !already && inner.load_block(b).is_err() {
                inner.failed.store(true, Ordering::Relaxed);
            }
            inner.pending.lock().unwrap().remove(&b);
        }
        if let Some(w) = inner.waker.lock().unwrap().as_ref() {
            w();
        }
    }
}

/// Positional read that fills `buf` completely.
pub fn read_exact_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        file.read_exact_at(buf, offset)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        let mut done = 0usize;
        while done < buf.len() {
            let n = file.seek_read(&mut buf[done..], offset + done as u64)?;
            if n == 0 {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "file shrank while open"));
            }
            done += n;
        }
        Ok(())
    }
}

/// Network, removable and optical drives are read in chunks instead of mapped, so that a slow
/// or disappearing device can never stall the UI thread inside a page fault.
#[cfg(windows)]
fn prefers_chunked(path: &Path) -> bool {
    let s = path.to_string_lossy();
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s);
    if s.starts_with(r"\\") || s.starts_with(r"UNC\") {
        return true;
    }
    let b = s.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        let root: Vec<u16> = format!("{}:\\", b[0] as char).encode_utf16().chain(Some(0)).collect();
        // SAFETY: `root` is a valid NUL-terminated UTF-16 string.
        let t = unsafe { GetDriveTypeW(root.as_ptr()) };
        // DRIVE_REMOVABLE = 2, DRIVE_REMOTE = 4, DRIVE_CDROM = 5
        return matches!(t, 2 | 4 | 5);
    }
    false
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn GetDriveTypeW(root: *const u16) -> u32;
}

#[cfg(not(windows))]
fn prefers_chunked(_path: &Path) -> bool {
    false
}

/// Strips the `\\?\` verbatim prefix for display.
pub fn display_path(path: &Path) -> String {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        s.into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn mmap_and_chunked_agree() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        let data: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        f.write_all(&data).unwrap();
        f.flush().unwrap();
        let a = Source::open(f.path(), OpenMode::Auto).unwrap();
        let b = Source::open(f.path(), OpenMode::Chunked).unwrap();
        assert_eq!(b.kind_name(), "chunked");
        for r in [0..10u64, 65_530..65_600, 100_000..250_000, 299_990..400_000] {
            let x = a.read(r.clone()).unwrap();
            let y = b.read(r.clone()).unwrap();
            assert_eq!(x, y);
        }
        // Non-blocking read eventually succeeds once the loader fills the cache.
        let mut got = None;
        for _ in 0..200 {
            if let Some(v) = b.try_read(70_000..140_000) {
                got = Some(v.into_owned());
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(got.unwrap(), &data[70_000..140_000]);
    }

    #[test]
    fn empty_file() {
        let f = tempfile::NamedTempFile::new().unwrap();
        let s = Source::open(f.path(), OpenMode::Auto).unwrap();
        assert_eq!(s.len(), 0);
        assert_eq!(s.read(0..10).unwrap().len(), 0);
    }
}
