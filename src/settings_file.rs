//! Where the app keeps its settings (`state.txt`), and the searches saved in it, for the command
//! line to run them (`bigtext-cli filter --saved NAME`) as the app does.

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
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library").join("Application Support"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
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
    [(q.regex, 'r'), (q.case_sensitive, 'c'), (q.whole_word, 'w'), (q.dot_all, 'd')].iter().filter(|x| x.0).map(|x| x.1).collect()
}

pub fn query_from(pattern: String, flags: &str) -> Query {
    Query { pattern, regex: flags.contains('r'), case_sensitive: flags.contains('c'), whole_word: flags.contains('w'), dot_all: flags.contains('d') }
}

/// "name|flags|invert|column|pattern" (a `saved=` line).
pub fn parse_saved(v: &str) -> Option<SavedSearch> {
    let mut it = v.splitn(5, '|');
    let name = it.next()?.to_string();
    let flags = it.next()?;
    let invert = it.next()? == "1";
    let column = it.next()?.parse().ok();
    let query = query_from(it.next()?.to_string(), flags);
    Some(SavedSearch { name, query, invert, column })
}

/// The saved searches of a settings file (the app's, or one exported from it).
pub fn saved_searches(path: &Path) -> std::io::Result<Vec<SavedSearch>> {
    let text = std::fs::read_to_string(path)?;
    Ok(text.lines().filter_map(|l| l.strip_prefix("saved=")).filter_map(parse_saved).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_lines() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("state.txt");
        std::fs::write(&p, "font_size=14\nsaved=Timeouts|ri|0||time ?out\nsaved=No health|c|1|2|healthcheck\n").unwrap();
        let s = saved_searches(&p).unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!((s[0].name.as_str(), s[0].query.regex, s[0].query.case_sensitive, s[0].query.pattern.as_str()), ("Timeouts", true, false, "time ?out"));
        assert_eq!((s[1].invert, s[1].column), (true, Some(2)));
        assert_eq!(query_flags(&s[0].query), "r");
    }
}
