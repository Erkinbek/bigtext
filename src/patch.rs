//! Applying a unified diff (a .diff or .patch file) to a text of any size: each hunk is found
//! where it says, or up to [`FUZZ`] lines away when it matches only one place there, in one pass
//! over the text.

use std::collections::VecDeque;
use std::ops::Range;

use crate::diff::line_hash;
use crate::jobs::{CancelToken, Progress};
use crate::lines::LineReader;
use crate::text::TextBuf;

/// How far (in lines) a hunk is looked for from where it says it is.
pub const FUZZ: u64 = 4096;

/// One `@@` block of a patch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PatchHunk {
    /// The first line in the old text (1-based; for no lines, the line before) and how many.
    pub old_start: u64,
    pub old_count: u64,
    pub new_start: u64,
    pub new_count: u64,
    /// `b' '`, `b'-'` or `b'+'`, the text without its line break, and whether it had none
    /// (diff's "\ No newline at end of file").
    pub lines: Vec<(u8, Vec<u8>, bool)>,
    /// The header's line in the patch (1-based).
    pub at: usize,
}

/// The hunks for one file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FilePatch {
    pub old_name: String,
    pub new_name: String,
    pub hunks: Vec<PatchHunk>,
}

fn name_of(line: &[u8]) -> String {
    // "--- a/x.txt\t2024-05-01 10:00" names "a/x.txt".
    let s = String::from_utf8_lossy(line);
    s.split('\t').next().unwrap_or("").trim().to_string()
}

/// "@@ -12,3 +12,4 @@" → (12, 3, 12, 4); a missing count is 1.
fn header(line: &[u8]) -> Option<(u64, u64, u64, u64)> {
    let s = std::str::from_utf8(line).ok()?.strip_prefix("@@ -")?;
    let (ranges, _) = s.split_once(" @@")?;
    let (old, new) = ranges.split_once(" +")?;
    let range = |r: &str| -> Option<(u64, u64)> {
        match r.split_once(',') {
            Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
            None => Some((r.parse().ok()?, 1)),
        }
    };
    let (a, b) = range(old)?;
    let (c, d) = range(new)?;
    Some((a, b, c, d))
}

/// Reads a unified diff: the files it changes and their hunks.
pub fn parse(text: &[u8]) -> Result<Vec<FilePatch>, String> {
    let lines: Vec<&[u8]> = text.split(|&c| c == b'\n').map(|l| l.strip_suffix(b"\r").unwrap_or(l)).collect();
    let mut files: Vec<FilePatch> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i];
        if let (Some(old), Some(new)) = (l.strip_prefix(b"--- "), lines.get(i + 1).and_then(|n| n.strip_prefix(b"+++ "))) {
            files.push(FilePatch { old_name: name_of(old), new_name: name_of(new), hunks: Vec::new() });
            i += 2;
            continue;
        }
        let Some((old_start, old_count, new_start, new_count)) = l.starts_with(b"@@ -").then(|| header(l)).flatten() else {
            i += 1;
            continue;
        };
        if files.is_empty() {
            files.push(FilePatch::default());
        }
        let at = i + 1;
        let mut h = PatchHunk { old_start, old_count, new_start, new_count, lines: Vec::new(), at };
        let (mut old, mut new) = (0u64, 0u64);
        i += 1;
        while (old < old_count || new < new_count) && i < lines.len() {
            let l = lines[i];
            // Some tools drop the space of an empty context line.
            let (kind, body) = match l.first() {
                None => (b' ', l),
                Some(&k @ (b' ' | b'-' | b'+')) => (k, &l[1..]),
                Some(b'\\') => {
                    i += 1;
                    continue;
                }
                _ => return Err(format!("line {}: a hunk ends early ({old} of {old_count} old lines, {new} of {new_count} new)", i + 1)),
            };
            if kind != b'+' {
                old += 1;
            }
            if kind != b'-' {
                new += 1;
            }
            h.lines.push((kind, body.to_vec(), false));
            i += 1;
        }
        if old < old_count || new < new_count {
            return Err(format!("line {at}: the patch ends inside a hunk"));
        }
        // "\ No newline at end of file" belongs to the line before it.
        while lines.get(i).is_some_and(|l| l.starts_with(b"\\")) {
            if let Some(last) = h.lines.last_mut() {
                last.2 = true;
            }
            i += 1;
        }
        files.last_mut().expect("a file").hunks.push(h);
    }
    files.retain(|f| !f.hunks.is_empty());
    if files.is_empty() {
        return Err("no hunks (\"@@ -1,2 +1,3 @@\") in the patch".into());
    }
    Ok(files)
}

/// Why a hunk was not applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reject {
    /// Its old lines are not near where it says.
    NotFound,
    /// They are, but in more than one place and not exactly where it says.
    Ambiguous,
}

/// The edits that apply a patch, in order and not overlapping, and the hunks left out.
#[derive(Debug, Default)]
pub struct Planned {
    pub edits: Vec<(Range<u64>, Vec<u8>)>,
    pub applied: usize,
    /// (hunk index, its old start line, why).
    pub rejected: Vec<(usize, u64, Reject)>,
    /// Hunks found away from where they said (applied with an offset).
    pub moved: usize,
}

