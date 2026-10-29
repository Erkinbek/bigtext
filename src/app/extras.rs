//! Recent files and session restore, file comparison (streaming diff) and its results panel.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bigtext::diff::{self, DiffEnd, Hunk};
use bigtext::file::{self, OpenMode, Source};
use bigtext::jobs::{self, CancelToken, Progress};

use super::document::{Document, Restore};
use super::i18n::{tr, trf};
use super::tasks::TaskOutcome;
use crate::view::text_view::{self, DiffOverlay, ViewEnv};
use super::BigTextApp;

pub const RECENT_MAX: usize = 15;
pub const DIFF_CAP: usize = 100_000;

// ---------------------------------------------------------------------------------------------
// Persistence: a small key=value text file in the user's config directory.

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TabState {
    pub path: PathBuf,
    /// The file of the archive `path` that was open.
    pub member: Option<String>,
    pub caret: u64,
    pub top: u64,
    pub wrap: bool,
    pub bookmarks: Vec<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct Persisted {
    pub recent: Vec<PathBuf>,
    pub tabs: Vec<TabState>,
    pub active: usize,
    pub font_size: Option<f32>,
    pub dark: Option<bool>,
    pub backup_on_save: Option<bool>,
    pub syntax: Option<bool>,
    pub update_check: Option<bool>,
    pub skip_version: Option<String>,
    /// Unix time of the last update check.
    pub update_checked: Option<u64>,
    pub marks: Vec<super::marks::Mark>,
    /// Search history, newest first.
    pub history: Vec<String>,
    /// Changed key bindings: command id and binding ("none" = removed).
    pub keys: Vec<(String, String)>,
    /// Interface language code (en, ru, uz).
    pub lang: Option<String>,
    /// Font file of the text.
    pub font: Option<PathBuf>,
    pub level_bar: Option<bool>,
    /// Saved searches.
    pub saved: Vec<super::saved_searches::SavedSearch>,
    pub show_ws: Option<bool>,
    /// Paths of pinned tabs.
    pub pinned: Vec<PathBuf>,
    /// Filter view: context lines and whole entries.
    pub filter_ctx: Option<(u32, bool)>,
    /// Keys whose values are ids (Search → Id patterns…).
    pub id_keys: Option<Vec<String>>,
}

pub(super) fn query_flags(q: &bigtext::search::Query) -> String {
    [(q.regex, 'r'), (q.case_sensitive, 'c'), (q.whole_word, 'w'), (q.dot_all, 'd')].iter().filter(|x| x.0).map(|x| x.1).collect()
}

pub(super) fn query_from(pattern: String, flags: &str) -> bigtext::search::Query {
    bigtext::search::Query { pattern, regex: flags.contains('r'), case_sensitive: flags.contains('c'), whole_word: flags.contains('w'), dot_all: flags.contains('d') }
}

/// "name|flags|invert|column|pattern".
fn parse_saved(v: &str) -> Option<super::saved_searches::SavedSearch> {
    let mut it = v.splitn(5, '|');
    let name = it.next()?.to_string();
    let flags = it.next()?;
    let invert = it.next()? == "1";
    let column = it.next()?.parse().ok();
    let query = query_from(it.next()?.to_string(), flags);
    Some(super::saved_searches::SavedSearch { name, query, invert, column })
}

/// "enabled|color|flags|pattern".
pub(super) fn mark_line(m: &super::marks::Mark) -> String {
    format!("{}|{}|{}|{}", if m.enabled { "1" } else { "0" }, m.color, query_flags(&m.query), m.query.pattern)
}

/// Between an archive's path and its file in a tab line.
const MEMBER_SEP: char = '\u{1f}';

/// "caret|top|wrap|bookmarks|path", the path followed by U+001F and the archive's file when
/// it is one.
pub(super) fn tab_line(t: &TabState) -> String {
    let bm: Vec<String> = t.bookmarks.iter().map(|x| x.to_string()).collect();
    let member = t.member.as_deref().map(|m| format!("{MEMBER_SEP}{m}")).unwrap_or_default();
    format!("{}|{}|{}|{}|{}{member}", t.caret, t.top, if t.wrap { "1" } else { "0" }, bm.join(","), t.path.display())
}

pub(super) fn parse_tab(v: &str) -> Option<TabState> {
    let parts: Vec<&str> = v.splitn(5, '|').collect();
    let (path, member) = match parts.get(4)?.split_once(MEMBER_SEP) {
        Some((p, m)) => (p, Some(m.to_string())),
        None => (parts[4], None),
    };
    (parts.len() == 5).then(|| TabState {
        caret: parts[0].parse().unwrap_or(0),
        top: parts[1].parse().unwrap_or(0),
        wrap: parts[2] == "1",
        bookmarks: parts[3].split(',').filter_map(|x| x.parse().ok()).collect(),
        path: PathBuf::from(path),
        member,
    })
}

pub(super) fn parse_mark(v: &str) -> Option<super::marks::Mark> {
    let mut it = v.splitn(4, '|');
    let enabled = it.next()? == "1";
    let color = it.next()?.parse().ok()?;
    let flags = it.next()?;
    let pattern = it.next()?.to_string();
    let query = bigtext::search::Query {
        pattern,
        regex: flags.contains('r'),
        case_sensitive: flags.contains('c'),
        whole_word: flags.contains('w'),
        dot_all: flags.contains('d'),
    };
    Some(super::marks::Mark { query, color, enabled })
}

pub fn state_file() -> Option<PathBuf> {
    if std::env::var_os("BIGTEXT_NO_STATE").is_some() {
        return None;
    }
    // Portable mode: a BigText-data folder next to the program.
    if let Some(d) = super::portable::portable_dir() {
        return Some(d.join("state.txt"));
    }
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library").join("Application Support"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    }?;
    Some(base.join("BigText").join("state.txt"))
}

pub fn load() -> Persisted {
    let Some(path) = state_file() else { return Persisted::default() };
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text),
        Err(_) => Persisted::default(),
    }
}

