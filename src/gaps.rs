//! Time gaps in a log: the places where the next line with a timestamp is much later than the
//! one before it (the service was down or quiet) or much earlier (the clock jumped back).

use std::io;

use crate::encoding::Codec;
use crate::jobs::{CancelToken, Progress, Throttle};
use crate::lines::LineReader;
use crate::text::TextBuf;
use crate::timestamp::{self, Clock, Stamp};

/// Most gaps listed.
pub const GAPS_MAX: usize = 1_000_000;

/// A gap between two lines with timestamps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gap {
    /// Starts of the line before the gap and of the line after it.
    pub before: u64,
    pub after: u64,
    /// Time keys ([`timestamp::format_key`]) of the two lines.
    pub from: i64,
    pub to: i64,
}

impl Gap {
    /// The gap's length; negative when the clock jumped back.
    pub fn ms(&self) -> i64 {
        self.to - self.from
    }
}

#[derive(Clone, Debug, Default)]
pub struct Gaps {
    /// In file order.
    pub gaps: Vec<Gap>,
    pub capped: bool,
    /// Whether the times have dates.
    pub dated: bool,
    /// Lines with a timestamp.
    pub stamped: u64,
}

fn stamp(codec: Codec, line: &[u8]) -> Option<Stamp> {
    if codec.unit() == 1 {
        timestamp::parse_bytes(line)
    } else {
        let n = line.len().min(640);
        timestamp::parse(&codec.decode_lossy(&line[..n - n % codec.unit()]))
    }
}

/// Finds the gaps of at least `min_ms` (either way) between lines with timestamps, in one pass.
/// A single line whose time is far from both neighbours (a time inside a message, misread as
/// the line's) does not make a gap. `partial` gets the gaps so far now and then. Returns `None`
/// when cancelled.
pub fn scan(buf: &TextBuf, min_ms: i64, cancel: &CancelToken, progress: Option<&Progress>, mut partial: impl FnMut(&Gaps)) -> io::Result<Option<Gaps>> {
    let min_ms = min_ms.max(1);
    if let Some(p) = progress {
        p.set_total(buf.len());
    }
    let mut out = Gaps::default();
    let mut clock = Clock::default();
    let mut dated = None;
    // The last line taken as the time so far, and a gap waiting for the next line to confirm it
    // (with the clock as it was before the line after the gap).
    let mut prev: Option<(u64, i64)> = None;
    let mut pending: Option<(Gap, Clock)> = None;
    let mut lines = LineReader::new(buf);
    let mut throttle = Throttle::new(std::time::Duration::from_millis(250));
    let mut n = 0u32;
    let far = |a: i64, b: i64| (b - a).abs() >= min_ms;
    let push = |out: &mut Gaps, g: Gap| {
        if out.gaps.len() < GAPS_MAX {
            out.gaps.push(g);
        } else {
            out.capped = true;
        }
    };
    while let Some((off, line)) = lines.next()? {
        if let Some(st) = stamp(buf.codec, line) {
            out.stamped += 1;
            let d = *dated.get_or_insert(st.has_date);
            let mut before = clock.clone();
            let mut key = clock.key(st, d);
            if let Some((g, pre)) = pending.take() {
                let mut alt = pre.clone();
                let alt_key = alt.key(st, d);
                if !far(g.from, alt_key) && far(g.to, key) {
                    // The line after the gap was an outlier: back to the line before it, and to
                    // the clock as it was then.
                    prev = Some((g.before, g.from));
                    clock = alt;
                    key = alt_key;
                    before = pre;
                } else {
                    push(&mut out, g);
                    prev = Some((g.after, g.to));
                }
            }
            match prev {
                Some((pl, pk)) if far(pk, key) => pending = Some((Gap { before: pl, after: off, from: pk, to: key }, before)),
                _ => prev = Some((off, key)),
            }
        }
        n += 1;
        if n.is_multiple_of(4096) {
            if cancel.is_cancelled() {
                return Ok(None);
            }
            if let Some(p) = progress {
                p.set_done(lines.taken());
            }
            if throttle.ready() {
                out.dated = dated.unwrap_or(true);
                partial(&out);
            }
        }
    }
    if let Some((g, _)) = pending {
        push(&mut out, g);
    }
    out.dated = dated.unwrap_or(true);
    Ok(Some(out))
}

/// "30s", "5m", "1h", "500ms", "2d" or a number of seconds, in milliseconds.
pub fn parse_duration(s: &str) -> Option<i64> {
    let t = s.trim().to_ascii_lowercase();
    let split = t.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(t.len());
    let (num, unit) = t.split_at(split);
    let v: f64 = num.parse().ok()?;
    let mul = match unit.trim() {
        "" | "s" | "sec" => 1000.0,
        "ms" => 1.0,
        "m" | "min" => 60_000.0,
        "h" => 3_600_000.0,
        "d" => 86_400_000.0,
        _ => return None,
    };
    let ms = (v * mul).round() as i64;
    (ms > 0).then_some(ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::{OpenMode, Source};
    use crate::index;
    use std::io::Write;
    use std::sync::Arc;

    fn gaps(data: &str, min_ms: i64) -> Vec<(String, String, i64)> {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(data.as_bytes()).unwrap();
        f.flush().unwrap();
        let src = Arc::new(Source::open(f.path(), OpenMode::Auto).unwrap());
        let idx = index::build_blocking(&src, Codec::utf8());
        let mut t = TextBuf::new(src, idx, Codec::utf8());
        t.blocking = true;
        let g = scan(&t, min_ms, &CancelToken::new(), None, |_| {}).unwrap().unwrap();
        let line = |off: u64| data[off as usize..].lines().next().unwrap().to_string();
        g.gaps.iter().map(|x| (line(x.before), line(x.after), x.ms())).collect()
    }

    #[test]
    fn finds_gaps_and_back_jumps() {
        let log = "2024-05-01 10:00:00 a\n2024-05-01 10:00:05 b\n  at trace\n2024-05-01 10:02:05 c\n2024-05-01 10:02:06 d\n2024-05-01 09:00:00 e\n2024-05-01 09:00:01 f\n";
        let g = gaps(log, 30_000);
        assert_eq!(g, [
            ("2024-05-01 10:00:05 b".into(), "2024-05-01 10:02:05 c".into(), 120_000),
            ("2024-05-01 10:02:06 d".into(), "2024-05-01 09:00:00 e".into(), -3_726_000),
        ]);
        assert!(gaps(log, 10 * 3_600_000).is_empty());
    }

    #[test]
    fn a_single_odd_time_is_not_a_gap() {
        let log = "10:00:00 a\n10:00:01 b\n23:59:00 retry scheduled\n10:00:02 c\n10:01:00 d\n";
        assert_eq!(gaps(log, 30_000), [("10:00:02 c".into(), "10:01:00 d".into(), 58_000)]);
        // A gap right at the end is kept.
        assert_eq!(gaps("10:00:00 a\n10:05:00 b\n", 60_000).len(), 1);
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("30s"), Some(30_000));
        assert_eq!(parse_duration("1.5m"), Some(90_000));
        assert_eq!(parse_duration("500ms"), Some(500));
        assert_eq!(parse_duration("2h"), Some(7_200_000));
        assert_eq!(parse_duration("45"), Some(45_000));
        assert_eq!(parse_duration("x"), None);
        assert_eq!(parse_duration("0s"), None);
    }
}
