//! Short demos for four kinds of users (REJA 2.6): `bigtext --demo DIR --demo-for NAME`, NAME
//! one of `devops`, `data`, `dba`, `soc`. Each makes its own sample file in target/demo-data
//! (kept for the next run), records its frames to DIR/frames-NAME like the main demo, and shows
//! only what the app really does, with the real file size in its captions. The frames become a
//! video with `python scripts/make-demo.py DIR/frames-NAME --name demo-NAME --out DIR`.

use std::fmt::Write as _;
use std::io::BufWriter;

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Audience {
    DevOps,
    Data,
    Dba,
    Soc,
}

impl Audience {
    pub fn parse(s: &str) -> Option<Audience> {
        match s.to_ascii_lowercase().as_str() {
            "devops" => Some(Audience::DevOps),
            "data" => Some(Audience::Data),
            "dba" => Some(Audience::Dba),
            "soc" => Some(Audience::Soc),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Audience::DevOps => "devops",
            Audience::Data => "data",
            Audience::Dba => "dba",
            Audience::Soc => "soc",
        }
    }
}

const GIB: u64 = 1 << 30;
const MIB: u64 = 1 << 20;

/// A small, fixed-seed random generator: the same sample files on every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Writes `path` with `line` until it has `size` bytes, unless a file of at least that size is
/// there already. `line(i, rng, out)` appends line `i`.
fn generate(
    path: &Path,
    size: u64,
    header: &str,
    footer: &str,
    mut line: impl FnMut(u64, &mut Rng, &mut String),
) -> std::io::Result<()> {
    if std::fs::metadata(path).is_ok_and(|m| m.len() >= size) {
        return Ok(());
    }
    let mut w = BufWriter::with_capacity(4 << 20, std::fs::File::create(path)?);
    w.write_all(header.as_bytes())?;
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let (mut written, mut i, mut s) = (header.len() as u64, 0u64, String::with_capacity(1 << 16));
    while written < size {
        s.clear();
        for _ in 0..256 {
            line(i, &mut rng, &mut s);
            i += 1;
        }
        w.write_all(s.as_bytes())?;
        written += s.len() as u64;
    }
    w.write_all(footer.as_bytes())?;
    w.flush()
}

const URLS: [&str; 8] = [
    "/",
    "/api/orders",
    "/api/orders/42",
    "/api/cart",
    "/api/checkout",
    "/static/app.js",
    "/login",
    "/api/search?q=shoes",
];

/// An nginx access log of one hour (14:00 to 15:00), with request times. From 14:32 to 14:36
/// the checkout backend fails: many 502 and 504 answers and slow requests.
fn access_log(path: &Path) -> std::io::Result<()> {
    // About 150 bytes a line: the hour is spread over the lines of 1 GB.
    let lines = GIB / 150;
    generate(path, GIB, "", "", |i, r, s| {
        let ms = i * 3_600_000 / lines;
        let (min, sec) = (ms / 60_000, ms / 1000 % 60);
        let outage = (32..36).contains(&min);
        let url = URLS[r.below(URLS.len() as u64) as usize];
        let (status, secs) = if outage && url.starts_with("/api/") && r.below(100) < 45 {
            (
                [502, 504][r.below(2) as usize],
                2.0 + r.below(7000) as f64 / 1000.0,
            )
        } else {
            let st = match r.below(100) {
                0..=2 => 404,
                3..=7 => 304,
                8 => 500,
                _ => 200,
            };
            (st, 0.004 + r.below(300) as f64 / 1000.0)
        };
        let _ = writeln!(
            s,
            "10.{}.{}.{} - - [20/Nov/2026:14:{min:02}:{sec:02} +0000] \"GET {url} HTTP/1.1\" {status} {} \"-\" \"Mozilla/5.0 (X11; Linux x86_64)\" {secs:.3}",
            r.below(4),
            r.below(256),
            r.below(256),
            200 + r.below(40_000)
        );
    })
}

