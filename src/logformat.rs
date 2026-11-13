//! Web server and system log formats: nginx / Apache access logs, AWS load balancer logs
//! (Application and Classic), IIS logs (W3C extended) and syslog (RFC 3164 and RFC 5424). A
//! log's format is recognised from its first lines; its lines are split into columns (a CSV
//! table), and a web log is summed up in one streaming pass into ready reports: the top URLs,
//! the requests and 5xx errors per minute, and the slowest requests.

use std::borrow::Cow;
use std::cmp::{Ordering, Reverse};
use std::collections::{BTreeMap, BinaryHeap, HashMap};
use std::io::{self, Write};
use std::ops::Range;
use std::time::{Duration, Instant};

use memchr::{memchr, memrchr};

use crate::jobs::{CancelToken, Progress};
use crate::lines::LineReader;
use crate::text::TextBuf;
use crate::timestamp;

/// Lines, and bytes, read to recognise a format.
const SAMPLE_LINES: usize = 2000;
const SAMPLE_BYTES: usize = 4 << 20;
/// Most different URLs counted; the requests to others are counted together.
pub const URLS_MAX: usize = 100_000;
/// Bytes of a URL kept as its key.
const URL_KEY_MAX: usize = 1000;
/// Slowest requests kept.
pub const SLOW_MAX: usize = 1000;
/// Fields after the standard ones of an access log line given columns of their own.
const EXTRA_MAX: usize = 16;
/// Time between partial reports.
const PARTIAL_EVERY: Duration = Duration::from_secs(2);
const MINUTE_MS: i64 = 60_000;
const DAY_MS: i64 = 86_400_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// nginx and Apache: the common and combined log formats, with extra fields after them.
    Access,
    /// AWS Application Load Balancer.
    Alb,
    /// AWS Classic Load Balancer.
    Elb,
    /// IIS and other W3C extended logs (`#Fields:` lines name the columns).
    Iis,
    /// BSD syslog (RFC 3164), also with RFC 3339 times as rsyslog writes them.
    Syslog,
    Syslog5424,
}

impl Kind {
    /// English name (translated in the app).
    pub fn name(self) -> &'static str {
        match self {
            Kind::Access => "nginx / Apache access log",
            Kind::Alb => "AWS Application Load Balancer log",
            Kind::Elb => "AWS Classic Load Balancer log",
            Kind::Iis => "IIS log (W3C)",
            Kind::Syslog => "syslog",
            Kind::Syslog5424 => "syslog (RFC 5424)",
        }
    }

    /// A log of web requests, with URLs and status codes for the reports.
    pub fn is_web(self) -> bool {
        matches!(self, Kind::Access | Kind::Alb | Kind::Elb | Kind::Iis)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tok {
    Bare,
    Quoted,
    Bracket,
}

/// A field of a line: start, end (without quotes or brackets) and kind.
type Token = (usize, usize, Tok);

/// The fields of a line separated by spaces: words, "quoted text" (a backslash escapes the next
/// character) and [bracketed text]. `false` for an unclosed quote or bracket.
fn tokens(line: &[u8], out: &mut Vec<Token>) -> bool {
    out.clear();
    let n = line.len();
    let mut i = 0;
    loop {
        while i < n && line[i] == b' ' {
            i += 1;
        }
        if i >= n {
            return true;
        }
        match line[i] {
            b'"' => {
                let mut j = i + 1;
                while j < n && line[j] != b'"' {
                    j += if line[j] == b'\\' { 2 } else { 1 };
                }
                if j >= n {
                    return false;
                }
                out.push((i + 1, j, Tok::Quoted));
                i = j + 1;
            }
            b'[' => {
                let Some(k) = memchr(b']', &line[i + 1..]) else {
                    return false;
                };
                out.push((i + 1, i + 1 + k, Tok::Bracket));
                i += k + 2;
            }
            _ => {
                let s = i;
                i = memchr(b' ', &line[i..]).map_or(n, |k| i + k);
                out.push((s, i, Tok::Bare));
            }
        }
    }
}

fn at(line: &[u8], t: Token) -> &[u8] {
    &line[t.0..t.1]
}

fn is_status(b: &[u8]) -> bool {
    b == b"-" || (b.len() == 3 && b.iter().all(u8::is_ascii_digit))
}

fn is_size(b: &[u8]) -> bool {
    b == b"-" || (!b.is_empty() && b.iter().all(u8::is_ascii_digit))
}

fn digits(b: &[u8]) -> Option<i64> {
    (!b.is_empty() && b.len() <= 18 && b.iter().all(u8::is_ascii_digit))
        .then(|| b.iter().fold(0i64, |a, &c| a * 10 + (c - b'0') as i64))
}

/// A time as written in a log line, taken apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Civil {
    y: i64,
    mo: i64,
    d: i64,
    h: i64,
    mi: i64,
    s: i64,
    ms: i64,
    /// Minutes east of UTC.
    tz: Option<i64>,
}

impl Civil {
    fn valid(self) -> Option<Civil> {
        ((1..=12).contains(&self.mo)
            && (1..=31).contains(&self.d)
            && self.h < 24
            && self.mi < 60
            && self.s <= 60)
            .then_some(self)
    }

    /// Milliseconds since 0000-03-01, as written (the offset left out).
    fn key(self) -> i64 {
        timestamp::days_from_civil(self.y, self.mo, self.d) * DAY_MS
            + ((self.h * 60 + self.mi) * 60 + self.s) * 1000
            + self.ms
    }

    /// RFC 3339: "2000-10-10T13:55:36-07:00".
    fn iso(self) -> Vec<u8> {
        let mut b = Vec::with_capacity(29);
        push2(&mut b, self.y / 100);
        push2(&mut b, self.y % 100);
        b.push(b'-');
        push2(&mut b, self.mo);
        b.push(b'-');
        push2(&mut b, self.d);
        b.push(b'T');
        push2(&mut b, self.h);
        b.push(b':');
        push2(&mut b, self.mi);
        b.push(b':');
        push2(&mut b, self.s);
        if self.ms > 0 {
            b.push(b'.');
            b.push(b'0' + (self.ms / 100) as u8);
            push2(&mut b, self.ms % 100);
        }
        match self.tz {
            Some(0) => b.push(b'Z'),
            Some(z) => {
                b.push(if z < 0 { b'-' } else { b'+' });
                push2(&mut b, z.abs() / 60);
                b.push(b':');
                push2(&mut b, z.abs() % 60);
            }
            None => {}
        }
        b
    }
}

/// Two digits.
fn push2(b: &mut Vec<u8>, n: i64) {
    b.extend([b'0' + (n / 10 % 10) as u8, b'0' + (n % 10) as u8]);
}

fn month(b: &[u8]) -> Option<i64> {
    const M: [&[u8; 3]; 12] = [
        b"jan", b"feb", b"mar", b"apr", b"may", b"jun", b"jul", b"aug", b"sep", b"oct", b"nov",
        b"dec",
    ];
    let l = [
        b.first()?.to_ascii_lowercase(),
        b.get(1)?.to_ascii_lowercase(),
        b.get(2)?.to_ascii_lowercase(),
    ];
    M.iter().position(|m| **m == l).map(|i| i as i64 + 1)
}

/// "+0300", "-07:00" or "Z" at the start of `b`: minutes east of UTC and the length.
fn zone(b: &[u8]) -> Option<(i64, usize)> {
    match b.first()? {
        b'Z' => Some((0, 1)),
        &c @ (b'+' | b'-') => {
            let sign = if c == b'-' { -1 } else { 1 };
            let h = digits(b.get(1..3)?)?;
            let (m, len) = if b.get(3) == Some(&b':') {
                (digits(b.get(4..6)?)?, 6)
            } else {
                (digits(b.get(3..5)?)?, 5)
            };
            (h <= 14 && m < 60).then_some((sign * (h * 60 + m), len))
        }
        _ => None,
    }
}

/// An access log time: "10/Oct/2000:13:55:36 -0700".
fn clf_time(b: &[u8]) -> Option<Civil> {
    if b.len() < 20
        || b[2] != b'/'
        || b[6] != b'/'
        || b[11] != b':'
        || b[14] != b':'
        || b[17] != b':'
    {
        return None;
    }
    let tz = b
        .get(20..)
        .and_then(|z| zone(z.strip_prefix(b" ").unwrap_or(z)))
        .map(|z| z.0);
    Civil {
        y: digits(&b[7..11])?,
        mo: month(&b[3..6])?,
        d: digits(&b[..2])?,
        h: digits(&b[12..14])?,
        mi: digits(&b[15..17])?,
        s: digits(&b[18..20])?,
        ms: 0,
        tz,
    }
    .valid()
}

/// An RFC 3339 time at the start of `b` ("2018-07-02T22:23:00.186641Z", also with a space for
/// the T when `space` is allowed) and its length.
fn iso_time(b: &[u8], space: bool) -> Option<(Civil, usize)> {
    if b.len() < 19
        || b[4] != b'-'
        || b[7] != b'-'
        || !(b[10] == b'T' || (space && b[10] == b' '))
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let mut c = Civil {
        y: digits(&b[..4])?,
        mo: digits(&b[5..7])?,
        d: digits(&b[8..10])?,
        h: digits(&b[11..13])?,
        mi: digits(&b[14..16])?,
        s: digits(&b[17..19])?,
        ms: 0,
        tz: None,
    };
    let mut end = 19;
    if b.get(19) == Some(&b'.') {
        let n = b[20..].iter().take_while(|c| c.is_ascii_digit()).count();
        if n == 0 {
            return None;
        }
        let first3 = &b[20..20 + n.min(3)];
        c.ms = digits(first3)? * 10i64.pow(3 - first3.len() as u32);
        end = 20 + n;
    }
    if let Some((z, len)) = zone(&b[end..]) {
        c.tz = Some(z);
        end += len;
    }
    Some((c.valid()?, end))
}

