//! Column header → Compare table with…: two open tables compared by a key column whatever
//! their row order; the rows changed, removed and added open as a new read-only table.

use bigtext::csv_diff::{self, KeyedSpec};
use bigtext::jobs::{CancelToken, Progress};

use super::filter_panel::fmt_count;
use super::i18n::{tr, trf};
use super::tasks::{self, TaskOutcome};
use super::BigTextApp;

/// The dialog: the two tables (by document id) and their key columns.
pub struct CompareTablesDialog {
    pub a: u64,
    pub key_a: usize,
    pub b: u64,
    pub key_b: usize,
    /// List the rows that are the same too.
    pub all: bool,
}

impl BigTextApp {
    /// Indices of the tabs showing a ready table.
    fn ready_tables(&self) -> Vec<usize> {
        (0..self.docs.len()).filter(|&i| self.docs[i].is_ready() && self.docs[i].table && self.docs[i].table_state.dialect.is_some()).collect()
    }

    /// The other table's column named like `name` (or at `fallback`).
    fn same_column(&self, doc: usize, name: &str, fallback: usize) -> usize {
        let names = self.column_names_of(doc);
        names.iter().position(|n| n.trim().eq_ignore_ascii_case(name.trim())).unwrap_or(fallback.min(names.len().saturating_sub(1)))
    }

    pub(crate) fn open_table_compare(&mut self, col: usize) {
        let tables = self.ready_tables();
        let Some(&b) = tables.iter().find(|&&i| i != self.active) else {
            self.notify(tr("Open the other table too (CSV / TSV in the table view), then compare.").into(), None);
            return;
        };
        let name = self.column_names_of(self.active).get(col).cloned().unwrap_or_default();
        let key_b = self.same_column(b, &name, col);
        self.table_compare = Some(CompareTablesDialog { a: self.docs[self.active].id, key_a: col, b: self.docs[b].id, key_b, all: false });
    }

    pub(super) fn table_compare_window(&mut self, ctx: &egui::Context) {
        let Some((aid, bid)) = self.table_compare.as_ref().map(|c| (c.a, c.b)) else { return };
        let index = |id: u64| self.docs.iter().position(|d| d.id == id);
        let (Some(ai), bi) = (index(aid), index(bid)) else {
            self.table_compare = None;
            return;
        };
        let tables: Vec<(u64, String)> = self.ready_tables().into_iter().filter(|&i| i != ai).map(|i| (self.docs[i].id, self.docs[i].title.clone())).collect();
        let a_names = self.column_names_of(ai);
        let b_names = bi.map(|i| self.column_names_of(i)).unwrap_or_default();
        let a_title = self.docs[ai].title.clone();
        let key_a_name = |c: &CompareTablesDialog| a_names.get(c.key_a).cloned().unwrap_or_default();
        // Picking another table picks its column of the same name.
        let picked: Option<(u64, String)> = {
            let c = self.table_compare.as_ref().expect("dialog");
            Some((c.b, key_a_name(c)))
        };
        let Some(c) = self.table_compare.as_mut() else { return };
        let mut open = true;
        let mut go = false;
        let mut new_b = None;
        egui::Window::new(tr("Compare tables")).open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
            ui.label(tr("Pairs each row with the other table's row with the same key, whatever the order, and lists the rows changed, removed and added."));
            egui::Grid::new("compare-tables").num_columns(3).show(ui, |ui| {
                ui.label(tr("Table"));
                ui.strong(&a_title);
                egui::ComboBox::from_id_salt("cmp-key-a").selected_text(a_names.get(c.key_a).cloned().unwrap_or_default()).width(160.0).show_ui(ui, |ui| {
                    for (i, n) in a_names.iter().enumerate() {
                        ui.selectable_value(&mut c.key_a, i, n);
                    }
                });
                ui.end_row();
                ui.label(tr("Compared with"));
                let shown = tables.iter().find(|t| t.0 == c.b).map(|t| t.1.clone()).unwrap_or_default();
                let before = c.b;
                egui::ComboBox::from_id_salt("cmp-b").selected_text(shown).width(220.0).show_ui(ui, |ui| {
                    for (id, title) in &tables {
                        ui.selectable_value(&mut c.b, *id, title);
                    }
                });
                if c.b != before {
                    new_b = Some(c.b);
                }
                egui::ComboBox::from_id_salt("cmp-key-b").selected_text(b_names.get(c.key_b).cloned().unwrap_or_default()).width(160.0).show_ui(ui, |ui| {
                    for (i, n) in b_names.iter().enumerate() {
                        ui.selectable_value(&mut c.key_b, i, n);
                    }
                });
                ui.end_row();
            });
            ui.checkbox(&mut c.all, tr("List the rows that are the same too"));
            ui.weak(tr("Columns pair up by header name; the columns only one table has are not compared."));
            go = ui.add_enabled(bi.is_some() && c.a != c.b, egui::Button::new(tr("Compare"))).clicked();
        });
        if let (Some(b), Some((_, name))) = (new_b, picked) {
            if let Some(i) = index(b) {
                let key_b = self.same_column(i, &name, 0);
                if let Some(c) = self.table_compare.as_mut() {
                    c.key_b = key_b;
                }
            }
        }
        if go {
            let c = self.table_compare.take().expect("dialog");
            self.start_table_compare(c);
        } else if !open {
            self.table_compare = None;
        }
    }

    pub(crate) fn start_table_compare(&mut self, c: CompareTablesDialog) {
        let index = |id: u64| self.docs.iter().position(|d| d.id == id);
        let (Some(ai), Some(bi)) = (index(c.a), index(c.b)) else { return };
        let (a, b) = (&self.docs[ai], &self.docs[bi]);
        let (Some(ad), Some(bd)) = (a.table_state.dialect, b.table_state.dialect) else { return };
        let spec = KeyedSpec { key_a: c.key_a, key_b: c.key_b, header: a.table_state.header && b.table_state.header, all: c.all };
        let (asnap, bsnap) = (a.buf.snapshot(), b.buf.snapshot());
        let title = format!("{} vs {}", a.title, b.title);
        let target = bigtext::decompress::temp_file("compared.csv");
        self.spawn_task(tr("Comparing tables").into(), move |progress: &Progress, cancel: &CancelToken| {
            let res = (|| {
                if let Some(dir) = target.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                let mut w = std::io::BufWriter::with_capacity(8 << 20, std::fs::File::options().write(true).create_new(true).open(&target)?);
                let st = csv_diff::keyed_diff_to((&asnap, ad), (&bsnap, bd), spec, &mut w, cancel, Some(progress))?;
                std::io::Write::flush(&mut w)?;
                Ok::<_, std::io::Error>(st)
            })();
            match res {
                Ok(Some(st)) => {
                    let message = trf(
                        "{changed} rows changed, {removed} removed, {added} added; {same} the same.",
                        &[("changed", &fmt_count(st.changed)), ("removed", &fmt_count(st.removed)), ("added", &fmt_count(st.added)), ("same", &fmt_count(st.same))],
                    );
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
