//! Alerts on new lines (View → Alerts on new lines…): patterns per tab. The complete lines added
//! to a followed log (a growing file, a pipe, a remote `tail -F`, a container's log) that match
//! one are listed, counted on the tab and in the status bar, and announced with a system
//! notification and a sound — at most one every ten seconds, the rest counted into the next —
//! and, when BigText is in the background, its taskbar button flashes. Lines already in the file
//! when the first pattern is set do not alert.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bigtext::jobs::{self, CancelToken};
use bigtext::search::{Matcher, Query};
use bigtext::watch::{self, Hit, Scan};

use super::filter_panel::fmt_count;
use super::i18n::{tr, trf};
use super::{notify, BigTextApp, Msg};

/// Lines kept in a tab's list.
pub const HITS_KEPT: usize = 500;
/// At most one notification in this time; the lines in between come in the next.
const NOTICE_GAP: Duration = Duration::from_secs(10);
/// Recent patterns kept for the menu.
const RECENT_MAX: usize = 12;
/// Characters of a line in a notification.
const NOTICE_CHARS: usize = 180;

/// Where a look's result comes (the outer `None` until it is done).
type ScanSlot = Arc<Mutex<Option<Option<Scan>>>>;

/// A tab's alerts.
pub struct Alerts {
    pub patterns: Vec<Query>,
    matchers: Arc<Vec<Matcher>>,
    /// The next look starts here (a line start).
    pub scanned: u64,
    job: Option<(ScanSlot, CancelToken)>,
    /// The latest lines that matched, oldest first.
    pub hits: VecDeque<Hit>,
    pub total: u64,
    /// Lines that matched while the tab was not in view.
    pub unseen: u64,
}

impl Alerts {
    fn new(scanned: u64) -> Alerts {
        Alerts {
            patterns: Vec::new(),
            matchers: Arc::new(Vec::new()),
            scanned,
            job: None,
            hits: VecDeque::new(),
            total: 0,
            unseen: 0,
        }
    }
}

impl Drop for Alerts {
    fn drop(&mut self) {
        if let Some((_, c)) = &self.job {
            c.cancel();
        }
    }
}

/// Lines waiting for the next notification.
struct Pending {
    doc: u64,
    title: String,
    lines: u64,
    first: String,
    offset: u64,
}

pub struct AlertsUi {
    pub open: bool,
    /// The pattern being typed, with its options.
    pub draft: Query,
    pub notify: bool,
    pub sound: bool,
    /// Notifications and sounds only while BigText is not the active window.
    pub background_only: bool,
    pub recent: Vec<Query>,
    pub error: Option<String>,
    last_notice: Option<Instant>,
    pending: Vec<Pending>,
    /// The tab and line a click on the last notification opens.
    target: Arc<Mutex<Option<(u64, u64)>>>,
    click_set: bool,
}

impl Default for AlertsUi {
    fn default() -> Self {
        AlertsUi {
            open: false,
            draft: Query {
                case_sensitive: true,
                ..Default::default()
            },
            notify: true,
            sound: true,
            background_only: false,
            recent: Vec::new(),
            error: None,
            last_notice: None,
            pending: Vec::new(),
            target: Arc::default(),
            click_set: false,
        }
    }
}

/// A pattern as one line: flags (`r` regex, `c` case-sensitive, `w` whole words), `|`, text.
pub fn query_line(q: &Query) -> String {
    let flags: String = [(q.regex, 'r'), (q.case_sensitive, 'c'), (q.whole_word, 'w')]
        .iter()
        .filter(|x| x.0)
        .map(|x| x.1)
        .collect();
    format!("{flags}|{}", q.pattern)
}

pub fn parse_query_line(s: &str) -> Option<Query> {
    let (flags, pattern) = s.split_once('|')?;
    (!pattern.is_empty() && !pattern.contains(['\n', '\r'])).then(|| Query {
        pattern: pattern.to_string(),
        regex: flags.contains('r'),
        case_sensitive: flags.contains('c'),
        whole_word: flags.contains('w'),
        dot_all: false,
    })
}

impl AlertsUi {
    /// The options as one line: notify, sound, background only.
    pub fn options_line(&self) -> String {
        let b = |x: bool| if x { '1' } else { '0' };
        format!(
            "{}{}{}",
            b(self.notify),
            b(self.sound),
            b(self.background_only)
        )
    }

    pub fn set_options_line(&mut self, s: &str) {
        let f: Vec<char> = s.chars().collect();
        if f.len() == 3 {
            (self.notify, self.sound, self.background_only) =
                (f[0] == '1', f[1] == '1', f[2] == '1');
        }
    }

