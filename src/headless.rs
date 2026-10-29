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
        More patterns: -e PAT (each must match; with --any, one of them does),
        --not PAT (must not match); all can be repeated.
  levels ERROR,WARN,INFO,DEBUG [CONTEXT] FILE [-o OUT]
        Lines of these log levels.
        CONTEXT: -A N / -B N / -C N lines after / before / around each line,
        --entries the whole log entry (a stack trace under the line), -n line
        numbers (\"12:\" a match, \"13-\" context); \"--\" between groups;
        --from TIME / --to TIME only the lines between those times (\"14:30\",
        \"2024-05-01 14:30:00\"; --to includes its second).
  extract [-i] [-w] [-E] [-g N] [--count] PATTERN FILE [-o OUT]
        Only the matched texts (or capture group N), one per line; with
        --count, each different one with how often it matched, most first.
  count [-v] [-i] [-w] [-E] PATTERN FILE
        The number of lines with a match.
  split (--size 100MB | --lines 1000000) FILE [--out-dir DIR]
        Parts name.part001.ext, … cut at line breaks; prints their paths.
  stats FILE...
        Size, lines, encoding, line endings and the first and last times.
  merge FILE FILE... [-o OUT]
        Log lines of all files in time order, each labelled with its file.
  gaps FILE [--min 30s]
        Places where the next timestamp is at least that much later (or
        earlier: the clock went back), with line numbers. Exit code 1 when
        there are none.
  patterns FILE [--top N] [--rare] [-o OUT]
        The log's message templates (numbers, ids, IPs and paths replaced by
        <n>, <hex>, <ip>, <path>) with how many lines each has, as CSV, most
        lines first (--rare: fewest first).
  join LEFT RIGHT (--on COL | --left-on COL --right-on COL) [--inner] [-o OUT]
        Each row of the LEFT table with the other columns of the RIGHT table's
        rows with the same key (COL: a header name or a 1-based number), as
        CSV; rows without a match get empty fields, or are left out (--inner).
  jsonl FILE [-q QUERY] [-v] [--fields a,b.c] [--to jsonl|csv] [--count] [-o OUT]
        JSON Lines that meet a field query (.level == error and .ms > 500;
        ==, !=, <, >, <=, >=, contains, ~ regex, exists, missing), as they
        are, only some fields (--fields), or as CSV; --count prints how many.
  derive FILE --name NAME --expr EXPR [-o OUT]
        The table with a new last column computed per row, like {price} * {qty},
        substr({ts}, 0, 10) or if({status} = \"ok\", 1, 0).
  pivot FILE --rows COL --cols COL [--value COL --agg sum|mean|min|max]
        [--by hour|day|month] [-o OUT]
        A pivot table as CSV: a row per value of --rows (or per hour, day or
        month of a time column), a column per value of --cols, row counts or
        the aggregated --value in the cells, and a Total column.
  validate FILE [--no-header] [--numeric 3,5]
        Checks a CSV / TSV table: rows with another field count, stray or
        unclosed quotes, control characters, and (--numeric) text in number
        columns (1-based). Prints line: problem; exit code 1 when any.
  values FIELD FILE [-o OUT]
        The values of a JSON Lines or logfmt (key=value) field with how many
        lines have each and their first and last time, as CSV, most frequent
        first. Nested JSON keys: user.id.
  convert FILE --to json|jsonl|md|xlsx|csv [--from KIND] [--path $.a] [-o OUT]
        A CSV / TSV table as JSON, JSON Lines, Markdown or Excel (.xlsx needs
        -o); JSON Lines as CSV or one JSON array; a JSON array (or the one at
        --path, or the first under the top object) as JSON Lines; logfmt
        (key=value) lines as CSV. --from json|jsonl|logfmt|csv when the
        kind is not what it looks like. The header and lines before the table
        (sep=;, comments) are detected; --header / --no-header, --skip N and
        --quote ' override that (also for validate).

Exit codes: 0 done, 1 nothing matched, 2 error.
";

const COMMANDS: [&str; 16] = ["filter", "levels", "count", "split", "stats", "merge", "convert", "values", "gaps", "patterns", "extract", "validate", "join", "pivot", "derive", "jsonl"];

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

    /// Every value of a repeated option, in order.
    fn all(&self, names: &[&str]) -> Vec<&str> {
        self.opts.iter().filter(|(k, _)| names.contains(&k.as_str())).map(|(_, v)| v.as_str()).collect()
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
fn write_kept(buf: &TextBuf, m: &Matcher, spec: FilterSpec, terms: &[filter::Term], numbers: bool, target: Option<&str>, out: &mut dyn Write) -> Res<u64> {
    let cancel = CancelToken::new();
    let mut kept = 0;
    with_output(target, out, |w| {
        let to_file = target.is_some();
        if to_file && !numbers {
            kept = filter::write_lines_to(buf, m, spec, terms, &mut &mut *w, &cancel, None)?.map(|n| n.lines).unwrap_or(0);
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
        let end = filter::filter_lines_with(buf, m, spec, terms, &cancel, None, |ls, le, kind| {
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
const CONTEXT_OPTS: [&str; 8] = ["-A", "--after", "-B", "--before", "-C", "--context", "--from", "--to"];

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
    Ok(FilterSpec { invert, column: None, before, after, entries: a.flag(&["--entries"]), range: None })
}

/// The lines between `--from TIME` and `--to TIME` (up to and including that second).
fn time_range(a: &Args, buf: &TextBuf) -> Res<Option<(u64, u64)>> {
    let (from, to) = (a.opt(&["--from"]), a.opt(&["--to"]));
    if from.is_none() && to.is_none() {
        return Ok(None);
    }
    let parse = |t: Option<&str>| t.map(|t| timestamp::parse(t).ok_or_else(|| Fail(format!("not a time: {t}")))).transpose();
    match timestamp::time_range(buf, parse(from)?, parse(to)?, &CancelToken::new()) {
        Ok(r) => Ok(Some(r)),
        Err(_) => fail("the file has no timestamps"),
    }
}

fn cmd_filter(args: &[String], out: &mut dyn Write, count_only: bool) -> Res<i32> {
    let flags = [&["-v", "--invert", "-i", "--ignore-case", "-w", "--word", "-E", "--regex", "--any"][..], &CONTEXT_FLAGS].concat();
    let a = Args::parse(args, &flags, &[&["-o", "--output", "-e", "--pattern", "--not"][..], &CONTEXT_OPTS].concat())?;
    let mut pats: Vec<String> = a.all(&["-e", "--pattern"]).into_iter().map(String::from).collect();
    let file = match &a.rest[..] {
        [pattern, file] => {
            pats.insert(0, pattern.clone());
            file
        }
        [file] if !pats.is_empty() => file,
        _ => return fail("needs PATTERN (or -e PATTERN) and FILE"),
    };
    let (buf, _) = open(Path::new(file))?;
    // --any: one alternation of the patterns; otherwise every pattern must match.
    let any = a.flag(&["--any"]) && pats.len() > 1;
    let driver = if any {
        let regex = a.flag(&["-E", "--regex"]);
        let alt: Vec<String> = pats.iter().map(|p| format!("(?:{})", if regex { p.clone() } else { regex_syntax::escape(p) })).collect();
        Query { pattern: alt.join("|"), regex: true, ..query(&a, "") }
    } else {
        query(&a, &pats[0])
    };
    let m = Matcher::new(&driver, buf.codec).map_err(Fail)?;
    let mut owned: Vec<(Matcher, bool)> = Vec::new();
    for p in if any { &[][..] } else { &pats[1..] } {
        owned.push((Matcher::new(&query(&a, p), buf.codec).map_err(Fail)?, false));
    }
    for p in a.all(&["--not"]) {
        owned.push((Matcher::new(&query(&a, p), buf.codec).map_err(Fail)?, true));
    }
    let terms: Vec<filter::Term> = owned.iter().map(|(m, negated)| filter::Term { m, negated: *negated }).collect();
    let mut spec = spec_from(&a, a.flag(&["-v", "--invert"]))?;
    spec.range = time_range(&a, &buf)?;
    let n = if count_only {
        let mut n = 0u64;
        filter::filter_lines_with(&buf, &m, FilterSpec { invert: spec.invert, range: spec.range, ..Default::default() }, &terms, &CancelToken::new(), None, |_, _, _| {
            n += 1;
            true
        });
        writeln!(out, "{n}")?;
        n
    } else {
        write_kept(&buf, &m, spec, &terms, a.flag(&["-n", "--line-numbers"]), a.opt(&["-o", "--output"]), out)?
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
    let spec = FilterSpec { range: time_range(&a, &buf)?, ..spec_from(&a, false)? };
    let n = write_kept(&buf, &m, spec, &[], a.flag(&["-n", "--line-numbers"]), a.opt(&["-o", "--output"]), out)?;
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

/// What a file holds, from its first lines: json, jsonl, logfmt or csv.
fn sniff_kind(buf: &TextBuf) -> &'static str {
    let mut r = LineReader::new(buf);
    let mut lines = Vec::new();
    while lines.len() < 10 {
        let Ok(Some((_, l))) = r.next() else { break };
        let t = l.trim_ascii();
        if !t.is_empty() {
            lines.push(t.to_vec());
        }
    }
    match lines.first().and_then(|l| l.first()) {
        Some(b'[') => "json",
        Some(b'{') if lines.len() > 1 && lines.iter().all(|l| l.first() == Some(&b'{') && l.last() == Some(&b'}')) => "jsonl",
        Some(b'{') => "json",
        _ if lines.iter().filter(|l| crate::fields::line_fields(&String::from_utf8_lossy(l)).is_some_and(|f| f.0 == crate::fields::Syntax::Logfmt)).count() * 2 > lines.len() => "logfmt",
        _ => "csv",
    }
}

fn cmd_convert(args: &[String], out: &mut dyn Write) -> Res<i32> {
    let a = Args::parse(args, &["--no-header", "--header"], &["--to", "--from", "--path", "-o", "--output", "--skip", "--quote"])?;
    let [file] = &a.rest[..] else { return fail("needs FILE") };
    let to = a.opt(&["--to"]).ok_or_else(|| Fail("give --to json, jsonl, md, xlsx or csv".into()))?.to_ascii_lowercase();
    let target = a.opt(&["-o", "--output"]);
    let (buf, _) = open(Path::new(file))?;
    let cancel = CancelToken::new();
    let from = match a.opt(&["--from"]) {
        Some(f) => f.to_ascii_lowercase(),
        None => sniff_kind(&buf).to_string(),
    };
    match (from.as_str(), to.as_str()) {
        ("json", "jsonl") => {
            let at = match a.opt(&["--path"]) {
                Some(p) => crate::json::resolve(&buf, &crate::json::parse_path(p).map_err(Fail)?, &cancel).map_err(Fail)?.0,
                None => crate::json::find_array(&buf, &cancel).map_err(Fail)?.0,
            };
            with_output(target, out, |w| {
                crate::json::array_to_lines(&buf, at, &mut &mut *w, &cancel, None).map_err(Fail)?;
                Ok(())
            })?;
            return Ok(0);
        }
        ("jsonl", "json") => {
            with_output(target, out, |w| {
                crate::json::lines_to_array(&buf, &mut &mut *w, &cancel, None)?;
                Ok(())
            })?;
            return Ok(0);
        }
        // One object (per line) as a table, too.
        ("jsonl", "csv") | ("json", "csv") => {
            with_output(target, out, |w| {
                crate::json::jsonl_to_csv(&buf, &mut &mut *w, &cancel, None)?;
                Ok(())
            })?;
            return Ok(0);
        }
        ("logfmt", "csv") => {
            with_output(target, out, |w| {
                crate::fields::logfmt_to_csv(&buf, &mut &mut *w, &cancel, None)?;
                Ok(())
            })?;
            return Ok(0);
        }
        ("csv", _) => {}
        (f, t) => return fail(format!("cannot convert {f} to {t} (json → jsonl, jsonl → json or csv, logfmt → csv, csv → json, jsonl, md, xlsx)")),
    }
    let format = Format::ALL.into_iter().find(|f| f.ext() == to || (to == "markdown" && *f == Format::Markdown)).ok_or_else(|| Fail(format!("unknown format {to}")))?;
    if format == Format::Xlsx && target.is_none() {
        return fail("an .xlsx workbook needs -o FILE");
    }
    let g = table_of(&buf, file, &a)?;
    let spec = table_export::Spec { dialect: g.dialect, cols: None, header: g.header, format };
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

/// The dialect of a table file (by its name and content).
/// The table in `file`: its delimiter, the lines before it and whether it has a header, with
/// `--skip N`, `--quote '` and `--header` / `--no-header` overriding what was detected.
fn table_of(buf: &TextBuf, file: &str, a: &Args) -> Res<csv::TableGuess> {
    let bom = buf.codec.bom_len as u64;
    let sample = buf.read(bom..buf.len().min(bom + 64 * 1024)).ok_or_else(|| Fail("could not read the file".into()))?;
    let preferred = if file.to_ascii_lowercase().ends_with(".tsv") { b'\t' } else { b',' };
    let mut g = csv::detect_table(&buf.codec, &sample, bom + sample.len() as u64 == buf.len(), preferred, bom);
    if let Some(n) = a.opt(&["--skip"]) {
        let n: usize = n.parse().map_err(|_| Fail(format!("not a line count: {n}")))?;
        let mut at = bom;
        for _ in 0..n {
            at = crate::filter::line_end_at(buf, at).unwrap_or(buf.len());
        }
        g.skip = n;
        g.dialect.start = if n == 0 { 0 } else { at };
    }
    match a.opt(&["--quote"]) {
        Some(q) if q.len() == 1 => g.dialect.quote = q.as_bytes()[0],
        Some(q) => return fail(format!("the quote is one character, not {q}")),
        None => {}
    }
    if a.flag(&["--no-header"]) {
        g.header = false;
    } else if a.flag(&["--header"]) {
        g.header = true;
    }
    Ok(g)
}

/// A column given as a header name or a 1-based number.
fn column_index(buf: &TextBuf, g: &csv::TableGuess, col: &str) -> Res<usize> {
    if let Ok(n) = col.parse::<usize>() {
        return if n > 0 { Ok(n - 1) } else { fail("columns count from 1") };
    }
    let mut names = Vec::new();
    csv::for_each_record(buf, g.dialect, &CancelToken::new(), None, |r| {
        names = r.fields.iter().filter_map(|f| buf.read(f.clone()).map(|b| csv::field_value(&buf.codec, g.dialect, &b))).collect();
        false
    });
    names.iter().position(|n| n.trim() == col.trim()).ok_or_else(|| Fail(format!("no column named {col}")))
}

fn cmd_join(args: &[String], out: &mut dyn Write) -> Res<i32> {
    let a = Args::parse(args, &["--inner", "--no-header", "--header"], &["--on", "--left-on", "--right-on", "-o", "--output", "--skip", "--quote"])?;
    let [lf, rf] = &a.rest[..] else { return fail("needs LEFT and RIGHT") };
    let (left, _) = open(Path::new(lf))?;
    let (right, _) = open(Path::new(rf))?;
    let (lg, rg) = (table_of(&left, lf, &a)?, table_of(&right, rf, &a)?);
    let (lc, rc) = match (a.opt(&["--on"]), a.opt(&["--left-on"]), a.opt(&["--right-on"])) {
        (Some(c), None, None) => (c, c),
        (None, Some(l), Some(r)) => (l, r),
        _ => return fail("give --on COL, or --left-on COL and --right-on COL"),
    };
    let spec = crate::csv_join::JoinSpec { left_key: column_index(&left, &lg, lc)?, right_key: column_index(&right, &rg, rc)?, inner: a.flag(&["--inner"]), header: lg.header && rg.header };
    with_output(a.opt(&["-o", "--output"]), out, |w| {
        crate::csv_join::join_to((&left, lg.dialect), (&right, rg.dialect), spec, &mut &mut *w, &CancelToken::new(), None)?;
        Ok(())
    })?;
    Ok(0)
}

fn cmd_jsonl(args: &[String], out: &mut dyn Write) -> Res<i32> {
    let a = Args::parse(args, &["-v", "--invert", "--count"], &["-q", "--query", "--fields", "--to", "-o", "--output"])?;
    let [file] = &a.rest[..] else { return fail("needs FILE") };
    let q = a.opt(&["-q", "--query"]).map(crate::jsonq::FieldQuery::parse).transpose().map_err(Fail)?;
    let invert = a.flag(&["-v", "--invert"]);
    let fields: Vec<String> = a.opt(&["--fields"]).map(|f| f.split(',').map(|s| s.trim().trim_start_matches('.').to_string()).filter(|s| !s.is_empty()).collect()).unwrap_or_default();
    let csv_out = match a.opt(&["--to"]).map(str::to_ascii_lowercase).as_deref() {
        None | Some("jsonl") => false,
        Some("csv") if !fields.is_empty() => true,
        Some("csv") => return fail("--to csv needs --fields"),
        Some(t) => return fail(format!("unknown --to {t} (jsonl, csv)")),
    };
    let (buf, _) = open(Path::new(file))?;
    if buf.codec.unit() != 1 {
        return fail("JSON Lines must be UTF-8");
    }
    let quote = |s: &str| if s.contains([',', '"', '\n', '\r']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_string() };
    let mut n = 0u64;
    with_output(a.opt(&["-o", "--output"]), out, |w| {
        if csv_out {
            writeln!(w, "{}", fields.iter().map(|f| quote(f)).collect::<Vec<_>>().join(","))?;
        }
        let mut r = LineReader::new(&buf);
        while let Some((_, line)) = r.next()? {
            let keep = q.as_ref().is_none_or(|q| q.matches(line)) != invert;
            if !keep || (line.trim_ascii().is_empty() && q.is_none()) {
                continue;
            }
            n += 1;
            if a.flag(&["--count"]) {
                continue;
            }
            if fields.is_empty() {
                w.write_all(line.strip_suffix(b"\r").unwrap_or(line))?;
                w.write_all(b"\n")?;
            } else if csv_out {
                let row: Vec<String> = fields.iter().map(|f| quote(&crate::fields::find_value(line, f, crate::fields::Syntax::Json).unwrap_or_default())).collect();
                writeln!(w, "{}", row.join(","))?;
            } else {
                let parts: Vec<String> = fields.iter().map(|f| format!("\"{}\": {}", f.replace('"', "\\\""), crate::fields::find_raw_json(line, f).map(|v| String::from_utf8_lossy(v).into_owned()).unwrap_or_else(|| "null".into()))).collect();
                writeln!(w, "{{{}}}", parts.join(", "))?;
            }
        }
        if a.flag(&["--count"]) {
            writeln!(w, "{n}")?;
        }
        Ok(())
    })?;
    Ok(if n > 0 { 0 } else { 1 })
}

fn cmd_derive(args: &[String], out: &mut dyn Write) -> Res<i32> {
    let a = Args::parse(args, &["--no-header", "--header"], &["--name", "--expr", "-o", "--output", "--skip", "--quote"])?;
    let [file] = &a.rest[..] else { return fail("needs FILE") };
    let (name, expr) = match (a.opt(&["--name"]), a.opt(&["--expr"])) {
        (Some(n), Some(e)) => (n.to_string(), e.to_string()),
        _ => return fail("give --name NAME and --expr EXPR"),
    };
    let (buf, _) = open(Path::new(file))?;
    let g = table_of(&buf, file, &a)?;
    let mut names = Vec::new();
    if g.header {
        csv::for_each_record(&buf, g.dialect, &CancelToken::new(), None, |r| {
            names = r.fields.iter().filter_map(|f| buf.read(f.clone()).map(|b| csv::field_value(&buf.codec, g.dialect, &b))).collect();
            false
        });
    }
    crate::csv_expr::Expr::compile(&expr, &names).map_err(Fail)?;
    let rw = csv::Rewrite::Derive { name, expr, names, header: g.header };
    with_output(a.opt(&["-o", "--output"]), out, |w| {
        csv::rewrite_to(&buf, g.dialect, &rw, &mut &mut *w, &CancelToken::new(), None)?;
        Ok(())
    })?;
    Ok(0)
}

fn cmd_pivot(args: &[String], out: &mut dyn Write) -> Res<i32> {
    use crate::csv_pivot::{Agg, Bucket, PivotSpec};
    let a = Args::parse(args, &["--no-header", "--header"], &["--rows", "--cols", "--value", "--agg", "--by", "-o", "--output", "--skip", "--quote"])?;
    let [file] = &a.rest[..] else { return fail("needs FILE") };
    let (buf, _) = open(Path::new(file))?;
    let g = table_of(&buf, file, &a)?;
    let col = |name: &str| -> Res<usize> {
        let c = a.opt(&[name]).ok_or_else(|| Fail(format!("give {name} COL")))?;
        column_index(&buf, &g, c)
    };
    let value = a.opt(&["--value"]).map(|c| column_index(&buf, &g, c)).transpose()?;
    let agg = match a.opt(&["--agg"]) {
        None if value.is_some() => Agg::Sum,
        None => Agg::Count,
        Some(n) => Agg::ALL.into_iter().find(|x| x.name() == n.to_ascii_lowercase()).ok_or_else(|| Fail(format!("unknown --agg {n} (count, sum, mean, min, max)")))?,
    };
    if agg != Agg::Count && value.is_none() {
        return fail("--agg needs --value COL");
    }
    let bucket = match a.opt(&["--by"]).map(str::to_ascii_lowercase).as_deref() {
        None => Bucket::AsIs,
        Some("hour") => Bucket::Hour,
        Some("day") => Bucket::Day,
        Some("month") => Bucket::Month,
        Some(b) => return fail(format!("unknown --by {b} (hour, day, month)")),
    };
    let spec = PivotSpec { rows: col("--rows")?, cols: col("--cols")?, value, agg, bucket, header: g.header };
    let p = crate::csv_pivot::pivot(&buf, g.dialect, spec, &CancelToken::new(), None).map_err(|_| Fail("could not read the file".into()))?;
    with_output(a.opt(&["-o", "--output"]), out, |w| Ok(crate::csv_pivot::pivot_to_csv(&p, agg, &mut &mut *w)?))?;
    Ok(0)
}

fn cmd_validate(args: &[String], out: &mut dyn Write) -> Res<i32> {
    let a = Args::parse(args, &["--no-header", "--header"], &["--numeric", "--skip", "--quote"])?;
    let [file] = &a.rest[..] else { return fail("needs FILE") };
    let mut numeric = Vec::new();
    for n in a.opt(&["--numeric"]).unwrap_or("").split(',').map(str::trim).filter(|n| !n.is_empty()) {
        match n.parse::<usize>() {
            Ok(n) if n > 0 => numeric.push(n - 1),
            _ => return fail(format!("not a column number: {n}")),
        }
    }
    let (buf, _) = open(Path::new(file))?;
    let g = table_of(&buf, file, &a)?;
    let c = crate::csv_check::check(&buf, g.dialect, g.header, &numeric, &CancelToken::new(), None, |_| {})?.unwrap_or_default();
    let (mut at, mut line) = (0u64, 0u64);
    for p in &c.problems {
        buf.for_each_chunk(at..p.start, |base, ch| {
            line += buf.codec.count_nl(ch, base);
            true
        })
        .ok_or_else(|| Fail("could not read the file".into()))?;
        at = p.start;
        let what = match p.kind {
            crate::csv_check::Kind::Fields { got, want } => format!("{got} fields instead of {want}"),
            crate::csv_check::Kind::StrayQuote { col } => format!("column {}: a stray quote", col + 1),
            crate::csv_check::Kind::Unclosed { col } => format!("column {}: a quote that is never closed", col + 1),
            crate::csv_check::Kind::Control { col } => format!("column {}: a control character", col + 1),
            crate::csv_check::Kind::NotNumber { col } => format!("column {}: not a number", col + 1),
            crate::csv_check::Kind::Huge => "longer than 64 KB, not checked".to_string(),
        };
        writeln!(out, "{}: {what}", line + 1)?;
    }
    Ok(if c.problems.is_empty() { 0 } else { 1 })
}

fn cmd_extract(args: &[String], out: &mut dyn Write) -> Res<i32> {
    let a = Args::parse(args, &["-i", "--ignore-case", "-w", "--word", "-E", "--regex", "--count"], &["-g", "--group", "-o", "--output"])?;
    let [pattern, file] = &a.rest[..] else { return fail("needs PATTERN and FILE") };
    let group = a.opt(&["-g", "--group"]).map(|g| g.parse::<usize>().map_err(|_| Fail(format!("not a group number: {g}")))).transpose()?.unwrap_or(0);
    let (buf, _) = open(Path::new(file))?;
    let m = Matcher::new(&query(&a, pattern), buf.codec).map_err(Fail)?;
    if group > crate::search::group_names(&m).len() {
        return fail(format!("the pattern has no group {group}"));
    }
    let cancel = CancelToken::new();
    let mut n = 0;
    with_output(a.opt(&["-o", "--output"]), out, |w| {
        if a.flag(&["--count"]) {
            let t = crate::extract::count(&buf, &m, group, 0..buf.len(), &cancel, None)?.unwrap_or_default();
            for (v, c) in &t.values {
                writeln!(w, "{c}\t{v}")?;
            }
            n = t.matches;
        } else {
            n = crate::extract::write_values_to(&buf, &m, group, 0..buf.len(), &mut &mut *w, &cancel, None)?.unwrap_or(0);
        }
        Ok(())
    })?;
    Ok(if n > 0 { 0 } else { 1 })
}

fn cmd_patterns(args: &[String], out: &mut dyn Write) -> Res<i32> {
    let a = Args::parse(args, &["--rare"], &["--top", "-o", "--output"])?;
    let [file] = &a.rest[..] else { return fail("needs FILE") };
    let top = a.opt(&["--top"]).map(|n| n.parse::<usize>().map_err(|_| Fail(format!("not a number: {n}")))).transpose()?;
    let (buf, _) = open(Path::new(file))?;
    let mut p = crate::patterns::build(&buf, None, &CancelToken::new(), None, |_| {})?.unwrap_or_default();
    crate::patterns::sort(&mut p.templates, a.flag(&["--rare"]));
    if let Some(n) = top {
        p.templates.truncate(n);
    }
    with_output(a.opt(&["-o", "--output"]), out, |w| Ok(crate::patterns::to_csv(&p.templates, &mut &mut *w)?))?;
    Ok(0)
}

fn cmd_gaps(args: &[String], out: &mut dyn Write) -> Res<i32> {
    let a = Args::parse(args, &[], &["--min"])?;
    let [file] = &a.rest[..] else { return fail("needs FILE") };
    let min = a.opt(&["--min"]).unwrap_or("30s");
    let min_ms = crate::gaps::parse_duration(min).ok_or_else(|| Fail(format!("not a duration: {min} (30s, 5m, 1h)")))?;
    let (buf, _) = open(Path::new(file))?;
    let g = crate::gaps::scan(&buf, min_ms, &CancelToken::new(), None, |_| {})?.unwrap_or_default();
    // Line numbers, counted forward from gap to gap.
    let (mut at, mut line) = (0u64, 0u64);
    let mut line_of = |off: u64| -> Res<u64> {
        buf.for_each_chunk(at..off, |base, c| {
            line += buf.codec.count_nl(c, base);
            true
        })
        .ok_or_else(|| Fail("could not read the file".into()))?;
        at = off;
        Ok(line + 1)
    };
    for x in &g.gaps {
        let (a1, a2) = (line_of(x.before)?, line_of(x.after)?);
        let key = |k| timestamp::format_key(k, g.dated, true);
        writeln!(out, "{a1}-{a2}: {} ({} -> {})", timestamp::format_delta(x.ms()), key(x.from), key(x.to))?;
    }
    Ok(if g.gaps.is_empty() { 1 } else { 0 })
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
        "gaps" => cmd_gaps(rest, out),
        "patterns" => cmd_patterns(rest, out),
        "extract" => cmd_extract(rest, out),
        "validate" => cmd_validate(rest, out),
        "join" => cmd_join(rest, out),
        "pivot" => cmd_pivot(rest, out),
        "derive" => cmd_derive(rest, out),
        "jsonl" => cmd_jsonl(rest, out),
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
    fn convert_json_and_logfmt() {
        let d = tempfile::tempdir().unwrap();
        let j = file(&d, "a.json", b"{\"n\": 2, \"events\": [ {\"id\": 1}, {\"id\": 2} ]}");
        assert_eq!(run_args(&["convert", &j, "--to", "jsonl"]).1, "{\"id\":1}\n{\"id\":2}\n");
        assert_eq!(run_args(&["convert", &j, "--to", "jsonl", "--path", "$.events[1]"]).0, 2);
        let l = file(&d, "a.jsonl", b"{\"id\":1}\n{\"id\":2}\n");
        assert_eq!(run_args(&["convert", &l, "--to", "json"]).1, "[\n{\"id\":1},\n{\"id\":2}\n]\n");
        let f = file(&d, "a.log", b"level=info msg=ok\nlevel=error msg=\"bad x\"\n");
        assert_eq!(run_args(&["convert", &f, "--to", "csv"]).1, "level,msg\ninfo,ok\nerror,bad x\n");
        assert_eq!(run_args(&["convert", &f, "--to", "json"]).0, 2);
    }

    #[test]
    fn convert_detects_the_table() {
        let d = tempfile::tempdir().unwrap();
        let t = file(&d, "t.csv", b"sep=;\n# note\nk;v\nx;1\n");
        assert_eq!(run_args(&["convert", &t, "--to", "jsonl"]).1, "{\"k\": \"x\", \"v\": \"1\"}\n");
        let n = file(&d, "n.csv", b"1,2\n3,4\n5,6\n");
        assert_eq!(run_args(&["convert", &n, "--to", "jsonl"]).1.lines().next(), Some("{\"column1\": \"1\", \"column2\": \"2\"}"));
        assert_eq!(run_args(&["convert", &n, "--to", "jsonl", "--header"]).1.lines().next(), Some("{\"1\": \"3\", \"2\": \"4\"}"));
        let q = file(&d, "q.csv", b"a,b\n'x,y',2\n");
        assert_eq!(run_args(&["convert", &q, "--to", "jsonl", "--quote", "'"]).1, "{\"a\": \"x,y\", \"b\": \"2\"}\n");
    }

    #[test]
    fn json_lines_queries() {
        let d = tempfile::tempdir().unwrap();
        let f = file(&d, "e.jsonl", b"{\"level\":\"error\",\"ms\":700,\"u\":{\"id\":1}}\n{\"level\":\"info\",\"ms\":20}\n{\"level\":\"error\",\"ms\":5,\"msg\":\"a, b\"}\n");
        assert_eq!(run_args(&["jsonl", &f, "-q", ".level == error and .ms > 500"]).1, "{\"level\":\"error\",\"ms\":700,\"u\":{\"id\":1}}\n");
        assert_eq!(run_args(&["jsonl", &f, "-q", ".level = error", "--count"]).1, "2\n");
        assert_eq!(run_args(&["jsonl", &f, "-q", ".level = error", "-v", "--fields", "level,ms"]).1, "{\"level\": \"info\", \"ms\": 20}\n");
        assert_eq!(run_args(&["jsonl", &f, "--fields", "ms,u.id,msg", "--to", "csv"]).1, "ms,u.id,msg\n700,1,\n20,,\n5,,\"a, b\"\n");
        assert_eq!(run_args(&["jsonl", &f, "-q", ".nope exists"]).0, 1);
        assert_eq!(run_args(&["jsonl", &f, "-q", "level"]).0, 2);
    }

    #[test]
    fn derived_column() {
        let d = tempfile::tempdir().unwrap();
        let t = file(&d, "s.csv", b"price,qty\r\n2.5,4\r\n1,\"x\"\r\n");
        assert_eq!(run_args(&["derive", &t, "--name", "total", "--expr", "{price} * {qty}"]).1, "price,qty,total\r\n2.5,4,10\r\n1,\"x\",\r\n");
        assert_eq!(run_args(&["derive", &t, "--name", "x", "--expr", "{nope}"]).0, 2);
    }

    #[test]
    fn pivot_tables() {
        let d = tempfile::tempdir().unwrap();
        let t = file(&d, "s.csv", b"city,product,qty\nT,tea,3\nB,tea,5\nT,rice,7\n");
        assert_eq!(run_args(&["pivot", &t, "--rows", "city", "--cols", "product"]).1, "city,rice,tea,Total\nT,1,1,2\nB,0,1,1\n");
        assert_eq!(run_args(&["pivot", &t, "--rows", "1", "--cols", "2", "--value", "qty"]).1, "city,rice,tea,Total\nT,7,3,10\nB,,5,5\n");
        assert_eq!(run_args(&["pivot", &t, "--rows", "city", "--cols", "product", "--agg", "mean"]).0, 2);
    }

    #[test]
    fn join_tables() {
        let d = tempfile::tempdir().unwrap();
        let o = file(&d, "o.csv", b"order,user\n1,u1\n2,u3\n");
        let u = file(&d, "u.csv", b"id,name\nu1,Ann\nu2,Bob\n");
        assert_eq!(run_args(&["join", &o, &u, "--left-on", "user", "--right-on", "id"]).1, "order,user,name\n1,u1,Ann\n2,u3,\n");
        assert_eq!(run_args(&["join", &o, &u, "--left-on", "2", "--right-on", "1", "--inner"]).1, "order,user,name\n1,u1,Ann\n");
        assert_eq!(run_args(&["join", &o, &u, "--on", "nope"]).0, 2);
    }

    #[test]
    fn validate_a_table() {
        let d = tempfile::tempdir().unwrap();
        let t = file(&d, "t.csv", b"id,qty\n1,2\n2\n3,x\n");
        assert_eq!(run_args(&["validate", "--numeric", "2", &t]), (1, "3: 1 fields instead of 2\n4: column 2: not a number\n".into(), String::new()));
        let ok = file(&d, "ok.csv", b"a,b\n1,2\n");
        assert_eq!(run_args(&["validate", &ok]).0, 0);
    }

    #[test]
    fn extract_matches() {
        let d = tempfile::tempdir().unwrap();
        let log = file(&d, "a.log", b"GET /a 200\nGET /b 404\nPOST /a 200\n");
        assert_eq!(run_args(&["extract", "-E", "[0-9]{3}", &log]).1, "200\n404\n200\n");
        assert_eq!(run_args(&["extract", "-E", "--count", "-g", "1", r"(GET|POST) /", &log]).1, "2\tGET\n1\tPOST\n");
        assert_eq!(run_args(&["extract", "-E", "-g", "2", "(x)", &log]).0, 2);
        assert_eq!(run_args(&["extract", "nope", &log]).0, 1);
    }

    #[test]
    fn message_patterns() {
        let d = tempfile::tempdir().unwrap();
        let log = file(&d, "a.log", b"10:00:00 INFO req 1 ok\n10:00:01 INFO req 2 ok\n10:00:02 ERROR disk full\n");
        let (code, out, _) = run_args(&["patterns", &log]);
        assert_eq!(code, 0);
        assert_eq!(out.lines().nth(1), Some("2,INFO,INFO req <n> ok,0,23"));
        assert_eq!(run_args(&["patterns", "--rare", "--top", "1", &log]).1.lines().nth(1), Some("1,ERROR,ERROR disk full,46,46"));
    }

    #[test]
    fn between_times() {
        let d = tempfile::tempdir().unwrap();
        let log = file(&d, "a.log", b"2024-05-01 10:00:00 ERROR a\n2024-05-01 10:00:30 ERROR b\n2024-05-01 10:01:00 INFO c\n2024-05-01 10:01:30 ERROR d\n");
        assert_eq!(run_args(&["filter", "ERROR", "--from", "10:00:30", "--to", "10:01:00", &log]).1, "2024-05-01 10:00:30 ERROR b\n");
        assert_eq!(run_args(&["levels", "error", "--from", "2024-05-01 10:00:15", &log]).1, "2024-05-01 10:00:30 ERROR b\n2024-05-01 10:01:30 ERROR d\n");
        assert_eq!(run_args(&["count", "ERROR", "--to", "10:00:30", &log]).1, "2\n");
        assert_eq!(run_args(&["filter", "ERROR", "--from", "noon", &log]).0, 2);
    }

    #[test]
    fn combined_patterns() {
        let d = tempfile::tempdir().unwrap();
        let log = file(&d, "a.log", b"ERROR timeout user=1\nERROR timeout healthcheck\nWARN timeout user=1\nERROR disk\n");
        assert_eq!(run_args(&["filter", "ERROR", "-e", "timeout", "--not", "healthcheck", &log]).1, "ERROR timeout user=1\n");
        assert_eq!(run_args(&["count", "-e", "disk", "-e", "WARN", "--any", &log]).1, "2\n");
        assert_eq!(run_args(&["count", "-e", "ERROR", "-e", "user=1", &log]).1, "1\n");
        assert_eq!(run_args(&["filter", "-E", "--any", "-e", "^WARN", "-e", "disk$", &log]).1, "WARN timeout user=1\nERROR disk\n");
        assert_eq!(run_args(&["filter", &log]).0, 2);
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
    fn gaps_with_line_numbers() {
        let d = tempfile::tempdir().unwrap();
        let log = file(&d, "a.log", b"2024-05-01 10:00:00 a\n2024-05-01 10:00:05 b\n  trace\n2024-05-01 10:02:05 c\n");
        assert_eq!(run_args(&["gaps", &log]), (0, "2-4: +2 m 00.000 s (2024-05-01 10:00:05 -> 2024-05-01 10:02:05)\n".into(), String::new()));
        assert_eq!(run_args(&["gaps", "--min", "5m", &log]).0, 1);
        assert_eq!(run_args(&["gaps", "--min", "soon", &log]).0, 2);
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