pub(super) fn parse(text: &str) -> Persisted {
    let mut p = Persisted::default();
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        match k {
            "recent" => p.recent.push(PathBuf::from(v)),
            "active" => p.active = v.parse().unwrap_or(0),
            "font_size" => p.font_size = v.parse().ok(),
            "dark" => p.dark = Some(v == "1"),
            "backup_on_save" => p.backup_on_save = Some(v == "1"),
            "syntax" => p.syntax = Some(v == "1"),
            "level_bar" => p.level_bar = Some(v == "1"),
            "show_ws" => p.show_ws = Some(v == "1"),
            "id_keys" => p.id_keys = Some(v.split(',').filter(|k| !k.is_empty()).map(String::from).collect()),
            "filter_ctx" => p.filter_ctx = v.split_once('|').and_then(|(n, e)| Some((n.parse().ok()?, e == "1"))),
            "update_check" => p.update_check = Some(v == "1"),
            "skip_version" => p.skip_version = Some(v.to_string()),
            "update_checked" => p.update_checked = v.parse().ok(),
            "mark" => p.marks.extend(parse_mark(v)),
            "saved" => p.saved.extend(parse_saved(v)),
            "pinned" => p.pinned.push(PathBuf::from(v)),
            "find" => p.history.push(v.to_string()),
            "lang" => p.lang = Some(v.to_string()),
            "font" => p.font = Some(PathBuf::from(v)),
            "key" => p.keys.extend(v.split_once('=').map(|(a, b)| (a.to_string(), b.to_string()))),
            "tab" => p.tabs.extend(parse_tab(v)),
            _ => {}
        }
    }
    p
}

pub(super) fn serialize(p: &Persisted) -> String {
    let mut s = String::from("# BigText session state\n");
    let b = |x: bool| if x { "1" } else { "0" };
    if let Some(f) = p.font_size {
        s.push_str(&format!("font_size={f}\n"));
    }
    if let Some(d) = p.dark {
        s.push_str(&format!("dark={}\n", b(d)));
    }
    if let Some(d) = p.backup_on_save {
        s.push_str(&format!("backup_on_save={}\n", b(d)));
    }
    if let Some(d) = p.syntax {
        s.push_str(&format!("syntax={}\n", b(d)));
    }
    if let Some(k) = &p.id_keys {
        s.push_str(&format!("id_keys={}\n", k.join(",")));
    }
    if let Some((n, e)) = p.filter_ctx {
        s.push_str(&format!("filter_ctx={n}|{}\n", b(e)));
    }
    if let Some(d) = p.show_ws {
        s.push_str(&format!("show_ws={}\n", b(d)));
    }
    if let Some(d) = p.level_bar {
        s.push_str(&format!("level_bar={}\n", b(d)));
    }
    if let Some(d) = p.update_check {
        s.push_str(&format!("update_check={}\n", b(d)));
    }
    if let Some(v) = &p.skip_version {
        s.push_str(&format!("skip_version={v}\n"));
    }
    if let Some(f) = &p.font {
        s.push_str(&format!("font={}\n", f.display()));
    }
    if let Some(l) = &p.lang {
        s.push_str(&format!("lang={l}\n"));
    }
    if let Some(t) = p.update_checked {
        s.push_str(&format!("update_checked={t}\n"));
    }
    for r in &p.recent {
        s.push_str(&format!("recent={}\n", r.display()));
    }
    for m in &p.marks {
        s.push_str(&format!("mark={}\n", mark_line(m)));
    }
    for x in p.saved.iter().filter(|x| !x.query.pattern.contains(['\n', '\r'])) {
        let col = x.column.map(|c| c.to_string()).unwrap_or_default();
        s.push_str(&format!("saved={}|{}|{}|{col}|{}\n", x.name.replace('|', "/"), query_flags(&x.query), b(x.invert), x.query.pattern));
    }
    for x in &p.pinned {
        s.push_str(&format!("pinned={}\n", x.display()));
    }
    for (cmd, k) in &p.keys {
        s.push_str(&format!("key={cmd}={k}\n"));
    }
    for h in p.history.iter().filter(|h| !h.contains(['\n', '\r'])) {
        s.push_str(&format!("find={h}\n"));
    }
    for t in &p.tabs {
        s.push_str(&format!("tab={}\n", tab_line(t)));
    }
    s.push_str(&format!("active={}\n", p.active));
    s
}

/// Writes the state on a worker thread (never blocks the UI).
pub fn save_async(p: Persisted) {
    let Some(path) = state_file() else { return };
    let text = serialize(&p);
    jobs::spawn("save-state", move || write_state(&path, &text));
}

/// Writes the state before returning: on exit, when a new BigText process reads it right away.
pub fn save_now(p: Persisted) {
    if let Some(path) = state_file() {
        write_state(&path, &serialize(&p));
    }
}

fn write_state(path: &Path, text: &str) {
    // One writer at a time, since all of them use the same temporary file.
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

// ---------------------------------------------------------------------------------------------
// File comparison.

#[derive(Default)]
pub struct DiffData {
    pub hunks: Vec<Hunk>,
    pub done: bool,
    pub capped: bool,
    pub end: Option<DiffEnd>,
    pub error: Option<String>,
    pub other: Option<Arc<Source>>,
    pub elapsed: Option<Duration>,
}

/// What the comparison ignores (the compare panel's switches).
#[derive(Clone, Default, PartialEq, Eq)]
pub struct DiffSwitches {
    pub whitespace: bool,
    pub case: bool,
    pub timestamps: bool,
    pub mask: String,
}

impl DiffSwitches {
    pub fn options(&self) -> Result<diff::DiffOptions, String> {
        let mask = match self.mask.trim() {
            "" => None,
            m => Some(regex_automata::meta::Regex::new(m).map_err(|e| trf("Mask: {error}", &[("error", &e.to_string())]))?),
        };
        Ok(diff::DiffOptions { whitespace: self.whitespace, case: self.case, timestamps: self.timestamps, mask })
    }

    /// The switches that are on, "white space, case", or `None`.
    pub fn summary(&self) -> Option<String> {
        let mask = self.mask.trim();
        let parts: Vec<String> = [(self.whitespace, tr("white space")), (self.case, tr("case")), (self.timestamps, tr("timestamps"))]
            .into_iter()
            .filter(|(on, _)| *on)
            .map(|(_, s)| s.to_string())
            .chain((!mask.is_empty()).then(|| format!("{} {mask}", tr("mask"))))
            .collect();
        (!parts.is_empty()).then(|| parts.join(", "))
    }
}

/// What a document is compared with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiffOther {
    /// Another file.
    File(PathBuf),
    /// The document's own file as saved, so its unsaved edits show.
    Saved,
    /// The copy saving left beside the file (`name.bak`).
    Backup(PathBuf),
}

impl DiffOther {
    /// The file B is read from, when it is not the document's own.
    pub fn path(&self) -> Option<&Path> {
        match self {
            DiffOther::File(p) | DiffOther::Backup(p) => Some(p),
            DiffOther::Saved => None,
        }
    }
}