const CITIES: [&str; 8] = [
    "Tashkent",
    "Samarkand",
    "Bukhara",
    "Namangan",
    "Andijan",
    "Fergana",
    "Nukus",
    "Karshi",
];
const PRODUCTS: [&str; 6] = ["shoes", "jacket", "phone", "laptop", "lamp", "chair"];

/// Orders as CSV: 2 GB, about 28 million rows.
fn orders_csv(path: &Path) -> std::io::Result<()> {
    generate(
        path,
        2 * GIB,
        "order_id,date,customer,city,product,qty,amount,status\n",
        "",
        |i, r, s| {
            let _ = writeln!(
                s,
                "{},2026-{:02}-{:02},C{:06},{},{},{},{}.{:02},{}",
                1_000_000 + i,
                1 + r.below(11),
                1 + r.below(28),
                r.below(400_000),
                CITIES[r.below(8) as usize],
                PRODUCTS[r.below(6) as usize],
                1 + r.below(5),
                1 + r.below(999),
                r.below(100),
                ["paid", "paid", "paid", "shipped", "refunded"][r.below(5) as usize]
            );
        },
    )
}

/// A mysqldump of a shop: small `customers` and `products` tables, then 1 GB of `orders`.
fn shop_dump(path: &Path) -> std::io::Result<()> {
    if std::fs::metadata(path).is_ok_and(|m| m.len() >= GIB) {
        return Ok(());
    }
    let mut w = BufWriter::with_capacity(4 << 20, std::fs::File::create(path)?);
    let mut r = Rng(0x2545_F491_4F6C_DD1D);
    w.write_all(b"-- MySQL dump 10.13  Distrib 8.4.0, for Linux (x86_64)\n--\n-- Host: localhost    Database: shop\n-- ------------------------------------------------------\n/*!40101 SET NAMES utf8mb4 */;\n/*!40014 SET @OLD_FOREIGN_KEY_CHECKS=@@FOREIGN_KEY_CHECKS, FOREIGN_KEY_CHECKS=0 */;\n\n")?;
    let mut table = |w: &mut BufWriter<std::fs::File>,
                     name: &str,
                     cols: &str,
                     rows: u64,
                     row: &mut dyn FnMut(u64, &mut Rng) -> String|
     -> std::io::Result<()> {
        writeln!(w, "--\n-- Table structure for table `{name}`\n--\n\nDROP TABLE IF EXISTS `{name}`;\nCREATE TABLE `{name}` (\n{cols}\n) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;\n\nLOCK TABLES `{name}` WRITE;")?;
        let mut n = 0;
        while n < rows {
            let batch = (rows - n).min(1000);
            let values: Vec<String> = (n..n + batch).map(|i| row(i, &mut r)).collect();
            writeln!(w, "INSERT INTO `{name}` VALUES {};", values.join(","))?;
            n += batch;
        }
        writeln!(w, "UNLOCK TABLES;\n")
    };
    table(
        &mut w,
        "customers",
        "  `id` int NOT NULL,\n  `name` varchar(100) NOT NULL,\n  `city` varchar(50) DEFAULT NULL,\n  PRIMARY KEY (`id`)",
        50_000,
        &mut |i, r| {
            format!(
                "({i},'Customer {i}','{}')",
                CITIES[r.below(8) as usize]
            )
        },
    )?;
    table(
        &mut w,
        "products",
        "  `id` int NOT NULL,\n  `name` varchar(100) NOT NULL,\n  `price` decimal(10,2) NOT NULL,\n  PRIMARY KEY (`id`)",
        5_000,
        &mut |i, r| {
            format!(
                "({i},'{} {i}',{}.{:02})",
                PRODUCTS[r.below(6) as usize],
                1 + r.below(999),
                r.below(100)
            )
        },
    )?;
    // About 48 bytes a row: 1 GB of orders.
    table(
        &mut w,
        "orders",
        "  `id` bigint NOT NULL,\n  `customer_id` int NOT NULL,\n  `product_id` int NOT NULL,\n  `qty` int NOT NULL,\n  `created` datetime NOT NULL,\n  PRIMARY KEY (`id`)",
        GIB / 48,
        &mut |i, r| {
            format!(
                "({i},{},{},{},'2026-{:02}-{:02} 1{}:00:00')",
                r.below(50_000),
                r.below(5_000),
                1 + r.below(5),
                1 + r.below(11),
                1 + r.below(28),
                r.below(10)
            )
        },
    )?;
    w.write_all(b"/*!40014 SET FOREIGN_KEY_CHECKS=@OLD_FOREIGN_KEY_CHECKS */;\n\n-- Dump completed on 2026-11-20 10:00:00\n")?;
    w.flush()
}

