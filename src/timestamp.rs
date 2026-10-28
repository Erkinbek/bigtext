//! Log timestamps: recognising one near the start of a line, and finding the first line at or
//! after a given time with a binary search over byte offsets (log lines are in time order), so
//! a jump in a 50 GB log reads only a few hundred kilobytes.

use crate::filter::{line_end_at, line_start_at};
use crate::jobs::CancelToken;
use crate::text::TextBuf;

/// A point in time from a log line: milliseconds since 0000-03-01 when the line has a date,
/// otherwise milliseconds since midnight.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Stamp {
    pub ms: i64,
    pub has_date: bool,
}

const DAY: i64 = 86_400_000;
/// Only this many characters at the start of a line are searched for a time.
const LOOK: usize = 160;
/// Bytes of a line read to find its timestamp.
const LINE_HEAD: u64 = 512;
/// Lines without a timestamp (stack traces, continuations) skipped before giving up.
const SKIP_LINES: usize = 2000;

impl Stamp {
    pub fn time_of_day(self) -> i64 {
        self.ms.rem_euclid(DAY)
    }
}

/// Turns the timestamps of one log, in order, into sortable keys: a date's milliseconds, or for
/// times without a date the time of day plus the days that have passed (a time much earlier
/// than the one before is the next day).
#[derive(Clone, Debug, Default)]
pub struct Clock {
    day: i64,
    last: Option<i64>,
}

impl Clock {
    /// `dated`: the log's times have dates (the first one had), so they are compared as such.
    pub fn key(&mut self, st: Stamp, dated: bool) -> i64 {
        let key = if dated && st.has_date {
            self.day = st.ms.div_euclid(DAY);
            st.ms
        } else {
            let tod = st.time_of_day();
            if self.last.is_some_and(|l| tod + DAY / 2 < l.rem_euclid(DAY)) {
                self.day += 1;
            }
            self.day * DAY + tod
        };
        self.last = Some(key);
        key
    }
}

/// Days from 0000-03-01 (proleptic Gregorian) for a civil date (H. Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe
}

fn num(b: &[u8]) -> Option<i64> {
    if b.is_empty() || !b.iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some(b.iter().fold(0i64, |a, &c| a * 10 + (c - b'0') as i64))
}

fn month_name(b: &[u8]) -> Option<i64> {
    const M: [&[u8; 3]; 12] = [b"jan", b"feb", b"mar", b"apr", b"may", b"jun", b"jul", b"aug", b"sep", b"oct", b"nov", b"dec"];
    let l = [b.first()?.to_ascii_lowercase(), b.get(1)?.to_ascii_lowercase(), b.get(2)?.to_ascii_lowercase()];
    M.iter().position(|m| **m == l).map(|i| i as i64 + 1)
}

fn valid(y: i64, m: i64, d: i64) -> Option<(i64, i64, i64)> {
    ((1..=12).contains(&m) && (1..=31).contains(&d) && (1..=9999).contains(&y)).then_some((y, m, d))
}

/// A date ending exactly at the end of `b`: `YYYY-MM-DD`, `YYYY/MM/DD`, `YYYY.MM.DD`,
/// `DD.MM.YYYY`, `DD-MM-YYYY`, `DD/MM/YYYY` or `MM/DD/YYYY`, `DD/Mon/YYYY` and syslog's
/// `Mon DD` (no year; 2000 is used, so only order within one file matters).
fn date_before(b: &[u8]) -> Option<(i64, i64, i64)> {
    let n = b.len();
    let sep = |c: u8| matches!(c, b'-' | b'/' | b'.');
    if n >= 10 {
        let t = &b[n - 10..];
        let left_ok = n == 10 || !b[n - 11].is_ascii_digit();
        if left_ok && sep(t[4]) && t[7] == t[4] {
            if let (Some(y), Some(m), Some(d)) = (num(&t[..4]), num(&t[5..7]), num(&t[8..])) {
                return valid(y, m, d);
            }
        }
        if left_ok && sep(t[2]) && t[5] == t[2] {
            if let (Some(a), Some(c), Some(y)) = (num(&t[..2]), num(&t[3..5]), num(&t[6..])) {
                // Slashes are month-first (US) unless that cannot be a month.
                return if t[2] == b'/' && a <= 12 { valid(y, a, c) } else { valid(y, c, a) };
            }
        }
    }
    if n >= 11 {
        let t = &b[n - 11..];
        if t[2] == b'/' && t[6] == b'/' {
            if let (Some(d), Some(m), Some(y)) = (num(&t[..2]), month_name(&t[3..6]), num(&t[7..])) {
                return valid(y, m, d);
            }
        }
    }
    // Syslog: "Mon DD" or "Mon  D".
    let digits = b.iter().rev().take_while(|c| c.is_ascii_digit()).count();
    if (1..=2).contains(&digits) {
        let rest = &b[..n - digits];
        let rest = rest.strip_suffix(b" ").map(|r| r.strip_suffix(b" ").unwrap_or(r))?;
        if rest.len() >= 3 && (rest.len() == 3 || !rest[rest.len() - 4].is_ascii_alphabetic()) {
            let m = month_name(&rest[rest.len() - 3..])?;
            return valid(2000, m, num(&b[n - digits..])?);
        }
    }
    None
}

