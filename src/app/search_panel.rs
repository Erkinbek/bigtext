//! Find / replace panel, "Find All" results list and directed find-next/previous jobs.

use std::ops::Range;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, Key, RichText, TextEdit};

use bigtext::encoding::Codec;
use bigtext::jobs::{self, CancelToken, Progress, Throttle};
use bigtext::search::{self, Matcher, Query, SearchEnd};
use bigtext::text::{rows, TextBuf};

use super::i18n::{tr, trf};
use super::{BigTextApp, Msg};

/// Maximum number of results kept by "Find All".
pub const RESULT_CAP: usize = 1_000_000;
/// Patterns remembered in the search history.
pub const HISTORY_MAX: usize = 30;

#[derive(Default)]
pub struct ResultsData {
    pub matches: Vec<(u64, u64)>,
    pub capped: bool,
    pub done: bool,
    pub end: Option<SearchEnd>,
    pub elapsed: Option<Duration>,
}

pub struct SearchResults {
    pub query: Query,
    pub version: u64,
    pub range: Range<u64>,
    pub data: Arc<Mutex<ResultsData>>,
    pub cancel: CancelToken,
    pub progress: Progress,
}

impl SearchResults {
    pub fn is_running(&self) -> bool {
        !self.data.lock().unwrap().done
    }
}

pub struct FindJob {
    pub cancel: CancelToken,
}

#[derive(Default)]
pub struct SearchUi {
    pub open: bool,
    pub replace_open: bool,
    pub query: Query,
    pub replace_with: String,
    pub in_selection: bool,
    pub focus_find: bool,
    pub status: String,
    pub show_results: bool,
    /// Filter: keep the lines without a match.
    pub invert: bool,
    pub show_filter: bool,
    /// Filter a table by this column only (0-based).
    pub filter_column: Option<usize>,
    /// Recent patterns, newest first.
    pub history: Vec<String>,
    /// Position while stepping through the history with the arrow keys.
    history_pos: Option<usize>,
    compiled: Option<(Query, Codec, Result<Arc<Matcher>, String>)>,
}

impl SearchUi {
    /// Compiled matcher for the current query in the given encoding (cached).
    pub fn matcher(&mut self, codec: Codec) -> Result<Arc<Matcher>, String> {
        if let Some((q, c, r)) = &self.compiled {
            if *q == self.query && *c == codec {
                return r.clone();
            }
        }
        let r = Matcher::new(&self.query, codec).map(Arc::new);
        self.compiled = Some((self.query.clone(), codec, r.clone()));
        r
    }

    /// Puts the current pattern at the top of the history.
    pub fn remember(&mut self) {
        let p = self.query.pattern.clone();
        if p.is_empty() {
            return;
        }
        self.history.retain(|h| *h != p);
        self.history.insert(0, p);
        self.history.truncate(HISTORY_MAX);
        self.history_pos = None;
    }

    /// Up/Down in the find field: older/newer history entries.
    fn step_history(&mut self, older: bool) {
        if self.history.is_empty() {
            return;
        }
        let next = match (self.history_pos, older) {
            (None, true) => Some(0),
            (None, false) => None,
            (Some(i), true) => Some((i + 1).min(self.history.len() - 1)),
            (Some(0), false) => None,
            (Some(i), false) => Some(i - 1),
        };
        self.history_pos = next;
        self.query.pattern = next.map(|i| self.history[i].clone()).unwrap_or_default();
    }
}

impl BigTextApp {
    pub(crate) fn open_search(&mut self, replace: bool) {
        self.search.open = true;
        self.search.replace_open = replace || self.search.replace_open && self.search.open;
        if replace {
            self.search.replace_open = true;
        }
        self.search.focus_find = true;
        // Prefill from a short single-line selection.
        if let Some(d) = self.docs.get(self.active) {
            let (s, e) = d.selection();
            if e > s && e - s <= 256 {
                if let Some(b) = d.buf.read(s..e) {
                    let t = d.buf.codec.decode_lossy(&b);
                    if !t.contains('\n') {
                        self.search.query.pattern = if self.search.query.regex { regex_syntax::escape(&t) } else { t };
                    }
                }
            }
        }
    }