/// A line of the text: where it starts and ends (line break included), and its hash.
#[derive(Clone, Copy)]
struct Seen {
    off: u64,
    end: u64,
    hash: u64,
}

/// Works out where each of `hunks` (in order) goes in `buf` and the edits that apply them; new
/// lines end with `eol`. `None` when cancelled or the text could not be read.
pub fn plan(buf: &TextBuf, hunks: &[PatchHunk], eol: &[u8], cancel: &CancelToken, progress: Option<&Progress>) -> Option<Planned> {
    let mut r = LineReader::new(buf);
    let mut window: VecDeque<Seen> = VecDeque::new();
    // The index of window[0], and of the next line to read.
    let (mut first, mut next) = (0u64, 0u64);
    let mut eof = false;
    // The first line a hunk may start at, and how far the last one moved.
    let mut floor = 0u64;
    let mut delta = 0i64;
    let mut out = Planned::default();
    if let Some(p) = progress {
        p.set_total(buf.len());
    }
    for (k, h) in hunks.iter().enumerate() {
        if cancel.is_cancelled() {
            return None;
        }
        let old: Vec<u64> = h.lines.iter().filter(|l| l.0 != b'+').map(|l| line_hash(&l.1)).collect();
        let n = old.len() as u64;
        let said = if h.old_count == 0 { h.old_start } else { h.old_start.saturating_sub(1) };
        let expected = (said as i64 + delta).max(0) as u64;
        let lo = expected.saturating_sub(FUZZ).max(floor);
        let hi = expected + FUZZ;
        // Lines before `lo` are not needed any more.
        while first < lo && !window.is_empty() {
            window.pop_front();
            first += 1;
        }
        // Read through `hi + n` (or the end).
        while !eof && next < hi + n {
            let before = r.taken();
            match r.next() {
                Ok(Some((off, line))) => {
                    let hash = (next >= lo).then(|| line_hash(line));
                    let end = off + (r.taken() - before);
                    if let Some(hash) = hash {
                        if window.is_empty() {
                            first = next;
                        }
                        window.push_back(Seen { off, end, hash });
                    }
                    next += 1;
                    if next % 65_536 == 0 {
                        if let Some(p) = progress {
                            p.set_done(end);
                        }
                        if cancel.is_cancelled() {
                            return None;
                        }
                    }
                }
                Ok(None) => eof = true,
                Err(_) => return None,
            }
        }
        if window.is_empty() {
            first = next;
        }
        let matches_at = |p: u64| -> bool {
            p >= first && p + n <= first + window.len() as u64 && old.iter().enumerate().all(|(i, &x)| window[(p - first) as usize + i].hash == x)
        };
        let at = if lo <= expected && matches_at(expected) {
            Some(expected)
        } else {
            let top = hi.min((first + window.len() as u64).saturating_sub(n));
            let mut found = (lo..=top).filter(|&p| matches_at(p));
            match (found.next(), found.next()) {
                (Some(p), None) => {
                    out.moved += 1;
                    Some(p)
                }
                (None, _) => {
                    out.rejected.push((k, h.old_start, Reject::NotFound));
                    None
                }
                (Some(_), Some(_)) => {
                    out.rejected.push((k, h.old_start, Reject::Ambiguous));
                    None
                }
            }
        };
        let Some(p) = at else { continue };
        // The start of line `i` (the end of the text past the last line).
        let start_of = |i: u64| -> u64 {
            match window.get((i - first) as usize) {
                Some(s) => s.off,
                None => window.back().map(|s| s.end).unwrap_or(if first == 0 { 0 } else { buf.len() }),
            }
        };
        // Each run of removed and added lines becomes one edit; context stays as it is.
        let mut i = 0usize;
        let mut line = p;
        while i < h.lines.len() {
            if h.lines[i].0 == b' ' {
                line += 1;
                i += 1;
                continue;
            }
            let from = line;
            let mut text = Vec::new();
            while i < h.lines.len() && h.lines[i].0 != b' ' {
                let (kind, body, no_eol) = &h.lines[i];
                if *kind == b'-' {
                    line += 1;
                } else {
                    text.extend_from_slice(body);
                    if !no_eol {
                        text.extend_from_slice(eol);
                    }
                }
                i += 1;
            }
            out.edits.push((start_of(from)..start_of(line), text));
        }
        out.applied += 1;
        floor = p + n;
        delta = p as i64 - said as i64;
    }
    Some(out)
}

