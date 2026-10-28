//! Filter view: the lines that match the search query (or, inverted, the lines that do not) in
//! a virtualized list under the text, with export to a file and copy to the clipboard.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId};

use bigtext::filter::{self, FilterSpec};
use bigtext::jobs::{self, human_bytes, CancelToken, Progress, Throttle};
use bigtext::search::{Matcher, Query, SearchEnd};
use bigtext::text::TextBuf;

use super::i18n::{tr, trf};
use super::tasks::{self, TaskOutcome};
use super::{same_path, BigTextApp, Msg};

/// Lines kept in the list; an export always writes all of them.
pub const FILTER_CAP: usize = 5_000_000;
/// "Copy lines" refuses to put more than this on the clipboard.
pub const COPY_MAX: u64 = 100 * 1024 * 1024;
/// Bytes of a line shown in the list.
const SHOW_MAX: u64 = 4096;

#[derive(Default)]
pub struct FilterData {
    /// Start offsets of the kept lines.
    pub lines: Vec<u64>,
    pub capped: bool,
    pub done: bool,
    pub end: Option<SearchEnd>,
    pub elapsed: Option<Duration>,
}

pub struct FilterResults {
    pub query: Query,
    pub spec: FilterSpec,
    pub matcher: Arc<Matcher>,
    pub version: u64,
    pub data: Arc<Mutex<FilterData>>,
    pub cancel: CancelToken,
    pub progress: Progress,
}

impl FilterResults {
    pub fn is_running(&self) -> bool {
        !self.data.lock().unwrap().done
    }
}

impl BigTextApp {
    /// Starts filtering the active document by the search query.
    pub(crate) fn start_filter(&mut self) {
        let Some(d) = self.docs.get(self.active) else { return };
        if !d.is_ready() {
            return;
        }
        if self.search.query.pattern.is_empty() {
            self.open_search(false);
            return;
        }
        let m = match self.search.matcher(d.buf.codec) {
            Ok(m) => m,
            Err(e) => {
                self.search.status = e;
                return;
            }
        };
        let column = self.search.filter_column.and_then(|c| Some((d.table_state.dialect?, c))).filter(|_| d.table);
        let spec = FilterSpec { invert: self.search.invert, column };
        let d = &mut self.docs[self.active];
        if let Some(r) = d.filter.take() {
            r.cancel.cancel();
        }
        let data = Arc::new(Mutex::new(FilterData::default()));
        let cancel = CancelToken::new();
        let progress = Progress::new(d.buf.len());
        d.filter = Some(FilterResults {
            query: self.search.query.clone(),
            spec,
            matcher: m.clone(),
            version: d.version,
            data: data.clone(),
            cancel: cancel.clone(),
            progress: progress.clone(),
        });
        self.search.show_filter = true;
        self.search.remember();
        let snap = d.buf.snapshot();
        let waker = self.waker.clone();
        jobs::spawn("filter", move || {
            let started = Instant::now();
            let mut batch: Vec<u64> = Vec::with_capacity(4096);
            let mut count = 0usize;
            let mut capped = false;
            let mut throttle = Throttle::new(Duration::from_millis(50));
            let end = filter::filter_lines(&snap, &m, spec, &cancel, Some(&progress), |ls, _| {
                if count >= FILTER_CAP {
                    capped = true;
                    return false;
                }
                batch.push(ls);
                count += 1;
                if batch.len() >= 4096 || throttle.ready() {
                    data.lock().unwrap().lines.append(&mut batch);
                    waker();
                }
                true
            });
            let mut g = data.lock().unwrap();
            g.lines.append(&mut batch);
            g.capped = capped;
            g.done = true;
            g.end = Some(end);
            g.elapsed = Some(started.elapsed());
            drop(g);
            waker();
        });
    }

    /// Writes every kept line (not only the listed ones) to a file the user picks.
    pub(crate) fn export_filter(&mut self) {
        let Some(d) = self.docs.get(self.active) else { return };
        let Some(r) = &d.filter else { return };
        if r.version != d.version {
            self.error = Some(tr("The document changed since the filter ran. Run the filter again first.").into());
            return;
        }
        let (snap, m, spec, name) = (d.buf.snapshot(), r.matcher.clone(), r.spec, d.title.clone());
        self.save_lines_dialog(snap, m, spec, &name);
    }

    /// Asks for a file and writes the lines that `m` and `spec` keep to it.
    pub(crate) fn save_lines_dialog(&mut self, snap: TextBuf, m: Arc<Matcher>, spec: FilterSpec, doc_title: &str) {
        let Some(target) = self.pick_output_file(tr("Save lines"), &format!("{doc_title}.lines.txt")) else { return };
        self.spawn_task(tr("Saving lines").into(), move |progress, cancel| export_lines(&snap, &m, spec, &target, progress, cancel));
    }

