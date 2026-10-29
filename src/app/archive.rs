//! Opening an archive (.zip, .tar, .tar.gz…): its files are listed in a picker, and the one
//! picked is unpacked into a temporary read-only copy like a .gz.

use std::path::PathBuf;

use bigtext::decompress::Member;
use bigtext::jobs::human_bytes;

use super::filter_panel::fmt_count;
use super::i18n::{tr, trf};
use super::BigTextApp;

/// The list of an archive's files waiting for a pick; `doc` is the loading tab.
pub struct ArchivePicker {
    pub doc: u64,
    pub archive: PathBuf,
    pub members: Vec<Member>,
    pub filter: String,
    pub selected: Option<usize>,
    pub focus: bool,
}

fn date(t: std::time::SystemTime) -> String {
    let Ok(d) = t.duration_since(std::time::UNIX_EPOCH) else { return String::new() };
    let (days, secs) = ((d.as_secs() / 86_400) as i64, d.as_secs() % 86_400);
    // Civil date from days since 1970-01-01.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year}-{month:02}-{day:02} {:02}:{:02}", secs / 3600, secs % 3600 / 60)
}

impl BigTextApp {
    pub(super) fn on_archive_listed(&mut self, doc: u64, archive: PathBuf, members: Vec<Member>) {
        let Some(d) = self.docs.iter_mut().find(|d| d.id == doc) else { return };
        d.unpacking = None;
        self.archive_picker = Some(ArchivePicker { doc, archive, members, filter: String::new(), selected: None, focus: true });
    }

    /// Unpacks the archive's file `member` into the picker's tab.
    pub(crate) fn pick_archive_member(&mut self, member: &str) {
        let Some(p) = self.archive_picker.take() else { return };
        let Some(d) = self.docs.iter_mut().find(|d| d.id == p.doc) else { return };
        let archive_name = p.archive.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        d.title = format!("{archive_name} › {}", bigtext::decompress::member_file_name(member));
        d.member = Some(member.to_string());
        let size = p.members.iter().find(|m| m.name == member).map(|m| m.size);
        self.spawn_open_job(p.doc, p.archive, Some(member.to_string()), None, size, true);
    }

    /// Help → Unpacked copies folder…: where unpacked copies and other temporary files go.
    pub(super) fn unpack_dir_window(&mut self, ctx: &egui::Context) {
        if !self.unpack_dir_open {
            return;
        }
        let mut open = true;
        let mut set: Option<Option<PathBuf>> = None;
        egui::Window::new(tr("Unpacked copies folder")).open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
            ui.label(tr("Compressed files and archive members are unpacked into temporary copies here; joins, pivots and comparisons keep their temporary files here too."));
            let shown = bigtext::file::display_path(&bigtext::decompress::temp_dir());
            ui.monospace(shown);
            if self.settings.unpack_dir.is_none() {
                ui.weak(tr("(the system temp folder)"));
            }
            ui.horizontal(|ui| {
                if ui.button(tr("Choose folder…")).clicked() {
                    if let Some(dir) = rfd::FileDialog::new().set_title(tr("Folder for unpacked copies")).pick_folder() {
                        set = Some(Some(dir));
                    }
                }
                if self.settings.unpack_dir.is_some() && ui.button(tr("Use the system temp folder")).clicked() {
                    set = Some(None);
                }
            });
        });
        if let Some(dir) = set {
            self.set_unpack_folder(dir);
        }
        if !open {
            self.unpack_dir_open = false;
        }
    }

    pub(super) fn archive_window(&mut self, ctx: &egui::Context) {
        let filter_id = egui::Id::new("archive-filter");
        let Some(p) = self.archive_picker.as_mut() else {
            // The text view takes the keys back once the picker is gone.
            if ctx.memory(|m| m.has_focus(filter_id)) {
                ctx.memory_mut(|m| m.surrender_focus(filter_id));
            }
            return;
        };
        let mut open = true;
        let mut pick: Option<String> = None;
        let name = p.archive.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        egui::Window::new(trf("Open a file from {archive}", &[("archive", &name)])).open(&mut open).collapsible(false).default_size([560.0, 420.0]).show(ctx, |ui| {
            let filter = p.filter.to_lowercase();
            let shown: Vec<usize> = (0..p.members.len()).filter(|&i| filter.is_empty() || p.members[i].name.to_lowercase().contains(&filter)).collect();
            ui.horizontal(|ui| {
                let resp = ui.add(egui::TextEdit::singleline(&mut p.filter).id(filter_id).hint_text(tr("filter by name")).desired_width(260.0));
                if std::mem::take(&mut p.focus) {
                    resp.request_focus();
                }
                ui.weak(trf("{n} of {total} files", &[("n", &fmt_count(shown.len() as u64)), ("total", &fmt_count(p.members.len() as u64))]));
            });
            if p.members.len() >= bigtext::decompress::MEMBERS_MAX {
                ui.weak(trf("Only the first {n} files are listed.", &[("n", &fmt_count(bigtext::decompress::MEMBERS_MAX as u64))]));
            }
            // Arrow keys move the selection, Enter opens it.
            let (down, up, enter) = ui.input(|i| (i.key_pressed(egui::Key::ArrowDown), i.key_pressed(egui::Key::ArrowUp), i.key_pressed(egui::Key::Enter)));
            let at = p.selected.and_then(|s| shown.iter().position(|&i| i == s));
            if down || up {
                let next = match (at, down) {
                    (None, _) => 0,
                    (Some(k), true) => (k + 1).min(shown.len().saturating_sub(1)),
                    (Some(k), false) => k.saturating_sub(1),
                };
                p.selected = shown.get(next).copied();
            }
            if enter {
                let i = p.selected.filter(|s| shown.contains(s)).or(shown.first().copied());
                pick = i.map(|i| p.members[i].name.clone());
            }
            ui.separator();
            let row_h = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
            egui::ScrollArea::vertical().auto_shrink([false, false]).max_height(320.0).show_rows(ui, row_h, shown.len(), |ui, range| {
                for &i in &shown[range] {
                    let m = &p.members[i];
                    ui.horizontal(|ui| {
                        let r = ui.selectable_label(p.selected == Some(i), &m.name);
                        if r.clicked() {
                            p.selected = Some(i);
                        }
                        if r.double_clicked() {
                            pick = Some(m.name.clone());
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if let Some(t) = m.modified {
                                ui.weak(date(t));
                            }
                            ui.label(human_bytes(m.size));
                        });
                    });
                }
            });
            ui.separator();
            ui.horizontal(|ui| {
                let chosen = p.selected.map(|i| p.members[i].name.clone());
                if ui.add_enabled(chosen.is_some(), egui::Button::new(tr("Open"))).clicked() {
                    pick = chosen;
                }
                ui.weak(tr("A compressed file inside (.gz, .zst, .xz, .bz2) is unpacked too."));
            });
        });
        let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        if let Some(m) = pick {
            self.pick_archive_member(&m);
        } else if !open || escape {
            // Nothing picked: the loading tab goes too.
            if let Some(p) = self.archive_picker.take() {
                self.close_tab(p.doc, true);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn dates() {
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        assert_eq!(super::date(t), "2023-11-14 22:13");
    }
}
