//! Pivot tables: one row per value of a column (or per day, hour or month of a time column), one
//! column per value of another, and in each cell the number of rows — or the sum, mean, minimum
//! or maximum of a third column — with a total per row.

use std::collections::HashMap;
use std::io::{self, Write};

use crate::csv::{self, Dialect, ScanEnd};
use crate::jobs::{CancelToken, Progress};
use crate::text::TextBuf;
use crate::timestamp;

/// Most columns of the result; rows with other column values are counted as left out.
pub const PIVOT_COLS_MAX: usize = 1000;
/// Most rows of the result.
pub const PIVOT_ROWS_MAX: usize = 100_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agg {
    Count,
    Sum,
    Mean,
    Min,
    Max,
}

impl Agg {
    pub const ALL: [Agg; 5] = [Agg::Count, Agg::Sum, Agg::Mean, Agg::Min, Agg::Max];

    pub fn name(self) -> &'static str {
        match self {
            Agg::Count => "count",
            Agg::Sum => "sum",
            Agg::Mean => "mean",
            Agg::Min => "min",
            Agg::Max => "max",
        }
    }
}

/// How the row column's values are grouped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bucket {
    AsIs,
    Hour,
    Day,
    Month,
}

impl Bucket {
    pub const ALL: [Bucket; 4] = [Bucket::AsIs, Bucket::Hour, Bucket::Day, Bucket::Month];
}

#[derive(Clone, Copy, Debug)]
pub struct PivotSpec {
    pub rows: usize,
    pub cols: usize,
    /// The column aggregated (none: rows are counted).
    pub value: Option<usize>,
    pub agg: Agg,
    pub bucket: Bucket,
    pub header: bool,
}

#[derive(Clone, Copy, Debug, Default)]
struct Cell {
    rows: u64,
    numeric: u64,
    sum: f64,
    min: Option<f64>,
    max: Option<f64>,
}

impl Cell {
    fn add(&mut self, v: Option<f64>) {
        self.rows += 1;
        if let Some(x) = v {
            self.numeric += 1;
            self.sum += x;
            self.min = Some(self.min.map_or(x, |m| m.min(x)));
            self.max = Some(self.max.map_or(x, |m| m.max(x)));
        }
    }

