//! Merging logs by time: the lines of several logs in timestamp order, each line labelled with
//! its file, written as one UTF-8 text. Lines without a time (stack traces, continuations) stay
//! with the line above them, and lines before a file's first time come first. Each file is
//! read once, front to back, so memory use does not depend on the file sizes.

use std::io::{self, Write};

use crate::encoding::Encoding;
use crate::jobs::{CancelToken, Progress};
use crate::text::TextBuf;
use crate::timestamp::{self, Stamp};

/// Bytes read from a file at a time.
const CHUNK: usize = 1 << 20;
/// Longer lines are cut into pieces of this size.
const MAX_LINE: usize = 16 << 20;
const DAY: i64 = 86_400_000;

pub struct Input {
    pub buf: TextBuf,
    /// Written before each line of this file.
    pub label: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Merged {
    pub lines: u64,
    pub bytes: u64,
    /// Inputs in which no line had a time (their lines come first).
    pub without_times: usize,
}

/// The lines of one input, in order, without their line breaks.
struct LineReader<'a> {
    buf: &'a TextBuf,
    data: Vec<u8>,
    /// Consumed bytes at the start of `data`.
    at: usize,
    /// File offset of the end of `data`.
    end: u64,
    line: Vec<u8>,
    chunk: Vec<u8>,
}

impl<'a> LineReader<'a> {
    fn new(buf: &'a TextBuf) -> LineReader<'a> {
        let start = (buf.codec.bom_len as u64).min(buf.len());
        LineReader { buf, data: Vec::new(), at: 0, end: start, line: Vec::new(), chunk: Vec::new() }
    }

    /// The next line (`Ok(None)` at the end of the file); `read` counts the bytes taken.
    fn next(&mut self, read: &mut u64) -> io::Result<Option<&[u8]>> {
        let codec = self.buf.codec;
        let unit = codec.unit();
        loop {
            let base = self.end - (self.data.len() - self.at) as u64;
            let pending = &self.data[self.at..];
            let cut = match codec.find_nl(pending, base) {
                Some(i) if i <= MAX_LINE => Some((i, i + unit)),
                _ if pending.len() > MAX_LINE => {
                    let mut n = MAX_LINE - MAX_LINE % unit;
                    // Not inside a UTF-8 character.
                    while codec.enc == Encoding::Utf8 && n > 0 && pending[n] & 0xC0 == 0x80 {
                        n -= 1;
                    }
                    Some((n, n))
                }
                None if self.end >= self.buf.len() => (!pending.is_empty()).then_some((pending.len(), pending.len())),
                _ => None,
            };
            if let Some((content, taken)) = cut {
                self.line.clear();
                self.line.extend_from_slice(&pending[..content]);
                let cr = codec.cr_unit();
                if content < taken && self.line.ends_with(cr) {
                    self.line.truncate(self.line.len() - cr.len());
                }
                self.at += taken;
                *read += taken as u64;
                return Ok(Some(&self.line));
            }
            if self.end >= self.buf.len() {
                return Ok(None);
            }
            self.data.drain(..self.at);
            self.at = 0;
            let to = (self.end + CHUNK as u64).min(self.buf.len());
            let n = self.buf.read_to(self.end..to, &mut self.chunk).ok_or_else(|| io::Error::other("could not read the file"))?;
            self.data.extend_from_slice(&self.chunk[..n]);
            self.end = to;
        }
    }
}

/// Turns timestamps into sortable keys: a date's milliseconds, or for times without a date the
/// time of day plus the days that have passed (a time much earlier than the one before is the
/// next day).
#[derive(Default)]
struct Clock {
    day: i64,
    last: Option<i64>,
}

impl Clock {
    fn key(&mut self, st: Stamp, dated: bool) -> i64 {
        let key = if dated && st.has_date {
            self.day = st.ms.div_euclid(DAY);
            st.ms
        } else {
            let tod = st.time_of_day();
            if self.last.is_some_and(|l| tod + DAY / 2 < l.rem_euclid(DAY)) {
                self.day += 1;
            }
            self.day * DAY + tod
        };
        self.last = Some(key);
        key
    }
}

struct Cursor<'a> {
    lines: LineReader<'a>,
    clock: Clock,
    /// The next line to write with its key (`i64::MIN` for lines before the first time).
    head: Option<(i64, String)>,
    seen_time: bool,
}