    /// Starts a streaming "Find All" over the document (or the selection).
    pub(crate) fn find_all(&mut self) {
        let Some(d) = self.docs.get(self.active) else { return };
        if !d.is_ready() {
            return;
        }
        let m = match self.search.matcher(d.buf.codec) {
            Ok(m) => m,
            Err(e) => {
                self.search.status = e;
                return;
            }
        };
        let range = if self.search.in_selection && d.has_selection() {
            let (s, e) = d.selection();
            s..e
        } else {
            0..d.buf.len()
        };
        self.search.remember();
        let d = &mut self.docs[self.active];
        if let Some(r) = d.results.take() {
            r.cancel.cancel();
        }
        let data = Arc::new(Mutex::new(ResultsData::default()));
        let cancel = CancelToken::new();
        let progress = Progress::new(range.end - range.start);
        let results = SearchResults {
            query: self.search.query.clone(),
            version: d.version,
            range: range.clone(),
            data: data.clone(),
            cancel: cancel.clone(),
            progress: progress.clone(),
        };
        d.results = Some(results);
        self.search.show_results = true;
        let snap = d.buf.snapshot();
        let waker = self.waker.clone();
        jobs::spawn("find-all", move || {
            let started = Instant::now();
            let mut batch: Vec<(u64, u64)> = Vec::with_capacity(4096);
            let mut count = 0usize;
            let mut throttle = Throttle::new(Duration::from_millis(50));
            let mut capped = false;
            let end = search::search_forward(&snap, &m, range, &cancel, Some(&progress), |s, e| {
                if count >= RESULT_CAP {
                    capped = true;
                    return false;
                }
                batch.push((s, e));
                count += 1;
                if batch.len() >= 4096 || throttle.ready() {
                    data.lock().unwrap().matches.append(&mut batch);
                    waker();
                }
                true
            });
            let mut g = data.lock().unwrap();
            g.matches.append(&mut batch);
            g.capped = capped;
            g.done = true;
            g.end = Some(end);
            g.elapsed = Some(started.elapsed());
            drop(g);
            waker();
        });
    }

    /// Find next/previous from the caret. Uses complete "Find All" results when they are valid,
    /// otherwise runs a directed search on a worker (works before any full search finishes).
    pub(crate) fn find_next(&mut self, forward: bool) {
        if self.search.query.pattern.is_empty() {
            self.open_search(false);
            return;
        }
        let Some(d) = self.docs.get(self.active) else { return };
        if !d.is_ready() {
            return;
        }
        let m = match self.search.matcher(d.buf.codec) {
            Ok(m) => m,
            Err(e) => {
                self.search.status = e;
                return;
            }
        };
        let (s, e) = d.selection();
        let from = if forward { e } else { s };
        if self.search.history.first() != Some(&self.search.query.pattern) {
            self.search.remember();
        }
        let d = &self.docs[self.active];
        // Fast path: complete results for the same query and version.
        if let Some(r) = &d.results {
            let g = r.data.lock().unwrap();
            if g.done && !g.capped && r.query == self.search.query && r.version == d.version && r.range == (0..d.buf.len()) {
                let v = &g.matches;
                if v.is_empty() {
                    self.search.status = tr("No matches").into();
                    return;
                }
                let idx = if forward {
                    let mut i = v.partition_point(|&(ms, _)| ms < from);
                    // Step over an empty match sitting exactly at the caret.
                    if i < v.len() && v[i] == (s, e) {
                        i += 1;
                    }
                    if i < v.len() { i } else { 0 }
                } else {
                    let i = v.partition_point(|&(ms, _)| ms < from);
                    if i > 0 { i - 1 } else { v.len() - 1 }
                };
                let (ms, me) = v[idx];
                drop(g);
                let d = &mut self.docs[self.active];
                d.select(ms, me);
                d.view.center_caret = true;
                let total = self.docs[self.active].results.as_ref().map(|r| r.data.lock().unwrap().matches.len()).unwrap_or(0);
                self.search.status = trf("Match {i} of {n}", &[("i", &(idx + 1).to_string()), ("n", &total.to_string())]);
                return;
            }
        }
        let d = &mut self.docs[self.active];
        if let Some(j) = d.find_job.take() {
            j.cancel.cancel();
        }
        let cancel = CancelToken::new();
        d.find_job = Some(FindJob { cancel: cancel.clone() });
        let snap = d.buf.snapshot();
        let version = d.version;
        let id = d.id;
        let tx = self.tx.clone();
        let waker = self.waker.clone();
        let sel_is_empty = s == e;
        jobs::spawn("find-next", move || {
            let len = snap.len();
            let mut wrapped = false;
            let found = if forward {
                let first = |range: Range<u64>| {
                    let mut f = None;
                    search::search_forward(&snap, &m, range, &cancel, None, |a, b| {
                        // Skip an empty match exactly at the caret when nothing is selected.
                        if a == b && a == from && sel_is_empty && a != 0 {
                            return true;
                        }
                        f = Some((a, b));
                        false
                    });
                    f
                };
                first(from..len).or_else(|| {
                    wrapped = true;
                    first(0..(from + m.overlap()).min(len))
                })
            } else {
                search::search_backward(&snap, &m, 0, from, &cancel).or_else(|| {
                    wrapped = true;
                    search::search_backward(&snap, &m, from, len, &cancel)
                })
            };
            if !cancel.is_cancelled() {
                let _ = tx.send(Msg::Found { doc: id, version, found, wrapped });
                waker();
            }
        });
        self.search.status = tr("Searching…").into();
    }

