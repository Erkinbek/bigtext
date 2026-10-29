//! Compressed files (`.gz`, `.zst`, `.xz`, `.bz2`): rotated logs are almost always compressed.
//! They are unpacked once, streaming, into a temporary file that is then opened like any other
//! file, so the editor keeps its random access and bounded memory. Archives (`.zip`, `.tar`,
//! `.tar.gz` and the like) list their files, and the one picked is unpacked the same way.

use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::jobs::{CancelToken, Progress};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Gzip,
    Zstd,
    Xz,
    Bzip2,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Gzip => "gzip",
            Kind::Zstd => "zstd",
            Kind::Xz => "xz",
            Kind::Bzip2 => "bzip2",
        }
    }

    /// The usual file extension.
    pub fn extension(self) -> &'static str {
        match self {
            Kind::Gzip => "gz",
            Kind::Zstd => "zst",
            Kind::Xz => "xz",
            Kind::Bzip2 => "bz2",
        }
    }

    pub const ALL: [Kind; 4] = [Kind::Gzip, Kind::Zstd, Kind::Xz, Kind::Bzip2];

    fn magic_ok(self, m: &[u8]) -> bool {
        match self {
            Kind::Gzip => m.starts_with(&[0x1F, 0x8B]),
            Kind::Zstd => m.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]),
            Kind::Xz => m.starts_with(&[0xFD, b'7', b'z', b'X', b'Z', 0]),
            Kind::Bzip2 => m.starts_with(b"BZh"),
        }
    }

    /// A reader of the unpacked bytes of `input`.
    fn decoder<'a, R: BufRead + 'a>(self, input: R) -> io::Result<Box<dyn Read + 'a>> {
        Ok(match self {
            Kind::Gzip => Box::new(flate2::bufread::MultiGzDecoder::new(input)),
            Kind::Zstd => Box::new(ZstdFrames { input: Some(input), frame: None }),
            // Dictionaries up to 1 GB (xz -9 uses 64 MB).
            Kind::Xz => Box::new(lzma_rust2::XzReader::new_mem_limit(input, true, 1 << 20)),
            Kind::Bzip2 => Box::new(bzip2::bufread::MultiBzDecoder::new(input)),
        })
    }
}

/// zstd frames back to back (as a log rotator appends them), read as one stream.
struct ZstdFrames<R: BufRead> {
    /// The input between frames; a frame's decoder owns it while it is read.
    input: Option<R>,
    frame: Option<ruzstd::decoding::StreamingDecoder<R, ruzstd::decoding::FrameDecoder>>,
}

impl<R: BufRead> Read for ZstdFrames<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            if let Some(f) = &mut self.frame {
                let n = f.read(buf)?;
                if n > 0 || buf.is_empty() {
                    return Ok(n);
                }
                // The frame is done: take the input back for the next one.
                self.input = self.frame.take().map(|f| f.into_inner());
            }
            let Some(mut input) = self.input.take() else { return Ok(0) };
            if input.fill_buf()?.is_empty() {
                self.input = Some(input);
                return Ok(0);
            }
            self.frame = Some(ruzstd::decoding::StreamingDecoder::new(input).map_err(io::Error::other)?);
        }
    }
}

const EXTENSIONS: [(&str, Kind); 7] = [("gz", Kind::Gzip), ("gzip", Kind::Gzip), ("zst", Kind::Zstd), ("zstd", Kind::Zstd), ("xz", Kind::Xz), ("bz2", Kind::Bzip2), ("bzip2", Kind::Bzip2)];

fn head(path: &Path, n: usize) -> Option<Vec<u8>> {
    let mut b = vec![0u8; n];
    let mut f = File::open(path).ok()?;
    let mut got = 0;
    while got < n {
        match f.read(&mut b[got..]) {
            Ok(0) => break,
            Ok(k) => got += k,
            Err(_) => return None,
        }
    }
    b.truncate(got);
    Some(b)
}

