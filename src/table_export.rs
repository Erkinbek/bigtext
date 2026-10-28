//! Exporting a CSV table, or some of its columns, as JSON, JSON Lines, a Markdown table or an
//! Excel workbook (.xlsx), streamed record by record. The header names the fields; without one
//! the columns are called `column1`, `column2`, ….

use std::io::{self, Seek, Write};

use crate::csv::{field_value, for_each_record, Dialect, ScanEnd};
use crate::jobs::{CancelToken, Progress};
use crate::text::TextBuf;

/// An Excel sheet holds at most this many rows and columns.
pub const XLSX_MAX_ROWS: u64 = 1_048_576;
pub const XLSX_MAX_COLS: usize = 16_384;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    Json,
    JsonLines,
    Markdown,
    Xlsx,
}

impl Format {
    pub const ALL: [Format; 4] = [Format::Json, Format::JsonLines, Format::Markdown, Format::Xlsx];

    pub fn name(self) -> &'static str {
        match self {
            Format::Json => "JSON",
            Format::JsonLines => "JSON Lines",
            Format::Markdown => "Markdown table",
            Format::Xlsx => "Excel workbook (.xlsx)",
        }
    }

    pub fn ext(self) -> &'static str {
        match self {
            Format::Json => "json",
            Format::JsonLines => "jsonl",
            Format::Markdown => "md",
            Format::Xlsx => "xlsx",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Exported {
    pub rows: u64,
    /// Rows or columns past Excel's limits were left out.
    pub truncated: bool,
}

fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn markdown_cell(s: &str) -> String {
    s.replace('\\', "\\\\").replace('|', "\\|").replace("\r\n", "<br>").replace(['\n', '\r'], "<br>")
}

fn xml_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\t' | '\n' | '\r' => out.push(c),
            // Not allowed in XML 1.0.
            c if (c as u32) < 0x20 || c == '\u{FFFE}' || c == '\u{FFFF}' => {}
            c => out.push(c),
        }
    }
    out
}

/// "A", "B", …, "Z", "AA", … for a 0-based column.
fn column_letters(mut col: usize) -> String {
    let mut s = Vec::new();
    loop {
        s.push(b'A' + (col % 26) as u8);
        if col < 26 {
            break;
        }
        col = col / 26 - 1;
    }
    s.reverse();
    String::from_utf8(s).unwrap_or_default()
}

/// A number Excel can store without changing how it reads: no leading zeros or plus signs.
fn as_number(s: &str) -> Option<f64> {
    let t = s.trim();
    if t != s || t.is_empty() || t.starts_with('+') || (t.len() > 1 && t.starts_with('0') && !t.starts_with("0.")) || t.starts_with("-0") && t.len() > 2 && !t.starts_with("-0.") {
        return None;
    }
    let n: f64 = t.parse().ok()?;
    n.is_finite().then_some(n)
}

const XLSX_PARTS: [(&str, &str); 4] = [
    (
        "[Content_Types].xml",
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#,
    ),
    (
        "_rels/.rels",
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#,
    ),
    (
        "xl/workbook.xml",
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
    ),
    (
        "xl/_rels/workbook.xml.rels",
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
    ),
];

/// What to export.
#[derive(Clone, Copy, Debug)]
pub struct Spec<'a> {
    pub dialect: Dialect,
    /// These columns, in this order (all when `None`).
    pub cols: Option<&'a [usize]>,
    /// The first record names the columns.
    pub header: bool,
    pub format: Format,
}