    /// Save dialog for a new output file next to the active document; refuses files open in
    /// BigText (they are memory-mapped and cannot be replaced).
    pub(crate) fn pick_output_file(&mut self, title: &str, name: &str) -> Option<std::path::PathBuf> {
        let mut dlg = rfd::FileDialog::new().set_title(title).set_file_name(name);
        if let Some(dir) = self.docs.get(self.active).and_then(|d| d.path.as_ref()).and_then(|p| p.parent()) {
            dlg = dlg.set_directory(bigtext::file::display_path(dir));
        }
        let target = dlg.save_file()?;
        if self.docs.iter().any(|d| d.path.as_deref().is_some_and(|p| same_path(p, &target))) {
            self.error = Some(tr("That file is open in BigText. Choose another file.").into());
            return None;
        }
        Some(target)
    }

    /// Copies the kept lines to the clipboard (up to [`COPY_MAX`]).
    pub(crate) fn copy_filter(&mut self) {
        let Some(d) = self.docs.get(self.active) else { return };
        let Some(r) = &d.filter else { return };
        let (snap, m, spec, id) = (d.buf.snapshot(), r.matcher.clone(), r.spec, d.id);
        self.copy_lines(snap, m, spec, id);
    }

    /// Copies the lines that `m` and `spec` keep to the clipboard (up to [`COPY_MAX`]).
    pub(crate) fn copy_lines(&mut self, snap: TextBuf, m: Arc<Matcher>, spec: FilterSpec, id: u64) {
        let tx = self.tx.clone();
        let waker = self.waker.clone();
        jobs::spawn("copy-lines", move || {
            let mut out = Vec::new();
            let r = filter::write_lines_to(&snap, &m, spec, &mut CappedVec(&mut out, COPY_MAX), &CancelToken::new(), None);
            let result = match r {
                Ok(_) => Ok(snap.codec.decode_lossy(&out)),
                Err(e) if e.kind() == std::io::ErrorKind::OutOfMemory => {
                    Err(trf("The lines are larger than {size}. Use “Save lines…” instead.", &[("size", &human_bytes(COPY_MAX))]))
                }
                Err(e) => Err(e.to_string()),
            };
            let _ = tx.send(Msg::Copied { doc: id, result, cut: false });
            waker();
        });
    }

    pub(super) fn filter_panel(&mut self, ui: &mut egui::Ui) {
        let font = FontId::monospace(self.settings.font_size.min(14.0));
        let mut close = false;
        let mut export = false;
        let mut copy = false;
        let mut rerun = false;
        let mut jump = None;
        {
            let Some(d) = self.docs.get_mut(self.active) else { return };
            let Some(r) = &d.filter else { return };
            let (n, capped, done, elapsed, end) = {
                let g = r.data.lock().unwrap();
                (g.lines.len(), g.capped, g.done, g.elapsed, g.end)
            };
            let stale = r.version != d.version;
            ui.horizontal(|ui| {
                let what = if r.spec.invert { tr("lines without") } else { tr("lines with") };
                ui.strong(format!("{} {what} “{}”", fmt_count(n as u64), r.query.pattern));
                if let Some((_, c)) = r.spec.column {
                    ui.label(trf("in column {n}", &[("n", &(c + 1).to_string())]));
                }
                if capped {
                    ui.label(trf("(list capped at {n}; Save lines writes all)", &[("n", &fmt_count(FILTER_CAP as u64))]));
                }
                if !done {
                    let f = r.progress.fraction();
                    ui.add(egui::ProgressBar::new(f).desired_width(160.0).text(format!("{} {:.0}%", tr("filtering"), f * 100.0)));
                    ui.ctx().request_repaint_after(Duration::from_millis(50));
                    if ui.small_button(tr("Cancel")).clicked() {
                        r.cancel.cancel();
                    }
                } else if let Some(t) = elapsed {
                    let note = if end == Some(SearchEnd::Cancelled) { tr(" (cancelled)") } else { "" };
                    ui.label(format!("{} {:.2}s{note}", tr("in"), t.as_secs_f64()));
                }
                if stale {
                    ui.colored_label(Color32::from_rgb(220, 150, 60), tr("stale (document changed)"));
                    if ui.small_button(tr("Run again")).clicked() {
                        rerun = true;
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("×").clicked() {
                        close = true;
                    }
                    if ui.add_enabled(done && !stale, egui::Button::new(tr("Save lines…")).small()).clicked() {
                        export = true;
                    }
                    if ui.add_enabled(done && !stale && n > 0, egui::Button::new(tr("Copy lines")).small()).clicked() {
                        copy = true;
                    }
                });
            });
            let row_h = ui.ctx().fonts_mut(|f| f.row_height(&font)) + 2.0;
            let data = r.data.clone();
            let m = r.matcher.clone();
            let buf = &d.buf;
            let caret_line = d.line_bounds(d.view.caret, 64 * 1024).map(|b| b.0);
            egui::ScrollArea::both().auto_shrink([false, false]).show_rows(ui, row_h, n, |ui, range| {
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                let lines: Vec<u64> = {
                    let g = data.lock().unwrap();
                    g.lines[range.start.min(g.lines.len())..range.end.min(g.lines.len())].to_vec()
                };
                for ls in lines {
                    let job = line_job(buf, ls, &m, &font, ui.visuals(), stale);
                    if ui.selectable_label(caret_line == Some(ls) && !stale, job).clicked() {
                        jump = Some(ls);
                    }
                }
            });
            if let (Some(ls), false) = (jump, stale) {
                d.set_caret(ls.max(d.min_caret()), false);
                d.view.center_caret = true;
            }
            if close {
                if let Some(r) = d.filter.take() {
                    r.cancel.cancel();
                }
            }
        }
        if close {
            self.search.show_filter = false;
        }
        if rerun {
            if let Some(q) = self.docs.get(self.active).and_then(|d| d.filter.as_ref()).map(|r| (r.query.clone(), r.spec.invert)) {
                self.search.query = q.0;
                self.search.invert = q.1;
                self.start_filter();
            }
        }
        if export {
            self.export_filter();
        }
        if copy {
            self.copy_filter();
        }
    }
}

