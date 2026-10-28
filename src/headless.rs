//! BigText's tools for scripts, without a window: `bigtext-cli COMMAND …` (or `bigtext --COMMAND
//! …`). They stream like the editor does, so they work on files of any size.
//!
//! Exit codes: 0 done (for `filter`, `levels` and `count`: some line matched), 1 nothing
//! matched, 2 an error.

use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::csv;
use crate::encoding::{self, Detected};
use crate::file::{display_path, OpenMode, Source};
use crate::filter::{self, FilterSpec};
use crate::index::LineIndex;
use crate::jobs::{human_bytes, CancelToken, Progress};
use crate::lines::LineReader;
use crate::loglevel::{self, Level, Levels};
use crate::merge::{self, Input};
use crate::search::{Matcher, Query};
use crate::split::{self, SplitBy};
use crate::table_export::{self, Format};
use crate::text::TextBuf;
use crate::timestamp;

pub const USAGE: &str = "\
bigtext-cli: BigText's tools for scripts (bigtext --COMMAND works too)

  filter [-v] [-i] [-w] [-E] PATTERN FILE [-o OUT]
        Lines with a match (-v: without one; -i: ignore case; -w: whole words;
        -E: regular expression). To OUT in the file's encoding, or to the
        console. Exit code 1 when no line matched.
  levels ERROR,WARN,INFO,DEBUG FILE [-o OUT]
        Lines of these log levels.
  count [-v] [-i] [-w] [-E] PATTERN FILE
        The number of lines with a match.
  split (--size 100MB | --lines 1000000) FILE [--out-dir DIR]
        Parts name.part001.ext, … cut at line breaks; prints their paths.
  stats FILE...
        Size, lines, encoding, line endings and the first and last times.
  merge FILE FILE... [-o OUT]
        Log lines of all files in time order, each labelled with its file.
  convert FILE --to json|jsonl|md|xlsx|csv [-o OUT]
        A CSV / TSV table as JSON, JSON Lines, Markdown or Excel (.xlsx needs
        -o), or JSON Lines as CSV.

Exit codes: 0 done, 1 nothing matched, 2 error.
";

const COMMANDS: [&str; 7] = ["filter", "levels", "count", "split", "stats", "merge", "convert"];

/// Whether `arg` starts a command (`filter` or `--filter`).
pub fn is_command(arg: &str) -> bool {
    COMMANDS.contains(&arg.trim_start_matches('-'))
}

struct Fail(String);

impl From<io::Error> for Fail {
    fn from(e: io::Error) -> Fail {
        Fail(e.to_string())
    }
}

type Res<T> = Result<T, Fail>;

fn fail<T>(msg: impl Into<String>) -> Res<T> {
    Err(Fail(msg.into()))
}

/// The arguments of one command: flags, options with a value, and the rest.
struct Args {
    flags: Vec<String>,
    opts: Vec<(String, String)>,
    rest: Vec<String>,
}

impl Args {
    fn parse(args: &[String], flags: &[&str], opts: &[&str]) -> Res<Args> {
        let mut a = Args { flags: Vec::new(), opts: Vec::new(), rest: Vec::new() };
        let mut it = args.iter();
        while let Some(x) = it.next() {
            if flags.contains(&x.as_str()) {
                a.flags.push(x.clone());
            } else if opts.contains(&x.as_str()) {
                let v = it.next().ok_or_else(|| Fail(format!("{x} needs a value")))?;
                a.opts.push((x.clone(), v.clone()));
            } else if x.starts_with('-') && x.len() > 1 {
                return fail(format!("unknown option {x}"));
            } else {
                a.rest.push(x.clone());
            }
        }
        Ok(a)
    }

    fn flag(&self, names: &[&str]) -> bool {
        self.flags.iter().any(|f| names.contains(&f.as_str()))
    }

    fn opt(&self, names: &[&str]) -> Option<&str> {
        self.opts.iter().rev().find(|(k, _)| names.contains(&k.as_str())).map(|(_, v)| v.as_str())
    }
}

fn open(path: &Path) -> Res<(TextBuf, Detected)> {
    let src = Source::open(path, OpenMode::Auto).map_err(|e| Fail(format!("{}: {e}", display_path(path))))?;
    let n = src.len().min(1 << 20);
    let det = encoding::detect(&src.read(0..n)?, n == src.len());
    let len = src.len();
    let mut buf = TextBuf::new(Arc::new(src), LineIndex::new(len), det.codec);
    buf.blocking = true;
    Ok((buf, det))
}

