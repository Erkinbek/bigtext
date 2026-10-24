//! Core benchmarks on a real file (no GUI).
//!
//! ```text
//! bigtext-bench FILE [--literal TEXT]... [--regex RE]... [--replace FROM=TO] [--chunked] [--no-replace]
//! ```
//! Measures open + detection, first screen layout, go-to before/after indexing, index
//! throughput and memory, literal/regex search throughput and streaming replace-all
//! throughput with peak private memory.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bigtext::encoding;
use bigtext::file::{OpenMode, Source};
use bigtext::index::{self, LineIndex};
use bigtext::jobs::{human_bytes, CancelToken, Progress};
use bigtext::replace::Template;
use bigtext::save::{self, Transform};
use bigtext::search::{self, Matcher, Query};
use bigtext::sysinfo;
use bigtext::text::{rows, TextBuf};

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

fn mem() -> String {
    sysinfo::private_bytes().map(human_bytes).unwrap_or_else(|| "n/a".into())
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn first_screen(buf: &TextBuf, top: u64, rows_n: usize) -> usize {
    let mut r = rows::row_at(buf, top, None).expect("row");
    let mut chars = 0;
    for _ in 0..rows_n {
        if let Some(b) = buf.read(r.start..r.end.min(r.start + 400)) {
            chars += buf.codec.decode_lossy(&b).chars().count();
        }
        match rows::next_row(buf, &r, None).expect("row") {
            Some(n) => r = n,
            None => break,
        }
    }
    chars
}

fn goto_bench(buf: &TextBuf, n: usize, max_line: u64, seed: u64) -> (f64, f64, bool) {
    let mut rng = Rng(seed);
    let (mut total, mut worst) = (0.0f64, 0.0f64);
    let mut all_exact = true;
    for _ in 0..n {
        let line = rng.next() % max_line.max(1);
        let t = Instant::now();
        let (off, exact) = buf.line_start(line).unwrap_or((buf.len(), false));
        let off = if exact { off } else { rows::seg_start(buf, off).unwrap() };
        let _ = first_screen(buf, off, 50);
        let e = ms(t.elapsed());
        all_exact &= exact;
        total += e;
        worst = worst.max(e);
    }
    (total / n as f64, worst, all_exact)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut file = None;
    let mut literals = Vec::new();
    let mut regexes = Vec::new();
    let mut replace: Option<(String, String)> = None;
    let mut mode = OpenMode::Auto;
    let mut do_replace = true;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--literal" => literals.push(args.next().expect("--literal TEXT")),
            "--regex" => regexes.push(args.next().expect("--regex RE")),
            "--replace" => {
                let v = args.next().expect("--replace FROM=TO");
                let (a, b) = v.split_once('=').expect("FROM=TO");
                replace = Some((a.into(), b.into()));
            }
            "--chunked" => mode = OpenMode::Chunked,
            "--no-replace" => do_replace = false,
            _ => file = Some(PathBuf::from(a)),
        }
    }
    let file = file.expect("usage: bigtext-bench FILE [--literal T] [--regex R] [--replace A=B]");
    if literals.is_empty() {
        literals.push("timeout connection".into());
    }
    if regexes.is_empty() {
        regexes.push(r"id=99\d{4} ".into());
        regexes.push(r"ERROR\] id=\d+ timeout".into());
    }
    let replace = replace.unwrap_or(("timeout".into(), "TIMEOUT!".into()));

    println!("file: {}", file.display());
    println!("private memory at start: {}", mem());

    // 1. Open + detect.
    let t = Instant::now();
    let src = Arc::new(Source::open(&file, mode).expect("open"));
    let n = src.len().min(1 << 20);
    let det = encoding::detect(&src.read(0..n).unwrap(), n == src.len());
    let open_ms = ms(t.elapsed());
    println!(
        "size: {} ({} bytes), backing: {}, encoding: {}, line endings: {}",
        human_bytes(src.len()),
        src.len(),
        src.kind_name(),
        det.codec.enc.name(),
        det.line_ending.name()
    );
    println!("open + detect: {open_ms:.2} ms");

    // 2. First screen before any indexing.
    let empty_index = LineIndex::new(src.len());
    let mut buf = TextBuf::new(src.clone(), empty_index, det.codec);
    buf.blocking = true;
    let t = Instant::now();
    let chars = first_screen(&buf, 0, 60);
    println!("first screen (60 rows, {chars} chars): {:.3} ms  => open-to-first-screen {:.2} ms", ms(t.elapsed()), open_ms + ms(t.elapsed()));

    // 3. Go-to before indexing (estimated positions).
    let est_lines = (src.len() as f64 / 120.0) as u64;
    let (avg, worst, _) = goto_bench(&buf, 200, est_lines, 1);
    println!("go-to line before indexing (estimate + layout of 50 rows): avg {avg:.3} ms, max {worst:.3} ms");

    // 4. Index.
    let t = Instant::now();
    let idx = index::build_blocking(&src, det.codec);
    let it = t.elapsed();
    println!(
        "index: {:.2} s ({:.0} MB/s), {} lines, index memory {} ({:.4}% of file)",
        it.as_secs_f64(),
        src.len() as f64 / 1048576.0 / it.as_secs_f64().max(1e-9),
        idx.total_lines().unwrap_or(0),
        human_bytes(idx.memory_bytes() as u64),
        idx.memory_bytes() as f64 * 100.0 / src.len().max(1) as f64
    );
    let mut buf = TextBuf::new(src.clone(), idx.clone(), det.codec);
    buf.blocking = true;
    println!("private memory with file open + index: {}", mem());

    // 5. Go-to after indexing.
    let lines = idx.total_lines().unwrap_or(1);
    let (avg, worst, exact) = goto_bench(&buf, 2000, lines, 2);
    println!("go-to line after indexing (exact={exact}, + layout of 50 rows): avg {avg:.3} ms, max {worst:.3} ms");

    // 6/7. Search throughput.
    let run = |q: &Query, label: &str| {
        let m = Matcher::new(q, det.codec).expect("pattern");
        let t = Instant::now();
        let mut count = 0u64;
        search::search_forward(&buf, &m, 0..buf.len(), &CancelToken::new(), None, |_, _| {
            count += 1;
            true
        });
        let s = t.elapsed().as_secs_f64();
        println!(
            "{label} {:?}: {count} matches in {:.2} s = {:.0} MB/s ({:.2} GB/s){}",
            q.pattern,
            s,
            buf.len() as f64 / 1048576.0 / s.max(1e-9),
            buf.len() as f64 / 1073741824.0 / s.max(1e-9),
            if m.is_literal() { " [memmem]" } else { " [regex]" }
        );
    };
    for l in &literals {
        run(&Query { pattern: l.clone(), case_sensitive: true, ..Default::default() }, "literal");
        run(&Query { pattern: l.clone(), case_sensitive: false, ..Default::default() }, "literal (ignore case)");
    }
    for r in &regexes {
        run(&Query { pattern: r.clone(), regex: true, case_sensitive: true, ..Default::default() }, "regex");
    }

    // 8. Streaming replace-all into a temp file next to the input (deleted afterwards).
    if do_replace {
        let q = Query { pattern: replace.0.clone(), case_sensitive: true, ..Default::default() };
        let m = Matcher::new(&q, det.codec).unwrap();
        let tpl = Template::new(&replace.1, false);
        let before = sysinfo::private_bytes().unwrap_or(0);
        let peak = Arc::new(std::sync::atomic::AtomicU64::new(before));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let sampler = {
            let (peak, stop) = (peak.clone(), stop.clone());
            std::thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    if let Some(p) = sysinfo::private_bytes() {
                        peak.fetch_max(p, std::sync::atomic::Ordering::Relaxed);
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            })
        };
        let t = Instant::now();
        let target = file.with_extension("bench-replace-out");
        let r = save::write_temp(&buf, &target, Transform::Replace { matcher: &m, template: &tpl, range: 0..buf.len() }, &CancelToken::new(), &Progress::default());
        let s = t.elapsed().as_secs_f64();
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = sampler.join();
        match r {
            Ok((temp, stats)) => {
                println!(
                    "replace all {:?} -> {:?}: {} replacements, wrote {} in {:.2} s = {:.0} MB/s (incl. fsync); private memory before {} peak {}",
                    replace.0,
                    replace.1,
                    stats.count,
                    human_bytes(stats.bytes_out),
                    s,
                    buf.len() as f64 / 1048576.0 / s.max(1e-9),
                    human_bytes(before),
                    human_bytes(peak.load(std::sync::atomic::Ordering::Relaxed))
                );
                let _ = std::fs::remove_file(temp);
            }
            Err(e) => println!("replace failed: {e}"),
        }
    }
    println!("private memory at end (file open, idle): {}", mem());
}