/// The compression of `path`, judged by its extension and confirmed by its magic bytes.
/// Compressed tar files are archives ([`detect_archive`]).
pub fn detect(path: &Path) -> Option<Kind> {
    if detect_archive(path).is_some() {
        return None;
    }
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    let kind = EXTENSIONS.iter().find(|(e, _)| *e == ext)?.1;
    kind.magic_ok(&head(path, 8)?).then_some(kind)
}

/// The name of the unpacked file: `app.log.gz` → `app.log`.
pub fn inner_name(path: &Path) -> String {
    path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "unpacked".into())
}

/// A file of files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Archive {
    Zip,
    /// A tar file, compressed as a whole or not.
    Tar(Option<Kind>),
}

const TARS: [(&str, Option<Kind>); 10] = [
    (".tar", None),
    (".tar.gz", Some(Kind::Gzip)),
    (".tgz", Some(Kind::Gzip)),
    (".tar.zst", Some(Kind::Zstd)),
    (".tzst", Some(Kind::Zstd)),
    (".tar.xz", Some(Kind::Xz)),
    (".txz", Some(Kind::Xz)),
    (".tar.bz2", Some(Kind::Bzip2)),
    (".tbz2", Some(Kind::Bzip2)),
    (".tbz", Some(Kind::Bzip2)),
];

/// Whether `path` is an archive, by its name and confirmed by its first bytes.
pub fn detect_archive(path: &Path) -> Option<Archive> {
    let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
    if name.ends_with(".zip") {
        let h = head(path, 4)?;
        return (h.starts_with(b"PK\x03\x04") || h.starts_with(b"PK\x05\x06")).then_some(Archive::Zip);
    }
    let kind = TARS.iter().find(|(s, _)| name.ends_with(s))?.1;
    match kind {
        None => {
            let h = head(path, 265)?;
            (h.len() >= 262 && &h[257..262] == b"ustar").then_some(Archive::Tar(None))
        }
        Some(k) => k.magic_ok(&head(path, 8)?).then_some(Archive::Tar(Some(k))),
    }
}

/// A file in an archive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    /// Its path inside the archive.
    pub name: String,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

/// Most files listed from one archive.
pub const MEMBERS_MAX: usize = 100_000;

/// Days since 1970-01-01 of a civil date.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn zip_time(t: zip::DateTime) -> Option<SystemTime> {
    let days = days_from_civil(t.year() as i64, t.month() as u32, t.day() as u32);
    let secs = days * 86_400 + t.hour() as i64 * 3600 + t.minute() as i64 * 60 + t.second() as i64;
    u64::try_from(secs).ok().map(|s| UNIX_EPOCH + Duration::from_secs(s))
}

/// The input of a tar archive, unpacked as it is read.
fn tar_input<'a>(path: &Path, kind: Option<Kind>, cancel: &'a CancelToken, progress: &'a Progress) -> io::Result<Box<dyn Read + 'a>> {
    let file = File::open(path)?;
    progress.set_total(file.metadata()?.len());
    let input = BufReader::with_capacity(1 << 20, Counting { inner: file, progress, cancel });
    match kind {
        None => Ok(Box::new(input)),
        Some(k) => k.decoder(input),
    }
}

fn damaged(what: &str, e: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("The {what} archive is damaged or uses a kind of compression that cannot be read ({e})."))
}