/// A BSD syslog time at the start of `b`: "Oct 11 22:14:15" (the day may be space-padded).
fn bsd_time(b: &[u8]) -> Option<Civil> {
    if b.len() < 15 || b[3] != b' ' || b[6] != b' ' || b[9] != b':' || b[12] != b':' {
        return None;
    }
    let day = if b[4] == b' ' { &b[5..6] } else { &b[4..6] };
    Civil {
        y: 2000,
        mo: month(&b[..3])?,
        d: digits(day)?,
        h: digits(&b[7..9])?,
        mi: digits(&b[10..12])?,
        s: digits(&b[13..15])?,
        ms: 0,
        tz: None,
    }
    .valid()
}

#[derive(Clone, Copy, Debug)]
enum Cell {
    Empty,
    Text(usize, usize),
    Static(&'static str),
    /// An access log time, written as RFC 3339 in a table.
    Time(usize, usize),
}

impl Cell {
    fn raw(self, line: &[u8]) -> &[u8] {
        match self {
            Cell::Empty => b"",
            Cell::Text(a, b) | Cell::Time(a, b) => &line[a..b],
            Cell::Static(s) => s.as_bytes(),
        }
    }

    /// The value as a table shows it.
    fn shown(self, line: &[u8]) -> Cow<'_, [u8]> {
        match self {
            Cell::Time(a, b) => match clf_time(&line[a..b]) {
                Some(c) => Cow::Owned(c.iso()),
                None => Cow::Borrowed(&line[a..b]),
            },
            other => Cow::Borrowed(other.raw(line)),
        }
    }
}

fn text(t: &Token) -> Cell {
    Cell::Text(t.0, t.1)
}

/// How a request's duration is written.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Dur {
    #[default]
    None,
    /// Seconds in a column (nginx's $request_time).
    Secs(usize),
    /// Milliseconds in a column (IIS's time-taken).
    Millis(usize),
    /// Seconds in three columns from this one, summed (AWS: request, target and response
    /// processing times; -1 when the request did not get that far).
    Sum3(usize),
}

/// The columns the reports read.
#[derive(Clone, Debug, Default)]
struct Cols {
    time: Option<usize>,
    /// IIS: the date is a column of its own.
    date: Option<usize>,
    method: Option<usize>,
    url: Option<usize>,
    status: Option<usize>,
    bytes: Option<usize>,
    client: Option<usize>,
    dur: Dur,
}

/// A log's format and its columns.
#[derive(Clone, Debug)]
pub struct Format {
    pub kind: Kind,
    pub columns: Vec<String>,
    /// Access logs: a virtual host before the client; the referer and user agent after the size.
    vhost: bool,
    combined: bool,
    /// Syslog: lines start with a <priority>.
    pri: bool,
    cols: Cols,
}

const ALB_TYPES: [&[u8]; 6] = [b"http", b"https", b"h2", b"grpcs", b"ws", b"wss"];
const ALB_COLUMNS: [&str; 34] = [
    "type",
    "time",
    "elb",
    "client_ip",
    "client_port",
    "target_ip",
    "target_port",
    "request_processing_time",
    "target_processing_time",
    "response_processing_time",
    "elb_status_code",
    "target_status_code",
    "received_bytes",
    "sent_bytes",
    "request_verb",
    "request_url",
    "request_proto",
    "user_agent",
    "ssl_cipher",
    "ssl_protocol",
    "target_group_arn",
    "trace_id",
    "domain_name",
    "chosen_cert_arn",
    "matched_rule_priority",
    "request_creation_time",
    "actions_executed",
    "redirect_url",
    "error_reason",
    "target_port_list",
    "target_status_code_list",
    "classification",
    "classification_reason",
    "conn_trace_id",
];
const ELB_COLUMNS: [&str; 19] = [
    "time",
    "elb",
    "client_ip",
    "client_port",
    "backend_ip",
    "backend_port",
    "request_processing_time",
    "backend_processing_time",
    "response_processing_time",
    "elb_status_code",
    "backend_status_code",
    "received_bytes",
    "sent_bytes",
    "request_verb",
    "request_url",
    "request_proto",
    "user_agent",
    "ssl_cipher",
    "ssl_protocol",
];
/// Fields of an ALB line, and of a Classic ELB line, before its request.
const ALB_REQUEST: usize = 12;
const ELB_REQUEST: usize = 11;
const FACILITY: [&str; 24] = [
    "kern",
    "user",
    "mail",
    "daemon",
    "auth",
    "syslog",
    "lpr",
    "news",
    "uucp",
    "cron",
    "authpriv",
    "ftp",
    "ntp",
    "security",
    "console",
    "solaris-cron",
    "local0",
    "local1",
    "local2",
    "local3",
    "local4",
    "local5",
    "local6",
    "local7",
];
const SEVERITY: [&str; 8] = [
    "emerg", "alert", "crit", "err", "warning", "notice", "info", "debug",
];

/// An access log line's shape: a virtual host first, the combined format, and how many fields
/// follow the standard ones.
type Shape = (bool, bool, usize);

fn access_shape(t: &[Token], line: &[u8]) -> Option<Shape> {
    let o = if t.len() > 3 && t[3].2 == Tok::Bracket {
        0
    } else if t.len() > 4 && t[4].2 == Tok::Bracket {
        1
    } else {
        return None;
    };
    if t.len() < o + 7
        || t[..o + 3].iter().any(|x| x.2 != Tok::Bare)
        || t[o + 4].2 != Tok::Quoted
        || !is_status(at(line, t[o + 5]))
        || !is_size(at(line, t[o + 6]))
    {
        return None;
    }
    let time = at(line, t[o + 3]);
    if clf_time(time).is_none() && iso_time(time, false).is_none() {
        return None;
    }
    let combined = t.len() >= o + 9 && t[o + 7].2 == Tok::Quoted && t[o + 8].2 == Tok::Quoted;
    Some((o == 1, combined, t.len() - o - if combined { 9 } else { 7 }))
}

fn alb_line(t: &[Token], line: &[u8]) -> bool {
    t.len() > ALB_REQUEST
        && t[0].2 == Tok::Bare
        && ALB_TYPES.contains(&at(line, t[0]))
        && iso_time(at(line, t[1]), false).is_some_and(|(_, n)| n == t[1].1 - t[1].0)
        && is_status(at(line, t[8]))
        && t[ALB_REQUEST].2 == Tok::Quoted
}

fn elb_line(t: &[Token], line: &[u8]) -> bool {
    t.len() > ELB_REQUEST
        && iso_time(at(line, t[0]), false).is_some_and(|(_, n)| n == t[0].1 - t[0].0)
        && memchr(b':', at(line, t[2])).is_some()
        && is_status(at(line, t[7]))
        && t[ELB_REQUEST].2 == Tok::Quoted
}

/// "<34>" at the start of a line: the priority and the length (`None` when malformed;
/// `Some((None, 0))` when there is none).
fn pri_at(line: &[u8]) -> Option<(Option<u8>, usize)> {
    if line.first() != Some(&b'<') {
        return Some((None, 0));
    }
    let n = line[1..]
        .iter()
        .take(4)
        .take_while(|c| c.is_ascii_digit())
        .count();
    if !(1..=3).contains(&n) || line.get(1 + n) != Some(&b'>') {
        return None;
    }
    let v = digits(&line[1..1 + n])?;
    (v <= 191).then_some((Some(v as u8), n + 2))
}

struct Sys {
    pri: Option<u8>,
    time: Range<usize>,
    host: Range<usize>,
    app: Option<Range<usize>>,
    pid: Option<Range<usize>>,
    msg: Range<usize>,
}

/// An RFC 3164 line: [<pri>]time host [tag[pid]:] message.
fn parse_3164(line: &[u8]) -> Option<Sys> {
    let (pri, i) = pri_at(line)?;
    let rest = &line[i..];
    let te = i + if bsd_time(rest).is_some() {
        15
    } else {
        iso_time(rest, false)?.1
    };
    if line.get(te) != Some(&b' ') {
        return None;
    }
    let hs = te + 1;
    let he = hs + memchr(b' ', &line[hs..])?;
    if he == hs {
        return None;
    }
    let ms = he + 1;
    let tl = line[ms..]
        .iter()
        .take_while(|&&c| !matches!(c, b' ' | b':' | b'['))
        .count();
    let (mut app, mut pid, mut msg) = (None, None, ms..line.len());
    if (1..=48).contains(&tl) {
        let mut j = ms + tl;
        let mut p = None;
        if line.get(j) == Some(&b'[') {
            if let Some(k) = memchr(b']', &line[j..]) {
                p = Some(j + 1..j + k);
                j += k + 1;
            }
        }
        if line.get(j) == Some(&b':') {
            j += 1 + (line.get(j + 1) == Some(&b' ')) as usize;
            (app, pid, msg) = (Some(ms..ms + tl), p, j..line.len());
        }
    }
    Some(Sys {
        pri,
        time: i..te,
        host: hs..he,
        app,
        pid,
        msg,
    })
}

