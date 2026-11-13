//! Web log report (Search → Web log report…): the requests of an nginx / Apache, AWS load
//! balancer or IIS log summed up in one pass: requests by status, the top URLs, the requests and
//! 5xx errors per minute (with a chart) and the slowest requests. A URL filters the file to its
//! requests; a minute or a slow request goes to its line. Each report opens as a table too.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bigtext::jobs::{self, human_bytes, CancelToken, Progress};
use bigtext::logformat::{self, Format, Report};
use bigtext::search::Query;

use super::filter_panel::{file_name, fmt_count};
use super::i18n::{tr, trf};
use super::tasks::{self, TaskOutcome};
use super::BigTextApp;

#[derive(Default)]
pub struct ReportData {
    pub format: Option<Format>,
    pub r: Option<Report>,
    pub done: bool,
    pub error: Option<String>,
}

/// The report of one document.
pub struct ReportResults {
    pub version: u64,
    pub data: Arc<Mutex<ReportData>>,
    pub cancel: CancelToken,
    pub progress: Progress,
}

impl ReportResults {
    pub fn is_running(&self) -> bool {
        !self.data.lock().unwrap().done
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Urls,
    Minutes,
    Slow,
}

impl Tab {
    fn name(self) -> &'static str {
        match self {
            Tab::Urls => "Top URLs",
            Tab::Minutes => "5xx per minute",
            Tab::Slow => "Slowest requests",
        }
    }

    fn file_suffix(self) -> &'static str {
        match self {
            Tab::Urls => "urls",
            Tab::Minutes => "minutes",
            Tab::Slow => "slowest",
        }
    }
}

/// Document, minutes, worst first, only errors.
type OrderKey = (u64, usize, bool, bool);

#[derive(Default)]
pub struct ReportUi {
    pub open: bool,
    pub tab: Tab,
    /// The minutes with the most 5xx first, rather than in time order.
    pub worst_first: bool,
    /// Only the minutes with 5xx errors.
    pub only_errors: bool,
    /// Row order of the minutes, for what it was made for.
    order: Option<(OrderKey, Vec<u32>)>,
}

enum Act {
    FilterUrl(String),
    Goto(u64),
    Table(Tab),
    Export(Tab),
    Rerun,
}

/// Bars of the minutes chart are at least this wide.
const BAR_MIN: f32 = 3.0;
/// …and at most this wide, when there are few.
const BAR_MAX: f32 = 18.0;
const CHART_HEIGHT: f32 = 110.0;

impl BigTextApp {
    pub(crate) fn open_log_report(&mut self) {
        self.log_report.open = true;
        let fresh = self
            .docs
            .get(self.active)
            .and_then(|d| d.log_report.as_ref().map(|r| r.version == d.version));
        if fresh != Some(true) {
            self.start_log_report();
        }
    }

    pub(crate) fn start_log_report(&mut self) {
        let Some(d) = self.docs.get_mut(self.active) else {
            return;
        };
        if !d.is_ready() {
            return;
        }
        if let Some(r) = d.log_report.take() {
            r.cancel.cancel();
        }
        let data = Arc::new(Mutex::new(ReportData::default()));
        let (cancel, progress) = (CancelToken::new(), Progress::new(d.buf.len()));
        d.log_report = Some(ReportResults {
            version: d.version,
            data: data.clone(),
            cancel: cancel.clone(),
            progress: progress.clone(),
        });
        let snap = d.buf.snapshot();
        let waker = self.waker.clone();
        jobs::spawn("log-report", move || {
            let error = match logformat::detect(&snap) {
                Ok(Some(f)) if f.has_requests() => {
                    data.lock().unwrap().format = Some(f.clone());
                    let partial = |r: &Report| {
                        data.lock().unwrap().r = Some(r.clone());
                        waker();
                    };
                    match logformat::report(&snap, &f, &cancel, Some(&progress), partial) {
                        Ok(Some(r)) => {
                            data.lock().unwrap().r = Some(r);
                            None
                        }
                        Ok(None) => None,
                        Err(e) => Some(e.to_string()),
                    }
                }
                Ok(Some(f)) => Some(trf("This is a {format}: it has no web requests. File → Open log as table splits it into columns.", &[("format", tr(f.kind.name()))])),
                Ok(None) => Some(tr("This is not a web server log (nginx / Apache, AWS load balancer or IIS).").to_string()),
                Err(e) => Some(e.to_string()),
            };
            let mut g = data.lock().unwrap();
            g.error = error;
            g.done = true;
            drop(g);
            waker();
        });
    }

