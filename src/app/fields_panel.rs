//! Log fields: the caret's JSON Lines or logfmt line as a key → value table that follows the
//! caret, with a filter, an exclusion and a highlight per field, and a count of a field's values
//! over the whole file.

use bigtext::fields::{self, Field, Syntax};
use bigtext::jobs::{CancelToken, Progress};
use bigtext::search::Query;

use super::filter_panel::fmt_count;
use super::i18n::{tr, trf};
use super::tasks::TaskOutcome;
use super::BigTextApp;

/// A longer line is not parsed.
const LINE_MAX: u64 = 64 * 1024;
/// Characters of a value shown in a row (the hover shows all of it).
const SHOW_MAX: usize = 300;
/// Lines of a multi-line value shown in a row.
const SHOW_LINES: usize = 8;

#[derive(Clone)]
pub(crate) enum Parsed {
    Fields(Syntax, Vec<Field>),
    NotStructured,
    TooLong,
}

#[derive(Default)]
pub struct FieldsUi {
    pub open: bool,
    /// The parsed caret line, for (document, version, line start).
    cache: Option<((u64, u64, u64), Parsed)>,
}

enum Act {
    Filter(Query, bool),
    Highlight(Query),
    Count(String, Syntax),
}

impl BigTextApp {
    pub(crate) fn caret_fields(&mut self) -> Option<Parsed> {
        let d = self.docs.get(self.active)?;
        if !d.is_ready() || d.hex {
            return None;
        }
        let (ls, le, _) = d.line_bounds(d.view.caret, LINE_MAX)?;
        let key = (d.id, d.version, ls);
        if let Some((k, p)) = &self.fields.cache {
            if *k == key {
                return Some(p.clone());
            }
        }
        let start = ls.max(d.buf.codec.bom_len as u64);
        let parsed = if le - start > LINE_MAX {
            Parsed::TooLong
        } else {
            let text = d.buf.codec.decode_lossy(&d.buf.read(start..le)?);
            match fields::line_fields(&text) {
                Some((s, f)) => Parsed::Fields(s, f),
                None => Parsed::NotStructured,
            }
        };
        self.fields.cache = Some((key, parsed.clone()));
        Some(parsed)
    }

    pub(super) fn fields_panel(&mut self, ui: &mut egui::Ui) {
        let parsed = self.caret_fields();
        let mut act = None;
        ui.horizontal(|ui| {
            ui.strong(tr("Log fields"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("×").clicked() {
                    self.fields.open = false;
                }
            });
        });
        ui.separator();
        match parsed {
            None => {}
            Some(Parsed::TooLong) => {
                ui.weak(trf("The line is longer than {size}.", &[("size", &bigtext::jobs::human_bytes(LINE_MAX))]));
            }
            Some(Parsed::NotStructured) => {
                ui.weak(tr("The caret's line is not a JSON object or logfmt key=value pairs."));
            }
            Some(Parsed::Fields(syntax, list)) => {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    egui::Grid::new("log-fields").num_columns(3).striped(true).show(ui, |ui| {
                        for f in &list {
                            ui.label(egui::RichText::new(&f.key).monospace().strong());
                            let resp = ui.label(egui::RichText::new(shown(&f.value)).monospace());
                            if f.value.chars().count() > SHOW_MAX || f.value.lines().count() > SHOW_LINES {
                                resp.on_hover_text(&f.value);
                            }
                            ui.menu_button("…", |ui| {
                                if ui.button(tr("Filter: lines with this value")).clicked() {
                                    act = Some(Act::Filter(query_for(f, syntax), false));
                                    ui.close();
                                }
                                if ui.button(tr("Filter: lines without it")).clicked() {
                                    act = Some(Act::Filter(query_for(f, syntax), true));
                                    ui.close();
                                }
                                if ui.button(tr("Highlight this value")).clicked() {
                                    act = Some(Act::Highlight(query_for(f, syntax)));
                                    ui.close();
                                }
                                if ui.button(tr("Count the values of this field…")).clicked() {
                                    act = Some(Act::Count(f.key.clone(), syntax));
                                    ui.close();
                                }
                                if ui.button(tr("Copy value")).clicked() {
                                    ui.ctx().copy_text(f.value.clone());
                                    ui.close();
                                }
                            });
                            ui.end_row();
                        }
                    });
                });
            }
        }
        match act {
            Some(Act::Filter(q, invert)) => {
                self.search.query = q;
                self.search.invert = invert;
                self.start_filter();
            }
            Some(Act::Highlight(q)) => self.add_mark(q),
            Some(Act::Count(key, syntax)) => self.count_field_values(key, syntax),
            None => {}
        }
    }

    /// Counts the values of `key` over the whole document in the background and opens them as a
    /// table, most frequent first.
    pub(crate) fn count_field_values(&mut self, key: String, syntax: Syntax) {
        let Some(d) = self.docs.get(self.active) else { return };
        let snap = d.buf.snapshot();
        let doc = d.title.clone();
        let safe: String = key.chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect();
        let target = bigtext::decompress::temp_file(&format!("{safe}-values.csv"));
        let title = trf("{key} values ({name})", &[("key", &key), ("name", &doc)]);
        self.spawn_task(tr("Counting values").into(), move |progress: &Progress, cancel: &CancelToken| {
            let r = (|| {
                let Some(c) = fields::value_counts(&snap, &key, syntax, cancel, Some(progress))? else { return Ok(None) };
                if let Some(dir) = target.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                let file = std::fs::File::options().write(true).create_new(true).open(&target)?;
                let mut w = std::io::BufWriter::new(file);
                fields::counts_to_csv(&c, &key, &mut w)?;
                std::io::Write::flush(&mut w)?;
                Ok::<_, std::io::Error>(Some(c))
            })();
            match r {
                Ok(Some(c)) if c.lines == 0 => {
                    bigtext::decompress::remove_temp(&target);
                    TaskOutcome::Failed(trf("No line has the field “{key}”.", &[("key", &key)]))
                }
                Ok(Some(c)) => {
                    let mut message = trf("{n} values in {lines} lines; {without} lines without “{key}”.", &[("n", &fmt_count(c.values.len() as u64)), ("lines", &fmt_count(c.lines)), ("without", &fmt_count(c.without)), ("key", &key)]);
                    if c.other > 0 {
                        message.push(' ');
                        message.push_str(&trf("Only the first {max} different values are listed; {n} lines have others.", &[("max", &fmt_count(fields::VALUES_MAX as u64)), ("n", &fmt_count(c.other))]));
                    }
                    TaskOutcome::TempTable(Box::new(super::tasks::TempTable { path: target, title, message }))
                }
                Ok(None) => {
                    bigtext::decompress::remove_temp(&target);
                    TaskOutcome::Cancelled
                }
                Err(e) => {
                    bigtext::decompress::remove_temp(&target);
                    TaskOutcome::Failed(e.to_string())
                }
            }
        });
    }
}

