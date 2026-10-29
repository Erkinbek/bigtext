//! View → JSON fields…: every key path of a JSON Lines file (or of a JSON array's items) with its
//! types, counts, nulls, number range and examples, like column statistics for nested JSON;
//! copy it as a JSON Schema sketch or save it as CSV.

use bigtext::jobs::{CancelToken, Progress};
use bigtext::json_fields::{self, Fields};

use super::filter_panel::{file_name, fmt_count};
use super::i18n::{tr, trf};
use super::tasks::{self, TaskOutcome};
use super::BigTextApp;

#[derive(Default)]
pub struct JsonFieldsUi {
    /// Document title and the fields.
    pub result: Option<(String, Fields)>,
    pub filter: String,
}

fn show(x: f64) -> String {
    if x.fract() == 0.0 && x.abs() < 1e15 {
        format!("{}", x as i64)
    } else {
        format!("{x:.3}")
    }
}

impl BigTextApp {
    pub(crate) fn start_json_fields(&mut self) {
        let Some(d) = self.docs.get(self.active).filter(|d| d.is_ready()) else { return };
        let (snap, title) = (d.buf.snapshot(), d.title.clone());
        self.spawn_task(tr("Reading the JSON fields").into(), move |progress: &Progress, cancel: &CancelToken| match json_fields::field_stats(&snap, cancel, Some(progress)) {
            Ok(Some(f)) if f.records > 0 => TaskOutcome::JsonFields(Box::new((title, f))),
            Ok(Some(_)) => TaskOutcome::Failed(tr("No JSON objects found: one per line (JSON Lines), or the items of a JSON array.").into()),
            Ok(None) => TaskOutcome::Cancelled,
            Err(e) => TaskOutcome::Failed(e),
        });
    }

    pub(super) fn json_fields_window(&mut self, ctx: &egui::Context) {
        let Some((title, f)) = self.json_fields.result.as_ref() else { return };
        let mut open = true;
        let (mut copy, mut export) = (false, false);
        let filter = &mut self.json_fields.filter;
        egui::Window::new(tr("JSON fields")).open(&mut open).default_size([900.0, 480.0]).resizable(true).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.weak(title.as_str());
                ui.strong(trf("{n} records, {paths} fields", &[("n", &fmt_count(f.records)), ("paths", &fmt_count(f.stats.len() as u64))]));
                if f.skipped > 0 {
                    ui.weak(trf("({n} lines that are not JSON objects left out)", &[("n", &fmt_count(f.skipped))]));
                }
                if f.capped {
                    ui.colored_label(ui.visuals().warn_fg_color, trf("Only the first {n} fields are listed.", &[("n", &fmt_count(json_fields::PATHS_MAX as u64))]));
                }
            });
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(filter).hint_text(tr("Filter the fields")).desired_width(220.0));
                copy = ui.button(tr("Copy as JSON Schema")).clicked();
                export = ui.button(tr("Export CSV…")).clicked();
            });
            ui.separator();
            let rows: Vec<&json_fields::FieldStat> = f.stats.iter().filter(|s| filter.is_empty() || s.path.to_lowercase().contains(&filter.to_lowercase())).collect();
            let widths = [240.0, 140.0, 80.0, 60.0, 80.0, 80.0, 80.0, 60.0];
            let heads = [tr("Field"), tr("Types"), tr("Count"), tr("Nulls"), tr("Min"), tr("Max"), tr("Mean"), tr("Distinct"), tr("Examples")];
            let row_h = 20.0;
            ui.horizontal(|ui| {
                for (i, h) in heads.iter().enumerate() {
                    if let Some(&w) = widths.get(i) {
                        ui.add_sized([w, row_h], egui::Label::new(egui::RichText::new(*h).strong()).truncate());
                    } else {
                        ui.label(egui::RichText::new(*h).strong());
                    }
                }
            });
            egui::ScrollArea::both().auto_shrink([false, false]).show_rows(ui, row_h, rows.len(), |ui, range| {
                for s in &rows[range] {
                    ui.horizontal(|ui| {
                        let distinct = if s.many { format!("{}+", s.distinct.len()) } else { s.distinct.len().to_string() };
                        let cells = [
                            s.path.clone(),
                            s.type_names(),
                            fmt_count(s.count),
                            fmt_count(s.types[5]),
                            s.min.map(show).unwrap_or_default(),
                            s.max.map(show).unwrap_or_default(),
                            s.mean().map(show).unwrap_or_default(),
                            distinct,
                        ];
                        for (cell, w) in cells.iter().zip(widths) {
                            ui.add_sized([w, row_h], egui::Label::new(egui::RichText::new(cell).monospace()).truncate());
                        }
                        ui.label(egui::RichText::new(s.examples.join(" | ")).weak());
                    });
                }
            });
        });
        let schema = copy.then(|| json_fields::fields_to_schema(f));
        let export = export.then(|| (format!("{title}.fields.csv"), f.clone()));
        if let Some(schema) = schema {
            ctx.copy_text(schema);
            self.notify(tr("Copied the JSON Schema sketch.").into(), None);
        }
        if let Some((name, fields)) = export {
            if let Some(target) = self.pick_output_file(tr("Export CSV"), &name) {
                self.spawn_task(tr("Saving").into(), move |_: &Progress, _: &CancelToken| -> TaskOutcome {
                    tasks::write_via_temp(&target, |w| {
                        json_fields::fields_to_csv(&fields, w)?;
                        Ok(Some(trf("Saved {n} fields to {file}.", &[("n", &fmt_count(fields.stats.len() as u64)), ("file", &file_name(&target))])))
                    })
                });
            }
        }
        if !open {
            self.json_fields.result = None;
        }
    }
}
