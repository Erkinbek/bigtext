//! The status bar: every field is a control (Ln/Col goes to a line, the encoding reopens, the
//! line ending picks the one to save with, READ-ONLY turns it off), and right-click or … chooses
//! which fields are shown.

use std::time::Duration;

use egui::{Align, Color32, CursorIcon, Layout, RichText};

use bigtext::encoding::{Encoding, LineEnding};
use bigtext::file;
use bigtext::jobs::human_bytes;

use super::commands::Cmd;
use super::i18n::{tr, trf};
use super::{filter_panel, follow_id, marks, BigTextApp};

/// Which fields the status bar shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StatusFields {
    pub offset: bool,
    /// Lines, characters and bytes selected.
    pub sel: bool,
    /// Time since the previous line, and across the selection.
    pub dt: bool,
    /// A merged log's files in their colours (otherwise "N files").
    pub legend: bool,
    /// How the file is read (mapped, chunked).
    pub kind: bool,
    pub index_time: bool,
    pub path: bool,
}

impl Default for StatusFields {
    fn default() -> Self {
        StatusFields { offset: true, sel: true, dt: true, legend: true, kind: false, index_time: false, path: false }
    }
}

impl StatusFields {
    /// Names in a `status=` line.
    const NAMES: [&'static str; 7] = ["offset", "selection", "time", "legend", "access", "index-time", "path"];

    fn flags(&mut self) -> [&mut bool; 7] {
        [&mut self.offset, &mut self.sel, &mut self.dt, &mut self.legend, &mut self.kind, &mut self.index_time, &mut self.path]
    }

    /// The fields shown, "offset,selection,…".
    pub fn to_line(mut self) -> String {
        Self::NAMES.iter().zip(self.flags()).filter(|(_, on)| **on).map(|(n, _)| *n).collect::<Vec<_>>().join(",")
    }

    pub fn from_line(v: &str) -> StatusFields {
        let on: Vec<&str> = v.split(',').map(str::trim).collect();
        let mut f = StatusFields::default();
        for (n, flag) in Self::NAMES.iter().zip(f.flags()) {
            *flag = on.contains(n);
        }
        f
    }

    /// The checkboxes of the field menu: returns the new choice when one was flipped.
    fn menu(mut self, ui: &mut egui::Ui) -> Option<StatusFields> {
        let old = self;
        ui.label(RichText::new(tr("Show in the status bar")).strong());
        let labels = [
            tr("Byte offset"),
            tr("Selection size"),
            tr("Time since the previous line"),
            tr("Merged log's files"),
            tr("File access (mapped or chunked)"),
            tr("Indexing time"),
            tr("File path"),
        ];
        for (label, flag) in labels.into_iter().zip(self.flags()) {
            ui.checkbox(flag, label);
        }
        (self != old).then_some(self)
    }
}

/// What a click in the status bar asks for, done once the bar is drawn.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum StatusAct {
    GotoLine,
    GotoOffset,
    Reopen(Option<Encoding>),
    Eol(Option<LineEnding>),
    ToggleReadOnly,
    CopyPath,
    Fields(StatusFields),
}

/// A field that does something when clicked.
fn field(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>, tip: &str) -> egui::Response {
    ui.add(egui::Button::new(text).frame(false)).on_hover_cursor(CursorIcon::PointingHand).on_hover_text(tip)
}

/// A field that opens a menu.
fn field_menu(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>, tip: &str, content: impl FnOnce(&mut egui::Ui)) {
    let (r, _) = egui::containers::menu::MenuButton::from_button(egui::Button::new(text).frame(false)).ui(ui, content);
    r.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(tip);
}

