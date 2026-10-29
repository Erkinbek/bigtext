//! An open document: text buffer, undo history, view state and its background jobs.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bigtext::encoding::{Detected, LineEnding};
use bigtext::file::{self, Source};
use bigtext::index::{self, LineIndex};
use bigtext::jobs::{CancelToken, Waker};
use bigtext::text::rows;
use bigtext::text::{Swap, TextBuf};

/// Line commands (duplicate/delete line) scan at most this far on the UI thread; longer lines
/// can still be selected and edited.
const LINE_CMD_LIMIT: u64 = 8 * 1024 * 1024;
/// A selection and the document version: (start, end, version).
type SelKey = (u64, u64, u64);

/// Characters of a selection are counted (on the UI thread) up to this size.
pub const SEL_CHARS_MAX: u64 = 4 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EditKind {
    Typing,
    Delete,
    Other,
}

pub struct UndoGroup {
    pub swaps: Vec<Swap>,
    pub sel_before: (u64, u64),
    pub sel_after: (u64, u64),
    pub kind: EditKind,
    pub at: Instant,
    /// Content state ids before and after this group (see `Document::is_modified`).
    pub state_before: u64,
    pub state_after: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DocState {
    Loading,
    Ready,
    /// Saving/replacing: the source is released while the file is being swapped on disk.
    Finalizing(String),
    Failed(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Banner {
    Binary,
    ChangedOnDisk { deleted: bool, grown: bool },
    Info(String),
    Error(String),
}

/// Per-document view state (positions are byte offsets in the document).
pub struct ViewState {
    pub top: u64,
    pub scroll_x: f32,
    pub caret: u64,
    pub anchor: u64,
    pub desired_cell: Option<u32>,
    pub wrap: bool,
    pub rows_on_screen: usize,
    pub text_cols: usize,
    pub max_cells: u32,
    pub scroll_to_caret: bool,
    pub center_caret: bool,
    /// Jumps so far (Go to, F3, a click in a list…), for Back / Forward.
    pub jumps: u64,
    pub pending_goto_line: Option<(u64, u64)>,
    pub wheel_rem: f32,
    pub hex_top: u64,
    pub first_paint: Option<Instant>,
    pub dragging: bool,
    pub width_cache: std::collections::HashMap<u64, u32>,
    pub width_cache_version: u64,
    /// Geometry of the last painted text area (hit testing in the self-test).
    pub text_rect: Option<egui::Rect>,
    pub cell: egui::Vec2,
    pub scroll_marks: crate::view::text_view::ScrollMarks,
    /// Column (block) selection, made with Alt+drag.
    pub block: Option<crate::view::block::Block>,
    pub block_drag: bool,
    /// More cursors (anchor, caret) besides `anchor`/`caret` (Ctrl+D, Alt+click).
    pub extra: Vec<(u64, u64)>,
    /// The document version the extra cursors belong to; any other edit drops them.
    pub extra_version: u64,
    /// The selection before an Alt+press, for adding a cursor when it turns out to be a click.
    pub alt_prev: Option<(u64, u64)>,
}

impl ViewState {
    /// Centres the caret after a jump and counts the jump.
    pub fn center(&mut self) {
        self.center_caret = true;
        self.jumps += 1;
    }
}

impl Default for ViewState {
    fn default() -> Self {
        ViewState {
            jumps: 0,
            top: 0,
            scroll_x: 0.0,
            caret: 0,
            anchor: 0,
            desired_cell: None,
            wrap: false,
            rows_on_screen: 40,
            text_cols: 120,
            max_cells: 0,
            scroll_to_caret: false,
            center_caret: false,
            pending_goto_line: None,
            wheel_rem: 0.0,
            hex_top: 0,
            first_paint: None,
            dragging: false,
            width_cache: std::collections::HashMap::new(),
            width_cache_version: 0,
            text_rect: None,
            cell: egui::Vec2::ZERO,
            scroll_marks: Default::default(),
            block: None,
            block_drag: false,
            extra: Vec::new(),
            extra_version: 0,
            alt_prev: None,
        }
    }
}

pub struct Document {
    pub id: u64,
    pub path: Option<PathBuf>,
    pub title: String,
    pub buf: TextBuf,
    pub line_ending: LineEnding,
    pub eol_on_save: Option<LineEnding>,
    pub looks_binary: bool,
    pub read_only: bool,
    pub hex: bool,
    pub undo: Vec<UndoGroup>,
    pub redo: Vec<UndoGroup>,
    pub version: u64,
    pub saved_version: u64,
    /// Identifies the current content; undoing back to the saved state clears "modified".
    content_state: u64,
    saved_content_state: u64,
    next_content_state: u64,
    pub view: ViewState,
    pub state: DocState,
    pub banner: Option<Banner>,
    pub index_cancel: CancelToken,
    pub watch_cancel: CancelToken,
    pub opened_at: Instant,
    pub open_elapsed: Option<Duration>,
    pub follow: bool,
    pub results: Option<super::search_panel::SearchResults>,
    pub filter: Option<super::filter_panel::FilterResults>,
    pub find_job: Option<super::search_panel::FindJob>,
    pub save_job: Option<super::ops::SaveJob>,
    pub close_after_save: bool,
    pub reload_prefix: Option<Vec<u64>>,
    /// Bookmarked line starts (sorted byte offsets).
    pub bookmarks: Vec<u64>,
    /// Offsets of the differences found by File → Compare (marked on the scrollbar).
    pub diff_marks: Vec<u64>,
    /// The compressed file this document is an unpacked copy of (`path` is the temporary copy).
    pub unpacked_from: Option<PathBuf>,
    /// `path` is a temporary copy (unpacked archive, standard input): Save means Save As, and
    /// the copy is deleted when the tab closes.
    pub temporary: bool,
    /// Progress and cancellation of the open job (shown while a compressed file is unpacked).
    pub unpacking: Option<(bigtext::jobs::Progress, CancelToken)>,
    /// Character count of the selection, cached.
    sel_chars: std::cell::Cell<Option<(SelKey, Option<u64>)>>,
    /// Second pane of a split view (shares caret and selection, scrolls independently).
    pub split: Option<PaneView>,
    pub active_pane: u8,
    /// Syntax highlighting override (None = from the file extension).
    pub lang: Option<crate::view::highlight::Lang>,
    /// Format guessed from the first lines, for files whose extension says nothing.
    pub sniffed_lang: crate::view::highlight::Lang,
    /// The file labels of a merged log (File → Merge logs by time), painted in their colours.
    pub merged: Option<super::merge_logs::MergeTags>,
    /// Time histogram (Search → Time histogram).
    pub timeline: Option<super::timeline::TimelineResults>,
    pub gaps: Option<super::gaps_panel::GapsResults>,
    pub patterns: Option<super::patterns_window::PatternsResults>,
    pub problems: Option<super::problems_panel::ProblemsResults>,
    pub chart: Option<super::column_chart::ChartResults>,
    pub json_check: Option<super::json_validate::JsonCheckResults>,
    pub json_diff: Option<super::json_diff_panel::JsonDiffResults>,
    /// Outline for View → JSON structure.
    pub json: Option<super::json_panel::JsonTree>,
    /// Pinned tabs stay first and are not closed by "Close other tabs".
    pub pinned: bool,
    /// A session's filter, run once the document has opened.
    pub pending_filter: Option<super::sessions::SavedFilter>,
    /// Shown as a table (CSV / TSV); on by default for .csv and .tsv files.
    pub table: bool,
    pub table_state: crate::view::table_view::TableState,
    /// Position to restore once the file has opened (session restore, diff jumps).
    pub restore: Option<Restore>,
    last_indexed: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Restore {
    pub caret: u64,
    pub top: u64,
    pub wrap: bool,
    pub bookmarks: Vec<u64>,
    pub line: Option<u64>,
    /// Go to the end (follow mode from the command line).
    pub end: bool,
    /// Select this range (a match from Find in files).
    pub select: Option<(u64, u64)>,
    /// Run this filter (a session's).
    pub filter: Option<super::sessions::SavedFilter>,
}

/// Scroll state of the secondary split pane.
#[derive(Clone, Debug, Default)]
pub struct PaneView {
    pub top: u64,
    pub scroll_x: f32,
    pub wheel_rem: f32,
    pub rows_on_screen: usize,
    pub text_cols: usize,
    pub text_rect: Option<egui::Rect>,
}

impl PaneView {
    /// Exchanges the scroll state with the document's primary view state.
    pub fn swap_with(&mut self, v: &mut ViewState) {
        std::mem::swap(&mut self.top, &mut v.top);
        std::mem::swap(&mut self.scroll_x, &mut v.scroll_x);
        std::mem::swap(&mut self.wheel_rem, &mut v.wheel_rem);
        std::mem::swap(&mut self.rows_on_screen, &mut v.rows_on_screen);
        std::mem::swap(&mut self.text_cols, &mut v.text_cols);
        std::mem::swap(&mut self.text_rect, &mut v.text_rect);
    }
}

impl Document {
    pub fn loading(id: u64, path: PathBuf) -> Document {
        let title = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| file::display_path(&path));
        let table = matches!(crate::view::highlight::Lang::from_path(Some(&path)), crate::view::highlight::Lang::Csv(_));
        let src = Arc::new(Source::empty());
        let idx = LineIndex::new(0);
        Document {
            id,
            path: Some(path),
            title,
            buf: TextBuf::new(src, idx, bigtext::encoding::Codec::utf8()),
            line_ending: LineEnding::Unknown,
            eol_on_save: None,
            looks_binary: false,
            read_only: false,
            hex: false,
            undo: Vec::new(),
            redo: Vec::new(),
            version: 0,
            saved_version: 0,
            content_state: 0,
            saved_content_state: 0,
            next_content_state: 1,
            view: ViewState::default(),
            state: DocState::Loading,
            banner: None,
            index_cancel: CancelToken::new(),
            watch_cancel: CancelToken::new(),
            opened_at: Instant::now(),
            open_elapsed: None,
            follow: false,
            results: None,
            filter: None,
            find_job: None,
            save_job: None,
            close_after_save: false,
            reload_prefix: None,
            bookmarks: Vec::new(),
            diff_marks: Vec::new(),
            unpacked_from: None,
            temporary: false,
            unpacking: None,
            sel_chars: std::cell::Cell::new(None),
            split: None,
            active_pane: 0,
            lang: None,
            sniffed_lang: crate::view::highlight::Lang::Plain,
            merged: None,
            timeline: None,
            gaps: None,
            patterns: None,
            problems: None,
            chart: None,
            json_check: None,
            json_diff: None,
            json: None,
            pinned: false,
            pending_filter: None,
            table,
            table_state: Default::default(),
            restore: None,
            last_indexed: 0,
        }
    }

    pub fn untitled(id: u64, n: usize) -> Document {
        let mut d = Document::loading(id, PathBuf::new());
        d.path = None;
        d.title = super::i18n::trf("Untitled-{n}", &[("n", &n.to_string())]);
        d.state = DocState::Ready;
        d
    }

    /// Installs a freshly opened source (initial open, reload or after save).
    pub fn install(&mut self, source: Arc<Source>, det: Detected, prefix: Option<Vec<u64>>, waker: Waker) {
        self.index_cancel.cancel();
        self.index_cancel = CancelToken::new();
        source.set_waker(waker.clone());
        let index = match prefix {
            Some(p) => LineIndex::with_prefix(source.len(), p),
            None => LineIndex::new(source.len()),
        };
        if !index.is_done() {
            index::spawn_indexer(source.clone(), det.codec, index.clone(), self.index_cancel.clone(), waker);
        }
        self.buf = TextBuf::new(source, index, det.codec);
        self.line_ending = det.line_ending;
        self.looks_binary = det.looks_binary;
        if det.looks_binary && self.banner.is_none() {
            self.banner = Some(Banner::Binary);
        }
        self.undo.clear();
        self.redo.clear();
        self.version += 1;
        self.saved_version = self.version;
        self.content_state = self.next_content_state;
        self.saved_content_state = self.content_state;
        self.next_content_state += 1;
        self.last_indexed = 0;
        let min = self.min_caret();
        self.view.caret = self.view.caret.clamp(min, self.buf.len());
        self.view.anchor = self.view.caret;
        self.view.top = self.view.top.min(self.buf.len());
        let head = self.buf.read(0..self.buf.len().min(16 * 1024)).map(|b| self.buf.codec.decode_lossy(&b)).unwrap_or_default();
        self.sniffed_lang = crate::view::highlight::Lang::sniff(&head);
        self.state = DocState::Ready;
    }

    /// The format highlighting uses when none was chosen: from the extension, otherwise from
    /// the content.
    pub fn auto_lang(&self) -> crate::view::highlight::Lang {
        match crate::view::highlight::Lang::from_path(self.path.as_deref()) {
            crate::view::highlight::Lang::Plain => self.sniffed_lang,
            l => l,
        }
    }

    /// The format the document is highlighted as.
    pub fn effective_lang(&self) -> crate::view::highlight::Lang {
        self.lang.unwrap_or_else(|| self.auto_lang())
    }

    pub fn is_modified(&self) -> bool {
        self.content_state != self.saved_content_state
    }

    pub fn is_ready(&self) -> bool {
        self.state == DocState::Ready
    }

    pub fn editable(&self) -> bool {
        self.is_ready() && !self.read_only && !self.hex && self.save_job.is_none()
    }

    /// Cancels every background job that holds a snapshot of the source.
    pub fn cancel_jobs(&mut self) {
        self.index_cancel.cancel();
        if let Some(r) = &self.results {
            r.cancel.cancel();
        }
        if let Some(r) = &self.filter {
            r.cancel.cancel();
        }
        if let Some(t) = &self.timeline {
            t.cancel.cancel();
        }
        if let Some(g) = &self.gaps {
            g.cancel.cancel();
        }
        if let Some(p) = &self.patterns {
            p.cancel.cancel();
        }
        if let Some(p) = &self.problems {
            p.cancel.cancel();
        }
        if let Some(c) = &self.chart {
            c.cancel.cancel();
        }
        if let Some(j) = &self.json_check {
            j.cancel.cancel();
        }
        if let Some(j) = &self.json_diff {
            j.cancel.cancel();
        }
        if let Some((_, c)) = &self.unpacking {
            c.cancel();
        }
        if let Some(j) = self.find_job.take() {
            j.cancel.cancel();
        }
    }

    /// Re-attaches the buffer (keeping all edits) to a byte-identical reopened source.
    pub fn rebind(&mut self, source: Arc<Source>, waker: Waker) {
        self.index_cancel = CancelToken::new();
        source.set_waker(waker.clone());
        let index = LineIndex::new(source.len());
        index::spawn_indexer(source.clone(), self.buf.codec, index.clone(), self.index_cancel.clone(), waker);
        self.buf = self.buf.with_source(source, index);
        self.last_indexed = 0;
        self.state = DocState::Ready;
    }

    pub fn min_caret(&self) -> u64 {
        (self.buf.codec.bom_len as u64).min(self.buf.len())
    }

    pub fn selection(&self) -> (u64, u64) {
        let (a, b) = (self.view.anchor, self.view.caret);
        (a.min(b), a.max(b))
    }

    pub fn has_selection(&self) -> bool {
        self.view.anchor != self.view.caret
    }

    pub fn set_caret(&mut self, off: u64, extend: bool) {
        let off = off.clamp(self.min_caret(), self.buf.len());
        self.view.caret = off;
        if !extend {
            self.view.anchor = off;
        }
        self.view.scroll_to_caret = true;
    }

    pub fn select(&mut self, start: u64, end: u64) {
        self.view.anchor = start.clamp(self.min_caret(), self.buf.len());
        self.view.caret = end.clamp(self.min_caret(), self.buf.len());
        self.view.scroll_to_caret = true;
        self.view.desired_cell = None;
    }

    /// Called every frame: lets line counts catch up with the background indexer.
    pub fn tick(&mut self) -> bool {
        if self.is_ready() && (self.buf.index.failed() || self.buf.source.has_io_error()) && self.banner.is_none() {
            self.banner = Some(Banner::Error(super::i18n::tr("Reading the file failed (was it removed, truncated or is the drive unavailable?). Display may be incomplete.").into()));
        }
        let idx = self.buf.index.indexed_bytes();
        let mut changed = false;
        if idx != self.last_indexed {
            self.last_indexed = idx;
            if self.buf.has_inexact() {
                changed |= self.buf.refresh_counts(512);
            }
        }
        changed
    }

    // ------------------------------------------------------------------------------------------
    // Editing.

    fn push_undo(&mut self, swaps: Vec<Swap>, sel_before: (u64, u64), sel_after: (u64, u64), kind: EditKind) {
        self.redo.clear();
        let now = Instant::now();
        let state_before = self.content_state;
        self.content_state = self.next_content_state;
        self.next_content_state += 1;
        if let Some(last) = self.undo.last_mut() {
            let coalesce = kind != EditKind::Other
                && last.kind == kind
                && now.duration_since(last.at) < Duration::from_millis(1500)
                && last.sel_after == sel_before;
            if coalesce {
                last.swaps.extend(swaps);
                last.sel_after = sel_after;
                last.at = now;
                last.state_after = self.content_state;
                return;
            }
        }
        self.undo.push(UndoGroup { swaps, sel_before, sel_after, kind, at: now, state_before, state_after: self.content_state });
        if self.undo.len() > 10_000 {
            self.undo.remove(0);
        }
    }

    /// Replaces `range` with `bytes` (already encoded) and moves the caret after the insertion.
    pub fn edit(&mut self, range: std::ops::Range<u64>, bytes: &[u8], kind: EditKind) {
        if !self.editable() {
            return;
        }
        let sel_before = (self.view.anchor, self.view.caret);
        let swap = self.buf.replace(range.clone(), bytes);
        self.shift_bookmarks(range.clone(), bytes.len() as u64);
        let caret = range.start.min(self.buf.len()) + bytes.len() as u64;
        self.version += 1;
        self.view.anchor = caret;
        self.view.caret = caret;
        self.view.desired_cell = None;
        self.view.scroll_to_caret = true;
        self.push_undo(vec![swap], sel_before, (caret, caret), kind);
    }

    /// Records an externally built swap (batch replace, duplicate line) as one undo step.
    pub fn record(&mut self, swap: Swap, sel_after: (u64, u64)) {
        let sel_before = (self.view.anchor, self.view.caret);
        self.clamp_bookmarks();
        self.version += 1;
        self.view.anchor = sel_after.0;
        self.view.caret = sel_after.1;
        self.view.scroll_to_caret = true;
        self.push_undo(vec![swap], sel_before, sel_after, EditKind::Other);
    }

    pub fn insert_text(&mut self, text: &str) {
        let (s, e) = self.selection();
        let bytes = self.buf.codec.encode(text);
        let kind = if text.chars().count() <= 2 && !text.contains('\n') { EditKind::Typing } else { EditKind::Other };
        self.edit(s..e, &bytes, kind);
    }

    pub fn delete_selection(&mut self) -> bool {
        let (s, e) = self.selection();
        if s == e {
            return false;
        }
        self.edit(s..e, &[], EditKind::Other);
        true
    }

    /// Length of a CRLF pair ending at `off`, if any.
    fn crlf_before(&self, off: u64) -> Option<u64> {
        let codec = self.buf.codec;
        let cr = codec.cr_unit();
        let nl = codec.nl_unit();
        let n = (cr.len() + nl.len()) as u64;
        if off < n || codec.nl != bigtext::encoding::NlByte::Lf {
            return None;
        }
        let b = self.buf.read(off - n..off)?;
        (b.starts_with(cr) && b[cr.len()..] == *nl).then_some(n)
    }

    pub fn backspace(&mut self) {
        if self.delete_selection() {
            return;
        }
        let c = self.view.caret;
        if c <= self.min_caret() {
            return;
        }
        let start = match self.crlf_before(c) {
            Some(n) => c - n,
            None => self.buf.prev_char(c),
        };
        self.edit(start.max(self.min_caret())..c, &[], EditKind::Delete);
    }

    pub fn delete_forward(&mut self) {
        if self.delete_selection() {
            return;
        }
        let c = self.view.caret;
        if c >= self.buf.len() {
            return;
        }
        let mut end = self.buf.next_char(c);
        let cr = self.buf.codec.cr_unit().len() as u64;
        let nl = self.buf.codec.nl_unit().len() as u64;
        if let Some(n) = self.crlf_before((c + cr + nl).min(self.buf.len())) {
            if c + n == (c + cr + nl).min(self.buf.len()) {
                end = c + n;
            }
        }
        let sel = (self.view.anchor, self.view.caret);
        let swap = self.buf.replace(c..end, &[]);
        self.version += 1;
        self.view.scroll_to_caret = true;
        self.push_undo(vec![swap], sel, sel, EditKind::Delete);
    }

    /// Keeps bookmarks on their lines across an edit of `range` replaced by `inserted` bytes.
    fn shift_bookmarks(&mut self, range: std::ops::Range<u64>, inserted: u64) {
        if self.bookmarks.is_empty() {
            return;
        }
        let removed = range.end - range.start;
        for b in &mut self.bookmarks {
            if *b >= range.end && (*b > range.start || removed > 0) {
                *b = *b - removed + inserted;
            } else if *b > range.start {
                *b = range.start;
            }
        }
        self.bookmarks.dedup();
    }

    /// Drops bookmarks that no longer fit (after batch edits or undo/redo).
    fn clamp_bookmarks(&mut self) {
        let len = self.buf.len();
        self.bookmarks.retain(|&b| b <= len);
    }

    /// Toggles a bookmark on the line containing the caret.
    pub fn toggle_bookmark(&mut self) {
        let Some((ls, _, _)) = self.line_bounds(self.view.caret, LINE_CMD_LIMIT) else { return };
        match self.bookmarks.binary_search(&ls) {
            Ok(i) => {
                self.bookmarks.remove(i);
            }
            Err(i) => self.bookmarks.insert(i, ls),
        }
    }

    /// Moves to the next (or previous) bookmark, wrapping around.
    pub fn goto_bookmark(&mut self, forward: bool) {
        if self.bookmarks.is_empty() {
            return;
        }
        let c = self.view.caret;
        let line_start = self.line_bounds(c, LINE_CMD_LIMIT).map(|b| b.0).unwrap_or(c);
        let target = if forward {
            self.bookmarks.iter().copied().find(|&b| b > line_start).unwrap_or(self.bookmarks[0])
        } else {
            self.bookmarks.iter().rev().copied().find(|&b| b < line_start).unwrap_or(*self.bookmarks.last().unwrap())
        };
        self.set_caret(target, false);
        self.view.center();
    }

    pub fn undo(&mut self) {
        if !self.is_ready() {
            return;
        }
        if let Some(g) = self.undo.pop() {
            for s in g.swaps.iter().rev() {
                self.buf.apply(s, false);
            }
            self.version += 1;
            self.view.anchor = g.sel_before.0.min(self.buf.len());
            self.view.caret = g.sel_before.1.min(self.buf.len());
            self.view.scroll_to_caret = true;
            self.content_state = g.state_before;
            self.redo.push(g);
            self.clamp_bookmarks();
        }
    }

    pub fn redo(&mut self) {
        if !self.is_ready() {
            return;
        }
        if let Some(g) = self.redo.pop() {
            for s in &g.swaps {
                self.buf.apply(s, true);
            }
            self.version += 1;
            self.view.anchor = g.sel_after.0.min(self.buf.len());
            self.view.caret = g.sel_after.1.min(self.buf.len());
            self.view.scroll_to_caret = true;
            self.content_state = g.state_after;
            self.undo.push(g);
            self.clamp_bookmarks();
        }
    }

    /// Bounds of the real line containing `off`: (start, content_end, next_line_start).
    /// Scans at most `limit` bytes in each direction.
    pub fn line_bounds(&self, off: u64, limit: u64) -> Option<(u64, u64, u64)> {
        let codec = self.buf.codec;
        let unit = codec.unit() as u64;
        let len = self.buf.len();
        let lo = off.saturating_sub(limit);
        let lo = lo - lo % unit;
        let before = self.buf.read(lo..off)?;
        let start = match codec.rfind_nl(&before, lo) {
            Some(i) => lo + i as u64 + unit,
            None if lo == 0 => 0,
            None => return None,
        };
        let hi = (off + limit).min(len);
        let after = self.buf.read(off..hi)?;
        let (ce, next) = match codec.find_nl(&after, off) {
            Some(i) => {
                let nl = off + i as u64;
                let cr = codec.cr_unit();
                let mut ce = nl;
                if codec.nl == bigtext::encoding::NlByte::Lf && i >= cr.len() && &after[i - cr.len()..i] == cr {
                    ce -= cr.len() as u64;
                }
                (ce, nl + unit)
            }
            None if hi == len => (len, len),
            None => return None,
        };
        Some((start, ce, next))
    }

    /// Lines (and whether that count is exact), characters (for selections up to
    /// [`SEL_CHARS_MAX`]) and bytes of the selection. The character count is cached.
    pub fn selection_stats(&self) -> Option<(u64, bool, Option<u64>, u64)> {
        let (s, e) = self.selection();
        if s == e {
            return None;
        }
        let unit = self.buf.codec.unit() as u64;
        let (a, ea) = self.buf.line_of(s);
        // A selection that ends at a line start (a triple click) does not count that line.
        let (b, eb) = self.buf.line_of(e.saturating_sub(unit).max(s));
        let key = (s, e, self.version);
        let chars = match self.sel_chars.get() {
            Some((k, n)) if k == key => n,
            _ => {
                let n = (e - s <= SEL_CHARS_MAX).then(|| self.buf.read(s..e).map(|bytes| self.buf.codec.decode_lossy(&bytes).chars().count() as u64)).flatten();
                if n.is_some() {
                    self.sel_chars.set(Some((key, n)));
                }
                n
            }
        };
        Some((b - a + 1, ea && eb, chars, e - s))
    }

    /// Milliseconds between the timestamps of the first and the last selected line, when the
    /// selection spans lines that both start with a time.
    pub fn selection_time_delta(&self) -> Option<i64> {
        let (s, e) = self.selection();
        let e = e.saturating_sub(self.buf.codec.unit() as u64).max(s);
        let a = self.line_bounds(s, 64 * 1024)?.0;
        let b = self.line_bounds(e, 64 * 1024)?.0;
        if a == b {
            return None;
        }
        let (x, y) = (bigtext::timestamp::stamp_at(&self.buf, a)?, bigtext::timestamp::stamp_at(&self.buf, b)?);
        (x.has_date == y.has_date).then_some(y.ms - x.ms)
    }

    /// Selects the whole line containing `off` including its line break (triple click).
    pub fn select_line(&mut self, off: u64) {
        if let Some((ls, _ce, next)) = self.line_bounds(off, LINE_CMD_LIMIT) {
            self.select(ls.max(self.min_caret()), next);
            self.view.scroll_to_caret = false;
        }
    }

    pub fn duplicate_line(&mut self) {
        if !self.editable() {
            return;
        }
        let (s, e) = self.selection();
        if s != e {
            let pieces = self.buf.copy_pieces(s..e);
            let swap = self.buf.insert_pieces(e, pieces);
            self.record(swap, (e, e + (e - s)));
            return;
        }
        let Some((ls, _ce, next)) = self.line_bounds(self.view.caret, LINE_CMD_LIMIT) else {
            self.banner = Some(Banner::Info(super::i18n::tr("Line is too long to duplicate").into()));
            return;
        };
        let caret = self.view.caret;
        let len = self.buf.len();
        if next == len && !self.ends_with_nl() {
            // Last line without newline: insert "\n" + line.
            let nl = self.buf.codec.line_break(self.line_ending);
            let mut pieces = self.buf.copy_pieces(ls..len);
            let sel = (self.view.anchor, self.view.caret);
            let s1 = self.buf.replace(len..len, &nl);
            let at = self.buf.len();
            let s2 = self.buf.insert_pieces(at, std::mem::take(&mut pieces));
            self.version += 1;
            let nc = caret + (len - ls) + nl.len() as u64;
            self.view.anchor = nc;
            self.view.caret = nc;
            self.push_undo(vec![s1, s2], sel, (nc, nc), EditKind::Other);
        } else {
            let pieces = self.buf.copy_pieces(ls..next);
            let swap = self.buf.insert_pieces(next, pieces);
            let nc = caret + (next - ls);
            self.record(swap, (nc, nc));
        }
    }

    fn ends_with_nl(&self) -> bool {
        let len = self.buf.len();
        let u = self.buf.codec.nl_unit();
        len >= u.len() as u64 && self.buf.read(len - u.len() as u64..len).is_some_and(|b| *b == *u)
    }

    pub fn delete_line(&mut self) {
        if !self.editable() {
            return;
        }
        let Some((ls, _ce, next)) = self.line_bounds(self.view.caret, LINE_CMD_LIMIT) else {
            self.banner = Some(Banner::Info(super::i18n::tr("Line is too long to delete with this command; select it instead").into()));
            return;
        };
        let start = if next == self.buf.len() && ls > 0 && !self.ends_with_nl() {
            // Last line: also remove the preceding line break.
            self.crlf_before(ls).map(|n| ls - n).unwrap_or(ls.saturating_sub(self.buf.codec.unit() as u64))
        } else {
            ls
        };
        self.edit(start..next, &[], EditKind::Other);
        let c = self.view.caret.min(self.buf.len());
        self.set_caret(c, false);
    }

    /// Leading whitespace of the line containing `off` (for auto-indent).
    pub fn indent_of_line(&self, off: u64) -> Vec<u8> {
        let Ok(seg) = rows::seg_containing(&self.buf, off) else { return Vec::new() };
        if !seg.line_start {
            return Vec::new();
        }
        let end = off.min(seg.content_end).min(seg.start + 256);
        let Some(b) = self.buf.read(seg.start..end) else { return Vec::new() };
        let codec = self.buf.codec;
        let mut out = Vec::new();
        let mut i = 0;
        while i < b.len() {
            let (c, l) = codec.decode_char(&b[i..]);
            if c == ' ' || c == '\t' {
                out.extend_from_slice(&b[i..i + l]);
                i += l;
            } else {
                break;
            }
        }
        out
    }

    pub fn newline(&mut self) {
        let mut bytes = self.buf.codec.line_break(self.line_ending);
        bytes.extend(self.indent_of_line(self.view.caret));
        let (s, e) = self.selection();
        self.edit(s..e, &bytes, EditKind::Other);
    }

    /// Selection text for the clipboard if it is small enough to copy synchronously.
    pub fn selection_text(&self, max: u64) -> Option<Result<String, u64>> {
        let (s, e) = self.selection();
        if s == e {
            return None;
        }
        if e - s > max {
            return Some(Err(e - s));
        }
        let b = self.buf.read(s..e)?;
        Some(Ok(self.buf.codec.decode_lossy(&b)))
    }

    /// Goes to 1-based `line`; estimated positions are refined once the index catches up.
    pub fn goto_line(&mut self, line: u64) {
        let target = line.max(1) - 1;
        let (off, exact) = match self.buf.line_start(target) {
            Some(x) => x,
            None => {
                let (n, _) = self.buf.line_count();
                self.buf.line_start(n.saturating_sub(1)).unwrap_or((self.buf.len(), false))
            }
        };
        let off = if exact { off } else { rows::seg_start(&self.buf, off).unwrap_or(off) };
        let off = off.max(self.min_caret());
        self.set_caret(off, false);
        self.view.center();
        self.view.pending_goto_line = if exact { None } else { Some((target, off)) };
    }

    /// Refines an estimated go-to once the index covers the requested line.
    pub fn refine_goto(&mut self) {
        if let Some((target, off)) = self.view.pending_goto_line {
            if self.view.caret != off || self.has_selection() {
                self.view.pending_goto_line = None;
                return;
            }
            if let Some((o, true)) = self.buf.line_start(target) {
                let o = o.max(self.min_caret());
                self.set_caret(o, false);
                self.view.center_caret = true;
                self.view.pending_goto_line = None;
            }
        }
    }

    pub fn goto_offset(&mut self, off: u64) {
        let off = self.buf.snap_char(off.min(self.buf.len()));
        self.set_caret(off, false);
        self.view.center();
    }
}
