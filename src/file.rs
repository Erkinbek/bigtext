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
/// Block size for a remote file: every request costs a round trip, so fewer, larger ones.
pub const REMOTE_BLOCK: u64 = 256 * 1024;
/// Bytes the block cache keeps (64 MB).
const CACHE_BYTES: u64 = 64 * 1024 * 1024;
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

/// Positional reads of a file's bytes: a local file, or a remote one fetched in parts (HTTP
/// ranges, an SSH session).
pub trait RangeRead: Send + Sync {
    /// Fills `buf` with the bytes from `offset`.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()>;
}

impl RangeRead for File {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        read_exact_at(self, buf, offset)
    }
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
                    return Ok(Source { path: path_buf, len, backing: Backing::Chunked(Chunked::new(Box::new(m.file), len, CHUNK_BLOCK, false)), modified })
                }
            }
        }
        Ok(Source { path: path_buf, len, backing: Backing::Chunked(Chunked::new(Box::new(file), len, CHUNK_BLOCK, false)), modified })
    }

    /// A remote file of `len` bytes, read in parts through `reader` as they are needed (what is
    /// on screen, what a search reaches); nothing is downloaded up front.
    pub fn remote(reader: Box<dyn RangeRead>, len: u64, modified: Option<SystemTime>) -> Source {
        let backing = if len == 0 { Backing::Empty } else { Backing::Chunked(Chunked::new(reader, len, REMOTE_BLOCK, true)) };
        Source { path: None, len, backing, modified }
    }

    /// A remote file read in parts ([`Source::remote`]): whole-file scans such as the line
    /// index would download all of it, so they do not start on their own.
    pub fn is_remote(&self) -> bool {
        matches!(&self.backing, Backing::Chunked(c) if c.inner.remote)
    }

    /// Bytes read from the file so far (for a remote file: downloaded).
    pub fn bytes_read(&self) -> u64 {
        match &self.backing {
            Backing::Chunked(c) => c.inner.bytes_read.load(Ordering::Relaxed),
            _ => 0,
        }
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
            Backing::Chunked(c) => c.inner.read_direct(offset, buf),
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
    fn insert(&mut self, b: u64, data: Arc<Vec<u8>>, capacity: usize) {
        self.tick += 1;
        if self.map.len() >= capacity {
            if let Some((&oldest, _)) = self.map.iter().min_by_key(|(_, e)| e.1) {
                self.map.remove(&oldest);
            }
        }
        self.map.insert(b, (data, self.tick));
    }
}

struct ChunkedInner {
    reader: Box<dyn RangeRead>,
    len: u64,
    /// Block size of the cache ([`CHUNK_BLOCK`], [`REMOTE_BLOCK`]).
    block: u64,
    /// Blocks the cache keeps.
    capacity: usize,
    remote: bool,
    cache: Mutex<BlockCache>,
    pending: Mutex<HashSet<u64>>,
    waker: Mutex<Option<Waker>>,
    failed: AtomicBool,
    reads: AtomicU64,
    bytes_read: AtomicU64,
}

impl ChunkedInner {
    fn block_range(&self, b: u64) -> Range<u64> {
        let s = b * self.block;
        s..(s + self.block).min(self.len)
    }

    /// Reads `buf` from `offset` past the cache (large reads of workers streaming the file).
    fn read_direct(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.reader.read_at(offset, buf)?;
        self.bytes_read.fetch_add(buf.len() as u64, Ordering::Relaxed);
        Ok(())
    }

    fn load_block(&self, b: u64) -> io::Result<Arc<Vec<u8>>> {
        self.load_blocks(b, b).map(|mut v| v.remove(0))
    }

    /// Loads blocks `first..=last` with one read (one request for a remote file) and caches
    /// them.
    fn load_blocks(&self, first: u64, last: u64) -> io::Result<Vec<Arc<Vec<u8>>>> {
        let start = self.block_range(first).start;
        let end = self.block_range(last).end;
        let mut buf = vec![0u8; (end - start) as usize];
        self.read_direct(start, &mut buf)?;
        self.reads.fetch_add(1, Ordering::Relaxed);
        let mut cache = self.cache.lock().unwrap();
        let mut out = Vec::with_capacity((last - first + 1) as usize);
        for b in first..=last {
            let r = self.block_range(b);
            let data = Arc::new(buf[(r.start - start) as usize..(r.end - start) as usize].to_vec());
            cache.insert(b, data.clone(), self.capacity);
            out.push(data);
        }
        Ok(out)
    }
}

