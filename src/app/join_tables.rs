//! File → Join tables…: the rows of one open table with the other columns of the rows of another
//! that have the same key, as a new read-only table.

use bigtext::csv_join::{self, JoinSpec};
use bigtext::jobs::{CancelToken, Progress};

use super::filter_panel::fmt_count;
use super::i18n::{tr, trf};
use super::tasks::{self, TaskOutcome};
use super::BigTextApp;

/// The join dialog: tabs (by document id) and key columns.
pub struct JoinDialog {
    pub left: u64,
    pub right: u64,
    pub left_key: usize,
    pub right_key: usize,
    pub inner: bool,
}

impl BigTextApp {
    /// Indices of the tabs showing a ready table.
    fn table_tabs(&self) -> Vec<usize> {
        (0..self.docs.len()).filter(|&i| self.docs[i].is_ready() && self.docs[i].table && self.docs[i].table_state.dialect.is_some()).collect()
    }

    pub(crate) fn open_join(&mut self) {
        let tabs = self.table_tabs();
        if tabs.len() < 2 {
            self.notify(tr("Open two tables (CSV / TSV in the table view) to join them.").into(), None);
            return;
        }
        let left = if tabs.contains(&self.active) { self.active } else { tabs[0] };
        let right = tabs.iter().copied().find(|&i| i != left).unwrap_or(tabs[1]);
        self.join = Some(JoinDialog { left: self.docs[left].id, right: self.docs[right].id, left_key: 0, right_key: 0, inner: false });
    }

    pub(super) fn join_window(&mut self, ctx: &egui::Context) {
        let Some((lid, rid)) = self.join.as_ref().map(|j| (j.left, j.right)) else { return };
        let tabs: Vec<(u64, String)> = self.table_tabs().into_iter().map(|i| (self.docs[i].id, self.docs[i].title.clone())).collect();
        let index = |id: u64| self.docs.iter().position(|d| d.id == id);
        let (li, ri) = (index(lid), index(rid));
        let left_names = li.map(|i| self.column_names_of(i)).unwrap_or_default();
        let right_names = ri.map(|i| self.column_names_of(i)).unwrap_or_default();
        let Some(j) = self.join.as_mut() else { return };
        let mut open = true;
        let mut go = false;
        egui::Window::new(tr("Join tables")).open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
            ui.label(tr("Adds the other columns of the lookup table's rows with the same key to each row of the first table."));
            let pick = |ui: &mut egui::Ui, salt: &str, id: &mut u64| {
                let shown = tabs.iter().find(|t| t.0 == *id).map(|t| t.1.clone()).unwrap_or_default();
                egui::ComboBox::from_id_salt(salt).selected_text(shown).width(220.0).show_ui(ui, |ui| {
                    for (tid, title) in &tabs {
                        ui.selectable_value(id, *tid, title);
                    }
                });
            };
            let key = |ui: &mut egui::Ui, salt: &str, col: &mut usize, names: &[String]| {
                egui::ComboBox::from_id_salt(salt).selected_text(names.get(*col).cloned().unwrap_or_default()).width(160.0).show_ui(ui, |ui| {
                    for (i, n) in names.iter().enumerate() {
                        ui.selectable_value(col, i, n);
                    }
                });
            };
            egui::Grid::new("join").num_columns(3).show(ui, |ui| {
                ui.label(tr("Table"));
                pick(ui, "join-left", &mut j.left);
                key(ui, "join-left-key", &mut j.left_key, &left_names);
                ui.end_row();
                ui.label(tr("Lookup table"));
                pick(ui, "join-right", &mut j.right);
                key(ui, "join-right-key", &mut j.right_key, &right_names);
                ui.end_row();
            });
            ui.checkbox(&mut j.inner, tr("Only the rows with a match (inner join)"));
            let ok = li.is_some() && ri.is_some() && j.left != j.right;
            go = ui.add_enabled(ok, egui::Button::new(tr("Join"))).clicked();
        });
        if go {
            let j = self.join.take().expect("dialog");
            self.start_join(j);
        } else if !open {
            self.join = None;
        }
    }

    pub(crate) fn start_join(&mut self, j: JoinDialog) {
        let index = |id: u64| self.docs.iter().position(|d| d.id == id);
        let (Some(li), Some(ri)) = (index(j.left), index(j.right)) else { return };
        let (l, r) = (&self.docs[li], &self.docs[ri]);
        let (Some(ld), Some(rd)) = (l.table_state.dialect, r.table_state.dialect) else { return };
        let spec = JoinSpec { left_key: j.left_key, right_key: j.right_key, inner: j.inner, header: l.table_state.header && r.table_state.header };
        let (lsnap, rsnap) = (l.buf.snapshot(), r.buf.snapshot());
        let title = format!("{} + {}", l.title, r.title);
        let target = bigtext::decompress::temp_file("joined.csv");
        self.spawn_task(tr("Joining tables").into(), move |progress: &Progress, cancel: &CancelToken| {
            let res = (|| {
                if let Some(dir) = target.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                let mut w = std::io::BufWriter::with_capacity(8 << 20, std::fs::File::options().write(true).create_new(true).open(&target)?);
                let st = csv_join::join_to(&lsnap, ld, &rsnap, rd, spec, &mut w, cancel, Some(progress))?;
                std::io::Write::flush(&mut w)?;
                Ok::<_, std::io::Error>(st)
            })();
            match res {
                Ok(Some(st)) => {
                    let message = trf("{matched} of {rows} rows had a match; {out} rows.", &[("matched", &fmt_count(st.matched)), ("rows", &fmt_count(st.left_rows)), ("out", &fmt_count(st.out_rows))]);
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
