//! File → Export lines…, Save selection as… and Split file…: writing parts of the document to
//! new files on a worker, at line boundaries and with bounded memory.

use bigtext::filter;
use bigtext::jobs::{human_bytes, CancelToken, Progress};
use bigtext::split::{self, SplitBy};
use bigtext::text::TextBuf;

use super::filter_panel::{file_name, fmt_count};
use super::i18n::{tr, trf};
use super::tasks::{self, TaskOutcome};
use super::BigTextApp;

pub struct LinesDialog {
    from: String,
    to: String,
    error: Option<String>,
    /// Between two times rather than two line numbers.
    times: bool,
    from_time: String,
    to_time: String,
}

pub struct SplitDialog {
    by_lines: bool,
    pack: Option<bigtext::decompress::Kind>,
    size_mb: String,
    lines: String,
    error: Option<String>,
}

#[derive(Default)]
pub struct ExportUi {
    pub lines: Option<LinesDialog>,
    pub split: Option<SplitDialog>,
}

impl ExportUi {
    pub fn is_open(&self) -> bool {
        self.lines.is_some() || self.split.is_some()
    }
}

/// Writes `range` of the document (with its byte order mark) to `target`.
fn write_range(snap: &TextBuf, range: std::ops::Range<u64>, target: &std::path::Path, progress: &Progress, cancel: &CancelToken) -> TaskOutcome {
    progress.set_total(range.end - range.start);
    progress.set_done(0);
    let bom = snap.codec.bom_len as u64;
    tasks::write_via_temp(target, |w| {
        if bom > 0 {
            if let Some(b) = snap.read(0..bom) {
                std::io::Write::write_all(w, &b)?;
            }
        }
        Ok(filter::write_range_to(snap, range, w, cancel, Some(progress))?
            .map(|n| trf("Saved {size} to {file}.", &[("size", &human_bytes(n)), ("file", &file_name(target))])))
    })
}

impl BigTextApp {
    /// File name the outputs are named after.
    fn output_base_name(&self) -> String {
        match self.docs.get(self.active) {
            Some(d) => match (&d.unpacked_from, &d.path) {
                (Some(p), _) => bigtext::decompress::inner_name(p),
                (None, Some(_)) => d.title.clone(),
                (None, None) => format!("{}.txt", d.title),
            },
            None => "document.txt".into(),
        }
    }

    pub(crate) fn open_export_lines(&mut self) {
        let Some(d) = self.docs.get(self.active).filter(|d| d.is_ready()) else { return };
        let (s, e) = d.selection();
        let e = if e > s { e - d.buf.codec.unit() as u64 } else { e };
        let (a, b) = (d.buf.line_of(s).0 + 1, d.buf.line_of(e).0 + 1);
        self.export.lines = Some(LinesDialog { from: a.to_string(), to: b.to_string(), error: None, times: false, from_time: String::new(), to_time: String::new() });
    }

    pub(crate) fn save_selection_as(&mut self) {
        let Some(d) = self.docs.get(self.active).filter(|d| d.is_ready()) else { return };
        let (s, e) = d.selection();
        if s == e {
            self.error = Some(tr("Select some text first.").into());
            return;
        }
        let snap = d.buf.snapshot();
        let name = format!("{}.selection.txt", self.output_base_name());
        let Some(target) = self.pick_output_file(tr("Save selection as"), &name) else { return };
        self.spawn_task(tr("Saving selection").into(), move |progress, cancel| write_range(&snap, s..e, &target, progress, cancel));
    }

    pub(crate) fn open_split(&mut self) {
        if self.docs.get(self.active).is_some_and(|d| d.is_ready()) {
            self.export.split = Some(SplitDialog { by_lines: false, pack: None, size_mb: "100".into(), lines: "1000000".into(), error: None });
        }
    }