/// The regular files of an archive (at most [`MEMBERS_MAX`]). `None` when cancelled.
pub fn list_members(path: &Path, archive: Archive, cancel: &CancelToken, progress: &Progress) -> io::Result<Option<Vec<Member>>> {
    let mut out = Vec::new();
    let r = (|| -> io::Result<()> {
        match archive {
            Archive::Zip => {
                let mut z = zip::ZipArchive::new(File::open(path)?).map_err(|e| damaged("zip", e))?;
                progress.set_total(z.len() as u64);
                for i in 0..z.len().min(MEMBERS_MAX) {
                    if cancel.is_cancelled() {
                        return Err(io::Error::other("cancelled"));
                    }
                    let f = z.by_index_raw(i).map_err(|e| damaged("zip", e))?;
                    if f.is_file() {
                        let name = f.name().map_err(|e| damaged("zip", e))?.into_owned();
                        out.push(Member { name, size: f.size(), modified: f.last_modified().and_then(zip_time) });
                    }
                    progress.set_done(i as u64 + 1);
                }
            }
            Archive::Tar(kind) => {
                let mut t = tar::Archive::new(tar_input(path, kind, cancel, progress)?);
                for e in t.entries()? {
                    let e = e?;
                    let h = e.header();
                    if h.entry_type().is_file() {
                        let name = e.path()?.to_string_lossy().into_owned();
                        out.push(Member { name, size: e.size(), modified: h.mtime().ok().map(|s| UNIX_EPOCH + Duration::from_secs(s)) });
                        if out.len() >= MEMBERS_MAX {
                            break;
                        }
                    }
                }
            }
        }
        Ok(())
    })();
    match r {
        Ok(()) => Ok(Some(out)),
        Err(_) if cancel.is_cancelled() => Ok(None),
        Err(e) => Err(e),
    }
}

/// The last part of a member's path, safe as a file name.
pub fn member_file_name(member: &str) -> String {
    let base = member.rsplit(['/', '\\']).find(|s| !s.is_empty()).unwrap_or("");
    let clean: String = base.chars().map(|c| if c.is_control() || "<>:\"|?*".contains(c) { '_' } else { c }).collect();
    if clean.is_empty() || clean == "." || clean == ".." {
        "member".into()
    } else {
        clean
    }
}

/// Unpacks the archive's file `member` into a new temporary file: its path, or `None` when
/// cancelled.
pub fn unpack_member(path: &Path, archive: Archive, member: &str, cancel: &CancelToken, progress: &Progress) -> io::Result<Option<PathBuf>> {
    let target = temp_file(&member_file_name(member));
    if let Some(dir) = target.parent() {
        fs::create_dir_all(dir)?;
    }
    let r = (|| -> io::Result<bool> {
        let mut w = io::BufWriter::with_capacity(8 << 20, File::create(&target)?);
        let found = match archive {
            Archive::Zip => {
                let mut z = zip::ZipArchive::new(File::open(path)?).map_err(|e| damaged("zip", e))?;
                let f = z.by_name(member).map_err(|e| damaged("zip", e))?;
                progress.set_total(f.size());
                io::copy(&mut Counting { inner: f, progress, cancel }, &mut w)?;
                true
            }
            Archive::Tar(kind) => {
                let mut t = tar::Archive::new(tar_input(path, kind, cancel, progress)?);
                let mut found = false;
                for e in t.entries()? {
                    let mut e = e?;
                    if e.header().entry_type().is_file() && e.path()?.to_string_lossy() == member {
                        io::copy(&mut e, &mut w)?;
                        found = true;
                        break;
                    }
                }
                found
            }
        };
        w.flush()?;
        Ok(found)
    })();
    match r {
        Ok(true) => Ok(Some(target)),
        Ok(false) => {
            remove_temp(&target);
            Err(io::Error::new(io::ErrorKind::NotFound, format!("{member} is not in the archive")))
        }
        Err(_) if cancel.is_cancelled() => {
            remove_temp(&target);
            Ok(None)
        }
        Err(e) => {
            remove_temp(&target);
            Err(e)
        }
    }
}

/// Directory for unpacked copies (in the system temp directory).
pub fn temp_dir() -> PathBuf {
    std::env::temp_dir().join("BigText").join("unpacked")
}

/// A fresh path in [`temp_dir`] for unpacking `path`.
pub fn temp_path_for(path: &Path) -> PathBuf {
    temp_file(&inner_name(path))
}

/// A fresh path for a temporary file named `name`, in its own folder in [`temp_dir`] (which
/// [`remove_temp`] deletes with it, and [`cleanup_old`] after a crash).
pub fn temp_file(name: &str) -> PathBuf {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    temp_dir().join(format!("{}-{nanos:x}", std::process::id())).join(name)
}