/// The other file of a comparison, shown beside the document (View → Compare side by side).
pub struct SideBySide {
    pub doc: Box<Document>,
    pub path: PathBuf,
    /// The tops of A and B after the last scroll sync.
    pub synced: (u64, u64),
    /// A pane still to move to a line (true: A) once its offset is exact.
    pub pending: Option<(bool, u64)>,
}

impl Drop for SideBySide {
    fn drop(&mut self) {
        self.doc.cancel_jobs();
        self.doc.watch_cancel.cancel();
    }
}

/// The ranges of `hunks`' lines on one side (`a`: A) that meet `range`; a hunk without lines on
/// that side gives an empty range where the other side's lines go.
pub fn diff_spans(hunks: &[Hunk], range: std::ops::Range<u64>, a: bool) -> Vec<std::ops::Range<u64>> {
    let span = |h: &Hunk| if a { h.a_off..h.a_end } else { h.b_off..h.b_end };
    let k = hunks.partition_point(|h| span(h).end < range.start);
    hunks[k..].iter().map(span).take_while(|s| s.start <= range.end).collect()
}

/// The lengths of the common start and end of two lines, on character boundaries.
fn prefix_suffix(a: &[u8], b: &[u8]) -> (usize, usize) {
    let mut p = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    while p > 0 && p < a.len() && a[p] & 0xC0 == 0x80 {
        p -= 1;
    }
    let room = a.len().min(b.len()) - p;
    let mut s = a.iter().rev().zip(b.iter().rev()).take(room).take_while(|(x, y)| x == y).count();
    while s > 0 && a[a.len() - s] & 0xC0 == 0x80 {
        s -= 1;
    }
    (p, s)
}

/// The changed middle of each line of the hunks that replace as many lines as they remove (and
/// are not long), on one side, within `range`.
fn changed_parts(hunks: &[Hunk], own: &bigtext::text::TextBuf, other: &bigtext::text::TextBuf, range: std::ops::Range<u64>, a: bool, color: egui::Color32) -> Vec<(u64, u64, egui::Color32)> {
    const MAX_HUNK: u64 = 256 << 10;
    const MAX_LINE: usize = 16 << 10;
    let trim = |l: &[u8]| -> usize {
        let l = l.strip_suffix(b"\n").unwrap_or(l);
        l.strip_suffix(b"\r").unwrap_or(l).len()
    };
    let mut out = Vec::new();
    let k = hunks.partition_point(|h| (if a { h.a_end } else { h.b_end }) <= range.start);
    for h in &hunks[k..] {
        let (mine, theirs) = if a { (h.a_off..h.a_end, h.b_off..h.b_end) } else { (h.b_off..h.b_end, h.a_off..h.a_end) };
        if mine.start >= range.end {
            break;
        }
        if h.a_count != h.b_count || h.a_count > 200 || mine.end - mine.start > MAX_HUNK || theirs.end - theirs.start > MAX_HUNK {
            continue;
        }
        let (Some(x), Some(y)) = (own.read(mine.clone()), other.read(theirs)) else { continue };
        let mut at = mine.start;
        for (lx, ly) in x.split_inclusive(|&c| c == b'\n').zip(y.split_inclusive(|&c| c == b'\n')) {
            let (nx, ny) = (trim(lx), trim(ly));
            if nx <= MAX_LINE && ny <= MAX_LINE && lx[..nx] != ly[..ny] {
                let (p, s) = prefix_suffix(&lx[..nx], &ly[..ny]);
                out.push((at + p as u64, at + (nx - s) as u64, color));
            }
            at += lx.len() as u64;
        }
    }
    out
}

/// The offset to show `line` at the top, and whether it is exact (the index may still be
/// counting lines).
fn line_top(buf: &bigtext::text::TextBuf, line: u64) -> (u64, bool) {
    match buf.line_start(line) {
        Some((o, true)) => (o, true),
        Some((o, false)) => (bigtext::text::rows::seg_start(buf, o).unwrap_or(o), false),
        None => {
            let (n, exact) = buf.line_count();
            (buf.line_start(n.saturating_sub(1)).map(|(o, _)| o).unwrap_or(0), exact)
        }
    }
}

/// Keeps the two panes at corresponding lines: the one the user moved leads (B only while the
/// pointer is over it). A line found by estimate is looked up again until it is exact.
fn sync_tops(a: &mut Document, side: &mut SideBySide, hunks: &[Hunk], lead_b: bool) {
    let (at, bt) = (a.view.top, side.doc.view.top);
    let first = side.synced.0 == u64::MAX;
    // Which pane follows (true: A) and the line it should show.
    let target = if lead_b && !first && bt != side.synced.1 {
        Some((true, diff::map_line(hunks, side.doc.buf.line_of(bt).0, false)))
    } else if at != side.synced.0 {
        Some((false, diff::map_line(hunks, a.buf.line_of(at).0, true)))
    } else {
        side.pending
    };
    side.pending = None;
    let Some((to_a, line)) = target else {
        side.synced = (at, bt);
        return;
    };
    let follower = if to_a { &mut *a } else { &mut *side.doc };
    let (top, exact) = line_top(&follower.buf, line);
    follower.view.top = top;
    follower.view.scroll_to_caret = false;
    if !exact {
        side.pending = Some((to_a, line));
    }
    side.synced = if to_a { (top, bt) } else { (at, top) };
}