    pub(super) fn log_report_window(&mut self, ctx: &egui::Context) {
        if !self.log_report.open {
            return;
        }
        let mut open = true;
        let mut act = None;
        let (mut tab, mut worst, mut only) = (
            self.log_report.tab,
            self.log_report.worst_first,
            self.log_report.only_errors,
        );
        egui::Window::new(tr("Web log report")).open(&mut open).default_size([880.0, 500.0]).resizable(true).show(ctx, |ui| {
            let Some(d) = self.docs.get(self.active) else { return };
            let Some(res) = &d.log_report else {
                ui.weak(tr("Open a log first."));
                return;
            };
            let g = res.data.lock().unwrap();
            let stale = res.version != d.version;
            ui.horizontal_wrapped(|ui| {
                if let Some(f) = &g.format {
                    ui.strong(tr(f.kind.name()));
                }
                if let Some(r) = &g.r {
                    ui.label(trf("{n} requests", &[("n", &fmt_count(r.requests))]));
                    ui.label(format!("4xx {}", pct(r.client_errors, r.requests)));
                    let five = egui::RichText::new(format!("5xx {}", pct(r.server_errors, r.requests)));
                    ui.label(if r.server_errors > 0 { five.color(ui.visuals().error_fg_color) } else { five });
                    ui.label(trf("{size} sent", &[("size", &human_bytes(r.bytes))]));
                    if r.other > 0 {
                        ui.weak(trf("{n} lines in another format", &[("n", &fmt_count(r.other))]));
                    }
                }
                if !g.done {
                    let f = res.progress.fraction();
                    ui.add(egui::ProgressBar::new(f).desired_width(120.0).text(format!("{:.0}%", f * 100.0)));
                    ui.ctx().request_repaint_after(Duration::from_millis(100));
                    if ui.small_button(tr("Cancel")).clicked() {
                        res.cancel.cancel();
                    }
                }
                if stale {
                    ui.colored_label(super::theme::warn_text(ui), tr("stale (document changed)"));
                    if ui.small_button(tr("Run again")).clicked() {
                        act = Some(Act::Rerun);
                    }
                }
            });
            if let Some(e) = &g.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            let (Some(r), Some(f)) = (&g.r, &g.format) else {
                if g.error.is_none() {
                    ui.weak(tr("Reading the lines…"));
                }
                return;
            };
            statuses_line(ui, r);
            ui.separator();
            let timed = f.has_durations();
            if !timed && tab == Tab::Slow {
                tab = Tab::Urls;
            }
            ui.horizontal(|ui| {
                ui.selectable_value(&mut tab, Tab::Urls, tr(Tab::Urls.name()));
                ui.selectable_value(&mut tab, Tab::Minutes, tr(Tab::Minutes.name()));
                ui.add_enabled_ui(timed, |ui| ui.selectable_value(&mut tab, Tab::Slow, tr(Tab::Slow.name()))).response.on_disabled_hover_text(tr("This log has no request times (for nginx, add $request_time to log_format)."));
                if g.done {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(tr("Export CSV…")).clicked() {
                            act = Some(Act::Export(tab));
                        }
                        if ui.button(tr("Open as table")).clicked() {
                            act = Some(Act::Table(tab));
                        }
                    });
                }
            });
            let font = egui::FontId::monospace(13.0);
            let row_h = ui.ctx().fonts_mut(|f| f.row_height(&font)) + 4.0;
            match tab {
                Tab::Urls => {
                    let head = format!("{:>11} {:>6} {:>9} {:>9} {:>10}{}  URL", tr("Requests"), "%", "4xx", "5xx", tr("Sent"), if timed { format!(" {:>8} {:>8}", tr("Mean, s"), tr("Max, s")) } else { String::new() });
                    ui.label(egui::RichText::new(head).font(font.clone()).weak());
                    if r.other_urls > 0 {
                        ui.weak(trf("(+{n} requests to other URLs)", &[("n", &fmt_count(r.other_urls))]));
                    }
                    let n = r.urls.len();
                    egui::ScrollArea::both().id_salt("report-urls").auto_shrink([false, false]).show_rows(ui, row_h, n, |ui, range| {
                        for u in &r.urls[range.start.min(n)..range.end.min(n)] {
                            let times = if timed { format!(" {:>8} {:>8}", u.mean().map(secs).unwrap_or_default(), if u.timed > 0 { secs(u.secs_max) } else { String::new() }) } else { String::new() };
                            let text = format!("{:>11} {:>6} {:>9} {:>9} {:>10}{times}  {}", fmt_count(u.requests), pct(u.requests, r.requests), fmt_count(u.client_errors), fmt_count(u.server_errors), human_bytes(u.bytes), u.url);
                            let mut rich = egui::RichText::new(text).font(font.clone());
                            if u.server_errors * 20 > u.requests {
                                rich = rich.color(ui.visuals().error_fg_color);
                            }
                            let resp = ui.add(egui::Button::selectable(false, rich).wrap_mode(egui::TextWrapMode::Extend)).on_hover_text(tr("Click: show only the requests to this URL; right-click: more"));
                            if resp.clicked() && !stale {
                                act = Some(Act::FilterUrl(u.url.clone()));
                            }
                            resp.context_menu(|ui| {
                                if ui.add_enabled(!stale, egui::Button::new(tr("Filter: the requests to this URL"))).clicked() {
                                    act = Some(Act::FilterUrl(u.url.clone()));
                                    ui.close();
                                }
                                if ui.add_enabled(!stale, egui::Button::new(tr("Go to the first request"))).clicked() {
                                    act = Some(Act::Goto(u.first));
                                    ui.close();
                                }
                                if ui.button(tr("Copy URL")).clicked() {
                                    ui.ctx().copy_text(u.url.clone());
                                    ui.close();
                                }
                            });
                        }
                    });
                }
                Tab::Minutes => {
                    if let Some(off) = minutes_chart(ui, r) {
                        if !stale {
                            act = Some(Act::Goto(off));
                        }
                    }
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut worst, false, tr("In time order"));
                        ui.radio_value(&mut worst, true, tr("Most 5xx first"));
                        ui.checkbox(&mut only, tr("Only minutes with 5xx"));
                    });
                    let key = (d.id, r.minutes.len(), worst, only);
                    if self.log_report.order.as_ref().map(|o| o.0) != Some(key) {
                        let mut idx: Vec<u32> = (0..r.minutes.len() as u32).filter(|&i| !only || r.minutes[i as usize].server_errors > 0).collect();
                        if worst {
                            idx.sort_by(|&a, &b| {
                                let (x, y) = (&r.minutes[a as usize], &r.minutes[b as usize]);
                                y.server_errors.cmp(&x.server_errors).then(x.minute.cmp(&y.minute))
                            });
                        }
                        self.log_report.order = Some((key, idx));
                    }
                    let order = &self.log_report.order.as_ref().expect("order").1;
                    let head = format!("{:<17} {:>10} {:>9} {:>7} {:>9}", tr("Minute"), tr("Requests"), "5xx", "%", "4xx");
                    ui.label(egui::RichText::new(head).font(font.clone()).weak());
                    let n = order.len();
                    egui::ScrollArea::both().id_salt("report-minutes").auto_shrink([false, false]).show_rows(ui, row_h, n, |ui, range| {
                        for &i in &order[range.start.min(n)..range.end.min(n)] {
                            let m = &r.minutes[i as usize];
                            let text = format!("{:<17} {:>10} {:>9} {:>7} {:>9}", m.label(), fmt_count(m.requests), fmt_count(m.server_errors), pct(m.server_errors, m.requests), fmt_count(m.client_errors));
                            let mut rich = egui::RichText::new(text).font(font.clone());
                            if m.server_errors * 20 > m.requests {
                                rich = rich.color(ui.visuals().error_fg_color);
                            }
                            let resp = ui.add(egui::Button::selectable(false, rich).wrap_mode(egui::TextWrapMode::Extend)).on_hover_text(tr("Click: go to the minute's first line"));
                            if resp.clicked() && !stale {
                                act = Some(Act::Goto(m.first));
                            }
                        }
                    });
                }
                Tab::Slow => {
                    let head = format!("{:>9}  {:<19} {:>6}  {:<7} {}", tr("Seconds"), tr("Time"), tr("Status"), tr("Method"), "URL");
                    ui.label(egui::RichText::new(head).font(font.clone()).weak());
                    let n = r.slow.len();
                    egui::ScrollArea::both().id_salt("report-slow").auto_shrink([false, false]).show_rows(ui, row_h, n, |ui, range| {
                        for s in &r.slow[range.start.min(n)..range.end.min(n)] {
                            let text = format!("{:>9}  {:<19} {:>6}  {:<7} {}  {}", secs(s.secs), s.time, s.status, s.method, s.url, s.client);
                            let resp = ui.add(egui::Button::selectable(false, egui::RichText::new(text).font(font.clone())).wrap_mode(egui::TextWrapMode::Extend));
                            let resp = resp.on_hover_ui(|ui| {
                                let line = d.line_bounds(s.offset, 4096).and_then(|(ls, le, _)| d.buf.read(ls.max(d.buf.codec.bom_len as u64)..le)).map(|b| d.buf.codec.decode_lossy(&b));
                                if let Some(l) = line.filter(|_| !stale) {
                                    ui.label(egui::RichText::new(l).monospace());
                                }
                                ui.weak(trf("{line} · click: go to the line", &[("line", &format!("{} {}", tr("Line"), fmt_count(s.line)))]));
                            });
                            if resp.clicked() && !stale {
                                act = Some(Act::Goto(s.offset));
                            }
                        }
                    });
                }
            }
        });
        self.log_report.open = open;
        self.log_report.tab = tab;
        self.log_report.worst_first = worst;
        self.log_report.only_errors = only;
        match act {
            Some(Act::FilterUrl(url)) => self.filter_by_url(&url),
            Some(Act::Goto(off)) => {
                if let Some(d) = self.docs.get_mut(self.active) {
                    d.goto_offset(off);
                }
            }
            Some(Act::Table(t)) => self.report_csv(t, false),
            Some(Act::Export(t)) => self.report_csv(t, true),
            Some(Act::Rerun) => self.start_log_report(),
            None => {}
        }
    }

    /// Filters the active document to the requests to `url` (a URL of its report).
    pub(crate) fn filter_by_url(&mut self, url: &str) {
        let Some(pattern) = self
            .docs
            .get(self.active)
            .and_then(|d| d.log_report.as_ref())
            .and_then(|r| {
                r.data
                    .lock()
                    .unwrap()
                    .format
                    .as_ref()
                    .map(|f| f.url_regex(url))
            })
        else {
            return;
        };
        self.search.query = Query {
            pattern,
            regex: true,
            case_sensitive: true,
            whole_word: false,
            dot_all: false,
        };
        self.search.invert = false;
        self.start_filter();
    }

    /// Writes a report as CSV to a file the user picks (`export`) or opens it as a table.
    pub(crate) fn report_csv(&mut self, tab: Tab, export: bool) {
        let Some(d) = self.docs.get(self.active) else {
            return;
        };
        let Some(r) = d
            .log_report
            .as_ref()
            .and_then(|x| x.data.lock().unwrap().r.clone())
        else {
            return;
        };
        let rows = match tab {
            Tab::Urls => r.urls.len(),
            Tab::Minutes => r.minutes.len(),
            Tab::Slow => r.slow.len(),
        } as u64;
        let write = move |w: &mut dyn std::io::Write| match tab {
            Tab::Urls => logformat::urls_csv(&r, w, None),
            Tab::Minutes => logformat::minutes_csv(&r, w),
            Tab::Slow => logformat::slow_csv(&r, w, None),
        };
        let stem = std::path::Path::new(&d.title)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "log".into());
        let name = format!("{stem}.{}.csv", tab.file_suffix());
        if export {
            let Some(target) = self.pick_output_file(tr("Export CSV"), &name) else {
                return;
            };
            self.spawn_task(
                tr("Saving").into(),
                move |_: &Progress, _: &CancelToken| -> TaskOutcome {
                    tasks::write_via_temp(&target, |w| {
                        write(w)?;
                        Ok(Some(trf(
                            "Saved {n} rows to {file}.",
                            &[("n", &fmt_count(rows)), ("file", &file_name(&target))],
                        )))
                    })
                },
            );
            return;
        }
        let target = bigtext::decompress::temp_file(&name);
        let title = format!("{} ({})", tr(tab.name()), d.title);
        self.spawn_task(
            tr("Saving").into(),
            move |_: &Progress, _: &CancelToken| -> TaskOutcome {
                let r = (|| {
                    if let Some(dir) = target.parent() {
                        std::fs::create_dir_all(dir)?;
                    }
                    let mut w = std::io::BufWriter::new(
                        std::fs::File::options()
                            .write(true)
                            .create_new(true)
                            .open(&target)?,
                    );
                    write(&mut w)?;
                    std::io::Write::flush(&mut w)
                })();
                match r {
                    Ok(()) => TaskOutcome::TempTable(Box::new(tasks::TempTable {
                        path: target,
                        title,
                        message: String::new(),
                    })),
                    Err(e) => {
                        bigtext::decompress::remove_temp(&target);
                        TaskOutcome::Failed(e.to_string())
                    }
                }
            },
        );
    }
}