pub(super) fn export_lines(snap: &TextBuf, m: &Matcher, spec: FilterSpec, target: &std::path::Path, progress: &Progress, cancel: &CancelToken) -> TaskOutcome {
    progress.set_total(snap.len());
    tasks::write_via_temp(target, |w| {
        Ok(filter::write_lines_to(snap, m, spec, w, cancel, Some(progress))?
            .map(|n| trf("Saved {lines} lines ({size}) to {file}.", &[("lines", &fmt_count(n.lines)), ("size", &human_bytes(n.bytes)), ("file", &file_name(target))])))
    })
}

pub(super) fn file_name(p: &std::path::Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| bigtext::file::display_path(p))
}

/// 1234567 → "1,234,567".
pub fn fmt_count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A writer that fails with `OutOfMemory` once more than the limit was written.
struct CappedVec<'a>(&'a mut Vec<u8>, u64);

impl std::io::Write for CappedVec<'_> {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        if self.0.len() as u64 + b.len() as u64 > self.1 {
            return Err(std::io::Error::from(std::io::ErrorKind::OutOfMemory));
        }
        self.0.extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// One list row: line number and the line's text with its matches highlighted. When the first
/// match is far to the right, the text starts a little before it.
fn line_job(buf: &TextBuf, ls: u64, m: &Matcher, font: &FontId, v: &egui::Visuals, stale: bool) -> LayoutJob {
    let mut job = LayoutJob::default();
    let weak = TextFormat { font_id: font.clone(), color: v.weak_text_color(), ..Default::default() };
    let normal = TextFormat { font_id: font.clone(), color: if stale { v.weak_text_color() } else { v.text_color() }, ..Default::default() };
    let hl = TextFormat { font_id: font.clone(), color: v.strong_text_color(), background: v.selection.bg_fill, ..Default::default() };
    let (line, exact) = buf.line_of(ls.min(buf.len()));
    job.append(&format!("{}{:>8}  ", if exact { " " } else { "~" }, line + 1), 0.0, weak.clone());
    let codec = buf.codec;
    let start = ls.max(codec.bom_len as u64).min(buf.len());
    let Some(raw) = buf.read(start..(start + SHOW_MAX).min(buf.len())) else {
        job.append("…", 0.0, normal);
        return job;
    };
    let cut = codec.find_nl(&raw, start).unwrap_or(raw.len());
    let mut raw = &raw[..cut];
    // Drop the CR of a CRLF line break.
    let cr = codec.cr_unit();
    if raw.ends_with(cr) && codec.nl == bigtext::encoding::NlByte::Lf {
        raw = &raw[..raw.len() - cr.len()];
    }
    let end = start + raw.len() as u64;
    let hits = bigtext::search::matches_in_view(buf, m, start..end, 64);
    let mut from = start;
    if let Some(&(s, _)) = hits.first() {
        if s > start + 160 {
            let skip = buf.snap_char(s - 60).max(start);
            job.append("…", 0.0, weak);
            from = skip;
        }
    }
    let clean = |b: &[u8]| -> String { codec.decode_lossy(b).chars().map(|c| if c == '\t' { ' ' } else if c.is_control() { '·' } else { c }).collect() };
    let slice = |a: u64, b: u64| &raw[(a - start) as usize..(b - start) as usize];
    for &(s, e) in &hits {
        let (s, e) = (s.clamp(from, end), e.clamp(from, end));
        if e <= from || s >= end {
            continue;
        }
        if s > from {
            job.append(&clean(slice(from, s)), 0.0, normal.clone());
        }
        job.append(&clean(slice(s, e)), 0.0, hl.clone());
        from = e;
    }
    if from < end {
        job.append(&clean(slice(from, end)), 0.0, normal);
    }
    job
}
