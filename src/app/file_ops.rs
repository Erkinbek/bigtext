//! The side bar's file operations: New file…, New folder…, Rename… and moving a file or folder
//! to the Recycle Bin (the Trash on macOS and Linux), so a mistake can be undone there. Tabs of
//! the files involved are closed first (reopened after a rename); a file with unsaved changes
//! stops the operation.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};

use bigtext::file::display_path;
use egui::Key;

use super::i18n::{tr, trf};
use super::BigTextApp;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameKind {
    NewFile,
    NewFolder,
    Rename,
}

/// The dialog asking for a name.
pub struct NameDialog {
    pub kind: NameKind,
    /// The folder to create in, or the file or folder to rename.
    pub base: PathBuf,
    pub name: String,
    pub error: Option<String>,
    focus: bool,
}

pub struct FileOps {
    pub dialog: Option<NameDialog>,
    tx: Sender<(PathBuf, Result<(), String>)>,
    rx: Receiver<(PathBuf, Result<(), String>)>,
    /// Moves to the trash still running.
    pub trashing: usize,
}

impl Default for FileOps {
    fn default() -> Self {
        let (tx, rx) = channel();
        FileOps { dialog: None, tx, rx, trashing: 0 }
    }
}

/// Why `name` cannot be a file name here, if it cannot.
pub fn bad_name(name: &str) -> Option<&'static str> {
    let n = name.trim();
    if n.is_empty() {
        return Some("Type a name.");
    }
    if n == "." || n == ".." || n.contains(['/', '\\']) || (cfg!(windows) && n.contains(['<', '>', ':', '"', '|', '?', '*'])) {
        return Some("This name cannot be used for a file.");
    }
    None
}

/// "the Recycle Bin" or "the Trash", as this system calls it.
pub fn trash_label() -> &'static str {
    if cfg!(windows) {
        tr("Move to the Recycle Bin")
    } else {
        tr("Move to the Trash")
    }
}

/// `p` is `root` or inside it (with or without the long-path prefix).
fn under(p: &Path, root: &Path) -> bool {
    let (p, root) = (PathBuf::from(display_path(p)), PathBuf::from(display_path(root)));
    p == root || p.starts_with(&root)
}

impl BigTextApp {
    pub(crate) fn ask_name(&mut self, kind: NameKind, base: PathBuf) {
        let name = if kind == NameKind::Rename { base.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default() } else { String::new() };
        self.file_ops.dialog = Some(NameDialog { kind, base, name, error: None, focus: true });
    }

    /// The tabs of files at or under `path`; an error naming one with unsaved changes.
    fn tabs_under(&self, path: &Path) -> Result<Vec<(u64, PathBuf)>, String> {
        let mut v = Vec::new();
        for d in &self.docs {
            let Some(p) = d.path.as_deref().filter(|p| under(p, path)) else { continue };
            if d.is_modified() {
                return Err(trf("Save or close “{name}” first: it has unsaved changes.", &[("name", &d.title)]));
            }
            v.push((d.id, p.to_path_buf()));
        }
        Ok(v)
    }

    /// The side bar shows `dir` again with what changed in it.
    fn reread(&mut self, dir: &Path) {
        self.folder.expanded.insert(dir.to_path_buf());
        self.refresh_folder();
    }