/// Draws the document (A, left) and the file it is compared with (B, right), their differing
/// lines tinted, scrolling together.
pub(super) fn show_side_by_side(ui: &mut egui::Ui, a: &mut Document, r: &mut DiffResults, env: &mut ViewEnv) {
    let Some(side) = r.side.as_mut() else { return };
    let g = r.data.lock().unwrap();
    let hunks: &[Hunk] = &g.hunks;
    // Offsets in A no longer match once it is edited.
    let outdated = a.version != r.version;
    let dark = ui.visuals().dark_mode;
    let full = ui.available_rect_before_wrap();
    let mid = full.center().x.round();
    let left = egui::Rect::from_min_max(full.min, egui::pos2(mid - 2.0, full.bottom()));
    let right = egui::Rect::from_min_max(egui::pos2(mid + 2.0, full.top()), full.max);
    ui.painter().vline(mid, full.y_range(), ui.visuals().widgets.noninteractive.bg_stroke);
    let lead_b = ui.ctx().pointer_hover_pos().is_some_and(|p| right.contains(p));
    let alpha = if dark { 90 } else { 110 };
    let (red, green) = (egui::Color32::from_rgba_unmultiplied(230, 70, 70, alpha), egui::Color32::from_rgba_unmultiplied(70, 190, 90, alpha));
    {
        let b_buf = &side.doc.buf;
        let spans = |range: std::ops::Range<u64>| if outdated { Vec::new() } else { diff_spans(hunks, range, true) };
        let hl = |buf: &bigtext::text::TextBuf, range: std::ops::Range<u64>| {
            let mut v = (env.highlight)(buf, range.clone());
            if !outdated {
                v.extend(changed_parts(hunks, buf, b_buf, range, true, red));
            }
            v
        };
        let mut e = ViewEnv { font_size: &mut *env.font_size, highlight: &hl, actions: &mut *env.actions, focus: env.focus, lang: env.lang, pane: 0, show_ws: env.show_ws, diff: Some(DiffOverlay { spans: &spans, added: false }) };
        ui.scope_builder(egui::UiBuilder::new().max_rect(left), |ui| text_view::show(ui, a, &mut e));
    }
    {
        let a_buf = &a.buf;
        let spans = |range: std::ops::Range<u64>| diff_spans(hunks, range, false);
        let hl = |buf: &bigtext::text::TextBuf, range: std::ops::Range<u64>| if outdated { Vec::new() } else { changed_parts(hunks, buf, a_buf, range, false, green) };
        let lang = if env.lang == crate::view::highlight::Lang::Plain { env.lang } else { side.doc.effective_lang() };
        let mut actions = Vec::new();
        let mut e = ViewEnv { font_size: &mut *env.font_size, highlight: &hl, actions: &mut actions, focus: env.focus, lang, pane: 1, show_ws: env.show_ws, diff: Some(DiffOverlay { spans: &spans, added: true }) };
        ui.scope_builder(egui::UiBuilder::new().max_rect(right), |ui| text_view::show(ui, &mut side.doc, &mut e));
    }
    if !outdated && a.is_ready() && side.doc.is_ready() {
        sync_tops(a, side, hunks, lead_b);
    }
}

/// Where a patch's hunks go in a document (from a worker).
pub struct PatchDone {
    pub doc: u64,
    pub version: u64,
    /// The patch's file name and its number of hunks.
    pub name: String,
    pub total: usize,
    pub planned: bigtext::patch::Planned,
}

pub struct DiffResults {
    pub doc: u64,
    pub other: DiffOther,
    /// The document's version when the comparison started.
    pub version: u64,
    /// The switches it ran with.
    pub switches: DiffSwitches,
    /// B beside the document, when View → Compare side by side is on.
    pub side: Option<SideBySide>,
    pub data: Arc<Mutex<DiffData>>,
    pub cancel: CancelToken,
    pub progress: Progress,
}

impl BigTextApp {
    pub(super) fn add_recent(&mut self, path: &Path) {
        let p = PathBuf::from(file::display_path(path));
        self.recent.retain(|r| !super::same_path(r, &p));
        self.recent.insert(0, p);
        self.recent.truncate(RECENT_MAX);
        self.persist_state();
    }

    pub(super) fn persisted(&self) -> Persisted {
        Persisted {
            recent: self.recent.clone(),
            tabs: self
                .docs
                .iter()
                // Standard input cannot be reopened.
                .filter(|d| !d.temporary || d.unpacked_from.is_some())
                .filter_map(|d| {
                    Some(TabState {
                        path: PathBuf::from(file::display_path(d.unpacked_from.as_deref().or(d.path.as_deref())?)),
                        member: d.member.clone(),
                        caret: d.view.caret,
                        top: d.view.top,
                        wrap: d.view.wrap,
                        bookmarks: d.bookmarks.clone(),
                    })
                })
                .collect(),
            active: self.active,
            font_size: Some(self.settings.font_size),
            dark: Some(self.settings.dark),
            backup_on_save: Some(self.settings.backup_on_save),
            syntax: Some(self.settings.syntax),
            update_check: Some(self.update.auto),
            skip_version: self.update.skipped.map(|v| v.to_string()),
            update_checked: Some(self.update.last_check).filter(|&t| t > 0),
            marks: self.settings.marks.clone(),
            history: self.search.history.clone(),
            lang: Some(super::i18n::lang().code().to_string()),
            font: self.settings.font_path.clone(),
            level_bar: Some(self.settings.level_bar),
            saved: self.settings.saved.clone(),
            show_ws: Some(self.settings.show_ws),
            filter_ctx: Some((self.settings.filter_context, self.settings.filter_entries)),
            id_keys: self.settings.id_keys.clone(),
            pinned: self.docs.iter().filter(|d| d.pinned).filter_map(|d| Some(PathBuf::from(file::display_path(d.unpacked_from.as_deref().or(d.path.as_deref())?)))).collect(),
            keys: self.keymap.changed.iter().map(|(c, k)| (c.id(), super::commands::key_to_string(*k))).collect(),
        }
    }

    pub(super) fn persist_state(&self) {
        if self.persist {
            save_async(self.persisted());
        }
    }

    /// Restores the previous session (tabs, positions, bookmarks, settings).
    pub(super) fn restore_session(&mut self, p: Persisted) {
        if let Some(f) = p.font_size {
            self.settings.font_size = f.clamp(6.0, 72.0);
        }
        if let Some(d) = p.dark {
            self.settings.dark = d;
        }
        if let Some(b) = p.backup_on_save {
            self.settings.backup_on_save = b;
        }
        if let Some(s) = p.syntax {
            self.settings.syntax = s;
        }
        self.recent = p.recent;
        for t in p.tabs {
            let pinned = p.pinned.contains(&t.path);
            self.open_path_member(t.path.clone(), t.member.clone());
            if let Some(d) = self.docs.last_mut() {
                d.pinned = pinned;
                d.restore = Some(Restore { caret: t.caret, top: t.top, wrap: t.wrap, bookmarks: t.bookmarks, ..Default::default() });
            }
        }
        if !self.docs.is_empty() {
            self.active = p.active.min(self.docs.len() - 1);
        }
    }

    pub(super) fn compare_dialog(&mut self) {
        if !self.docs.get(self.active).is_some_and(|d| d.is_ready()) {
            return;
        }
        if let Some(other) = rfd::FileDialog::new().set_title(tr("Compare with…")).pick_file() {
            self.start_diff(other);
        }
    }

