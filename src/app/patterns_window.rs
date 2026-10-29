//! Message patterns (Search → Message patterns…): a window listing a log's line templates with
//! how many lines each has, most first or rarest first; a row filters the file to its lines or
//! hides them.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use egui::Color32;

use bigtext::jobs::{self, CancelToken, Progress};
use bigtext::patterns::{self, Patterns, Template};
use bigtext::search::Query;
use bigtext::timestamp;

use super::filter_panel::fmt_count;
use super::i18n::{tr, trf};
use super::tasks::TaskOutcome;
use super::BigTextApp;

#[derive(Default)]
pub struct PatternsData {
    pub p: Option<Patterns>,
    pub done: bool,
    pub error: Option<String>,
}

/// The patterns of one document.
pub struct PatternsResults {
    pub version: u64,
    /// Built from the filter's lines only.
    pub filtered: bool,
    pub data: Arc<Mutex<PatternsData>>,
    pub cancel: CancelToken,
    pub progress: Progress,
}

impl PatternsResults {
    pub fn is_running(&self) -> bool {
        !self.data.lock().unwrap().done
    }
}

#[derive(Default)]
pub struct PatternsUi {
    pub open: bool,
    pub rare: bool,
    pub only_filter: bool,
    /// Row order: (document, templates, rare) and the indices.
    order: Option<((u64, usize, bool), Vec<u32>)>,
}

enum Act {
    Filter(String, bool),
    Goto(u64),
    Export,
    Rerun,
}

impl BigTextApp {
    pub(crate) fn open_patterns(&mut self) {
        self.patterns.open = true;
        let fresh = self.docs.get(self.active).and_then(|d| d.patterns.as_ref().map(|p| p.version == d.version && p.filtered == self.patterns.only_filter));
        if fresh != Some(true) {
            self.start_patterns();
        }
    }

    pub(crate) fn start_patterns(&mut self) {
        let only_filter = self.patterns.only_filter;
        let Some(d) = self.docs.get_mut(self.active) else { return };
        if !d.is_ready() {
            return;
        }
        // The filter's matched lines, when asked for and the filter is complete.
        let only = if only_filter {
            d.filter.as_ref().filter(|r| r.version == d.version).and_then(|r| {
                let g = r.data.lock().unwrap();
                g.done.then(|| g.lines.iter().enumerate().filter(|&(i, _)| !g.is_context(i)).map(|(_, &s)| s).collect::<Vec<u64>>())
            })
        } else {
            None
        };
        if let Some(p) = d.patterns.take() {
            p.cancel.cancel();
        }
        let data = Arc::new(Mutex::new(PatternsData::default()));
        let (cancel, progress) = (CancelToken::new(), Progress::new(d.buf.len()));
        d.patterns = Some(PatternsResults { version: d.version, filtered: only.is_some(), data: data.clone(), cancel: cancel.clone(), progress: progress.clone() });
        let snap = d.buf.snapshot();
        let waker = self.waker.clone();
        jobs::spawn("patterns", move || {
            let partial = |p: &Patterns| {
                data.lock().unwrap().p = Some(p.clone());
                waker();
            };
            let r = patterns::build(&snap, only.as_deref(), &cancel, Some(&progress), partial);
            let mut g = data.lock().unwrap();
            match r {
                Ok(Some(p)) => g.p = Some(p),
                Ok(None) => {}
                Err(e) => g.error = Some(e.to_string()),
            }
            g.done = true;
            drop(g);
            waker();
        });
    }

