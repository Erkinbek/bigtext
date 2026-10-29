//! Table view column tools: sorting rows, moving and renaming columns, column statistics,
//! filtering by a column and exporting columns. Rewrites follow the column-delete rule:
//! documents up to `IN_MEMORY_MAX` change in memory as one undo step, bigger ones are
//! rewritten on disk (temp file, atomic swap, optional backup).

use bigtext::csv::{self, ColumnStats, Cond, CondOp, Dialect, Groups, Rewrite, ScanEnd};
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
    pub group: Option<GroupDialog>,
    /// Finished groups: document title, groups.
    pub groups: Option<(String, Groups)>,
    pub conds: Option<CondDialog>,
}

/// Group rows by `col`, optionally summing `value`.
pub struct GroupDialog {
    pub col: usize,
    pub value: Option<usize>,
    pub names: Vec<String>,
}

/// Several conditions on columns.
pub struct CondDialog {
    pub conds: Vec<Cond>,
    pub names: Vec<String>,
}

/// Groups shown in the results window.
const GROUPS_SHOWN: usize = 500;

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
            TableAction::ExportAs { cols, format } => self.export_table(cols, format),
            TableAction::GroupBy { col } => {
                let names = self.column_names();
                self.csv.group = Some(GroupDialog { col, value: None, names });
            }
            TableAction::CheckRows => self.check_rows(),
            TableAction::Conditions { col } => {
                let names = self.column_names();
                self.csv.conds = Some(CondDialog { conds: vec![Cond { col, op: CondOp::Contains, value: String::new() }], names });
            }
        }
    }

    /// The column names of the active table (from its header, or "Column N").
    fn column_names(&self) -> Vec<String> {
        let Some(d) = self.docs.get(self.active) else { return Vec::new() };
        let Some(dialect) = d.table_state.dialect else { return Vec::new() };
        let start = d.min_caret();
        let Some(b) = d.buf.read(start..(start + 64 * 1024).min(d.buf.len())) else { return Vec::new() };
        let codec = d.buf.codec;
        let end = codec.find_nl(&b, start).unwrap_or(b.len());
        let line = &b[..end];
        let line = line.strip_suffix(codec.cr_unit()).unwrap_or(line);
        let mut fields = Vec::new();
        csv::split_fields(&codec, dialect, line, &mut fields);
        fields
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let v = csv::field_value(&codec, dialect, &line[r.clone()]);
                if d.table_state.header && !v.trim().is_empty() { v } else { trf("Column {n}", &[("n", &(i + 1).to_string())]) }
            })
            .collect()
    }

    /// Counts the rows per value of a column (and sums another) in the background.
    pub(crate) fn start_group_by(&mut self, col: usize, value: Option<usize>) {
        let Some(d) = self.docs.get(self.active) else { return };
        let Some(dialect) = d.table_state.dialect else { return };
        let (snap, header, title) = (d.buf.snapshot(), d.table_state.header, d.title.clone());
        self.spawn_task(tr("Grouping rows").into(), move |progress, cancel| match csv::group_by(&snap, dialect, col, value, header, cancel, Some(progress)) {
            Ok(g) => TaskOutcome::Groups(Box::new((title, g))),
            Err(ScanEnd::Cancelled) => TaskOutcome::Cancelled,
            Err(_) => TaskOutcome::Failed(tr("Could not read the document.").into()),
        });
    }

    /// Writes the rows meeting every condition to a temporary CSV and opens it as a table.
    pub(crate) fn start_conditions(&mut self, conds: Vec<Cond>) {
        let Some(d) = self.docs.get(self.active) else { return };
        let Some(dialect) = d.table_state.dialect else { return };
        let (snap, header) = (d.buf.snapshot(), d.table_state.header);
        let stem = std::path::Path::new(&d.title).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "rows".into());
        let target = bigtext::decompress::temp_file(&format!("{stem}.filtered.csv"));
        let title = trf("{name} (filtered)", &[("name", &d.title)]);
        self.spawn_task(tr("Filtering rows").into(), move |progress, cancel| {
            let r = (|| {
                if let Some(dir) = target.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                let file = std::fs::File::options().write(true).create_new(true).open(&target)?;
                let mut w = std::io::BufWriter::with_capacity(8 << 20, file);
                let r = csv::filter_rows_to(&snap, dialect, &conds, header, &mut w, cancel, Some(progress))?;
                std::io::Write::flush(&mut w)?;
                Ok::<_, std::io::Error>(r)
            })();
            match r {
                Ok(Some((kept, rows))) => {
                    let message = trf("{kept} of {rows} rows meet the conditions.", &[("kept", &fmt_count(kept)), ("rows", &fmt_count(rows))]);
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

    /// Writes the table (or some columns) as JSON, JSON Lines, Markdown or Excel to a file the
    /// user picks.
    pub(crate) fn export_table(&mut self, cols: Option<Vec<usize>>, format: bigtext::table_export::Format) {
        let Some(d) = self.docs.get(self.active) else { return };
        if d.table_state.dialect.is_none() {
            return;
        }
        let stem = std::path::Path::new(&d.title).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| d.title.clone());
        let Some(target) = self.pick_output_file(tr("Export table"), &format!("{stem}.{}", format.ext())) else { return };
        self.export_table_to(cols, format, target);
    }

    /// Writes the table (or some columns) in `format` to `target` in the background.
    pub(crate) fn export_table_to(&mut self, cols: Option<Vec<usize>>, format: bigtext::table_export::Format, target: std::path::PathBuf) {
        let Some(d) = self.docs.get(self.active) else { return };
        let Some(dialect) = d.table_state.dialect else { return };
        let (snap, header) = (d.buf.snapshot(), d.table_state.header);
        self.spawn_task(tr("Exporting the table").into(), move |progress, cancel| {
            tasks::write_via_temp(&target, |w| {
                let spec = bigtext::table_export::Spec { dialect, cols: cols.as_deref(), header, format };
                Ok(bigtext::table_export::export(&snap, spec, w, cancel, Some(progress))?.map(|st| {
                    let mut m = trf("Exported {n} rows to {file}.", &[("n", &fmt_count(st.rows)), ("file", &super::filter_panel::file_name(&target))]);
                    if st.truncated {
                        m.push(' ');
                        m.push_str(tr("Rows or columns past Excel's limits were left out."));
                    }
                    m
                }))
            })
        });
    }

    /// Saves the groups shown to a CSV file the user picks.
    fn export_groups(&mut self) {
        let Some((title, g)) = self.csv.groups.clone() else { return };
        let stem = std::path::Path::new(&title).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let Some(target) = self.pick_output_file(tr("Export groups"), &format!("{stem}.groups.csv")) else { return };
        self.spawn_task(tr("Exporting groups").into(), move |_, _| {
            tasks::write_via_temp(&target, |w| {
                csv::groups_to_csv(&g, w)?;
                Ok(Some(trf("Saved {n} groups.", &[("n", &fmt_count(g.groups.len() as u64))])))
            })
        });
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
        self.group_windows(ctx);
        self.conditions_window(ctx);
    }

    fn group_windows(&mut self, ctx: &egui::Context) {
        if let Some(g) = &mut self.csv.group {
            let mut open = true;
            let mut go = false;
            let name = g.names.get(g.col).cloned().unwrap_or_default();
            egui::Window::new(trf("Group rows by “{name}”", &[("name", &name)])).collapsible(false).resizable(false).open(&mut open).show(ctx, |ui| {
                ui.label(tr("Counts the rows of each value of the column, like GROUP BY."));
                ui.horizontal(|ui| {
                    ui.label(tr("Also sum"));
                    let shown = g.value.and_then(|c| g.names.get(c)).cloned().unwrap_or_else(|| tr("(nothing)").into());
                    egui::ComboBox::from_id_salt("group-sum").selected_text(shown).show_ui(ui, |ui| {
                        ui.selectable_value(&mut g.value, None, tr("(nothing)"));
                        for (i, n) in g.names.iter().enumerate() {
                            ui.selectable_value(&mut g.value, Some(i), n);
                        }
                    });
                });
                go = ui.button(tr("Group")).clicked();
            });
            if go {
                let (col, value) = (g.col, g.value);
                self.csv.group = None;
                self.start_group_by(col, value);
            } else if !open {
                self.csv.group = None;
            }
        }
        if let Some((title, g)) = &self.csv.groups {
            let mut open = true;
            let mut export = false;
            egui::Window::new(trf("Groups of “{name}”", &[("name", &g.key_name)])).open(&mut open).default_width(480.0).default_height(420.0).show(ctx, |ui| {
                ui.weak(title.as_str());
                ui.label(trf("{rows} rows in {n} groups", &[("rows", &fmt_count(g.rows)), ("n", &fmt_count(g.groups.len() as u64))]));
                if g.capped {
                    ui.colored_label(ui.visuals().warn_fg_color, trf("Only the first {n} values are counted.", &[("n", &fmt_count(csv::GROUPS_MAX as u64))]));
                }
                if g.groups.len() > GROUPS_SHOWN {
                    ui.weak(trf("The largest {n} are shown; Export saves all.", &[("n", &GROUPS_SHOWN.to_string())]));
                }
                export = ui.button(tr("Export…")).clicked();
                ui.separator();
                let num = |x: f64| if x.fract() == 0.0 && x.abs() < 1e15 { format!("{x:.0}") } else { format!("{x:.4}").trim_end_matches('0').trim_end_matches('.').to_string() };
                egui::ScrollArea::vertical().show(ui, |ui| {
                    egui::Grid::new("groups").num_columns(if g.value_name.is_some() { 4 } else { 2 }).striped(true).show(ui, |ui| {
                        ui.strong(&g.key_name);
                        ui.strong(tr("Rows"));
                        if let Some(v) = &g.value_name {
                            ui.strong(trf("Sum of {name}", &[("name", v)]));
                            ui.strong(tr("Mean"));
                        }
                        ui.end_row();
                        for x in g.groups.iter().take(GROUPS_SHOWN) {
                            let key: String = x.key.chars().take(60).collect();
                            ui.monospace(if key.is_empty() { tr("(empty)").to_string() } else { key });
                            ui.monospace(fmt_count(x.rows));
                            if g.value_name.is_some() {
                                ui.monospace(if x.numeric > 0 { num(x.sum) } else { String::new() });
                                ui.monospace(x.mean().map(num).unwrap_or_default());
                            }
                            ui.end_row();
                        }
                    });
                });
            });
            if export {
                self.export_groups();
            }
            if !open {
                self.csv.groups = None;
            }
        }
    }

    fn conditions_window(&mut self, ctx: &egui::Context) {
        let Some(c) = &mut self.csv.conds else { return };
        let mut open = true;
        let mut go = false;
        let mut remove = None;
        egui::Window::new(tr("Filter rows by conditions")).collapsible(false).open(&mut open).default_width(520.0).show(ctx, |ui| {
            ui.label(tr("Keeps the rows that meet every condition and opens them as a new table."));
            egui::Grid::new("conds").num_columns(4).show(ui, |ui| {
                for (i, cond) in c.conds.iter_mut().enumerate() {
                    let shown = c.names.get(cond.col).cloned().unwrap_or_default();
                    egui::ComboBox::from_id_salt(("cond-col", i)).selected_text(shown).width(140.0).show_ui(ui, |ui| {
                        for (k, n) in c.names.iter().enumerate() {
                            ui.selectable_value(&mut cond.col, k, n);
                        }
                    });
                    egui::ComboBox::from_id_salt(("cond-op", i)).selected_text(tr(cond.op.name())).width(120.0).show_ui(ui, |ui| {
                        for op in CondOp::ALL {
                            ui.selectable_value(&mut cond.op, op, tr(op.name()));
                        }
                    });
                    ui.add_enabled(cond.op.takes_value(), egui::TextEdit::singleline(&mut cond.value).desired_width(140.0));
                    if ui.small_button("×").clicked() {
                        remove = Some(i);
                    }
                    ui.end_row();
                }
            });
            if ui.button(tr("+ Condition")).clicked() {
                let col = c.conds.last().map(|x| x.col).unwrap_or(0);
                c.conds.push(Cond { col, op: CondOp::Contains, value: String::new() });
            }
            ui.separator();
            go = ui.add_enabled(!c.conds.is_empty(), egui::Button::new(tr("Show matching rows"))).clicked();
        });
        if let Some(i) = remove {
            c.conds.remove(i);
        }
        if go {
            let conds = c.conds.clone();
            self.csv.conds = None;
            self.start_conditions(conds);
        } else if !open {
            self.csv.conds = None;
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