    /// Next (or previous) difference of the comparison of the active document (F7, Shift+F7).
    pub(crate) fn step_diff(&mut self, forward: bool) {
        let Some(r) = &self.diff else {
            self.notify(tr("Compare the file with another first (File → Compare with…).").into(), None);
            return;
        };
        let capped = r.data.lock().unwrap().capped;
        let doc = r.doc;
        let Some(i) = self.docs.iter().position(|d| d.id == doc) else { return };
        self.active = i;
        if let Some((k, n)) = self.docs[i].goto_diff(forward) {
            let mut msg = trf("Difference {i} of {n}", &[("i", &k.to_string()), ("n", &n.to_string())]);
            if capped {
                msg.push_str(tr(" (capped, more exist)"));
            }
            self.notify(msg, None);
        }
    }

    /// Compares the other way round: the other file becomes the document, this one the file
    /// compared with.
    pub(crate) fn swap_diff(&mut self) {
        let Some(DiffResults { other: DiffOther::File(other), doc, .. }) = &self.diff else { return };
        let other = other.clone();
        let Some(a) = self.docs.iter().find(|d| d.id == *doc).and_then(|d| d.path.clone()) else { return };
        self.open_path(other);
        if let Some(d) = self.docs.get(self.active) {
            self.diff_pending = Some((d.id, a));
        }
    }

    /// Starts a comparison waiting for its document to be ready.
    pub(super) fn run_pending_diff(&mut self) {
        let Some((id, other)) = self.diff_pending.clone() else { return };
        let Some(i) = self.docs.iter().position(|d| d.id == id) else {
            self.diff_pending = None;
            return;
        };
        if self.docs[i].is_ready() {
            self.diff_pending = None;
            self.active = i;
            self.start_diff(other);
        }
    }

    pub(crate) fn start_diff(&mut self, other: PathBuf) {
        self.start_diff_with(DiffOther::File(other));
    }

    /// Compares the active document, unsaved edits included, with `other`.
    pub(crate) fn start_diff_with(&mut self, other: DiffOther) {
        let Some(d) = self.docs.get(self.active) else { return };
        let old_side = match self.diff.take() {
            Some(mut r) => {
                r.cancel.cancel();
                r.side.take()
            }
            None => None,
        };
        // Comparing again with the same file keeps it open beside.
        let b_path = other.path().map(Path::to_path_buf).or_else(|| d.path.clone());
        let side = old_side.filter(|s| Some(&s.path) == b_path.as_ref() && s.doc.banner.is_none()).map(|mut s| {
            s.synced = (u64::MAX, u64::MAX);
            s.pending = None;
            s
        });
        let switches = self.diff_switches.clone();
        let opts = match switches.options() {
            Ok(o) => o,
            Err(e) => {
                self.notify(e, None);
                return;
            }
        };
        let a = d.buf.snapshot();
        let saved = (other == DiffOther::Saved).then(|| d.buf.source.clone());
        let data = Arc::new(Mutex::new(DiffData::default()));
        let cancel = CancelToken::new();
        let progress = Progress::default();
        self.diff = Some(DiffResults { doc: d.id, other: other.clone(), version: d.version, switches, side, data: data.clone(), cancel: cancel.clone(), progress: progress.clone() });
        let waker = self.waker.clone();
        jobs::spawn("diff", move || {
            let t = Instant::now();
            let b = match (saved, other.path()) {
                (Some(s), _) => s,
                (None, Some(p)) => match Source::open(p, OpenMode::Auto) {
                    Ok(b) => Arc::new(b),
                    Err(e) => {
                        let mut g = data.lock().unwrap();
                        g.error = Some(super::friendly_io(&e, p));
                        g.done = true;
                        waker();
                        return;
                    }
                },
                (None, None) => return,
            };
            data.lock().unwrap().other = Some(b.clone());
            let mut batch = Vec::new();
            let mut count = 0usize;
            let mut capped = false;
            let mut last_wake = Instant::now();
            let end = diff::diff_with(&a, &*b, &opts, &cancel, Some(&progress), |h| {
                if count >= DIFF_CAP {
                    capped = true;
                    return false;
                }
                count += 1;
                batch.push(h);
                if last_wake.elapsed() > Duration::from_millis(50) {
                    data.lock().unwrap().hunks.append(&mut batch);
                    last_wake = Instant::now();
                    waker();
                }
                true
            });
            let mut g = data.lock().unwrap();
            g.hunks.append(&mut batch);
            g.capped = capped;
            g.end = Some(end);
            g.done = true;
            g.elapsed = Some(t.elapsed());
            if end == DiffEnd::Failed {
                g.error = Some(tr("Reading one of the files failed.").into());
            }
            drop(g);
            waker();
        });
    }

    pub(super) fn save_diff_dialog(&mut self) {
        let Some(r) = &self.diff else { return };
        let stem = |p: &Path| p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let a = self.docs.iter().find(|d| d.id == r.doc).and_then(|d| d.path.as_deref().map(stem)).unwrap_or_else(|| "a".into());
        let name = match r.other.path() {
            Some(p) if !matches!(r.other, DiffOther::Backup(_)) => format!("{a}-vs-{}.diff", stem(p)),
            _ => format!("{a}.diff"),
        };
        if let Some(target) = rfd::FileDialog::new().set_title(tr("Save as .diff…")).set_file_name(name).add_filter("diff", &["diff", "patch"]).save_file() {
            self.export_diff(target);
        }
    }

    /// Writes the comparison as a unified diff to `target`, on a worker: from the document to
    /// the other file, or for the saved file and the backup, from them to the document (what was
    /// changed).
    pub(crate) fn export_diff(&mut self, target: PathBuf) {
        let Some(r) = &self.diff else { return };
        let Some(d) = self.docs.iter().find(|d| d.id == r.doc) else { return };
        let opts = match r.switches.options() {
            Ok(o) => o,
            Err(e) => {
                self.notify(e, None);
                return;
            }
        };
        let doc = d.buf.snapshot();
        let doc_name = d.path.as_deref().map(file::display_path).unwrap_or_else(|| d.title.clone());
        let (other, doc_first) = match &r.other {
            DiffOther::File(p) => (p.clone(), true),
            DiffOther::Backup(p) => (p.clone(), false),
            DiffOther::Saved => match &d.path {
                Some(p) => (p.clone(), false),
                None => return,
            },
        };
        let saved = (r.other == DiffOther::Saved).then(|| d.buf.source.clone());
        self.spawn_task(tr("Saving the diff").into(), move |progress, cancel| {
            let src = match saved {
                Some(s) => s,
                None => match Source::open(&other, OpenMode::Auto) {
                    Ok(s) => Arc::new(s),
                    Err(e) => return TaskOutcome::Failed(super::friendly_io(&e, &other)),
                },
            };
            let other_name = file::display_path(&other);
            let (a, b, a_name, b_name): (&dyn diff::LineSource, &dyn diff::LineSource, &str, &str) =
                if doc_first { (&doc, &*src, &doc_name, &other_name) } else { (&*src, &doc, &other_name, &doc_name) };
            let u = bigtext::unified::Unified { a, b, a_name, b_name, opts: &opts, context: 3 };
            let file_name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            super::tasks::write_via_temp(&target, |w| {
                let differ = bigtext::unified::write_unified(&u, w, cancel, Some(progress))?;
                Ok(differ.map(|d| if d { trf("Saved the differences as {file}.", &[("file", &file_name)]) } else { tr("The files are the same; the diff is empty.").into() }))
            })
        });
    }

