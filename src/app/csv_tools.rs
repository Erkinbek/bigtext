//! Table view column tools: sorting rows, moving and renaming columns, column statistics,
//! filtering by a column and exporting columns. Rewrites follow the column-delete rule:
//! documents up to `IN_MEMORY_MAX` change in memory as one undo step, bigger ones are
//! rewritten on disk (temp file, atomic swap, optional backup).

use bigtext::csv::{self, ColumnStats, Dialect, Rewrite, ScanEnd};
use bigtext::jobs::{self, human_bytes, CancelToken, Progress};
use bigtext::text::Piece;

use super::columns::IN_MEMORY_MAX;
use super::document::{Banner, EditKind};
use super::filter_panel::fmt_count;
use super::i18n::{tr, trf};
use super::ops::{SaveJob, WriteSpec};
use super::tasks::{self, TaskOutcome};
use super::{BigTextApp, Confirm, ConfirmAction, Msg};
use crate::view::table_view::TableAction;

/// Sorting keeps every row's key in memory, so it is limited to files of this size.
pub const SORT_MAX: u64 = 1024 * 1024 * 1024;

pub struct CsvRewritten {
    pub doc: u64,
    pub version: u64,
    pub add_len: u64,
    pub pieces: Vec<Piece>,
    pub add: Vec<u8>,
    pub rewrite: Rewrite,
    pub records: u64,
}

pub struct RenameDialog {
    col: usize,
    name: String,
    focus: bool,
}

#[derive(Default)]
pub struct CsvUi {
    pub stats: Option<(String, ColumnStats)>,
    pub rename: Option<RenameDialog>,
}

/// A field value written so it reads back as `v`: quoted when it holds the delimiter, a
/// quote, a line break or edge spaces.
fn quote_field(v: &str, d: Dialect) -> String {
    let q = d.quote as char;
    if v.contains([d.delim as char, q, '\n', '\r']) || v.trim() != v {
        format!("{q}{}{q}", v.replace(q, &format!("{q}{q}")))
    } else {
        v.to_string()
    }
}

fn describe(rw: &Rewrite) -> String {
    match rw {
        Rewrite::Sort { col, desc, .. } => trf(if *desc { "Sorted by column {n} (descending)" } else { "Sorted by column {n}" }, &[("n", &(col + 1).to_string())]),
        Rewrite::Swap(a, b) => trf("Moved column {a} to {b}", &[("a", &(a + 1).to_string()), ("b", &(b + 1).to_string())]),
        Rewrite::Select(_) => tr("Columns exported").into(),
    }
}

impl BigTextApp {
    pub(crate) fn on_table_action(&mut self, a: TableAction) {
        match a {
            TableAction::DeleteColumns { cols, names } => self.ask_delete_columns(cols, names),
            TableAction::Sort { col, desc } => {
                let header = self.docs.get(self.active).is_some_and(|d| d.table_state.header);
                self.ask_rewrite(Rewrite::Sort { col, desc, header });
            }
            TableAction::Move { col, right } => {
                let other = if right { col + 1 } else { col.saturating_sub(1) };
                if other != col {
                    self.ask_rewrite(Rewrite::Swap(col, other));
                }
            }
            TableAction::Stats { col } => self.start_column_stats(col),
            TableAction::Filter { col, name } => {
                self.search.filter_column = Some(col);
                self.open_search(false);
                self.search.status = trf("Filter looks in column “{name}” only", &[("name", &name)]);
            }
            TableAction::Rename { col, name } => self.csv.rename = Some(RenameDialog { col, name, focus: true }),
            TableAction::Export { cols } => self.export_columns(cols),
        }
    }

    /// Runs a rewrite in memory, or asks first when it has to go to disk (no undo).
    fn ask_rewrite(&mut self, rw: Rewrite) {
        let Some(d) = self.docs.get(self.active) else { return };
        let Some(dialect) = d.table_state.dialect else { return };
        if !d.editable() {
            return;
        }
        let len = d.buf.len();
        if matches!(rw, Rewrite::Sort { .. }) && len > SORT_MAX {
            self.error = Some(trf("Sorting works on files up to {size}; this one is {len}.", &[("size", &human_bytes(SORT_MAX)), ("len", &human_bytes(len))]));
            return;
        }
        let on_disk = len > IN_MEMORY_MAX;
        if !on_disk {
            self.rewrite_csv(d.id, dialect, rw, false, false);
            return;
        }
        if d.path.is_none() {
            self.error = Some(tr("Save the document to a file first; large tables are changed by rewriting the file.").into());
            return;
        }
        let message = trf(
            "{what}?\n\n“{file}” is {size}, so the result is written to a new file that atomically replaces it. Undo will not be available.",
            &[("what", &describe(&rw)), ("file", &d.title), ("size", &human_bytes(len))],
        );
        self.confirm = Some(Confirm {
            title: tr("Rewrite the table").into(),
            message,
            action: ConfirmAction::CsvRewrite { doc: d.id, dialect, rewrite: rw, on_disk },
            checkbox: Some((tr("Keep a backup (.bak)").into(), true)),
        });
    }