    pub(super) fn patterns_window(&mut self, ctx: &egui::Context) {
        if !self.patterns.open {
            return;
        }
        let mut open = true;
        let mut act = None;
        let (mut rare, mut only) = (self.patterns.rare, self.patterns.only_filter);
        let has_filter = self.docs.get(self.active).is_some_and(|d| d.filter.as_ref().is_some_and(|r| !r.is_running() && r.version == d.version));
        egui::Window::new(tr("Message patterns")).open(&mut open).default_size([760.0, 420.0]).resizable(true).show(ctx, |ui| {
            let Some(d) = self.docs.get(self.active) else { return };
            let Some(r) = &d.patterns else {
                ui.weak(tr("Open a log first."));
                return;
            };
            let g = r.data.lock().unwrap();
            let stale = r.version != d.version;
            ui.horizontal(|ui| {
                if let Some(p) = &g.p {
                    ui.strong(trf("{n} templates in {lines} lines", &[("n", &fmt_count(p.templates.len() as u64)), ("lines", &fmt_count(p.lines))]));
                    if p.other > 0 {
                        ui.label(trf("(+{n} lines of other templates)", &[("n", &fmt_count(p.other))]));
                    }
                }
                ui.radio_value(&mut rare, false, tr("Most lines"));
                ui.radio_value(&mut rare, true, tr("Rarest"));
                ui.add_enabled(has_filter || only, egui::Checkbox::new(&mut only, tr("Only the filtered lines")));
                if !g.done {
                    let f = r.progress.fraction();
                    ui.add(egui::ProgressBar::new(f).desired_width(120.0).text(format!("{:.0}%", f * 100.0)));
                    ui.ctx().request_repaint_after(Duration::from_millis(100));
                    if ui.small_button(tr("Cancel")).clicked() {
                        r.cancel.cancel();
                    }
                }
                if stale {
                    ui.colored_label(Color32::from_rgb(220, 150, 60), tr("stale (document changed)"));
                    if ui.small_button(tr("Run again")).clicked() {
                        act = Some(Act::Rerun);
                    }
                }
                if g.done && g.p.is_some() && ui.small_button(tr("Export CSV…")).clicked() {
                    act = Some(Act::Export);
                }
            });
            if let Some(e) = &g.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            let Some(p) = &g.p else {
                ui.weak(tr("Reading the lines…"));
                return;
            };
            let n = p.templates.len();
            let key = (d.id, n, rare);
            if self.patterns.order.as_ref().map(|o| o.0) != Some(key) {
                let mut idx: Vec<u32> = (0..n as u32).collect();
                if rare {
                    idx.sort_by(|&a, &b| {
                        let (x, y) = (&p.templates[a as usize], &p.templates[b as usize]);
                        x.count.cmp(&y.count).then(x.first.cmp(&y.first))
                    });
                }
                self.patterns.order = Some((key, idx));
            }
            let order = &self.patterns.order.as_ref().expect("order").1;
            let font = egui::FontId::monospace(13.0);
            let row_h = ui.ctx().fonts_mut(|f| f.row_height(&font)) + 4.0;
            ui.separator();
            egui::ScrollArea::both().auto_shrink([false, false]).show_rows(ui, row_h, n, |ui, range| {
                for &i in &order[range.start.min(n)..range.end.min(n)] {
                    let t = &p.templates[i as usize];
                    let level = t.level.map(|l| l.name()).unwrap_or("");
                    let text = format!("{:>10}  {level:<5}  {}", fmt_count(t.count), t.text);
                    let resp = ui.add(egui::Button::selectable(false, egui::RichText::new(text).font(font.clone())).wrap_mode(egui::TextWrapMode::Extend));
                    let resp = resp.on_hover_ui(|ui| row_tip(ui, d, t));
                    if resp.clicked() && !stale {
                        act = Some(Act::Filter(t.text.clone(), false));
                    }
                    resp.context_menu(|ui| {
                        if ui.button(tr("Filter: these lines")).clicked() {
                            act = Some(Act::Filter(t.text.clone(), false));
                            ui.close();
                        }
                        if ui.button(tr("Filter: hide these lines")).clicked() {
                            act = Some(Act::Filter(t.text.clone(), true));
                            ui.close();
                        }
                        if ui.button(tr("Go to the first line")).clicked() {
                            act = Some(Act::Goto(t.first));
                            ui.close();
                        }
                        if ui.button(tr("Copy template")).clicked() {
                            ui.ctx().copy_text(t.text.clone());
                            ui.close();
                        }
                    });
                }
            });
        });
        self.patterns.open = open;
        let changed = only != self.patterns.only_filter;
        self.patterns.rare = rare;
        self.patterns.only_filter = only;
        match act {
            Some(Act::Filter(text, hide)) => self.filter_by_template(&text, hide),
            Some(Act::Goto(off)) => {
                if let Some(d) = self.docs.get_mut(self.active) {
                    d.goto_offset(off);
                }
            }
            Some(Act::Export) => self.export_patterns(),
            Some(Act::Rerun) => self.start_patterns(),
            None if changed => self.start_patterns(),
            None => {}
        }
    }

    /// Filters the active document to the lines of a template (or, `hide`, the other lines).
    pub(crate) fn filter_by_template(&mut self, text: &str, hide: bool) {
        self.search.query = Query { pattern: patterns::regex_for(text), regex: true, case_sensitive: true, whole_word: false, dot_all: false };
        self.search.invert = hide;
        self.start_filter();
    }

    fn export_patterns(&mut self) {
        let Some(d) = self.docs.get(self.active) else { return };
        let Some(r) = &d.patterns else { return };
        let Some(mut list) = r.data.lock().unwrap().p.as_ref().map(|p| p.templates.clone()) else { return };
        patterns::sort(&mut list, self.patterns.rare);
        let name = format!("{}.patterns.csv", d.title);
        let Some(target) = self.pick_output_file(tr("Export CSV"), &name) else { return };
        self.spawn_task(tr("Saving").into(), move |_: &Progress, _: &CancelToken| -> TaskOutcome {
            super::tasks::write_via_temp(&target, |w| {
                patterns::to_csv(&list, w)?;
                Ok(Some(trf("Saved {n} templates to {file}.", &[("n", &fmt_count(list.len() as u64)), ("file", &super::filter_panel::file_name(&target))])))
            })
        });
    }
}

/// A row's hover: its first line and the times of its first and last lines.
fn row_tip(ui: &mut egui::Ui, d: &super::document::Document, t: &Template) {
    let line = d.line_bounds(t.first, 4096).and_then(|(ls, le, _)| d.buf.read(ls.max(d.buf.codec.bom_len as u64)..le)).map(|b| d.buf.codec.decode_lossy(&b));
    if let Some(l) = line {
        ui.label(egui::RichText::new(l).monospace());
    }
    let time = |off: u64| timestamp::stamp_at(&d.buf, off).map(|s| timestamp::format_key(s.ms, s.has_date, true));
    if let (Some(a), Some(b)) = (time(t.first), time(t.last)) {
        ui.weak(trf("first {time}", &[("time", &a)]) + " · " + &trf("last {time}", &[("time", &b)]));
    }
    ui.weak(tr("Click: show only these lines; right-click: more"));
}
