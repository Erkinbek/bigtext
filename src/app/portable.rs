//! Portable mode and moving settings between computers.
//!
//! With a `BigText-data` folder next to the program, the settings, recent files and sessions are
//! kept there instead of the user's profile, so BigText runs from a USB stick with its settings.
//! Help → Export settings / Import settings write and read the settings that are not about
//! one computer's files: highlights, key bindings, saved searches, theme, font, language.

use std::path::{Path, PathBuf};

use super::commands::{key_from_string, Cmd};
use super::extras::{self, Persisted};
use super::i18n::{self, tr, trf};
use super::BigTextApp;

/// Name of the folder that turns portable mode on.
pub const PORTABLE_DIR: &str = "BigText-data";

#[derive(Default)]
pub struct PortableUi {
    pub open: bool,
}

/// The settings folder next to the program, when there is one.
pub fn portable_dir() -> Option<PathBuf> {
    bigtext::settings_file::portable_dir()
}

/// The settings to carry to another computer (no files, tabs or update state).
fn shareable(p: &Persisted) -> Persisted {
    Persisted {
        font_size: p.font_size,
        dark: p.dark,
        theme: p.theme,
        cvd: p.cvd,
        scale: p.scale,
        backup_on_save: p.backup_on_save,
        syntax: p.syntax,
        marks: p.marks.clone(),
        keys: p.keys.clone(),
        lang: p.lang.clone(),
        font: p.font.clone(),
        level_bar: p.level_bar,
        saved: p.saved.clone(),
        show_ws: p.show_ws,
        status: p.status,
        gutter: p.gutter,
        wrap_column: p.wrap_column,
        ruler: p.ruler,
        ..Default::default()
    }
}

impl BigTextApp {
    pub(crate) fn export_settings_to(&self, path: &Path) -> Result<(), String> {
        let text = extras::serialize(&shareable(&self.persisted())).replace("# BigText session state", "# BigText settings");
        std::fs::write(path, text).map_err(|e| super::friendly_io(&e, path))
    }

    /// Takes the settings in `path`: highlights and saved searches are added (a saved search
    /// of the same name is replaced), everything else replaces the current value.
    pub(crate) fn import_settings_from(&mut self, path: &Path, ctx: Option<&egui::Context>) -> Result<(), String> {
        let text = std::fs::read_to_string(path).map_err(|e| super::friendly_io(&e, path))?;
        let p = extras::parse(&text);
        self.restore_look(&p);
        if let Some(ctx) = ctx {
            self.apply_look(ctx);
        }
        let s = &mut self.settings;
        if let Some(v) = p.font_size {
            s.font_size = v.clamp(6.0, 72.0);
        }
        if let Some(v) = p.backup_on_save {
            s.backup_on_save = v;
        }
        if let Some(v) = p.syntax {
            s.syntax = v;
        }
        if let Some(v) = p.level_bar {
            s.level_bar = v;
        }
        if let Some(v) = p.show_ws {
            s.show_ws = v;
        }
        if let Some(v) = p.status {
            s.status = v;
        }
        if let Some(v) = p.gutter {
            s.show_gutter = v;
        }
        if let Some(v) = p.wrap_column {
            s.wrap_column = v;
        }
        if let Some(v) = p.ruler {
            s.ruler = v;
        }
        for m in p.marks {
            if !s.marks.iter().any(|x| x.query == m.query) {
                s.marks.push(m);
            }
        }
        for x in p.saved {
            match s.saved.iter_mut().find(|y| y.name == x.name) {
                Some(y) => *y = x,
                None => s.saved.push(x),
            }
        }
        if !p.keys.is_empty() {
            self.keymap.changed.clear();
            for (id, k) in p.keys {
                if let (Some(c), Some(k)) = (Cmd::from_id(&id), key_from_string(&k)) {
                    self.keymap.changed.insert(c, k);
                }
            }
        }
        if let Some(l) = p.lang.as_deref().and_then(i18n::Lang::from_code) {
            i18n::set_lang(l);
        }
        // A font that is not on this computer is skipped.
        if let (Some(f), Some(ctx)) = (p.font, ctx) {
            if super::fonts::apply_font(ctx, Some(&f)).is_ok() {
                self.settings.font_path = Some(f);
            }
        }
        self.persist_state();
        Ok(())
    }

    pub(crate) fn export_settings(&mut self) {
        let Some(path) = rfd::FileDialog::new().set_title(tr("Export settings")).set_file_name("bigtext.settings").save_file() else { return };
        match self.export_settings_to(&path) {
            Ok(()) => self.notify(trf("Settings saved to {file}.", &[("file", &super::filter_panel::file_name(&path))]), None),
            Err(e) => self.error = Some(e),
        }
    }

    pub(crate) fn import_settings(&mut self, ctx: &egui::Context) {
        let Some(path) = rfd::FileDialog::new().set_title(tr("Import settings")).add_filter("BigText settings", &["settings", "txt"]).pick_file() else { return };
        match self.import_settings_from(&path, Some(ctx)) {
            Ok(()) => self.notify(tr("Settings imported.").into(), None),
            Err(e) => self.error = Some(e),
        }
    }

    /// Creates the portable settings folder next to the program and saves the settings there.
    fn make_portable(&mut self) -> Result<PathBuf, String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let dir = exe.parent().ok_or("no program folder")?.join(PORTABLE_DIR);
        let old_sessions = super::sessions::sessions_dir();
        std::fs::create_dir_all(&dir).map_err(|e| super::friendly_io(&e, &dir))?;
        extras::save_now(self.persisted());
        // The saved sessions come along.
        if let (Some(from), Some(to)) = (old_sessions, super::sessions::sessions_dir()) {
            if from != to {
                if let Ok(entries) = std::fs::read_dir(&from) {
                    let _ = std::fs::create_dir_all(&to);
                    for e in entries.flatten() {
                        let _ = std::fs::copy(e.path(), to.join(e.file_name()));
                    }
                }
            }
        }
        Ok(dir)
    }

    pub(super) fn portable_window(&mut self, ctx: &egui::Context) {
        if !self.portable.open {
            return;
        }
        let mut open = true;
        let mut make = false;
        egui::Window::new(tr("Portable mode")).collapsible(false).resizable(false).open(&mut open).show(ctx, |ui| {
            ui.set_max_width(440.0);
            match portable_dir() {
                Some(d) => {
                    ui.label(tr("BigText is portable: its settings, recent files and sessions are kept in"));
                    ui.monospace(bigtext::file::display_path(&d));
                    ui.weak(tr("Delete that folder (with BigText closed) to keep the settings in your user profile again."));
                }
                None => {
                    ui.label(tr("Keep the settings, recent files and sessions next to bigtext.exe instead of your user profile, to run BigText with its settings from a USB stick."));
                    ui.weak(trf("This creates a “{dir}” folder next to the program.", &[("dir", PORTABLE_DIR)]));
                    make = ui.button(tr("Make portable")).clicked();
                }
            }
        });
        if make {
            match self.make_portable() {
                Ok(_) => self.notify(tr("BigText is portable now.").into(), None),
                Err(e) => self.error = Some(e),
            }
        }
        if !open {
            self.portable.open = false;
        }
    }
}