    pub(super) fn rewrite_csv(&mut self, doc: u64, dialect: Dialect, rw: Rewrite, on_disk: bool, backup: bool) {
        let Some(d) = self.docs.iter_mut().find(|d| d.id == doc) else { return };
        if !d.editable() {
            return;
        }
        if on_disk {
            let Some(path) = d.path.clone() else { return };
            self.start_write(doc, path, WriteSpec::Csv { dialect, rewrite: rw }, backup);
            return;
        }
        let cancel = CancelToken::new();
        let progress = Progress::new(d.buf.len());
        d.save_job = Some(SaveJob { cancel: cancel.clone(), progress: progress.clone(), label: describe(&rw) });
        let (snap, version) = (d.buf.snapshot(), d.version);
        let (tx, waker) = (self.tx.clone(), self.waker.clone());
        jobs::spawn("csv-rewrite", move || {
            let mut out = Vec::with_capacity(snap.len() as usize);
            let msg = match csv::rewrite_to(&snap, dialect, &rw, &mut out, &cancel, Some(&progress)) {
                Ok(Some(stats)) => {
                    let (pieces, add) = snap.build_batch(&[(0..snap.len(), out)]);
                    Msg::CsvRewritten(Box::new(CsvRewritten { doc, version, add_len: snap.add_len(), pieces, add, rewrite: rw, records: stats.count }))
                }
                Ok(None) => Msg::JobFailed { doc, msg: None },
                Err(e) => Msg::JobFailed { doc, msg: Some(e.to_string()) },
            };
            let _ = tx.send(msg);
            waker();
        });
    }

    pub(super) fn on_csv_rewritten(&mut self, c: CsvRewritten) {
        let Some(d) = self.docs.iter_mut().find(|d| d.id == c.doc) else { return };
        d.save_job = None;
        if d.version != c.version || d.buf.add_len() != c.add_len {
            d.banner = Some(Banner::Info(tr("The document changed meanwhile; nothing was changed.").into()));
            return;
        }
        let (top_line, _) = d.buf.line_of(d.view.top);
        let swap = d.buf.apply_batch(c.pieces, &c.add);
        let top = d.buf.line_start(top_line).map(|(o, _)| o).unwrap_or(0).max(d.min_caret());
        let caret = d.min_caret();
        d.record(swap, (caret, caret));
        d.view.top = top;
        d.view.scroll_to_caret = false;
        if let Rewrite::Swap(a, b) = c.rewrite {
            d.table_state.columns_swapped(a, b);
        }
        d.banner = Some(Banner::Info(trf("{what}: {n} lines. Undo with Ctrl+Z.", &[("what", &describe(&c.rewrite)), ("n", &fmt_count(c.records))])));
    }

    fn start_column_stats(&mut self, col: usize) {
        let Some(d) = self.docs.get(self.active) else { return };
        let Some(dialect) = d.table_state.dialect else { return };
        let (snap, header, title) = (d.buf.snapshot(), d.table_state.header, d.title.clone());
        self.spawn_task(tr("Column statistics").into(), move |progress, cancel| match csv::column_stats(&snap, dialect, col, header, cancel, Some(progress)) {
            Ok(s) => TaskOutcome::Stats(Box::new((title, s))),
            Err(ScanEnd::Cancelled) => TaskOutcome::Cancelled,
            Err(_) => TaskOutcome::Failed(tr("Could not read the document.").into()),
        });
    }

