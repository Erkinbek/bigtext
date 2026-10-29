//! Temporary files for comparisons that do not fit in memory, in a folder of their own in the
//! temp directory; dropping a [`Spill`] deletes the files it made and then the folder.

use std::cell::RefCell;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read};
use std::path::{Path, PathBuf};

pub(crate) struct Spill {
    dir: PathBuf,
    made: RefCell<Vec<PathBuf>>,
}

impl Spill {
    pub fn new(name: &str) -> io::Result<Spill> {
        let dir = crate::decompress::temp_file(name).parent().map(Path::to_path_buf).unwrap_or_else(std::env::temp_dir);
        std::fs::create_dir_all(&dir)?;
        Ok(Spill { dir, made: RefCell::new(Vec::new()) })
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    pub fn create(&self, name: &str) -> io::Result<BufWriter<File>> {
        let p = self.path(name);
        let f = File::create(&p)?;
        self.made.borrow_mut().push(p);
        Ok(BufWriter::with_capacity(64 << 10, f))
    }

    pub fn open(&self, name: &str) -> io::Result<BufReader<File>> {
        Ok(BufReader::with_capacity(256 << 10, File::open(self.path(name))?))
    }
}

impl Drop for Spill {
    fn drop(&mut self) {
        for p in self.made.borrow().iter() {
            let _ = std::fs::remove_file(p);
        }
        // Only when that left it empty.
        let _ = std::fs::remove_dir(&self.dir);
    }
}

/// Fills `b` from `r`: false at the end (a cut-off record counts as the end).
pub(crate) fn read_record(r: &mut impl Read, b: &mut [u8]) -> io::Result<bool> {
    match r.read_exact(b) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
        Err(e) => Err(e),
    }
}
