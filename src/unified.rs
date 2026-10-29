//! Unified diffs (`diff -u`): written from a comparison of files of any size, streamed (no line
//! is held in memory), and applied back to a text.

use std::io::{self, Write};
use std::ops::Range;

use crate::diff::{self, DiffEnd, DiffOptions, Hunk, LineSource};
use crate::jobs::{CancelToken, Progress};

const CHUNK: usize = 1 << 16;
const BOM: &[u8] = b"\xEF\xBB\xBF";

/// What to compare and how to name the two sides.
pub struct Unified<'a> {
    pub a: &'a dyn LineSource,
    pub b: &'a dyn LineSource,
    pub a_name: &'a str,
    pub b_name: &'a str,
    pub opts: &'a DiffOptions,
    /// Unchanged lines shown around each change.
    pub context: u64,
}

fn read_failed() -> io::Error {
    io::Error::other("reading one of the files failed")
}

/// Writes the unified diff of `u.a` and `u.b`: whether they differ, or `None` when cancelled.
pub fn write_unified(u: &Unified, w: &mut dyn Write, cancel: &CancelToken, progress: Option<&Progress>) -> io::Result<Option<bool>> {
    let mut block: Vec<Hunk> = Vec::new();
    // The end of the last block's lines in A: (line, offset).
    let mut floor = (0u64, 0u64);
    let mut err = None;
    let mut any = false;
    let end = diff::diff_with(u.a, u.b, u.opts, cancel, progress, |h| {
        let r = (|| {
            if !any {
                any = true;
                writeln!(w, "--- {}\n+++ {}", u.a_name, u.b_name)?;
            }
            // Hunks whose context would meet share one block.
            if block.last().is_some_and(|l| h.a_line - (l.a_line + l.a_count) > 2 * u.context) {
                floor = write_block(u, &block, floor, w)?;
                block.clear();
            }
            block.push(h);
            Ok(())
        })();
        match r {
            Ok(()) => true,
            Err(e) => {
                err = Some(e);
                false
            }
        }
    });
    if let Some(e) = err {
        return Err(e);
    }
    match end {
        DiffEnd::Completed => {
            if !block.is_empty() {
                write_block(u, &block, floor, w)?;
            }
            Ok(Some(any))
        }
        DiffEnd::Cancelled => Ok(None),
        DiffEnd::Stopped | DiffEnd::Failed => Err(read_failed()),
    }
}

/// One `@@` block: context before, the hunks with the unchanged lines between them, context after.
/// Returns where it ends in A.
fn write_block(u: &Unified, block: &[Hunk], floor: (u64, u64), w: &mut dyn Write) -> io::Result<(u64, u64)> {
    let (first, last) = (block[0], block[block.len() - 1]);
    let lead = u.context.min(first.a_line - floor.0);
    let lead_off = back_lines(u.a, first.a_off, lead, floor.1)?;
    let (trail, trail_end) = forward_lines(u.a, last.a_end, u.context)?;
    let between: u64 = block.windows(2).map(|p| p[1].a_line - (p[0].a_line + p[0].a_count)).sum();
    let na = lead + block.iter().map(|h| h.a_count).sum::<u64>() + between + trail;
    let nb = lead + block.iter().map(|h| h.b_count).sum::<u64>() + between + trail;
    // 1-based; an empty side names the line before.
    let start = |s: u64, n: u64| if n == 0 { s } else { s + 1 };
    writeln!(w, "@@ -{},{na} +{},{nb} @@", start(first.a_line - lead, na), start(first.b_line - lead, nb))?;
    copy_lines(u.a, lead_off..first.a_off, b' ', w)?;
    for (i, h) in block.iter().enumerate() {
        if i > 0 {
            copy_lines(u.a, block[i - 1].a_end..h.a_off, b' ', w)?;
        }
        copy_lines(u.a, h.a_off..h.a_end, b'-', w)?;
        copy_lines(u.b, h.b_off..h.b_end, b'+', w)?;
    }
    copy_lines(u.a, last.a_end..trail_end, b' ', w)?;
    Ok((last.a_line + last.a_count + trail, trail_end))
}

