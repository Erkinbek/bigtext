//! Where the app keeps its settings (`state.txt`), and the searches saved in it, for the command
//! line to run them (`bigtext-cli filter --saved NAME`) as the app does.

use std::io::{self, BufRead, Read};
use std::path::{Path, PathBuf};

use crate::search::Query;

/// A portable copy keeps its settings in this folder next to the program.
pub const PORTABLE_DIR: &str = "BigText-data";

/// The portable settings folder, when there is one next to the program.
pub fn portable_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let d = exe.parent()?.join(PORTABLE_DIR);
    d.is_dir().then_some(d)
}

/// The settings file (`None` with `BIGTEXT_NO_STATE`).
pub fn state_file() -> Option<PathBuf> {
    if std::env::var_os("BIGTEXT_NO_STATE").is_some() {
        return None;
    }
    // Portable mode: a BigText-data folder next to the program.
    if let Some(d) = portable_dir() {
        return Some(d.join("state.txt"));
    }
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join("Library").join("Application Support"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    }?;
    Some(base.join("BigText").join("state.txt"))
}

/// A search saved in the app (the search bar's ★ menu).
#[derive(Clone, Debug, PartialEq)]
pub struct SavedSearch {
    pub name: String,
    pub query: Query,
    /// Filter the lines without a match.
    pub invert: bool,
    /// Filter by this table column only (0-based).
    pub column: Option<usize>,
}

/// A query's switches as letters: r(egex), c(ase), w(ord), d(ot all).
pub fn query_flags(q: &Query) -> String {
    [
        (q.regex, 'r'),
        (q.case_sensitive, 'c'),
        (q.whole_word, 'w'),
        (q.dot_all, 'd'),
    ]
    .iter()
    .filter(|x| x.0)
    .map(|x| x.1)
    .collect()
}

pub fn query_from(pattern: String, flags: &str) -> Query {
    Query {
        pattern,
        regex: flags.contains('r'),
        case_sensitive: flags.contains('c'),
        whole_word: flags.contains('w'),
        dot_all: flags.contains('d'),
    }
}

/// "name|flags|invert|column|pattern" (a `saved=` line).
pub fn parse_saved(v: &str) -> Option<SavedSearch> {
    let mut it = v.splitn(5, '|');
    let name = it.next()?.to_string();
    let flags = it.next()?;
    let invert = it.next()? == "1";
    let column = it.next()?.parse().ok();
    let query = query_from(it.next()?.to_string(), flags);
    Some(SavedSearch {
        name,
        query,
        invert,
        column,
    })
}

/// The first line of the settings file the app writes, and of one exported from it.
const HEADERS: [&str; 2] = ["# BigText session state", "# BigText settings"];

/// Lines longer than this are not the app's (its longest, a copied text, is about 2 MB
/// escaped): they are skipped, so a wrong file of any size is read in bounded memory.
const LINE_MAX: u64 = 4 << 20;

/// The saved searches of a settings file (the app's, or one exported from it). A file that is
/// not one (a log, a table, a binary file) is an `InvalidData` error, so a wrong path is told
/// apart from a settings file without saved searches: the app starts its file with one of
/// [`HEADERS`], and a hand-written one has `saved=` lines.
pub fn saved_searches(path: &Path) -> io::Result<Vec<SavedSearch>> {
    let mut r = io::BufReader::with_capacity(64 << 10, std::fs::File::open(path)?);
    let not_settings = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "not a BigText settings file (the app's state.txt, or one from Help → Export settings)",
        )
    };
    let (mut first, mut header, mut keys_seen, mut saved_lines) = (true, false, false, false);
    let mut out = Vec::new();
    let mut line = Vec::new();
    while let Some(whole) = read_line(&mut r, &mut line, LINE_MAX)? {
        // As the app reads it: bytes from a hand edit in another encoding are only replaced.
        let text = String::from_utf8_lossy(&line);
        let mut text = text.trim_end_matches(['\n', '\r']);
        if first {
            first = false;
            text = text.strip_prefix('\u{feff}').unwrap_or(text);
            header = HEADERS.contains(&text.trim_end());
        }
        if !header && !keys_seen && !text.trim().is_empty() && !text.starts_with('#') {
            // Without the app's first line, a file whose first setting is not `key=value` is
            // something else, known without reading on.
            if !is_setting(text) {
                return Err(not_settings());
            }
            keys_seen = true;
        }
        if let Some(v) = text.strip_prefix("saved=").filter(|_| whole) {
            saved_lines = true;
            out.extend(parse_saved(v));
        }
    }
    if header || saved_lines {
        Ok(out)
    } else {
        Err(not_settings())
    }
}

/// Whether `line` is `key=value` with a key as the app writes them (letters, digits, `_`).
fn is_setting(line: &str) -> bool {
    line.split_once('=').is_some_and(|(k, _)| {
        !k.is_empty() && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    })
}

/// Reads the next line into `line`, with its line break, keeping at most `max` bytes of it.
/// `None` at the end of the file, else whether the line was read whole.
fn read_line(r: &mut impl BufRead, line: &mut Vec<u8>, max: u64) -> io::Result<Option<bool>> {
    line.clear();
    let n = r.by_ref().take(max).read_until(b'\n', line)?;
    if n == 0 {
        return Ok(None);
    }
    if line.ends_with(b"\n") || (n as u64) < max {
        return Ok(Some(true));
    }
    // The rest of a long line is skipped.
    loop {
        let b = r.fill_buf()?;
        if b.is_empty() {
            break;
        }
        match memchr::memchr(b'\n', b) {
            Some(i) => {
                r.consume(i + 1);
                break;
            }
            None => {
                let k = b.len();
                r.consume(k);
            }
        }
    }
    Ok(Some(false))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_lines() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("state.txt");
        std::fs::write(
            &p,
            "font_size=14\nsaved=Timeouts|ri|0||time ?out\nsaved=No health|c|1|2|healthcheck\n",
        )
        .unwrap();
        let s = saved_searches(&p).unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(
            (
                s[0].name.as_str(),
                s[0].query.regex,
                s[0].query.case_sensitive,
                s[0].query.pattern.as_str()
            ),
            ("Timeouts", true, false, "time ?out")
        );
        assert_eq!((s[1].invert, s[1].column), (true, Some(2)));
        assert_eq!(query_flags(&s[0].query), "r");
    }

    #[test]
    fn long_lines_are_cut() {
        let mut r = io::Cursor::new(&b"saved=a|||0|x\r\n0123456789abc\nend"[..]);
        let mut line = Vec::new();
        let mut lines = Vec::new();
        while let Some(whole) = read_line(&mut r, &mut line, 8).unwrap() {
            lines.push((String::from_utf8(line.clone()).unwrap(), whole));
        }
        assert_eq!(
            lines,
            [
                ("saved=a|".to_string(), false),
                ("01234567".to_string(), false),
                ("end".to_string(), true)
            ]
        );
        assert!(is_setting("font_size=14") && is_setting("saved=a=b"));
        assert!(!is_setting("2024-05-01 level=info") && !is_setting("=x") && !is_setting("a,b"));
    }
}