/// A syslog host that is really something else: a log level or a word ending in a colon.
fn odd_host(h: &[u8]) -> bool {
    const LEVELS: [&[u8]; 10] = [
        b"info",
        b"warn",
        b"warning",
        b"error",
        b"debug",
        b"trace",
        b"fatal",
        b"critical",
        b"notice",
        b"err",
    ];
    h.starts_with(b"[") || h.ends_with(b":") || LEVELS.iter().any(|l| h.eq_ignore_ascii_case(l))
}

struct Sys5424 {
    pri: u8,
    fields: [Range<usize>; 5],
    sd: Range<usize>,
    msg: Range<usize>,
}

/// An RFC 5424 line: <pri>1 time host app procid msgid structured-data [message].
fn parse_5424(line: &[u8]) -> Option<Sys5424> {
    let (Some(pri), i) = pri_at(line)? else {
        return None;
    };
    if !line[i..].starts_with(b"1 ") {
        return None;
    }
    let mut j = i + 2;
    let mut fields: [Range<usize>; 5] = Default::default();
    for f in &mut fields {
        let e = j + memchr(b' ', &line[j..])?;
        if e == j {
            return None;
        }
        *f = j..e;
        j = e + 1;
    }
    let time = &line[fields[0].clone()];
    if time != b"-" && iso_time(time, false).is_none_or(|(_, n)| n != time.len()) {
        return None;
    }
    let sd_start = j;
    match line.get(j) {
        Some(b'-') => j += 1,
        Some(b'[') => {
            while line.get(j) == Some(&b'[') {
                let mut quoted = false;
                j += 1;
                loop {
                    match line.get(j)? {
                        b'\\' => j += 1,
                        b'"' => quoted = !quoted,
                        b']' if !quoted => break,
                        _ => {}
                    }
                    j += 1;
                }
                j += 1;
            }
        }
        _ => return None,
    }
    let sd = sd_start..j;
    let msg = match line.get(j) {
        None => j..j,
        Some(b' ') => {
            let s = j + 1;
            s + if line[s..].starts_with(b"\xEF\xBB\xBF") {
                3
            } else {
                0
            }..line.len()
        }
        _ => return None,
    };
    Some(Sys5424 {
        pri,
        fields,
        sd,
        msg,
    })
}

/// A request ("GET /a HTTP/1.1") as method, URL and protocol.
fn split_request(line: &[u8], a: usize, b: usize) -> [Cell; 3] {
    let r = &line[a..b];
    match memchr(b' ', r) {
        Some(i)
            if i > 0
                && r[..i]
                    .iter()
                    .all(|c| c.is_ascii_uppercase() || matches!(c, b'-' | b'_')) =>
        {
            let ra = a + i + 1;
            match memrchr(b' ', &line[ra..b]) {
                Some(j) if line[ra + j + 1..b].starts_with(b"HTTP/") => [
                    Cell::Text(a, a + i),
                    Cell::Text(ra, ra + j),
                    Cell::Text(ra + j + 1, b),
                ],
                _ => [Cell::Text(a, a + i), Cell::Text(ra, b), Cell::Empty],
            }
        }
        _ => [Cell::Empty, Cell::Text(a, b), Cell::Empty],
    }
}

/// "10.0.0.1:443" as address and port.
fn split_ip_port(line: &[u8], t: &Token) -> [Cell; 2] {
    match memrchr(b':', at(line, *t)) {
        Some(k) if t.1 - t.0 > 1 => [Cell::Text(t.0, t.0 + k), Cell::Text(t.0 + k + 1, t.1)],
        _ => [text(t), Cell::Empty],
    }
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn position(columns: &[String], name: &str) -> Option<usize> {
    columns.iter().position(|c| c == name)
}

/// A decimal number of seconds ("0.123"), or "-".
fn is_seconds(b: &[u8]) -> bool {
    b == b"-"
        || (b.contains(&b'.')
            && b.iter().all(|c| c.is_ascii_digit() || *c == b'.')
            && b.iter().filter(|c| **c == b'.').count() == 1)
}

impl Format {
    fn new(kind: Kind, columns: Vec<String>) -> Format {
        Format {
            kind,
            columns,
            vhost: false,
            combined: false,
            pri: false,
            cols: Cols::default(),
        }
    }

    /// The format of a log's first lines, when most of them have one.
    pub fn guess<L: AsRef<[u8]>>(lines: &[L]) -> Option<Format> {
        let lines: Vec<&[u8]> = lines.iter().map(|l| l.as_ref()).collect();
        let data: Vec<&[u8]> = lines
            .iter()
            .copied()
            .filter(|l| !l.trim_ascii().is_empty() && !l.starts_with(b"#"))
            .collect();
        if data.is_empty() {
            return None;
        }
        if let Some(f) = Format::iis(&lines, data.len()) {
            return Some(f);
        }
        let mut t = Vec::new();
        let (mut access, mut alb, mut elb, mut sys, mut sys5424) = (Vec::new(), 0, 0, 0, 0);
        for &l in &data {
            if tokens(l, &mut t) {
                if let Some(s) = access_shape(&t, l) {
                    access.push((s, l));
                }
                alb += alb_line(&t, l) as usize;
                elb += elb_line(&t, l) as usize;
            }
            sys += parse_3164(l).is_some_and(|s| {
                s.app.is_some_and(|a| l[a.start].is_ascii_alphabetic()) && !odd_host(&l[s.host])
            }) as usize;
            sys5424 += parse_5424(l).is_some() as usize;
        }
        // The first of equal counts: the stricter formats come first.
        let best = [
            (Kind::Access, access.len()),
            (Kind::Alb, alb),
            (Kind::Elb, elb),
            (Kind::Syslog5424, sys5424),
            (Kind::Syslog, sys),
        ]
        .into_iter()
        .fold((Kind::Access, 0), |b, x| if x.1 > b.1 { x } else { b });
        if best.1 == 0 || best.1 * 2 < data.len() {
            return None;
        }
        Some(match best.0 {
            Kind::Access => Format::access(&access),
            Kind::Alb | Kind::Elb => {
                let alb = best.0 == Kind::Alb;
                let base: &[&str] = if alb { &ALB_COLUMNS } else { &ELB_COLUMNS };
                // Fields AWS adds after the known ones get columns too (two ip:port fields and
                // the request take more than one column each).
                let known = base.len() - 4;
                let mut most = 0;
                for l in &data {
                    if tokens(l, &mut t)
                        && (if alb {
                            alb_line(&t, l)
                        } else {
                            elb_line(&t, l)
                        })
                    {
                        most = most.max(t.len());
                    }
                }
                let mut columns = names(base);
                for i in 0..most.saturating_sub(known).min(EXTRA_MAX) {
                    columns.push(format!("extra{}", i + 1));
                }
                let mut f = Format::new(best.0, columns);
                let p = |n: &str| position(&f.columns, n);
                f.cols = Cols {
                    time: p("time"),
                    date: None,
                    method: p("request_verb"),
                    url: p("request_url"),
                    status: p("elb_status_code"),
                    bytes: p("sent_bytes"),
                    client: p("client_ip"),
                    dur: p("request_processing_time").map_or(Dur::None, Dur::Sum3),
                };
                f
            }
            Kind::Syslog => {
                let pri = data.iter().filter(|l| l.starts_with(b"<")).count() * 2 > data.len();
                let mut columns = if pri {
                    names(&["facility", "severity"])
                } else {
                    Vec::new()
                };
                columns.extend(names(&["time", "host", "app", "pid", "message"]));
                let mut f = Format::new(Kind::Syslog, columns);
                f.pri = pri;
                f
            }
            Kind::Syslog5424 => Format::new(
                Kind::Syslog5424,
                names(&[
                    "facility",
                    "severity",
                    "time",
                    "host",
                    "app",
                    "pid",
                    "msgid",
                    "structured_data",
                    "message",
                ]),
            ),
            Kind::Iis => unreachable!(),
        })
    }

    /// A W3C extended log: `#Fields:` lines name the columns; most data lines have as many
    /// fields as the names in effect.
    fn iis(lines: &[&[u8]], data: usize) -> Option<Format> {
        let mut columns: Vec<String> = Vec::new();
        let (mut have, mut fit) = (0usize, 0usize);
        for l in lines {
            if let Some(rest) = l.strip_prefix(b"#Fields:") {
                let list: Vec<String> = String::from_utf8_lossy(rest)
                    .split_ascii_whitespace()
                    .map(String::from)
                    .collect();
                have = list.len();
                for n in list {
                    if !columns.contains(&n) {
                        columns.push(n);
                    }
                }
            } else if !l.starts_with(b"#")
                && !l.trim_ascii().is_empty()
                && have > 0
                && l.split(|&c| c == b' ').count() == have
            {
                fit += 1;
            }
        }
        if columns.is_empty() || fit == 0 || fit * 2 < data {
            return None;
        }
        let mut f = Format::new(Kind::Iis, columns);
        let p = |n: &str| position(&f.columns, n);
        f.cols = Cols {
            time: p("time"),
            date: p("date"),
            method: p("cs-method"),
            url: p("cs-uri-stem"),
            status: p("sc-status"),
            bytes: p("sc-bytes"),
            client: p("c-ip"),
            dur: p("time-taken").map_or(Dur::None, Dur::Millis),
        };
        Some(f)
    }

    fn access(lines: &[(Shape, &[u8])]) -> Format {
        let vhost = lines.iter().filter(|x| x.0 .0).count() * 2 > lines.len();
        let combined = lines.iter().filter(|x| x.0 .1).count() * 2 > lines.len();
        let same: Vec<&(Shape, &[u8])> = lines
            .iter()
            .filter(|x| x.0 .0 == vhost && x.0 .1 == combined)
            .collect();
        let extra = same
            .iter()
            .map(|x| x.0 .2)
            .max()
            .unwrap_or(0)
            .min(EXTRA_MAX);
        // The first extra field holding seconds in every line that has it is the request time.
        let first_extra = (vhost as usize) + if combined { 9 } else { 7 };
        let mut t = Vec::new();
        let duration = (0..extra).find(|&i| {
            let mut seen = false;
            let all = same.iter().all(|(_, l)| {
                tokens(l, &mut t);
                match t.get(first_extra + i) {
                    Some(&x) => {
                        let v = at(l, x);
                        seen |= v != b"-";
                        is_seconds(v)
                    }
                    None => true,
                }
            });
            all && seen
        });
        let mut columns = if vhost { names(&["vhost"]) } else { Vec::new() };
        columns.extend(names(&[
            "client", "ident", "user", "time", "method", "url", "protocol", "status", "bytes",
        ]));
        if combined {
            columns.extend(names(&["referer", "user_agent"]));
        }
        let mut n = 0;
        for i in 0..extra {
            if Some(i) == duration {
                columns.push("request_time".into());
            } else {
                n += 1;
                columns.push(format!("extra{n}"));
            }
        }
        let mut f = Format::new(Kind::Access, columns);
        f.vhost = vhost;
        f.combined = combined;
        let p = |n: &str| position(&f.columns, n);
        f.cols = Cols {
            time: p("time"),
            date: None,
            method: p("method"),
            url: p("url"),
            status: p("status"),
            bytes: p("bytes"),
            client: p("client"),
            dur: p("request_time").map_or(Dur::None, Dur::Secs),
        };
        f
    }

    /// The reports need a URL and a status column.
    pub fn has_requests(&self) -> bool {
        self.kind.is_web() && self.cols.url.is_some() && self.cols.status.is_some()
    }

    /// The format has request durations (for the slowest requests).
    pub fn has_durations(&self) -> bool {
        self.cols.dur != Dur::None
    }

    /// A regular expression for the lines of the requests to `url` (a report's URL key).
    pub fn url_regex(&self, url: &str) -> String {
        let u = regex_syntax::escape(url);
        match self.kind {
            Kind::Iis => format!(r"(?:^| ){u}(?: |$)"),
            _ => format!(r#""[A-Z_-]+ (?:[a-zA-Z]+://[^/ "]*)?{u}(?:\?[^ "]*)?[ "]"#),
        }
    }
}

/// What a line is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Split {
    Row,
    /// An IIS `#` line.
    Directive,
    Blank,
    /// A line in another format.
    Other,
}

/// Splits lines into a format's columns.
pub struct Splitter<'f> {
    f: &'f Format,
    toks: Vec<Token>,
    cells: Vec<Cell>,
    /// IIS: the column of each field of the `#Fields:` line in effect (`usize::MAX`: none).
    map: Vec<usize>,
}

