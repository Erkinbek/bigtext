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

  filter [-v] [-i] [-w] [-E] [CONTEXT] PATTERN FILE [-o OUT]
        Lines with a match (-v: without one; -i: ignore case; -w: whole words;
        -E: regular expression). To OUT in the file's encoding, or to the
        console. Exit code 1 when no line matched.
  levels ERROR,WARN,INFO,DEBUG [CONTEXT] FILE [-o OUT]
        Lines of these log levels.
        CONTEXT: -A N / -B N / -C N lines after / before / around each line,
        --entries the whole log entry (a stack trace under the line), -n line
        numbers (\"12:\" a match, \"13-\" context); \"--\" between groups.
  count [-v] [-i] [-w] [-E] PATTERN FILE
        The number of lines with a match.
  split (--size 100MB | --lines 1000000) FILE [--out-dir DIR]
        Parts name.part001.ext, … cut at line breaks; prints their paths.
  stats FILE...
        Size, lines, encoding, line endings and the first and last times.
  merge FILE FILE... [-o OUT]
        Log lines of all files in time order, each labelled with its file.
  values FIELD FILE [-o OUT]
        The values of a JSON Lines or logfmt (key=value) field with how many
        lines have each and their first and last time, as CSV, most frequent
        first. Nested JSON keys: user.id.
  convert FILE --to json|jsonl|md|xlsx|csv [-o OUT]
        A CSV / TSV table as JSON, JSON Lines, Markdown or Excel (.xlsx needs
        -o), or JSON Lines as CSV.

Exit codes: 0 done, 1 nothing matched, 2 error.
";

const COMMANDS: [&str; 8] = ["filter", "levels", "count", "split", "stats", "merge", "convert", "values"];

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

/// Writes the kept lines: in the file's encoding to a file, as UTF-8 to the console. With
/// `numbers`, each line starts with its line number ("12:" a match, "13-" context, like grep).
fn write_kept(buf: &TextBuf, m: &Matcher, spec: FilterSpec, numbers: bool, target: Option<&str>, out: &mut dyn Write) -> Res<u64> {
    let cancel = CancelToken::new();
    let mut kept = 0;
    with_output(target, out, |w| {
        let to_file = target.is_some();
        if to_file && !numbers {
            kept = filter::write_lines_to(buf, m, spec, &mut &mut *w, &cancel, None)?.map(|n| n.lines).unwrap_or(0);
            return Ok(());
        }
        let codec = buf.codec;
        let bom = codec.bom_len as u64;
        if to_file && bom > 0 {
            w.write_all(&buf.read(0..bom).ok_or_else(|| Fail("could not read the file".into()))?)?;
        }
        let mut err: Option<io::Error> = None;
        let mut prev: Option<u64> = None;
        // Line breaks counted up to an offset: (offset, breaks before it).
        let mut counted = (0u64, 0u64);
        let end = filter::filter_lines(buf, m, spec, &cancel, None, |ls, le, kind| {
            let r = (|| -> io::Result<()> {
                if let Some(pe) = prev.filter(|&pe| spec.has_context() && ls > pe) {
                    if to_file {
                        w.write_all(&codec.encode("--"))?;
                        w.write_all(&filter::line_break_before(buf, pe))?;
                    } else {
                        writeln!(w, "--")?;
                    }
                }
                prev = Some(le);
                let mut prefix = String::new();
                if numbers {
                    let mut n = counted.1;
                    buf.for_each_chunk(counted.0..ls, |base, c| {
                        n += codec.count_nl(c, base);
                        true
                    })
                    .ok_or_else(|| io::Error::other("could not read the file"))?;
                    counted = (ls, n);
                    prefix = format!("{}{}", n + 1, if kind == filter::LineKind::Hit { ':' } else { '-' });
                }
                let bytes = buf.read(ls.max(bom)..le).ok_or_else(|| io::Error::other("could not read the file"))?;
                if to_file {
                    w.write_all(&codec.encode(&prefix))?;
                    w.write_all(&bytes)
                } else {
                    let text = codec.decode_lossy(&bytes);
                    writeln!(w, "{prefix}{}", text.trim_end_matches(['\n', '\r']))
                }
            })();
            if let Err(e) = r {
                err = Some(e);
                return false;
            }
            kept += 1;
            true
        });
        match err {
            Some(e) => Err(e.into()),
            None if end == crate::search::SearchEnd::Unavailable => fail("could not read the file"),
            None => Ok(()),
        }
    })?;
    Ok(kept)
}

/// Flags of the context options (-A/-B/-C take a value).
const CONTEXT_FLAGS: [&str; 3] = ["--entries", "-n", "--line-numbers"];
const CONTEXT_OPTS: [&str; 6] = ["-A", "--after", "-B", "--before", "-C", "--context"];

/// The filter with the context options.
fn spec_from(a: &Args, invert: bool) -> Res<FilterSpec> {
    let num = |names: &[&str]| -> Res<Option<u32>> {
        a.opt(names)
            .map(|v| v.parse::<u32>().ok().filter(|&n| n <= filter::CONTEXT_MAX).ok_or_else(|| Fail(format!("not a line count (0-{}): {v}", filter::CONTEXT_MAX))))
            .transpose()
    };
    let around = num(&["-C", "--context"])?;
    let before = num(&["-B", "--before"])?.or(around).unwrap_or(0);
    let after = num(&["-A", "--after"])?.or(around).unwrap_or(0);
    Ok(FilterSpec { invert, column: None, before, after, entries: a.flag(&["--entries"]) })
}

fn cmd_filter(args: &[String], out: &mut dyn Write, count_only: bool) -> Res<i32> {
    let flags = [&["-v", "--invert", "-i", "--ignore-case", "-w", "--word", "-E", "--regex"][..], &CONTEXT_FLAGS].concat();
    let a = Args::parse(args, &flags, &[&["-o", "--output"][..], &CONTEXT_OPTS].concat())?;
    let [pattern, file] = &a.rest[..] else { return fail("needs PATTERN and FILE") };
    let (buf, _) = open(Path::new(file))?;
    let m = Matcher::new(&query(&a, pattern), buf.codec).map_err(Fail)?;
    let spec = spec_from(&a, a.flag(&["-v", "--invert"]))?;
    let n = if count_only {
        let mut n = 0u64;
        filter::filter_lines(&buf, &m, FilterSpec { invert: spec.invert, ..Default::default() }, &CancelToken::new(), None, |_, _, _| {
            n += 1;
            true
        });
        writeln!(out, "{n}")?;
        n
    } else {
        write_kept(&buf, &m, spec, a.flag(&["-n", "--line-numbers"]), a.opt(&["-o", "--output"]), out)?
    };
    Ok(if n > 0 { 0 } else { 1 })
}

fn cmd_levels(args: &[String], out: &mut dyn Write) -> Res<i32> {
    let a = Args::parse(args, &CONTEXT_FLAGS, &[&["-o", "--output"][..], &CONTEXT_OPTS].concat())?;
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
    let n = write_kept(&buf, &m, spec_from(&a, false)?, a.flag(&["-n", "--line-numbers"]), a.opt(&["-o", "--output"]), out)?;
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

fn cmd_values(args: &[String], out: &mut dyn Write) -> Res<i32> {
    let a = Args::parse(args, &[], &["-o", "--output"])?;
    let [key, file] = &a.rest[..] else { return fail("needs FIELD and FILE") };
    let (buf, _) = open(Path::new(file))?;
    // JSON Lines when the first non-empty line is an object, otherwise logfmt.
    let mut r = LineReader::new(&buf);
    let mut syntax = crate::fields::Syntax::Logfmt;
    while let Some((_, l)) = r.next()? {
        match l.trim_ascii().first() {
            None => continue,
            Some(b'{') => syntax = crate::fields::Syntax::Json,
            Some(_) => {}
        }
        break;
    }
    let c = crate::fields::value_counts(&buf, key, syntax, &CancelToken::new(), None)?.unwrap_or_default();
    with_output(a.opt(&["-o", "--output"]), out, |w| Ok(crate::fields::counts_to_csv(&c, key, &mut &mut *w)?))?;
    Ok(if c.lines > 0 { 0 } else { 1 })
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
        "values" => cmd_values(rest, out),
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
    fn context_options() {
        let d = tempfile::tempdir().unwrap();
        let log = file(&d, "app.log", b"2024-05-01 10:00:00 INFO a\n2024-05-01 10:00:01 ERROR b\n\tat x.Y(Y.java:1)\n2024-05-01 10:00:02 INFO c\n2024-05-01 10:00:03 INFO d\n2024-05-01 10:00:04 ERROR e\n");
        let (code, out, _) = run_args(&["filter", "-n", "-B", "1", "ERROR", &log]);
        assert_eq!(code, 0);
        assert_eq!(out, "1-2024-05-01 10:00:00 INFO a\n2:2024-05-01 10:00:01 ERROR b\n--\n5-2024-05-01 10:00:03 INFO d\n6:2024-05-01 10:00:04 ERROR e\n");
        assert_eq!(run_args(&["levels", "--entries", "error", &log]).1, "2024-05-01 10:00:01 ERROR b\n\tat x.Y(Y.java:1)\n--\n2024-05-01 10:00:04 ERROR e\n");
        assert_eq!(run_args(&["filter", "-C", "1", "c$", &log, "-E"]).1.lines().count(), 3);
        assert_eq!(run_args(&["count", "-C", "3", "ERROR", &log]).1, "2\n");
        let o = d.path().join("o.log");
        run_args(&["filter", "-n", "-A", "1", "ERROR", &log, "-o", &o.to_string_lossy()]);
        assert_eq!(std::fs::read_to_string(&o).unwrap(), "2:2024-05-01 10:00:01 ERROR b\n3-\tat x.Y(Y.java:1)\n--\n6:2024-05-01 10:00:04 ERROR e\n");
        assert_eq!(run_args(&["filter", "-A", "x", "ERROR", &log]).0, 2);
    }

    #[test]
    fn values_of_a_field() {
        let d = tempfile::tempdir().unwrap();
        let jl = file(&d, "e.jsonl", b"\n{\"level\":\"info\"}\n{\"level\":\"error\"}\n{\"level\":\"info\"}\n");
        assert_eq!(run_args(&["values", "level", &jl]), (0, "level,lines,percent,first,last\ninfo,2,66.67,,\nerror,1,33.33,,\n".into(), String::new()));
        let lf = file(&d, "a.log", b"ts=2024-05-01T10:00:00Z status=200\nts=2024-05-01T10:00:09Z status=200\n");
        assert_eq!(run_args(&["values", "status", &lf]).1.lines().nth(1), Some("200,2,100.00,2024-05-01 10:00:00,2024-05-01 10:00:09"));
        assert_eq!(run_args(&["values", "nope", &lf]).0, 1);
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