impl BigTextApp {
    pub(super) fn status_bar(&mut self, ui: &mut egui::Ui) {
        let ids = self.caret_ids();
        let delta = self.caret_time_delta();
        let fields = self.settings.status;
        // The command a field runs, with its shortcut.
        let tip = |cmd: Cmd| match self.keymap.get(cmd) {
            Some(k) => format!("{} ({})", tr(cmd.label()), super::commands::key_text(k)),
            None => tr(cmd.label()).to_string(),
        };
        let (line_tip, offset_tip) = (tip(Cmd::GotoLine), tip(Cmd::GotoOffset));
        let mut follow = None;
        let mut acts = Vec::new();
        // A horizontal layout's response only hovers: the scope takes the right-click.
        let bar = ui.scope_builder(egui::UiBuilder::new().sense(egui::Sense::click()), |ui| {
            ui.horizontal(|ui| {
                if let Some(d) = self.docs.get(self.active).filter(|d| d.is_ready()) {
                    let caret = d.view.caret;
                    let (line, exact) = d.buf.line_of(caret);
                    let col = match d.buf.line_start(line) {
                        Some((ls, true)) if caret >= ls && caret - ls <= 128 * 1024 => {
                            let b = d.buf.read(ls..caret);
                            b.map(|b| format!("{}", bigtext::text::rows::cell_width(&d.buf.codec, &b, ls) + 1)).unwrap_or_else(|| "…".into())
                        }
                        Some((ls, _)) if caret >= ls => format!("~{}", caret - ls + 1),
                        _ => "?".into(),
                    };
                    let pos = trf("Ln {line}, Col {col}", &[("line", &format!("{}{}", if exact { "" } else { "~" }, line + 1)), ("col", &col)]);
                    if field(ui, pos, &line_tip).clicked() {
                        acts.push(StatusAct::GotoLine);
                    }
                    if let Some(ms) = delta.filter(|_| fields.dt) {
                        ui.separator();
                        ui.label(format!("Δ {}", bigtext::timestamp::format_delta(ms))).on_hover_text(tr("Time since the previous line with a timestamp"));
                    }
                    if !d.view.extra.is_empty() {
                        ui.separator();
                        ui.label(trf("{n} cursors", &[("n", &(d.view.extra.len() + 1).to_string())])).on_hover_text(tr("Esc: back to one cursor"));
                    }
                    if fields.offset {
                        ui.separator();
                        if field(ui, trf("Offset {n}", &[("n", &format!("{caret} (0x{caret:X})"))]), &offset_tip).clicked() {
                            acts.push(StatusAct::GotoOffset);
                        }
                    }
                    if let Some((lines, exact, chars, bytes)) = d.selection_stats().filter(|_| fields.sel) {
                        ui.separator();
                        let n = |x: u64| filter_panel::fmt_count(x);
                        let lines = format!("{}{}", if exact { "" } else { "~" }, n(lines));
                        let text = match chars {
                            Some(c) => trf("Sel: {lines} lines, {chars} chars, {bytes} bytes", &[("lines", &lines), ("chars", &n(c)), ("bytes", &n(bytes))]),
                            None => trf("Sel: {lines} lines, {bytes} bytes", &[("lines", &lines), ("bytes", &n(bytes))]),
                        };
                        ui.label(text);
                        if let Some(ms) = d.selection_time_delta().filter(|_| fields.dt) {
                            ui.separator();
                            ui.label(format!("Δt {}", bigtext::timestamp::format_delta(ms))).on_hover_text(tr("Time between the timestamps of the first and last selected lines"));
                        }
                    }
                    if let Some(b) = d.view.block {
                        let (lo, hi) = b.cells();
                        ui.separator();
                        ui.label(trf("Block: {cols} columns wide", &[("cols", &(hi - lo).to_string())]));
                    }
                    let (lines, lexact) = d.buf.line_count();
                    ui.separator();
                    ui.label(trf("{n} lines", &[("n", &format!("{}{}", if lexact { "" } else { "~" }, lines))]));
                    if !ids.is_empty() {
                        ui.separator();
                        follow = follow_id::id_chips(ui, &ids);
                    }
                    // The colours of a merged log's files.
                    if let Some(t) = &d.merged {
                        ui.separator();
                        let dark = ui.visuals().dark_mode;
                        if fields.legend {
                            for (i, l) in t.labels.iter().enumerate() {
                                ui.label(RichText::new(l).background_color(marks::fill(i, dark)));
                            }
                        } else {
                            ui.label(trf("{n} files", &[("n", &t.labels.len().to_string())])).on_hover_ui(|ui| {
                                for (i, l) in t.labels.iter().enumerate() {
                                    ui.label(RichText::new(l).background_color(marks::fill(i, dark)));
                                }
                            });
                        }
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    // For trackpads, where a right-click is awkward.
                    field_menu(ui, "…", tr("Choose the fields of the status bar"), |ui| {
                        if let Some(f) = fields.menu(ui) {
                            acts.push(StatusAct::Fields(f));
                        }
                    });
                    self.tasks_status(ui);
                    let Some(d) = self.docs.get(self.active) else { return };
                    if let Some(j) = &d.save_job {
                        if ui.small_button(tr("Cancel")).clicked() {
                            j.cancel.cancel();
                        }
                        ui.add(egui::ProgressBar::new(j.progress.fraction()).desired_width(160.0).text(format!("{} {:.0}%", j.label, j.progress.fraction() * 100.0)));
                        ui.separator();
                    }
                    if d.follow {
                        let label = if d.rotations > 0 { format!("{} ×{}", tr("FOLLOW"), d.rotations + 1) } else { tr("FOLLOW").to_string() };
                        ui.label(RichText::new(label).color(Color32::from_rgb(90, 180, 90))).on_hover_text(trf("Following the file; it was rotated {n} times so far", &[("n", &d.rotations.to_string())]));
                        ui.separator();
                    }
                    if d.table && !d.hex {
                        let delim = d.table_state.dialect.map(|x| tr(x.name())).unwrap_or("…");
                        ui.label(RichText::new(format!("{} · {delim}", tr("TABLE"))).color(Color32::from_rgb(90, 160, 220)));
                        ui.separator();
                    }
                    if let Some(r) = d.results.as_ref().filter(|r| r.is_running()) {
                        ui.add(egui::ProgressBar::new(r.progress.fraction()).desired_width(140.0).text(format!("{} {:.0}%", tr("Searching"), r.progress.fraction() * 100.0)));
                        ui.separator();
                    } else if d.find_job.is_some() {
                        ui.label(tr("Searching…"));
                        ui.separator();
                    }
                    if d.read_only {
                        let text = RichText::new(tr("READ-ONLY")).color(Color32::from_rgb(220, 150, 60));
                        // A merged log has no file of its own to edit.
                        if d.merged.is_some() {
                            ui.label(text);
                        } else if field(ui, text, tr("Click to allow editing")).clicked() {
                            acts.push(StatusAct::ToggleReadOnly);
                        }
                        ui.separator();
                    }
                    if fields.kind {
                        ui.label(d.buf.source.kind_name());
                        ui.separator();
                    }
                    ui.label(human_bytes(d.buf.len()));
                    ui.separator();
                    let eol = match d.eol_on_save {
                        Some(to) if to != d.line_ending => format!("{} → {}", d.line_ending.name(), to.name()),
                        _ => d.line_ending.name().to_string(),
                    };
                    field_menu(ui, eol, tr("Line endings on save"), |ui| {
                        for (to, label) in [(None, "Keep as is"), (Some(LineEnding::Lf), "Convert to LF"), (Some(LineEnding::CrLf), "Convert to CRLF"), (Some(LineEnding::Cr), "Convert to CR")] {
                            if ui.radio(d.eol_on_save == to, tr(label)).clicked() {
                                acts.push(StatusAct::Eol(to));
                            }
                        }
                    });
                    ui.separator();
                    field_menu(ui, d.buf.codec.enc.name(), tr("Reopen with encoding"), |ui| {
                        if ui.add_enabled(d.forced_enc.is_some(), egui::Button::new(tr("Auto-detect (forget)"))).clicked() {
                            acts.push(StatusAct::Reopen(None));
                        }
                        ui.separator();
                        for enc in Encoding::ALL {
                            if ui.radio(d.forced_enc == Some(enc), enc.name()).clicked() {
                                acts.push(StatusAct::Reopen(Some(enc)));
                            }
                        }
                    });
                    if d.is_ready() && !d.buf.index.is_done() && !d.buf.is_empty() {
                        ui.separator();
                        ui.add(egui::ProgressBar::new(d.buf.index.progress()).desired_width(120.0).text(format!("{} {:.0}%", tr("Indexing"), d.buf.index.progress() * 100.0)));
                    } else if let Some(t) = d.buf.index.finished_after().filter(|t| fields.index_time && *t > Duration::ZERO) {
                        ui.separator();
                        ui.label(trf("Indexed in {t}s", &[("t", &format!("{:.2}", t.as_secs_f32()))]));
                    }
                    if let Some(p) = d.unpacked_from.as_deref().or(d.path.as_deref()).filter(|_| fields.path) {
                        ui.separator();
                        let r = ui.add(egui::Label::new(file::display_path(p)).truncate().sense(egui::Sense::click()));
                        if r.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(tr("Click to copy the path")).clicked() {
                            acts.push(StatusAct::CopyPath);
                        }
                    }
                });
            });
        });
        bar.response.context_menu(|ui| {
            if let Some(f) = fields.menu(ui) {
                acts.push(StatusAct::Fields(f));
            }
        });
        let ctx = ui.ctx().clone();
        for a in acts {
            self.status_act(&ctx, a);
        }
        if let Some(id) = follow {
            self.follow_id(&id);
        }
    }

