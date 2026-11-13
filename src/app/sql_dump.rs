//! SQL dump tables (File → SQL dump tables…): the tables of a mysqldump, pg_dump or SQLite dump
//! with their rows and sizes, read in one pass. A click goes to a table's CREATE TABLE; tables
//! are saved to a file that restores on its own, opened in a tab, or the dump is split into a
//! file per table.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bigtext::jobs::{self, human_bytes, CancelToken, Progress};
use bigtext::sqldump::{self, Dump, Extract};

use super::filter_panel::{file_name, fmt_count};
use super::i18n::{tr, trf};
use super::tasks::{self, TaskOutcome};
use super::BigTextApp;

#[derive(Default)]
pub struct DumpData {
    pub d: Option<Dump>,
    pub done: bool,
    pub error: Option<String>,
}

/// The tables of one document.
pub struct DumpResults {
    pub version: u64,
    pub data: Arc<Mutex<DumpData>>,
    pub cancel: CancelToken,
    pub progress: Progress,
}

impl DumpResults {
    pub fn is_running(&self) -> bool {
        !self.data.lock().unwrap().done
    }
}

pub struct DumpUi {
    pub open: bool,
    pub filter: String,
    pub largest_first: bool,
    /// The dump's settings (and what it defines before its tables) in each file written, so it
    /// restores on its own.
    pub settings: bool,
    /// The checked tables, by name.
    pub checked: BTreeSet<String>,
}

impl Default for DumpUi {
    fn default() -> Self {
        DumpUi {
            open: false,
            filter: String::new(),
            largest_first: false,
            settings: true,
            checked: BTreeSet::new(),
        }
    }
}

enum Act {
    Goto(u64),
    Save(Vec<String>),
    Open(Vec<String>),
    Split,
    Rerun,
}

impl BigTextApp {
    /// File → SQL dump tables…
    pub(crate) fn open_sql_dump(&mut self) {
        self.sql_dump.open = true;
        let fresh = self
            .docs
            .get(self.active)
            .and_then(|d| d.sql_dump.as_ref().map(|r| r.version == d.version));
        if fresh != Some(true) {
            self.start_sql_dump();
        }
    }

    pub(crate) fn start_sql_dump(&mut self) {
        let Some(d) = self.docs.get_mut(self.active) else {
            return;
        };
        if !d.is_ready() {
            return;
        }
        if let Some(r) = d.sql_dump.take() {
            r.cancel.cancel();
        }
        self.sql_dump.checked.clear();
        let data = Arc::new(Mutex::new(DumpData::default()));
        let (cancel, progress) = (CancelToken::new(), Progress::new(d.buf.len()));
        d.sql_dump = Some(DumpResults {
            version: d.version,
            data: data.clone(),
            cancel: cancel.clone(),
            progress: progress.clone(),
        });
        let snap = d.buf.snapshot();
        let waker = self.waker.clone();
        jobs::spawn("sql-dump", move || {
            let partial = |p: &Dump| {
                data.lock().unwrap().d = Some(p.clone());
                waker();
            };
            let r = sqldump::scan(&snap, &cancel, Some(&progress), partial);
            let mut g = data.lock().unwrap();
            match r {
                Ok(Some(dump)) => g.d = Some(dump),
                Ok(None) => {}
                Err(e) => g.error = Some(e.to_string()),
            }
            g.done = true;
            drop(g);
            waker();
        });
    }