/// Removes unpacked copies left behind (by a crash) more than two days ago. Files still open
/// in another BigText window cannot be deleted on Windows and are skipped.
pub fn cleanup_old() {
    let Ok(dirs) = fs::read_dir(temp_dir()) else { return };
    let limit = SystemTime::now() - Duration::from_secs(2 * 24 * 3600);
    for d in dirs.flatten() {
        if d.metadata().and_then(|m| m.modified()).is_ok_and(|t| t < limit) {
            let _ = fs::remove_dir_all(d.path());
        }
    }
}

/// Deletes an unpacked copy and its directory, retrying while a worker still maps the file.
pub fn remove_temp(path: &Path) {
    for _ in 0..100 {
        if fs::remove_file(path).is_ok() || !path.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if let Some(dir) = path.parent() {
        let _ = fs::remove_dir(dir);
    }
}

/// The compression an output file gets from its name: `lines.txt.gz` → gzip. `None` for other
/// names (and for archives such as `.tgz`).
pub fn target_kind(path: &Path) -> Option<Kind> {
    let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
    if TARS.iter().any(|(s, _)| name.ends_with(s)) {
        return None;
    }
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    EXTENSIONS.iter().find(|(e, _)| *e == ext).map(|e| e.1)
}

/// zstd output in frames of [`ZSTD_FRAME`] bytes (ruzstd compresses a buffer at a time).
pub struct ZstdPacker<W: Write> {
    inner: W,
    buf: Vec<u8>,
    frames: u64,
}

/// Bytes compressed into one zstd frame.
pub const ZSTD_FRAME: usize = 8 << 20;

impl<W: Write> ZstdPacker<W> {
    fn frame(&mut self) -> io::Result<()> {
        let packed = ruzstd::encoding::compress_to_vec(&self.buf[..], ruzstd::encoding::CompressionLevel::Fastest);
        self.inner.write_all(&packed)?;
        self.buf.clear();
        self.frames += 1;
        Ok(())
    }
}

impl<W: Write> Write for ZstdPacker<W> {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        let n = b.len().min(ZSTD_FRAME - self.buf.len());
        self.buf.extend_from_slice(&b[..n]);
        if self.buf.len() >= ZSTD_FRAME {
            self.frame()?;
        }
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// A writer that compresses what it is given for a `.gz`, `.zst`, `.xz` or `.bz2` file (or
/// passes it through). [`Packer::finish`] must end it.
pub enum Packer<W: Write> {
    Plain(W),
    Gzip(flate2::write::GzEncoder<W>),
    Zstd(ZstdPacker<W>),
    Xz(Box<lzma_rust2::XzWriter<W>>),
    Bzip2(bzip2::write::BzEncoder<W>),
}

impl<W: Write> Packer<W> {
    pub fn new(inner: W, kind: Option<Kind>) -> io::Result<Packer<W>> {
        Ok(match kind {
            None => Packer::Plain(inner),
            Some(Kind::Gzip) => Packer::Gzip(flate2::write::GzEncoder::new(inner, flate2::Compression::default())),
            Some(Kind::Zstd) => Packer::Zstd(ZstdPacker { inner, buf: Vec::new(), frames: 0 }),
            Some(Kind::Xz) => Packer::Xz(Box::new(lzma_rust2::XzWriter::new(inner, lzma_rust2::XzOptions::with_preset(3))?)),
            Some(Kind::Bzip2) => Packer::Bzip2(bzip2::write::BzEncoder::new(inner, bzip2::Compression::default())),
        })
    }

    /// Whether the output is compressed.
    pub fn packs(&self) -> bool {
        !matches!(self, Packer::Plain(_))
    }

    /// Ends the compressed stream and gives the writer back.
    pub fn finish(self) -> io::Result<W> {
        match self {
            Packer::Plain(w) => Ok(w),
            Packer::Gzip(e) => e.finish(),
            Packer::Zstd(mut z) => {
                // An empty input still gets one (empty) frame.
                if !z.buf.is_empty() || z.frames == 0 {
                    z.frame()?;
                }
                Ok(z.inner)
            }
            Packer::Xz(x) => x.finish(),
            Packer::Bzip2(b) => b.finish(),
        }
    }
}

impl<W: Write> Write for Packer<W> {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        match self {
            Packer::Plain(w) => w.write(b),
            Packer::Gzip(e) => e.write(b),
            Packer::Zstd(z) => z.write(b),
            Packer::Xz(x) => x.write(b),
            Packer::Bzip2(e) => e.write(b),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Packer::Plain(w) => w.flush(),
            Packer::Gzip(e) => e.flush(),
            Packer::Zstd(z) => z.flush(),
            Packer::Xz(x) => x.flush(),
            Packer::Bzip2(e) => e.flush(),
        }
    }
}

/// Only a plain output can be written out of order (an Excel file, for one).
impl<W: Write + Seek> Seek for Packer<W> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        match self {
            Packer::Plain(w) => w.seek(pos),
            _ => Err(io::Error::new(io::ErrorKind::Unsupported, "this kind of file cannot be written compressed")),
        }
    }
}

