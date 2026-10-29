//! Pivot by this column… (a column header's menu): rows by one column (or by hour, day or month
//! of a time column), columns by another, and counts or a sum, mean, minimum or maximum in the
//! cells, as a new read-only table.

use bigtext::csv_pivot::{self, Agg, Bucket, PivotSpec};
use bigtext::jobs::{CancelToken, Progress};

use super::filter_panel::fmt_count;
use super::i18n::{tr, trf};
use super::tasks::{self, TaskOutcome};
use super::BigTextApp;

pub struct PivotDialog {
    pub rows: usize,
    pub cols: usize,
    pub value: Option<usize>,
    pub agg: Agg,
    pub bucket: Bucket,
    names: Vec<String>,
}

fn agg_label(a: Agg) -> &'static str {
    match a {
        Agg::Count => "Count rows",
        Agg::Sum => "Sum",
        Agg::Mean => "Mean",
        Agg::Min => "Minimum",
        Agg::Max => "Maximum",
    }
}

fn bucket_label(b: Bucket) -> &'static str {
    match b {
        Bucket::AsIs => "Each value",
        Bucket::Hour => "By hour",
        Bucket::Day => "By day",
        Bucket::Month => "By month",
    }
}

impl BigTextApp {
    pub(crate) fn open_pivot(&mut self, col: usize) {
        let names = self.column_names_of(self.active);
        let cols = if col + 1 < names.len() { col + 1 } else { 0 };
        self.pivot = Some(PivotDialog { rows: col, cols, value: None, agg: Agg::Count, bucket: Bucket::AsIs, names });
    }

    pub(super) fn pivot_window(&mut self, ctx: &egui::Context) {
        let Some(p) = self.pivot.as_mut() else { return };
        let mut open = true;
        let mut go = false;
        egui::Window::new(tr("Pivot table")).open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
            let pick = |ui: &mut egui::Ui, salt: &str, col: &mut usize, names: &[String]| {
                egui::ComboBox::from_id_salt(salt).selected_text(names.get(*col).cloned().unwrap_or_default()).width(180.0).show_ui(ui, |ui| {
                    for (i, n) in names.iter().enumerate() {
                        ui.selectable_value(col, i, n);
                    }
                });
            };
            egui::Grid::new("pivot").num_columns(2).show(ui, |ui| {
                ui.label(tr("Rows"));
                ui.horizontal(|ui| {
                    pick(ui, "pivot-rows", &mut p.rows, &p.names);
                    egui::ComboBox::from_id_salt("pivot-bucket").selected_text(tr(bucket_label(p.bucket))).show_ui(ui, |ui| {
                        for b in Bucket::ALL {
                            ui.selectable_value(&mut p.bucket, b, tr(bucket_label(b)));
                        }
                    });
                });
                ui.end_row();
                ui.label(tr("Columns"));
                pick(ui, "pivot-cols", &mut p.cols, &p.names);
                ui.end_row();
                ui.label(tr("Cells"));
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("pivot-agg").selected_text(tr(agg_label(p.agg))).show_ui(ui, |ui| {
                        for a in Agg::ALL {
                            ui.selectable_value(&mut p.agg, a, tr(agg_label(a)));
                        }
                    });
                    if p.agg != Agg::Count {
                        let mut v = p.value.unwrap_or(0);
                        pick(ui, "pivot-value", &mut v, &p.names);
                        p.value = Some(v);
                    }
                });
                ui.end_row();
            });
            ui.weak(tr("By hour, day or month groups the rows column by its times."));
            go = ui.add_enabled(p.rows != p.cols, egui::Button::new(tr("Make the table"))).clicked();
        });
        if go {
            let p = self.pivot.take().expect("dialog");
            let spec = PivotSpec { rows: p.rows, cols: p.cols, value: if p.agg == Agg::Count { None } else { p.value }, agg: p.agg, bucket: p.bucket, header: true };
            self.start_pivot(spec);
        } else if !open {
            self.pivot = None;
        }
    }

    pub(crate) fn start_pivot(&mut self, spec: PivotSpec) {
        let Some(d) = self.docs.get(self.active) else { return };
        let Some(dialect) = d.table_state.dialect else { return };
        let spec = PivotSpec { header: d.table_state.header, ..spec };
        let (snap, doc) = (d.buf.snapshot(), d.title.clone());
        let target = bigtext::decompress::temp_file("pivot.csv");
        let title = trf("Pivot of {name}", &[("name", &doc)]);
        self.spawn_task(tr("Making the pivot table").into(), move |progress: &Progress, cancel: &CancelToken| {
            let res = (|| {
                let p = match csv_pivot::pivot(&snap, dialect, spec, cancel, Some(progress)) {
                    Ok(p) => p,
                    Err(bigtext::csv::ScanEnd::Cancelled) => return Ok(None),
                    Err(_) => return Err(std::io::Error::other(tr("Could not read the document."))),
                };
                if let Some(dir) = target.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                let mut w = std::io::BufWriter::new(std::fs::File::options().write(true).create_new(true).open(&target)?);
                csv_pivot::pivot_to_csv(&p, spec.agg, &mut w)?;
                std::io::Write::flush(&mut w)?;
                Ok(Some(p))
            })();
            match res {
                Ok(Some(p)) => {
                    let (r, c) = p.size();
                    let mut message = trf("{rows} rows × {cols} columns from {records} records.", &[("rows", &fmt_count(r as u64)), ("cols", &fmt_count(c as u64)), ("records", &fmt_count(p.records))]);
                    if p.left_out > 0 {
                        message.push(' ');
                        message.push_str(&trf("{n} records were left out (more than {cols} columns or {rows} rows).", &[("n", &fmt_count(p.left_out)), ("cols", &fmt_count(csv_pivot::PIVOT_COLS_MAX as u64)), ("rows", &fmt_count(csv_pivot::PIVOT_ROWS_MAX as u64))]));
                    }
                    TaskOutcome::TempTable(Box::new(tasks::TempTable { path: target, title, message }))
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