    pub(super) fn sql_dump_window(&mut self, ctx: &egui::Context) {
        if !self.sql_dump.open {
            return;
        }
        let mut open = true;
        let mut act = None;
        let ui_state = &mut self.sql_dump;
        let d = self.docs.get(self.active);
        egui::Window::new(tr("SQL dump tables")).open(&mut open).default_size([720.0, 480.0]).resizable(true).collapsible(false).show(ctx, |ui| {
            let Some(d) = d else { return };
            let Some(res) = &d.sql_dump else {
                ui.weak(tr("Open a SQL dump first."));
                return;
            };
            let g = res.data.lock().unwrap();
            let stale = res.version != d.version;
            ui.horizontal_wrapped(|ui| {
                if let Some(dump) = &g.d {
                    ui.strong(tr(dump.dialect.name()));
                    ui.label(trf("{n} tables, {s} statements", &[("n", &fmt_count(dump.tables.len() as u64)), ("s", &fmt_count(dump.statements))]));
                }
                if !g.done {
                    let f = res.progress.fraction();
                    ui.add(egui::ProgressBar::new(f).desired_width(120.0).text(format!("{:.0}%", f * 100.0)));
                    ui.ctx().request_repaint_after(Duration::from_millis(100));
                    if ui.small_button(tr("Cancel")).clicked() {
                        res.cancel.cancel();
                    }
                }
                if stale {
                    ui.colored_label(super::theme::warn_text(ui), tr("stale (document changed)"));
                    if ui.small_button(tr("Run again")).clicked() {
                        act = Some(Act::Rerun);
                    }
                }
            });
            if let Some(e) = &g.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            let Some(dump) = &g.d else {
                ui.weak(tr("Reading the lines…"));
                return;
            };
            if g.done && dump.tables.is_empty() {
                ui.weak(tr("No CREATE TABLE, INSERT or COPY statements were found: this does not look like a SQL dump."));
                return;
            }
            ui.horizontal(|ui| {
                ui.label(tr("Filter:"));
                ui.add(egui::TextEdit::singleline(&mut ui_state.filter).desired_width(160.0));
                ui.radio_value(&mut ui_state.largest_first, false, tr("In the dump's order"));
                ui.radio_value(&mut ui_state.largest_first, true, tr("Largest first"));
            });
            ui.checkbox(&mut ui_state.settings, tr("With the dump's settings, so each file restores on its own"));
            let needle = ui_state.filter.trim().to_lowercase();
            let mut order: Vec<usize> = (0..dump.tables.len()).filter(|&i| needle.is_empty() || dump.tables[i].name.to_lowercase().contains(&needle)).collect();
            if ui_state.largest_first {
                order.sort_by_key(|&i| std::cmp::Reverse(dump.tables[i].bytes()));
            }
            let font = egui::FontId::monospace(13.0);
            ui.add(egui::Label::new(egui::RichText::new(format!("    {:<44} {:>14} {:>10}", tr("Table"), tr("Rows"), tr("Size"))).font(font.clone()).weak()).extend());
            let row_h = ui.ctx().fonts_mut(|f| f.row_height(&font)) + 6.0;
            let n = order.len();
            egui::ScrollArea::both().id_salt("sql-dump-tables").auto_shrink([false, false]).max_height((ui.available_height() - 40.0).max(80.0)).show_rows(ui, row_h, n, |ui, range| {
                for &i in &order[range.start.min(n)..range.end.min(n)] {
                    let t = &dump.tables[i];
                    ui.horizontal(|ui| {
                        let mut on = ui_state.checked.contains(&t.name);
                        if ui.checkbox(&mut on, "").changed() {
                            if on {
                                ui_state.checked.insert(t.name.clone());
                            } else {
                                ui_state.checked.remove(&t.name);
                            }
                        }
                        let text = format!("{:<44} {:>14} {:>10}", t.name, fmt_count(t.rows), human_bytes(t.bytes()));
                        let rich = egui::RichText::new(text).font(font.clone());
                        let rich = if t.created { rich } else { rich.weak() };
                        let resp = ui.add(egui::Button::selectable(false, rich).wrap_mode(egui::TextWrapMode::Extend));
                        let resp = if t.created { resp.on_hover_text(tr("Click: go to its CREATE TABLE; right-click: more")) } else { resp.on_hover_text(tr("The dump has no CREATE TABLE for it, only its rows or changes")) };
                        if resp.clicked() && !stale {
                            act = Some(Act::Goto(t.first));
                        }
                        resp.context_menu(|ui| {
                            if ui.add_enabled(g.done && !stale, egui::Button::new(tr("Save this table as…"))).clicked() {
                                act = Some(Act::Save(vec![t.name.clone()]));
                                ui.close();
                            }
                            if ui.add_enabled(g.done && !stale, egui::Button::new(tr("Open this table in a new tab"))).clicked() {
                                act = Some(Act::Open(vec![t.name.clone()]));
                                ui.close();
                            }
                            if ui.button(tr("Copy name")).clicked() {
                                ui.ctx().copy_text(t.name.clone());
                                ui.close();
                            }
                        });
                    });
                }
            });
            ui.separator();
            let checked: Vec<String> = dump.tables.iter().filter(|t| ui_state.checked.contains(&t.name)).map(|t| t.name.clone()).collect();
            let ready = g.done && !stale;
            ui.horizontal(|ui| {
                if ui.small_button(tr("All")).clicked() {
                    ui_state.checked.extend(order.iter().map(|&i| dump.tables[i].name.clone()));
                }
                if ui.small_button(tr("None")).clicked() {
                    ui_state.checked.clear();
                }
                let label = if checked.is_empty() { tr("Save the checked tables as one file…").to_string() } else { trf("Save the {n} checked as one file…", &[("n", &checked.len().to_string())]) };
                if ui.add_enabled(ready && !checked.is_empty(), egui::Button::new(label)).clicked() {
                    act = Some(Act::Save(checked.clone()));
                }
                if ui.add_enabled(ready && !checked.is_empty(), egui::Button::new(tr("Open them in a tab"))).clicked() {
                    act = Some(Act::Open(checked.clone()));
                }
                if ui.add_enabled(ready, egui::Button::new(tr("Split into a file per table…"))).on_hover_text(tr("The files restore the dump in the order of their names: 0-before-tables, the tables, 2-after-tables")).clicked() {
                    act = Some(Act::Split);
                }
            });
        });
        self.sql_dump.open = open;
        match act {
            Some(Act::Goto(off)) => {
                if let Some(d) = self.docs.get_mut(self.active) {
                    d.goto_offset(off);
                }
            }
            Some(Act::Save(names)) => self.save_dump_tables(names, false),
            Some(Act::Open(names)) => self.save_dump_tables(names, true),
            Some(Act::Split) => self.split_dump(),
            Some(Act::Rerun) => self.start_sql_dump(),
            None => {}
        }
    }