/// Copies the lines of `range` with `mark` before each, in chunks; a last line without a line
/// break gets diff's "\ No newline at end of file".
fn copy_lines(src: &dyn LineSource, range: Range<u64>, mark: u8, w: &mut dyn Write) -> io::Result<()> {
    let mut pos = range.start;
    if pos == 0 && range.end >= BOM.len() as u64 {
        let mut head = [0u8; 3];
        src.read_into(0, &mut head).ok_or_else(read_failed)?;
        if head == BOM {
            pos = BOM.len() as u64;
        }
    }
    if pos >= range.end {
        return Ok(());
    }
    let mut buf = vec![0u8; CHUNK];
    let mut at_start = true;
    let mut last = b'\n';
    while pos < range.end {
        let n = ((range.end - pos) as usize).min(CHUNK);
        let chunk = &mut buf[..n];
        src.read_into(pos, chunk).ok_or_else(read_failed)?;
        let mut s = 0;
        for i in memchr::memchr_iter(b'\n', chunk) {
            if at_start {
                w.write_all(&[mark])?;
            }
            w.write_all(&chunk[s..=i])?;
            s = i + 1;
            at_start = true;
        }
        if s < n {
            if at_start {
                w.write_all(&[mark])?;
            }
            w.write_all(&chunk[s..])?;
            at_start = false;
        }
        last = chunk[n - 1];
        pos += n as u64;
    }
    if last != b'\n' {
        w.write_all(b"\n\\ No newline at end of file\n")?;
    }
    Ok(())
}

/// The start of the line `n` lines before the one starting at `off`, not before `floor` (a line
/// start).
fn back_lines(src: &dyn LineSource, off: u64, n: u64, floor: u64) -> io::Result<u64> {
    if n == 0 || off <= floor {
        return Ok(off);
    }
    let mut left = n;
    // The line break that ends the line before `off` is not counted.
    let mut end = off - 1;
    let mut buf = vec![0u8; CHUNK];
    while end > floor {
        let s = end.saturating_sub(CHUNK as u64).max(floor);
        let chunk = &mut buf[..(end - s) as usize];
        src.read_into(s, chunk).ok_or_else(read_failed)?;
        for i in memchr::memrchr_iter(b'\n', chunk) {
            left -= 1;
            if left == 0 {
                return Ok(s + i as u64 + 1);
            }
        }
        end = s;
    }
    Ok(floor)
}