impl<'f> Splitter<'f> {
    pub fn new(f: &'f Format) -> Splitter<'f> {
        Splitter {
            f,
            toks: Vec::new(),
            cells: Vec::with_capacity(f.columns.len()),
            map: Vec::new(),
        }
    }

    pub fn split(&mut self, line: &[u8]) -> Split {
        self.cells.clear();
        if line.trim_ascii().is_empty() {
            return Split::Blank;
        }
        let ok = match self.f.kind {
            Kind::Iis => {
                if line.starts_with(b"#") {
                    if let Some(rest) = line.strip_prefix(b"#Fields:") {
                        let f = self.f;
                        self.map = rest
                            .split(|c| c.is_ascii_whitespace())
                            .filter(|w| !w.is_empty())
                            .map(|w| {
                                position(&f.columns, &String::from_utf8_lossy(w))
                                    .unwrap_or(usize::MAX)
                            })
                            .collect();
                    }
                    return Split::Directive;
                }
                self.iis(line)
            }
            Kind::Access => tokens(line, &mut self.toks) && self.access(line),
            Kind::Alb => {
                tokens(line, &mut self.toks)
                    && alb_line(&self.toks, line)
                    && self.aws(line, [3, 4], ALB_REQUEST)
            }
            Kind::Elb => {
                tokens(line, &mut self.toks)
                    && elb_line(&self.toks, line)
                    && self.aws(line, [2, 3], ELB_REQUEST)
            }
            Kind::Syslog => self.syslog(line),
            Kind::Syslog5424 => self.syslog5424(line),
        };
        if ok {
            self.cells.resize(self.f.columns.len(), Cell::Empty);
            Split::Row
        } else {
            self.cells.clear();
            Split::Other
        }
    }

    /// The last row's values as a table shows them.
    pub fn values<'l>(&self, line: &'l [u8]) -> Vec<Cow<'l, [u8]>> {
        self.cells.iter().map(|c| c.shown(line)).collect()
    }

    fn iis(&mut self, line: &[u8]) -> bool {
        self.cells.resize(self.f.columns.len(), Cell::Empty);
        let (mut s, mut k) = (0, 0);
        loop {
            let e = memchr(b' ', &line[s..]).map_or(line.len(), |p| s + p);
            if let Some(&col) = self.map.get(k) {
                if col != usize::MAX {
                    self.cells[col] = Cell::Text(s, e);
                }
            }
            k += 1;
            if e >= line.len() {
                break;
            }
            s = e + 1;
        }
        k == self.map.len()
    }

    fn access(&mut self, line: &[u8]) -> bool {
        let Some((vhost, combined, _)) = access_shape(&self.toks, line) else {
            return false;
        };
        if vhost != self.f.vhost {
            return false;
        }
        let (t, c) = (&self.toks, &mut self.cells);
        let o = vhost as usize;
        c.extend(t[..o + 3].iter().map(text));
        c.push(Cell::Time(t[o + 3].0, t[o + 3].1));
        c.extend(split_request(line, t[o + 4].0, t[o + 4].1));
        c.extend([text(&t[o + 5]), text(&t[o + 6])]);
        let mut rest = o + 7;
        if self.f.combined {
            if combined {
                c.extend([text(&t[o + 7]), text(&t[o + 8])]);
                rest = o + 9;
            } else {
                c.extend([Cell::Empty, Cell::Empty]);
            }
        }
        c.extend(t[rest..].iter().map(text));
        true
    }

    fn aws(&mut self, line: &[u8], ip_port: [usize; 2], request: usize) -> bool {
        for (i, t) in self.toks.iter().enumerate() {
            if ip_port.contains(&i) {
                self.cells.extend(split_ip_port(line, t));
            } else if i == request {
                self.cells.extend(split_request(line, t.0, t.1));
            } else {
                self.cells.push(text(t));
            }
        }
        true
    }

    fn syslog(&mut self, line: &[u8]) -> bool {
        let Some(s) = parse_3164(line) else {
            return false;
        };
        let r = |r: Range<usize>| Cell::Text(r.start, r.end);
        if self.f.pri {
            match s.pri {
                Some(p) => self.cells.extend([
                    Cell::Static(FACILITY[(p >> 3) as usize]),
                    Cell::Static(SEVERITY[(p & 7) as usize]),
                ]),
                None => self.cells.extend([Cell::Empty, Cell::Empty]),
            }
        }
        self.cells.extend([
            r(s.time),
            r(s.host),
            s.app.map_or(Cell::Empty, r),
            s.pid.map_or(Cell::Empty, r),
            r(s.msg),
        ]);
        true
    }

    fn syslog5424(&mut self, line: &[u8]) -> bool {
        let Some(s) = parse_5424(line) else {
            return false;
        };
        let r = |r: Range<usize>| {
            if &line[r.clone()] == b"-" {
                Cell::Empty
            } else {
                Cell::Text(r.start, r.end)
            }
        };
        self.cells.extend([
            Cell::Static(FACILITY[(s.pri >> 3) as usize]),
            Cell::Static(SEVERITY[(s.pri & 7) as usize]),
        ]);
        let [time, host, app, pid, msgid] = s.fields;
        self.cells.extend([
            r(time),
            r(host),
            r(app),
            r(pid),
            r(msgid),
            r(s.sd),
            Cell::Text(s.msg.start, s.msg.end),
        ]);
        true
    }
}

/// The format of a log, from its first lines (`None` for other logs, and for UTF-16 text).
pub fn detect(buf: &TextBuf) -> io::Result<Option<Format>> {
    if buf.codec.unit() != 1 {
        return Ok(None);
    }
    let mut r = LineReader::new(buf);
    let (mut lines, mut bytes) = (Vec::new(), 0);
    while lines.len() < SAMPLE_LINES && bytes < SAMPLE_BYTES {
        let Some((_, l)) = r.next()? else { break };
        bytes += l.len() + 1;
        lines.push(l.to_vec());
    }
    Ok(Format::guess(&lines))
}

