//! Robustness tests for the parsers that read files from anywhere: random and broken input,
//! mixed from the pieces that matter to each parser, must neither panic nor give ranges outside
//! the input. Seeded, so a failing case can be run again.

use std::io::Write;
use std::sync::{Arc, Mutex};

use crate::csv::{self, Dialect, ScanEnd};
use crate::csv_rows::{self, RecordIndex};
use crate::encoding::Codec;
use crate::file::{OpenMode, Source};
use crate::index;
use crate::jobs::CancelToken;
use crate::text::rows::SEG;
use crate::text::TextBuf;

/// How many times more inputs to try: `BIGTEXT_FUZZ_ROUNDS=50 cargo test --lib robustness`
/// for a longer run (1 by default).
fn rounds(n: u64) -> u64 {
    let k = std::env::var("BIGTEXT_FUZZ_ROUNDS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(1);
    n * k.max(1)
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

/// `len` bytes or a little more, picked from `pieces`, with now and then a random byte.
fn noise(r: &mut Rng, pieces: &[&[u8]], len: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(len + 64);
    while v.len() < len {
        if r.below(50) == 0 {
            v.push(r.next() as u8);
        } else {
            v.extend_from_slice(pieces[r.below(pieces.len() as u64) as usize]);
        }
    }
    v
}

/// Up to `max` bytes of [`noise`].
fn noise_upto(r: &mut Rng, pieces: &[&[u8]], max: u64) -> Vec<u8> {
    let n = r.below(max) as usize;
    noise(r, pieces, n)
}

fn buf_with(data: &[u8]) -> (tempfile::NamedTempFile, TextBuf) {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(data).unwrap();
    f.flush().unwrap();
    let src = Arc::new(Source::open(f.path(), OpenMode::Auto).unwrap());
    let codec = Codec::utf8();
    let idx = index::build_blocking(&src, codec);
    let mut t = TextBuf::new(src, idx, codec);
    t.blocking = true;
    (f, t)
}

const CSV_PIECES: &[&[u8]] = &[
    b"a",
    b"12",
    b",",
    b",",
    b";",
    b"\"",
    b"\"\"",
    b"\n",
    b"\r\n",
    b"\r",
    b"\t",
    b" ",
    "\u{171}".as_bytes(),
    "\u{65e5}".as_bytes(),
    b"\"x,y\"",
    b"\"q\nw\"",
];

#[test]
fn csv_records_and_rows_tile_any_bytes() {
    let cancel = CancelToken::new();
    for seed in 1..=rounds(40) {
        let mut r = Rng(seed * 0x9E37_79B9);
        let size = if seed % 5 == 0 {
            3 * SEG as usize
        } else {
            r.below(20_000) as usize
        };
        let data = noise(&mut r, CSV_PIECES, size);
        let (_f, buf) = buf_with(&data);
        let len = buf.len();
        let d = Dialect::new(b',');
        // Records follow one another and their fields stay inside them.
        let mut next = d.first(&buf);
        let end = csv::for_each_record(&buf, d, &cancel, None, |rec| {
            assert_eq!(rec.content.start, next, "seed {seed}");
            assert!(
                rec.content.end <= rec.next && rec.next <= len,
                "seed {seed}"
            );
            for f in rec.fields {
                assert!(rec.content.start <= f.start && f.end <= rec.content.end);
            }
            next = rec.next;
            true
        });
        assert_eq!(end, ScanEnd::Done);
        let _ = csv::detect_table(&buf.codec, &data, true, b',', 0);
        // The table's rows tile the document, stay bounded and are found from any offset.
        let out = Mutex::new(Arc::new(RecordIndex::empty(d)));
        csv_rows::index_records(&buf, d, &cancel, None, &out, &|| {});
        let idx = out.lock().unwrap().clone();
        let mut rows = vec![csv_rows::row_at(&buf, &idx, 0).unwrap()];
        while let Some(row) = csv_rows::next_row(&buf, &idx, rows.last().unwrap()).unwrap() {
            assert!(row.start > rows.last().unwrap().start, "seed {seed}");
            rows.push(row);
        }
        for w in rows.windows(2) {
            assert_eq!(w[0].seg.next, w[1].start, "seed {seed}");
            assert!(w[0].seg.next - w[0].start <= 2 * SEG + 16);
        }
        for _ in 0..50 {
            let p = r.below(len + 1);
            let row = csv_rows::row_at(&buf, &idx, p).unwrap();
            assert!(
                row.start <= p && (p < row.seg.next || row.seg.next == len),
                "seed {seed} at {p}"
            );
        }
    }
}

const SQL_PIECES: &[&[u8]] = &[
    b"CREATE TABLE ",
    b"`t`",
    b"\"public\".\"u\"",
    b" (id int, v text);\n",
    b"INSERT INTO ",
    b" VALUES (1,'a;b'),(2,'c\\'d');\n",
    b"'",
    b"\\'",
    b";",
    b"\n",
    b"-- note\n",
    b"/*",
    b"*/",
    b"/*!40101 SET NAMES utf8 */;\n",
    b"DELIMITER ;;\n",
    b"DELIMITER ;\n",
    b"$$",
    b"$f$",
    b"COPY public.u (id, v) FROM stdin;\n",
    b"1\tx\n",
    b"\\.\n",
    b"SET @saved_cs = 1;\n",
    b"\\restrict abc\n",
    b"ALTER TABLE ONLY u ADD CONSTRAINT f FOREIGN KEY (id) REFERENCES t(id);\n",
    b"CREATE VIEW w AS SELECT 1;\n",
    b"DROP TABLE IF EXISTS `t`;\n",
    b"BEGIN",
    b"END;;\n",
    b" ",
];

#[test]
fn sql_dump_scan_survives_any_dump() {
    let cancel = CancelToken::new();
    for seed in 1..=rounds(200) {
        let mut r = Rng(seed * 0x2545_F491);
        let data = noise_upto(&mut r, SQL_PIECES, 30_000);
        let (_f, buf) = buf_with(&data);
        let len = buf.len();
        let Ok(Some(dump)) = crate::sqldump::scan(&buf, &cancel, None, |_| {}) else {
            continue;
        };
        let inside = |r: &std::ops::Range<u64>| r.start <= r.end && r.end <= len;
        for t in &dump.tables {
            assert!(t.parts.iter().all(|p| inside(&p.range)), "seed {seed}");
        }
        assert!(dump
            .header
            .iter()
            .chain(&dump.prelude)
            .chain(&dump.other)
            .all(|p| inside(&p.range)));
        assert!(inside(&dump.footer), "seed {seed}");
        let all: Vec<usize> = (0..dump.tables.len()).collect();
        let mut out = Vec::new();
        crate::sqldump::write_tables(
            &buf,
            &dump,
            &all,
            crate::sqldump::Extract::WHOLE,
            &mut out,
            &cancel,
            None,
        )
        .unwrap();
    }
}

const LOG_PIECES: &[&[u8]] = &[
    b"10.0.0.1 - - ",
    b"[13/Nov/2026:10:00:00 +0000] ",
    b"\"GET /a?b=1 HTTP/1.1\" ",
    b"200 ",
    b"1234 ",
    b"\"-\" ",
    b"\"Mozilla/5.0\"",
    b"0.012",
    b"<34>",
    b"<165>1 ",
    b"Nov 13 10:00:00 ",
    b"host ",
    b"app[123]: ",
    b"2026-11-13T10:00:00.123Z ",
    b"#Fields: date time cs-uri-stem sc-status\n",
    b"https ",
    b"\n",
    b" ",
    b"\"",
    b"-",
];

#[test]
fn log_formats_survive_any_lines() {
    for seed in 1..=rounds(300) {
        let mut r = Rng(seed * 0x1234_5679);
        let text = noise_upto(&mut r, LOG_PIECES, 4000);
        let lines: Vec<&[u8]> = text.split(|&b| b == b'\n').collect();
        for line in &lines {
            let _ = crate::timestamp::parse_bytes(line);
            let _ = crate::patterns::template(line);
        }
        if let Some(f) = crate::logformat::Format::guess(&lines) {
            let mut s = crate::logformat::Splitter::new(&f);
            for line in &lines {
                let _ = s.split(line);
            }
        }
    }
}

const JSON_PIECES: &[&[u8]] = &[
    b"{",
    b"}",
    b"[",
    b"]",
    b":",
    b",",
    b"\"k\"",
    b"\"\\u00e9\"",
    b"\"\\",
    b"\"\\uD83D\"",
    b"1",
    b"-2.5e3",
    b"true",
    b"null",
    b" ",
    b"\n",
    b"\"open",
    b"0x",
    b"01",
];

#[test]
fn json_check_survives_any_bytes() {
    let cancel = CancelToken::new();
    for seed in 1..=rounds(200) {
        let mut r = Rng(seed * 0x0BAD_5EED);
        let data = noise_upto(&mut r, JSON_PIECES, 8000);
        let (_f, buf) = buf_with(&data);
        let _ = crate::json_check::check_document(&buf, &cancel, None);
    }
}

#[test]
fn queries_and_expressions_survive_any_text() {
    let q: &[&[u8]] = &[
        b".a", b".b.c", b" == ", b" != ", b" > ", b" <= ", b"\"x\"", b"1", b" and ", b" or ",
        b"not ", b"(", b")", b"[0]", b" ~ ", b"\"", b".", b" ",
    ];
    let e: &[&[u8]] = &[
        b"{a}", b"{b}", b"{", b"}", b"+", b"-", b"*", b"/", b"(", b")", b"substr(", b",", b"1",
        b"\"s\"", b"upper(", b" ", b"if(", b">", b"\"",
    ];
    let names = vec!["a".to_string(), "b".to_string()];
    for seed in 1..=rounds(2000) {
        let mut r = Rng(seed * 0x7F4A_7C15);
        let text = String::from_utf8_lossy(&noise_upto(&mut r, q, 60)).into_owned();
        let _ = crate::jsonq::FieldQuery::parse(&text);
        let text = String::from_utf8_lossy(&noise_upto(&mut r, e, 60)).into_owned();
        let _ = crate::csv_expr::Expr::compile(&text, &names);
        let _ = crate::timestamp::parse(&text);
    }
}

#[test]
fn redaction_survives_any_line() {
    let pieces: &[&[u8]] = &[
        b"token=",
        b"Bearer ",
        b"@",
        b"example.com",
        b"\"password\": \"",
        b"\"",
        b"?key=",
        b"&",
        b"10.0.0.",
        b"::",
        b"eyJ",
        b".",
        b" ",
        b"Cookie: ",
        "\u{171}".as_bytes(),
        b"\\",
        b"Authorization: ",
        b"a",
        b"1",
    ];
    let opts = crate::redact::Options {
        ips: true,
        custom: vec![r"(\d+)".into()],
        ..Default::default()
    };
    let mut r = crate::redact::Redactor::new(&opts, [9; 16]).unwrap();
    for seed in 1..=rounds(500) {
        let mut rng = Rng(seed * 0x51ED_2701);
        let line = noise_upto(&mut rng, pieces, 400);
        let mut out = Vec::new();
        r.line(&line, &mut out);
    }
}