/// Up to `n` lines from `from`: how many there are and where they end.
fn forward_lines(src: &dyn LineSource, from: u64, n: u64) -> io::Result<(u64, u64)> {
    let len = src.len();
    let (mut count, mut pos) = (0u64, from);
    let mut buf = vec![0u8; CHUNK];
    while count < n && pos < len {
        let k = ((len - pos) as usize).min(CHUNK);
        let chunk = &mut buf[..k];
        src.read_into(pos, chunk).ok_or_else(read_failed)?;
        let mut used = k;
        for i in memchr::memchr_iter(b'\n', chunk) {
            count += 1;
            if count == n {
                used = i + 1;
                break;
            }
        }
        pos += used as u64;
        if pos == len && count < n && chunk[used - 1] != b'\n' {
            // A last line without a line break.
            count += 1;
        }
    }
    Ok((count, pos))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::{OpenMode, Source};

    fn src(data: &[u8]) -> (tempfile::NamedTempFile, Source) {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(data).unwrap();
        f.flush().unwrap();
        let s = Source::open(f.path(), OpenMode::Auto).unwrap();
        (f, s)
    }

    fn unified(a: &str, b: &str, context: u64) -> (Option<bool>, String) {
        let (_fa, sa) = src(a.as_bytes());
        let (_fb, sb) = src(b.as_bytes());
        let opts = DiffOptions::default();
        let u = Unified { a: &sa, b: &sb, a_name: "a.txt", b_name: "b.txt", opts: &opts, context };
        let mut out = Vec::new();
        let r = write_unified(&u, &mut out, &CancelToken::new(), None).unwrap();
        (r, String::from_utf8(out).unwrap())
    }

    fn numbers(n: u32) -> String {
        (1..=n).map(|i| format!("{i}\n")).collect()
    }

    /// `text` with its 1-based lines `at` replaced.
    fn change(text: &str, at: &[(usize, &str)]) -> String {
        text.lines().enumerate().map(|(i, l)| format!("{}\n", at.iter().find(|c| c.0 == i + 1).map(|c| c.1).unwrap_or(l))).collect()
    }

    #[test]
    fn one_change_with_context() {
        let a = numbers(10);
        let b = change(&a, &[(5, "five")]);
        assert_eq!(unified(&a, &b, 3), (Some(true), "--- a.txt\n+++ b.txt\n@@ -2,7 +2,7 @@\n 2\n 3\n 4\n-5\n+five\n 6\n 7\n 8\n".into()));
        assert_eq!(unified(&a, &a, 3), (Some(false), String::new()));
    }

    #[test]
    fn near_changes_share_a_block_far_ones_do_not() {
        let a = numbers(30);
        let near = change(&a, &[(5, "five"), (10, "ten")]);
        let (_, out) = unified(&a, &near, 3);
        assert_eq!(out.matches("@@ -").count(), 1, "{out}");
        assert!(out.contains("@@ -2,12 +2,12 @@\n"), "{out}");
        let far = change(&a, &[(5, "five"), (20, "twenty")]);
        let (_, out) = unified(&a, &far, 3);
        assert!(out.contains("@@ -2,7 +2,7 @@\n") && out.contains("@@ -17,7 +17,7 @@\n 17\n 18\n 19\n-20\n+twenty\n 21\n 22\n 23\n"), "{out}");
    }

    #[test]
    fn edges_insertions_and_missing_newline() {
        // Lines added at the start, nothing to show before them.
        let (_, out) = unified("x\ny\n", "new\nx\ny\n", 3);
        assert_eq!(out, "--- a.txt\n+++ b.txt\n@@ -1,2 +1,3 @@\n+new\n x\n y\n");
        // An empty file against one with lines.
        let (_, out) = unified("", "a\nb\n", 3);
        assert_eq!(out, "--- a.txt\n+++ b.txt\n@@ -0,0 +1,2 @@\n+a\n+b\n");
        // The last line loses its line break.
        let (_, out) = unified("a\nb\n", "a\nb", 3);
        assert_eq!(out, "--- a.txt\n+++ b.txt\n@@ -1,2 +1,2 @@\n a\n-b\n+b\n\\ No newline at end of file\n");
        // No context: an insertion names the line before it.
        let (_, out) = unified(&numbers(5), &numbers(5).replace("3\n", "3\nin\n"), 0);
        assert_eq!(out, "--- a.txt\n+++ b.txt\n@@ -3,0 +4,1 @@\n+in\n");
        // A byte order mark is not part of the first line.
        let (_, out) = unified("\u{feff}a\nb\n", "\u{feff}z\nb\n", 1);
        assert_eq!(out, "--- a.txt\n+++ b.txt\n@@ -1,2 +1,2 @@\n-a\n+z\n b\n");
    }

    #[test]
    fn long_lines_stream_through_small_chunks() {
        let long = "x".repeat(CHUNK * 3 + 17);
        let a = format!("1\n2\n{long}\n4\n");
        let b = format!("1\n2\n{long}y\n4\n");
        let (_, out) = unified(&a, &b, 1);
        assert_eq!(out, format!("--- a.txt\n+++ b.txt\n@@ -2,3 +2,3 @@\n 2\n-{long}\n+{long}y\n 4\n"));
    }
}