    pub(crate) fn status_act(&mut self, ctx: &egui::Context, a: StatusAct) {
        match a {
            StatusAct::GotoLine => self.run(ctx, Cmd::GotoLine),
            StatusAct::GotoOffset => self.run(ctx, Cmd::GotoOffset),
            StatusAct::Reopen(enc) => self.reopen_with(enc),
            StatusAct::Eol(to) => {
                if let Some(d) = self.docs.get_mut(self.active) {
                    d.eol_on_save = to;
                }
            }
            StatusAct::ToggleReadOnly => self.run(ctx, Cmd::ReadOnly),
            StatusAct::CopyPath => {
                if let Some(p) = self.docs.get(self.active).and_then(|d| d.unpacked_from.as_deref().or(d.path.as_deref())) {
                    ctx.copy_text(file::display_path(p));
                }
            }
            StatusAct::Fields(f) => {
                self.settings.status = f;
                self.persist_state();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_line() {
        let d = StatusFields::default();
        assert_eq!(d.to_line(), "offset,selection,time,legend");
        assert_eq!(StatusFields::from_line(&d.to_line()), d);
        let f = StatusFields { offset: false, path: true, index_time: true, ..d };
        assert_eq!(StatusFields::from_line(&f.to_line()), f);
        assert_eq!(StatusFields::from_line(""), StatusFields { offset: false, sel: false, dt: false, legend: false, kind: false, index_time: false, path: false });
    }
}