    /// The finished dump of the active document, its file stem and a snapshot.
    fn dump_parts(&self) -> Option<(Dump, String, bigtext::text::TextBuf)> {
        let d = self.docs.get(self.active)?;
        let r = d.sql_dump.as_ref().filter(|r| r.version == d.version)?;
        let dump = r.data.lock().unwrap().d.clone()?;
        let name = d
            .title
            .trim_end_matches(".gz")
            .trim_end_matches(".zst")
            .trim_end_matches(".xz")
            .trim_end_matches(".bz2");
        let stem = name.strip_suffix(".sql").unwrap_or(name).to_string();
        Some((dump, stem, d.buf.snapshot()))
    }

    /// Writes the tables `names` to a file the user picks, or (`tab`) to a temporary copy that
    /// opens read-only.
    pub(crate) fn save_dump_tables(&mut self, names: Vec<String>, tab: bool) {
        let Some((dump, stem, snap)) = self.dump_parts() else {
            return;
        };
        let which: Vec<usize> = names
            .iter()
            .filter_map(|n| dump.tables.iter().position(|t| &t.name == n))
            .collect();
        if which.is_empty() {
            return;
        }
        let x = if self.sql_dump.settings {
            Extract::WHOLE
        } else {
            Extract::BARE
        };
        let file = if which.len() == 1 {
            sqldump::file_name(&stem, &dump.tables[which[0]].name)
        } else {
            format!("{stem}.tables.sql")
        };
        let title = if which.len() == 1 {
            format!("{} ({stem})", dump.tables[which[0]].name)
        } else {
            trf(
                "{n} tables ({name})",
                &[("n", &which.len().to_string()), ("name", &stem)],
            )
        };
        if tab {
            let (path, f) = match super::remote::temp_copy(&file) {
                Ok(x) => x,
                Err(e) => {
                    self.error = Some(e);
                    return;
                }
            };
            self.spawn_task(
                tr("Writing the tables").into(),
                move |progress: &Progress, cancel: &CancelToken| -> TaskOutcome {
                    let mut w = std::io::BufWriter::new(f);
                    let r = sqldump::write_tables(
                        &snap,
                        &dump,
                        &which,
                        x,
                        &mut w,
                        cancel,
                        Some(progress),
                    )
                    .and_then(|n| std::io::Write::flush(&mut w).map(|_| n));
                    match r {
                        Ok(Some(bytes)) => {
                            TaskOutcome::Remote(Box::new(super::remote::RemoteDone {
                                path,
                                title,
                                bytes,
                            }))
                        }
                        Ok(None) => {
                            bigtext::decompress::remove_temp(&path);
                            TaskOutcome::Cancelled
                        }
                        Err(e) => {
                            bigtext::decompress::remove_temp(&path);
                            TaskOutcome::Failed(e.to_string())
                        }
                    }
                },
            );
            return;
        }
        let Some(target) = self.pick_output_file(tr("Save the tables as"), &file) else {
            return;
        };
        self.spawn_task(
            tr("Writing the tables").into(),
            move |progress: &Progress, cancel: &CancelToken| -> TaskOutcome {
                tasks::write_via_temp(&target, |w| {
                    Ok(
                        sqldump::write_tables(&snap, &dump, &which, x, w, cancel, Some(progress))?
                            .map(|n| {
                                trf(
                                    "Saved {size} to {file}.",
                                    &[("size", &human_bytes(n)), ("file", &file_name(&target))],
                                )
                            }),
                    )
                })
            },
        );
    }

    /// Writes a file per table into a folder the user picks.
    pub(crate) fn split_dump(&mut self) {
        let Some((dump, stem, snap)) = self.dump_parts() else {
            return;
        };
        let start = self
            .docs
            .get(self.active)
            .and_then(|d| d.path.as_ref())
            .and_then(|p| p.parent().map(|p| p.to_path_buf()));
        let mut dlg = rfd::FileDialog::new().set_title(tr("Folder for the files"));
        if let Some(s) = start {
            dlg = dlg.set_directory(s);
        }
        let Some(dir) = dlg.pick_folder() else { return };
        let settings = self.sql_dump.settings;
        self.spawn_task(tr("Splitting the dump").into(), move |progress: &Progress, cancel: &CancelToken| -> TaskOutcome {
            match sqldump::split(&snap, &dump, &dir, &stem, settings, cancel, Some(progress)) {
                Ok(Some(files)) => {
                    let total: u64 = files.iter().map(|f| f.1).sum();
                    let message = trf("Wrote {n} files ({size}) to {dir}; restore them in the order of their names.", &[("n", &files.len().to_string()), ("size", &human_bytes(total)), ("dir", &bigtext::file::display_path(&dir))]);
                    TaskOutcome::Done { message, open: files.first().map(|f| f.0.clone()) }
                }
                Ok(None) => TaskOutcome::Cancelled,
                Err(e) => TaskOutcome::Failed(e.to_string()),
            }
        });
    }
}