/// Writes through `w`, refusing to seek (for the console).
struct NoSeek<W>(W);

impl<W: Write> Write for NoSeek<W> {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0.write(b)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl<W> Seek for NoSeek<W> {
    fn seek(&mut self, _: SeekFrom) -> io::Result<u64> {
        Err(io::Error::other("cannot seek"))
    }
}

/// Writes to `-o FILE` (via a temporary file, moved into place when done) or to `out`.
fn with_output(target: Option<&str>, out: &mut dyn Write, f: impl FnOnce(&mut dyn WriteSeek) -> Res<()>) -> Res<()> {
    match target {
        Some(t) => {
            let path = PathBuf::from(t);
            let temp = crate::save::temp_path_for(&path);
            let file = std::fs::File::options().write(true).create_new(true).open(&temp).map_err(|e| Fail(format!("{}: {e}", display_path(&temp))))?;
            let r = (|| {
                let mut w = io::BufWriter::with_capacity(8 << 20, &file);
                f(&mut w)?;
                w.flush()?;
                drop(w);
                file.sync_all()?;
                Ok(())
            })();
            drop(file);
            match r {
                Ok(()) => crate::save::finalize(&temp, &path, false).map_err(Fail),
                Err(e) => {
                    let _ = std::fs::remove_file(&temp);
                    Err(e)
                }
            }
        }
        None => {
            let mut w = NoSeek(io::BufWriter::new(out));
            f(&mut w)?;
            w.flush()?;
            Ok(())
        }
    }
}

trait WriteSeek: Write + Seek {}
impl<T: Write + Seek> WriteSeek for T {}

fn query(a: &Args, pattern: &str) -> Query {
    Query { pattern: pattern.to_string(), regex: a.flag(&["-E", "--regex"]), case_sensitive: !a.flag(&["-i", "--ignore-case"]), whole_word: a.flag(&["-w", "--word"]), dot_all: false }
}

/// Writes the kept lines: in the file's encoding to a file, as UTF-8 to the console.
fn write_kept(buf: &TextBuf, m: &Matcher, spec: FilterSpec, target: Option<&str>, out: &mut dyn Write) -> Res<u64> {
    let cancel = CancelToken::new();
    let mut kept = 0;
    with_output(target, out, |w| {
        if target.is_some() {
            kept = filter::write_lines_to(buf, m, spec, &mut &mut *w, &cancel, None)?.map(|n| n.lines).unwrap_or(0);
            return Ok(());
        }
        let mut err = None;
        filter::filter_lines(buf, m, spec, &cancel, None, |ls, le| {
            let line = buf.read(ls.max(buf.codec.bom_len as u64)..le).map(|b| buf.codec.decode_lossy(&b));
            let text = line.as_deref().unwrap_or("").trim_end_matches(['\n', '\r']).to_string();
            if let Err(e) = writeln!(w, "{text}") {
                err = Some(e);
                return false;
            }
            kept += 1;
            true
        });
        match err {
            Some(e) => Err(e.into()),
            None => Ok(()),
        }
    })?;
    Ok(kept)
}

fn cmd_filter(args: &[String], out: &mut dyn Write, count_only: bool) -> Res<i32> {
    let a = Args::parse(args, &["-v", "--invert", "-i", "--ignore-case", "-w", "--word", "-E", "--regex"], &["-o", "--output"])?;
    let [pattern, file] = &a.rest[..] else { return fail("needs PATTERN and FILE") };
    let (buf, _) = open(Path::new(file))?;
    let m = Matcher::new(&query(&a, pattern), buf.codec).map_err(Fail)?;
    let spec = FilterSpec { invert: a.flag(&["-v", "--invert"]), column: None };
    let n = if count_only {
        let mut n = 0u64;
        filter::filter_lines(&buf, &m, spec, &CancelToken::new(), None, |_, _| {
            n += 1;
            true
        });
        writeln!(out, "{n}")?;
        n
    } else {
        write_kept(&buf, &m, spec, a.opt(&["-o", "--output"]), out)?
    };
    Ok(if n > 0 { 0 } else { 1 })
}

fn cmd_levels(args: &[String], out: &mut dyn Write) -> Res<i32> {
    let a = Args::parse(args, &[], &["-o", "--output"])?;
    let [names, file] = &a.rest[..] else { return fail("needs LEVELS and FILE") };
    let mut levels = Levels::default();
    for n in names.split(',').map(str::trim).filter(|n| !n.is_empty()) {
        let l = Level::ALL.into_iter().find(|l| l.name().eq_ignore_ascii_case(n) || (n.eq_ignore_ascii_case("warning") && *l == Level::Warn));
        match l {
            Some(l) if !levels.contains(l) => levels = levels.toggled(l),
            Some(_) => {}
            None => return fail(format!("unknown level {n} (ERROR, WARN, INFO, DEBUG)")),
        }
    }
    let q = loglevel::query(levels).ok_or_else(|| Fail("no levels".into()))?;
    let (buf, _) = open(Path::new(file))?;
    let m = Matcher::new(&q, buf.codec).map_err(Fail)?;
    let n = write_kept(&buf, &m, FilterSpec::default(), a.opt(&["-o", "--output"]), out)?;
    Ok(if n > 0 { 0 } else { 1 })
}

/// "100MB", "1.5G", "500k" or a number of bytes.
fn parse_size(s: &str) -> Option<u64> {
    let t = s.trim().to_ascii_lowercase();
    let t = t.trim_end_matches('b');
    let (num, mul) = match t.chars().last()? {
        'k' => (&t[..t.len() - 1], 1u64 << 10),
        'm' => (&t[..t.len() - 1], 1 << 20),
        'g' => (&t[..t.len() - 1], 1 << 30),
        't' => (&t[..t.len() - 1], 1 << 40),
        _ => (t, 1),
    };
    let v: f64 = num.trim().parse().ok()?;
    (v > 0.0).then_some((v * mul as f64) as u64)
}

fn cmd_split(args: &[String], out: &mut dyn Write) -> Res<i32> {
    let a = Args::parse(args, &[], &["--size", "--lines", "--out-dir"])?;
    let [file] = &a.rest[..] else { return fail("needs FILE") };
    let by = match (a.opt(&["--size"]), a.opt(&["--lines"])) {
        (Some(s), None) => SplitBy::Bytes(parse_size(s).ok_or_else(|| Fail(format!("not a size: {s}")))?),
        (None, Some(n)) => SplitBy::Lines(n.parse().ok().filter(|&n| n > 0).ok_or_else(|| Fail(format!("not a line count: {n}")))?),
        _ => return fail("give --size or --lines"),
    };
    let path = Path::new(file);
    let dir = a.opt(&["--out-dir"]).map(PathBuf::from).or_else(|| path.parent().map(Path::to_path_buf)).unwrap_or_default();
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "part".into());
    let (buf, _) = open(path)?;
    let parts = split::split_to_files(&buf, by, &dir, &name, &CancelToken::new(), &Progress::default())?.unwrap_or_default();
    for p in parts {
        writeln!(out, "{}", display_path(&p))?;
    }
    Ok(0)
}