fn stamp(line: &str) -> Option<Stamp> {
    timestamp::parse(line)
}

/// Writes the lines of `inputs` merged by time to `out`. Returns `None` when cancelled.
pub fn merge_to<W: Write>(inputs: &[Input], out: &mut W, cancel: &CancelToken, progress: Option<&Progress>) -> io::Result<Option<Merged>> {
    let width = inputs.iter().map(|i| i.label.chars().count()).max().unwrap_or(0);
    let labels: Vec<String> = inputs.iter().map(|i| format!("{:<width$} │ ", i.label)).collect();
    if let Some(p) = progress {
        p.set_total(inputs.iter().map(|i| i.buf.len()).sum());
    }
    let mut read = 0u64;
    let mut cursors: Vec<Cursor> = inputs.iter().map(|i| Cursor { lines: LineReader::new(&i.buf), clock: Clock::default(), head: None, seen_time: false }).collect();
    // First lines; times compare as dates only when every file's first time has one.
    let mut firsts = Vec::with_capacity(cursors.len());
    for c in &mut cursors {
        let codec = c.lines.buf.codec;
        let line = c.lines.next(&mut read)?.map(|l| codec.decode_lossy(l));
        firsts.push(line);
    }
    let dated = firsts.iter().flatten().filter_map(|l| stamp(l)).all(|s| s.has_date);
    for (c, line) in cursors.iter_mut().zip(firsts) {
        c.head = line.map(|l| match stamp(&l) {
            Some(st) => {
                c.seen_time = true;
                (c.clock.key(st, dated), l)
            }
            None => (i64::MIN, l),
        });
    }
    let mut stats = Merged::default();
    let write_line = |out: &mut W, label: &str, text: &str, stats: &mut Merged| -> io::Result<()> {
        out.write_all(label.as_bytes())?;
        out.write_all(text.as_bytes())?;
        out.write_all(b"\n")?;
        stats.lines += 1;
        stats.bytes += (label.len() + text.len() + 1) as u64;
        Ok(())
    };
    loop {
        if cancel.is_cancelled() {
            return Ok(None);
        }
        // Few inputs: a linear scan for the earliest head (ties keep the input order).
        let Some(i) = (0..cursors.len()).filter(|&i| cursors[i].head.is_some()).min_by_key(|&i| (cursors[i].head.as_ref().map(|h| h.0), i)) else { break };
        let c = &mut cursors[i];
        let codec = c.lines.buf.codec;
        let (_, text) = c.head.take().unwrap();
        write_line(out, &labels[i], &text, &mut stats)?;
        // The lines after it without a time belong to it.
        while let Some(raw) = c.lines.next(&mut read)? {
            let line = codec.decode_lossy(raw);
            match stamp(&line) {
                Some(st) => {
                    c.seen_time = true;
                    c.head = Some((c.clock.key(st, dated), line));
                    break;
                }
                None => write_line(out, &labels[i], &line, &mut stats)?,
            }
            if cancel.is_cancelled() {
                return Ok(None);
            }
        }
        if let Some(p) = progress {
            p.set_done(read);
        }
    }
    stats.without_times = cursors.iter().filter(|c| !c.seen_time).count();
    Ok(Some(stats))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::{self, Codec};
    use crate::file::{OpenMode, Source};
    use crate::index::LineIndex;
    use std::sync::Arc;

    fn input(dir: &tempfile::TempDir, name: &str, bytes: &[u8]) -> Input {
        let path = dir.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        let src = Arc::new(Source::open(&path, OpenMode::Auto).unwrap());
        let det = encoding::detect(&src.read(0..src.len()).unwrap(), true);
        let len = src.len();
        Input { buf: TextBuf::new(src, LineIndex::new(len), det.codec), label: name.into() }
    }

    fn merge(inputs: &[Input]) -> (String, Merged) {
        let mut out = Vec::new();
        let m = merge_to(inputs, &mut out, &CancelToken::new(), None).unwrap().unwrap();
        (String::from_utf8(out).unwrap(), m)
    }

    #[test]
    fn merges_by_time_and_keeps_continuations() {
        let dir = tempfile::tempdir().unwrap();
        let a = input(&dir, "a", b"2024-05-01 10:00:00 a1\n2024-05-01 10:00:02 a2\n  at x()\n  at y()\n2024-05-01 10:00:04 a3\n");
        let b = input(&dir, "bb", b"header\n2024-05-01 10:00:01 b1\r\n2024-05-01 10:00:02 b2\r\n2024-05-01 10:00:03 b3");
        let (text, m) = merge(&[a, b]);
        assert_eq!(
            text,
            "bb │ header\n\
             a  │ 2024-05-01 10:00:00 a1\n\
             bb │ 2024-05-01 10:00:01 b1\n\
             a  │ 2024-05-01 10:00:02 a2\n\
             a  │   at x()\n\
             a  │   at y()\n\
             bb │ 2024-05-01 10:00:02 b2\n\
             bb │ 2024-05-01 10:00:03 b3\n\
             a  │ 2024-05-01 10:00:04 a3\n"
        );
        assert_eq!((m.lines, m.bytes as usize, m.without_times), (9, text.len(), 0));
    }

    #[test]
    fn times_without_dates_roll_over_midnight() {
        let dir = tempfile::tempdir().unwrap();
        let a = input(&dir, "a", b"23:59:58 a1\n00:00:02 a2\n");
        let b = input(&dir, "b", b"23:59:59 b1\n00:00:01 b2\n");
        assert_eq!(merge(&[a, b]).0, "a │ 23:59:58 a1\nb │ 23:59:59 b1\nb │ 00:00:01 b2\na │ 00:00:02 a2\n");
    }

    #[test]
    fn a_file_without_dates_compares_times_of_day() {
        let dir = tempfile::tempdir().unwrap();
        let a = input(&dir, "a", b"09:00:00 a1\n11:00:00 a2\n");
        let c = input(&dir, "c", b"2024-05-01 10:00:00 c1\n");
        assert_eq!(merge(&[a, c]).0, "a │ 09:00:00 a1\nc │ 2024-05-01 10:00:00 c1\na │ 11:00:00 a2\n");
    }

    #[test]
    fn other_encodings_and_files_without_times() {
        let dir = tempfile::tempdir().unwrap();
        let utf16: Vec<u8> = [0xFF, 0xFE].into_iter().chain("2024-05-01 10:00:01 привет\r\n".encode_utf16().flat_map(|u| u.to_le_bytes())).collect();
        let a = input(&dir, "u16", &utf16);
        let b = input(&dir, "plain", b"no times\nat all\n");
        let c = input(&dir, "empty", b"");
        let (text, m) = merge(&[a, b, c]);
        assert_eq!(text, "plain │ no times\nplain │ at all\nu16   │ 2024-05-01 10:00:01 привет\n");
        assert_eq!(m.without_times, 2);
        assert_eq!(Codec::utf8().unit(), 1);
    }

    #[test]
    fn cancel_stops() {
        let dir = tempfile::tempdir().unwrap();
        let a = input(&dir, "a", b"10:00:00 x\n");
        let cancel = CancelToken::new();
        cancel.cancel();
        assert_eq!(merge_to(&[a], &mut Vec::new(), &cancel, None).unwrap(), None);
    }

    #[test]
    fn long_lines_are_split_and_chunks_join() {
        let dir = tempfile::tempdir().unwrap();
        let mut big = b"2024-05-01 10:00:00 start\n".to_vec();
        big.extend(std::iter::repeat_n(b'x', MAX_LINE + 10));
        big.extend_from_slice(b"\n2024-05-01 10:00:01 end\n");
        let a = input(&dir, "a", &big);
        let (text, m) = merge(&[a]);
        // "a │ " is 6 bytes; the long line comes out as two lines.
        assert_eq!(m.lines, 4);
        assert!(text.ends_with(&format!("\na │ {}\na │ 2024-05-01 10:00:01 end\n", "x".repeat(10))));
        assert_eq!(text.len(), (6 + 25 + 1) + (6 + MAX_LINE + 1) + (6 + 10 + 1) + (6 + 23 + 1));
    }
}