    /// Writes the lines from one time to another (inclusive to its second; `to` without a date
    /// is on `from`'s day) to a file the user picks.
    pub(crate) fn export_times(&mut self, from: Option<bigtext::timestamp::Stamp>, to: Option<bigtext::timestamp::Stamp>, target: Option<std::path::PathBuf>) {
        let Some(d) = self.docs.get(self.active) else { return };
        let snap = d.buf.snapshot();
        let name = format!("{}.between.txt", self.output_base_name());
        let Some(target) = target.or_else(|| self.pick_output_file(tr("Export lines"), &name)) else { return };
        let to = match (from, to) {
            (Some(f), Some(t)) => Some(bigtext::timestamp::rebase(t, f)),
            (_, t) => t,
        };
        self.spawn_task(tr("Exporting lines between times").into(), move |progress, cancel| match bigtext::timestamp::time_range(&snap, from, to, cancel) {
            Ok((s, e)) if e > s => write_range(&snap, s..e, &target, progress, cancel),
            Ok(_) => TaskOutcome::Failed(tr("No lines between these times.").into()),
            Err(bigtext::timestamp::TimeError::Cancelled) => TaskOutcome::Cancelled,
            Err(_) => TaskOutcome::Failed(tr("The document has no timestamps to go by.").into()),
        });
    }

    fn export_lines(&mut self, from: u64, to: u64) {
        let Some(d) = self.docs.get(self.active) else { return };
        let snap = d.buf.snapshot();
        let name = format!("{}.lines-{from}-{to}.txt", self.output_base_name());
        let Some(target) = self.pick_output_file(tr("Export lines"), &name) else { return };
        self.spawn_task(trf("Exporting lines {a}–{b}", &[("a", &from.to_string()), ("b", &to.to_string())]), move |progress, cancel| {
            match split::line_range(&snap, from, to, cancel, Some(progress)) {
                Some(range) => write_range(&snap, range, &target, progress, cancel),
                None if cancel.is_cancelled() => TaskOutcome::Cancelled,
                None => TaskOutcome::Failed(trf("Line {n} does not exist.", &[("n", &from.to_string())])),
            }
        });
    }