/// An application log with what a log should not have when shared: e-mail addresses, bearer
/// tokens, tokens in URLs and a password. A failed-login burst is near the middle.
fn app_log(path: &Path) -> std::io::Result<()> {
    let lines = 64 * MIB / 120;
    generate(path, 64 * MIB, "", "", |i, r, s| {
        let ms = i * 3_600_000 / lines;
        let t = format!(
            "2026-11-20 14:{:02}:{:02}.{:03}",
            ms / 60_000,
            ms / 1000 % 60,
            ms % 1000
        );
        let user = ["ann.k", "bob.t", "dilnoza", "ops-bot"][r.below(4) as usize];
        if (lines / 2..lines / 2 + 12).contains(&i) {
            let _ = writeln!(s, "{t} ERROR [auth] auth failed for user=root@shop.example from 203.0.113.7 password=Winter2026! attempt={}", i - lines / 2 + 1);
            return;
        }
        let _ = match r.below(4) {
            0 => writeln!(
                s,
                "{t} INFO  [auth] login ok user={user}@shop.example ip=10.0.{}.{}",
                r.below(256),
                r.below(256)
            ),
            1 => writeln!(
                s,
                "{t} DEBUG [http] GET /api/orders?page={}&token=tk_{:016x} Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.{:012x}",
                r.below(50),
                r.next(),
                r.next() & 0xFFFF_FFFF_FFFF
            ),
            2 => writeln!(
                s,
                "{t} INFO  [orders] order {} paid by {user}@shop.example amount={}.{:02}",
                100_000 + i,
                1 + r.below(999),
                r.below(100)
            ),
            _ => writeln!(s, "{t} WARN  [db] slow query {} ms", 200 + r.below(800)),
        };
    })
}

/// A scene: on its first frame shows `caption` and runs `start`; once `ready` says so, stays
/// on screen for `ms`.
fn scene(
    caption: &'static str,
    ms: u64,
    mut start: impl FnMut(&mut BigTextApp, &mut Ctx) + 'static,
    mut ready: impl FnMut(&mut BigTextApp) -> bool + 'static,
) -> StepFn {
    Box::new(move |app, c| {
        if c.first() {
            c.caption = Some(caption.into());
            c.mark = 0;
            start(app, c);
            return Step::Wait;
        }
        if c.mark == 0 {
            if !ready(app) {
                return if c.waited() > Duration::from_secs(180) {
                    Step::Fail(format!("{caption}: still waiting"))
                } else {
                    Step::Wait
                };
            }
            c.mark = 1;
            c.mark_time = Instant::now();
        }
        if held(c, ms) {
            Step::Wait
        } else {
            c.mark = 0;
            Step::Next
        }
    })
}

/// Makes the sample file and sets the window size; nothing is recorded yet.
fn setup(make: fn(&Path) -> std::io::Result<()>, file: &'static str) -> StepFn {
    Box::new(move |app, c| {
        if c.first() {
            if let Err(e) = std::fs::create_dir_all(&c.dir).and_then(|_| make(&c.file(file))) {
                return Step::Fail(e.to_string());
            }
            if let Some(ctx) = app.test_ctx() {
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(1280.0, 720.0)));
            }
            return Step::Wait;
        }
        if c.waited() < Duration::from_millis(800) {
            Step::Wait
        } else {
            Step::Next
        }
    })
}

/// The first caption, with the recording started.
fn intro(caption: &'static str) -> StepFn {
    Box::new(move |_app, c| {
        if c.first() {
            c.recording = true;
            c.caption = Some(caption.into());
            c.mark_time = Instant::now();
        }
        if held(c, 2600) {
            Step::Wait
        } else {
            Step::Next
        }
    })
}

