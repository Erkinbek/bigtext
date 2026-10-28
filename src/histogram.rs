//! Time histogram of a log: how many lines, and how many of them match a pattern (errors, the
//! search), fall in each stretch of time — from one pass over the text. Buckets start one
//! second wide and get wider (10 s, 1 min, …) when the log spans too much time, so the
//! histogram stays small for any log.

use std::collections::BTreeMap;
use std::io;

use crate::encoding::Codec;
use crate::jobs::{CancelToken, Progress, Throttle};
use crate::lines::LineReader;
use crate::search::Matcher;
use crate::text::TextBuf;
use crate::timestamp::{self, Clock, Stamp};

/// The bucket width grows when there would be more buckets than this.
pub const MAX_BUCKETS: usize = 20_000;
const SEC: i64 = 1000;
const MIN: i64 = 60 * SEC;
const HOUR: i64 = 60 * MIN;
const DAY: i64 = 24 * HOUR;
/// Bucket widths, finest first; each divides the next.
const WIDTHS: [i64; 7] = [SEC, 10 * SEC, MIN, 10 * MIN, HOUR, DAY, 7 * DAY];
/// Bar widths offered for the chart.
const STEPS: [i64; 22] = [
    SEC,
    2 * SEC,
    5 * SEC,
    10 * SEC,
    15 * SEC,
    30 * SEC,
    MIN,
    2 * MIN,
    5 * MIN,
    10 * MIN,
    15 * MIN,
    30 * MIN,
    HOUR,
    2 * HOUR,
    3 * HOUR,
    6 * HOUR,
    12 * HOUR,
    DAY,
    2 * DAY,
    7 * DAY,
    14 * DAY,
    28 * DAY,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bucket {
    pub lines: u64,
    pub hits: u64,
    /// Offset of the bucket's first line.
    pub first: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Histogram {
    /// Bucket width in milliseconds.
    pub width: i64,
    /// Buckets with lines, by start time (keys of [`Clock::key`]).
    pub buckets: BTreeMap<i64, Bucket>,
    /// The times have dates (keys are dates, not times of day).
    pub dated: bool,
    /// All lines, and the lines that come before the first time (not in any bucket).
    pub lines: u64,
    pub untimed: u64,
    pub hits: u64,
}

impl Default for Histogram {
    fn default() -> Self {
        Histogram { width: WIDTHS[0], buckets: BTreeMap::new(), dated: true, lines: 0, untimed: 0, hits: 0 }
    }
}

impl Histogram {
    fn add(&mut self, key: i64, off: u64, hit: bool) {
        let b = self.buckets.entry(key.div_euclid(self.width) * self.width).or_insert(Bucket { lines: 0, hits: 0, first: off });
        b.lines += 1;
        b.hits += hit as u64;
        b.first = b.first.min(off);
        if self.buckets.len() > MAX_BUCKETS {
            self.coarsen();
        }
    }

    /// Merges the buckets into wider ones until there are few enough.
    fn coarsen(&mut self) {
        while self.buckets.len() > MAX_BUCKETS {
            let Some(&w) = WIDTHS.iter().find(|&&w| w > self.width) else { return };
            self.width = w;
            let old = std::mem::take(&mut self.buckets);
            for (k, b) in old {
                let e = self.buckets.entry(k.div_euclid(w) * w).or_insert(Bucket { lines: 0, hits: 0, first: b.first });
                e.lines += b.lines;
                e.hits += b.hits;
                e.first = e.first.min(b.first);
            }
        }
    }

    /// First and last bucket start.
    pub fn span(&self) -> Option<(i64, i64)> {
        Some((*self.buckets.first_key_value()?.0, *self.buckets.last_key_value()?.0))
    }

    /// The narrowest bar width (a multiple of the bucket width) that needs at most `max_bars`
    /// bars for the whole span.
    pub fn step_for(&self, max_bars: usize) -> i64 {
        let Some((a, b)) = self.span() else { return self.width };
        let fits = |s: i64| ((b.div_euclid(s) - a.div_euclid(s)) as usize) < max_bars.max(1);
        STEPS.iter().copied().filter(|s| s % self.width == 0).find(|&s| fits(s)).unwrap_or_else(|| {
            // Longer than the widest step allows: whole multiples of it.
            let w = STEPS[STEPS.len() - 1];
            let n = ((b - a) / w) as usize / max_bars.max(1) + 1;
            w * n as i64
        })
    }

    /// The buckets summed into bars `step` wide: (start, totals), in time order, without empty
    /// bars.
    pub fn bars(&self, step: i64) -> Vec<(i64, Bucket)> {
        let mut out: Vec<(i64, Bucket)> = Vec::new();
        for (&k, b) in &self.buckets {
            let start = k.div_euclid(step) * step;
            match out.last_mut() {
                Some((s, t)) if *s == start => {
                    t.lines += b.lines;
                    t.hits += b.hits;
                    t.first = t.first.min(b.first);
                }
                _ => out.push((start, *b)),
            }
        }
        out
    }
}

fn stamp(codec: Codec, line: &[u8]) -> Option<Stamp> {
    if codec.unit() == 1 {
        timestamp::parse_bytes(line)
    } else {
        let n = line.len().min(640);
        timestamp::parse(&codec.decode_lossy(&line[..n - n % codec.unit()]))
    }
}

/// Builds the histogram of `buf`; lines that `m` matches count as hits. Lines without a time
/// count in the bucket of the line above. `partial` gets the histogram so far now and then.
/// Returns `None` when cancelled.
pub fn build(buf: &TextBuf, m: Option<&Matcher>, cancel: &CancelToken, progress: Option<&Progress>, mut partial: impl FnMut(&Histogram)) -> io::Result<Option<Histogram>> {
    let codec = buf.codec;
    if let Some(p) = progress {
        p.set_total(buf.len());
    }
    let mut h = Histogram::default();
    let mut clock = Clock::default();
    let mut dated = None;
    let mut last_key = None;
    let mut lines = LineReader::new(buf);
    let mut throttle = Throttle::new(std::time::Duration::from_millis(250));
    let mut n = 0u32;
    while let Some((off, line)) = lines.next()? {
        h.lines += 1;
        let hit = m.is_some_and(|m| {
            let mut found = false;
            m.find_in(line, off, 0, line.len(), line.len(), &mut None, &mut |_, _| {
                found = true;
                false
            });
            found
        });
        h.hits += hit as u64;
        if let Some(st) = stamp(codec, line) {
            let d = *dated.get_or_insert(st.has_date);
            last_key = Some(clock.key(st, d));
        }
        match last_key {
            Some(k) => h.add(k, off, hit),
            None => h.untimed += 1,
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
                h.dated = dated.unwrap_or(true);
                partial(&h);
            }
        }
    }
    h.dated = dated.unwrap_or(true);
    Ok(Some(h))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding;
    use crate::file::{OpenMode, Source};
    use crate::index::LineIndex;
    use crate::search::Query;
    use std::sync::Arc;

    fn hist(bytes: &[u8], pattern: Option<&str>) -> Histogram {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.log");
        std::fs::write(&path, bytes).unwrap();
        let src = Arc::new(Source::open(&path, OpenMode::Auto).unwrap());
        let det = encoding::detect(&src.read(0..src.len()).unwrap(), true);
        let len = src.len();
        let buf = TextBuf::new(src, LineIndex::new(len), det.codec);
        let m = pattern.map(|p| Matcher::new(&Query { pattern: p.into(), case_sensitive: true, ..Default::default() }, det.codec).unwrap());
        build(&buf, m.as_ref(), &CancelToken::new(), None, |_| {}).unwrap().unwrap()
    }

    #[test]
    fn counts_lines_and_hits_per_second() {
        let text = "head\n2024-05-01 10:00:00 a ERROR\n  trace ERROR\n2024-05-01 10:00:00 b\n2024-05-01 10:00:02 c ERROR\n";
        let h = hist(text.as_bytes(), Some("ERROR"));
        let t0 = timestamp::parse("2024-05-01 10:00:00").unwrap().ms;
        assert_eq!((h.lines, h.untimed, h.hits, h.width, h.dated), (5, 1, 3, SEC, true));
        assert_eq!(h.buckets.get(&t0), Some(&Bucket { lines: 3, hits: 2, first: 5 }));
        assert_eq!(h.buckets.get(&(t0 + 2 * SEC)).map(|b| (b.lines, b.hits)), Some((1, 1)));
        assert_eq!(h.span(), Some((t0, t0 + 2 * SEC)));
        // Ten-second bars put every timed line in one bar.
        assert_eq!(h.bars(10 * SEC), vec![(t0, Bucket { lines: 4, hits: 3, first: 5 })]);
    }

    #[test]
    fn buckets_widen_for_long_logs() {
        let mut text = String::new();
        // One line a minute for 30 days: too many seconds and minutes, so 10-minute buckets.
        for i in 0..30 * 24 * 60i64 {
            let (d, h, m) = (1 + i / 1440, i / 60 % 24, i % 60);
            text.push_str(&format!("2024-05-{d:02} {h:02}:{m:02}:00 x\n"));
        }
        let h = hist(text.as_bytes(), None);
        assert_eq!(h.width, 10 * MIN);
        assert!(h.buckets.len() <= MAX_BUCKETS);
        assert_eq!(h.buckets.values().map(|b| b.lines).sum::<u64>(), 30 * 24 * 60);
        assert!(h.buckets.values().all(|b| b.lines == 10));
        // About 100 bars over 30 days: 12-hour bars.
        assert_eq!(h.step_for(100), 12 * HOUR);
        assert_eq!(h.bars(DAY).len(), 30);
    }

    #[test]
    fn times_without_dates() {
        let h = hist(b"23:59:59 a\n00:00:01 b\n", None);
        assert!(!h.dated);
        assert_eq!(h.span(), Some((DAY - SEC, DAY + SEC)));
        assert_eq!(timestamp::format_key(DAY + SEC, h.dated, true), "00:00:01 +1 d");
    }

    #[test]
    fn utf16_logs() {
        let utf16: Vec<u8> = [0xFF, 0xFE].into_iter().chain("10:00:00 x\r\n10:00:01 ERROR\r\n".encode_utf16().flat_map(|u| u.to_le_bytes())).collect();
        let h = hist(&utf16, Some("ERROR"));
        assert_eq!((h.lines, h.hits, h.buckets.len()), (2, 1, 2));
    }
}
