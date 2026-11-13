//! SQL dumps — mysqldump (MySQL, MariaDB), pg_dump's plain format and SQLite's `.dump` — read
//! in one streaming pass into their tables: where each table's statements are (its structure,
//! its rows as INSERT statements or a COPY block, its keys, indexes and triggers), how many rows
//! and bytes it has, and the dump's opening and closing settings. A table, or several, can then
//! be written to a file of their own that restores on its own, and a dump split into a file per
//! table (with one more for what belongs to no table: views, functions, events).
//!
//! The reader knows enough SQL to find where statements end: quoted strings (with MySQL's
//! backslash escapes), quoted names, comments, MySQL's `/*!…*/` and `DELIMITER`, PostgreSQL's
//! dollar quotes, COPY data and psql's backslash commands.

use std::collections::HashMap;
use std::io::{self, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::jobs::{CancelToken, Progress};
use crate::lines::LineReader;
use crate::text::TextBuf;

/// Bytes of a statement's start kept to tell what it is.
const HEAD: usize = 400;
/// Time between partial results.
const PARTIAL_EVERY: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Dialect {
    MySql,
    Postgres,
    Sqlite,
    #[default]
    Other,
}

impl Dialect {
    pub fn name(self) -> &'static str {
        match self {
            Dialect::MySql => "MySQL / MariaDB dump",
            Dialect::Postgres => "PostgreSQL dump",
            Dialect::Sqlite => "SQLite dump",
            Dialect::Other => "SQL dump",
        }
    }

    /// From the dump's first bytes.
    pub fn sniff(head: &[u8]) -> Dialect {
        let has = |s: &[u8]| head.windows(s.len()).any(|w| w.eq_ignore_ascii_case(s));
        if has(b"MySQL dump") || has(b"MariaDB dump") || has(b"/*!40101") || has(b"ENGINE=") {
            Dialect::MySql
        } else if has(b"PostgreSQL database dump") || has(b"pg_catalog.") {
            Dialect::Postgres
        } else if has(b"PRAGMA foreign_keys") || has(b"BEGIN TRANSACTION;") {
            Dialect::Sqlite
        } else {
            Dialect::Other
        }
    }
}

/// A part of a table: a byte range, and text to write after it (MySQL's `DELIMITER ;` after a
/// trigger that changed the delimiter).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    pub range: Range<u64>,
    pub tail: Option<&'static str>,
    /// A foreign key added after the data (pg_dump), which needs the table it refers to.
    pub foreign: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Table {
    /// As written, without quotes (`public.orders` with its schema).
    pub name: String,
    /// In file order; the comments before a statement belong to it.
    pub parts: Vec<Part>,
    /// Offset of its CREATE TABLE (or its first statement).
    pub first: u64,
    pub created: bool,
    /// INSERT rows (tuples) or COPY lines.
    pub rows: u64,
    pub statements: u64,
}

impl Table {
    pub fn bytes(&self) -> u64 {
        self.parts.iter().map(|p| p.range.end - p.range.start).sum()
    }
}

#[derive(Clone, Debug, Default)]
pub struct Dump {
    pub dialect: Dialect,
    /// In the order they first appear.
    pub tables: Vec<Table>,
    /// The settings before the first table (SET, psql's backslash commands).
    pub header: Vec<Part>,
    /// What the dump defines before its first table: functions, types, schemas, extensions.
    pub prelude: Vec<Part>,
    /// The settings after the last statement of anything else.
    pub footer: Range<u64>,
    /// What belongs to no table, after the header (views, functions, events, sequences of no
    /// table).
    pub other: Vec<Part>,
    pub statements: u64,
}

/// What goes into a file of some tables.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Extract {
    /// The dump's opening and closing settings.
    pub settings: bool,
    /// What the dump defines before its first table (a trigger's function, a column's type).
    pub prelude: bool,
    /// Foreign keys added after the data.
    pub foreign: bool,
}

impl Extract {
    /// A file that restores on its own.
    pub const WHOLE: Extract = Extract { settings: true, prelude: true, foreign: true };
    /// Only the tables' own statements.
    pub const BARE: Extract = Extract { settings: false, prelude: false, foreign: true };
}

impl Dump {
    pub fn find(&self, name: &str) -> Option<usize> {
        self.tables.iter().position(|t| t.name == name).or_else(|| {
            // Without its schema, when that is unambiguous.
            let short: Vec<usize> = self.tables.iter().enumerate().filter(|(_, t)| t.name.rsplit('.').next() == Some(name)).map(|(i, _)| i).collect();
            (short.len() == 1).then(|| short[0])
        })
    }
}

/// What a statement is.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Stmt {
    /// A table's statement; `rows_list`: an INSERT with a column list (its first parenthesis
    /// is not a row).
    Table { name: String, create: bool, insert: bool, columns: bool, copy: bool, foreign: bool },
    /// UNLOCK TABLES: the table LOCK TABLES named.
    Unlock,
    /// A sequence (PostgreSQL), and the table that owns it.
    Sequence { name: String, owner: Option<String> },
    /// A view (mysqldump drops a table of its name first).
    View(String),
    /// SET, COMMIT, PRAGMA, psql's backslash commands: settings.
    Setting,
    Other,
}

/// A statement's start without MySQL's version comments (`/*!40101 `, `*/`).
fn clean_head(head: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(head.len());
    let mut i = 0;
    while i < head.len() {
        if head[i..].starts_with(b"/*!") {
            i += 3;
            while i < head.len() && head[i].is_ascii_digit() {
                i += 1;
            }
            out.push(b' ');
        } else if head[i..].starts_with(b"*/") {
            i += 2;
            out.push(b' ');
        } else {
            out.push(head[i]);
            i += 1;
        }
    }
    out
}

fn contains_ci(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w.eq_ignore_ascii_case(needle))
}