/// Counts the compressed bytes read, for the progress bar, and stops reading when cancelled.
struct Counting<'a, R> {
    inner: R,
    progress: &'a Progress,
    cancel: &'a CancelToken,
}

impl<R: Read> Read for Counting<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // Not `Interrupted`: readers retry that kind forever.
        if self.cancel.is_cancelled() {
            return Err(io::Error::other("cancelled"));
        }
        let n = self.inner.read(buf)?;
        self.progress.add(n as u64);
        Ok(n)
    }
}

/// Remembers whether a failure came from the output (disk full) rather than the decoder.
struct Output<'a, W> {
    inner: &'a mut W,
    failed: bool,
}

impl<W: Write> Write for Output<'_, W> {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.inner.write(b).inspect_err(|_| self.failed = true)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush().inspect_err(|_| self.failed = true)
    }
}

/// Unpacks `src` into `out`. Returns the unpacked size, or `None` when cancelled.
pub fn unpack_to<W: Write>(src: &Path, kind: Kind, out: &mut W, cancel: &CancelToken, progress: &Progress) -> io::Result<Option<u64>> {
    let file = File::open(src)?;
    progress.set_total(file.metadata()?.len());
    let input = BufReader::with_capacity(1 << 20, Counting { inner: file, progress, cancel });
    let mut out = Output { inner: out, failed: false };
    let out = &mut out;
    let r = kind.decoder(input).and_then(|mut d| io::copy(&mut d, out));
    match r {
        Ok(n) => Ok(Some(n)),
        Err(_) if cancel.is_cancelled() => Ok(None),
        Err(e) if out.failed => Err(e),
        Err(e) => Err(io::Error::new(io::ErrorKind::InvalidData, format!("The {} data is damaged or incomplete ({e}).", kind.name()))),
    }
}