    fn remember(&mut self, q: &Query) {
        self.recent.retain(|r| r != q);
        self.recent.insert(0, q.clone());
        self.recent.truncate(RECENT_MAX);
    }
}

/// A pattern as the list shows it: `/regex/`, `"text"`, with `Aa` when case matters.
fn shown(q: &Query) -> String {
    let mut s = if q.regex {
        format!("/{}/", q.pattern)
    } else {
        format!("“{}”", q.pattern)
    };
    if q.case_sensitive {
        s.push_str("  Aa");
    }
    if q.whole_word {
        s.push_str("  ab|");
    }
    s
}

impl BigTextApp {
    /// View → Alerts on new lines…
    pub(crate) fn open_alerts(&mut self) {
        self.alerts.open = true;
        self.alerts.error = None;
        if let Some(a) = self
            .docs
            .get_mut(self.active)
            .and_then(|d| d.alerts.as_mut())
        {
            a.unseen = 0;
        }
    }

    /// Alerts on `q` in the active tab, from the lines added from now on.
    pub(crate) fn add_alert(&mut self, q: Query) -> Result<(), String> {
        let q = Query {
            pattern: q.pattern.trim_end_matches(['\n', '\r']).to_string(),
            ..q
        };
        if q.pattern.is_empty() {
            return Err(tr("Type a pattern first.").into());
        }
        let Some(d) = self.docs.get_mut(self.active).filter(|d| d.is_ready()) else {
            return Err(tr("Open a log first.").into());
        };
        Matcher::new(&q, d.buf.codec)?;
        let codec = d.buf.codec;
        let a = d.alerts.get_or_insert_with(|| Alerts::new(d.buf.len()));
        if !a.patterns.contains(&q) {
            a.patterns.push(q.clone());
            a.matchers = Arc::new(
                a.patterns
                    .iter()
                    .filter_map(|q| Matcher::new(q, codec).ok())
                    .collect(),
            );
        }
        self.alerts.remember(&q);
        if !self.alerts.click_set {
            self.alerts.click_set = true;
            let (tx, waker) = (self.tx.clone(), self.waker.clone());
            notify::on_click(Box::new(move || {
                let _ = tx.send(Msg::AlertClicked);
                waker();
            }));
        }
        self.persist_state();
        Ok(())
    }

    /// Takes the active tab's pattern `i` away (and the alerts, after the last).
    pub(crate) fn remove_alert(&mut self, i: usize) {
        let Some(d) = self.docs.get_mut(self.active) else {
            return;
        };
        let Some(a) = d.alerts.as_mut() else { return };
        if i < a.patterns.len() {
            a.patterns.remove(i);
            let codec = d.buf.codec;
            a.matchers = Arc::new(
                a.patterns
                    .iter()
                    .filter_map(|q| Matcher::new(q, codec).ok())
                    .collect(),
            );
        }
        if a.patterns.is_empty() {
            d.alerts = None;
        }
    }