    fn export_columns(&mut self, cols: Vec<usize>) {
        let Some(d) = self.docs.get(self.active) else { return };
        let Some(dialect) = d.table_state.dialect else { return };
        let snap = d.buf.snapshot();
        let stem = std::path::Path::new(&d.title).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| d.title.clone());
        let Some(target) = self.pick_output_file(tr("Export columns"), &format!("{stem}.columns.csv")) else { return };
        self.spawn_task(tr("Exporting columns").into(), move |progress, cancel| {
            tasks::write_via_temp(&target, |w| {
                Ok(csv::rewrite_to(&snap, dialect, &Rewrite::Select(cols), w, cancel, Some(progress))?
                    .map(|s| trf("Exported {n} lines to {file}.", &[("n", &fmt_count(s.count)), ("file", &super::filter_panel::file_name(&target))])))
            })
        });
    }

    fn rename_column(&mut self, col: usize, name: &str) {
        let Some(d) = self.docs.get_mut(self.active) else { return };
        let Some(dialect) = d.table_state.dialect else { return };
        if !d.editable() {
            return;
        }
        let snap = d.buf.snapshot();
        let mut field = None;
        csv::for_each_record(&snap, dialect, &CancelToken::new(), None, |r| {
            field = r.fields.get(col).cloned();
            false
        });
        let Some(field) = field else { return };
        let bytes = d.buf.codec.encode(&quote_field(name, dialect));
        d.edit(field, &bytes, EditKind::Other);
        d.table_state.keep_layout();
    }

    /// Self-test: confirms the open rename dialog with `name`.
    pub(crate) fn test_rename(&mut self, name: &str) {
        if let Some(r) = self.csv.rename.take() {
            self.rename_column(r.col, name);
        }
    }

    pub(super) fn csv_windows(&mut self, ctx: &egui::Context) {
        if let Some(r) = &mut self.csv.rename {
            let mut open = true;
            let mut ok = false;
            egui::Window::new(tr("Rename column")).collapsible(false).resizable(false).open(&mut open).show(ctx, |ui| {
                let e = ui.text_edit_singleline(&mut r.name);
                if r.focus {
                    e.request_focus();
                    r.focus = false;
                }
                ok = e.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.weak(tr("Changes the header cell; Ctrl+Z undoes it."));
                ok |= ui.button(tr("Rename")).clicked();
            });
            if ok {
                let (col, name) = (r.col, r.name.clone());
                self.csv.rename = None;
                self.rename_column(col, &name);
            } else if !open || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.csv.rename = None;
            }
        }
        if let Some((title, s)) = &self.csv.stats {
            let mut open = true;
            egui::Window::new(trf("Column “{name}”", &[("name", &s.name)])).open(&mut open).default_width(360.0).show(ctx, |ui| {
                ui.weak(format!("{title} · {}", trf("column {n}", &[("n", &(s.col + 1).to_string())])));
                let num = |x: f64| if x.fract() == 0.0 && x.abs() < 1e15 { format!("{x:.0}") } else { format!("{x:.6}").trim_end_matches('0').trim_end_matches('.').to_string() };
                egui::Grid::new("stats").num_columns(2).striped(true).show(ui, |ui| {
                    let mut row = |k: &str, v: String| {
                        ui.label(k);
                        ui.monospace(v);
                        ui.end_row();
                    };
                    row(tr("Rows"), fmt_count(s.rows));
                    row(tr("Empty"), fmt_count(s.empty));
                    row(tr("Distinct values"), format!("{}{}", if s.distinct_capped { "≥ " } else { "" }, fmt_count(s.distinct as u64)));
                    row(tr("Longest value"), trf("{n} characters", &[("n", &s.longest.to_string())]));
                    row(tr("Numbers"), fmt_count(s.numeric));
                    if s.numeric > 0 {
                        row(tr("Sum"), num(s.sum));
                        row(tr("Mean"), num(s.mean().unwrap_or(0.0)));
                        row(tr("Minimum"), num(s.min.unwrap_or(0.0)));
                        row(tr("Maximum"), num(s.max.unwrap_or(0.0)));
                    }
                });
                if !s.top.is_empty() {
                    ui.separator();
                    ui.strong(if s.distinct_capped { tr("Most frequent (approximate)") } else { tr("Most frequent") });
                    egui::Grid::new("top").num_columns(2).striped(true).show(ui, |ui| {
                        for (v, n) in &s.top {
                            let shown: String = v.chars().take(60).collect();
                            ui.monospace(shown);
                            ui.monospace(fmt_count(*n));
                            ui.end_row();
                        }
                    });
                }
            });
            if !open {
                self.csv.stats = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        let d = Dialect::new(b',');
        assert_eq!(quote_field("name", d), "name");
        assert_eq!(quote_field("a,b", d), "\"a,b\"");
        assert_eq!(quote_field("say \"hi\"", d), "\"say \"\"hi\"\"\"");
        assert_eq!(quote_field(" x", d), "\" x\"");
    }
}