    fn split_file(&mut self, by: SplitBy, pack: Option<bigtext::decompress::Kind>) {
        let Some(d) = self.docs.get(self.active) else { return };
        let snap = d.buf.snapshot();
        let mut dlg = rfd::FileDialog::new().set_title(tr("Folder for the parts"));
        if let Some(dir) = d.unpacked_from.as_ref().or(d.path.as_ref()).and_then(|p| p.parent()) {
            dlg = dlg.set_directory(bigtext::file::display_path(dir));
        }
        let Some(dir) = dlg.pick_folder() else { return };
        let name = self.output_base_name();
        self.spawn_task(tr("Splitting").into(), move |progress, cancel| match split::split_to_files(&snap, by, &dir, &name, pack, cancel, progress) {
            Ok(Some(files)) => TaskOutcome::Done {
                message: trf("Wrote {n} parts to {dir}.", &[("n", &fmt_count(files.len() as u64)), ("dir", &bigtext::file::display_path(&dir))]),
                open: files.first().cloned(),
            },
            Ok(None) => TaskOutcome::Cancelled,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                TaskOutcome::Failed(trf("{file} already exists in that folder; nothing was written.", &[("file", &file_name(&split::packed_part_path(&dir, &name, 1, pack)))]))
            }
            Err(e) => TaskOutcome::Failed(bigtext::save::describe(&e, &dir)),
        });
    }

    pub(super) fn export_windows(&mut self, ctx: &egui::Context) {
        let parse = |s: &str| s.trim().replace(['_', ',', ' '], "").parse::<u64>().ok().filter(|&n| n > 0);
        if let Some(g) = &mut self.export.lines {
            let mut open = true;
            let mut go = None;
            let mut go_times = None;
            egui::Window::new(tr("Export lines")).collapsible(false).resizable(false).open(&mut open).show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.radio_value(&mut g.times, false, tr("Line numbers"));
                    ui.radio_value(&mut g.times, true, tr("Between times"));
                });
                egui::Grid::new("export-lines").num_columns(2).show(ui, |ui| {
                    if g.times {
                        ui.label(tr("From"));
                        ui.add(egui::TextEdit::singleline(&mut g.from_time).hint_text("2024-05-01 14:00"));
                        ui.end_row();
                        ui.label(tr("To (inclusive)"));
                        ui.add(egui::TextEdit::singleline(&mut g.to_time).hint_text("14:30"));
                        ui.end_row();
                    } else {
                        ui.label(tr("From line"));
                        ui.text_edit_singleline(&mut g.from);
                        ui.end_row();
                        ui.label(tr("To line (inclusive)"));
                        ui.text_edit_singleline(&mut g.to);
                        ui.end_row();
                    }
                });
                if g.times {
                    ui.weak(tr("The log's lines in time order are found by binary search; an empty field means its start or end."));
                }
                ui.weak(tr("The lines are copied to a new file; the document does not change."));
                if let Some(e) = &g.error {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                if ui.button(tr("Save as…")).clicked() {
                    if g.times {
                        let t = |s: &str| if s.trim().is_empty() { Ok(None) } else { bigtext::timestamp::parse(s).map(Some).ok_or(()) };
                        match (t(&g.from_time), t(&g.to_time)) {
                            (Ok(a), Ok(b)) if a.is_some() || b.is_some() => go_times = Some((a, b)),
                            _ => g.error = Some(tr("Enter a time like 2024-05-01 14:00:00 or 14:30.").into()),
                        }
                    } else {
                        match (parse(&g.from), parse(&g.to)) {
                            (Some(a), Some(b)) if a <= b => go = Some((a, b)),
                            _ => g.error = Some(tr("Enter two line numbers, the first not larger than the second.").into()),
                        }
                    }
                }
            });
            if !open || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.export.lines = None;
            }
            if let Some((a, b)) = go {
                self.export.lines = None;
                self.export_lines(a, b);
            }
            if let Some((a, b)) = go_times {
                self.export.lines = None;
                self.export_times(a, b, None);
            }
        }
        if let Some(g) = &mut self.export.split {
            let mut open = true;
            let mut go = None;
            egui::Window::new(tr("Split file")).collapsible(false).resizable(false).open(&mut open).show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.radio_value(&mut g.by_lines, false, tr("Parts of"));
                    ui.add(egui::TextEdit::singleline(&mut g.size_mb).desired_width(70.0));
                    ui.label("MB");
                });
                ui.horizontal(|ui| {
                    ui.radio_value(&mut g.by_lines, true, tr("Parts of"));
                    ui.add(egui::TextEdit::singleline(&mut g.lines).desired_width(90.0));
                    ui.label(tr("lines"));
                });
                ui.horizontal(|ui| {
                    ui.label(tr("Compress the parts:"));
                    let shown = g.pack.map(|k| format!(".{}", k.extension())).unwrap_or_else(|| tr("no").into());
                    egui::ComboBox::from_id_salt("split-pack").selected_text(shown).show_ui(ui, |ui| {
                        ui.selectable_value(&mut g.pack, None, tr("no"));
                        for k in bigtext::decompress::Kind::ALL {
                            ui.selectable_value(&mut g.pack, Some(k), format!(".{}", k.extension()));
                        }
                    });
                });
                ui.weak(tr("Parts end at line breaks and are named name.part001.ext, name.part002.ext, … in the folder you choose."));
                if let Some(e) = &g.error {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                if ui.button(tr("Choose folder and split…")).clicked() {
                    let by = if g.by_lines { parse(&g.lines).map(SplitBy::Lines) } else { g.size_mb.trim().replace(',', ".").parse::<f64>().ok().filter(|&m| m > 0.0).map(|m| SplitBy::Bytes((m * 1048576.0) as u64)) };
                    match by {
                        Some(by) => go = Some(by),
                        None => g.error = Some(tr("Enter a positive number.").into()),
                    }
                }
            });
            if !open || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.export.split = None;
            }
            if let Some(by) = go {
                let pack = self.export.split.take().and_then(|g| g.pack);
                self.split_file(by, pack);
            }
        }
    }
}
