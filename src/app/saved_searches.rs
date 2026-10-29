//! Saved searches: a pattern with its options, the "!" (lines without) switch and the table
//! column, kept under a name such as "Timeouts" or "5xx responses" and applied as a filter with
//! one click from the ★ menu of the search bar.

use egui::RichText;



use super::i18n::{tr, trf};
use super::BigTextApp;

pub use bigtext::settings_file::SavedSearch;

#[derive(Default)]
pub struct SavedUi {
    pub new_name: String,
}

impl BigTextApp {
    /// Saves the search bar's search under `name` (replacing one of that name).
    pub(crate) fn save_search(&mut self, name: &str) {
        let name = name.trim().replace(['|', '\n', '\r'], " ");
        if name.is_empty() || self.search.query.pattern.is_empty() || self.search.query.pattern.contains(['\n', '\r']) {
            return;
        }
        let s = SavedSearch { name: name.clone(), query: self.search.query.clone(), invert: self.search.invert, column: self.search.filter_column };
        match self.settings.saved.iter_mut().find(|x| x.name == name) {
            Some(x) => *x = s,
            None => self.settings.saved.push(s),
        }
        self.persist_state();
    }

    /// Puts saved search `i` into the search bar and filters the active document with it.
    pub(crate) fn apply_saved(&mut self, i: usize) {
        let Some(s) = self.settings.saved.get(i).cloned() else { return };
        self.search.query = s.query;
        self.search.invert = s.invert;
        self.search.filter_column = s.column;
        self.search.open = true;
        self.start_filter();
    }

    /// The ★ menu: save the current search, apply or delete saved ones.
    pub(super) fn saved_menu(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(260.0);
        let mut apply = None;
        let mut delete = None;
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.saved_ui.new_name).desired_width(150.0).hint_text(tr("Name")));
            let ok = !self.saved_ui.new_name.trim().is_empty() && !self.search.query.pattern.is_empty();
            if ui.add_enabled(ok, egui::Button::new(tr("Save search"))).on_hover_text(tr("Saves the pattern with its options, “!” and the column")).clicked() {
                let name = std::mem::take(&mut self.saved_ui.new_name);
                self.save_search(&name);
            }
        });
        if !self.settings.saved.is_empty() {
            ui.separator();
        }
        for (i, s) in self.settings.saved.iter().enumerate() {
            ui.horizontal(|ui| {
                let mut tip = format!("{}{}", if s.invert { "! " } else { "" }, s.query.pattern);
                if let Some(c) = s.column {
                    tip.push_str(&format!("  ({})", trf("in column {n}", &[("n", &(c + 1).to_string())])));
                }
                if ui.button(&s.name).on_hover_text(RichText::new(tip).monospace()).clicked() {
                    apply = Some(i);
                }
                if ui.small_button("×").on_hover_text(tr("Delete")).clicked() {
                    delete = Some(i);
                }
            });
        }
        if let Some(i) = apply {
            ui.close();
            self.apply_saved(i);
        }
        if let Some(i) = delete {
            self.settings.saved.remove(i);
            self.persist_state();
        }
    }
}