    pub(super) fn on_found(&mut self, doc: u64, version: u64, found: Option<(u64, u64)>, wrapped: bool) {
        let Some(d) = self.docs.iter_mut().find(|d| d.id == doc) else { return };
        d.find_job = None;
        if d.version != version {
            return;
        }
        match found {
            Some((s, e)) => {
                d.select(s, e);
                d.view.center_caret = true;
                self.search.status = if wrapped { tr("Wrapped around").into() } else { String::new() };
            }
            None => self.search.status = tr("No matches").into(),
        }
    }

    pub(super) fn search_panel(&mut self, ui: &mut egui::Ui) {
        let codec = self.docs.get(self.active).map(|d| d.buf.codec).unwrap_or(Codec::utf8());
        let mut do_next = None;
        let mut do_all = false;
        let mut do_cancel = false;
        let mut do_close = false;
        let mut do_replace = false;
        let mut do_replace_all = false;
        let mut do_preview = false;
        let mut do_filter = false;
        let mut do_mark = false;
        ui.horizontal(|ui| {
            ui.label(tr("Find"));
            let r = ui.add(TextEdit::singleline(&mut self.search.query.pattern).desired_width(320.0).hint_text(tr("text or regex")).font(egui::TextStyle::Monospace));
            if self.search.focus_find {
                r.request_focus();
                self.search.focus_find = false;
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                do_next = Some(!ui.input(|i| i.modifiers.shift));
                r.request_focus();
            }
            if r.has_focus() && ui.input(|i| i.key_pressed(Key::Escape)) {
                do_close = true;
            }
            if r.has_focus() {
                let (up, down) = ui.input_mut(|i| (i.consume_key(egui::Modifiers::NONE, Key::ArrowUp), i.consume_key(egui::Modifiers::NONE, Key::ArrowDown)));
                if up || down {
                    self.search.step_history(up);
                }
            }
            let history = self.search.history.clone();
            ui.add_enabled_ui(!history.is_empty(), |ui| {
                ui.menu_button("▾", |ui| {
                    for h in history {
                        let label: String = h.chars().take(80).collect();
                        if ui.button(RichText::new(label).monospace()).clicked() {
                            self.search.query.pattern = h;
                            self.search.focus_find = true;
                            ui.close();
                        }
                    }
                    ui.separator();
                    if ui.button(tr("Clear history")).clicked() {
                        self.search.history.clear();
                    }
                })
                .response
                .on_hover_text(tr("Search history (Up/Down in the field)"));
            });
            let q = &mut self.search.query;
            ui.toggle_value(&mut q.case_sensitive, RichText::new("Aa").monospace()).on_hover_text(tr("Case sensitive"));
            ui.toggle_value(&mut q.whole_word, RichText::new("W").monospace()).on_hover_text(tr("Whole word"));
            ui.toggle_value(&mut q.regex, RichText::new(".*").monospace()).on_hover_text(tr("Regular expression (linear time)"));
            ui.toggle_value(&mut q.dot_all, RichText::new("\\n").monospace()).on_hover_text(tr("Multiline: dot matches newline"));
            ui.toggle_value(&mut self.search.in_selection, RichText::new("Sel").monospace()).on_hover_text(tr("Search in selection (Find All / Replace All)"));
            if ui.button("◀").on_hover_text(tr("Find previous (Shift+F3)")).clicked() {
                do_next = Some(false);
            }
            if ui.button("▶").on_hover_text(tr("Find next (F3)")).clicked() {
                do_next = Some(true);
            }
            if ui.button(tr("Find All")).clicked() {
                do_all = true;
            }
            ui.separator();
            ui.toggle_value(&mut self.search.invert, RichText::new("!").monospace()).on_hover_text(tr("Filter: keep the lines WITHOUT a match"));
            if ui.button(tr("Filter")).on_hover_text(tr("Show only the lines with a match (Ctrl+Shift+F)")).clicked() {
                do_filter = true;
            }
            if ui.button(tr("Mark")).on_hover_text(tr("Highlight this pattern in its own colour (View → Highlights)")).clicked() {
                do_mark = true;
            }
            let running = self.docs.get(self.active).and_then(|d| d.results.as_ref()).is_some_and(|r| r.is_running());
            if running && ui.button(tr("Cancel")).clicked() {
                do_cancel = true;
            }
            if ui.small_button("×").clicked() {
                do_close = true;
            }
            if !self.search.status.is_empty() {
                ui.label(&self.search.status);
            }
        });
        // The engine's message points at the error with a caret, so it is shown in full, in
        // monospace, while typing.
        if !self.search.query.pattern.is_empty() {
            if let Err(e) = self.search.matcher(codec) {
                let color = ui.visuals().error_fg_color;
                egui::Frame::new().inner_margin(egui::Margin::symmetric(8, 2)).show(ui, |ui| {
                    ui.label(RichText::new(regex_error_text(&e)).monospace().color(color));
                });
            }
        }
        if self.search.replace_open {
            ui.horizontal(|ui| {
                ui.label(tr("Replace"));
                ui.add(TextEdit::singleline(&mut self.search.replace_with).desired_width(320.0).hint_text("$1, ${name}, \\n, \\t").font(egui::TextStyle::Monospace));
                if ui.button(tr("Replace")).clicked() {
                    do_replace = true;
                }
                if ui.button(tr("Replace All")).clicked() {
                    do_replace_all = true;
                }
                if ui.button(tr("Preview")).clicked() {
                    do_preview = true;
                }
            });
        }
        if do_close {
            self.search.open = false;
        }
        if do_cancel {
            if let Some(r) = self.docs.get(self.active).and_then(|d| d.results.as_ref()) {
                r.cancel.cancel();
            }
        }
        if do_all {
            self.find_all();
        }
        if let Some(fwd) = do_next {
            self.find_next(fwd);
        }
        if do_replace {
            self.replace_current();
        }
        if do_replace_all {
            self.replace_all();
        }
        if do_preview {
            self.replace_preview();
        }
        if do_filter {
            self.start_filter();
        }
        if do_mark {
            self.add_mark_from_search();
        }
    }

