//! Generates the test corpus used for benchmarks and manual testing.
//!
//! ```text
//! gen-testfiles [--out DIR] [--only a,b,...] [--skip-big] [--force]
//! ```
//! Files: random-lines-1g.txt, random-lines-10g.txt, single-line-2g.txt, utf16le.txt,
//! cp1251.txt, binary.bin, mixed-eol.txt, short-lines-100m.txt and a handful of small edge cases.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

const GIB: u64 = 1024 * 1024 * 1024;
const MIB: u64 = 1024 * 1024;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // wyrand
        self.0 = self.0.wrapping_add(0xa076_1d64_78bd_642f);
        let t = (self.0 as u128).wrapping_mul((self.0 ^ 0xe703_7ed1_a0b4_28db) as u128);
        ((t >> 64) ^ t) as u64
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const WORDS: &[&str] = &[
    "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india", "juliet",
    "kilo", "lima", "mike", "november", "oscar", "papa", "quebec", "romeo", "sierra", "tango",
    "uniform", "victor", "whiskey", "xray", "yankee", "zulu", "request", "response", "error",
    "timeout", "connection", "database", "session", "user", "payload", "checksum", "retry",
    "latency", "cache", "miss", "hit", "worker", "thread", "queue", "socket", "buffer", "index",
];
const LEVELS: &[&str] = &["INFO", "DEBUG", "WARN", "ERROR", "TRACE"];
const RU_WORDS: &[&str] = &[
    "привет", "мир", "строка", "файл", "данные", "ошибка", "пользователь", "сервер", "запрос",
    "ответ", "время", "журнал", "проверка", "система", "память", "поиск", "замена", "текст",
];

fn write_until(path: &Path, target: u64, mut fill: impl FnMut(&mut Vec<u8>)) -> io::Result<u64> {
    let mut w = BufWriter::with_capacity(8 * MIB as usize, File::create(path)?);
    let mut buf = Vec::with_capacity(9 * MIB as usize);
    let mut written = 0u64;
    while written < target {
        buf.clear();
        while (buf.len() as u64) < 8 * MIB && written + (buf.len() as u64) < target {
            fill(&mut buf);
        }
        w.write_all(&buf)?;
        written += buf.len() as u64;
    }
    w.flush()?;
    Ok(written)
}

fn log_line(rng: &mut Rng, line_no: &mut u64, buf: &mut Vec<u8>, eol: &[u8]) {
    *line_no += 1;
    let secs = *line_no % 86_400;
    let _ = write!(
        buf,
        "2026-10-{:02} {:02}:{:02}:{:02}.{:03} [{}] id={} ",
        1 + (*line_no / 86_400) % 28,
        secs / 3600,
        (secs / 60) % 60,
        secs % 60,
        rng.below(1000),
        LEVELS[rng.below(LEVELS.len() as u64) as usize],
        rng.below(1_000_000)
    );
    let n = rng.below(24);
    for i in 0..n {
        if i > 0 {
            buf.push(b' ');
        }
        buf.extend_from_slice(WORDS[rng.below(WORDS.len() as u64) as usize].as_bytes());
    }
    buf.extend_from_slice(eol);
}

fn gen_random_lines(path: &Path, size: u64) -> io::Result<u64> {
    let mut rng = Rng(size);
    let mut line_no = 0;
    write_until(path, size, |b| log_line(&mut rng, &mut line_no, b, b"\n"))
}

fn gen_single_line(path: &Path, size: u64) -> io::Result<u64> {
    const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut rng = Rng(7);
    let mut chunk = vec![0u8; 4 * MIB as usize];
    for c in chunk.iter_mut() {
        *c = B64[rng.below(64) as usize];
    }
    // Put a findable marker roughly in the middle.
    let marker = b"NEEDLE_IN_THE_HAYSTACK";
    let mut w = BufWriter::with_capacity(8 * MIB as usize, File::create(path)?);
    let mut written = 0u64;
    while written < size {
        let n = (size - written).min(chunk.len() as u64) as usize;
        if written <= size / 2 && size / 2 < written + n as u64 {
            let at = (size / 2 - written) as usize;
            let mut c = chunk[..n].to_vec();
            let end = (at + marker.len()).min(n);
            c[at..end].copy_from_slice(&marker[..end - at]);
            w.write_all(&c)?;
        } else {
            w.write_all(&chunk[..n])?;
        }
        written += n as u64;
    }
    w.flush()?;
    Ok(written)
}

fn gen_utf16le(path: &Path, size: u64) -> io::Result<u64> {
    let mut rng = Rng(16);
    let mut first = true;
    let mut n = 0u64;
    write_until(path, size, |b| {
        if first {
            b.extend_from_slice(&[0xFF, 0xFE]);
            first = false;
        }
        n += 1;
        let mut s = format!("{n:08} ");
        for _ in 0..rng.below(12) {
            if rng.below(2) == 0 {
                s.push_str(RU_WORDS[rng.below(RU_WORDS.len() as u64) as usize]);
            } else {
                s.push_str(WORDS[rng.below(WORDS.len() as u64) as usize]);
            }
            s.push(' ');
        }
        if n % 1000 == 0 {
            s.push_str("😀 ");
        }
        s.push_str("\r\n");
        b.extend(s.encode_utf16().flat_map(|u| u.to_le_bytes()));
    })
}