    pub(crate) fn apply_patch_dialog(&mut self) {
        if !self.docs.get(self.active).is_some_and(|d| d.is_ready()) {
            return;
        }
        if let Some(p) = rfd::FileDialog::new().set_title(tr("Apply patch…")).add_filter("diff, patch", &["diff", "patch"]).add_filter("*", &["*"]).pick_file() {
            self.apply_patch_file(p);
        }
    }

    /// Applies the unified diff in `patch` to the active document as one undo step: each hunk
    /// where it says or, found in one place only, a few lines away.
    pub(crate) fn apply_patch_file(&mut self, patch: PathBuf) {
        let Some(d) = self.docs.get(self.active) else { return };
        if !d.editable() {
            self.notify(tr("The document is read-only").into(), None);
            return;
        }
        let pname = patch.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let text = match std::fs::metadata(&patch) {
            Ok(m) if m.len() > 512 << 20 => {
                self.error = Some(trf("{file} is too large for a patch ({size}).", &[("file", &pname), ("size", &jobs::human_bytes(m.len()))]));
                return;
            }
            _ => match std::fs::read(&patch) {
                Ok(t) => t,
                Err(e) => {
                    self.error = Some(super::friendly_io(&e, &patch));
                    return;
                }
            },
        };
        let mut files = match bigtext::patch::parse(&text) {
            Ok(f) => f,
            Err(e) => {
                self.error = Some(trf("{file} is not a patch: {error}", &[("file", &pname), ("error", &e)]));
                return;
            }
        };
        // The file named like the document, or the only one.
        let name = d.path.as_deref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| d.title.clone());
        let named = |n: &str| n.replace('\\', "/").rsplit('/').next() == Some(name.as_str());
        let Some(i) = files.iter().position(|f| named(&f.new_name) || named(&f.old_name)).or((files.len() == 1).then_some(0)) else {
            self.error = Some(trf("{file} changes {n} files, none of them named {name}.", &[("file", &pname), ("n", &files.len().to_string()), ("name", &name)]));
            return;
        };
        let hunks = files.swap_remove(i).hunks;
        let eol: &[u8] = match d.line_ending {
            bigtext::encoding::LineEnding::CrLf => b"\r\n",
            bigtext::encoding::LineEnding::Cr => b"\r",
            _ => b"\n",
        };
        let eol = eol.to_vec();
        let snap = d.buf.snapshot();
        let (doc, version, total) = (d.id, d.version, hunks.len());
        self.spawn_task(trf("Applying {file}", &[("file", &pname)]), move |progress, cancel| match bigtext::patch::plan(&snap, &hunks, &eol, cancel, Some(progress)) {
            Some(planned) => TaskOutcome::Patch(Box::new(PatchDone { doc, version, name: pname, total, planned })),
            None if cancel.is_cancelled() => TaskOutcome::Cancelled,
            None => TaskOutcome::Failed(tr("Reading the document failed.").into()),
        });
    }

    pub(super) fn on_patch_done(&mut self, p: PatchDone) {
        let Some(d) = self.docs.iter_mut().find(|d| d.id == p.doc) else { return };
        if d.version != p.version {
            self.error = Some(tr("The document changed while the patch was being placed; nothing was changed.").into());
            return;
        }
        let planned = p.planned;
        if let Some(first) = planned.edits.first().map(|e| e.0.start) {
            let (pieces, add) = d.buf.build_batch(&planned.edits);
            let (s, _) = d.selection();
            let swap = d.buf.apply_batch(pieces, &add);
            d.record(swap, (s, s));
            d.set_caret(first.min(d.buf.len()), false);
            d.view.center();
        }
        let mut msg = trf("Applied {n} of {total} hunks from {file} (undo with Ctrl+Z).", &[("n", &planned.applied.to_string()), ("total", &p.total.to_string()), ("file", &p.name)]);
        if planned.moved > 0 {
            msg.push(' ');
            msg.push_str(&trf("{n} were found a few lines away.", &[("n", &planned.moved.to_string())]));
        }
        if !planned.rejected.is_empty() {
            let list: Vec<String> = planned
                .rejected
                .iter()
                .take(10)
                .map(|&(k, line, why)| {
                    let why = match why {
                        bigtext::patch::Reject::NotFound => tr("not found"),
                        bigtext::patch::Reject::Ambiguous => tr("matches more than one place"),
                    };
                    trf("#{k} (line {line}): {why}", &[("k", &(k + 1).to_string()), ("line", &line.to_string()), ("why", why)])
                })
                .collect();
            d.banner = Some(super::document::Banner::Info(trf("Patch hunks not applied: {list}", &[("list", &list.join("; "))])));
        }
        self.notify(msg, None);
    }

    /// Opens the other file beside the document, or closes it, as View → Compare side by side
    /// asks.
    pub(super) fn sync_side_doc(&mut self) {
        let Some(r) = &mut self.diff else { return };
        if !self.side_by_side {
            r.side = None;
            return;
        }
        if r.side.is_some() {
            return;
        }
        let path = match r.other.path() {
            Some(p) => p.to_path_buf(),
            None => match self.docs.iter().find(|d| d.id == r.doc).and_then(|d| d.path.clone()) {
                Some(p) => p,
                None => return,
            },
        };
        let id = self.alloc_id();
        let mut doc = Document::loading(id, path.clone());
        doc.read_only = true;
        if let Some(r) = &mut self.diff {
            r.side = Some(SideBySide { doc: Box::new(doc), path: path.clone(), synced: (u64::MAX, u64::MAX), pending: None });
        }
        self.spawn_open(id, path, None);
    }

    /// Copies the differences' offsets into the compared document for its scrollbar marks.
    pub(super) fn sync_diff_marks(&mut self) {
        let (id, data) = match &self.diff {
            Some(r) => (Some(r.doc), Some(r.data.clone())),
            None => (None, None),
        };
        for d in &mut self.docs {
            if Some(d.id) != id {
                d.diff_marks.clear();
            } else if let Some(data) = &data {
                let g = data.lock().unwrap();
                if g.hunks.len() != d.diff_marks.len() {
                    d.diff_marks = g.hunks.iter().map(|h| h.a_off).collect();
                }
            }
        }
    }

    pub(super) fn diff_panel(&mut self, ui: &mut egui::Ui) {
        let Some(r) = &self.diff else { return };
        let font = egui::FontId::monospace(self.settings.font_size.min(13.0));
        let row_h = ui.ctx().fonts_mut(|f| f.row_height(&font)) + 2.0;
        let (n, done, capped, err, elapsed, other) = {
            let g = r.data.lock().unwrap();
            (g.hunks.len(), g.done, g.capped, g.error.clone(), g.elapsed, g.other.clone())
        };
        let mut close = false;
        let mut open_other: Option<u64> = None;
        let mut jump: Option<u64> = None;
        let (mut swap, mut step) = (false, None);
        let mut switches = self.diff_switches.clone();
        let mut rerun = false;
        let a_doc = self.docs.iter().find(|d| d.id == r.doc);
        let outdated = a_doc.is_some_and(|d| d.version != r.version);
        let mut side = self.side_by_side;
        let mut save_as = false;
        ui.horizontal(|ui| {
            ui.strong(match &r.other {
                DiffOther::File(p) => trf("Compare with {file}", &[("file", &file::display_path(p))]),
                DiffOther::Saved => tr("Unsaved edits compared with the saved file").into(),
                DiffOther::Backup(p) => trf("Compare with the backup {file}", &[("file", &file::display_path(p))]),
            });
            if let Some(e) = &err {
                ui.colored_label(ui.visuals().error_fg_color, e);
            } else if !done {
                ui.add(egui::ProgressBar::new(r.progress.fraction()).desired_width(160.0).text(format!("{} {:.0}%", tr("comparing"), r.progress.fraction() * 100.0)));
                ui.ctx().request_repaint_after(Duration::from_millis(50));
                if ui.small_button(tr("Cancel")).clicked() {
                    r.cancel.cancel();
                }
            } else if n == 0 {
                ui.label(trf("files are identical ({secs}s)", &[("secs", &format!("{:.2}", elapsed.unwrap_or_default().as_secs_f64()))]));
            } else {
                ui.label(format!(
                    "{}{}",
                    trf("{n} difference(s) in {secs}s", &[("n", &n.to_string()), ("secs", &format!("{:.2}", elapsed.unwrap_or_default().as_secs_f64()))]),
                    if capped { tr(" (capped, more exist)") } else { "" }
                ));
            }
            if let Some(what) = r.switches.summary() {
                ui.weak(trf("ignoring {what}", &[("what", &what)]));
            }
            if outdated && done {
                ui.colored_label(ui.visuals().warn_fg_color, tr("(outdated, run again)"));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("×").clicked() {
                    close = true;
                }
                ui.toggle_value(&mut side, tr("Side by side")).on_hover_text(tr("Show the other file beside this one, scrolling together"));
                if done && err.is_none() && n > 0 && ui.small_button(tr("Save as .diff…")).on_hover_text(tr("Write the differences as a unified diff (a patch) with 3 lines of context")).clicked() {
                    save_as = true;
                }
                if matches!(r.other, DiffOther::File(_)) && ui.small_button(tr("Swap A↔B")).on_hover_text(tr("Compare the other way round: open the other file and compare it with this one")).clicked() {
                    swap = true;
                }
                if ui.small_button("◀").on_hover_text(tr("Previous difference (Shift+F7)")).clicked() {
                    step = Some(false);
                }
                if ui.small_button("▶").on_hover_text(tr("Next difference (F7)")).clicked() {
                    step = Some(true);
                }
            });
        });
        ui.horizontal(|ui| {
            ui.label(tr("Ignore:"));
            ui.checkbox(&mut switches.whitespace, tr("white space"));
            ui.checkbox(&mut switches.case, tr("case"));
            ui.checkbox(&mut switches.timestamps, tr("timestamps")).on_hover_text(tr("The time at the start of each line"));
            ui.label(tr("mask"));
            let resp = ui.add(egui::TextEdit::singleline(&mut switches.mask).desired_width(160.0).hint_text("req=[0-9a-f]+").font(egui::TextStyle::Monospace));
            resp.on_hover_text(tr("A regular expression; its matches do not count (ids, durations)"));
            if (switches != r.switches || outdated) && ui.button(tr("Run again")).clicked() {
                rerun = true;
            }
        });
        let a_buf = a_doc.map(|d| &d.buf);
        let has_b_file = r.other.path().is_some();
        let data = r.data.clone();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, row_h, n, |ui, range| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            let g = data.lock().unwrap();
            for i in range {
                let Some(h) = g.hunks.get(i).copied() else { break };
                let snippet = |b: Option<&[u8]>, codec: bigtext::encoding::Codec| -> String {
                    match b {
                        Some(b) => {
                            let s = codec.decode_lossy(b);
                            s.lines().next().unwrap_or("").chars().map(|c| if c.is_control() { '·' } else { c }).collect()
                        }
                        None => "…".into(),
                    }
                };
                let (a_text, codec) = match a_buf {
                    Some(buf) if h.a_count > 0 => (snippet(buf.read(h.a_off..h.a_off + 160).as_deref(), buf.codec), buf.codec),
                    Some(buf) => (String::new(), buf.codec),
                    None => (String::new(), bigtext::encoding::Codec::utf8()),
                };
                let b_text = match &other {
                    Some(s) if h.b_count > 0 => snippet(s.get(h.b_off..(h.b_off + 160).min(s.len()), false).as_deref(), codec),
                    _ => String::new(),
                };
                ui.horizontal(|ui| {
                    let label = format!("A {:>8} −{:<5} B {:>8} +{:<5}", h.a_line + 1, h.a_count, h.b_line + 1, h.b_count);
                    if ui.add(egui::Button::new(egui::RichText::new(label).font(font.clone())).frame(false)).on_hover_text(tr("Go to this line in A")).clicked() {
                        jump = Some(h.a_line + 1);
                    }
                    if has_b_file && ui.small_button("B").on_hover_text(tr("Open the other file at this line")).clicked() {
                        open_other = Some(h.b_line + 1);
                    }
                    // Both sides get half of the remaining width so B stays visible next to a long A line.
                    let w = ((ui.available_width() - 2.0 * ui.spacing().item_spacing.x - 16.0) / 2.0).max(40.0);
                    let side = |ui: &mut egui::Ui, text: String, color: egui::Color32| {
                        ui.allocate_ui_with_layout(egui::vec2(w, row_h), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.set_width(w);
                            ui.add(egui::Label::new(egui::RichText::new(text).font(font.clone()).color(color)).truncate());
                        });
                    };
                    side(ui, a_text, egui::Color32::from_rgb(220, 110, 110));
                    // The arrow is in the monospace font only.
                    ui.monospace("→");
                    side(ui, b_text, egui::Color32::from_rgb(110, 190, 110));
                });
            }
        });
        let (doc_id, other_path) = (r.doc, r.other.path().map(Path::to_path_buf));
        if let Some(line) = jump {
            if let Some(i) = self.docs.iter().position(|d| d.id == doc_id) {
                self.active = i;
                self.docs[i].goto_line(line);
            }
        }
        if let (Some(line), Some(other_path)) = (open_other, other_path) {
            let existing = self.docs.iter().position(|d| d.path.as_deref().is_some_and(|p| super::same_path(p, &other_path)));
            match existing {
                Some(i) => {
                    self.active = i;
                    self.docs[i].goto_line(line);
                }
                None => {
                    self.open_path(other_path);
                    if let Some(d) = self.docs.last_mut() {
                        d.restore = Some(Restore { line: Some(line), ..Default::default() });
                    }
                }
            }
        }
        if close {
            if let Some(r) = self.diff.take() {
                r.cancel.cancel();
            }
        }
        self.diff_switches = switches;
        self.side_by_side = side;
        if save_as {
            self.save_diff_dialog();
        }
        if rerun {
            if let Some(r) = &self.diff {
                let (doc, other) = (r.doc, r.other.clone());
                if let Some(i) = self.docs.iter().position(|d| d.id == doc) {
                    self.active = i;
                    self.start_diff_with(other);
                }
            }
        }
        if swap {
            self.swap_diff();
        }
        if let Some(forward) = step {
            self.step_diff(forward);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_roundtrip() {
        let p = Persisted {
            recent: vec![PathBuf::from(r"C:\logs\a.log"), PathBuf::from("/tmp/b|c.txt")],
            tabs: vec![
                TabState { path: PathBuf::from(r"D:\big file.txt"), member: None, caret: 123, top: 100, wrap: true, bookmarks: vec![0, 55, 1 << 40] },
                TabState { path: PathBuf::from(r"D:\logs|old.zip"), member: Some("logs/app.log.gz".into()), caret: 1, top: 0, wrap: false, bookmarks: vec![] },
            ],
            active: 0,
            font_size: Some(16.0),
            dark: Some(false),
            backup_on_save: Some(true),
            syntax: Some(false),
            update_check: Some(false),
            skip_version: Some("0.2.0".into()),
            update_checked: Some(1_761_550_000),
            marks: vec![super::super::marks::Mark {
                query: bigtext::search::Query { pattern: r"id=\d+|x".into(), regex: true, case_sensitive: true, whole_word: false, dot_all: false },
                color: 3,
                enabled: false,
            }],
            history: vec!["ERROR".into(), "a|b=c".into()],
            keys: vec![("goto_time".into(), "ctrl+alt+T".into()), ("find_all".into(), "none".into())],
            lang: Some("uz".into()),
            font: Some(PathBuf::from(r"C:\Windows\Fonts\consola.ttf")),
            level_bar: Some(false),
            show_ws: Some(true),
            filter_ctx: Some((3, true)),
            id_keys: Some(vec!["rid".into(), "trace_id".into()]),
            pinned: vec![PathBuf::from(r"D:\big file.txt")],
            saved: vec![
                super::super::saved_searches::SavedSearch {
                    name: "5xx responses".into(),
                    query: bigtext::search::Query { pattern: r#"" 5\d\d |a|b"#.into(), regex: true, case_sensitive: true, whole_word: false, dot_all: false },
                    invert: false,
                    column: None,
                },
                super::super::saved_searches::SavedSearch {
                    name: "not ok".into(),
                    query: bigtext::search::Query { pattern: "ok".into(), regex: false, case_sensitive: false, whole_word: true, dot_all: false },
                    invert: true,
                    column: Some(3),
                },
            ],
        };
        assert_eq!(parse(&serialize(&p)).saved, p.saved);
        assert_eq!(parse(&serialize(&p)).show_ws, Some(true));
        assert_eq!(parse(&serialize(&p)).filter_ctx, Some((3, true)));
        assert_eq!(parse(&serialize(&p)).id_keys, p.id_keys);
        assert_eq!(parse(&serialize(&p)).pinned, p.pinned);
        assert_eq!(parse(&serialize(&p)).font, p.font);
        assert_eq!(parse(&serialize(&p)).level_bar, Some(false));
        assert_eq!(parse(&serialize(&p)).lang.as_deref(), Some("uz"));
        let q = parse(&serialize(&p));
        assert_eq!((q.marks.clone(), q.history.clone(), q.keys.clone()), (p.marks.clone(), p.history.clone(), p.keys.clone()));
        assert_eq!(q.recent, p.recent);
        assert_eq!(q.tabs.len(), 2);
        assert_eq!((q.tabs[1].path.clone(), q.tabs[1].member.as_deref()), (PathBuf::from(r"D:\logs|old.zip"), Some("logs/app.log.gz")));
        assert_eq!(q.tabs[0].member, None);
        let t = &q.tabs[0];
        assert_eq!((t.caret, t.top, t.wrap, t.bookmarks.clone(), t.path.clone()), (123, 100, true, vec![0, 55, 1 << 40], PathBuf::from(r"D:\big file.txt")));
        assert_eq!((q.font_size, q.dark, q.backup_on_save, q.syntax), (Some(16.0), Some(false), Some(true), Some(false)));
        assert_eq!((q.update_check, q.skip_version.as_deref(), q.update_checked), (Some(false), Some("0.2.0"), Some(1_761_550_000)));
    }
}