/// Unpacks `src` into a new temporary file. Returns its path, or `None` when cancelled (the
/// partial file is removed).
pub fn unpack(src: &Path, kind: Kind, cancel: &CancelToken, progress: &Progress) -> io::Result<Option<PathBuf>> {
    let target = temp_path_for(src);
    if let Some(dir) = target.parent() {
        fs::create_dir_all(dir)?;
    }
    let r = (|| {
        let mut w = io::BufWriter::with_capacity(8 << 20, File::create(&target)?);
        let n = unpack_to(src, kind, &mut w, cancel, progress)?;
        w.flush()?;
        Ok(n)
    })();
    match r {
        Ok(Some(_)) => Ok(Some(target)),
        Ok(None) => {
            remove_temp(&target);
            Ok(None)
        }
        Err(e) => {
            remove_temp(&target);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gz(data: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    fn zst(data: &[u8]) -> Vec<u8> {
        ruzstd::encoding::compress_to_vec(data, ruzstd::encoding::CompressionLevel::Fastest)
    }

    fn roundtrip(name: &str, packed: &[u8]) -> Vec<u8> {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(name);
        fs::write(&p, packed).unwrap();
        let kind = detect(&p).expect("detected");
        let mut out = Vec::new();
        unpack_to(&p, kind, &mut out, &CancelToken::new(), &Progress::default()).unwrap().unwrap();
        out
    }

    #[test]
    fn gzip_and_zstd_with_several_members() {
        let a = b"first part\n".repeat(1000);
        let b = b"second part\n".repeat(1000);
        let mut two_gz = gz(&a);
        two_gz.extend(gz(&b));
        assert_eq!(roundtrip("app.log.gz", &two_gz), [a.clone(), b.clone()].concat());
        let mut two_zst = zst(&a);
        two_zst.extend(zst(&b));
        assert_eq!(roundtrip("app.log.zst", &two_zst), [a, b].concat());
    }

    #[test]
    fn detection_needs_extension_and_magic() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("notes.gz");
        fs::write(&plain, b"not compressed").unwrap();
        assert_eq!(detect(&plain), None);
        let other = dir.path().join("data.bin");
        fs::write(&other, gz(b"x")).unwrap();
        assert_eq!(detect(&other), None);
        assert_eq!(inner_name(Path::new("/x/app.log.gz")), "app.log");
        // A compressed tar is an archive, not a compressed file.
        let tgz = dir.path().join("backup.tgz");
        fs::write(&tgz, gz(b"x")).unwrap();
        assert_eq!((detect(&tgz), detect_archive(&tgz)), (None, Some(Archive::Tar(Some(Kind::Gzip)))));
    }

    fn xz(data: &[u8]) -> Vec<u8> {
        let mut w = lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::with_preset(1)).unwrap();
        w.write_all(data).unwrap();
        w.finish().unwrap()
    }

    fn bz2(data: &[u8]) -> Vec<u8> {
        let mut e = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    #[test]
    fn packing_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let data: Vec<u8> = (0..300_000).flat_map(|i: u32| format!("line {i} of the log\n").into_bytes()).collect();
        for name in ["out.txt.gz", "out.txt.zst", "out.txt.xz", "out.txt.bz2", "empty.zst"] {
            let p = dir.path().join(name);
            let kind = target_kind(&p);
            assert!(kind.is_some(), "{name}");
            let mut w = Packer::new(Vec::new(), kind).unwrap();
            let body: &[u8] = if name.starts_with("empty") { b"" } else { &data };
            // Small writes and one large one.
            w.write_all(&body[..body.len().min(10)]).unwrap();
            w.write_all(&body[body.len().min(10)..]).unwrap();
            assert!(w.packs());
            let packed = w.finish().unwrap();
            assert!(packed.len() < body.len().max(100), "{name}");
            fs::write(&p, &packed).unwrap();
            assert_eq!(detect(&p), kind, "{name}");
            let mut out = Vec::new();
            unpack_to(&p, kind.unwrap(), &mut out, &CancelToken::new(), &Progress::default()).unwrap().unwrap();
            assert!(out == body, "{name}");
        }
        assert_eq!(target_kind(Path::new("lines.txt")), None);
        assert_eq!(target_kind(Path::new("backup.tgz")), None);
        assert_eq!(target_kind(Path::new("logs.tar.gz")), None);
        let mut plain = Packer::new(std::io::Cursor::new(Vec::new()), None).unwrap();
        plain.write_all(b"abc").unwrap();
        assert_eq!(plain.seek(SeekFrom::Start(1)).unwrap(), 1);
        let mut gz = Packer::new(std::io::Cursor::new(Vec::new()), Some(Kind::Gzip)).unwrap();
        assert!(gz.seek(SeekFrom::Start(0)).is_err());
    }

    #[test]
    fn xz_and_bzip2() {
        let a = b"xz and bzip2 line\n".repeat(5000);
        assert_eq!(roundtrip("app.log.xz", &xz(&a)), a);
        let mut two = bz2(&a[..1000]);
        two.extend(bz2(&a[1000..]));
        assert_eq!(roundtrip("app.log.bz2", &two), a);
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("cut.log.xz");
        let packed = xz(&a);
        fs::write(&p, &packed[..packed.len() / 2]).unwrap();
        let err = unpack_to(&p, Kind::Xz, &mut Vec::new(), &CancelToken::new(), &Progress::default()).unwrap_err();
        assert!(err.to_string().contains("xz"), "{err}");
    }

    fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, data) in files {
            z.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            z.write_all(data).unwrap();
        }
        z.add_directory("empty/", zip::write::SimpleFileOptions::default()).unwrap();
        z.finish().unwrap().into_inner()
    }

    fn tar_of(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for (name, data) in files {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_mtime(1_700_000_000);
            h.set_cksum();
            b.append_data(&mut h, name, *data).unwrap();
        }
        b.into_inner().unwrap()
    }

    #[test]
    fn archives_list_and_unpack_members() {
        let dir = tempfile::tempdir().unwrap();
        let files: [(&str, &[u8]); 2] = [("logs/app.log", b"one\ntwo\n"), ("logs/old/app.log.gz", &gz(b"packed\n"))];
        let cases = [("bundle.zip", zip_of(&files), Archive::Zip), ("bundle.tar", tar_of(&files), Archive::Tar(None)), ("bundle.tar.gz", gz(&tar_of(&files)), Archive::Tar(Some(Kind::Gzip))), ("bundle.tar.xz", xz(&tar_of(&files)), Archive::Tar(Some(Kind::Xz)))];
        for (name, bytes, kind) in cases {
            let p = dir.path().join(name);
            fs::write(&p, &bytes).unwrap();
            assert_eq!(detect_archive(&p), Some(kind), "{name}");
            let m = list_members(&p, kind, &CancelToken::new(), &Progress::default()).unwrap().unwrap();
            let names: Vec<&str> = m.iter().map(|m| m.name.as_str()).collect();
            assert_eq!(names, ["logs/app.log", "logs/old/app.log.gz"], "{name}");
            assert_eq!(m[0].size, 8);
            let out = unpack_member(&p, kind, "logs/app.log", &CancelToken::new(), &Progress::default()).unwrap().unwrap();
            assert_eq!((fs::read(&out).unwrap(), out.file_name().unwrap().to_string_lossy().into_owned()), (b"one\ntwo\n".to_vec(), "app.log".to_string()));
            remove_temp(&out);
            // A compressed file inside is unpacked as it is; it is then a .gz like any other.
            let inner = unpack_member(&p, kind, "logs/old/app.log.gz", &CancelToken::new(), &Progress::default()).unwrap().unwrap();
            assert_eq!(detect(&inner), Some(Kind::Gzip));
            remove_temp(&inner);
            assert!(unpack_member(&p, kind, "nope.txt", &CancelToken::new(), &Progress::default()).is_err());
        }
        let t = list_members(&dir.path().join("bundle.tar"), Archive::Tar(None), &CancelToken::new(), &Progress::default()).unwrap().unwrap();
        assert_eq!(t[0].modified, Some(UNIX_EPOCH + Duration::from_secs(1_700_000_000)));
        let plain = dir.path().join("fake.zip");
        fs::write(&plain, b"not a zip").unwrap();
        assert_eq!(detect_archive(&plain), None);
        assert_eq!(member_file_name("a/b\\c:d.log"), "c_d.log");
        assert_eq!(member_file_name("../"), "member");
        assert_eq!(days_from_civil(2024, 2, 29), 19_782);
    }

    #[test]
    fn damaged_data_and_cancel() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("cut.log.gz");
        let packed = gz(&b"some log line\n".repeat(10_000));
        fs::write(&p, &packed[..packed.len() / 2]).unwrap();
        let err = unpack_to(&p, Kind::Gzip, &mut Vec::new(), &CancelToken::new(), &Progress::default()).unwrap_err();
        assert!(err.to_string().contains("gzip"), "{err}");
        fs::write(&p, &packed).unwrap();
        let c = CancelToken::new();
        c.cancel();
        assert_eq!(unpack(&p, Kind::Gzip, &c, &Progress::default()).unwrap(), None);
    }
}