/// The first timestamp in `text` (looking at its first characters only).
pub fn parse(text: &str) -> Option<Stamp> {
    let mut b = [0u8; LOOK];
    let mut n = 0;
    for c in text.chars().take(LOOK) {
        b[n] = if c.is_ascii() { c as u8 } else { b'?' };
        n += 1;
    }
    parse_ascii(&b[..n])
}

/// Like [`parse`], for the bytes of a line in an ASCII-compatible encoding (UTF-8, the Windows
/// and ISO code pages), without decoding it.
pub fn parse_bytes(line: &[u8]) -> Option<Stamp> {
    let mut b = [0u8; LOOK];
    let n = line.len().min(LOOK);
    for (d, &c) in b.iter_mut().zip(&line[..n]) {
        *d = if c.is_ascii() { c } else { b'?' };
    }
    parse_ascii(&b[..n])
}

/// The first timestamp in `b` (ASCII; other characters replaced by '?').
fn parse_ascii(b: &[u8]) -> Option<Stamp> {
    let mut i = 0;
    while i + 8 <= b.len() {
        let t = &b[i..];
        let two = |k: usize| num(&t[k..k + 2]);
        let start_ok = i == 0 || !b[i - 1].is_ascii_digit();
        if start_ok && t[2] == b':' && t[5] == b':' {
            if let (Some(h), Some(m), Some(s)) = (two(0), two(3), two(6)) {
                if h < 24 && m < 60 && s <= 60 && (t.len() == 8 || !t[8].is_ascii_digit()) {
                    let mut ms = (h * 3600 + m * 60 + s) * 1000;
                    if t.len() > 9 && matches!(t[8], b'.' | b',') && t[9].is_ascii_digit() {
                        let digits: Vec<u8> = t[9..].iter().take_while(|c| c.is_ascii_digit()).copied().collect();
                        let mut f = num(&digits[..digits.len().min(3)]).unwrap_or(0);
                        for _ in digits.len().min(3)..3 {
                            f *= 10;
                        }
                        ms += f;
                    }
                    let before = &b[..i];
                    let trimmed = before.strip_suffix(b"T").or_else(|| before.strip_suffix(b" ")).or_else(|| before.strip_suffix(b":")).unwrap_or(before);
                    let trimmed = trimmed.strip_suffix(b" ").unwrap_or(trimmed);
                    return Some(match date_before(trimmed) {
                        Some((y, mo, d)) => Stamp { ms: days_from_civil(y, mo, d) * DAY + ms, has_date: true },
                        None => Stamp { ms, has_date: false },
                    });
                }
            }
        }
        i += 1;
    }
    None
}

/// Formats a duration in milliseconds: "850 ms", "12.345 s", "3 m 05.120 s", "2 h 03 m 05 s",
/// "4 d 02 h 03 m".
pub fn format_delta(ms: i64) -> String {
    let sign = if ms < 0 { "−" } else { "+" };
    let a = ms.unsigned_abs();
    let (d, h, m, s, f) = (a / 86_400_000, a / 3_600_000 % 24, a / 60_000 % 60, a / 1000 % 60, a % 1000);
    if a < 1000 {
        format!("{sign}{a} ms")
    } else if a < 60_000 {
        format!("{sign}{s}.{f:03} s")
    } else if a < 3_600_000 {
        format!("{sign}{m} m {s:02}.{f:03} s")
    } else if d == 0 {
        format!("{sign}{h} h {m:02} m {s:02} s")
    } else {
        format!("{sign}{d} d {h:02} h {m:02} m")
    }
}