fn csv_field<W: Write + ?Sized>(out: &mut W, b: &[u8]) -> io::Result<()> {
    let quote = b.iter().any(|&c| matches!(c, b',' | b'"' | b'\n' | b'\r'))
        || b.first() == Some(&b' ')
        || b.last() == Some(&b' ');
    if !quote {
        return out.write_all(b);
    }
    out.write_all(b"\"")?;
    for (i, part) in b.split(|&c| c == b'"').enumerate() {
        if i > 0 {
            out.write_all(b"\"\"")?;
        }
        out.write_all(part)?;
    }
    out.write_all(b"\"")
}

fn csv_row<W: Write + ?Sized, B: AsRef<[u8]>>(
    out: &mut W,
    fields: impl IntoIterator<Item = B>,
) -> io::Result<()> {
    for (i, f) in fields.into_iter().enumerate() {
        if i > 0 {
            out.write_all(b",")?;
        }
        csv_field(out, f.as_ref())?;
    }
    out.write_all(b"\n")
}

/// The log as a CSV table in the format's columns. Returns (rows, lines in another format), or
/// `None` when cancelled. The bytes are written as they are (the log's encoding is kept).
pub fn to_csv<W: Write>(
    buf: &TextBuf,
    f: &Format,
    out: &mut W,
    cancel: &CancelToken,
    progress: Option<&Progress>,
) -> io::Result<Option<(u64, u64)>> {
    if let Some(p) = progress {
        p.set_total(buf.len());
    }
    csv_row(out, &f.columns)?;
    let mut sp = Splitter::new(f);
    let mut r = LineReader::new(buf);
    let (mut rows, mut other, mut n) = (0u64, 0u64, 0u64);
    let mut row = Vec::with_capacity(4096);
    while let Some((_, line)) = r.next()? {
        match sp.split(line) {
            Split::Row => {
                // A row at a time: many small writes to `out` are slower.
                row.clear();
                csv_row(&mut row, sp.cells.iter().map(|c| c.shown(line)))?;
                out.write_all(&row)?;
                rows += 1;
            }
            Split::Other => other += 1,
            Split::Directive | Split::Blank => {}
        }
        n += 1;
        if n.is_multiple_of(4096) {
            if cancel.is_cancelled() {
                return Ok(None);
            }
            if let Some(p) = progress {
                p.set_done(r.taken());
            }
        }
    }
    Ok(Some((rows, other)))
}

/// The requests to one URL.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UrlStat {
    /// The path, without the query string (and for AWS without the scheme and host).
    pub url: String,
    pub requests: u64,
    pub client_errors: u64,
    pub server_errors: u64,
    pub bytes: u64,
    /// Requests with a duration, their total and the longest, in seconds.
    pub timed: u64,
    pub secs_sum: f64,
    pub secs_max: f64,
    /// Offset of the first request's line.
    pub first: u64,
}

impl UrlStat {
    pub fn mean(&self) -> Option<f64> {
        (self.timed > 0).then(|| self.secs_sum / self.timed as f64)
    }
}

/// The requests of one minute.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Minute {
    /// Minutes since 0000-03-01, as written in the log (its offset from UTC left out).
    pub minute: i64,
    pub requests: u64,
    pub client_errors: u64,
    pub server_errors: u64,
    /// Offset of the minute's first line.
    pub first: u64,
}

impl Minute {
    /// "2024-05-01 14:32".
    pub fn label(&self) -> String {
        timestamp::format_key(self.minute * MINUTE_MS, true, false)
    }
}

/// One of the slowest requests.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Slow {
    pub secs: f64,
    /// Offset and number (1-based) of its line.
    pub offset: u64,
    pub line: u64,
    pub time: String,
    pub status: String,
    pub method: String,
    pub url: String,
    pub client: String,
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub lines: u64,
    pub requests: u64,
    /// Lines in another format, and the first of them.
    pub other: u64,
    pub first_other: Option<u64>,
    /// Requests per status code (0: none written), by code.
    pub statuses: Vec<(u16, u64)>,
    pub client_errors: u64,
    pub server_errors: u64,
    pub bytes: u64,
    /// Requests with a duration.
    pub timed: u64,
    /// Most requested first.
    pub urls: Vec<UrlStat>,
    /// Requests to URLs past the first [`URLS_MAX`].
    pub other_urls: u64,
    /// In time order.
    pub minutes: Vec<Minute>,
    /// Slowest first.
    pub slow: Vec<Slow>,
}

struct SlowEntry {
    micros: u64,
    s: Slow,
}

impl PartialEq for SlowEntry {
    fn eq(&self, o: &Self) -> bool {
        self.cmp(o) == Ordering::Equal
    }
}
impl Eq for SlowEntry {}
impl PartialOrd for SlowEntry {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for SlowEntry {
    /// Slower is greater; of two as slow, the earlier line.
    fn cmp(&self, o: &Self) -> Ordering {
        self.micros
            .cmp(&o.micros)
            .then(o.s.offset.cmp(&self.s.offset))
    }
}

#[derive(Default)]
struct UrlAcc {
    requests: u64,
    client_errors: u64,
    server_errors: u64,
    bytes: u64,
    timed: u64,
    secs_sum: f64,
    secs_max: f64,
    first: u64,
}

struct Acc {
    lines: u64,
    requests: u64,
    other: u64,
    first_other: Option<u64>,
    statuses: Vec<u64>,
    bytes: u64,
    timed: u64,
    urls: HashMap<Box<[u8]>, UrlAcc>,
    other_urls: u64,
    minutes: BTreeMap<i64, Minute>,
    cur: Option<Minute>,
    slow: BinaryHeap<Reverse<SlowEntry>>,
}

/// The URL key: the path without the query string, and without `scheme://host` before it.
fn url_key(u: &[u8]) -> &[u8] {
    let mut u = u;
    if let Some(p) = u.windows(3).take(12).position(|w| w == b"://") {
        let rest = &u[p + 3..];
        u = match memchr(b'/', rest) {
            Some(k) => &rest[k..],
            None => b"/",
        };
    }
    let u = memchr(b'?', u).map_or(u, |k| &u[..k]);
    &u[..u.len().min(URL_KEY_MAX)]
}

impl Acc {
    fn new() -> Acc {
        Acc {
            lines: 0,
            requests: 0,
            other: 0,
            first_other: None,
            statuses: vec![0; 1000],
            bytes: 0,
            timed: 0,
            urls: HashMap::new(),
            other_urls: 0,
            minutes: BTreeMap::new(),
            cur: None,
            slow: BinaryHeap::new(),
        }
    }

    /// The time of a row, in milliseconds since 0000-03-01 as written.
    fn when(f: &Format, line: &[u8], cells: &[Cell]) -> Option<i64> {
        let t = cells.get(f.cols.time?)?.raw(line);
        let c = match f.kind {
            Kind::Iis => {
                let d = cells.get(f.cols.date?)?.raw(line);
                let mut b = [0u8; 40];
                if d.len() + t.len() + 1 > b.len() {
                    return None;
                }
                b[..d.len()].copy_from_slice(d);
                b[d.len()] = b' ';
                b[d.len() + 1..d.len() + 1 + t.len()].copy_from_slice(t);
                iso_time(&b[..d.len() + 1 + t.len()], true)?.0
            }
            Kind::Access => clf_time(t).or_else(|| iso_time(t, false).map(|x| x.0))?,
            _ => iso_time(t, false)?.0,
        };
        Some(c.key())
    }

    fn duration(f: &Format, line: &[u8], cells: &[Cell]) -> Option<f64> {
        let num = |i: usize| -> Option<f64> {
            std::str::from_utf8(cells.get(i)?.raw(line))
                .ok()?
                .parse::<f64>()
                .ok()
        };
        match f.cols.dur {
            Dur::None => None,
            Dur::Secs(i) => num(i).filter(|s| *s >= 0.0),
            Dur::Millis(i) => num(i).filter(|s| *s >= 0.0).map(|ms| ms / 1000.0),
            Dur::Sum3(i) => {
                let (a, b, c) = (num(i)?, num(i + 1)?, num(i + 2)?);
                (a >= 0.0 && b >= 0.0 && c >= 0.0).then_some(a + b + c)
            }
        }
    }

