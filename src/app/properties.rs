//! File → Properties… (Ctrl+I): the document's size, backing, encoding and origin, one
//! background pass for its lines, line breaks, longest line, NUL and non-UTF-8 bytes, and on
//! request its SHA-256.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bigtext::jobs::{self, human_bytes, CancelToken, Progress};
use bigtext::stats::{self, Stats};
use bigtext::timestamp::{self, Stamp};

use super::filter_panel::fmt_count;
use super::i18n::{tr, trf};
use super::BigTextApp;

#[derive(Default)]
pub struct PropsData {
    /// The figures so far (`done` once the pass is over).
    pub stats: Option<Stats>,
    pub span: Option<(Stamp, Stamp)>,
    pub failed: bool,
    /// The SHA-256 in hex, once computed.
    pub sha: Option<String>,
}

pub struct PropertiesUi {
    pub doc: u64,
    pub version: u64,
    pub data: Arc<Mutex<PropsData>>,
    pub cancel: CancelToken,
    pub progress: Progress,
    /// The SHA-256 being computed.
    pub sha: Option<(CancelToken, Progress)>,
}

impl Drop for PropertiesUi {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some((c, _)) = &self.sha {
            c.cancel();
        }
    }
}

impl BigTextApp {
    pub(crate) fn open_properties(&mut self) {
        let Some(d) = self.docs.get(self.active) else { return };
        if !d.is_ready() {
            return;
        }
        let snap = d.buf.snapshot();
        let data = Arc::new(Mutex::new(PropsData::default()));
        let (cancel, progress) = (CancelToken::new(), Progress::default());
        self.properties = Some(PropertiesUi { doc: d.id, version: d.version, data: data.clone(), cancel: cancel.clone(), progress: progress.clone(), sha: None });
        let waker = self.waker.clone();
        jobs::spawn("properties", move || {
            let r = stats::scan(&snap, &cancel, Some(&progress), |s| {
                data.lock().unwrap().stats = Some(*s);
                waker();
            });
            let span = r.is_some().then(|| stats::time_span(&snap)).flatten();
            let mut g = data.lock().unwrap();
            match r {
                Some(s) => g.stats = Some(s),
                None => g.failed = !cancel.is_cancelled(),
            }
            g.span = span;
            drop(g);
            waker();
        });
    }

    pub(super) fn start_sha(&mut self) {
        let Some(p) = &mut self.properties else { return };
        let Some(d) = self.docs.iter().find(|d| d.id == p.doc) else { return };
        let snap = d.buf.snapshot();
        let (cancel, progress) = (CancelToken::new(), Progress::default());
        p.sha = Some((cancel.clone(), progress.clone()));
        let (data, waker) = (p.data.clone(), self.waker.clone());
        jobs::spawn("sha256", move || {
            if let Some(h) = stats::sha256(&snap, &cancel, Some(&progress)) {
                data.lock().unwrap().sha = Some(h);
            }
            waker();
        });
    }