/// Words and names of a statement's start.
struct Words<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Words<'a> {
    fn new(b: &'a [u8]) -> Self {
        Words { b, i: 0 }
    }

    fn skip_space(&mut self) {
        while self.i < self.b.len() && self.b[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }

    /// The next bare word, upper-cased (`None` at a quote or the end).
    fn word(&mut self) -> Option<String> {
        self.skip_space();
        let s = self.i;
        while self.i < self.b.len() && (self.b[self.i].is_ascii_alphanumeric() || self.b[self.i] == b'_') {
            self.i += 1;
        }
        (self.i > s).then(|| String::from_utf8_lossy(&self.b[s..self.i]).to_ascii_uppercase())
    }

    /// Takes `w` (case-insensitive) when it is next.
    fn take(&mut self, w: &str) -> bool {
        let save = self.i;
        if self.word().as_deref() == Some(w) {
            return true;
        }
        self.i = save;
        false
    }

    /// A name: `name`, `"Name"`, `` `name` ``, `schema.name`, `[name]`; quotes taken off.
    fn name(&mut self) -> Option<String> {
        self.skip_space();
        let mut out = String::new();
        loop {
            let c = *self.b.get(self.i)?;
            match c {
                b'`' | b'"' | b'[' => {
                    let close = if c == b'[' { b']' } else { c };
                    self.i += 1;
                    let s = self.i;
                    loop {
                        match self.b.get(self.i) {
                            None => return None,
                            Some(&x) if x == close && self.b.get(self.i + 1) == Some(&close) && close != b']' => self.i += 2,
                            Some(&x) if x == close => break,
                            _ => self.i += 1,
                        }
                    }
                    let doubled = [close, close];
                    out.push_str(&String::from_utf8_lossy(&self.b[s..self.i]).replace(std::str::from_utf8(&doubled).unwrap_or(""), std::str::from_utf8(&doubled[..1]).unwrap_or("")));
                    self.i += 1;
                }
                _ if c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c >= 0x80 => {
                    let s = self.i;
                    while self.i < self.b.len() && (self.b[self.i].is_ascii_alphanumeric() || matches!(self.b[self.i], b'_' | b'$') || self.b[self.i] >= 0x80) {
                        self.i += 1;
                    }
                    out.push_str(&String::from_utf8_lossy(&self.b[s..self.i]));
                }
                _ => return None,
            }
            if self.b.get(self.i) == Some(&b'.') {
                self.i += 1;
                out.push('.');
            } else {
                break;
            }
        }
        (!out.is_empty()).then_some(out)
    }

    /// Skips to just after the word `w` (not inside quotes).
    fn find(&mut self, w: &str) -> bool {
        while self.i < self.b.len() {
            match self.b[self.i] {
                b'\'' | b'"' | b'`' => {
                    let q = self.b[self.i];
                    self.i += 1;
                    while self.i < self.b.len() && self.b[self.i] != q {
                        self.i += 1;
                    }
                    self.i += 1;
                }
                c if c.is_ascii_alphabetic() => {
                    if self.word().as_deref() == Some(w) {
                        return true;
                    }
                }
                _ => self.i += 1,
            }
        }
        false
    }

    fn next_is(&mut self, c: u8) -> bool {
        self.skip_space();
        self.b.get(self.i) == Some(&c)
    }
}