/// A value cut to what a row shows.
fn shown(v: &str) -> String {
    let mut lines = v.lines();
    let mut out: String = lines.by_ref().take(SHOW_LINES).collect::<Vec<_>>().join("\n");
    let mut cut = lines.next().is_some();
    if out.chars().count() > SHOW_MAX {
        out = out.chars().take(SHOW_MAX).collect();
        cut = true;
    }
    if cut {
        out.push('…');
    }
    out
}

/// A regular expression for the lines where this field has this value.
pub(crate) fn query_for(f: &Field, syntax: Syntax) -> Query {
    let raw = regex_syntax::escape(&f.raw);
    let pattern = match syntax {
        Syntax::Json => {
            let leaf = f.key.rsplit('.').next().unwrap_or(&f.key);
            format!(r#""{}"\s*:\s*{raw}\s*[,}}\]]"#, regex_syntax::escape(leaf))
        }
        Syntax::Logfmt => format!(r"(?:^|\s){}={raw}(?:\s|$)", regex_syntax::escape(&f.key)),
    };
    Query { pattern, regex: true, case_sensitive: true, whole_word: false, dot_all: false }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bigtext::encoding::Codec;
    use bigtext::search::Matcher;

    fn hits(q: &Query, line: &str) -> bool {
        let m = Matcher::new(q, Codec::utf8()).unwrap();
        let mut found = false;
        m.find_in(line.as_bytes(), 0, 0, line.len(), line.len(), &mut None, &mut |_, _| {
            found = true;
            false
        });
        found
    }

    #[test]
    fn filter_patterns_match_the_value_only() {
        let (s, f) = fields::line_fields(r#"{"status": 42, "msg": "a.b (c)", "u": {"id": 7}}"#).unwrap();
        let q = query_for(&f[0], s);
        assert!(hits(&q, r#"{"status":42}"#) && hits(&q, r#"{"status": 42, "x": 1}"#));
        assert!(!hits(&q, r#"{"status": 420}"#));
        assert!(hits(&query_for(&f[1], s), r#"{"msg": "a.b (c)"}"#) && !hits(&query_for(&f[1], s), r#"{"msg": "aXb (c)"}"#));
        assert!(hits(&query_for(&f[2], s), r#"{"u": {"id": 7}}"#));
        let (s, f) = fields::line_fields("level=info msg=\"x y\"").unwrap();
        assert!(hits(&query_for(&f[0], s), "a=1 level=info b=2") && !hits(&query_for(&f[0], s), "level=infos"));
        assert!(hits(&query_for(&f[1], s), "msg=\"x y\""));
    }

    #[test]
    fn long_values_are_cut() {
        assert_eq!(shown("a\nb"), "a\nb");
        assert_eq!(shown(&"x\n".repeat(10)).lines().count(), SHOW_LINES);
        assert!(shown(&"y".repeat(400)).ends_with('…'));
    }
}