fn cmd_stats(args: &[String], out: &mut dyn Write) -> Res<i32> {
    let a = Args::parse(args, &[], &[])?;
    if a.rest.is_empty() {
        return fail("needs FILE");
    }
    for file in &a.rest {
        let path = Path::new(file);
        let (buf, det) = open(path)?;
        let codec = buf.codec;
        let mut lines = 0u64;
        let read = buf.for_each_chunk(0..buf.len(), |base, chunk| {
            lines += codec.count_nl(chunk, base);
            true
        });
        if read.is_none() {
            return fail(format!("{}: could not read the file", display_path(path)));
        }
        // A last line without a line break counts too.
        let tail = buf.len().saturating_sub(codec.unit() as u64);
        if buf.len() > codec.bom_len as u64 && buf.read(tail..buf.len()).is_some_and(|b| codec.find_nl(&b, tail).is_none()) {
            lines += 1;
        }
        let mut first = None;
        let mut r = LineReader::new(&buf);
        for _ in 0..1000 {
            let Some((_, l)) = r.next()? else { break };
            if let Some(st) = timestamp::parse(&codec.decode_lossy(&l[..l.len().min(640)])) {
                first = Some(st);
                break;
            }
        }
        let last = (|| {
            let from = buf.len().saturating_sub(1 << 20);
            let b = buf.read(from..buf.len())?;
            let text = codec.decode_lossy(&b);
            text.lines().rev().take(1000).find_map(timestamp::parse)
        })();
        writeln!(out, "file: {}", display_path(path))?;
        writeln!(out, "size: {} ({} bytes)", human_bytes(buf.len()), buf.len())?;
        writeln!(out, "lines: {lines}")?;
        writeln!(out, "encoding: {}", codec.enc.name())?;
        writeln!(out, "line endings: {}", det.line_ending.name())?;
        if let (Some(f), Some(l)) = (first, last) {
            let key = |s: timestamp::Stamp| timestamp::format_key(s.ms, s.has_date, true);
            writeln!(out, "first time: {}", key(f))?;
            writeln!(out, "last time: {}", key(l))?;
            if f.has_date && l.has_date {
                writeln!(out, "span: {}", timestamp::format_delta(l.ms - f.ms).trim_start_matches('+'))?;
            }
        }
        if a.rest.len() > 1 {
            writeln!(out)?;
        }
    }
    Ok(0)
}