    /// Virtualized "Find All" results list.
    pub(super) fn results_panel(&mut self, ui: &mut egui::Ui) {
        let font = FontId::monospace(self.settings.font_size.min(14.0));
        let Some(d) = self.docs.get_mut(self.active) else { return };
        let Some(r) = &d.results else { return };
        let (n, capped, done, elapsed, end) = {
            let g = r.data.lock().unwrap();
            (g.matches.len(), g.capped, g.done, g.elapsed, g.end)
        };
        let mut close = false;
        let mut jump = None;
        let mut lines_action: Option<bool> = None;
        ui.horizontal(|ui| {
            let mut s = trf("{n} matches", &[("n", &super::filter_panel::fmt_count(n as u64))]);
            if capped {
                s.push_str(&trf(" (capped at {n} — more exist)", &[("n", &super::filter_panel::fmt_count(RESULT_CAP as u64))]));
            }
            ui.strong(s);
            if !done {
                ui.add(egui::ProgressBar::new(r.progress.fraction()).desired_width(160.0).text(format!("{} {:.0}%", tr("searching"), r.progress.fraction() * 100.0)));
                ui.ctx().request_repaint_after(Duration::from_millis(50));
            } else if let Some(t) = elapsed {
                let mb = (r.range.end - r.range.start) as f64 / (1024.0 * 1024.0);
                let note = if end == Some(SearchEnd::Cancelled) { tr(" (cancelled)") } else { "" };
                ui.label(format!("{} {:.2}s ({:.0} MB/s){note}", tr("in"), t.as_secs_f64(), mb / t.as_secs_f64().max(1e-6)));
            }
            if r.version != d.version {
                ui.colored_label(Color32::from_rgb(220, 150, 60), tr("stale (document changed)"));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("×").clicked() {
                    close = true;
                }
                if ui.add_enabled(done && r.version == d.version && n > 0, egui::Button::new(tr("Save lines…")).small()).on_hover_text(tr("Save every line with a match to a file")).clicked() {
                    lines_action = Some(true);
                }
                if ui.add_enabled(done && r.version == d.version && n > 0, egui::Button::new(tr("Copy lines")).small()).on_hover_text(tr("Copy every line with a match to the clipboard")).clicked() {
                    lines_action = Some(false);
                }
            });
        });
        let row_h = ui.ctx().fonts_mut(|f| f.row_height(&font)) + 2.0;
        let stale = r.version != d.version;
        let data = r.data.clone();
        let buf = &d.buf;
        let caret = d.view.caret;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, row_h, n, |ui, range| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            let g = data.lock().unwrap();
            for i in range {
                let Some(&(s, e)) = g.matches.get(i) else { break };
                let (line, exact) = buf.line_of(s.min(buf.len()));
                let job = snippet_job(buf, s, e, line, exact, &font, ui.visuals(), stale);
                let sel = caret >= s && caret <= e;
                let resp = ui.selectable_label(sel, job);
                if resp.clicked() {
                    jump = Some((s, e));
                }
            }
        });
        if let Some((s, e)) = jump {
            if !stale {
                d.select(s, e);
                d.view.center_caret = true;
            }
        }
        if close {
            if let Some(r) = d.results.take() {
                r.cancel.cancel();
            }
            self.search.show_results = false;
        }
        if let Some(save) = lines_action {
            let Some(d) = self.docs.get(self.active) else { return };
            let Some(r) = &d.results else { return };
            match Matcher::new(&r.query, d.buf.codec) {
                Ok(m) => {
                    let (snap, id, name) = (d.buf.snapshot(), d.id, d.title.clone());
                    if save {
                        self.save_lines_dialog(snap, Arc::new(m), Default::default(), &name);
                    } else {
                        self.copy_lines(snap, Arc::new(m), Default::default(), id);
                    }
                }
                Err(e) => self.error = Some(e),
            }
        }
    }

    /// Matches to highlight in the visible range of the active document.
    pub(super) fn view_highlighter(&mut self) -> Option<Arc<Matcher>> {
        if !self.search.open || self.search.query.pattern.is_empty() {
            return None;
        }
        let codec = self.docs.get(self.active)?.buf.codec;
        self.search.matcher(codec).ok()
    }

    /// Everything painted behind the text of the active document, bottom to top: highlights
    /// (marks), the other occurrences of the selected text, and search matches.
    pub(crate) fn view_highlights(&mut self, dark: bool) -> impl Fn(&TextBuf, Range<u64>) -> Vec<(u64, u64, Color32)> {
        let found = self.view_highlighter();
        let marks = match self.docs.get(self.active).map(|d| d.buf.codec) {
            Some(c) => self.mark_matchers(c),
            None => Vec::new(),
        };
        let occ = self.occurrence_matcher();
        let found_color = if dark { Color32::from_rgba_unmultiplied(200, 160, 0, 90) } else { Color32::from_rgba_unmultiplied(255, 200, 0, 120) };
        let occ_color = if dark { Color32::from_white_alpha(30) } else { Color32::from_black_alpha(30) };
        move |b: &TextBuf, r: Range<u64>| {
            let mut v = Vec::new();
            let mut add = |m: &Matcher, color: Color32| v.extend(search::matches_in_view(b, m, r.clone(), 2000).into_iter().map(|(s, e)| (s, e, color)));
            for (m, c) in &marks {
                add(m, super::marks::fill(*c, dark));
            }
            if let Some(m) = &occ {
                add(m, occ_color);
            }
            if let Some(m) = &found {
                add(m, found_color);
            }
            v
        }
    }

    /// Matcher for the other occurrences of a short selection on one line (whole words when the
    /// selection is a word), as editors highlight them after a double click.
    fn occurrence_matcher(&mut self) -> Option<Arc<Matcher>> {
        let d = self.docs.get(self.active)?;
        let (s, e) = d.selection();
        if s == e || e - s > 200 || d.hex || !d.is_ready() {
            return None;
        }
        let codec = d.buf.codec;
        let text = codec.decode_lossy(&d.buf.read(s..e)?);
        if text.contains(['\n', '\r']) || text.trim().is_empty() || (self.search.open && self.search.query.pattern == text) {
            return None;
        }
        if let Some((t, c, m)) = &self.occurrences {
            if *t == text && *c == codec {
                return m.clone();
            }
        }
        let word = text.chars().all(|c| c.is_alphanumeric() || c == '_');
        let q = Query { pattern: text.clone(), case_sensitive: true, whole_word: word, ..Default::default() };
        let m = Matcher::new(&q, codec).ok().map(Arc::new);
        self.occurrences = Some((text, codec, m.clone()));
        m
    }
}