    fn get(&self, agg: Agg) -> Option<f64> {
        match agg {
            Agg::Count => Some(self.rows as f64),
            Agg::Sum => (self.numeric > 0).then_some(self.sum),
            Agg::Mean => (self.numeric > 0).then(|| self.sum / self.numeric as f64),
            Agg::Min => self.min,
            Agg::Max => self.max,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Pivot {
    pub row_name: String,
    rows: Vec<String>,
    cols: Vec<String>,
    cells: HashMap<(u32, u32), Cell>,
    totals: Vec<Cell>,
    /// Data rows read.
    pub records: u64,
    /// Rows left out because there were too many row or column values.
    pub left_out: u64,
    time_rows: bool,
}

impl Pivot {
    pub fn size(&self) -> (usize, usize) {
        (self.rows.len(), self.cols.len())
    }
}

/// The row key of `v` grouped by `b` (as it is when it is not a time).
fn bucket(v: &str, b: Bucket) -> (String, bool) {
    if b == Bucket::AsIs {
        return (v.to_string(), false);
    }
    // "2024-05-01 10:15" without seconds too.
    let Some(st) = timestamp::parse(v).or_else(|| timestamp::parse(&format!("{v}:00"))) else { return (v.to_string(), false) };
    let k = timestamp::format_key(st.ms, st.has_date, true);
    let key = match (b, st.has_date) {
        (Bucket::Hour, true) => format!("{}:00", &k[..13]),
        (Bucket::Hour, false) => format!("{}:00", &k[..2]),
        (Bucket::Day, true) => k[..10].to_string(),
        (Bucket::Month, true) => k[..7].to_string(),
        _ => return (v.to_string(), false),
    };
    (key, true)
}

/// Builds the pivot table in one pass over the records.
pub fn pivot(buf: &TextBuf, d: Dialect, spec: PivotSpec, cancel: &CancelToken, progress: Option<&Progress>) -> Result<Pivot, ScanEnd> {
    let codec = buf.codec;
    let mut p = Pivot::default();
    let (mut row_ids, mut col_ids): (HashMap<String, u32>, HashMap<String, u32>) = (HashMap::new(), HashMap::new());
    let mut first = spec.header;
    let mut failed = false;
    let mut timed = true;
    let end = csv::for_each_record(buf, d, cancel, progress, |r| {
        let mut field = |col: usize| -> String {
            match r.fields.get(col).map(|f| buf.read(f.clone())) {
                Some(Some(b)) => csv::field_value(&codec, d, &b),
                Some(None) => {
                    failed = true;
                    String::new()
                }
                None => String::new(),
            }
        };
        if first {
            first = false;
            p.row_name = field(spec.rows);
            return !failed;
        }
        if r.content.is_empty() && r.fields.len() == 1 {
            return true;
        }
        p.records += 1;
        let (row, is_time) = bucket(field(spec.rows).trim(), spec.bucket);
        timed &= is_time;
        let col = field(spec.cols).trim().to_string();
        let value = spec.value.and_then(|c| csv::parse_number(&field(c)));
        let ri = match row_ids.get(&row) {
            Some(&i) => i,
            None if p.rows.len() < PIVOT_ROWS_MAX => {
                p.rows.push(row.clone());
                p.totals.push(Cell::default());
                row_ids.insert(row, p.rows.len() as u32 - 1);
                p.rows.len() as u32 - 1
            }
            None => {
                p.left_out += 1;
                return !failed;
            }
        };
        let ci = match col_ids.get(&col) {
            Some(&i) => i,
            None if p.cols.len() < PIVOT_COLS_MAX => {
                p.cols.push(col.clone());
                col_ids.insert(col, p.cols.len() as u32 - 1);
                p.cols.len() as u32 - 1
            }
            None => {
                p.left_out += 1;
                return !failed;
            }
        };
        p.cells.entry((ri, ci)).or_default().add(value);
        p.totals[ri as usize].add(value);
        !failed
    });
    if failed {
        return Err(ScanEnd::Failed);
    }
    match end {
        ScanEnd::Done => {
            p.time_rows = timed && p.records > 0;
            Ok(p)
        }
        other => Err(other),
    }
}

fn number(x: f64) -> String {
    if x.fract() == 0.0 && x.abs() < 1e15 {
        format!("{}", x as i64)
    } else {
        format!("{x}")
    }
}

/// The pivot table as CSV: the row column, one column per value (in number order when they are
/// all numbers), and Total; rows by time when the rows are times, otherwise largest total first.
pub fn pivot_to_csv<W: Write>(p: &Pivot, agg: Agg, out: &mut W) -> io::Result<()> {
    let q = |s: &str| if s.contains([',', '"', '\n', '\r']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_string() };
    let mut cols: Vec<usize> = (0..p.cols.len()).collect();
    let nums: Option<Vec<f64>> = p.cols.iter().map(|c| csv::parse_number(c)).collect();
    match nums {
        Some(n) => cols.sort_by(|&a, &b| n[a].total_cmp(&n[b])),
        None => cols.sort_by(|&a, &b| p.cols[a].cmp(&p.cols[b])),
    }
    let mut rows: Vec<usize> = (0..p.rows.len()).collect();
    if p.time_rows {
        rows.sort_by(|&a, &b| p.rows[a].cmp(&p.rows[b]));
    } else {
        rows.sort_by(|&a, &b| p.totals[b].get(agg).unwrap_or(f64::MIN).total_cmp(&p.totals[a].get(agg).unwrap_or(f64::MIN)).then(a.cmp(&b)));
    }
    let name = if p.row_name.trim().is_empty() { "value" } else { p.row_name.as_str() };
    write!(out, "{}", q(name))?;
    for &c in &cols {
        write!(out, ",{}", q(if p.cols[c].is_empty() { "(empty)" } else { &p.cols[c] }))?;
    }
    writeln!(out, ",Total")?;
    for &r in &rows {
        write!(out, "{}", q(&p.rows[r]))?;
        for &c in &cols {
            let v = match p.cells.get(&(r as u32, c as u32)) {
                Some(cell) => cell.get(agg).map(number).unwrap_or_default(),
                None if agg == Agg::Count => "0".to_string(),
                None => String::new(),
            };
            write!(out, ",{v}")?;
        }
        writeln!(out, ",{}", p.totals[r].get(agg).map(number).unwrap_or_default())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::Codec;
    use crate::file::{OpenMode, Source};
    use std::sync::Arc;

    fn csv_of(data: &[u8], spec: PivotSpec) -> String {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(data).unwrap();
        f.flush().unwrap();
        let src = Arc::new(Source::open(f.path(), OpenMode::Auto).unwrap());
        let idx = crate::index::build_blocking(&src, Codec::utf8());
        let mut t = TextBuf::new(src, idx, Codec::utf8());
        t.blocking = true;
        let p = pivot(&t, Dialect::new(b','), spec, &CancelToken::new(), None).unwrap();
        let mut out = Vec::new();
        pivot_to_csv(&p, spec.agg, &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    const SALES: &[u8] = b"city,product,qty,ts\nTashkent,tea,3,2024-05-01 10:15\nBukhara,tea,5,2024-05-01 11:00\nTashkent,rice,7,2024-05-02 09:30\nTashkent,tea,1,2024-05-02 18:00\n";

    #[test]
    fn counts_and_sums() {
        let spec = PivotSpec { rows: 0, cols: 1, value: None, agg: Agg::Count, bucket: Bucket::AsIs, header: true };
        assert_eq!(csv_of(SALES, spec), "city,rice,tea,Total\nTashkent,1,2,3\nBukhara,0,1,1\n");
        let sum = PivotSpec { value: Some(2), agg: Agg::Sum, ..spec };
        assert_eq!(csv_of(SALES, sum), "city,rice,tea,Total\nTashkent,7,4,11\nBukhara,,5,5\n");
        let max = PivotSpec { value: Some(2), agg: Agg::Max, ..spec };
        assert_eq!(csv_of(SALES, max), "city,rice,tea,Total\nTashkent,7,3,7\nBukhara,,5,5\n");
    }

    #[test]
    fn time_buckets() {
        let by_day = PivotSpec { rows: 3, cols: 0, value: Some(2), agg: Agg::Sum, bucket: Bucket::Day, header: true };
        assert_eq!(csv_of(SALES, by_day), "ts,Bukhara,Tashkent,Total\n2024-05-01,5,3,8\n2024-05-02,,8,8\n");
        let by_hour = PivotSpec { bucket: Bucket::Hour, agg: Agg::Count, value: None, ..by_day };
        assert_eq!(csv_of(SALES, by_hour).lines().nth(1), Some("2024-05-01 10:00,0,1,1"));
    }
}
