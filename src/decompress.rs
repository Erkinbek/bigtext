//! Compressed files (`.gz`, `.zst`): rotated logs are almost always compressed. They are
//! unpacked once, streaming, into a temporary file that is then opened like any other file,
//! so the editor keeps its random access and bounded memory.

use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::jobs::{CancelToken, Progress};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Gzip,
    Zstd,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Gzip => "gzip",
            Kind::Zstd => "zstd",
        }
    }
}

const EXTENSIONS: [(&str, Kind); 5] = [("gz", Kind::Gzip), ("gzip", Kind::Gzip), ("tgz", Kind::Gzip), ("zst", Kind::Zstd), ("zstd", Kind::Zstd)];

/// The compression of `path`, judged by its extension and confirmed by its magic bytes.
pub fn detect(path: &Path) -> Option<Kind> {
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    let kind = EXTENSIONS.iter().find(|(e, _)| *e == ext)?.1;
    let mut magic = [0u8; 4];
    let mut f = File::open(path).ok()?;
    let n = f.read(&mut magic).ok()?;
    let ok = match kind {
        Kind::Gzip => n >= 2 && magic[..2] == [0x1F, 0x8B],
        Kind::Zstd => n >= 4 && magic == [0x28, 0xB5, 0x2F, 0xFD],
    };
    ok.then_some(kind)
}

/// The name of the unpacked file: `app.log.gz` → `app.log`.
pub fn inner_name(path: &Path) -> String {
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "unpacked".into());
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("tgz")) {
        format!("{stem}.tar")
    } else {
        stem
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
    let mut input = BufReader::with_capacity(1 << 20, Counting { inner: file, progress, cancel });
    let mut out = Output { inner: out, failed: false };
    let out = &mut out;
    let r = match kind {
        Kind::Gzip => io::copy(&mut flate2::bufread::MultiGzDecoder::new(input), out),
        Kind::Zstd => {
            // A file can hold several frames back to back (e.g. appended by a log rotator).
            let mut total = 0u64;
            loop {
                if input.fill_buf()?.is_empty() {
                    break Ok(total);
                }
                let mut dec = ruzstd::decoding::StreamingDecoder::new(&mut input).map_err(io::Error::other)?;
                match io::copy(&mut dec, out) {
                    Ok(n) => total += n,
                    Err(e) => break Err(e),
                }
            }
        }
    };
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
        assert_eq!(inner_name(Path::new("/x/backup.tgz")), "backup.tar");
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