/// A regex error without blank lines and the generic first line, its indentation removed
/// evenly so the caret still points at the right character.
fn regex_error_text(e: &str) -> String {
    let lines: Vec<&str> = e.lines().filter(|l| !l.trim().is_empty() && l.trim() != "error parsing regex").collect();
    let indent = lines.iter().map(|l| l.len() - l.trim_start().len()).min().unwrap_or(0);
    lines.iter().map(|l| &l[indent.min(l.len())..]).collect::<Vec<_>>().join("\n")
}

fn snippet_job(buf: &TextBuf, s: u64, e: u64, line: u64, exact: bool, font: &FontId, v: &egui::Visuals, stale: bool) -> LayoutJob {
    let mut job = LayoutJob::default();
    let weak = TextFormat { font_id: font.clone(), color: v.weak_text_color(), ..Default::default() };
    let normal = TextFormat { font_id: font.clone(), color: if stale { v.weak_text_color() } else { v.text_color() }, ..Default::default() };
    let hl = TextFormat { font_id: font.clone(), color: v.strong_text_color(), background: v.selection.bg_fill, ..Default::default() };
    job.append(&format!("{}{:>8}  ", if exact { " " } else { "~" }, line + 1), 0.0, weak);
    let codec = buf.codec;
    let e_vis = e.min(s + 200);
    let pre_start = s.saturating_sub(60);
    let (pre, mid, post) = match (buf.read(pre_start..s), buf.read(s..e_vis), buf.read(e_vis..(e_vis + 120).min(buf.len()))) {
        (Some(a), Some(b), Some(c)) => (a.into_owned(), b.into_owned(), c.into_owned()),
        _ => {
            job.append("…", 0.0, normal);
            return job;
        }
    };
    // Cut context at line breaks.
    let pre = match codec.rfind_nl(&pre, pre_start) {
        Some(i) => pre[i + codec.unit()..].to_vec(),
        None => pre,
    };
    let post = match codec.find_nl(&post, e_vis) {
        Some(i) => post[..i].to_vec(),
        None => post,
    };
    let clean = |b: &[u8]| -> String {
        codec.decode_lossy(b).chars().map(|c| if c.is_control() { if c == '\t' { ' ' } else { '·' } } else { c }).collect()
    };
    let pre_s = clean(skip_partial(&codec, &pre));
    job.append(&pre_s, 0.0, normal.clone());
    job.append(&clean(&mid), 0.0, hl);
    if e_vis < e {
        job.append("…", 0.0, normal.clone());
    }
    job.append(&clean(&post), 0.0, normal);
    let _ = rows::TAB_WIDTH;
    job
}

fn skip_partial<'a>(codec: &Codec, b: &'a [u8]) -> &'a [u8] {
    &b[codec.snap_forward(b).min(b.len())..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regex_error_keeps_the_caret_under_the_error() {
        let q = Query { pattern: "ab(cd".into(), regex: true, ..Default::default() };
        let e = Matcher::new(&q, Codec::utf8()).err().unwrap();
        let t = regex_error_text(&e);
        let lines: Vec<&str> = t.lines().collect();
        let pat = lines.iter().position(|l| l.contains("ab(cd")).expect(&t);
        let caret = lines[pat + 1];
        assert_eq!(caret.trim(), "^", "{t}");
        assert_eq!(caret.find('^'), lines[pat].find('('), "{t}");
    }
}
