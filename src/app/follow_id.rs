//! Following an id: the request, trace or session ids on the caret's line as status-bar chips,
//! and the file filtered to one id (whole log entries) with when it was first and last seen and
//! how many of its lines are errors.

use std::sync::Arc;

use bigtext::correlate::{FoundId, IdFinder, DEFAULT_KEYS};
use bigtext::filter::FilterSpec;
use bigtext::search::{Matcher, Query};
use bigtext::timestamp::{self, Stamp};

use super::i18n::{tr, trf};
use super::BigTextApp;

/// Chips shown in the status bar.
const CHIPS: usize = 4;
/// Bytes of the caret's line searched for ids.
const LINE_MAX: u64 = 16 * 1024;

#[derive(Default)]
pub struct IdUi {
    /// Ids of the caret's line, for (document, version, line start).
    cache: Option<((u64, u64, u64), Vec<FoundId>)>,
    finder: Option<(Vec<String>, Arc<IdFinder>)>,
    /// The text of the Id patterns window while it is open.
    pub editor: Option<String>,
}

/// What the filter of one id found besides its lines.
#[derive(Clone, Copy, Debug, Default)]
pub struct FollowSummary {
    pub first: Option<Stamp>,
    pub last: Option<Stamp>,
    pub errors: usize,
}

impl FollowSummary {
    /// "first 10:00:01 · last 10:00:04 · 3.2 s · 2 errors".
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        let key = |s: Stamp| timestamp::format_key(s.ms, s.has_date, true);
        if let (Some(a), Some(b)) = (self.first, self.last) {
            parts.push(trf("first {time}", &[("time", &key(a))]));
            parts.push(trf("last {time}", &[("time", &key(b))]));
            let ms = if a.tz.is_some() && b.tz.is_some() { b.utc_ms() - a.utc_ms() } else { b.ms - a.ms };
            parts.push(timestamp::format_delta(ms).trim_start_matches('+').to_string());
        }
        parts.push(match self.errors {
            0 => tr("no errors").to_string(),
            n => trf("{n} errors", &[("n", &n.to_string())]),
        });
        parts.join(" · ")
    }
}

impl BigTextApp {
    /// The keys whose values are ids (Search → Id patterns…).
    fn id_finder(&mut self) -> Arc<IdFinder> {
        let keys = self.settings.id_keys.clone().unwrap_or_else(|| DEFAULT_KEYS.iter().map(|k| k.to_string()).collect());
        if let Some((k, f)) = &self.ids.finder {
            if *k == keys {
                return f.clone();
            }
        }
        let f = Arc::new(IdFinder::new(&keys));
        self.ids.finder = Some((keys, f.clone()));
        self.ids.cache = None;
        f
    }

    /// The ids on the caret's line (cached while the caret stays on it).
    pub(crate) fn caret_ids(&mut self) -> Vec<FoundId> {
        let finder = self.id_finder();
        let Some(d) = self.docs.get(self.active) else { return Vec::new() };
        if !d.is_ready() || d.hex {
            return Vec::new();
        }
        let Some((ls, le, _)) = d.line_bounds(d.view.caret, 64 * 1024) else { return Vec::new() };
        let key = (d.id, d.version, ls);
        if let Some((k, v)) = &self.ids.cache {
            if *k == key {
                return v.clone();
            }
        }
        let Some(b) = d.buf.read(ls.max(d.buf.codec.bom_len as u64)..le.min(ls + LINE_MAX)) else { return Vec::new() };
        let ids = finder.find(&d.buf.codec.decode_lossy(&b), CHIPS);
        self.ids.cache = Some((key, ids.clone()));
        ids
    }

