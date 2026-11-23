//! The add buffer of a [`TextBuf`](super::TextBuf): every inserted text, appended in chunks.
//!
//! Offsets are global and contiguous (chunk `k` starts where chunk `k - 1` ends), so add pieces
//! keep plain `start`/`len` values and the recovery journal sees one byte stream. Each inserted
//! text lies inside a single chunk, so a piece's bytes are always one slice. Only the last chunk
//! grows; when a snapshot shares it, a small one is copied and a larger one is left alone and a
//! new chunk starts, so a write never copies more than [`COPY_MAX`] old bytes.

use std::sync::Arc;

/// Small inserts share a chunk up to this size; larger ones get a chunk of their own.
const CHUNK_MAX: usize = 1 << 20;
/// A last chunk shared with a snapshot is copied up to this size, otherwise a new one starts.
const COPY_MAX: usize = 64 << 10;

#[derive(Clone, Debug, Default)]
pub(crate) struct AddBuf {
    /// (global offset of the chunk, its bytes)
    chunks: Arc<Vec<(u64, Arc<Vec<u8>>)>>,
    len: u64,
}

impl AddBuf {
    pub fn from_vec(bytes: Vec<u8>) -> AddBuf {
        let len = bytes.len() as u64;
        let chunks = if bytes.is_empty() {
            Vec::new()
        } else {
            vec![(0, Arc::new(bytes))]
        };
        AddBuf {
            chunks: Arc::new(chunks),
            len,
        }
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    /// Appends `text` in one piece of one chunk and returns its global offset.
    pub fn push(&mut self, text: &[u8]) -> u64 {
        let start = self.len;
        if text.is_empty() {
            return start;
        }
        let chunks = Arc::make_mut(&mut self.chunks);
        match chunks.last_mut() {
            Some((_, c))
                if c.len() + text.len() <= CHUNK_MAX
                    && (Arc::strong_count(c) == 1 || c.len() <= COPY_MAX) =>
            {
                Arc::make_mut(c).extend_from_slice(text);
            }
            _ => chunks.push((start, Arc::new(text.to_vec()))),
        }
        self.len += text.len() as u64;
        start
    }

    fn chunk_index(&self, off: u64) -> usize {
        self.chunks
            .partition_point(|&(s, _)| s <= off)
            .saturating_sub(1)
    }

    /// The bytes of `start..end`, which must lie inside one inserted text (or one chunk).
    pub fn slice(&self, start: u64, end: u64) -> &[u8] {
        if start == end {
            return &[];
        }
        let (s, c) = &self.chunks[self.chunk_index(start)];
        &c[(start - s) as usize..(end - s) as usize]
    }

    /// Whether `start..end` lies inside one chunk (so it can be read as one slice).
    pub fn contiguous(&self, start: u64, end: u64) -> bool {
        start == end || self.chunk_index(start) == self.chunk_index(end - 1)
    }

    /// A copy of the bytes from `from` to the end.
    pub fn copy_from(&self, from: u64) -> Vec<u8> {
        let from = from.min(self.len);
        let mut out = Vec::with_capacity((self.len - from) as usize);
        for (s, c) in self.chunks[self.chunk_index(from)..].iter() {
            out.extend_from_slice(&c[from.saturating_sub(*s) as usize..]);
        }
        out
    }

    #[cfg(test)]
    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }
}