    /// Looks at the lines added to tabs with alerts, and announces those that matched.
    pub(super) fn alerts_tick(&mut self, ctx: &egui::Context) {
        let focused = ctx.input(|i| i.viewport().focused).unwrap_or(true);
        let active = self.docs.get(self.active).map(|d| d.id);
        let mut found: Vec<Pending> = Vec::new();
        let mut any = false;
        for d in &mut self.docs {
            let (ready, modified) = (d.is_ready(), d.is_modified());
            let Some(a) = d.alerts.as_mut() else { continue };
            any = true;
            if let Some((slot, _)) = &a.job {
                let done = slot.lock().unwrap().take();
                match done {
                    Some(Some(scan)) => {
                        a.job = None;
                        a.scanned = scan.next;
                        if scan.count > 0 {
                            a.total += scan.count;
                            if !(focused && active == Some(d.id)) {
                                a.unseen += scan.count;
                            }
                            let first = scan.hits.first().cloned();
                            for h in scan.hits {
                                a.hits.push_back(h);
                            }
                            while a.hits.len() > HITS_KEPT {
                                a.hits.pop_front();
                            }
                            if let Some(h) = first {
                                found.push(Pending {
                                    doc: d.id,
                                    title: d.title.clone(),
                                    lines: scan.count,
                                    first: h.text,
                                    offset: h.offset,
                                });
                            }
                        }
                    }
                    Some(None) => a.job = None,
                    None => continue,
                }
            }
            if focused && active == Some(d.id) {
                a.unseen = 0;
            }
            if a.job.is_some() || !ready || modified || a.matchers.is_empty() {
                continue;
            }
            let len = d.buf.len();
            // Rotated or cut: the new file's lines are new.
            if len < a.scanned {
                a.scanned = 0;
            }
            if len == a.scanned {
                continue;
            }
            let slot = Arc::new(Mutex::new(None));
            let cancel = CancelToken::new();
            let (snap, matchers, from) = (d.buf.snapshot(), a.matchers.clone(), a.scanned);
            let (s2, c2, waker) = (slot.clone(), cancel.clone(), self.waker.clone());
            jobs::spawn("alerts", move || {
                let r = watch::scan(&snap, &matchers, from, HITS_KEPT, &c2);
                *s2.lock().unwrap() = Some(r);
                waker();
            });
            a.job = Some((slot, cancel));
        }
        if !any {
            notify::hide();
        }
        if !found.is_empty() && !focused {
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                egui::UserAttentionType::Informational,
            ));
        }
        let ui = &mut self.alerts;
        for f in found {
            match ui.pending.iter_mut().find(|p| p.doc == f.doc) {
                Some(p) => p.lines += f.lines,
                None => ui.pending.push(f),
            }
        }
        if ui.pending.is_empty() {
            return;
        }
        let wait = ui
            .last_notice
            .map_or(Duration::ZERO, |t| NOTICE_GAP.saturating_sub(t.elapsed()));
        if !wait.is_zero() {
            ctx.request_repaint_after(wait);
            return;
        }
        let pending = std::mem::take(&mut ui.pending);
        if ui.background_only && focused {
            return;
        }
        let p = &pending[0];
        let mut body: String = p.first.chars().take(NOTICE_CHARS).collect();
        let more = pending.iter().map(|p| p.lines).sum::<u64>() - 1;
        if more > 0 {
            body.push('\n');
            body.push_str(&trf("(+{n} more lines)", &[("n", &fmt_count(more))]));
        }
        let title = if pending.len() == 1 {
            format!("BigText: {}", p.title)
        } else {
            trf("BigText: {n} tabs", &[("n", &pending.len().to_string())])
        };
        *ui.target.lock().unwrap() = Some((p.doc, p.offset));
        if ui.notify {
            let shown = notify::show(&title, &body, ui.sound);
            if !shown && ui.sound {
                notify::beep();
            }
        } else if ui.sound {
            notify::beep();
        }
        ui.last_notice = Some(Instant::now());
    }

    /// A click on a notification: its tab and line, and BigText to the front.
    pub(super) fn on_alert_clicked(&mut self) {
        let Some((doc, off)) = *self.alerts.target.lock().unwrap() else {
            return;
        };
        if let Some(i) = self.docs.iter().position(|d| d.id == doc) {
            self.active = i;
            let d = &mut self.docs[i];
            if off < d.buf.len() {
                d.goto_offset(off);
            }
            if let Some(a) = d.alerts.as_mut() {
                a.unseen = 0;
            }
        }
        if let Some(ctx) = self.ctx_clone() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
    }

    pub(super) fn alerts_window(&mut self, ctx: &egui::Context) {
        if !self.alerts.open {
            return;
        }
        let mut open = true;
        let (mut add, mut remove, mut goto, mut clear, mut follow) =
            (None, None, None, false, false);
        let ui_state = &mut self.alerts;
        let d = self.docs.get(self.active);
        egui::Window::new(tr("Alerts on new lines")).open(&mut open).default_size([640.0, 440.0]).resizable(true).collapsible(false).show(ctx, |ui| {
            let Some(d) = d else {
                ui.weak(tr("Open a log first."));
                return;
            };
            ui.horizontal(|ui| {
                ui.strong(&d.title);
                if d.follow {
                    ui.colored_label(super::theme::ok_text(ui), tr("FOLLOW"));
                }
            });
            if !d.follow && d.growing.is_none() {
                ui.horizontal(|ui| {
                    ui.colored_label(super::theme::warn_text(ui), tr("Alerts look at the lines added while the file is followed."));
                    if ui.small_button(tr("Follow it")).clicked() {
                        follow = true;
                    }
                });
            }
            ui.weak(tr("A line added from now on that matches one of the patterns is shown below, counted on the tab, and announced."));
            ui.add_space(4.0);
            let a = d.alerts.as_ref();
            for (i, q) in a.map(|a| a.patterns.as_slice()).unwrap_or_default().iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(shown(q)).monospace());
                    if ui.small_button("×").on_hover_text(tr("Remove this pattern")).clicked() {
                        remove = Some(i);
                    }
                });
            }
            ui.horizontal(|ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut ui_state.draft.pattern).hint_text("ERROR|FATAL").desired_width(240.0));
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.toggle_value(&mut ui_state.draft.regex, ".*").on_hover_text(tr("Regular expression"));
                ui.toggle_value(&mut ui_state.draft.case_sensitive, "Aa").on_hover_text(tr("Match case"));
                ui.toggle_value(&mut ui_state.draft.whole_word, "ab|").on_hover_text(tr("Whole words"));
                if ui.button(tr("Add")).clicked() || enter {
                    add = Some(ui_state.draft.clone());
                }
                if !ui_state.recent.is_empty() {
                    ui.menu_button(tr("Recent"), |ui| {
                        for q in &ui_state.recent {
                            if ui.button(shown(q)).clicked() {
                                add = Some(q.clone());
                                ui.close();
                            }
                        }
                    });
                }
            });
            if let Some(e) = &ui_state.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            ui.horizontal(|ui| {
                ui.checkbox(&mut ui_state.notify, tr("System notification"));
                ui.checkbox(&mut ui_state.sound, tr("Sound"));
                ui.checkbox(&mut ui_state.background_only, tr("Only when BigText is in the background"));
            });
            ui.separator();
            let Some(a) = a else { return };
            ui.horizontal(|ui| {
                ui.strong(trf("{n} lines matched", &[("n", &fmt_count(a.total))]));
                if a.total as usize > a.hits.len() {
                    ui.weak(trf("(the last {n} are listed)", &[("n", &fmt_count(a.hits.len() as u64))]));
                }
                if a.total > 0 && ui.small_button(tr("Clear")).clicked() {
                    clear = true;
                }
            });
            let font = egui::FontId::monospace(13.0);
            let row_h = ui.ctx().fonts_mut(|f| f.row_height(&font)) + 4.0;
            let n = a.hits.len();
            egui::ScrollArea::both().id_salt("alert-hits").auto_shrink([false, false]).stick_to_bottom(true).show_rows(ui, row_h, n, |ui, range| {
                for h in a.hits.range(range) {
                    let resp = ui.add(egui::Button::selectable(false, egui::RichText::new(&h.text).font(font.clone())).wrap_mode(egui::TextWrapMode::Extend));
                    if resp.on_hover_text(tr("Click: go to the line")).clicked() {
                        goto = Some(h.offset);
                    }
                }
            });
        });
        let s = &mut self.alerts;
        s.open = open;
        if let Some(q) = add {
            match self.add_alert(q) {
                Ok(()) => {
                    self.alerts.error = None;
                    self.alerts.draft.pattern.clear();
                }
                Err(e) => self.alerts.error = Some(e),
            }
        }
        if let Some(i) = remove {
            self.remove_alert(i);
        }
        let Some(d) = self.docs.get_mut(self.active) else {
            return;
        };
        if follow {
            d.follow = true;
        }
        if let Some(a) = d.alerts.as_mut() {
            if clear {
                a.hits.clear();
                a.total = 0;
            }
            a.unseen = 0;
        }
        if let Some(off) = goto.filter(|&o| o < d.buf.len()) {
            d.goto_offset(off);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_and_options_are_kept_as_lines() {
        let q = Query {
            pattern: "ERROR|FATAL".into(),
            regex: true,
            case_sensitive: true,
            whole_word: false,
            dot_all: false,
        };
        assert_eq!(query_line(&q), "rc|ERROR|FATAL");
        assert_eq!(parse_query_line("rc|ERROR|FATAL"), Some(q));
        assert_eq!(
            parse_query_line("w|timeout").map(|q| (q.regex, q.case_sensitive, q.whole_word)),
            Some((false, false, true))
        );
        assert_eq!(parse_query_line("r|"), None);
        let mut ui = AlertsUi::default();
        assert_eq!(ui.options_line(), "110");
        ui.set_options_line("011");
        assert_eq!(
            (ui.notify, ui.sound, ui.background_only),
            (false, true, true)
        );
        for i in 0..20 {
            ui.remember(&Query {
                pattern: format!("p{i}"),
                ..Default::default()
            });
        }
        ui.remember(&Query {
            pattern: "p5".into(),
            ..Default::default()
        });
        assert_eq!(ui.recent.len(), RECENT_MAX);
        assert_eq!(ui.recent[0].pattern, "p5");
    }
}