    /// Filters the active document to the lines with `id` (whole log entries).
    pub(crate) fn follow_id(&mut self, id: &str) {
        let Some(d) = self.docs.get(self.active) else { return };
        if !d.is_ready() || id.is_empty() {
            return;
        }
        let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
        let q = Query { pattern: id.to_string(), regex: false, case_sensitive: true, whole_word: word(id.chars().next()) && word(id.chars().last()), dot_all: false };
        let m = match Matcher::new(&q, d.buf.codec) {
            Ok(m) => Arc::new(m),
            Err(e) => {
                self.error = Some(e);
                return;
            }
        };
        self.search.query = q.clone();
        self.search.invert = false;
        self.search.remember();
        let spec = FilterSpec { entries: true, ..Default::default() };
        let req = super::filter_panel::FilterRequest { query: q, m, spec, levels: None, follow: Some(id.to_string()), terms: Vec::new() };
        self.run_filter_req(req, None);
    }

    /// Follows the selected text, or else the first id on the caret's line.
    pub(crate) fn follow_id_at_caret(&mut self) {
        let Some(d) = self.docs.get(self.active) else { return };
        let (a, b) = d.selection();
        if b > a && b - a <= 256 {
            if let Some(t) = d.buf.read(a..b).map(|x| d.buf.codec.decode_lossy(&x)) {
                let t = t.trim();
                if !t.is_empty() && !t.contains(['\n', '\r']) {
                    let t = t.to_string();
                    self.follow_id(&t);
                    return;
                }
            }
        }
        match self.caret_ids().into_iter().next() {
            Some(f) => self.follow_id(&f.value),
            None => self.notify(tr("No id on this line (select one, or add its key in Search → Id patterns…)").into(), None),
        }
    }

    /// Search → Id patterns…: the keys whose values are ids, one per line.
    pub(super) fn id_patterns_window(&mut self, ctx: &egui::Context) {
        let Some(text) = self.ids.editor.as_mut() else { return };
        let mut open = true;
        let (mut apply, mut reset) = (false, false);
        egui::Window::new(tr("Id patterns")).open(&mut open).collapsible(false).resizable(true).default_width(320.0).show(ctx, |ui| {
            ui.label(tr("Keys whose values are ids, one per line (key=value, key: value, \"key\": \"value\"). UUIDs count when no key matches."));
            egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                ui.add(egui::TextEdit::multiline(text).desired_rows(12).desired_width(f32::INFINITY).font(egui::TextStyle::Monospace));
            });
            ui.horizontal(|ui| {
                apply = ui.button(tr("OK")).clicked();
                reset = ui.button(tr("Reset to defaults")).clicked();
            });
        });
        if reset {
            *text = DEFAULT_KEYS.join("\n");
        }
        if apply {
            let keys: Vec<String> = text.lines().map(str::trim).filter(|k| !k.is_empty() && !k.contains(',')).map(String::from).collect();
            let defaults: Vec<String> = DEFAULT_KEYS.iter().map(|k| k.to_string()).collect();
            self.settings.id_keys = (keys != defaults).then_some(keys);
            self.persist_state();
            open = false;
        }
        if !open {
            self.ids.editor = None;
        }
    }

    pub(crate) fn open_id_patterns(&mut self) {
        let keys = self.settings.id_keys.clone().unwrap_or_else(|| DEFAULT_KEYS.iter().map(|k| k.to_string()).collect());
        self.ids.editor = Some(keys.join("\n"));
    }
}

/// Status-bar chips for `ids`; returns the id clicked.
pub(super) fn id_chips(ui: &mut egui::Ui, ids: &[FoundId]) -> Option<String> {
    let mut clicked = None;
    for f in ids {
        let short: String = if f.value.chars().count() > 24 { f.value.chars().take(23).chain(['…']).collect() } else { f.value.clone() };
        let resp = ui.small_button(format!("{}: {short}", f.key)).on_hover_text(trf("Follow {id}: show only its lines and log entries (Ctrl+Shift+I)", &[("id", &f.value)]));
        if resp.clicked() {
            clicked = Some(f.value.clone());
        }
    }
    clicked
}