    fn add(&mut self, f: &Format, line: &[u8], cells: &[Cell], off: u64, n: u64) {
        let get = |i: Option<usize>| {
            i.and_then(|i| cells.get(i))
                .map_or(&b""[..], |c| c.raw(line))
        };
        self.requests += 1;
        let status_text = get(f.cols.status);
        let status = digits(status_text).filter(|s| *s < 1000).unwrap_or(0) as u16;
        self.statuses[status as usize] += 1;
        let (ce, se) = (
            (400..500).contains(&status) as u64,
            (500..600).contains(&status) as u64,
        );
        let bytes = digits(get(f.cols.bytes)).unwrap_or(0) as u64;
        self.bytes += bytes;
        let secs = Acc::duration(f, line, cells);
        self.timed += secs.is_some() as u64;
        if let Some(ms) = Acc::when(f, line, cells) {
            let m = ms.div_euclid(MINUTE_MS);
            if self.cur.is_none_or(|c| c.minute != m) {
                if let Some(c) = self.cur.take() {
                    let e = self.minutes.entry(c.minute).or_insert(Minute {
                        minute: c.minute,
                        first: c.first,
                        ..Default::default()
                    });
                    e.requests += c.requests;
                    e.client_errors += c.client_errors;
                    e.server_errors += c.server_errors;
                    e.first = e.first.min(c.first);
                }
                self.cur = Some(self.minutes.remove(&m).unwrap_or(Minute {
                    minute: m,
                    first: off,
                    ..Default::default()
                }));
            }
            let c = self.cur.as_mut().expect("minute");
            c.requests += 1;
            c.client_errors += ce;
            c.server_errors += se;
        }
        let key = url_key(get(f.cols.url));
        if self.urls.len() < URLS_MAX && !self.urls.contains_key(key) {
            self.urls.insert(
                key.into(),
                UrlAcc {
                    first: off,
                    ..Default::default()
                },
            );
        }
        match self.urls.get_mut(key) {
            Some(u) => {
                u.requests += 1;
                u.client_errors += ce;
                u.server_errors += se;
                u.bytes += bytes;
                if let Some(s) = secs {
                    u.timed += 1;
                    u.secs_sum += s;
                    u.secs_max = u.secs_max.max(s);
                }
            }
            None => self.other_urls += 1,
        }
        if let Some(s) = secs {
            let micros = (s * 1e6).round() as u64;
            let beats = self.slow.len() < SLOW_MAX
                || self
                    .slow
                    .peek()
                    .is_some_and(|Reverse(min)| micros > min.micros);
            if beats {
                let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
                let time = Acc::when(f, line, cells)
                    .map(|ms| timestamp::format_key(ms - ms.rem_euclid(1000), true, true))
                    .unwrap_or_default();
                let e = SlowEntry {
                    micros,
                    s: Slow {
                        secs: s,
                        offset: off,
                        line: n,
                        time,
                        status: text(status_text),
                        method: text(get(f.cols.method)),
                        url: text(get(f.cols.url)),
                        client: text(get(f.cols.client)),
                    },
                };
                if self.slow.len() >= SLOW_MAX {
                    self.slow.pop();
                }
                self.slow.push(Reverse(e));
            }
        }
    }