/// Opens the sample file; the caption gives its real size.
fn open_scene(file: &'static str, what: &'static str) -> StepFn {
    Box::new(move |app, c| {
        if c.first() {
            app.open_path(c.file(file));
            c.mark = 0;
            return Step::Wait;
        }
        if c.mark == 0 {
            let d = doc(app);
            if d.view.first_paint.is_none() {
                return Step::Wait;
            }
            c.caption = Some(format!(
                "A {} {what}, open at once: only what is on screen is read",
                bigtext::jobs::human_bytes(d.buf.len())
            ));
            c.mark = 1;
            c.mark_time = Instant::now();
        }
        if held(c, 3000) {
            Step::Wait
        } else {
            c.mark = 0;
            Step::Next
        }
    })
}

fn card() -> StepFn {
    Box::new(|_app, c| {
        if c.first() {
            c.caption = None;
            c.card = Some((
                "BigText".into(),
                "Free and open source · Windows, Linux, macOS\nerkinbek.github.io/bigtext".into(),
            ));
            c.mark_time = Instant::now();
        }
        if held(c, 2800) {
            return Step::Wait;
        }
        c.recording = false;
        Step::Next
    })
}

fn filter_done(app: &mut BigTextApp) -> bool {
    doc(app)
        .filter
        .as_ref()
        .is_some_and(|r| r.data.lock().unwrap().done)
}

fn close_filter(app: &mut BigTextApp) {
    app.search.fields = false;
    if let Some(r) = doc(app).filter.take() {
        r.cancel.cancel();
    }
}

fn devops() -> Vec<(&'static str, StepFn)> {
    use crate::app::log_report::Tab;
    vec![
        ("make the access log", setup(access_log, "access.log")),
        (
            "intro",
            intro("For DevOps: what went wrong at 14:32, in an hour of web server log"),
        ),
        ("open", open_scene("access.log", "nginx access log")),
        (
            "report: 5xx per minute",
            scene(
                "Log report: requests and 5xx errors per minute, from one pass over the file",
                5000,
                |app, _| {
                    app.log_report.tab = Tab::Minutes;
                    app.open_log_report();
                },
                |app| {
                    doc(app)
                        .log_report
                        .as_ref()
                        .is_some_and(|r| r.data.lock().unwrap().done)
                },
            ),
        ),
        (
            "field query",
            scene(
                "A query on the log's columns: the 5xx answers of those minutes",
                5000,
                |app, _| {
                    app.log_report.open = false;
                    app.search.query.pattern =
                        ".status >= 500 and .time >= 14:32 and .time < 14:36".into();
                    app.search.fields = true;
                    app.start_filter();
                },
                filter_done,
            ),
        ),
        (
            "slowest requests",
            scene(
                "The slowest requests as a table, each with its line in the log",
                4500,
                |app, _| {
                    close_filter(app);
                    app.report_csv(Tab::Slow, false);
                },
                |app| doc(app).title.starts_with("Slowest requests") && doc(app).is_ready(),
            ),
        ),
        ("card", card()),
    ]
}

fn data() -> Vec<(&'static str, StepFn)> {
    use crate::view::table_view::TableAction;
    use bigtext::csv::{Cond, CondOp};
    vec![
        ("make the CSV", setup(orders_csv, "orders.csv")),
        (
            "intro",
            intro("For data work: a CSV too big for a spreadsheet, without importing it"),
        ),
        ("open", open_scene("orders.csv", "CSV")),
        (
            "last row",
            scene(
                "Ctrl+End: the last of the rows, at once",
                2800,
                |app, _| {
                    let d = doc(app);
                    let end = d.buf.len();
                    d.goto_offset(end);
                },
                |_| true,
            ),
        ),
        (
            "rows that meet conditions",
            scene(
                "Rows where city = Samarkand and amount > 990, read from the whole file into a new table",
                4500,
                |app, _| {
                    app.start_conditions(vec![
                        Cond {
                            col: 3,
                            op: CondOp::Equals,
                            value: "Samarkand".into(),
                        },
                        Cond {
                            col: 6,
                            op: CondOp::Greater,
                            value: "990".into(),
                        },
                    ]);
                },
                |app| doc(app).title.ends_with("(filtered)") && doc(app).is_ready(),
            ),
        ),
        (
            "column statistics",
            scene(
                "Statistics of its amount column",
                4500,
                |app, _| app.on_table_action(TableAction::Stats { col: 6 }),
                |app| app.csv.stats.is_some(),
            ),
        ),
        ("card", card()),
    ]
}

