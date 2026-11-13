//! Add column… (a column header's menu): a new last column computed from the row by an
//! expression, previewed on the rows on screen, then written into every record (one undo step
//! up to 64 MB, a rewrite of the file above).

use bigtext::csv::{self, Rewrite};
use bigtext::csv_expr::Expr;

use super::i18n::tr;
use super::BigTextApp;

/// Rows previewed.
const PREVIEW_ROWS: usize = 10;

pub struct DeriveDialog {
    pub name: String,
    pub expr: String,
    names: Vec<String>,
    /// The expression compiled for `expr`.
    compiled: Option<(String, Result<Expr, String>)>,
    focus: bool,
}

impl BigTextApp {
    pub(crate) fn open_derive(&mut self) {
        let names = self.column_names_of(self.active);
        let expr = names
            .first()
            .map(|n| format!("{{{n}}}"))
            .unwrap_or_default();
        self.derive = Some(DeriveDialog {
            name: tr("new column").into(),
            expr,
            names,
            compiled: None,
            focus: true,
        });
    }

    /// The fields of up to [`PREVIEW_ROWS`] data rows from the top of the view.
    fn preview_rows(&self) -> Vec<Vec<String>> {
        let Some(d) = self.docs.get(self.active) else {
            return Vec::new();
        };
        let Some(dialect) = d.table_state.dialect else {
            return Vec::new();
        };
        let codec = d.buf.codec;
        let len = d.buf.len();
        let start = dialect.first(&d.buf).max(d.min_caret());
        let mut from = d.view.top.max(start);
        if d.table_state.header && from == start {
            // After the header record, which may span lines.
            let Some(b) = d.buf.read(start..(start + 64 * 1024).min(len)) else {
                return Vec::new();
            };
            from = csv::records_in(&codec, dialect, &b, true, 1)
                .first()
                .map_or(start, |r| start + r.2 as u64);
        }
        let Some(b) = d.buf.read(from..(from + 64 * 1024).min(len)) else {
            return Vec::new();
        };
        let mut fields = Vec::new();
        csv::records_in(&codec, dialect, &b, true, PREVIEW_ROWS)
            .into_iter()
            .map(|(_, rec, _)| {
                csv::split_fields(&codec, dialect, rec, &mut fields);
                fields
                    .iter()
                    .map(|f| csv::field_value(&codec, dialect, &rec[f.clone()]))
                    .collect()
            })
            .collect()
    }

    pub(super) fn derive_window(&mut self, ctx: &egui::Context) {
        if self.derive.is_none() {
            return;
        }
        let rows = self.preview_rows();
        let Some(dlg) = self.derive.as_mut() else {
            return;
        };
        if dlg.compiled.as_ref().map(|c| &c.0) != Some(&dlg.expr) {
            dlg.compiled = Some((dlg.expr.clone(), Expr::compile(&dlg.expr, &dlg.names)));
        }
        let mut open = true;
        let mut go = false;
        egui::Window::new(tr("Add a column")).open(&mut open).collapsible(false).resizable(true).default_width(460.0).show(ctx, |ui| {
            egui::Grid::new("derive").num_columns(2).show(ui, |ui| {
                ui.label(tr("Name"));
                ui.add(egui::TextEdit::singleline(&mut dlg.name).desired_width(300.0));
                ui.end_row();
                ui.label(tr("Expression"));
                let r = ui.add(egui::TextEdit::singleline(&mut dlg.expr).desired_width(300.0).font(egui::TextStyle::Monospace).hint_text("{price} * {qty}"));
                if dlg.focus {
                    r.request_focus();
                    dlg.focus = false;
                }
                ui.end_row();
            });
            ui.weak(tr("{name} or {3} is a column; + - * / on numbers, & joins text, = <> < > compare, and / or / not; upper, lower, trim, len, substr(t, start, n), replace(t, a, b), regex(t, \"id=([0-9]+)\"), if(c, a, b), num, abs, round(x, digits)."));
            ui.separator();
            match dlg.compiled.as_ref().map(|c| &c.1) {
                Some(Ok(e)) => {
                    ui.label(tr("Preview (rows on screen):"));
                    egui::Grid::new("derive-preview").striped(true).show(ui, |ui| {
                        for row in &rows {
                            ui.label(egui::RichText::new(row.iter().take(3).cloned().collect::<Vec<_>>().join(", ")).weak());
                            ui.label(egui::RichText::new(e.eval(row)).monospace().strong());
                            ui.end_row();
                        }
                    });
                }
                Some(Err(err)) => {
                    ui.colored_label(ui.visuals().error_fg_color, err);
                }
                None => {}
            }
            ui.separator();
            let ok = matches!(dlg.compiled.as_ref().map(|c| &c.1), Some(Ok(_))) && !dlg.name.trim().is_empty();
            go = ui.add_enabled(ok, egui::Button::new(tr("Add the column"))).clicked();
        });
        if go {
            let dlg = self.derive.take().expect("dialog");
            let header = self
                .docs
                .get(self.active)
                .is_some_and(|d| d.table_state.header);
            self.ask_rewrite(Rewrite::Derive {
                name: dlg.name.trim().to_string(),
                expr: dlg.expr,
                names: dlg.names,
                header,
            });
        } else if !open {
            self.derive = None;
        }
    }
}