/// Applies `edits` (in order, not overlapping) to a copy of `text`.
pub fn apply_to(text: &[u8], edits: &[(Range<u64>, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    let mut at = 0usize;
    for (r, t) in edits {
        out.extend_from_slice(&text[at..r.start as usize]);
        out.extend_from_slice(t);
        at = r.end as usize;
    }
    out.extend_from_slice(&text[at..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::DiffOptions;
    use crate::file::{OpenMode, Source};
    use crate::index::LineIndex;
    use crate::unified::{write_unified, Unified};
    use std::io::Write;
    use std::sync::Arc;

    fn src(data: &[u8]) -> (tempfile::NamedTempFile, Arc<Source>) {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(data).unwrap();
        f.flush().unwrap();
        let s = Source::open(f.path(), OpenMode::Auto).unwrap();
        (f, Arc::new(s))
    }

    fn diff_of(a: &[u8], b: &[u8], context: u64) -> Vec<u8> {
        let (_fa, sa) = src(a);
        let (_fb, sb) = src(b);
        let opts = DiffOptions::default();
        let u = Unified { a: &*sa, b: &*sb, a_name: "a", b_name: "b", opts: &opts, context };
        let mut out = Vec::new();
        write_unified(&u, &mut out, &CancelToken::new(), None).unwrap();
        out
    }

    fn apply(text: &[u8], patch: &[u8]) -> (Vec<u8>, Planned) {
        let (_f, s) = src(text);
        let buf = TextBuf::new(s.clone(), LineIndex::new(s.len()), crate::encoding::Codec::utf8());
        let files = parse(patch).unwrap();
        let p = plan(&buf, &files[0].hunks, b"\n", &CancelToken::new(), None).unwrap();
        (apply_to(text, &p.edits), p)
    }

    fn lines(n: usize) -> String {
        (0..n).map(|i| format!("line {i}\n")).collect()
    }

    #[test]
    fn parses_headers_counts_and_missing_newlines() {
        let p = b"diff --git a/x b/x\n--- a/x.txt\t2024-05-01\n+++ b/x.txt\n@@ -1,2 +1 @@\n-one\n two\n\\ No newline at end of file\n@@ -10 +9,2 @@\n-ten\n+TEN\n+ELEVEN\n";
        let f = parse(p).unwrap();
        assert_eq!((f.len(), f[0].old_name.as_str(), f[0].new_name.as_str(), f[0].hunks.len()), (1, "a/x.txt", "b/x.txt", 2));
        let h = &f[0].hunks[0];
        assert_eq!((h.old_start, h.old_count, h.new_start, h.new_count, h.at), (1, 2, 1, 1, 4));
        assert_eq!(h.lines, vec![(b'-', b"one".to_vec(), false), (b' ', b"two".to_vec(), true)]);
        assert_eq!((f[0].hunks[1].old_start, f[0].hunks[1].old_count, f[0].hunks[1].new_count), (10, 1, 2));
        assert!(parse(b"just text\n").is_err());
        assert!(parse(b"@@ -1,3 +1,3 @@\n a\n").is_err());
    }

    #[test]
    fn round_trip_through_a_written_diff() {
        let a = lines(3000);
        let b = a.replace("line 7\n", "line seven\n").replace("line 1500\n", "").replace("line 2998\n", "line 2998\nadded\n");
        for context in [0, 3] {
            let patch = diff_of(a.as_bytes(), b.as_bytes(), context);
            let (got, p) = apply(a.as_bytes(), &patch);
            assert_eq!((p.applied, p.rejected.len(), p.moved), (3, 0, 0));
            assert!(got == b.as_bytes(), "context {context}");
        }
        // Lines added at the very end and at the very start; a last line without a line break.
        for (x, y) in [("a\nb\n", "a\nb\nc\nd\n"), ("a\nb\n", "z\na\nb\n"), ("a\nb\n", "a\nb"), ("a\nb", "a\nc\n"), ("", "new\n")] {
            let patch = diff_of(x.as_bytes(), y.as_bytes(), 3);
            let (got, p) = apply(x.as_bytes(), &patch);
            assert_eq!((String::from_utf8(got).unwrap(), p.rejected.len()), (y.to_string(), 0), "{x:?} -> {y:?}");
        }
    }

    #[test]
    fn moved_hunks_are_found_and_unclear_ones_rejected() {
        let a = lines(200);
        let b = a.replace("line 100\n", "line one hundred\n");
        let patch = diff_of(a.as_bytes(), b.as_bytes(), 3);
        // The text has 40 more lines at the top than the patch expects.
        let shifted = format!("{}{a}", "extra\n".repeat(40));
        let (got, p) = apply(shifted.as_bytes(), &patch);
        assert_eq!((p.applied, p.moved), (1, 1));
        assert_eq!(String::from_utf8(got).unwrap(), format!("{}{b}", "extra\n".repeat(40)));
        // Already applied: its old lines are gone.
        let (_, p) = apply(b.as_bytes(), &patch);
        assert_eq!(p.rejected, vec![(0, 98, Reject::NotFound)]);
        // The same lines twice, neither where the patch says.
        let same = "x\ny\nz\n".repeat(5);
        let patch = b"@@ -100,3 +100,3 @@\n x\n-y\n+Y\n z\n";
        let (_, p) = apply(same.as_bytes(), patch);
        assert_eq!(p.rejected, vec![(0, 100, Reject::Ambiguous)]);
    }
}
