//! Command-line options for users (the developer options are handled in `main`).

use std::path::{Path, PathBuf};

use bigtext::search::Query;

pub const USAGE: &str = "BigText — fast text editor for huge files

Usage: bigtext [OPTIONS] [FILE[:LINE] ...]

  FILE:LINE        open FILE at line LINE (FILE:LINE:COLUMN, as compilers and grep -n
                   print it, works too)
  -                read standard input, following it as it grows
                   (e.g.  journalctl -f | bigtext -)
  -n, --line N     go to line N in the last file
  --find TEXT      search for TEXT (from the start, or from --line)
  --regex RE       search for a regular expression
  -f, --follow     follow the files as they grow, like tail -f
  -h, --help       show this help

Script commands, without a window (bigtext-cli runs the same; on Windows use it in
scripts, since bigtext.exe is a windowed program):
  --filter, --levels, --count, --split, --stats, --merge, --convert
                   see  bigtext-cli --help
";

#[derive(Debug, Default, PartialEq)]
pub struct UserArgs {
    pub files: Vec<(PathBuf, Option<u64>)>,
    pub find: Option<Query>,
    pub follow: bool,
    pub stdin: bool,
    pub help: bool,
}

/// `path:LINE` or `path:LINE:COLUMN` when `path` exists and the whole argument does not.
pub fn split_line(arg: &str, exists: impl Fn(&Path) -> bool) -> (PathBuf, Option<u64>) {
    if exists(Path::new(arg)) {
        return (PathBuf::from(arg), None);
    }
    let num = |s: &str| (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse::<u64>().ok()).flatten();
    if let Some((rest, last)) = arg.rsplit_once(':').filter(|(_, n)| num(n).is_some()) {
        if !rest.is_empty() && exists(Path::new(rest)) {
            return (PathBuf::from(rest), num(last));
        }
        if let Some((path, line)) = rest.rsplit_once(':').filter(|(_, n)| num(n).is_some()) {
            if !path.is_empty() && exists(Path::new(path)) {
                return (PathBuf::from(path), num(line));
            }
        }
    }
    (PathBuf::from(arg), None)
}

/// Handles one user argument (an option or a file); `next` yields an option's value.
pub fn take(args: &mut UserArgs, arg: &str, next: &mut dyn FnMut() -> Option<String>, exists: &dyn Fn(&Path) -> bool) -> Result<(), String> {
    match arg {
        "-h" | "--help" | "/?" => args.help = true,
        "-" => args.stdin = true,
        "-f" | "--follow" => args.follow = true,
        "-n" | "--line" | "--goto" => {
            let v = next().ok_or_else(|| format!("{arg} needs a line number"))?;
            let n = v.trim().parse::<u64>().map_err(|_| format!("{arg}: “{v}” is not a line number"))?;
            match args.files.last_mut() {
                Some(f) => f.1 = Some(n),
                None => return Err(format!("{arg} goes after the file it applies to")),
            }
        }
        "--find" | "--regex" => {
            let v = next().ok_or_else(|| format!("{arg} needs a pattern"))?;
            args.find = Some(Query { pattern: v, regex: arg == "--regex", case_sensitive: true, ..Default::default() });
        }
        _ if arg.starts_with("--") => return Err(format!("Unknown option {arg}. Run bigtext --help.")),
        _ => args.files.push(split_line(arg, exists)),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(argv: &[&str], existing: &[&str]) -> Result<UserArgs, String> {
        let exists = |p: &Path| existing.iter().any(|e| Path::new(e) == p);
        let mut a = UserArgs::default();
        let mut it = argv.iter().map(|s| s.to_string());
        while let Some(x) = it.next() {
            take(&mut a, &x, &mut || it.next(), &exists)?;
        }
        Ok(a)
    }

    #[test]
    fn files_with_lines() {
        let a = parse(&["app.log:120", r"C:\logs\b.log:7:3", "c.txt", "d.log", "--line", "50"], &["app.log", r"C:\logs\b.log", "c.txt", "d.log"]).unwrap();
        assert_eq!(
            a.files,
            vec![(PathBuf::from("app.log"), Some(120)), (PathBuf::from(r"C:\logs\b.log"), Some(7)), (PathBuf::from("c.txt"), None), (PathBuf::from("d.log"), Some(50))]
        );
        // A file whose name really ends in :12 opens as it is.
        assert_eq!(parse(&["odd:12"], &["odd:12", "odd"]).unwrap().files, vec![(PathBuf::from("odd:12"), None)]);
        // Unknown files are passed on unchanged (the editor reports them).
        assert_eq!(parse(&["missing.log:5"], &[]).unwrap().files, vec![(PathBuf::from("missing.log:5"), None)]);
    }

    #[test]
    fn options() {
        let a = parse(&["-", "--follow", "--regex", "ERROR \\d+"], &[]).unwrap();
        assert!(a.stdin && a.follow);
        let q = a.find.unwrap();
        assert!(q.regex && q.pattern == "ERROR \\d+");
        assert!(parse(&["--line", "5"], &[]).is_err());
        assert!(parse(&["x", "--line", "five"], &["x"]).is_err());
        assert!(parse(&["--frobnicate"], &[]).is_err());
        assert!(parse(&["-h"], &[]).unwrap().help);
    }
}