fn cmd_merge(args: &[String], out: &mut dyn Write) -> Res<i32> {
    let a = Args::parse(args, &[], &["-o", "--output"])?;
    if a.rest.len() < 2 {
        return fail("needs two or more files");
    }
    let mut inputs = Vec::new();
    let mut names: Vec<String> = Vec::new();
    for f in &a.rest {
        let p = Path::new(f);
        let mut label = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| f.clone());
        let base = label.clone();
        let mut k = 1;
        while names.contains(&label) {
            k += 1;
            label = format!("{base} ({k})");
        }
        names.push(label.clone());
        inputs.push(Input { buf: open(p)?.0, label });
    }
    with_output(a.opt(&["-o", "--output"]), out, |w| {
        merge::merge_to(&inputs, &mut &mut *w, &CancelToken::new(), None)?;
        Ok(())
    })?;
    Ok(0)
}

fn cmd_convert(args: &[String], out: &mut dyn Write) -> Res<i32> {
    let a = Args::parse(args, &["--no-header"], &["--to", "-o", "--output"])?;
    let [file] = &a.rest[..] else { return fail("needs FILE") };
    let to = a.opt(&["--to"]).ok_or_else(|| Fail("give --to json, jsonl, md, xlsx or csv".into()))?.to_ascii_lowercase();
    let target = a.opt(&["-o", "--output"]);
    let (buf, _) = open(Path::new(file))?;
    if to == "csv" {
        with_output(target, out, |w| {
            crate::json::jsonl_to_csv(&buf, &mut &mut *w, &CancelToken::new(), None)?;
            Ok(())
        })?;
        return Ok(0);
    }
    let format = Format::ALL.into_iter().find(|f| f.ext() == to || (to == "markdown" && *f == Format::Markdown)).ok_or_else(|| Fail(format!("unknown format {to}")))?;
    if format == Format::Xlsx && target.is_none() {
        return fail("an .xlsx workbook needs -o FILE");
    }
    let sample = buf.read(0..buf.len().min(64 * 1024)).ok_or_else(|| Fail("could not read the file".into()))?;
    let preferred = if file.to_ascii_lowercase().ends_with(".tsv") { b'\t' } else { b',' };
    let dialect = csv::detect(&buf.codec, &sample, sample.len() as u64 == buf.len(), preferred).unwrap_or(csv::Dialect::new(preferred));
    let spec = table_export::Spec { dialect, cols: None, header: !a.flag(&["--no-header"]), format };
    with_output(target, out, |w| {
        table_export::export(&buf, spec, w, &CancelToken::new(), None)?;
        Ok(())
    })?;
    Ok(0)
}