    fn apply_name(&mut self) -> Result<(), String> {
        let Some(d) = &self.file_ops.dialog else { return Ok(()) };
        let (kind, base, name) = (d.kind, d.base.clone(), d.name.trim().to_string());
        if let Some(e) = bad_name(&name) {
            return Err(tr(e).into());
        }
        let parent = if kind == NameKind::Rename { base.parent().map(Path::to_path_buf).unwrap_or_default() } else { base.clone() };
        let target = parent.join(&name);
        // A rename that only changes the case of letters is fine on case-insensitive disks.
        let same = kind == NameKind::Rename && base.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(&name));
        if target.exists() && !same {
            return Err(tr("A file or folder with this name is already there.").into());
        }
        let io = |e: std::io::Error| super::friendly_io(&e, &target);
        match kind {
            NameKind::NewFile => {
                std::fs::OpenOptions::new().write(true).create_new(true).open(&target).map_err(io)?;
                self.reread(&parent);
                self.open_path(target);
            }
            NameKind::NewFolder => {
                std::fs::create_dir(&target).map_err(io)?;
                self.reread(&parent);
                self.folder.expanded.insert(target);
            }
            NameKind::Rename => {
                let tabs = self.tabs_under(&base)?;
                for (id, _) in &tabs {
                    self.close_tab(*id, true);
                }
                if let Err(e) = std::fs::rename(&base, &target) {
                    for (_, p) in tabs {
                        self.open_path(p);
                    }
                    return Err(io(e));
                }
                // Expanded folders inside a renamed folder stay expanded.
                let moved: Vec<PathBuf> = self.folder.expanded.iter().filter(|p| under(p, &base)).cloned().collect();
                for p in moved {
                    self.folder.expanded.remove(&p);
                    if let Ok(rest) = p.strip_prefix(&base) {
                        self.folder.expanded.insert(target.join(rest));
                    }
                }
                for (_, p) in tabs {
                    let rest = p.strip_prefix(&base).map(Path::to_path_buf).unwrap_or_default();
                    self.open_path(if rest.as_os_str().is_empty() { target.clone() } else { target.join(rest) });
                }
                self.reread(&parent);
            }
        }
        self.persist_state();
        Ok(())
    }

    /// Asks before moving `path` to the trash.
    pub(crate) fn ask_trash(&mut self, path: PathBuf) {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| display_path(&path));
        let message = if cfg!(windows) { trf("Move “{name}” to the Recycle Bin? It can be restored from there.", &[("name", &name)]) } else { trf("Move “{name}” to the Trash? It can be put back from there.", &[("name", &name)]) };
        self.confirm = Some(super::Confirm { title: trash_label().into(), message, action: super::ConfirmAction::Trash(path), checkbox: None });
    }

    /// Moves `path` to the trash on a worker, its tabs closed first.
    pub(crate) fn trash_now(&mut self, path: PathBuf) {
        let tabs = match self.tabs_under(&path) {
            Ok(t) => t,
            Err(e) => {
                self.error = Some(e);
                return;
            }
        };
        for (id, _) in tabs {
            self.close_tab(id, true);
        }
        let (tx, waker) = (self.file_ops.tx.clone(), self.waker.clone());
        self.file_ops.trashing += 1;
        bigtext::jobs::spawn("trash", move || {
            let r = super::shell::move_to_trash(&path);
            let _ = tx.send((path, r));
            waker();
        });
    }

    /// Takes finished moves to the trash; draws the name dialog.
    pub(super) fn file_ops_window(&mut self, ctx: &egui::Context) {
        while let Ok((path, r)) = self.file_ops.rx.try_recv() {
            self.file_ops.trashing = self.file_ops.trashing.saturating_sub(1);
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            match r {
                Ok(()) => {
                    let text = if cfg!(windows) { trf("Moved {name} to the Recycle Bin.", &[("name", &name)]) } else { trf("Moved {name} to the Trash.", &[("name", &name)]) };
                    self.notify(text, None);
                    self.folder.expanded.retain(|p| !under(p, &path));
                    if let Some(parent) = path.parent() {
                        self.reread(&parent.to_path_buf());
                    }
                }
                Err(e) => self.error = Some(format!("{}: {e}", trf("Could not move {name}", &[("name", &name)]))),
            }
        }
        let Some(d) = &mut self.file_ops.dialog else { return };
        let title = match d.kind {
            NameKind::NewFile => tr("New file"),
            NameKind::NewFolder => tr("New folder"),
            NameKind::Rename => tr("Rename"),
        };
        let (mut ok, mut cancel) = (false, false);
        egui::Modal::new(egui::Id::new("name-dialog")).show(ctx, |ui| {
            ui.set_width(380.0);
            ui.heading(title);
            ui.weak(display_path(if d.kind == NameKind::Rename { d.base.parent().unwrap_or(&d.base) } else { &d.base }));
            let r = ui.add(egui::TextEdit::singleline(&mut d.name).hint_text(tr("Name")).desired_width(f32::INFINITY));
            if d.focus {
                r.request_focus();
                // A rename selects the name without its extension.
                if d.kind == NameKind::Rename {
                    let stem = d.name.rfind('.').filter(|&i| i > 0).unwrap_or(d.name.len());
                    let mut state = egui::TextEdit::load_state(ui.ctx(), r.id).unwrap_or_default();
                    state.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(d.name[..stem].chars().count()))));
                    state.store(ui.ctx(), r.id);
                }
                d.focus = false;
            }
            if r.changed() {
                d.error = None;
            }
            if let Some(e) = &d.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let label = if d.kind == NameKind::Rename { tr("Rename") } else { tr("Create") };
                ok = ui.button(label).clicked() || ui.input(|i| i.key_pressed(Key::Enter));
                cancel = ui.button(tr("Cancel")).clicked() || ui.input(|i| i.key_pressed(Key::Escape));
            });
        });
        if cancel {
            self.file_ops.dialog = None;
        } else if ok {
            match self.apply_name() {
                Ok(()) => self.file_ops.dialog = None,
                Err(e) => {
                    if let Some(d) = &mut self.file_ops.dialog {
                        d.error = Some(e);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_that_cannot_be_files() {
        assert_eq!(bad_name("  "), Some("Type a name."));
        assert!(bad_name("a/b").is_some() && bad_name("..").is_some());
        assert_eq!(bad_name("notes.txt"), None);
        assert_eq!(bad_name("a:b").is_some(), cfg!(windows));
    }

    #[test]
    fn moves_a_file_to_the_trash() {
        // Only where a trash surely exists: on a desktop, not on a CI runner without one.
        if std::env::var_os("BIGTEXT_TRASH_TEST").is_none() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("gone.txt");
        std::fs::write(&f, "x").unwrap();
        super::super::shell::move_to_trash(&f).unwrap();
        assert!(!f.exists());
    }
}