fn gen_cp1251(path: &Path, size: u64) -> io::Result<u64> {
    let mut rng = Rng(1251);
    let mut n = 0u64;
    write_until(path, size, |b| {
        n += 1;
        let mut s = format!("{n} ");
        for _ in 0..1 + rng.below(14) {
            s.push_str(RU_WORDS[rng.below(RU_WORDS.len() as u64) as usize]);
            s.push(' ');
        }
        s.push_str("\r\n");
        let (enc, _, _) = encoding_rs::WINDOWS_1251.encode(&s);
        b.extend_from_slice(&enc);
    })
}

fn gen_binary(path: &Path, size: u64) -> io::Result<u64> {
    let mut rng = Rng(0xB1);
    write_until(path, size, |b| {
        for _ in 0..4096 {
            let v = rng.next();
            // ~25% NUL bytes
            let bytes = v.to_le_bytes().map(|x| if x < 64 { 0 } else { x });
            b.extend_from_slice(&bytes);
        }
    })
}

fn gen_mixed_eol(path: &Path, size: u64) -> io::Result<u64> {
    let mut rng = Rng(0xE0);
    let mut line_no = 0;
    write_until(path, size, |b| {
        let eol: &[u8] = if rng.below(2) == 0 { b"\r\n" } else { b"\n" };
        log_line(&mut rng, &mut line_no, b, eol)
    })
}

fn gen_short_lines(path: &Path, lines: u64) -> io::Result<u64> {
    let mut w = BufWriter::with_capacity(8 * MIB as usize, File::create(path)?);
    let mut buf = Vec::with_capacity(9 * MIB as usize);
    let mut written = 0u64;
    let mut i = 0u64;
    while i < lines {
        buf.clear();
        while buf.len() < 8 * MIB as usize && i < lines {
            // 1..=4 digit numbers: very short lines, ~4 bytes average
            let _ = writeln!(buf, "{}", i % 9973);
            i += 1;
        }
        w.write_all(&buf)?;
        written += buf.len() as u64;
    }
    w.flush()?;
    Ok(written)
}

fn gen_small(dir: &Path) -> io::Result<()> {
    std::fs::write(dir.join("empty.txt"), b"")?;
    std::fs::write(dir.join("no-trailing-newline.txt"), b"first line\nsecond line without newline")?;
    std::fs::write(dir.join("cr-only.txt"), b"old\rmac\rline\rendings\r")?;
    std::fs::write(dir.join("bom-in-middle.txt"), "start\n\u{FEFF}after bom\nend\n".as_bytes())?;
    let mut nul = b"text with NUL ".to_vec();
    nul.extend_from_slice(&[0, 0, 0]);
    nul.extend_from_slice(b" bytes\n\xFF\xFE invalid utf8 \xC3\x28 here\n");
    std::fs::write(dir.join("nul-and-invalid.txt"), nul)?;
    let long_path_dir = dir.join("unicode-путь-日本");
    std::fs::create_dir_all(&long_path_dir)?;
    std::fs::write(long_path_dir.join("файл.txt"), "Unicode path test\n".as_bytes())?;
    Ok(())
}

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut out = PathBuf::from("testdata");
    let mut only: Option<Vec<String>> = None;
    let mut skip_big = false;
    let mut force = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--out" => {
                i += 1;
                out = PathBuf::from(&args[i]);
            }
            "--only" => {
                i += 1;
                only = Some(args[i].split(',').map(|s| s.trim().to_string()).collect());
            }
            "--skip-big" => skip_big = true,
            "--force" => force = true,
            "-h" | "--help" => {
                println!("usage: gen-testfiles [--out DIR] [--only name,...] [--skip-big] [--force]");
                return Ok(());
            }
            other => eprintln!("ignoring unknown argument {other}"),
        }
        i += 1;
    }
    std::fs::create_dir_all(&out)?;
    gen_small(&out)?;

    type Gen = fn(&Path, u64) -> io::Result<u64>;
    let jobs: Vec<(&str, u64, Gen, bool)> = vec![
        ("random-lines-1g.txt", GIB, gen_random_lines, false),
        ("single-line-2g.txt", 2 * GIB, gen_single_line, true),
        ("utf16le.txt", 64 * MIB, gen_utf16le, false),
        ("cp1251.txt", 64 * MIB, gen_cp1251, false),
        ("binary.bin", 64 * MIB, gen_binary, false),
        ("mixed-eol.txt", 64 * MIB, gen_mixed_eol, false),
        ("short-lines-100m.txt", 100_000_000, gen_short_lines, false),
        ("random-lines-10g.txt", 10 * GIB, gen_random_lines, true),
    ];
    for (name, size, f, big) in jobs {
        if let Some(only) = &only {
            if !only.iter().any(|o| o == name || name.starts_with(o.as_str())) {
                continue;
            }
        } else if big && skip_big {
            continue;
        }
        let path = out.join(name);
        if !force && path.exists() {
            println!("{name}: exists, skipping (use --force to regenerate)");
            continue;
        }
        let t = Instant::now();
        let n = f(&path, size)?;
        let secs = t.elapsed().as_secs_f64();
        println!("{name}: {:.2} MB in {secs:.1}s ({:.0} MB/s)", n as f64 / MIB as f64, n as f64 / MIB as f64 / secs);
    }
    Ok(())
}