struct Chunked {
    inner: Arc<ChunkedInner>,
    tx: Sender<u64>,
}

impl Chunked {
    fn new(reader: Box<dyn RangeRead>, len: u64, block: u64, remote: bool) -> Chunked {
        let inner = Arc::new(ChunkedInner {
            reader,
            len,
            block,
            capacity: (CACHE_BYTES / block) as usize,
            remote,
            cache: Mutex::new(BlockCache { map: HashMap::new(), tick: 0 }),
            pending: Mutex::new(HashSet::new()),
            waker: Mutex::new(None),
            failed: AtomicBool::new(false),
            reads: AtomicU64::new(0),
            bytes_read: AtomicU64::new(0),
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
            self.inner.read_direct(r.start, &mut buf).inspect_err(|_| {
                self.inner.failed.store(true, Ordering::Relaxed);
            })?;
            return Ok(buf);
        }
        let mut out = Vec::with_capacity(n);
        if n == 0 {
            return Ok(out);
        }
        let first = r.start / self.inner.block;
        let last = (r.end - 1) / self.inner.block;
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
        let first = r.start / self.inner.block;
        let last = (r.end - 1) / self.inner.block;
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
        batch.dedup();
        let asked = batch.clone();
        {
            let cache = inner.cache.lock().unwrap();
            batch.retain(|b| !cache.map.contains_key(b));
        }
        // Runs of neighbouring blocks load with one read: one request instead of many for a
        // remote file (a screen that straddles blocks, a jump that needs two).
        let mut i = 0;
        while i < batch.len() {
            let mut j = i;
            while j + 1 < batch.len() && batch[j + 1] == batch[j] + 1 && j - i < 15 {
                j += 1;
            }
            if inner.load_blocks(batch[i], batch[j]).is_err() {
                inner.failed.store(true, Ordering::Relaxed);
            }
            i = j + 1;
        }
        {
            let mut pending = inner.pending.lock().unwrap();
            for b in &asked {
                pending.remove(b);
            }
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
    fn GetFileInformationByHandle(file: *mut std::ffi::c_void, info: *mut FileInformation) -> i32;
}

/// BY_HANDLE_FILE_INFORMATION.
#[cfg(windows)]
#[repr(C)]
#[derive(Default)]
struct FileInformation {
    attributes: u32,
    times: [u32; 6],
    volume_serial: u32,
    size_high: u32,
    size_low: u32,
    links: u32,
    index_high: u32,
    index_low: u32,
}

/// Which file `path` names (volume and file number), to notice when a path comes to name
/// another file, as when a log is rotated. Unlike the creation time it is not fooled by NTFS
/// tunnelling. `None` when it cannot be read.
pub fn file_id(path: &Path) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::metadata(path).ok()?;
        Some((m.dev(), m.ino()))
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        // Opened sharing read, write and delete, so the writer is never in the way.
        let f = File::open(path).ok()?;
        let mut info = FileInformation::default();
        // SAFETY: the handle is open for the call and `info` is a BY_HANDLE_FILE_INFORMATION.
        let ok = unsafe { GetFileInformationByHandle(f.as_raw_handle(), &mut info) };
        (ok != 0).then_some((info.volume_serial as u64, ((info.index_high as u64) << 32) | info.index_low as u64))
    }
}

/// The file a log was most likely rotated to: a sibling whose name starts like this one's
/// (`app.log.1`, `app-20261028.log.gz` for `app.log`), changed in the two minutes before
/// `now`, the newest. A hint for the user, never opened by itself.
pub fn rotated_sibling(path: &Path, now: std::time::SystemTime) -> Option<PathBuf> {
    let dir = path.parent()?;
    let name = path.file_name()?.to_string_lossy().into_owned();
    let stem = path.file_stem()?.to_string_lossy().into_owned();
    let since = now - std::time::Duration::from_secs(120);
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n != name && n.starts_with(&stem)
        })
        .filter_map(|e| {
            let m = e.metadata().ok().filter(|m| m.is_file())?;
            let t = m.modified().ok().filter(|t| *t >= since)?;
            Some((t, e.path()))
        })
        .max_by_key(|x| x.0)
        .map(|x| x.1)
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
    fn file_ids_and_rotated_siblings() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("app.log");
        std::fs::write(&log, "one\n").unwrap();
        let first = file_id(&log).expect("an id");
        assert_eq!(file_id(&log), Some(first));
        // Rotation by rename: the path now names a new file.
        std::fs::rename(&log, dir.path().join("app.log.1")).unwrap();
        std::fs::write(&log, "two\n").unwrap();
        assert_ne!(file_id(&log), Some(first));
        assert_eq!(file_id(&dir.path().join("app.log.1")), Some(first));
        std::fs::write(dir.path().join("other.txt"), "x").unwrap();
        let old = dir.path().join("app-2020.log");
        std::fs::write(&old, "old").unwrap();
        File::options().write(true).open(&old).unwrap().set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3600)).unwrap();
        assert_eq!(rotated_sibling(&log, std::time::SystemTime::now()), Some(dir.path().join("app.log.1")));
        assert_eq!(file_id(&dir.path().join("missing.log")), None);
    }

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

    /// A remote file in memory that counts the requests made to it.
    struct Counting(Vec<u8>, Arc<Mutex<Vec<Range<u64>>>>);

    impl RangeRead for Counting {
        fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
            self.1.lock().unwrap().push(offset..offset + buf.len() as u64);
            buf.copy_from_slice(&self.0[offset as usize..offset as usize + buf.len()]);
            Ok(())
        }
    }

    #[test]
    fn a_remote_file_is_read_only_where_asked() {
        let data: Vec<u8> = (0..5_000_000u32).map(|i| (i % 253) as u8).collect();
        let asked = Arc::new(Mutex::new(Vec::new()));
        let s = Source::remote(Box::new(Counting(data.clone(), asked.clone())), data.len() as u64, None);
        assert!(s.is_remote() && s.kind_name() == "chunked" && s.path().is_none());
        // The UI asks without blocking: queued, then served from the cache.
        let mid = 2_600_000u64;
        let mut got = None;
        for _ in 0..400 {
            if let Some(v) = s.try_read(mid..mid + 4000) {
                got = Some(v.into_owned());
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(got.unwrap(), &data[mid as usize..mid as usize + 4000]);
        // One block around the middle was fetched, nothing else.
        assert_eq!(asked.lock().unwrap().len(), 1);
        assert_eq!(s.bytes_read(), REMOTE_BLOCK);
        // A read across two blocks fetches both with one request.
        asked.lock().unwrap().clear();
        let edge = 3 * REMOTE_BLOCK - 100;
        assert_eq!(s.read(edge..edge + 200).unwrap().into_owned(), &data[edge as usize..edge as usize + 200]);
        assert_eq!(s.read(edge..edge + 200).unwrap().len(), 200);
        assert_eq!(asked.lock().unwrap().len(), 2, "two blocks the first time, none the second");
        // Workers' large reads go straight to the reader.
        let mut buf = vec![0u8; 2 << 20];
        s.read_into(1000, &mut buf).unwrap();
        assert_eq!(&buf[..], &data[1000..1000 + (2 << 20)]);
        assert!(s.bytes_read() < data.len() as u64);
        assert!(!Source::open(&std::env::current_exe().unwrap(), OpenMode::Chunked).unwrap().is_remote());
    }

    #[test]
    fn neighbouring_blocks_load_with_one_request() {
        let data = vec![7u8; 3 << 20];
        let asked = Arc::new(Mutex::new(Vec::new()));
        let s = Source::remote(Box::new(Counting(data, asked.clone())), 3 << 20, None);
        // A screen that straddles four blocks.
        let r = REMOTE_BLOCK / 2..REMOTE_BLOCK * 3 + 10;
        for _ in 0..400 {
            if s.try_read(r.clone()).is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let asked = asked.lock().unwrap();
        assert_eq!((asked.len(), asked.first().cloned()), (1, Some(0..REMOTE_BLOCK * 4)));
    }

    #[test]
    fn empty_file() {
        let f = tempfile::NamedTempFile::new().unwrap();
        let s = Source::open(f.path(), OpenMode::Auto).unwrap();
        assert_eq!(s.len(), 0);
        assert_eq!(s.read(0..10).unwrap().len(), 0);
    }
}