fn pct(n: u64, of: u64) -> String {
    if of == 0 {
        "0%".into()
    } else {
        format!("{:.1}%", n as f64 * 100.0 / of as f64)
    }
}

fn secs(s: f64) -> String {
    format!("{s:.3}")
}

/// The most common status codes, coloured by class.
fn statuses_line(ui: &mut egui::Ui, r: &Report) {
    let mut list = r.statuses.clone();
    list.sort_by_key(|x| std::cmp::Reverse(x.1));
    ui.horizontal_wrapped(|ui| {
        for (s, n) in list.into_iter().take(12) {
            let text = egui::RichText::new(format!(
                "{} {}",
                if s == 0 {
                    "-".to_string()
                } else {
                    s.to_string()
                },
                fmt_count(n)
            ))
            .monospace();
            let text = match s {
                500..=599 => text.color(ui.visuals().error_fg_color),
                400..=499 => text.color(super::theme::warn_text(ui)),
                300..=399 | 0 => text.weak(),
                _ => text,
            };
            ui.label(text).on_hover_text(pct(n, r.requests));
        }
    });
}

/// Requests (grey) and 5xx errors (red) over time, a bar per minute or per several; the
/// offset of the first line of a clicked bar.
fn minutes_chart(ui: &mut egui::Ui, r: &Report) -> Option<u64> {
    let (first, last) = (r.minutes.first()?.minute, r.minutes.last()?.minute);
    let width = ui.available_width().max(100.0);
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(width, CHART_HEIGHT), egui::Sense::click());
    let span = last - first + 1;
    let max_bars = ((width / BAR_MIN).floor() as i64).max(1);
    let per = ((span + max_bars - 1) / max_bars).max(1);
    let n = ((last - first) / per + 1) as usize;
    let (mut req, mut err, mut firsts) = (vec![0u64; n], vec![0u64; n], vec![None::<u64>; n]);
    for m in &r.minutes {
        let i = ((m.minute - first) / per) as usize;
        req[i] += m.requests;
        err[i] += m.server_errors;
        firsts[i] = Some(firsts[i].map_or(m.first, |f| f.min(m.first)));
    }
    let (max_req, max_err) = (
        req.iter().copied().max().unwrap_or(0).max(1),
        err.iter().copied().max().unwrap_or(0),
    );
    let painter = ui.painter_at(rect);
    let v = ui.visuals();
    painter.rect_filled(rect, 2.0, v.extreme_bg_color);
    // Requests above, 5xx errors below, each to its own scale.
    let split = rect.top() + (rect.height() * 0.6).round();
    painter.hline(
        rect.x_range(),
        split,
        egui::Stroke::new(1.0, v.widgets.noninteractive.bg_stroke.color),
    );
    let bar_w = (rect.width() / n as f32).min(BAR_MAX);
    let gap = if bar_w >= 4.0 { 1.0 } else { 0.0 };
    let (grey, red) = (v.weak_text_color().gamma_multiply(0.6), v.error_fg_color);
    let (top_h, low_h) = (split - rect.top() - 3.0, rect.bottom() - split - 3.0);
    for i in 0..n {
        let x = rect.left() + i as f32 * bar_w;
        let h = req[i] as f32 / max_req as f32 * top_h;
        if req[i] > 0 {
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(x, split - h.max(1.0)),
                    egui::pos2(x + (bar_w - gap).max(1.0), split),
                ),
                0.0,
                grey,
            );
        }
        if err[i] > 0 {
            let h = (err[i] as f32 / max_err.max(1) as f32 * low_h).max(1.0);
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(x, rect.bottom() - h),
                    egui::pos2(x + (bar_w - gap).max(1.0), rect.bottom()),
                ),
                0.0,
                red,
            );
        }
    }
    let label = |m: i64| {
        logformat::Minute {
            minute: m,
            ..Default::default()
        }
        .label()
    };
    let mut clicked = None;
    if let Some(p) = resp.hover_pos() {
        let i = ((p.x - rect.left()) / bar_w) as usize;
        if i < n {
            let (from, to) = (first + i as i64 * per, first + (i as i64 + 1) * per - 1);
            let when = if per == 1 {
                label(from)
            } else {
                format!("{} – {}", label(from), label(to))
            };
            resp.clone().on_hover_text(format!(
                "{when}\n{}",
                trf(
                    "{n} requests, {e} 5xx",
                    &[("n", &fmt_count(req[i])), ("e", &fmt_count(err[i]))]
                )
            ));
            if resp.clicked() {
                clicked = firsts[i];
            }
        }
    }
    ui.horizontal_wrapped(|ui| {
        ui.weak(format!("{} – {}", label(first), label(last)));
        ui.weak(trf("Grey: requests (highest bar {m}); red, below: 5xx errors (highest bar {n}); {per} min per bar.", &[("n", &fmt_count(max_err)), ("m", &fmt_count(max_req)), ("per", &per.to_string())]));
    });
    clicked
}
