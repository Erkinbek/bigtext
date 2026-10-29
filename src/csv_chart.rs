//! The distribution of one table column: how many rows have values in each range (numbers),
//! each period (times), or of each value (text, the most frequent ones).

use std::collections::{BTreeMap, HashMap};

use crate::csv::{self, Dialect, ScanEnd};
use crate::jobs::{CancelToken, Progress, Throttle};
use crate::text::TextBuf;
use crate::timestamp;

/// Values looked at to decide what the column holds.
const SAMPLE: usize = 1000;
/// Ranges kept; past this the ranges get twice as wide.
const RANGES_MAX: usize = 400;
/// Different texts counted, and the most frequent shown.
const TEXTS_MAX: usize = 100_000;
pub const TEXT_BARS: usize = 50;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChartKind {
    Numbers,
    Times,
    Text,
}

/// One bar: its label, rows, the range it covers (numbers and times; times in milliseconds) and
/// where its first row starts.
#[derive(Clone, Debug, PartialEq)]
pub struct Bar {
    pub label: String,
    pub count: u64,
    pub lo: f64,
    pub hi: f64,
    pub first: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ColumnChart {
    pub kind: ChartKind,
    pub bars: Vec<Bar>,
    /// Rows counted in a bar.
    pub values: u64,
    /// Rows left out: empty, or not a number or time in such a column.
    pub other: u64,
    /// Text: rows with values beyond the different ones counted.
    pub capped: u64,
}

fn nice(x: f64) -> f64 {
    if !(x > 0.0) || !x.is_finite() {
        return 1.0;
    }
    let p = 10f64.powf(x.log10().floor());
    let m = x / p;
    p * if m <= 1.0 { 1.0 } else if m <= 2.0 { 2.0 } else if m <= 5.0 { 5.0 } else { 10.0 }
}

const TIME_STEPS: [f64; 19] = [1e3, 2e3, 5e3, 1e4, 15e3, 3e4, 6e4, 12e4, 3e5, 6e5, 9e5, 18e5, 36e5, 72e5, 108e5, 216e5, 432e5, 864e5, 6048e5];

fn nice_time(x: f64) -> f64 {
    TIME_STEPS.into_iter().find(|&s| s >= x).unwrap_or(30.0 * 864e5)
}

fn show(x: f64) -> String {
    if x.fract() == 0.0 && x.abs() < 1e15 {
        format!("{}", x as i64)
    } else {
        let s = format!("{x:.6}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// Counts in ranges of `width` that double when there are too many.
struct Ranges {
    width: f64,
    map: BTreeMap<i64, (u64, u64)>,
}

impl Ranges {
    fn add(&mut self, v: f64, off: u64) {
        let k = (v / self.width).floor().clamp(-9e18, 9e18) as i64;
        let e = self.map.entry(k).or_insert((0, off));
        e.0 += 1;
        if self.map.len() > RANGES_MAX {
            self.width *= 2.0;
            let old = std::mem::take(&mut self.map);
            for (k, (n, first)) in old {
                let e = self.map.entry(k.div_euclid(2)).or_insert((0, first));
                e.0 += n;
                e.1 = e.1.min(first);
            }
        }
    }
}

struct Builder {
    kind: ChartKind,
    ranges: Ranges,
    texts: HashMap<String, (u64, u64)>,
    dated: bool,
    out: ColumnChart,
}

impl Builder {
    fn add(&mut self, v: &str, off: u64) {
        let t = v.trim();
        if t.is_empty() {
            self.out.other += 1;
            return;
        }
        match self.kind {
            ChartKind::Numbers => match csv::parse_number(t) {
                Some(x) if x.is_finite() => {
                    self.ranges.add(x, off);
                    self.out.values += 1;
                }
                _ => self.out.other += 1,
            },
            ChartKind::Times => match timestamp::parse(t) {
                Some(st) => {
                    self.ranges.add(st.ms as f64, off);
                    self.out.values += 1;
                }
                None => self.out.other += 1,
            },
            ChartKind::Text => {
                self.out.values += 1;
                if let Some(e) = self.texts.get_mut(t) {
                    e.0 += 1;
                } else if self.texts.len() < TEXTS_MAX {
                    self.texts.insert(t.to_string(), (1, off));
                } else {
                    self.out.capped += 1;
                }
            }
        }
    }

    fn chart(&self) -> ColumnChart {
        let mut c = self.out.clone();
        c.kind = self.kind;
        c.bars = match self.kind {
            ChartKind::Text => {
                let mut v: Vec<(&String, &(u64, u64))> = self.texts.iter().collect();
                v.sort_by(|a, b| b.1 .0.cmp(&a.1 .0).then(a.1 .1.cmp(&b.1 .1)));
                v.into_iter().take(TEXT_BARS).map(|(k, &(n, first))| Bar { label: k.clone(), count: n, lo: f64::NAN, hi: f64::NAN, first }).collect()
            }
            kind => {
                let w = self.ranges.width;
                self.ranges
                    .map
                    .iter()
                    .map(|(&k, &(n, first))| {
                        let (lo, hi) = (k as f64 * w, (k + 1) as f64 * w);
                        let label = match kind {
                            ChartKind::Times => timestamp::format_key(lo as i64, self.dated, true),
                            _ => format!("{} – {}", show(lo), show(hi)),
                        };
                        Bar { label, count: n, lo, hi, first }
                    })
                    .collect()
            }
        };
        c
    }
}

/// Counts the values of column `col` in one pass; what the column holds is decided from its
/// first values. `partial` gets the chart so far now and then.
pub fn column_chart(buf: &TextBuf, d: Dialect, col: usize, header: bool, cancel: &CancelToken, progress: Option<&Progress>, mut partial: impl FnMut(&ColumnChart)) -> Result<ColumnChart, ScanEnd> {
    let codec = buf.codec;
    let mut sample: Vec<(String, u64)> = Vec::new();
    let mut b: Option<Builder> = None;
    let mut first = header;
    let mut failed = false;
    let mut throttle = Throttle::new(std::time::Duration::from_millis(250));
    let decide = |sample: &[(String, u64)]| -> Builder {
        let vals: Vec<&str> = sample.iter().map(|s| s.0.trim()).filter(|s| !s.is_empty()).collect();
        let nums: Vec<f64> = vals.iter().filter_map(|v| csv::parse_number(v)).filter(|x| x.is_finite()).collect();
        let times: Vec<timestamp::Stamp> = vals.iter().filter_map(|v| timestamp::parse(v)).collect();
        let most = |n: usize| !vals.is_empty() && n * 10 >= vals.len() * 8;
        let (kind, width, dated) = if most(times.len()) && !most(nums.len()) {
            let (lo, hi) = times.iter().fold((i64::MAX, i64::MIN), |(a, b), s| (a.min(s.ms), b.max(s.ms)));
            (ChartKind::Times, nice_time((hi - lo) as f64 / 60.0), times.iter().all(|s| s.has_date))
        } else if most(nums.len()) {
            let (lo, hi) = nums.iter().fold((f64::MAX, f64::MIN), |(a, b), &x| (a.min(x), b.max(x)));
            (ChartKind::Numbers, nice((hi - lo) / 50.0), false)
        } else {
            (ChartKind::Text, 1.0, false)
        };
        let out = ColumnChart { kind, bars: Vec::new(), values: 0, other: 0, capped: 0 };
        let mut b = Builder { kind, ranges: Ranges { width, map: BTreeMap::new() }, texts: HashMap::new(), dated, out };
        for (v, off) in sample {
            b.add(v, *off);
        }
        b
    };
    let end = csv::for_each_record(buf, d, cancel, progress, |r| {
        if r.content.is_empty() && r.fields.len() == 1 {
            return true;
        }
        let v = match r.fields.get(col).map(|f| buf.read(f.clone())) {
            Some(Some(x)) => csv::field_value(&codec, d, &x),
            Some(None) => {
                failed = true;
                return false;
            }
            None => String::new(),
        };
        if first {
            first = false;
            return true;
        }
        match b.as_mut() {
            Some(b) => b.add(&v, r.content.start),
            None => {
                sample.push((v, r.content.start));
                if sample.len() >= SAMPLE {
                    b = Some(decide(&sample));
                    sample.clear();
                }
            }
        }
        if let Some(b) = &b {
            if throttle.ready() {
                partial(&b.chart());
            }
        }
        true
    });
    if failed {
        return Err(ScanEnd::Failed);
    }
    match end {
        ScanEnd::Done => Ok(b.unwrap_or_else(|| decide(&sample)).chart()),
        other => Err(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::Codec;
    use crate::file::{OpenMode, Source};
    use std::io::Write;
    use std::sync::Arc;

    fn chart(data: &str, col: usize) -> ColumnChart {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(data.as_bytes()).unwrap();
        f.flush().unwrap();
        let src = Arc::new(Source::open(f.path(), OpenMode::Auto).unwrap());
        let idx = crate::index::build_blocking(&src, Codec::utf8());
        let mut t = TextBuf::new(src, idx, Codec::utf8());
        t.blocking = true;
        column_chart(&t, Dialect::new(b','), col, true, &CancelToken::new(), None, |_| {}).unwrap()
    }

    #[test]
    fn numbers_text_and_times() {
        let c = chart("city,qty,ts\nT,3,2024-05-01 10:00:00\nB,5,2024-05-01 10:30:00\nT,7,2024-05-01 12:00:00\nT,,x\n", 1);
        assert_eq!(c.kind, ChartKind::Numbers);
        assert_eq!((c.values, c.other), (3, 1));
        assert_eq!(c.bars.iter().map(|b| b.count).sum::<u64>(), 3);
        assert!(c.bars[0].lo <= 3.0 && c.bars[0].hi > 3.0);
        let t = chart("city,qty\nT,3\nB,5\nT,7\nB,1\nT,2\n", 0);
        assert_eq!(t.kind, ChartKind::Text);
        assert_eq!((t.bars[0].label.as_str(), t.bars[0].count, t.bars[1].label.as_str()), ("T", 3, "B"));
        let m = chart("city,qty,ts\nT,3,2024-05-01 10:00:00\nB,5,2024-05-01 10:30:00\nT,7,2024-05-01 12:00:00\n", 2);
        assert_eq!(m.kind, ChartKind::Times);
        assert_eq!(m.values, 3);
        assert!(m.bars[0].label.starts_with("2024-05-01 10:"), "{:?}", m.bars[0].label);
    }

    #[test]
    fn many_values_widen_the_ranges() {
        let mut data = String::from("v\n");
        for i in 0..5000 {
            data.push_str(&format!("{}\n", i * 37 % 100_000));
        }
        data.push_str("1e9\n");
        let c = chart(&data, 0);
        assert!(c.bars.len() <= RANGES_MAX, "{} bars", c.bars.len());
        assert_eq!(c.values, 5001);
        assert_eq!(c.bars.iter().map(|b| b.count).sum::<u64>(), 5001);
    }
}