fn classify(head: &[u8]) -> Stmt {
    let head = clean_head(head);
    let mut w = Words::new(&head);
    let table = |name: Option<String>, create: bool| name.map_or(Stmt::Other, |name| Stmt::Table { name, create, insert: false, columns: false, copy: false, foreign: false });
    let Some(first) = w.word() else {
        return if head.trim_ascii_start().starts_with(b"\\") { Stmt::Setting } else { Stmt::Other };
    };
    match first.as_str() {
        "SET" | "COMMIT" | "BEGIN" | "START" | "PRAGMA" | "USE" | "END" | "ROLLBACK" => Stmt::Setting,
        "SELECT" => {
            // pg_dump: SELECT pg_catalog.setval('public.orders_id_seq', 42, true);
            let s = String::from_utf8_lossy(&head);
            match s.find("setval(").and_then(|i| s[i + 7..].trim_start().strip_prefix('\'').map(|r| r.split('\'').next().unwrap_or("").to_string())) {
                Some(name) if !name.is_empty() => Stmt::Sequence { name, owner: None },
                _ if s.contains("set_config(") => Stmt::Setting,
                _ => Stmt::Other,
            }
        }
        "DROP" => {
            if w.take("TABLE") {
                w.take("IF");
                w.take("EXISTS");
                table(w.name(), false)
            } else if w.take("SEQUENCE") {
                w.take("IF");
                w.take("EXISTS");
                w.name().map_or(Stmt::Other, |name| Stmt::Sequence { name, owner: None })
            } else if w.take("VIEW") {
                w.take("IF");
                w.take("EXISTS");
                w.name().map_or(Stmt::Other, Stmt::View)
            } else {
                Stmt::Other
            }
        }
        "CREATE" => {
            w.take("OR");
            w.take("REPLACE");
            for _ in 0..3 {
                if !(w.take("GLOBAL") || w.take("LOCAL") || w.take("TEMPORARY") || w.take("TEMP") || w.take("UNLOGGED") || w.take("UNIQUE")) {
                    break;
                }
            }
            let save = w.i;
            match w.word().as_deref() {
                Some("TABLE") => {
                    w.take("IF");
                    w.take("NOT");
                    w.take("EXISTS");
                    table(w.name(), true)
                }
                Some("SEQUENCE") => {
                    w.take("IF");
                    w.take("NOT");
                    w.take("EXISTS");
                    w.name().map_or(Stmt::Other, |name| Stmt::Sequence { name, owner: None })
                }
                Some("INDEX") => {
                    if w.find("ON") {
                        w.take("ONLY");
                        table(w.name(), false)
                    } else {
                        Stmt::Other
                    }
                }
                _ => {
                    // CREATE DEFINER=… TRIGGER name … ON table (MySQL), CREATE TRIGGER (others).
                    w.i = save;
                    if w.find("TRIGGER") {
                        let _ = w.name();
                        if w.find("ON") {
                            return table(w.name(), false);
                        }
                    }
                    // CREATE ALGORITHM=… DEFINER=… VIEW name (MySQL), CREATE VIEW (others).
                    w.i = save;
                    if w.find("VIEW") {
                        return w.name().map_or(Stmt::Other, Stmt::View);
                    }
                    Stmt::Other
                }
            }
        }
        "ALTER" => {
            if w.take("TABLE") {
                w.take("IF");
                w.take("EXISTS");
                w.take("ONLY");
                match table(w.name(), false) {
                    Stmt::Table { name, .. } if contains_ci(&head, b"FOREIGN KEY") => Stmt::Table { name, create: false, insert: false, columns: false, copy: false, foreign: true },
                    other => other,
                }
            } else if w.take("SEQUENCE") {
                let name = w.name();
                let owner = if w.find("OWNED") && w.take("BY") { w.name().and_then(|c| c.rsplit_once('.').map(|(t, _)| t.to_string())) } else { None };
                name.map_or(Stmt::Other, |name| Stmt::Sequence { name, owner })
            } else {
                Stmt::Other
            }
        }
        "LOCK" => {
            w.take("TABLES");
            w.take("TABLE");
            table(w.name(), false)
        }
        "UNLOCK" => Stmt::Unlock,
        "INSERT" | "REPLACE" => {
            for _ in 0..3 {
                if !(w.take("LOW_PRIORITY") || w.take("DELAYED") || w.take("HIGH_PRIORITY") || w.take("IGNORE") || w.take("OR")) {
                    break;
                }
                // INSERT OR REPLACE / OR IGNORE (SQLite).
                w.take("REPLACE");
                w.take("IGNORE");
            }
            w.take("INTO");
            match w.name() {
                Some(name) => {
                    let columns = w.next_is(b'(');
                    Stmt::Table { name, create: false, insert: true, columns, copy: false, foreign: false }
                }
                None => Stmt::Other,
            }
        }
        "COPY" => match w.name() {
            Some(name) => {
                let copy = String::from_utf8_lossy(&head).to_ascii_lowercase().contains("from stdin");
                Stmt::Table { name, create: false, insert: false, columns: false, copy, foreign: false }
            }
            None => Stmt::Other,
        },
        _ => Stmt::Other,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum St {
    Code,
    Single,
    Double,
    Back,
    Block,
    Dollar,
    Copy,
}

/// A sequence's statements and the table that owns it.
type Seq = (Vec<Part>, Option<String>);

/// Who the statements without a table of their own (settings between them) go with.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Owner {
    Table(usize),
    Sequence(String),
    Other,
}

/// Reads a dump statement by statement.
struct Reader {
    dialect: Dialect,
    st: St,
    dollar: Vec<u8>,
    delimiter: Vec<u8>,
    /// Inside a MySQL `/*! … */` (its `*/` is not a comment end).
    exec: bool,
    /// Where the statement being read is: its span starts after the previous statement (so the
    /// comments before it are its), its first byte, its start, and for an INSERT its
    /// parenthesis depth and the parenthesised groups at depth 0 (rows).
    span: u64,
    start: Option<u64>,
    head: Vec<u8>,
    depth: i32,
    groups: u64,
    // The result.
    dump: Dump,
    index: HashMap<String, usize>,
    sequences: HashMap<String, Seq>,
    locked: Option<String>,
    /// The table whose COPY data is being read.
    copying: Option<usize>,
    /// What the last statement of an object went with.
    last: Option<Owner>,
    views: std::collections::HashSet<String>,
    /// The end of the last statement of an object: the closing settings start there.
    footer: u64,
}

impl Reader {
    fn new(dialect: Dialect) -> Reader {
        Reader {
            dialect,
            st: St::Code,
            dollar: Vec::new(),
            delimiter: b";".to_vec(),
            exec: false,
            span: 0,
            start: None,
            head: Vec::new(),
            depth: 0,
            groups: 0,
            dump: Dump { dialect, ..Default::default() },
            index: HashMap::new(),
            sequences: HashMap::new(),
            locked: None,
            copying: None,
            last: None,
            views: Default::default(),
            footer: 0,
        }
    }

    fn table(&mut self, name: &str, at: u64) -> usize {
        if let Some(&i) = self.index.get(name) {
            return i;
        }
        self.dump.tables.push(Table { name: name.to_string(), first: at, ..Default::default() });
        let i = self.dump.tables.len() - 1;
        self.index.insert(name.to_string(), i);
        i
    }

    fn add_part(parts: &mut Vec<Part>, range: Range<u64>, tail: Option<&'static str>, foreign: bool) {
        match parts.last_mut() {
            Some(p) if p.range.end == range.start && p.tail.is_none() && p.foreign == foreign => {
                p.range.end = range.end;
                p.tail = tail;
            }
            _ => parts.push(Part { range, tail, foreign }),
        }
    }

    fn add_to(&mut self, owner: &Owner, range: Range<u64>, tail: Option<&'static str>, foreign: bool) {
        match owner {
            Owner::Table(i) => Self::add_part(&mut self.dump.tables[*i].parts, range, tail, foreign),
            Owner::Sequence(name) => Self::add_part(&mut self.sequences.entry(name.clone()).or_default().0, range, tail, false),
            Owner::Other => Self::add_part(&mut self.dump.other, range, tail, false),
        }
    }

    /// The statement that started at `self.start` ends at `end`.
    fn finish(&mut self, end: u64) {
        let Some(start) = self.start.take() else { return };
        let span = self.span..end;
        self.span = end;
        self.dump.statements += 1;
        let tail = (self.delimiter != b";").then_some("\nDELIMITER ;\n");
        let mut foreign = false;
        let owner = match classify(&self.head) {
            Stmt::Table { name, create, insert, columns, copy, foreign: fk } => {
                foreign = fk;
                let i = self.table(&name, start);
                let t = &mut self.dump.tables[i];
                if create && !t.created {
                    t.created = true;
                    t.first = start;
                }
                t.statements += 1;
                if insert {
                    t.rows += self.groups.saturating_sub(columns as u64);
                }
                if self.head.len() >= 4 && self.head[..4].eq_ignore_ascii_case(b"LOCK") {
                    self.locked = Some(name);
                }
                if copy {
                    self.copying = Some(i);
                    self.st = St::Copy;
                }
                Some(Owner::Table(i))
            }
            Stmt::Unlock => Some(self.locked.take().and_then(|n| self.index.get(&n).copied()).map_or(Owner::Other, Owner::Table)),
            Stmt::Sequence { name, owner } => {
                let e = self.sequences.entry(name.clone()).or_default();
                if owner.is_some() {
                    e.1 = owner;
                }
                Some(Owner::Sequence(name))
            }
            // Settings go with what came before them (MySQL sets and restores the client's
            // character set around each CREATE TABLE and trigger).
            Stmt::Setting => {
                if self.last.is_none() {
                    Self::add_part(&mut self.dump.header, span, tail, false);
                } else if let Some(o) = self.last.clone() {
                    self.add_to(&o, span, tail, false);
                    // Restoring what was saved before the object (`@saved_cs_client`) ends that
                    // object, so the closing settings start after it.
                    if contains_ci(&self.head, b"@saved_") {
                        self.footer = end;
                    }
                }
                return;
            }
            Stmt::View(name) => {
                self.views.insert(name);
                Some(Owner::Other)
            }
            // Before the first table: what the tables may need (functions, types, schemas).
            Stmt::Other if self.last.is_none() => {
                Self::add_part(&mut self.dump.prelude, span, tail, false);
                return;
            }
            Stmt::Other => Some(Owner::Other),
        };
        let Some(owner) = owner else { return };
        self.add_to(&owner, span, tail, foreign);
        self.last = Some(owner);
        self.footer = end;
    }

    /// One line at `off` (without its line break).
    fn line(&mut self, off: u64, l: &[u8]) {
        let n = l.len();
        if self.st == St::Copy {
            if l == b"\\." {
                if let Some(i) = self.copying.take() {
                    let end = off + n as u64;
                    Self::add_part(&mut self.dump.tables[i].parts, self.span..end, None, false);
                    self.span = end;
                    self.footer = end;
                }
                self.st = St::Code;
            } else if let Some(i) = self.copying {
                self.dump.tables[i].rows += 1;
            }
            return;
        }
        if self.st == St::Code && self.start.is_none() {
            let t = l.trim_ascii_start();
            // psql's \connect, \restrict…: a statement of one line.
            if t.starts_with(b"\\") {
                self.start = Some(off);
                self.head.clear();
                self.head.extend_from_slice(&t[..t.len().min(HEAD)]);
                self.finish(off + n as u64);
                return;
            }
            if t.len() >= 10 && t[..10].eq_ignore_ascii_case(b"DELIMITER ") {
                let d = t[10..].trim_ascii();
                if !d.is_empty() {
                    self.delimiter = d.to_vec();
                }
                return;
            }
        }
        // The statement's start goes on over this line.
        if self.start.is_some() && self.head.len() < HEAD {
            self.head.push(b' ');
            let k = (HEAD - self.head.len()).min(n);
            self.head.extend_from_slice(&l[..k]);
        }
        let mut i = 0;
        while i < n {
            let b = l[i];
            match self.st {
                St::Code => {
                    if b.is_ascii_whitespace() {
                        i += 1;
                        continue;
                    }
                    let next = l.get(i + 1).copied();
                    // Comments start no statement.
                    if b == b'-' && next == Some(b'-') {
                        break;
                    }
                    if b == b'/' && next == Some(b'*') && l.get(i + 2) != Some(&b'!') {
                        self.st = St::Block;
                        i += 2;
                        continue;
                    }
                    if self.start.is_none() {
                        self.start = Some(off + i as u64);
                        self.head.clear();
                        self.head.extend_from_slice(&l[i..(i + HEAD).min(n)]);
                        self.depth = 0;
                        self.groups = 0;
                    }
                    match b {
                        b'\'' => self.st = St::Single,
                        b'"' => self.st = St::Double,
                        b'`' => self.st = St::Back,
                        b'/' if next == Some(b'*') => {
                            self.exec = true;
                            i += 3;
                            continue;
                        }
                        b'*' if self.exec && next == Some(b'/') => {
                            self.exec = false;
                            i += 2;
                            continue;
                        }
                        b'$' if self.dialect != Dialect::MySql => {
                            let tag = l[i + 1..].iter().take_while(|c| c.is_ascii_alphanumeric() || **c == b'_').count();
                            if l.get(i + 1 + tag) == Some(&b'$') && !l.get(i + 1).is_some_and(u8::is_ascii_digit) {
                                self.dollar = l[i..i + 2 + tag].to_vec();
                                self.st = St::Dollar;
                                i += 2 + tag;
                                continue;
                            }
                        }
                        b'(' => {
                            if self.depth == 0 {
                                self.groups += 1;
                            }
                            self.depth += 1;
                        }
                        b')' => self.depth -= 1,
                        _ => {}
                    }
                    if b == self.delimiter[0] && l[i..].starts_with(&self.delimiter) {
                        let d = self.delimiter.len();
                        self.finish(off + (i + d) as u64);
                        i += d;
                        continue;
                    }
                    i += 1;
                }
                St::Single | St::Double | St::Back => {
                    let q = match self.st {
                        St::Single => b'\'',
                        St::Double => b'"',
                        _ => b'`',
                    };
                    let escapes = self.dialect == Dialect::MySql && q != b'`';
                    let rest = &l[i..];
                    let hit = if escapes { memchr::memchr2(q, b'\\', rest) } else { memchr::memchr(q, rest) };
                    match hit {
                        None => i = n,
                        Some(k) => {
                            let at = i + k;
                            if l[at] == b'\\' || l.get(at + 1) == Some(&q) {
                                // An escaped character, or a doubled quote.
                                i = at + 2;
                            } else {
                                self.st = St::Code;
                                i = at + 1;
                            }
                        }
                    }
                }
                St::Block => match memchr::memmem::find(&l[i..], b"*/") {
                    Some(k) => {
                        self.st = St::Code;
                        i += k + 2;
                    }
                    None => i = n,
                },
                St::Dollar => match memchr::memmem::find(&l[i..], &self.dollar) {
                    Some(k) => {
                        i += k + self.dollar.len();
                        self.st = St::Code;
                    }
                    None => i = n,
                },
                St::Copy => break,
            }
        }
    }

    fn end(mut self, len: u64) -> Dump {
        if self.start.is_some() {
            self.finish(len);
        }
        let opening = self.dump.header.iter().chain(&self.dump.prelude).map(|p| p.range.end).max().unwrap_or(0);
        let footer = if self.last.is_none() { len } else { self.footer.max(opening) };
        self.dump.footer = footer..len;
        // Sequences go with the table that owns them; the others with what belongs to no table.
        let mut seqs: Vec<(String, Seq)> = self.sequences.into_iter().collect();
        seqs.sort_by_key(|(_, (parts, _))| parts.first().map_or(0, |p| p.range.start));
        for (_, (parts, owner)) in seqs {
            match owner.and_then(|o| self.index.get(&o).copied()) {
                Some(i) => self.dump.tables[i].parts.extend(parts),
                None => self.dump.other.extend(parts),
            }
        }
        // A "table" mysqldump only drops before making a view of its name is that view.
        let (views, mut kept) = (std::mem::take(&mut self.views), Vec::new());
        for t in std::mem::take(&mut self.dump.tables) {
            if !t.created && t.rows == 0 && views.contains(&t.name) {
                self.dump.other.extend(t.parts);
            } else {
                kept.push(t);
            }
        }
        self.dump.tables = kept;
        // The closing settings are the footer's, not the last object's.
        let trim = |parts: &mut Vec<Part>| {
            parts.retain(|p| p.range.start < footer);
            for p in parts.iter_mut() {
                p.range.end = p.range.end.min(footer);
            }
            parts.sort_by_key(|p| p.range.start);
        };
        for t in &mut self.dump.tables {
            trim(&mut t.parts);
        }
        trim(&mut self.dump.other);
        self.dump
    }
}

/// Reads the dump's tables; `partial` gets what is known so far every second. `None` when
/// cancelled.
pub fn scan(buf: &TextBuf, cancel: &CancelToken, progress: Option<&Progress>, mut partial: impl FnMut(&Dump)) -> io::Result<Option<Dump>> {
    let first = buf.read(0..buf.len().min(64 * 1024)).map(|b| b.to_vec()).unwrap_or_default();
    let mut r = Reader::new(Dialect::sniff(&first));
    if let Some(p) = progress {
        p.set_total(buf.len());
    }
    let mut lines = LineReader::new(buf);
    let (mut n, mut shown) = (0u64, Instant::now());
    while let Some((off, l)) = lines.next()? {
        r.line(off, l);
        n += 1;
        if n.is_multiple_of(8192) {
            if cancel.is_cancelled() {
                return Ok(None);
            }
            if let Some(p) = progress {
                p.set_done(lines.taken());
            }
            if shown.elapsed() >= PARTIAL_EVERY {
                partial(&r.dump);
                shown = Instant::now();
            }
        }
    }
    Ok(Some(r.end(buf.len())))
}

/// Writes `ranges` of `buf` to `out` one after another, each on lines of its own.
fn write_parts<W: Write + ?Sized>(buf: &TextBuf, parts: &[Part], out: &mut W, cancel: &CancelToken, progress: Option<&Progress>, last: &mut u8) -> io::Result<Option<u64>> {
    let mut total = 0;
    for p in parts {
        if p.range.is_empty() {
            continue;
        }
        let first = buf.read(p.range.start..p.range.start + 1).map(|b| b.to_vec()).unwrap_or_default();
        if *last != b'\n' && first.first() != Some(&b'\n') {
            out.write_all(b"\n")?;
        }
        match crate::filter::write_range_to(buf, p.range.clone(), &mut &mut *out, cancel, progress)? {
            Some(n) => total += n,
            None => return Ok(None),
        }
        *last = buf.read(p.range.end - 1..p.range.end).and_then(|b| b.first().copied()).unwrap_or(b'\n');
        if let Some(t) = p.tail {
            out.write_all(t.as_bytes())?;
            *last = b'\n';
        }
    }
    Ok(Some(total))
}

/// The opening parts (settings, and with `prelude` the definitions before the first table), in
/// file order.
fn opening(dump: &Dump, prelude: bool) -> Vec<Part> {
    let mut v: Vec<Part> = dump.header.to_vec();
    if prelude {
        v.extend(dump.prelude.iter().cloned());
    }
    v.sort_by_key(|p| p.range.start);
    v
}

/// Writes `middle` between the opening and closing settings (when `settings`).
fn write_file<W: Write + ?Sized>(buf: &TextBuf, dump: &Dump, middle: Vec<Part>, x: Extract, out: &mut W, cancel: &CancelToken, progress: Option<&Progress>) -> io::Result<Option<u64>> {
    let settings = x.settings;
    let mut all = if settings { opening(dump, x.prelude) } else { Vec::new() };
    all.extend(middle);
    if settings && !dump.footer.is_empty() {
        all.push(Part { range: dump.footer.clone(), tail: None, foreign: false });
    }
    let mut last = b'\n';
    let r = write_parts(buf, &all, out, cancel, progress, &mut last)?;
    if r.is_some() && last != b'\n' {
        out.write_all(b"\n")?;
    }
    Ok(r)
}

/// Writes tables `which` as one SQL file, with what `x` asks for. Returns the bytes written, or
/// `None` when cancelled.
pub fn write_tables<W: Write + ?Sized>(buf: &TextBuf, dump: &Dump, which: &[usize], x: Extract, out: &mut W, cancel: &CancelToken, progress: Option<&Progress>) -> io::Result<Option<u64>> {
    let mut parts: Vec<Part> = which.iter().filter_map(|&i| dump.tables.get(i)).flat_map(|t| t.parts.iter().cloned()).filter(|p| x.foreign || !p.foreign).collect();
    parts.sort_by_key(|p| p.range.start);
    write_file(buf, dump, parts, x, out, cancel, progress)
}

/// A file name for a table: its name with characters files cannot have replaced.
pub fn file_name(stem: &str, table: &str) -> String {
    let clean: String = table.chars().map(|c| if c.is_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' }).collect();
    format!("{stem}.{clean}.sql")
}

/// A file name of a split dump: `0` before the tables, `1` a table, `2` after them, so the
/// names sort in the order the files restore.
fn split_name(stem: &str, order: u8, what: &str) -> String {
    let clean: String = what.chars().map(|c| if c.is_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' }).collect();
    format!("{stem}.{order}-{clean}.sql")
}

/// Writes the dump into `dir` as files that restore it in the order of their names (the order
/// returned): `stem.0-before-tables.sql` (what the dump defines before its tables),
/// `stem.1-TABLE.sql` per table (in any order), `stem.2-after-tables.sql` (views, functions,
/// events and the foreign keys added after the data). Each has the dump's settings when `settings`. Returns the files with
/// their sizes, or `None` when cancelled.
pub fn split(buf: &TextBuf, dump: &Dump, dir: &Path, stem: &str, settings: bool, cancel: &CancelToken, progress: Option<&Progress>) -> io::Result<Option<Vec<(PathBuf, u64)>>> {
    let size = |parts: &[Part]| parts.iter().map(|p| p.range.end - p.range.start).sum::<u64>();
    if let Some(p) = progress {
        p.set_total(dump.tables.iter().map(Table::bytes).sum::<u64>() + size(&dump.other) + size(&dump.prelude));
    }
    let mut out = Vec::new();
    let mut used: HashMap<String, u32> = HashMap::new();
    let mut unique = |name: String| {
        let n = used.entry(name.to_lowercase()).or_insert(0);
        *n += 1;
        if *n == 1 {
            name
        } else {
            format!("{}-{n}.sql", name.trim_end_matches(".sql"))
        }
    };
    let write = |name: String, middle: Vec<Part>, out: &mut Vec<(PathBuf, u64)>| -> io::Result<bool> {
        let path = dir.join(name);
        let mut w = io::BufWriter::new(std::fs::File::create(&path)?);
        let Some(n) = write_file(buf, dump, middle, Extract { settings, prelude: false, foreign: true }, &mut w, cancel, progress)? else { return Ok(false) };
        w.flush()?;
        out.push((path, n));
        Ok(true)
    };
    if !dump.prelude.is_empty() && !write(unique(split_name(stem, 0, "before-tables")), dump.prelude.clone(), &mut out)? {
        return Ok(None);
    }
    let mut after: Vec<Part> = dump.other.clone();
    for t in &dump.tables {
        let (fk, own): (Vec<Part>, Vec<Part>) = t.parts.iter().cloned().partition(|p| p.foreign);
        after.extend(fk);
        if !write(unique(split_name(stem, 1, &t.name)), own, &mut out)? {
            return Ok(None);
        }
    }
    after.sort_by_key(|p| p.range.start);
    if !after.is_empty() && !write(unique(split_name(stem, 2, "after-tables")), after, &mut out)? {
        return Ok(None);
    }
    Ok(Some(out))
}

/// The tables as CSV: name, rows, bytes, whether the dump creates it.
pub fn tables_csv<W: Write + ?Sized>(dump: &Dump, out: &mut W) -> io::Result<()> {
    writeln!(out, "table,rows,bytes,created")?;
    for t in &dump.tables {
        let name = if t.name.contains([',', '"']) { format!("\"{}\"", t.name.replace('"', "\"\"")) } else { t.name.clone() };
        writeln!(out, "{name},{},{},{}", t.rows, t.bytes(), if t.created { "yes" } else { "no" })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding;
    use crate::file::{OpenMode, Source};
    use crate::index::LineIndex;
    use std::sync::Arc;

    fn buf(text: &str) -> (tempfile::TempDir, TextBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("d.sql");
        std::fs::write(&path, text).unwrap();
        let src = Arc::new(Source::open(&path, OpenMode::Auto).unwrap());
        let det = encoding::detect(&src.read(0..src.len()).unwrap(), true);
        let len = src.len();
        (dir, TextBuf::new(src, LineIndex::new(len), det.codec))
    }

    fn read(text: &str) -> (tempfile::TempDir, TextBuf, Dump) {
        let (d, b) = buf(text);
        let dump = scan(&b, &CancelToken::new(), None, |_| {}).unwrap().unwrap();
        (d, b, dump)
    }

    fn extract(b: &TextBuf, d: &Dump, names: &[&str], settings: bool) -> String {
        let which: Vec<usize> = names.iter().map(|n| d.find(n).unwrap()).collect();
        let mut out = Vec::new();
        write_tables(b, d, &which, if settings { Extract::WHOLE } else { Extract::BARE }, &mut out, &CancelToken::new(), None).unwrap().unwrap();
        String::from_utf8(out).unwrap()
    }

    fn text(src: &str, parts: &[Part]) -> String {
        parts.iter().map(|p| &src[p.range.start as usize..p.range.end as usize]).collect()
    }

    const MYSQL: &str = "-- MySQL dump 10.13  Distrib 8.4.0, for Linux (x86_64)
--
-- Host: localhost    Database: shop
/*!40101 SET @OLD_CHARACTER_SET_CLIENT=@@CHARACTER_SET_CLIENT */;
/*!40101 SET NAMES utf8mb4 */;
/*!40014 SET @OLD_FOREIGN_KEY_CHECKS=@@FOREIGN_KEY_CHECKS, FOREIGN_KEY_CHECKS=0 */;

--
-- Table structure for table `customers`
--

DROP TABLE IF EXISTS `customers`;
/*!40101 SET @saved_cs_client     = @@character_set_client */;
CREATE TABLE `customers` (
  `id` int NOT NULL AUTO_INCREMENT,
  `name` varchar(100) DEFAULT NULL COMMENT 'a; tricky (comment)',
  PRIMARY KEY (`id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
/*!40101 SET character_set_client = @saved_cs_client */;

--
-- Dumping data for table `customers`
--

LOCK TABLES `customers` WRITE;
/*!40000 ALTER TABLE `customers` DISABLE KEYS */;
INSERT INTO `customers` VALUES (1,'Ann; (the first)'),(2,'Bob\\'s \\\\ shop'),(3,'it''s');
INSERT INTO `customers` VALUES (4,'Dan');
/*!40000 ALTER TABLE `customers` ENABLE KEYS */;
UNLOCK TABLES;

--
-- Table structure for table `orders`
--

DROP TABLE IF EXISTS `orders`;
CREATE TABLE `orders` (
  `id` int NOT NULL,
  `customer_id` int DEFAULT NULL
) ENGINE=InnoDB;

LOCK TABLES `orders` WRITE;
INSERT INTO `orders` (`id`, `customer_id`) VALUES (10,1),(11,2);
UNLOCK TABLES;
/*!50003 SET @saved_cs_client      = @@character_set_client */ ;
DELIMITER ;;
/*!50003 CREATE*/ /*!50017 DEFINER=`root`@`localhost`*/ /*!50003 TRIGGER `orders_bi` BEFORE INSERT ON `orders` FOR EACH ROW BEGIN SET NEW.id = NEW.id + 0; END */;;
DELIMITER ;
/*!50003 SET character_set_client  = @saved_cs_client */ ;

--
-- Final view structure for view `big_orders`
--

/*!50001 DROP VIEW IF EXISTS `big_orders`*/;
/*!50001 SET @saved_col_connection     = @@collation_connection */;
/*!50001 CREATE VIEW `big_orders` AS select `orders`.`id` AS `id` from `orders` */;
/*!50001 SET collation_connection      = @saved_col_connection */;

/*!40014 SET FOREIGN_KEY_CHECKS=@OLD_FOREIGN_KEY_CHECKS */;
/*!40101 SET CHARACTER_SET_CLIENT=@OLD_CHARACTER_SET_CLIENT */;

-- Dump completed on 2026-11-13 10:00:00
";

    #[test]
    fn a_mysqldump() {
        let (_d, b, d) = read(MYSQL);
        assert_eq!(d.dialect, Dialect::MySql);
        let names: Vec<&str> = d.tables.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["customers", "orders"]);
        let (c, o) = (&d.tables[0], &d.tables[1]);
        assert_eq!((c.rows, c.created, o.rows), (4, true, 2));
        assert!(MYSQL[c.first as usize..].starts_with("CREATE TABLE `customers`"));
        let header = text(MYSQL, &d.header);
        assert!(header.contains("SET NAMES utf8mb4") && !header.contains("Table structure"), "{header:?}");
        assert!(d.prelude.is_empty());
        let footer = &MYSQL[d.footer.start as usize..d.footer.end as usize];
        assert!(footer.contains("SET FOREIGN_KEY_CHECKS=@OLD") && footer.contains("Dump completed") && !footer.contains("VIEW") && !footer.contains("@saved"), "{footer:?}");
        let other: String = d.other.iter().map(|p| &MYSQL[p.range.start as usize..p.range.end as usize]).collect();
        assert!(other.contains("CREATE VIEW `big_orders`") && !other.contains("INSERT"), "{other:?}");
        let x = extract(&b, &d, &["orders"], true);
        assert!(x.starts_with("-- MySQL dump") && x.contains("CREATE TABLE `orders`") && x.contains("VALUES (10,1),(11,2);") && x.contains("TRIGGER `orders_bi`"));
        assert!(!x.contains("customers") && !x.contains("big_orders"), "{x}");
        // The trigger's DELIMITER ;; is ended before the closing settings.
        let after = &x[x.find("orders_bi").unwrap()..];
        assert!(after.contains("DELIMITER ;\n") && after.find("DELIMITER ;\n") < after.find("SET FOREIGN_KEY_CHECKS"), "{after}");
        assert!(x.trim_end().ends_with("Dump completed on 2026-11-13 10:00:00"));
        let y = extract(&b, &d, &["customers"], false);
        assert!(y.contains("Table structure for table `customers`") && y.contains("UNLOCK TABLES;") && !y.contains("SET NAMES"), "{y}");
    }

    const PG: &str = "--
-- PostgreSQL database dump
--

\\restrict abc123

SET statement_timeout = 0;
SET client_encoding = 'UTF8';
SELECT pg_catalog.set_config('search_path', '', false);

CREATE FUNCTION public.touch() RETURNS trigger
    LANGUAGE plpgsql
    AS $$BEGIN NEW.at := now(); RETURN NEW; END;$$;

SET default_table_access_method = heap;

--
-- Name: orders; Type: TABLE; Schema: public; Owner: shop
--

CREATE TABLE public.orders (
    id integer NOT NULL,
    note text
);

CREATE SEQUENCE public.orders_id_seq
    AS integer
    START WITH 1;

ALTER SEQUENCE public.orders_id_seq OWNED BY public.orders.id;

CREATE TABLE public.\"Items\" (
    id integer
);

ALTER TABLE ONLY public.orders ALTER COLUMN id SET DEFAULT nextval('public.orders_id_seq'::regclass);

--
-- Data for Name: orders; Type: TABLE DATA; Schema: public; Owner: shop
--

COPY public.orders (id, note) FROM stdin;
1\tfirst; with a semicolon
2\t'quoted'
3\t\\N
\\.

COPY public.\"Items\" (id) FROM stdin;
7
\\.

SELECT pg_catalog.setval('public.orders_id_seq', 3, true);

ALTER TABLE ONLY public.orders
    ADD CONSTRAINT orders_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.orders
    ADD CONSTRAINT orders_item_fkey FOREIGN KEY (id) REFERENCES public.\"Items\"(id);

CREATE INDEX orders_note ON public.orders USING btree (note);

--
-- PostgreSQL database dump complete
--

\\unrestrict abc123
";

    #[test]
    fn a_pg_dump() {
        let (_d, b, d) = read(PG);
        assert_eq!(d.dialect, Dialect::Postgres);
        assert_eq!(d.tables.iter().map(|t| (t.name.as_str(), t.rows)).collect::<Vec<_>>(), [("public.orders", 3), ("public.Items", 1)]);
        assert_eq!(d.find("orders"), Some(0));
        let (header, prelude) = (text(PG, &d.header), text(PG, &d.prelude));
        assert!(header.contains("\\restrict abc123") && header.contains("default_table_access_method") && !header.contains("FUNCTION"), "{header:?}");
        assert!(prelude.contains("CREATE FUNCTION public.touch()") && !prelude.contains("CREATE TABLE"), "{prelude:?}");
        let x = extract(&b, &d, &["public.orders"], true);
        for part in ["\\restrict abc123", "CREATE TABLE public.orders", "CREATE SEQUENCE public.orders_id_seq", "OWNED BY public.orders.id", "SET DEFAULT nextval", "COPY public.orders", "first; with a semicolon\n2\t'quoted'\n3\t\\N\n\\.", "setval('public.orders_id_seq', 3, true)", "orders_pkey", "CREATE INDEX orders_note", "\\unrestrict abc123"] {
            assert!(x.contains(part), "{part:?} missing in {x}");
        }
        assert!(!x.contains("CREATE TABLE public.\"Items\"") && !x.contains("COPY public.\"Items\""), "{x}");
        // In the dump's order: the sequence after the table, the data before the keys.
        assert!(x.find("CREATE TABLE public.orders") < x.find("CREATE SEQUENCE") && x.find("COPY public.orders") < x.find("orders_pkey"));
        assert!(d.other.is_empty(), "{:?}", d.other);
        assert!(x.contains("orders_item_fkey"));
        let dir = tempfile::tempdir().unwrap();
        let files = split(&b, &d, dir.path(), "pg", true, &CancelToken::new(), None).unwrap().unwrap();
        let names: Vec<String> = files.iter().map(|(p, _)| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["pg.0-before-tables.sql", "pg.1-public.orders.sql", "pg.1-public.Items.sql", "pg.2-after-tables.sql"]);
        let read = |n: &str| std::fs::read_to_string(dir.path().join(n)).unwrap();
        assert!(read("pg.0-before-tables.sql").contains("CREATE FUNCTION public.touch()"));
        let orders = read("pg.1-public.orders.sql");
        assert!(!orders.contains("FUNCTION") && !orders.contains("orders_item_fkey") && orders.contains("orders_pkey"));
        let after = read("pg.2-after-tables.sql");
        assert!(after.contains("orders_item_fkey") && after.starts_with("--\n-- PostgreSQL database dump") && after.trim_end().ends_with("\\unrestrict abc123"), "{after}");
    }

    #[test]
    fn a_sqlite_dump_and_splitting() {
        let text = "PRAGMA foreign_keys=OFF;\nBEGIN TRANSACTION;\nCREATE TABLE t(a,b);\nINSERT INTO t VALUES(1,'x;y');\nINSERT INTO t VALUES(2,'z');\nCREATE TABLE \"u v\"(c);\nINSERT INTO \"u v\" VALUES('(');\nCREATE VIEW w AS SELECT a FROM t;\nCOMMIT;\n";
        let (dir, b, d) = read(text);
        assert_eq!(d.dialect, Dialect::Sqlite);
        assert_eq!(d.tables.iter().map(|t| (t.name.as_str(), t.rows)).collect::<Vec<_>>(), [("t", 2), ("u v", 1)]);
        assert_eq!(extract(&b, &d, &["t"], true), "PRAGMA foreign_keys=OFF;\nBEGIN TRANSACTION;\nCREATE TABLE t(a,b);\nINSERT INTO t VALUES(1,'x;y');\nINSERT INTO t VALUES(2,'z');\nCOMMIT;\n");
        let files = split(&b, &d, dir.path(), "d", true, &CancelToken::new(), None).unwrap().unwrap();
        let names: Vec<String> = files.iter().map(|(p, _)| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["d.1-t.sql", "d.1-u_v.sql", "d.2-after-tables.sql"]);
        assert_eq!(std::fs::read_to_string(dir.path().join("d.2-after-tables.sql")).unwrap(), "PRAGMA foreign_keys=OFF;\nBEGIN TRANSACTION;\nCREATE VIEW w AS SELECT a FROM t;\nCOMMIT;\n");
        let mut csv = Vec::new();
        tables_csv(&d, &mut csv).unwrap();
        assert!(String::from_utf8(csv).unwrap().starts_with("table,rows,bytes,created\nt,2,"));
    }

    #[test]
    fn statement_starts() {
        assert_eq!(classify(b"INSERT IGNORE INTO `a`.`b c` (x) VALUES (1)"), Stmt::Table { name: "a.b c".into(), create: false, insert: true, columns: true, copy: false, foreign: false });
        assert_eq!(classify(b"/*!40000 ALTER TABLE `t` DISABLE KEYS */"), Stmt::Table { name: "t".into(), create: false, insert: false, columns: false, copy: false, foreign: false });
        assert_eq!(classify(b"CREATE UNIQUE INDEX i ON ONLY public.t USING btree (a)"), Stmt::Table { name: "public.t".into(), create: false, insert: false, columns: false, copy: false, foreign: false });
        assert_eq!(classify(b"INSERT OR REPLACE INTO t VALUES(1)"), Stmt::Table { name: "t".into(), create: false, insert: true, columns: false, copy: false, foreign: false });
        assert_eq!(classify(b"ALTER SEQUENCE s OWNED BY public.t.id"), Stmt::Sequence { name: "s".into(), owner: Some("public.t".into()) });
        assert_eq!(classify(b"SELECT pg_catalog.setval('public.s', 3, true)"), Stmt::Sequence { name: "public.s".into(), owner: None });
        assert_eq!(classify(b"/*!40101 SET NAMES utf8mb4 */"), Stmt::Setting);
        assert_eq!(classify(b"CREATE VIEW v AS SELECT 1"), Stmt::View("v".into()));
        assert_eq!(classify(b"/*!50001 CREATE ALGORITHM=UNDEFINED */ /*!50013 DEFINER=`root`@`%` SQL SECURITY DEFINER */ /*!50001 VIEW `v w` AS select 1"), Stmt::View("v w".into()));
    }
}