fn dba() -> Vec<(&'static str, StepFn)> {
    vec![
        ("make the dump", setup(shop_dump, "shop.sql")),
        (
            "intro",
            intro("For DBAs: one table out of a big SQL dump, without restoring the database"),
        ),
        ("open", open_scene("shop.sql", "mysqldump")),
        (
            "tables",
            scene(
                "The dump's tables with their rows and sizes, read in one pass",
                5000,
                |app, _| app.open_sql_dump(),
                |app| {
                    doc(app)
                        .sql_dump
                        .as_ref()
                        .is_some_and(|r| r.data.lock().unwrap().done)
                },
            ),
        ),
        (
            "extract a table",
            scene(
                "customers, with the dump's settings: a file that restores on its own",
                5000,
                |app, _| app.save_dump_tables(vec!["customers".into()], true),
                |app| doc(app).title.starts_with("customers") && doc(app).is_ready(),
            ),
        ),
        ("card", card()),
    ]
}

fn soc() -> Vec<(&'static str, StepFn)> {
    vec![
        ("make the log", setup(app_log, "app.log")),
        (
            "intro",
            intro("For security teams: share the lines that matter, not the secrets in them"),
        ),
        ("open", open_scene("app.log", "application log")),
        (
            "find the incident",
            scene(
                "Find All: the failed logins",
                3500,
                |app, _| {
                    app.search.query = bigtext::search::Query {
                        pattern: "auth failed".into(),
                        regex: false,
                        case_sensitive: true,
                        whole_word: false,
                        dot_all: false,
                    };
                    app.search.open = true;
                    app.find_all();
                },
                |app| {
                    doc(app)
                        .results
                        .as_ref()
                        .is_some_and(|r| r.data.lock().unwrap().done)
                },
            ),
        ),
        (
            "share safely",
            scene(
                "Share safely: e-mail addresses, tokens and the password become labels before you paste",
                6500,
                |app, _| {
                    app.search.open = false;
                    app.search.show_results = false;
                    // From a few lines before the first failed login to a few after the last.
                    let d = doc(app);
                    let first = d
                        .results
                        .as_ref()
                        .and_then(|r| r.data.lock().unwrap().matches.first().map(|m| m.0));
                    if let Some(at) = first {
                        let start = line_start_before(d, at.saturating_sub(400));
                        let end = line_start_before(d, (at + 2400).min(d.buf.len()));
                        d.select(start, end);
                        d.view.center();
                    }
                    if let Some(ctx) = app.test_ctx() {
                        app.run(&ctx, crate::app::commands::Cmd::ShareSafely);
                    }
                },
                |app| !app.share.preview().is_empty(),
            ),
        ),
        ("card", card()),
    ]
}

impl SelfTest {
    /// One audience demo (see the module comment).
    pub fn audience_demo(out: PathBuf, who: Audience) -> SelfTest {
        let dir = std::env::current_dir()
            .unwrap_or_default()
            .join("target")
            .join("demo-data");
        let steps = match who {
            Audience::DevOps => devops(),
            Audience::Data => data(),
            Audience::Dba => dba(),
            Audience::Soc => soc(),
        };
        let name = who.name();
        let mut t = SelfTest::build(
            steps,
            out.join(format!("demo-{name}-report.txt")),
            dir,
            None,
        );
        t.recorder = Some(Recorder::new(out.join(format!("frames-{name}"))));
        t
    }
}