    fn report(&self) -> Report {
        let mut urls: Vec<UrlStat> = self
            .urls
            .iter()
            .map(|(k, u)| UrlStat {
                url: String::from_utf8_lossy(k).into_owned(),
                requests: u.requests,
                client_errors: u.client_errors,
                server_errors: u.server_errors,
                bytes: u.bytes,
                timed: u.timed,
                secs_sum: u.secs_sum,
                secs_max: u.secs_max,
                first: u.first,
            })
            .collect();
        urls.sort_by(|a, b| b.requests.cmp(&a.requests).then(a.first.cmp(&b.first)));
        let mut minutes = self.minutes.clone();
        if let Some(c) = self.cur {
            let e = minutes.entry(c.minute).or_insert(Minute {
                minute: c.minute,
                first: c.first,
                ..Default::default()
            });
            e.requests += c.requests;
            e.client_errors += c.client_errors;
            e.server_errors += c.server_errors;
            e.first = e.first.min(c.first);
        }
        let mut slow: Vec<&SlowEntry> = self.slow.iter().map(|Reverse(e)| e).collect();
        slow.sort_by(|a, b| b.cmp(a));
        let statuses = self
            .statuses
            .iter()
            .enumerate()
            .filter(|&(_, &n)| n > 0)
            .map(|(s, &n)| (s as u16, n))
            .collect();
        let sum = |r: Range<usize>| self.statuses[r].iter().sum();
        Report {
            lines: self.lines,
            requests: self.requests,
            other: self.other,
            first_other: self.first_other,
            statuses,
            client_errors: sum(400..500),
            server_errors: sum(500..600),
            bytes: self.bytes,
            timed: self.timed,
            urls,
            other_urls: self.other_urls,
            minutes: minutes.into_values().collect(),
            slow: slow.into_iter().map(|e| e.s.clone()).collect(),
        }
    }
}

/// The reports of a web log, in one pass; `partial` gets the report so far every few seconds.
/// `None` when cancelled.
pub fn report(
    buf: &TextBuf,
    f: &Format,
    cancel: &CancelToken,
    progress: Option<&Progress>,
    mut partial: impl FnMut(&Report),
) -> io::Result<Option<Report>> {
    if let Some(p) = progress {
        p.set_total(buf.len());
    }
    let mut acc = Acc::new();
    let mut sp = Splitter::new(f);
    let mut r = LineReader::new(buf);
    let mut shown = Instant::now();
    while let Some((off, line)) = r.next()? {
        acc.lines += 1;
        match sp.split(line) {
            Split::Row => {
                let n = acc.lines;
                acc.add(f, line, &sp.cells, off, n);
            }
            Split::Other => {
                acc.other += 1;
                acc.first_other.get_or_insert(off);
            }
            Split::Directive | Split::Blank => {}
        }
        if acc.lines.is_multiple_of(4096) {
            if cancel.is_cancelled() {
                return Ok(None);
            }
            if let Some(p) = progress {
                p.set_done(r.taken());
            }
            if shown.elapsed() >= PARTIAL_EVERY {
                partial(&acc.report());
                shown = Instant::now();
            }
        }
    }
    Ok(Some(acc.report()))
}

fn secs_text(s: f64) -> String {
    format!("{s:.3}")
}

/// The top URLs as CSV (all of them, or the first `top`).
pub fn urls_csv<W: Write + ?Sized>(r: &Report, out: &mut W, top: Option<usize>) -> io::Result<()> {
    let timed = r.timed > 0;
    let mut head = vec!["url", "requests", "percent", "4xx", "5xx", "bytes"];
    if timed {
        head.extend(["mean_seconds", "max_seconds"]);
    }
    csv_row(out, head)?;
    for u in r.urls.iter().take(top.unwrap_or(usize::MAX)) {
        let pct = if r.requests > 0 {
            u.requests as f64 * 100.0 / r.requests as f64
        } else {
            0.0
        };
        let mut row = vec![
            u.url.clone(),
            u.requests.to_string(),
            format!("{pct:.2}"),
            u.client_errors.to_string(),
            u.server_errors.to_string(),
            u.bytes.to_string(),
        ];
        if timed {
            row.push(u.mean().map(secs_text).unwrap_or_default());
            row.push(if u.timed > 0 {
                secs_text(u.secs_max)
            } else {
                String::new()
            });
        }
        csv_row(out, row)?;
    }
    Ok(())
}

/// The requests and errors per minute as CSV, in time order.
pub fn minutes_csv<W: Write + ?Sized>(r: &Report, out: &mut W) -> io::Result<()> {
    csv_row(out, ["minute", "requests", "4xx", "5xx", "5xx_percent"])?;
    for m in &r.minutes {
        let pct = if m.requests > 0 {
            m.server_errors as f64 * 100.0 / m.requests as f64
        } else {
            0.0
        };
        csv_row(
            out,
            [
                m.label(),
                m.requests.to_string(),
                m.client_errors.to_string(),
                m.server_errors.to_string(),
                format!("{pct:.2}"),
            ],
        )?;
    }
    Ok(())
}

/// The slowest requests as CSV, slowest first.
pub fn slow_csv<W: Write + ?Sized>(r: &Report, out: &mut W, top: Option<usize>) -> io::Result<()> {
    csv_row(
        out,
        [
            "seconds", "time", "status", "method", "url", "client", "line",
        ],
    )?;
    for s in r.slow.iter().take(top.unwrap_or(usize::MAX)) {
        csv_row(
            out,
            [
                secs_text(s.secs),
                s.time.clone(),
                s.status.clone(),
                s.method.clone(),
                s.url.clone(),
                s.client.clone(),
                s.line.to_string(),
            ],
        )?;
    }
    Ok(())
}

fn count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn pct(n: u64, of: u64) -> String {
    if of == 0 {
        "0%".into()
    } else {
        format!("{:.1}%", n as f64 * 100.0 / of as f64)
    }
}

/// The reports as text for a terminal: totals, then the first `top` rows of each.
pub fn write_summary<W: Write>(r: &Report, f: &Format, out: &mut W, top: usize) -> io::Result<()> {
    writeln!(
        out,
        "{}: {} requests in {} lines",
        f.kind.name(),
        count(r.requests),
        count(r.lines)
    )?;
    if r.other > 0 {
        writeln!(out, "{} lines in another format", count(r.other))?;
    }
    let mut by_count = r.statuses.clone();
    by_count.sort_by_key(|x| Reverse(x.1));
    let statuses: Vec<String> = by_count
        .iter()
        .take(8)
        .map(|(s, n)| {
            format!(
                "{} {} ({})",
                if *s == 0 {
                    "-".to_string()
                } else {
                    s.to_string()
                },
                count(*n),
                pct(*n, r.requests)
            )
        })
        .collect();
    writeln!(out, "Status: {}", statuses.join(", "))?;
    writeln!(
        out,
        "4xx: {} ({}), 5xx: {} ({}), sent: {}",
        count(r.client_errors),
        pct(r.client_errors, r.requests),
        count(r.server_errors),
        pct(r.server_errors, r.requests),
        crate::jobs::human_bytes(r.bytes)
    )?;
    writeln!(out, "\nTop URLs")?;
    let timed = r.timed > 0;
    writeln!(
        out,
        "{:>12} {:>7} {:>9} {:>9}{}  url",
        "requests",
        "%",
        "4xx",
        "5xx",
        if timed {
            format!(" {:>9} {:>9}", "mean s", "max s")
        } else {
            String::new()
        }
    )?;
    for u in r.urls.iter().take(top) {
        let t = if timed {
            format!(
                " {:>9} {:>9}",
                u.mean().map(secs_text).unwrap_or_default(),
                if u.timed > 0 {
                    secs_text(u.secs_max)
                } else {
                    String::new()
                }
            )
        } else {
            String::new()
        };
        writeln!(
            out,
            "{:>12} {:>7} {:>9} {:>9}{t}  {}",
            count(u.requests),
            pct(u.requests, r.requests),
            count(u.client_errors),
            count(u.server_errors),
            u.url
        )?;
    }
    if r.other_urls > 0 {
        writeln!(out, "(+{} requests to other URLs)", count(r.other_urls))?;
    }
    let mut worst: Vec<&Minute> = r.minutes.iter().filter(|m| m.server_errors > 0).collect();
    worst.sort_by(|a, b| {
        b.server_errors
            .cmp(&a.server_errors)
            .then(a.minute.cmp(&b.minute))
    });
    writeln!(out, "\nMinutes with the most 5xx")?;
    if worst.is_empty() {
        writeln!(out, "  none")?;
    } else {
        writeln!(
            out,
            "{:<17} {:>10} {:>9} {:>7}",
            "minute", "requests", "5xx", "%"
        )?;
        for m in worst.iter().take(top) {
            writeln!(
                out,
                "{:<17} {:>10} {:>9} {:>7}",
                m.label(),
                count(m.requests),
                count(m.server_errors),
                pct(m.server_errors, m.requests)
            )?;
        }
    }
    if f.has_durations() {
        writeln!(out, "\nSlowest requests")?;
        writeln!(
            out,
            "{:>10}  {:<19} {:>6}  {:<7} {:<10} url",
            "seconds", "time", "status", "method", "line"
        )?;
        for s in r.slow.iter().take(top) {
            writeln!(
                out,
                "{:>10}  {:<19} {:>6}  {:<7} {:<10} {}",
                secs_text(s.secs),
                s.time,
                s.status,
                s.method,
                s.line,
                s.url
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(f: &Format, lines: &[&str]) -> Vec<Vec<String>> {
        let mut sp = Splitter::new(f);
        let mut out = Vec::new();
        for l in lines {
            if sp.split(l.as_bytes()) == Split::Row {
                out.push(
                    sp.values(l.as_bytes())
                        .iter()
                        .map(|v| String::from_utf8_lossy(v).into_owned())
                        .collect(),
                );
            }
        }
        out
    }

    fn col<'a>(f: &Format, row: &'a [String], name: &str) -> &'a str {
        &row[position(&f.columns, name)
            .unwrap_or_else(|| panic!("no column {name} in {:?}", f.columns))]
    }

    const NGINX: [&str; 3] = [
        r#"203.0.113.9 - - [10/Oct/2023:13:55:36 +0500] "GET /api/users?id=7 HTTP/1.1" 200 2326 "https://example.com/" "Mozilla/5.0 (X11)" 0.123 0.120"#,
        r#"198.51.100.4 - bob [10/Oct/2023:13:55:37 +0500] "POST /login HTTP/2.0" 502 0 "-" "curl/8.0" 1.500 -"#,
        r#"198.51.100.4 - - [10/Oct/2023:13:56:01 +0500] "\x16\x03\x01" 400 157 "-" "-" 0.000 -"#,
    ];

    #[test]
    fn nginx_combined_with_request_time() {
        let f = Format::guess(&NGINX).expect("format");
        assert_eq!(f.kind, Kind::Access);
        assert_eq!(
            f.columns,
            [
                "client",
                "ident",
                "user",
                "time",
                "method",
                "url",
                "protocol",
                "status",
                "bytes",
                "referer",
                "user_agent",
                "request_time",
                "extra1"
            ]
        );
        let r = rows(&f, &NGINX);
        assert_eq!(r.len(), 3);
        assert_eq!(col(&f, &r[0], "time"), "2023-10-10T13:55:36+05:00");
        assert_eq!(
            (
                col(&f, &r[0], "method"),
                col(&f, &r[0], "url"),
                col(&f, &r[0], "protocol")
            ),
            ("GET", "/api/users?id=7", "HTTP/1.1")
        );
        assert_eq!(col(&f, &r[0], "user_agent"), "Mozilla/5.0 (X11)");
        assert_eq!(col(&f, &r[1], "user"), "bob");
        assert_eq!(col(&f, &r[1], "request_time"), "1.500");
        assert_eq!(
            (col(&f, &r[2], "method"), col(&f, &r[2], "url")),
            ("", r"\x16\x03\x01")
        );
        assert!(f.has_requests() && f.has_durations());
    }

    #[test]
    fn apache_common_and_vhost() {
        let common = [
            r#"127.0.0.1 - frank [10/Oct/2000:13:55:36 -0700] "GET /apache_pb.gif HTTP/1.0" 200 2326"#,
            r#"127.0.0.1 - - [10/Oct/2000:13:55:40 -0700] "GET /x HTTP/1.0" 404 -"#,
        ];
        let f = Format::guess(&common).unwrap();
        assert_eq!(
            f.columns,
            ["client", "ident", "user", "time", "method", "url", "protocol", "status", "bytes"]
        );
        assert!(!f.has_durations());
        let r = rows(&f, &common);
        assert_eq!(col(&f, &r[0], "time"), "2000-10-10T13:55:36-07:00");
        assert_eq!(col(&f, &r[1], "bytes"), "-");
        let vhost = [
            r#"www.example.com:443 127.0.0.1 - - [10/Oct/2000:13:55:36 -0700] "GET / HTTP/1.1" 200 5 "-" "x""#,
        ];
        let f = Format::guess(&vhost).unwrap();
        assert_eq!(&f.columns[..2], ["vhost", "client"]);
        assert_eq!(
            col(&f, &rows(&f, &vhost)[0], "vhost"),
            "www.example.com:443"
        );
    }

    const ALB: &str = r#"https 2018-07-02T22:23:00.186641Z app/my-loadbalancer/50dc6c495c0c9188 192.168.131.39:2817 10.0.0.1:80 0.086 0.048 0.037 200 200 0 57 "GET https://www.example.com:443/path/x?a=1 HTTP/1.1" "curl/7.46.0" ECDHE-RSA-AES128-GCM-SHA256 TLSv1.2 arn:aws:elasticloadbalancing:us-east-2:123456789012:targetgroup/my-targets/73e2d6bc24d8a067 "Root=1-58337281-1d84f3d73c47ec4e58577259" "www.example.com" "arn:aws:acm:us-east-2:123456789012:certificate/12345678-1234-1234-1234-123456789012" 1 2018-07-02T22:22:48.364000Z "authenticate,forward" "-" "-" "10.0.0.1:80" "200" "-" "-" TID_1234"#;

    #[test]
    fn aws_load_balancers() {
        let f = Format::guess(&[ALB]).unwrap();
        assert_eq!(f.kind, Kind::Alb);
        assert_eq!(f.columns.len(), ALB_COLUMNS.len());
        let r = &rows(&f, &[ALB])[0];
        assert_eq!(
            (col(&f, r, "client_ip"), col(&f, r, "client_port")),
            ("192.168.131.39", "2817")
        );
        assert_eq!(
            (
                col(&f, r, "request_verb"),
                col(&f, r, "request_url"),
                col(&f, r, "request_proto")
            ),
            ("GET", "https://www.example.com:443/path/x?a=1", "HTTP/1.1")
        );
        assert_eq!(col(&f, r, "user_agent"), "curl/7.46.0");
        assert_eq!(col(&f, r, "conn_trace_id"), "TID_1234");
        assert_eq!(col(&f, r, "elb_status_code"), "200");
        let elb = r#"2015-05-13T23:39:43.945958Z my-loadbalancer 192.168.131.39:2817 10.0.0.1:80 0.000073 0.001048 0.000057 200 200 0 29 "GET http://www.example.com:80/ HTTP/1.1" "curl/7.38.0" - -"#;
        let f = Format::guess(&[elb]).unwrap();
        assert_eq!(f.kind, Kind::Elb);
        let r = &rows(&f, &[elb])[0];
        assert_eq!(
            (
                col(&f, r, "backend_ip"),
                col(&f, r, "backend_port"),
                col(&f, r, "ssl_protocol")
            ),
            ("10.0.0.1", "80", "-")
        );
        assert_eq!(col(&f, r, "request_url"), "http://www.example.com:80/");
    }

    #[test]
    fn iis_fields_lines_name_the_columns() {
        let log = [
            "#Software: Microsoft Internet Information Services 10.0",
            "#Version: 1.0",
            "#Date: 2024-05-01 00:00:00",
            "#Fields: date time s-ip cs-method cs-uri-stem cs-uri-query s-port c-ip cs(User-Agent) sc-status time-taken",
            "2024-05-01 00:00:01 10.0.0.5 GET /default.aspx - 443 203.0.113.9 Mozilla/5.0+(Windows+NT+10.0) 200 15",
            "2024-05-01 00:00:02 10.0.0.5 POST /api/save id=4 443 203.0.113.9 - 500 2300",
            "#Fields: date time cs-method cs-uri-stem sc-status sc-bytes",
            "2024-05-01 00:01:00 GET /late 200 512",
            "a broken line",
        ];
        let f = Format::guess(&log).unwrap();
        assert_eq!(f.kind, Kind::Iis);
        assert_eq!(
            f.columns,
            [
                "date",
                "time",
                "s-ip",
                "cs-method",
                "cs-uri-stem",
                "cs-uri-query",
                "s-port",
                "c-ip",
                "cs(User-Agent)",
                "sc-status",
                "time-taken",
                "sc-bytes"
            ]
        );
        let r = rows(&f, &log);
        assert_eq!(r.len(), 3);
        assert_eq!(col(&f, &r[1], "time-taken"), "2300");
        assert_eq!(
            (
                col(&f, &r[2], "cs-uri-stem"),
                col(&f, &r[2], "sc-bytes"),
                col(&f, &r[2], "s-ip")
            ),
            ("/late", "512", "")
        );
    }

    #[test]
    fn syslog_both_rfcs() {
        let bsd = [
            "<34>Oct 11 22:14:15 mymachine su[230]: 'su root' failed for lonvick on /dev/pts/8",
            "<13>Oct  1 02:00:00 mymachine CRON: (root) CMD (run-parts)",
            "<13>Oct  1 02:00:01 mymachine last message repeated 2 times",
        ];
        let f = Format::guess(&bsd).unwrap();
        assert_eq!(f.kind, Kind::Syslog);
        assert_eq!(
            f.columns,
            ["facility", "severity", "time", "host", "app", "pid", "message"]
        );
        let r = rows(&f, &bsd);
        assert_eq!(
            r[0],
            [
                "auth",
                "crit",
                "Oct 11 22:14:15",
                "mymachine",
                "su",
                "230",
                "'su root' failed for lonvick on /dev/pts/8"
            ]
        );
        assert_eq!(
            r[1][2..],
            [
                "Oct  1 02:00:00",
                "mymachine",
                "CRON",
                "",
                "(root) CMD (run-parts)"
            ]
        );
        assert_eq!(r[2][4..], ["", "", "last message repeated 2 times"]);
        let iso = [
            "2024-05-01T10:00:00.123456+05:00 web1 nginx[812]: started",
            "2024-05-01T10:00:01+05:00 web1 kernel: eth0 up",
        ];
        let f = Format::guess(&iso).unwrap();
        assert_eq!(f.columns, ["time", "host", "app", "pid", "message"]);
        assert_eq!(
            rows(&f, &iso)[1],
            ["2024-05-01T10:00:01+05:00", "web1", "kernel", "", "eth0 up"]
        );
        // An application log with a level where the host would be is not syslog.
        assert!(Format::guess(&[
            "2024-05-01T10:00:00Z INFO main: started",
            "2024-05-01T10:00:01Z WARN main: slow"
        ])
        .is_none());
        let rfc5424 = [
            r#"<165>1 2003-10-11T22:14:15.003Z mymachine.example.com evntslog - ID47 [exampleSDID@32473 iut="3" eventSource="App]lication"] BOMAn application event"#,
            "<34>1 2003-10-11T22:14:15.003Z host su - ID47 - message",
        ];
        let f = Format::guess(&rfc5424).unwrap();
        assert_eq!(f.kind, Kind::Syslog5424);
        let r = rows(&f, &rfc5424);
        assert_eq!(
            r[0],
            [
                "local4",
                "notice",
                "2003-10-11T22:14:15.003Z",
                "mymachine.example.com",
                "evntslog",
                "",
                "ID47",
                r#"[exampleSDID@32473 iut="3" eventSource="App]lication"]"#,
                "BOMAn application event"
            ]
        );
        assert_eq!(r[1][7..], ["", "message"]);
    }

    #[test]
    fn other_logs_have_no_format() {
        assert!(Format::guess(&["2024-05-01 10:00:00 INFO started", "plain text"]).is_none());
        assert!(Format::guess(&["level=info msg=x", "level=warn msg=y"]).is_none());
        assert!(Format::guess::<&str>(&[]).is_none());
    }

    #[test]
    fn url_keys_and_filters() {
        assert_eq!(url_key(b"/a/b?x=1"), b"/a/b");
        assert_eq!(
            url_key(b"https://www.example.com:443/path/x?a=1"),
            b"/path/x"
        );
        assert_eq!(url_key(b"http://host"), b"/");
        let f = Format::guess(&NGINX).unwrap();
        let re = regex_automata::meta::Regex::new(&f.url_regex("/api/users")).unwrap();
        assert!(re.is_match(NGINX[0]));
        assert!(!re.is_match(r#"1.2.3.4 - - [10/Oct/2023:13:55:36 +0500] "GET /api/users/7 HTTP/1.1" 200 1 "https://x/api/users" "-""#));
        let alb = Format::guess(&[ALB]).unwrap();
        assert!(regex_automata::meta::Regex::new(&alb.url_regex("/path/x"))
            .unwrap()
            .is_match(ALB));
    }

    #[test]
    fn report_of_a_web_log() {
        let mut lines: Vec<String> = Vec::new();
        for i in 0..120 {
            let (url, status, secs) = match i % 4 {
                0 | 1 => ("/home", 200, 0.010),
                2 => (
                    "/api/items?page=2",
                    if i % 8 == 2 { 503 } else { 200 },
                    0.200,
                ),
                _ => ("/missing", 404, 0.001),
            };
            let secs = if i == 79 { 9.5 } else { secs };
            lines.push(format!(r#"10.0.0.{} - - [01/May/2024:10:{:02}:{:02} +0000] "GET {url} HTTP/1.1" {status} 100 "-" "ua" {secs:.3}"#, i % 3, i / 60, i % 60));
        }
        lines.push("garbage".into());
        let f = Format::guess(&lines).unwrap();
        let mut acc = Acc::new();
        let mut sp = Splitter::new(&f);
        let mut off = 0u64;
        for (n, l) in lines.iter().enumerate() {
            acc.lines += 1;
            match sp.split(l.as_bytes()) {
                Split::Row => acc.add(&f, l.as_bytes(), &sp.cells, off, n as u64 + 1),
                Split::Other => {
                    acc.other += 1;
                    acc.first_other.get_or_insert(off);
                }
                _ => {}
            }
            off += l.len() as u64 + 1;
        }
        let r = acc.report();
        assert_eq!((r.requests, r.other, r.lines), (120, 1, 121));
        assert_eq!(
            r.urls
                .iter()
                .map(|u| (u.url.as_str(), u.requests))
                .collect::<Vec<_>>(),
            [("/home", 60), ("/api/items", 30), ("/missing", 30)]
        );
        assert_eq!((r.urls[1].server_errors, r.urls[2].client_errors), (15, 30));
        assert_eq!(r.urls[0].first, 0);
        assert!((r.urls[1].mean().unwrap() - 0.2).abs() < 1e-9);
        assert_eq!(r.minutes.len(), 2);
        assert_eq!(r.minutes[0].label(), "2024-05-01 10:00");
        assert_eq!(
            (
                r.minutes[0].requests,
                r.minutes[0].server_errors,
                r.minutes[1].first
            ),
            (60, 8, lines[..60].iter().map(|l| l.len() as u64 + 1).sum())
        );
        assert_eq!(r.server_errors, 15);
        assert_eq!(r.statuses, [(200, 75), (404, 30), (503, 15)]);
        assert_eq!(r.slow[0].secs, 9.5);
        assert_eq!(
            (
                r.slow[0].line,
                r.slow[0].url.as_str(),
                r.slow[0].time.as_str()
            ),
            (80, "/missing", "2024-05-01 10:01:19")
        );
        assert_eq!(r.slow.len(), 120);
        let mut csv = Vec::new();
        urls_csv(&r, &mut csv, Some(1)).unwrap();
        assert_eq!(String::from_utf8(csv).unwrap(), "url,requests,percent,4xx,5xx,bytes,mean_seconds,max_seconds\n/home,60,50.00,0,0,6000,0.010,0.010\n");
        let mut csv = Vec::new();
        minutes_csv(&r, &mut csv).unwrap();
        assert!(String::from_utf8(csv)
            .unwrap()
            .starts_with("minute,requests,4xx,5xx,5xx_percent\n2024-05-01 10:00,60,15,8,13.33\n"));
        let mut text = Vec::new();
        write_summary(&r, &f, &mut text, 3).unwrap();
        let text = String::from_utf8(text).unwrap();
        assert!(
            text.contains("120 requests in 121 lines")
                && text.contains("/api/items")
                && text.contains("9.500"),
            "{text}"
        );
    }

    #[test]
    fn times() {
        assert_eq!(
            clf_time(b"10/Oct/2000:13:55:36 -0700").unwrap().iso(),
            b"2000-10-10T13:55:36-07:00"
        );
        assert_eq!(
            iso_time(b"2018-07-02T22:23:00.086Z", false)
                .unwrap()
                .0
                .iso(),
            b"2018-07-02T22:23:00.086Z"
        );
        assert_eq!(
            iso_time(b"2018-07-02T22:23:00.186641Z", false).unwrap(),
            (
                Civil {
                    y: 2018,
                    mo: 7,
                    d: 2,
                    h: 22,
                    mi: 23,
                    s: 0,
                    ms: 186,
                    tz: Some(0)
                },
                27
            )
        );
        assert!(
            iso_time(b"2018-07-02 22:23:00", false).is_none()
                && iso_time(b"2018-07-02 22:23:00", true).is_some()
        );
        assert_eq!(
            bsd_time(b"Oct  1 02:00:00").map(|c| (c.mo, c.d)),
            Some((10, 1))
        );
        assert!(clf_time(b"10/Foo/2000:13:55:36 -0700").is_none());
    }
}
