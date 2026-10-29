//! Highlights ("marks"): patterns painted in their own colours in every document, kept across
//! sessions — `ERROR` in red, a request id in blue, and so on.

use std::sync::Arc;

use egui::{Color32, RichText};

use bigtext::encoding::Codec;
use bigtext::search::{Matcher, Query};

use super::i18n::tr;
use super::BigTextApp;

#[derive(Clone, Debug, PartialEq)]
pub struct Mark {
    pub query: Query,
    /// Index into [`COLORS`].
    pub color: usize,
    pub enabled: bool,
}

pub const COLORS: [(u8, u8, u8); 8] = [
    (230, 70, 70),
    (60, 180, 80),
    (60, 130, 240),
    (240, 150, 30),
    (170, 90, 230),
    (30, 185, 200),
    (230, 80, 170),
    (170, 170, 40),
];

pub fn fill(color: usize, dark: bool) -> Color32 {
    let (r, g, b) = super::theme::mark_rgb(color, &COLORS);
    Color32::from_rgba_unmultiplied(r, g, b, super::theme::mark_alpha(dark))
}

pub fn solid(color: usize) -> Color32 {
    let (r, g, b) = super::theme::mark_rgb(color, &COLORS);
    Color32::from_rgb(r, g, b)
}

/// Compiled marks with their colour indexes.
pub type MarkMatchers = Vec<(Arc<Matcher>, usize)>;

#[derive(Default)]
pub struct MarksUi {
    pub open: bool,
    pub new_pattern: String,
    pub new_regex: bool,
    pub new_case: bool,
    /// Compiled matchers for the enabled marks, for one encoding.
    cache: Option<(Vec<Mark>, Codec, MarkMatchers)>,
}

impl BigTextApp {
    /// Adds the search box's pattern (with its options) as a new mark.
    pub(crate) fn add_mark_from_search(&mut self) {
        let q = self.search.query.clone();
        if q.pattern.is_empty() {
            self.search.status = tr("Type a pattern first").into();
            return;
        }
        if let Err(e) = Matcher::new(&q, Codec::utf8()) {
            self.search.status = e;
            return;
        }
        self.add_mark(q);
    }

    pub(crate) fn add_mark(&mut self, query: Query) {
        if let Some(m) = self.settings.marks.iter_mut().find(|m| m.query == query) {
            m.enabled = true;
        } else {
            let used: Vec<usize> = self.settings.marks.iter().map(|m| m.color).collect();
            let color = (0..COLORS.len()).find(|c| !used.contains(c)).unwrap_or(self.settings.marks.len() % COLORS.len());
            self.settings.marks.push(Mark { query, color, enabled: true });
        }
        self.persist_state();
    }

    /// Matchers of the enabled marks for documents in `codec` (with their colours).
    pub(super) fn mark_matchers(&mut self, codec: Codec) -> MarkMatchers {
        if let Some((marks, c, v)) = &self.marks.cache {
            if *marks == self.settings.marks && *c == codec {
                return v.clone();
            }
        }
        let v: MarkMatchers =
            self.settings.marks.iter().filter(|m| m.enabled).filter_map(|m| Matcher::new(&m.query, codec).ok().map(|x| (Arc::new(x), m.color))).collect();
        self.marks.cache = Some((self.settings.marks.clone(), codec, v.clone()));
        v
    }

    pub(super) fn marks_window(&mut self, ctx: &egui::Context) {
        if !self.marks.open {
            return;
        }
        let mut open = true;
        let mut changed = false;
        let mut remove = None;
        let mut add = None;
        egui::Window::new(tr("Highlights")).open(&mut open).default_width(460.0).show(ctx, |ui| {
            ui.label(tr("Patterns painted in their own colour in every document. Click a colour to change it."));
            ui.add_space(4.0);
            egui::Grid::new("marks").num_columns(4).striped(true).show(ui, |ui| {
                for (i, m) in self.settings.marks.iter_mut().enumerate() {
                    changed |= ui.checkbox(&mut m.enabled, "").changed();
                    let (rect, r) = ui.allocate_exact_size(egui::vec2(28.0, 16.0), egui::Sense::click());
                    ui.painter().rect_filled(rect, 3.0, solid(m.color));
                    if r.on_hover_text(tr("Next colour")).clicked() {
                        m.color = (m.color + 1) % COLORS.len();
                        changed = true;
                    }
                    let mut flags = String::new();
                    if m.query.regex {
                        flags.push_str(" .*");
                    }
                    if m.query.case_sensitive {
                        flags.push_str(" Aa");
                    }
                    if m.query.whole_word {
                        flags.push_str(" W");
                    }
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&m.query.pattern).monospace());
                        ui.weak(flags);
                    });
                    if ui.small_button("×").clicked() {
                        remove = Some(i);
                    }
                    ui.end_row();
                }
            });
            if self.settings.marks.is_empty() {
                ui.weak(tr("No highlights yet. Add one below, or use Mark in the search bar."));
            }
            ui.separator();
            ui.horizontal(|ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut self.marks.new_pattern).hint_text(tr("text or regex")).desired_width(220.0).font(egui::TextStyle::Monospace));
                ui.toggle_value(&mut self.marks.new_case, RichText::new("Aa").monospace()).on_hover_text(tr("Case sensitive"));
                ui.toggle_value(&mut self.marks.new_regex, RichText::new(".*").monospace()).on_hover_text(tr("Regular expression (linear time)"));
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (ui.button(tr("Add")).clicked() || enter) && !self.marks.new_pattern.is_empty() {
                    add = Some(Query { pattern: self.marks.new_pattern.clone(), regex: self.marks.new_regex, case_sensitive: self.marks.new_case, ..Default::default() });
                }
            });
            if let Some(q) = &add {
                if let Err(e) = Matcher::new(q, Codec::utf8()) {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                    add = None;
                }
            }
            if !self.settings.marks.is_empty() && ui.button(tr("Remove all")).clicked() {
                self.settings.marks.clear();
                changed = true;
            }
        });
        if let Some(i) = remove {
            self.settings.marks.remove(i);
            changed = true;
        }
        if let Some(q) = add {
            self.marks.new_pattern.clear();
            self.add_mark(q);
        }
        if changed {
            self.persist_state();
        }
        self.marks.open = open;
    }
}