    pub(super) fn properties_window(&mut self, ctx: &egui::Context) {
        let Some(p) = &self.properties else { return };
        let Some(di) = self.docs.iter().position(|d| d.id == p.doc) else {
            self.properties = None;
            return;
        };
        let d = &self.docs[di];
        let (st, span, failed, sha) = {
            let g = p.data.lock().unwrap();
            (g.stats, g.span, g.failed, g.sha.clone())
        };
        let done = st.is_some_and(|s| s.done);
        let sha_running = p.sha.as_ref().filter(|_| sha.is_none()).map(|(_, pr)| pr.fraction());
        let mut open = true;
        let (mut goto_line, mut goto_off, mut want_sha, mut copy) = (None, None, false, None);
        let path = d.path.as_deref().map(bigtext::file::display_path);
        egui::Window::new(trf("Properties of {name}", &[("name", &d.title)])).open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
            egui::Grid::new("properties").num_columns(2).spacing([16.0, 4.0]).striped(true).show(ui, |ui| {
                let row = |ui: &mut egui::Ui, k: &str, v: String| {
                    ui.strong(k);
                    ui.label(v);
                    ui.end_row();
                };
                if let Some(from) = &d.unpacked_from {
                    let mut v = bigtext::file::display_path(from);
                    if let Some(m) = &d.member {
                        v.push_str(" › ");
                        v.push_str(m);
                    }
                    row(ui, tr("Unpacked from"), v);
                } else if let Some(p) = &path {
                    row(ui, tr("File"), p.clone());
                }
                let len = d.buf.len();
                row(ui, tr("Size"), format!("{} ({} {})", human_bytes(len), fmt_count(len), tr("bytes")));
                let backing = if d.buf.source.is_mmap() { tr("mapped into memory") } else { tr("read in chunks") };
                row(ui, tr("Reading"), backing.into());
                let bom = if d.buf.codec.bom_len > 0 { tr(" with a byte order mark") } else { "" };
                row(ui, tr("Encoding"), format!("{}{bom}", d.buf.codec.enc.name()));
                let reason = if d.temporary { tr("yes (a temporary copy)") } else if d.read_only { tr("yes") } else { tr("no") };
                row(ui, tr("Read-only"), reason.into());
                if let Some(t) = d.buf.source.modified() {
                    row(ui, tr("Changed on disk"), super::archive::date(t));
                }
                if let Some(t) = d.open_elapsed {
                    row(ui, tr("Opened in"), format!("{:.2} s", t.as_secs_f64()));
                }
                match st {
                    Some(s) => {
                        let counting = if s.done { String::new() } else { format!(" ({})", tr("counting…")) };
                        row(ui, tr("Lines"), format!("{}{counting}", fmt_count(s.lines)));
                        row(ui, tr("Line breaks"), format!("{} · LF {} · CRLF {} · CR {}", d.line_ending.name(), fmt_count(s.lf), fmt_count(s.crlf), fmt_count(s.cr)));
                        ui.strong(tr("Longest line"));
                        ui.horizontal(|ui| {
                            ui.label(trf("{n} bytes, line {line}", &[("n", &fmt_count(s.longest.0)), ("line", &fmt_count(s.longest.1 + 1))]));
                            if ui.small_button(tr("Go to")).clicked() {
                                goto_line = Some(s.longest.1 + 1);
                            }
                        });
                        ui.end_row();
                        ui.strong(tr("NUL bytes"));
                        ui.horizontal(|ui| {
                            ui.label(fmt_count(s.nul));
                            if let Some(at) = s.first_nul {
                                if ui.small_button(tr("Go to the first")).clicked() {
                                    goto_off = Some(at);
                                }
                            }
                        });
                        ui.end_row();
                        if d.buf.codec.enc == bigtext::encoding::Encoding::Utf8 {
                            ui.strong(tr("Not UTF-8"));
                            ui.horizontal(|ui| {
                                ui.label(trf("{n} bytes", &[("n", &fmt_count(s.invalid_utf8))]));
                                if let Some(at) = s.first_invalid {
                                    if ui.small_button(tr("Go to the first")).clicked() {
                                        goto_off = Some(at);
                                    }
                                }
                            });
                            ui.end_row();
                        }
                        if s.done {
                            row(ui, tr("Ends with a line break"), if s.final_break { tr("yes") } else { tr("no") }.into());
                        }
                    }
                    None if failed => row(ui, tr("Lines"), tr("Reading the document failed.").into()),
                    None => row(ui, tr("Lines"), tr("counting…").into()),
                }
                if let Some((f, l)) = span {
                    let key = |s: Stamp| timestamp::format_key(s.ms, s.has_date, true);
                    row(ui, tr("First time"), key(f));
                    row(ui, tr("Last time"), key(l));
                    if f.has_date && l.has_date {
                        row(ui, tr("Span"), timestamp::format_delta(l.ms - f.ms).trim_start_matches('+').to_string());
                    }
                }
                ui.strong("SHA-256");
                ui.horizontal(|ui| match (&sha, sha_running) {
                    (Some(h), _) => {
                        ui.monospace(h);
                        if ui.small_button(tr("Copy")).clicked() {
                            copy = Some(h.clone());
                        }
                    }
                    (None, Some(f)) => {
                        ui.add(egui::ProgressBar::new(f).desired_width(160.0).show_percentage());
                        ui.ctx().request_repaint_after(Duration::from_millis(100));
                    }
                    (None, None) => want_sha = ui.small_button(tr("Compute")).clicked(),
                });
                ui.end_row();
            });
            if !done && !failed {
                let f = p.progress.fraction();
                ui.add(egui::ProgressBar::new(f).desired_width(260.0).show_percentage());
                ui.ctx().request_repaint_after(Duration::from_millis(100));
            }
            if d.is_modified() || d.version != p.version {
                ui.weak(tr("The figures include the unsaved changes."));
            }
        });
        if let Some(line) = goto_line {
            self.active = di;
            self.docs[di].goto_line(line);
        }
        if let Some(off) = goto_off {
            self.active = di;
            self.docs[di].goto_offset(off);
        }
        if want_sha {
            self.start_sha();
        }
        if let Some(h) = copy {
            ctx.copy_text(h);
        }
        if !open {
            self.properties = None;
        }
    }
}