/// The civil date of a day count from [`days_from_civil`] (H. Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Formats a key from [`Clock::key`]: "2024-05-01 14:32:05" for dated logs, otherwise
/// "14:32:05" (with "+1 d" and so on after midnight). Seconds are left out when `seconds` is
/// false.
pub fn format_key(key: i64, dated: bool, seconds: bool) -> String {
    let (day, ms) = (key.div_euclid(DAY), key.rem_euclid(DAY));
    let time = if seconds {
        format!("{:02}:{:02}:{:02}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60)
    } else {
        format!("{:02}:{:02}", ms / 3_600_000, ms / 60_000 % 60)
    };
    if dated {
        let (y, m, d) = civil_from_days(day);
        format!("{y:04}-{m:02}-{d:02} {time}")
    } else if day > 0 {
        format!("{time} +{day} d")
    } else {
        time
    }
}

/// The timestamp of the line starting at `ls`.
pub fn stamp_at(buf: &TextBuf, ls: u64) -> Option<Stamp> {
    let start = ls.max(buf.codec.bom_len as u64).min(buf.len());
    let b = buf.read(start..(start + LINE_HEAD).min(buf.len()))?;
    let cut = buf.codec.find_nl(&b, start).unwrap_or(b.len());
    let cut = cut - cut % buf.codec.unit();
    parse(&buf.codec.decode_lossy(&b[..cut]))
}

/// The first line at or after `from` (a line start) with a timestamp, before `limit`.
fn stamped_line(buf: &TextBuf, from: u64, limit: u64, cancel: &CancelToken) -> Option<(u64, u64, Stamp)> {
    let mut ls = from;
    for _ in 0..SKIP_LINES {
        if ls >= limit || ls >= buf.len() || cancel.is_cancelled() {
            return None;
        }
        let le = line_end_at(buf, ls)?;
        if let Some(st) = stamp_at(buf, ls) {
            return Some((ls, le, st));
        }
        ls = le;
    }
    None
}

#[derive(Debug, PartialEq, Eq)]
pub enum TimeError {
    NoTimestamps,
    Cancelled,
}

/// Start of the first line whose timestamp is at or after `target` (the end of the document
/// when every line is earlier). A target without a date uses the date of the first line (the
/// next day if that time is earlier than the first line's).
pub fn find_time(buf: &TextBuf, target: Stamp, cancel: &CancelToken) -> Result<u64, TimeError> {
    let len = buf.len();
    let Some((_, _, first)) = stamped_line(buf, 0, len, cancel) else {
        return Err(if cancel.is_cancelled() { TimeError::Cancelled } else { TimeError::NoTimestamps });
    };
    let key = match (first.has_date, target.has_date) {
        (true, false) => {
            let k = first.ms - first.time_of_day() + target.ms;
            if k < first.ms - 60_000 {
                k + DAY
            } else {
                k
            }
        }
        (false, true) => target.time_of_day(),
        _ => target.ms,
    };
    let (mut lo, mut hi) = (0u64, len);
    while hi - lo > 64 * 1024 {
        if cancel.is_cancelled() {
            return Err(TimeError::Cancelled);
        }
        let mid = lo + (hi - lo) / 2;
        let mid = mid - mid % buf.codec.unit() as u64;
        let ls = line_end_at(buf, mid.saturating_sub(1)).unwrap_or(hi);
        match stamped_line(buf, ls, hi, cancel) {
            Some((_, le, st)) if st.ms < key => lo = le,
            Some((s, _, _)) => hi = s,
            None => hi = mid,
        }
    }
    let mut ls = line_start_at(buf, lo).unwrap_or(0);
    while ls < len {
        if cancel.is_cancelled() {
            return Err(TimeError::Cancelled);
        }
        match stamped_line(buf, ls, len, cancel) {
            Some((s, _, st)) if st.ms >= key => return Ok(s),
            Some((_, le, _)) => ls = le,
            None => return Ok(len),
        }
    }
    Ok(len)
}

#[cfg(test)]
mod tests {
    #[test]
    fn civil_dates_round_trip_and_keys_format() {
        for (y, m, d) in [(2024, 2, 29), (2024, 3, 1), (1999, 12, 31), (2000, 1, 1), (2100, 6, 15)] {
            assert_eq!(super::civil_from_days(super::days_from_civil(y, m, d)), (y, m, d));
        }
        let st = super::parse("2024-05-01 14:32:05.250 x").unwrap();
        assert_eq!(super::format_key(st.ms, true, true), "2024-05-01 14:32:05");
        assert_eq!(super::format_key(st.ms, true, false), "2024-05-01 14:32");
        assert_eq!(super::format_key(super::DAY + 61_000, false, true), "00:01:01 +1 d");
        assert_eq!(super::parse_bytes("2024-05-01 14:32:05 привет".as_bytes()), Some(st).map(|s| super::Stamp { ms: s.ms - 250, ..s }));
        assert_eq!(super::parse_bytes(b"no time here"), None);
    }

    use super::*;
    use crate::encoding::Codec;
    use crate::file::{OpenMode, Source};
    use crate::index;
    use std::io::Write;
    use std::sync::Arc;

    fn day(y: i64, m: i64, d: i64) -> i64 {
        days_from_civil(y, m, d) * DAY
    }

    #[test]
    fn formats() {
        let t = |h: i64, m: i64, s: i64| (h * 3600 + m * 60 + s) * 1000;
        assert_eq!(parse("2024-05-01 14:32:05,123 INFO x"), Some(Stamp { ms: day(2024, 5, 1) + t(14, 32, 5) + 123, has_date: true }));
        assert_eq!(parse("[2024-05-01T14:32:05.5Z] x").map(|s| s.ms), Some(day(2024, 5, 1) + t(14, 32, 5) + 500));
        assert_eq!(parse("01.05.2024 14:32:05 x").map(|s| s.ms), Some(day(2024, 5, 1) + t(14, 32, 5)));
        assert_eq!(parse("05/01/2024 14:32:05 x").map(|s| s.ms), Some(day(2024, 5, 1) + t(14, 32, 5)));
        assert_eq!(parse("13/01/2024 14:32:05 x").map(|s| s.ms), Some(day(2024, 1, 13) + t(14, 32, 5)));
        assert_eq!(parse("127.0.0.1 - - [01/May/2024:14:32:05 +0000] \"GET\"").map(|s| s.ms), Some(day(2024, 5, 1) + t(14, 32, 5)));
        assert_eq!(parse("May  1 14:32:05 host sshd").map(|s| s.ms), Some(day(2000, 5, 1) + t(14, 32, 5)));
        assert_eq!(parse("14:32:05.123456 thread"), Some(Stamp { ms: t(14, 32, 5) + 123, has_date: false }));
        assert_eq!(parse("id=123:45:67 nothing"), None);
        assert_eq!(parse("no time here"), None);
        assert!(day(2024, 3, 1) - day(2024, 2, 28) == 2 * DAY);
    }

    #[test]
    fn deltas() {
        assert_eq!(format_delta(850), "+850 ms");
        assert_eq!(format_delta(-12_345), "−12.345 s");
        assert_eq!(format_delta(185_120), "+3 m 05.120 s");
        assert_eq!(format_delta(7_385_000), "+2 h 03 m 05 s");
    }

    fn log_buf(lines: usize) -> (tempfile::NamedTempFile, TextBuf, Vec<u64>) {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        let mut offs = Vec::new();
        let mut pos = 0u64;
        let mut data = String::new();
        for i in 0..lines {
            offs.push(pos);
            // Two lines per second, a stack trace line without a time every 10th line.
            let s = i / 2;
            let line = if i % 10 == 9 { "    at some.Frame(File.java:42)\n".to_string() } else { format!("2024-05-01 {:02}:{:02}:{:02}.{:03} INFO line {i}\n", s / 3600, s / 60 % 60, s % 60, (i % 2) * 500) };
            pos += line.len() as u64;
            data.push_str(&line);
        }
        f.write_all(data.as_bytes()).unwrap();
        f.flush().unwrap();
        let src = Arc::new(Source::open(f.path(), OpenMode::Auto).unwrap());
        let idx = index::build_blocking(&src, Codec::utf8());
        let mut t = TextBuf::new(src, idx, Codec::utf8());
        t.blocking = true;
        (f, t, offs)
    }

    #[test]
    fn binary_search() {
        let (_f, t, offs) = log_buf(100_000);
        let c = CancelToken::new();
        // 00:30:00.000 is line 3600.
        let target = parse("2024-05-01 00:30:00").unwrap();
        assert_eq!(find_time(&t, target, &c), Ok(offs[3600]));
        // Time only: the date of the first line is used.
        assert_eq!(find_time(&t, parse("00:30:00.400").unwrap(), &c), Ok(offs[3601]));
        // Line 3609 has no time; 00:30:04.500 is line 3609 → the next stamped line is 3610.
        assert_eq!(find_time(&t, parse("00:30:04.500").unwrap(), &c), Ok(offs[3610]));
        assert_eq!(find_time(&t, parse("2024-04-30 23:00:00").unwrap(), &c), Ok(0));
        assert_eq!(find_time(&t, parse("2024-05-02 00:00:00").unwrap(), &c), Ok(t.len()));
    }

    #[test]
    fn no_timestamps() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"just\ntext\n").unwrap();
        let src = Arc::new(Source::open(f.path(), OpenMode::Auto).unwrap());
        let idx = index::build_blocking(&src, Codec::utf8());
        let mut t = TextBuf::new(src, idx, Codec::utf8());
        t.blocking = true;
        assert_eq!(find_time(&t, parse("10:00:00").unwrap(), &CancelToken::new()), Err(TimeError::NoTimestamps));
    }
}