/// Writes the table as `spec` says. Returns `None` if cancelled.
pub fn export<W: Write + Seek>(buf: &TextBuf, spec: Spec, out: W, cancel: &CancelToken, progress: Option<&Progress>) -> io::Result<Option<Exported>> {
    let Spec { dialect: d, cols, header, format } = spec;
    let codec = buf.codec;
    let read_err = || io::Error::other("could not read the document");
    let mut names: Option<Vec<String>> = None;
    let mut stats = Exported::default();
    let mut err: Option<io::Error> = None;
    let mut zip = None;
    let mut plain = None;
    match format {
        Format::Xlsx => {
            let mut z = zip::ZipWriter::new(out);
            let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
            for (name, text) in XLSX_PARTS {
                z.start_file(name, opts).map_err(io::Error::other)?;
                z.write_all(text.as_bytes())?;
            }
            z.start_file("xl/worksheets/sheet1.xml", opts).map_err(io::Error::other)?;
            z.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>"#)?;
            zip = Some(z);
        }
        Format::Json => {
            let mut w = io::BufWriter::new(out);
            w.write_all(b"[")?;
            plain = Some(w);
        }
        _ => plain = Some(io::BufWriter::new(out)),
    }
    let mut xlsx_row = 0u64;
    let mut values: Vec<String> = Vec::new();
    let end = for_each_record(buf, d, cancel, progress, |r| {
        // A blank line at the very end is not a row.
        if r.content.is_empty() && r.next == r.content.end && r.fields.len() == 1 && names.is_some() {
            return true;
        }
        values.clear();
        let pick: Vec<usize> = match cols {
            Some(c) => c.to_vec(),
            None => (0..r.fields.len().max(names.as_ref().map(Vec::len).unwrap_or(0))).collect(),
        };
        for &c in &pick {
            let v = match r.fields.get(c) {
                Some(f) => match buf.read(f.clone()) {
                    Some(b) => field_value(&codec, d, &b),
                    None => {
                        err = Some(read_err());
                        return false;
                    }
                },
                None => String::new(),
            };
            values.push(v);
        }
        let first = names.is_none();
        if first {
            names = Some(if header { values.iter().enumerate().map(|(i, v)| if v.is_empty() { format!("column{}", i + 1) } else { v.clone() }).collect() } else { (1..=values.len()).map(|i| format!("column{i}")).collect() });
        }
        let names = names.as_ref().unwrap();
        let name = |i: usize| names.get(i).cloned().unwrap_or_else(|| format!("column{}", i + 1));
        let r: io::Result<()> = (|| {
            match format {
                Format::Xlsx => {
                    let z = zip.as_mut().unwrap();
                    let mut emit = |row: &[String], z: &mut zip::ZipWriter<W>| -> io::Result<()> {
                        if xlsx_row >= XLSX_MAX_ROWS {
                            stats.truncated = true;
                            return Ok(());
                        }
                        xlsx_row += 1;
                        let mut s = format!("<row r=\"{xlsx_row}\">");
                        for (i, v) in row.iter().enumerate().take(XLSX_MAX_COLS) {
                            let at = format!("{}{xlsx_row}", column_letters(i));
                            match as_number(v) {
                                Some(_) => s.push_str(&format!("<c r=\"{at}\"><v>{}</v></c>", v.trim())),
                                None if v.is_empty() => {}
                                None => s.push_str(&format!("<c r=\"{at}\" t=\"inlineStr\"><is><t xml:space=\"preserve\">{}</t></is></c>", xml_text(v))),
                            }
                        }
                        stats.truncated |= row.len() > XLSX_MAX_COLS;
                        s.push_str("</row>");
                        z.write_all(s.as_bytes())
                    };
                    if first && !header {
                        emit(names, z)?;
                    }
                    if !(first && header) {
                        emit(&values, z)?;
                        stats.rows += 1;
                    } else {
                        emit(&values, z)?;
                    }
                }
                Format::Markdown => {
                    let w = plain.as_mut().unwrap();
                    if first {
                        let head: Vec<String> = (0..values.len()).map(|i| markdown_cell(&name(i))).collect();
                        writeln!(w, "| {} |", head.join(" | "))?;
                        writeln!(w, "|{}", " --- |".repeat(head.len().max(1)))?;
                        if header {
                            return Ok(());
                        }
                    }
                    let cells: Vec<String> = values.iter().map(|v| markdown_cell(v)).collect();
                    writeln!(w, "| {} |", cells.join(" | "))?;
                    stats.rows += 1;
                }
                Format::Json | Format::JsonLines => {
                    if first && header {
                        return Ok(());
                    }
                    let w = plain.as_mut().unwrap();
                    let obj: Vec<String> = values.iter().enumerate().map(|(i, v)| format!("{}: {}", json_string(&name(i)), json_string(v))).collect();
                    if format == Format::Json {
                        w.write_all(if stats.rows == 0 { b"\n  " } else { b",\n  " })?;
                        write!(w, "{{{}}}", obj.join(", "))?;
                    } else {
                        writeln!(w, "{{{}}}", obj.join(", "))?;
                    }
                    stats.rows += 1;
                }
            }
            Ok(())
        })();
        if let Err(e) = r {
            err = Some(e);
            return false;
        }
        true
    });
    if let Some(e) = err {
        return Err(e);
    }
    match end {
        ScanEnd::Cancelled => return Ok(None),
        ScanEnd::Done => {}
        _ => return Err(read_err()),
    }
    if let Some(mut z) = zip {
        z.write_all(b"</sheetData></worksheet>")?;
        z.finish().map_err(io::Error::other)?;
    }
    if let Some(mut w) = plain {
        if format == Format::Json {
            w.write_all(if stats.rows == 0 { b"]\n" } else { b"\n]\n" })?;
        }
        w.flush()?;
    }
    Ok(Some(stats))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::Codec;
    use crate::file::{OpenMode, Source};
    use crate::index::LineIndex;
    use std::io::Read;
    use std::sync::Arc;

    fn buf(text: &str) -> (tempfile::TempDir, TextBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.csv");
        std::fs::write(&path, text).unwrap();
        let src = Arc::new(Source::open(&path, OpenMode::Auto).unwrap());
        let len = src.len();
        (dir, TextBuf::new(src, LineIndex::new(len), Codec::utf8()))
    }

    fn run(text: &str, cols: Option<&[usize]>, header: bool, f: Format) -> (Vec<u8>, Exported) {
        let (_d, b) = buf(text);
        let mut out = io::Cursor::new(Vec::new());
        let spec = Spec { dialect: Dialect::new(b','), cols, header, format: f };
        let st = export(&b, spec, &mut out, &CancelToken::new(), None).unwrap().unwrap();
        (out.into_inner(), st)
    }

    const T: &str = "name,qty,note\nAnn,007,\"a|b\"\nBob,12.5,\"x\"\"y\"\n";

    #[test]
    fn json_and_lines() {
        let (j, st) = run(T, None, true, Format::Json);
        assert_eq!(String::from_utf8(j).unwrap(), "[\n  {\"name\": \"Ann\", \"qty\": \"007\", \"note\": \"a|b\"},\n  {\"name\": \"Bob\", \"qty\": \"12.5\", \"note\": \"x\\\"y\"}\n]\n");
        assert_eq!(st.rows, 2);
        let (l, _) = run(T, Some(&[1, 0]), true, Format::JsonLines);
        assert_eq!(String::from_utf8(l).unwrap(), "{\"qty\": \"007\", \"name\": \"Ann\"}\n{\"qty\": \"12.5\", \"name\": \"Bob\"}\n");
        let (n, st) = run("1,2\n", None, false, Format::JsonLines);
        assert_eq!((String::from_utf8(n).unwrap().as_str(), st.rows), ("{\"column1\": \"1\", \"column2\": \"2\"}\n", 1));
        let (e, _) = run("a,b\n", None, true, Format::Json);
        assert_eq!(String::from_utf8(e).unwrap(), "[]\n");
    }

    #[test]
    fn markdown() {
        let (m, st) = run(T, None, true, Format::Markdown);
        assert_eq!(String::from_utf8(m).unwrap(), "| name | qty | note |\n| --- | --- | --- |\n| Ann | 007 | a\\|b |\n| Bob | 12.5 | x\"y |\n");
        assert_eq!(st.rows, 2);
    }

    #[test]
    fn xlsx_is_a_zip_with_the_sheet() {
        let (x, st) = run(T, None, true, Format::Xlsx);
        assert_eq!(st, Exported { rows: 2, truncated: false });
        let mut z = zip::ZipArchive::new(io::Cursor::new(x)).unwrap();
        let mut sheet = String::new();
        z.by_name("xl/worksheets/sheet1.xml").unwrap().read_to_string(&mut sheet).unwrap();
        assert!(sheet.contains(r#"<row r="1"><c r="A1" t="inlineStr"><is><t xml:space="preserve">name</t></is></c>"#), "{sheet}");
        // "007" stays text, 12.5 is a number.
        assert!(sheet.contains(r#"<c r="B2" t="inlineStr"><is><t xml:space="preserve">007</t></is></c>"#));
        assert!(sheet.contains(r#"<c r="B3"><v>12.5</v></c>"#));
        assert!(sheet.contains("x&quot;y"));
        for name in ["[Content_Types].xml", "_rels/.rels", "xl/workbook.xml", "xl/_rels/workbook.xml.rels"] {
            assert!(z.by_name(name).is_ok(), "{name}");
        }
        assert_eq!(column_letters(0), "A");
        assert_eq!(column_letters(25), "Z");
        assert_eq!(column_letters(26), "AA");
        assert_eq!(column_letters(16_383), "XFD");
    }
}