/// Runs a command; returns the exit code. `args` start with the command name.
pub fn run(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let Some(cmd) = args.first().map(|c| c.trim_start_matches('-').to_string()) else {
        let _ = write!(err, "{USAGE}");
        return 2;
    };
    let rest = &args[1..];
    let r = match cmd.as_str() {
        "filter" => cmd_filter(rest, out, false),
        "count" => cmd_filter(rest, out, true),
        "levels" => cmd_levels(rest, out),
        "split" => cmd_split(rest, out),
        "stats" => cmd_stats(rest, out),
        "merge" => cmd_merge(rest, out),
        "convert" => cmd_convert(rest, out),
        "help" | "h" => {
            let _ = write!(out, "{USAGE}");
            return 0;
        }
        _ => fail(format!("unknown command {cmd}")),
    };
    let _ = out.flush();
    match r {
        Ok(code) => code,
        Err(Fail(m)) => {
            let _ = writeln!(err, "bigtext {cmd}: {m}");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_args(args: &[&str]) -> (i32, String, String) {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(&args, &mut out, &mut err);
        (code, String::from_utf8_lossy(&out).into_owned(), String::from_utf8_lossy(&err).into_owned())
    }

    fn file(dir: &tempfile::TempDir, name: &str, text: &[u8]) -> String {
        let p = dir.path().join(name);
        std::fs::write(&p, text).unwrap();
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn filter_count_and_exit_codes() {
        let d = tempfile::tempdir().unwrap();
        let log = file(&d, "app.log", b"10:00:00 INFO a\r\n10:00:01 ERROR b\r\n10:00:02 error c\r\n");
        assert_eq!(run_args(&["filter", "ERROR", &log]), (0, "10:00:01 ERROR b\n".into(), String::new()));
        assert_eq!(run_args(&["--filter", "-i", "error", &log]).1, "10:00:01 ERROR b\n10:00:02 error c\n");
        assert_eq!(run_args(&["filter", "-v", "-E", "ERROR|INFO", &log]).1, "10:00:02 error c\n");
        assert_eq!(run_args(&["filter", "NOPE", &log]).0, 1);
        assert_eq!(run_args(&["count", "-i", "error", &log]), (0, "2\n".into(), String::new()));
        assert_eq!(run_args(&["levels", "error", &log]).1, "10:00:01 ERROR b\n");
        // To a file, in the file's own form (CRLF kept).
        let out = d.path().join("errors.log");
        assert_eq!(run_args(&["filter", "ERROR", &log, "-o", &out.to_string_lossy()]).0, 0);
        assert_eq!(std::fs::read(&out).unwrap(), b"10:00:01 ERROR b\r\n");
        // Errors.
        let (code, _, err) = run_args(&["filter", "x", &d.path().join("missing").to_string_lossy()]);
        assert_eq!(code, 2);
        assert!(err.starts_with("bigtext filter: "), "{err}");
        assert_eq!(run_args(&["filter", "--bogus", "x", &log]).0, 2);
        assert_eq!(run_args(&["frobnicate"]).0, 2);
        assert_eq!(run_args(&["filter", "-E", "(", &log]).0, 2);
        assert!(is_command("--stats") && is_command("merge") && !is_command("--follow"));
    }

    #[test]
    fn split_stats_merge_convert() {
        let d = tempfile::tempdir().unwrap();
        let log = file(&d, "a.log", b"2024-05-01 10:00:00 one\n2024-05-01 10:00:02 two\n2024-05-01 11:00:00 three\n");
        let (code, out, _) = run_args(&["split", "--lines", "2", &log]);
        assert_eq!(code, 0);
        assert_eq!(out.lines().count(), 2);
        assert_eq!(std::fs::read_to_string(d.path().join("a.part002.log")).unwrap(), "2024-05-01 11:00:00 three\n");
        let (_, s, _) = run_args(&["stats", &log]);
        assert!(s.contains("lines: 3\n") && s.contains("encoding: UTF-8") && s.contains("first time: 2024-05-01 10:00:00") && s.contains("last time: 2024-05-01 11:00:00") && s.contains("span: 1 h 00 m 00 s"), "{s}");
        let b = file(&d, "b.log", b"2024-05-01 10:00:01 b1\n");
        assert_eq!(run_args(&["merge", &log, &b]).1.lines().nth(1), Some("b.log │ 2024-05-01 10:00:01 b1"));
        let csv_path = file(&d, "t.csv", b"k,v\nx,1\n");
        assert_eq!(run_args(&["convert", &csv_path, "--to", "jsonl"]).1, "{\"k\": \"x\", \"v\": \"1\"}\n");
        assert_eq!(run_args(&["convert", &csv_path, "--to", "xlsx"]).0, 2);
        let jl = file(&d, "e.jsonl", b"{\"a\": 1}\n");
        assert_eq!(run_args(&["convert", &jl, "--to", "csv"]).1, "a\n1\n");
        assert_eq!(parse_size("100MB"), Some(100 << 20));
        assert_eq!(parse_size("1.5g"), Some(3 << 29));
        assert_eq!(parse_size("x"), None);
    }
}
