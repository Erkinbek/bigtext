//! Table view for CSV / TSV documents.
//!
//! Virtualized like the text view: only the rows on screen are read (each bounded by the
//! segment length, see `bigtext::text::rows`) and only the visible columns are decoded. A row
//! is a line, so a quoted field that spans lines shows as several rows; the text itself is
//! never changed by the view.

use std::collections::BTreeSet;
use std::hash::{Hash, Hasher};
use std::ops::Range;

use egui::{pos2, vec2, Align, Align2, Color32, CursorIcon, Event, EventFilter, FontId, Key, Layout, Painter, Rect, Sense, Stroke, Ui, UiBuilder};

use bigtext::csv::{self, Dialect, DELIMITERS};
use bigtext::text::rows::{self, Pending, Row};

use super::highlight::Lang;
use super::text_view;
use crate::app::document::Document;
use crate::app::i18n::{tr, trf as tr_f};

const SCROLLBAR: f32 = 14.0;
const TOOLBAR_H: f32 = 30.0;
/// Columns size themselves to their content up to this many characters; drag the border in
/// the header for more.
const MAX_AUTO_CHARS: usize = 40;
const MIN_CHARS: usize = 3;
/// Width of a column that has not been on screen yet, in characters.
const DEFAULT_CHARS: usize = 12;
const PAD: f32 = 6.0;

pub struct TableState {
    /// Detected from the file the first time the table is shown.
    pub dialect: Option<Dialect>,
    /// The first row holds the column names.
    pub header: bool,
    /// Column widths in points (0 = not measured yet). They grow to fit what has been on
    /// screen, except for columns whose border the user dragged (`fixed`).
    pub widths: Vec<f32>,
    pub fixed: Vec<bool>,
    pub scroll_x: f32,
    /// Selected columns: click a header; Ctrl+click adds or removes one, Shift+click a range.
    pub selected: BTreeSet<usize>,
    anchor: Option<usize>,
    /// Hash of the first line. When it changes (an edit, undo, another delimiter) the columns
    /// may have moved, so the layout and the selection start over.
    first_line: Option<u64>,
    /// Header cells drawn in the last frame (the self-test clicks them).
    pub header_cells: Vec<(usize, Rect)>,
    /// Lines before the table (`sep=;`, comments, a title); `dialect.start` is where it begins.
    pub skip: usize,
    /// Columns 0..frozen stay on the left while the others scroll.
    pub frozen: usize,
    /// Columns left out of the view (the file is not changed).
    pub hidden: BTreeSet<usize>,
}

impl Default for TableState {
    fn default() -> Self {
        TableState {
            dialect: None,
            header: true,
            widths: Vec::new(),
            fixed: Vec::new(),
            scroll_x: 0.0,
            selected: BTreeSet::new(),
            anchor: None,
            first_line: None,
            header_cells: Vec::new(),
            skip: 0,
            frozen: 0,
            hidden: BTreeSet::new(),
        }
    }
}

impl TableState {
    /// Forgets the column layout and the selection.
    pub fn reset_columns(&mut self) {
        self.widths.clear();
        self.fixed.clear();
        self.scroll_x = 0.0;
        self.selected.clear();
        self.anchor = None;
        self.frozen = 0;
        self.hidden.clear();
    }

    /// After the header was edited on purpose (a rename): keep the layout.
    pub fn keep_layout(&mut self) {
        self.first_line = None;
    }

    /// After columns `a` and `b` were exchanged in the document: their widths go with them.
    pub fn columns_swapped(&mut self, a: usize, b: usize) {
        if a.max(b) < self.widths.len() {
            self.widths.swap(a, b);
            self.fixed.swap(a, b);
        }
        self.selected.clear();
        self.selected.insert(b);
        self.anchor = Some(b);
        self.first_line = None;
        let (ha, hb) = (self.hidden.remove(&a), self.hidden.remove(&b));
        if ha {
            self.hidden.insert(b);
        }
        if hb {
            self.hidden.insert(a);
        }
    }

    /// After `cols` were deleted from the document: the other columns keep their widths.
    pub fn columns_deleted(&mut self, cols: &[usize]) {
        for &c in cols.iter().rev() {
            if c < self.widths.len() {
                self.widths.remove(c);
                self.fixed.remove(c);
            }
        }
        self.selected.clear();
        self.anchor = None;
        // The first line changed on purpose; take the new one as it is.
        self.first_line = None;
        let gone = |c: usize| cols.iter().filter(|&&d| d < c).count();
        self.hidden = self.hidden.iter().filter(|c| !cols.contains(c)).map(|&c| c - gone(c)).collect();
        self.frozen -= gone(self.frozen);
    }

    fn click_header(&mut self, col: usize, m: egui::Modifiers) {
        match self.anchor {
            Some(a) if m.shift => {
                if !m.command {
                    self.selected.clear();
                }
                self.selected.extend(a.min(col)..=a.max(col));
            }
            _ if m.command => {
                if !self.selected.remove(&col) {
                    self.selected.insert(col);
                }
                self.anchor = Some(col);
            }
            _ => {
                let only = self.selected.len() == 1 && self.selected.contains(&col);
                self.selected.clear();
                if !only {
                    self.selected.insert(col);
                }
                self.anchor = Some(col);
            }
        }
    }
}

pub enum TableAction {
    /// Delete these columns (sorted) with their values from every row, after asking.
    DeleteColumns { cols: Vec<usize>, names: Vec<String> },
    Sort { col: usize, desc: bool },
    Stats { col: usize },
    Filter { col: usize, name: String },
    Rename { col: usize, name: String },
    /// Exchange column `col` with its neighbour.
    Move { col: usize, right: bool },
    /// Write these columns (sorted) to a new file.
    Export { cols: Vec<usize> },
    /// Write these columns (all when `None`) as JSON, JSON Lines, Markdown or Excel.
    ExportAs { cols: Option<Vec<usize>>, format: bigtext::table_export::Format },
    /// Count (and sum) the rows per value of this column.
    GroupBy { col: usize },
    /// Rows that meet several conditions, starting with one on this column.
    Conditions { col: usize },
    /// Check every row (field counts, quotes, numbers).
    CheckRows,
    /// A pivot table with this column's values as rows.
    Pivot { col: usize },
    /// Keep columns 0..upto on the left (0: none).
    Freeze { upto: usize },
    /// Leave these columns out of the view.
    Hide { cols: Vec<usize> },
}

pub struct TableEnv<'a> {
    pub font_size: &'a mut f32,
    pub focus: bool,
    pub actions: &'a mut Vec<TableAction>,
}

/// One visible row split into fields.
struct Line {
    row: Row,
    /// Document offset of `bytes[0]` (after the BOM on the first line).
    base: u64,
    bytes: Vec<u8>,
    fields: Vec<Range<usize>>,
}

impl Line {
    fn value(&self, codec: &bigtext::encoding::Codec, d: Dialect, col: usize) -> Option<String> {
        let r = self.fields.get(col)?;
        Some(display(&csv::field_value(codec, d, &self.bytes[r.clone()])))
    }

    /// Index of the field holding document offset `off`.
    fn field_at(&self, off: u64) -> Option<usize> {
        let rel = off.checked_sub(self.base)? as usize;
        self.fields.iter().position(|r| rel >= r.start && rel <= r.end)
    }
}

/// Control characters (tabs, line breaks inside quotes) show as spaces.
fn display(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

fn is_number(s: &str) -> bool {
    let t = s.trim();
    t.bytes().any(|b| b.is_ascii_digit()) && t.bytes().all(|b| b.is_ascii_digit() || b"+-.eE".contains(&b)) && t.parse::<f64>().is_ok()
}

fn read_line(doc: &Document, d: Dialect, row: Row) -> Option<Line> {
    let base = row.start.max(doc.min_caret());
    let bytes = doc.buf.read(base..row.end.max(base))?.to_vec();
    let mut fields = Vec::new();
    csv::split_fields(&doc.buf.codec, d, &bytes, &mut fields);
    Some(Line { row, base, bytes, fields })
}

fn detect_dialect(doc: &mut Document) {
    if !doc.is_ready() {
        return;
    }
    let start = doc.min_caret();
    let end = (start + 64 * 1024).min(doc.buf.len());
    // Not loaded yet (slow drive): try again on the next frame.
    let Some(sample) = doc.buf.read(start..end) else { return };
    let preferred = match doc.effective_lang() {
        Lang::Csv(sep) => sep,
        _ => b',',
    };
    let g = csv::detect_table(&doc.buf.codec, &sample, end == doc.buf.len(), preferred, start);
    let st = &mut doc.table_state;
    st.dialect = Some(g.dialect);
    st.header = g.header;
    st.skip = g.skip;
}

/// Starts the table after `skip` lines (from the menu), keeping the delimiter and quote.
fn set_skip(doc: &mut Document, skip: usize) {
    let Some(mut d) = doc.table_state.dialect else { return };
    let start = doc.min_caret();
    let Some(sample) = doc.buf.read(start..(start + (1 << 20)).min(doc.buf.len())) else { return };
    let codec = doc.buf.codec;
    let unit = codec.unit();
    let mut at = 0;
    for _ in 0..skip {
        match codec.find_nl(&sample[at..], start + at as u64) {
            Some(i) => at += i + unit,
            None => {
                at = sample.len();
                break;
            }
        }
    }
    d.start = if skip == 0 { 0 } else { start + at as u64 };
    let st = &mut doc.table_state;
    st.dialect = Some(d);
    st.skip = skip;
    st.reset_columns();
}

/// Moves the first visible row by `n` rows.
fn step(doc: &mut Document, n: i64) {
    if let Ok(top) = rows::row_at(&doc.buf, doc.view.top, None) {
        if let Ok(r) = rows::step_rows(&doc.buf, top, n, None) {
            doc.view.top = r.start;
        }
    }
}

pub fn show(ui: &mut Ui, doc: &mut Document, env: &mut TableEnv) {
    let font = FontId::monospace(*env.font_size);
    let (cw, fh) = ui.ctx().fonts_mut(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
    let cw = cw.max(1.0);
    let rh = (fh + 8.0).round();
    let full = ui.available_rect_before_wrap();
    let id = ui.id().with(("tableview", doc.id));
    let visuals = ui.visuals().clone();
    if doc.table_state.dialect.is_none() {
        detect_dialect(doc);
    }
    let dialect = doc.table_state.dialect.unwrap_or(Dialect::new(b','));
    let codec = doc.buf.codec;
    let len = doc.buf.len();

    let toolbar_rect = Rect::from_min_max(full.min, pos2(full.right(), full.top() + TOOLBAR_H));
    let area = Rect::from_min_max(pos2(full.left(), toolbar_rect.bottom()), full.max);
    let (line_est, _) = doc.buf.line_count();
    let gutter_w = text_view::digits(line_est).max(3) as f32 * cw + 16.0;
    let header_rect = Rect::from_min_max(pos2(area.left() + gutter_w, area.top()), pos2(area.right() - SCROLLBAR, area.top() + rh));
    let body = Rect::from_min_max(pos2(header_rect.left(), header_rect.bottom()), pos2(header_rect.right(), area.bottom() - SCROLLBAR));
    let gutter = Rect::from_min_max(area.min, pos2(header_rect.left(), body.bottom()));
    let vbar = Rect::from_min_max(pos2(body.right(), area.top()), pos2(area.right(), body.bottom()));
    let hbar = Rect::from_min_max(pos2(body.left(), body.bottom()), pos2(body.right(), area.bottom()));
    let n_rows = ((body.height() / rh).ceil() as usize).max(1);

    let mut pending = false;
    let table_start = dialect.first(&doc.buf).max(doc.min_caret());
    let head = match rows::row_at(&doc.buf, table_start, None) {
        Ok(r) => Some(r),
        Err(Pending) => {
            pending = true;
            None
        }
    };
    let data_start = match (doc.table_state.header, head) {
        (true, Some(h)) if h.seg.is_last(len) => len,
        (true, Some(h)) => h.seg.next,
        _ => table_start,
    };
    if let Some(first) = head.and_then(|h| doc.buf.read(h.start..h.end)) {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        first.hash(&mut hasher);
        let sig = Some(hasher.finish());
        let st = &mut doc.table_state;
        if st.first_line.is_some() && st.first_line != sig {
            let scroll_x = st.scroll_x;
            st.reset_columns();
            st.scroll_x = scroll_x;
        }
        st.first_line = sig;
    }

    // Input, against the current layout.
    let resp = ui.interact(body, id, Sense::click_and_drag());
    if env.focus && (resp.clicked() || resp.is_pointer_button_down_on() || ui.memory(|m| m.focused().is_none())) {
        resp.request_focus();
    }
    ui.memory_mut(|m| m.set_focus_lock_filter(id, EventFilter { tab: false, horizontal_arrows: true, vertical_arrows: true, escape: false }));
    let mut delete_key = false;
    if doc.is_ready() {
        if ui.rect_contains_pointer(area) {
            handle_wheel(ui, doc, rh, env);
        }
        if resp.has_focus() {
            delete_key = handle_keys(ui, doc, n_rows, data_start, cw);
        }
        if doc.view.scroll_to_caret || doc.view.center_caret {
            match scroll_to_caret(doc, n_rows) {
                Ok(()) => {
                    doc.view.scroll_to_caret = false;
                    doc.view.center_caret = false;
                }
                Err(Pending) => pending = true,
            }
        }
        doc.view.top = doc.view.top.max(data_start);
        // Never scroll past the last row (an empty line after the final line break is not one).
        if len > 0 && doc.view.top >= len && data_start < len {
            if let Ok(r) = rows::row_at(&doc.buf, len - 1, None) {
                doc.view.top = r.start.max(data_start);
            }
        }
    }

    // Rows on screen.
    let rows: Vec<Row> = if doc.is_ready() && doc.view.top < len {
        match text_view::layout_rows(&doc.buf, doc.view.top, n_rows, None) {
            Ok(r) => r.into_iter().filter(|r| r.start < len).collect(),
            Err(Pending) => {
                pending = true;
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    let mut lines = Vec::with_capacity(rows.len());
    for &row in &rows {
        match read_line(doc, dialect, row) {
            Some(l) => lines.push(l),
            None => {
                pending = true;
                break;
            }
        }
    }
    let head_line = if doc.table_state.header { head.and_then(|h| read_line(doc, dialect, h)) } else { None };

    // Columns: measure the visible ones, then lay out.
    let ncols = lines.iter().chain(head_line.as_ref()).map(|l| l.fields.len()).max().unwrap_or(0).max(doc.table_state.widths.len());
    let st = &mut doc.table_state;
    st.widths.resize(ncols, 0.0);
    st.fixed.resize(ncols, false);
    let default_w = (DEFAULT_CHARS as f32 + 1.0) * cw + 2.0 * PAD;
    let name = |col: usize| -> String {
        match &head_line {
            Some(h) => h.value(&codec, dialect, col).filter(|s| !s.trim().is_empty()).unwrap_or_else(|| format!("({})", col + 1)),
            None => tr_f("Column {n}", &[("n", &(col + 1).to_string())]),
        }
    };
    st.hidden.retain(|&c| c < ncols);
    let measure: Vec<usize> = visible_columns(&st.widths, default_w, st.scroll_x, body.width()).collect();
    for col in measure {
        if st.fixed[col] || st.hidden.contains(&col) {
            continue;
        }
        let chars = lines.iter().filter_map(|l| l.value(&codec, dialect, col)).map(|v| v.chars().count()).chain([name(col).chars().count() + 1]).max().unwrap_or(0);
        let want = (chars.clamp(MIN_CHARS, MAX_AUTO_CHARS) as f32 + 1.0) * cw + 2.0 * PAD;
        st.widths[col] = st.widths[col].max(want);
    }
    let width = |st: &TableState, col: usize| if st.widths[col] > 0.0 { st.widths[col] } else { default_w };
    let shown_w = |st: &TableState, col: usize| if st.hidden.contains(&col) { 0.0 } else { width(st, col) };
    let mut xs = Vec::with_capacity(ncols + 1);
    xs.push(0.0f32);
    for col in 0..ncols {
        xs.push(xs[col] + shown_w(st, col));
    }
    // Frozen columns, at most two thirds of the width.
    let mut frozen = st.frozen.min(ncols);
    while frozen > 0 && xs[frozen] > body.width() * 0.66 {
        frozen -= 1;
    }
    let strip = xs[frozen];
    let total = xs[ncols];
    let max_scroll = (total - body.width() + 2.0 * cw).max(0.0);
    st.scroll_x = st.scroll_x.clamp(0.0, max_scroll);
    let scroll_x = st.scroll_x;
    let shift = |col: usize| if col < frozen { 0.0 } else { scroll_x };
    // Left and right edges of a column on screen; frozen columns do not scroll.
    let left = |col: usize| body.left() + xs[col] - shift(col);
    let right = |col: usize| body.left() + xs[col + 1] - shift(col);
    let split = body.left() + strip;
    // The part of `r` a column may paint in: the frozen strip, or right of it.
    let clip_of = |col: usize, r: Rect| if col < frozen { Rect::from_min_max(r.min, pos2(r.right().min(split), r.bottom())) } else { Rect::from_min_max(pos2(r.left().max(split), r.top()), r.max) };
    let visible: Vec<usize> = (0..ncols).filter(|&c| !st.hidden.contains(&c) && (c < frozen || (right(c) > split && left(c) < body.right()))).collect();
    st.selected.retain(|&c| c < ncols);
    let selected = st.selected.clone();

    // Toolbar (it may ask to delete the selected columns), and the Delete key.
    ui.painter().rect_filled(toolbar_rect, 0.0, visuals.faint_bg_color);
    let can_delete = !selected.is_empty() && selected.len() < ncols && doc.editable();
    let request = toolbar(ui, doc, toolbar_rect, ncols, can_delete) || (delete_key && can_delete);
    if request {
        let cols: Vec<usize> = selected.iter().copied().collect();
        let names = cols.iter().map(|&c| name(c)).collect();
        env.actions.push(TableAction::DeleteColumns { cols, names });
    }

    // Paint.
    let painter = ui.painter_at(area);
    let grid = visuals.widgets.noninteractive.bg_stroke.color;
    let zebra = if visuals.dark_mode { Color32::from_white_alpha(6) } else { Color32::from_black_alpha(8) };
    let text_color = visuals.text_color();
    let weak = visuals.weak_text_color();
    painter.rect_filled(area, 0.0, visuals.extreme_bg_color);
    painter.rect_filled(gutter, 0.0, visuals.faint_bg_color);
    painter.rect_filled(header_rect.union(Rect::from_min_max(pos2(gutter.left(), header_rect.top()), header_rect.max)), 0.0, visuals.faint_bg_color);
    let body_painter = painter.with_clip_rect(body);
    let sel_tint = visuals.selection.bg_fill.gamma_multiply(0.35);
    for &col in visible.iter().filter(|c| selected.contains(c)) {
        let r = Rect::from_min_max(pos2(left(col), body.top()), pos2(right(col), body.top() + lines.len() as f32 * rh));
        body_painter.with_clip_rect(clip_of(col, body)).rect_filled(r, 0.0, sel_tint);
    }
    let caret = doc.view.caret;
    let (mut line, line_exact) = lines.first().map(|l| doc.buf.line_of(l.row.start)).unwrap_or((0, true));
    for (i, l) in lines.iter().enumerate() {
        let y = body.top() + i as f32 * rh;
        let row_rect = Rect::from_min_max(pos2(body.left(), y), pos2(body.right(), y + rh));
        let starts_line = l.row.is_first_in_seg() && l.row.seg.line_start;
        if i > 0 && starts_line {
            line += 1;
        }
        if i % 2 == 1 {
            body_painter.rect_filled(row_rect, 0.0, zebra);
        }
        for &col in &visible {
            let Some(v) = l.value(&codec, dialect, col) else { continue };
            let cell = Rect::from_min_max(pos2(left(col), y), pos2(right(col), y + rh));
            paint_text(&body_painter.with_clip_rect(clip_of(col, cell.intersect(body))), cell, &v, &font, text_color, cw, is_number(&v));
        }
        if caret >= l.row.start && caret <= l.row.end.max(l.row.start) {
            if let Some(col) = l.field_at(caret) {
                let cell = Rect::from_min_max(pos2(left(col), y), pos2(right(col), y + rh)).shrink(1.0);
                body_painter.with_clip_rect(clip_of(col, body)).rect_stroke(cell, 2.0, Stroke::new(1.5, visuals.selection.stroke.color), egui::StrokeKind::Inside);
            }
        }
        let label = match (starts_line, line_exact, l.row.seg.line_start) {
            (true, true, _) => format!("{}", line + 1),
            (true, false, _) => format!("~{}", line + 1),
            (false, _, false) if l.row.is_first_in_seg() => "↪".into(),
            _ => String::new(),
        };
        painter.text(pos2(gutter.right() - 8.0, y + rh / 2.0), Align2::RIGHT_CENTER, label, font.clone(), weak);
        body_painter.hline(body.x_range(), y + rh, Stroke::new(0.5, grid));
    }
    for &col in &visible {
        let x = right(col);
        painter.with_clip_rect(clip_of(col, header_rect.union(body)).expand2(vec2(1.0, 0.0))).vline(x, header_rect.top()..=body.top() + lines.len() as f32 * rh, Stroke::new(1.0, grid));
    }
    painter.hline(gutter.left()..=header_rect.right(), header_rect.bottom(), Stroke::new(1.0, grid));
    painter.text(pos2(gutter.right() - 8.0, header_rect.center().y), Align2::RIGHT_CENTER, "#", font.clone(), weak);

    // Header cells: names, selection, tooltips and resize handles.
    let header_painter = painter.with_clip_rect(header_rect);
    let strong = visuals.strong_text_color();
    let mut header_cells = Vec::with_capacity(visible.len());
    for &col in &visible {
        let cell = Rect::from_min_max(pos2(left(col), header_rect.top()), pos2(right(col), header_rect.bottom()));
        let on = selected.contains(&col);
        if on {
            header_painter.rect_filled(cell.shrink2(vec2(0.5, 0.0)), 0.0, visuals.selection.bg_fill);
        }
        let n = name(col);
        let color = if on { visuals.selection.stroke.color } else { strong };
        let hit = clip_of(col, cell.intersect(header_rect));
        paint_text(&header_painter.with_clip_rect(hit), cell, &n, &font, color, cw, false);
        let r = ui.interact(hit, id.with(("head", col)), Sense::click());
        if r.clicked() {
            let m = ui.input(|i| i.modifiers);
            doc.table_state.click_header(col, m);
            resp.request_focus();
        }
        let editable = doc.editable();
        // The menu acts on the selected columns when it opens on one of them.
        r.context_menu(|ui| {
            let cols: Vec<usize> = if selected.contains(&col) { selected.iter().copied().collect() } else { vec![col] };
            let names: Vec<String> = cols.iter().map(|&c| name(c)).collect();
            column_menu(ui, env.actions, col, &n, ncols, cols, names, editable);
            ui.separator();
            let unfreeze = frozen == col + 1;
            if ui.button(tr(if unfreeze { "Unfreeze columns" } else { "Freeze columns up to here" })).clicked() {
                env.actions.push(TableAction::Freeze { upto: if unfreeze { 0 } else { col + 1 } });
                ui.close();
            }
            if ui.button(tr("Hide from the view")).clicked() {
                let cols: Vec<usize> = if selected.contains(&col) { selected.iter().copied().collect() } else { vec![col] };
                env.actions.push(TableAction::Hide { cols });
                ui.close();
            }
        });
        r.on_hover_text(format!(
            "{n}\n{}\n{}",
            tr_f("Column {n}", &[("n", &(col + 1).to_string())]),
            tr("Click to select; Ctrl+click adds, Shift+click selects a range; right-click for more")
        ));
        header_cells.push((col, hit));
    }
    doc.table_state.header_cells = header_cells;
    for &col in &visible {
        let x = right(col);
        let handle = Rect::from_min_max(pos2(x - 4.0, header_rect.top()), pos2(x + 4.0, header_rect.bottom()));
        if !header_rect.intersects(handle) {
            continue;
        }
        let r = ui.interact(handle, id.with(("resize", col)), Sense::click_and_drag());
        if r.hovered() || r.dragged() {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeColumn);
        }
        let st = &mut doc.table_state;
        if r.dragged() {
            st.widths[col] = (width(st, col) + r.drag_delta().x).max((MIN_CHARS as f32 + 1.0) * cw);
            st.fixed[col] = true;
        }
        if r.double_clicked() {
            // Back to fitting the content.
            st.widths[col] = 0.0;
            st.fixed[col] = false;
        }
    }

    // Clicks in the body: a click puts the caret in the cell, a double click opens it in the
    // text view.
    if let Some(pos) = resp.interact_pointer_pos().filter(|_| resp.clicked() || resp.double_clicked()) {
        let double = resp.double_clicked();
        let i = ((pos.y - body.top()) / rh).floor().max(0.0) as usize;
        let col = visible.iter().copied().find(|&c| pos.x >= left(c) && pos.x < right(c) && (c < frozen || pos.x >= split));
        if let (Some(l), Some(col)) = (lines.get(i), col) {
            let off = l.fields.get(col).map(|r| l.base + r.start as u64).unwrap_or(l.row.end);
            doc.set_caret(off, false);
            doc.view.desired_cell = None;
            if double {
                doc.table = false;
                doc.view.center();
            } else {
                doc.view.scroll_to_caret = false;
            }
        }
    }

    doc.view.text_rect = Some(body);
    doc.view.cell = vec2(cw, rh);
    if doc.view.first_paint.is_none() && !lines.is_empty() {
        doc.view.first_paint = Some(std::time::Instant::now());
    }
    if lines.is_empty() && !pending && doc.is_ready() {
        let msg = tr(if len == 0 { "Empty file" } else { "No data rows" });
        painter.text(body.center(), Align2::CENTER_CENTER, msg, FontId::proportional(16.0), weak);
    }
    if pending {
        painter.text(body.right_top() + vec2(-8.0, 4.0), Align2::RIGHT_TOP, tr("Loading…"), FontId::proportional(13.0), weak);
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(30));
    }

    if doc.is_ready() {
        text_view::vertical_scrollbar(ui, doc, id, vbar, &rows, None);
        horizontal_scrollbar(ui, doc, id, hbar, total, body.width(), max_scroll);
    }
}

/// Right-click menu of a column header.
#[allow(clippy::too_many_arguments)]
fn column_menu(ui: &mut Ui, actions: &mut Vec<TableAction>, col: usize, name: &str, ncols: usize, cols: Vec<usize>, names: Vec<String>, editable: bool) {
    let mut act = |ui: &mut Ui, enabled: bool, label: &str, a: TableAction| {
        if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
            actions.push(a);
            ui.close();
        }
    };
    act(ui, editable, tr("Sort ascending (A → Z, 1 → 9)"), TableAction::Sort { col, desc: false });
    act(ui, editable, tr("Sort descending (Z → A, 9 → 1)"), TableAction::Sort { col, desc: true });
    ui.separator();
    act(ui, true, tr("Statistics…"), TableAction::Stats { col });
    act(ui, true, tr("Filter rows by this column…"), TableAction::Filter { col, name: name.to_string() });
    act(ui, true, tr("Filter rows by conditions…"), TableAction::Conditions { col });
    act(ui, true, tr("Group rows by this column…"), TableAction::GroupBy { col });
    act(ui, true, tr("Pivot by this column…"), TableAction::Pivot { col });
    act(ui, true, tr("Check rows…"), TableAction::CheckRows);
    ui.separator();
    act(ui, editable, tr("Rename…"), TableAction::Rename { col, name: name.to_string() });
    act(ui, editable && col > 0, tr("Move left"), TableAction::Move { col, right: false });
    act(ui, editable && col + 1 < ncols, tr("Move right"), TableAction::Move { col, right: true });
    ui.separator();
    let label = if cols.len() == 1 { tr("Export this column") } else { tr("Export selected columns") };
    ui.menu_button(label, |ui| {
        act(ui, true, "CSV…", TableAction::Export { cols: cols.clone() });
        for format in bigtext::table_export::Format::ALL {
            act(ui, true, &format!("{}…", tr(format.name())), TableAction::ExportAs { cols: Some(cols.clone()), format });
        }
    });
    act(ui, editable && cols.len() < ncols, tr("Delete…"), TableAction::DeleteColumns { names, cols });
}

/// Columns that overlap the visible width.
fn visible_columns(widths: &[f32], default_w: f32, scroll_x: f32, view_w: f32) -> impl Iterator<Item = usize> + '_ {
    let mut x = 0.0f32;
    widths.iter().enumerate().filter_map(move |(col, &w)| {
        let w = if w > 0.0 { w } else { default_w };
        let (a, b) = (x, x + w);
        x = b;
        (b > scroll_x && a < scroll_x + view_w).then_some(col)
    })
}

/// Draws `text` in `cell`, cut to fit with an ellipsis; numbers align right.
#[allow(clippy::too_many_arguments)]
fn paint_text(painter: &Painter, cell: Rect, text: &str, font: &FontId, color: Color32, cw: f32, right: bool) {
    let max = ((cell.width() - 2.0 * PAD) / cw).floor().max(1.0) as usize;
    let shown: String = if text.chars().count() > max { text.chars().take(max.saturating_sub(1)).chain(['…']).collect() } else { text.to_string() };
    if shown.is_empty() {
        return;
    }
    let y = cell.center().y;
    if right {
        painter.text(pos2(cell.right() - PAD, y), Align2::RIGHT_CENTER, shown, font.clone(), color);
    } else {
        painter.text(pos2(cell.left() + PAD, y), Align2::LEFT_CENTER, shown, font.clone(), color);
    }
}

/// Delimiter, header switch, column selection. Returns true when "Delete columns" was clicked.
fn toolbar(ui: &mut Ui, doc: &mut Document, rect: Rect, ncols: usize, can_delete: bool) -> bool {
    let doc_id = doc.id;
    let read_only = doc.read_only;
    let mut delete = false;
    let mut set_skip_later = None;
    ui.scope_builder(UiBuilder::new().max_rect(rect.shrink2(vec2(8.0, 3.0))).layout(Layout::left_to_right(Align::Center)), |ui| {
        let st = &mut doc.table_state;
        let cur = st.dialect.unwrap_or(Dialect::new(b','));
        ui.label(tr("Delimiter"));
        egui::ComboBox::from_id_salt(("delimiter", doc_id)).selected_text(tr(cur.name())).show_ui(ui, |ui| {
            for delim in DELIMITERS {
                if ui.selectable_label(cur.delim == delim, tr(Dialect::new(delim).name())).clicked() && cur.delim != delim {
                    st.dialect = Some(Dialect { delim, ..cur });
                    st.reset_columns();
                }
            }
        });
        ui.checkbox(&mut st.header, tr("First row is header"));
        let mut skip = st.skip;
        ui.menu_button(tr("Table…"), |ui| {
            ui.horizontal(|ui| {
                ui.label(tr("Skip lines before the table"));
                ui.add(egui::DragValue::new(&mut skip).range(0..=1000));
            });
            ui.horizontal(|ui| {
                ui.label(tr("Quote"));
                for (q, name) in [(b'"', "\""), (b'\'', "'")] {
                    if ui.selectable_label(cur.quote == q, name).clicked() && cur.quote != q {
                        st.dialect = Some(Dialect { quote: q, ..cur });
                        st.reset_columns();
                    }
                }
            });
        });
        if skip != st.skip {
            set_skip_later = Some(skip);
        }
        if !st.hidden.is_empty() && ui.button(tr_f("{n} hidden · Show all", &[("n", &st.hidden.len().to_string())])).on_hover_text(tr("Show the columns hidden from the view")).clicked() {
            st.hidden.clear();
        }
        ui.separator();
        if st.selected.is_empty() {
            ui.weak(tr_f("{n} columns · click a column header to select it", &[("n", &ncols.to_string())]));
        } else {
            let n = st.selected.len();
            ui.label(tr_f("{n} of {total} columns selected", &[("n", &n.to_string()), ("total", &ncols.to_string())]));
            let label = tr(if n == 1 { "Delete column" } else { "Delete columns" });
            let tip = tr(if read_only {
                "The document is read-only"
            } else if n >= ncols {
                "Keep at least one column"
            } else {
                "Delete the selected columns and their values from every row (Delete key)"
            });
            let button = egui::Button::new(egui::RichText::new(label).color(ui.visuals().error_fg_color));
            if ui.add_enabled(can_delete, button).on_hover_text(tip).on_disabled_hover_text(tip).clicked() {
                delete = true;
            }
            if ui.button(tr("Clear selection")).on_hover_text("Esc").clicked() {
                st.selected.clear();
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.button(tr("Text view")).on_hover_text(tr("Show the file as text (Ctrl+T)")).clicked() {
                doc.table = false;
            }
        });
    });
    if let Some(skip) = set_skip_later {
        set_skip(doc, skip);
    }
    delete
}

fn handle_wheel(ui: &mut Ui, doc: &mut Document, rh: f32, env: &mut TableEnv) {
    let (delta, zoom) = ui.input(|i| (i.smooth_scroll_delta(), i.zoom_delta()));
    if zoom != 1.0 {
        *env.font_size = (*env.font_size * zoom).clamp(6.0, 72.0);
        return;
    }
    if delta.y != 0.0 {
        let rows_f = -delta.y / rh + doc.view.wheel_rem;
        let n = rows_f.trunc() as i64;
        doc.view.wheel_rem = rows_f - n as f32;
        if n != 0 {
            step(doc, n);
        }
    }
    if delta.x != 0.0 {
        doc.table_state.scroll_x = (doc.table_state.scroll_x - delta.x).max(0.0);
    }
}

/// Scrolling, undo / redo and the column selection keys. Returns true for Delete.
fn handle_keys(ui: &mut Ui, doc: &mut Document, n_rows: usize, data_start: u64, cw: f32) -> bool {
    let page = n_rows.saturating_sub(2).max(1) as i64;
    let events = ui.input(|i| i.events.clone());
    let mut delete = false;
    for ev in events {
        let Event::Key { key, pressed: true, modifiers, .. } = ev else { continue };
        match key {
            Key::Z if modifiers.command && modifiers.shift => doc.redo(),
            Key::Z if modifiers.command => doc.undo(),
            Key::Y if modifiers.command => doc.redo(),
            Key::Delete => delete = true,
            Key::Escape => doc.table_state.selected.clear(),
            Key::ArrowDown => step(doc, 1),
            Key::ArrowUp => step(doc, -1),
            Key::PageDown => step(doc, page),
            Key::PageUp => step(doc, -page),
            Key::ArrowRight => doc.table_state.scroll_x += 8.0 * cw,
            Key::ArrowLeft => doc.table_state.scroll_x = (doc.table_state.scroll_x - 8.0 * cw).max(0.0),
            Key::Home if modifiers.command => {
                doc.view.top = data_start;
                doc.table_state.scroll_x = 0.0;
            }
            Key::End if modifiers.command => {
                let len = doc.buf.len();
                if let Ok(last) = rows::row_at(&doc.buf, len.saturating_sub(1), None) {
                    if let Ok(r) = rows::step_rows(&doc.buf, last, -page, None) {
                        doc.view.top = r.start;
                    }
                }
            }
            Key::Home => doc.table_state.scroll_x = 0.0,
            Key::End => doc.table_state.scroll_x = f32::MAX,
            _ => {}
        }
    }
    delete
}

/// Brings the row with the caret on screen (after go to line, find next, a click elsewhere).
fn scroll_to_caret(doc: &mut Document, n_rows: usize) -> Result<(), Pending> {
    let row = rows::row_at(&doc.buf, doc.view.caret, None)?;
    let top = doc.view.top;
    let on_screen = row.start >= top && {
        let shown = text_view::layout_rows(&doc.buf, top, n_rows.saturating_sub(1).max(1), None)?;
        shown.iter().any(|r| r.start == row.start)
    };
    if doc.view.center_caret || !on_screen {
        let back = if doc.view.center_caret || row.start < top { (n_rows / 2) as i64 } else { n_rows.saturating_sub(2) as i64 };
        doc.view.top = rows::step_rows(&doc.buf, row, -back, None)?.start;
    }
    Ok(())
}

fn horizontal_scrollbar(ui: &mut Ui, doc: &mut Document, id: egui::Id, bar: Rect, content: f32, view_w: f32, max_scroll: f32) {
    let painter = ui.painter_at(bar);
    let v = ui.visuals();
    painter.rect_filled(bar, 0.0, v.faint_bg_color);
    if max_scroll <= 0.0 {
        return;
    }
    let st = &mut doc.table_state;
    let track_w = bar.width();
    let thumb_w = (track_w * view_w / content.max(1.0)).clamp(24.0_f32.min(track_w), track_w);
    let x = bar.left() + (track_w - thumb_w) * (st.scroll_x / max_scroll).clamp(0.0, 1.0);
    let thumb = Rect::from_min_max(pos2(x, bar.top() + 2.0), pos2(x + thumb_w, bar.bottom() - 2.0));
    let r = ui.interact(bar, id.with("hbar"), Sense::click_and_drag());
    let color = if r.dragged() || r.hovered() { v.widgets.hovered.bg_fill } else { v.widgets.inactive.bg_fill };
    painter.rect_filled(thumb, 4.0, color);
    if r.dragged() || r.is_pointer_button_down_on() {
        if let Some(p) = r.interact_pointer_pos() {
            let f = ((p.x - bar.left() - thumb_w / 2.0) / (track_w - thumb_w).max(1.0)).clamp(0.0, 1.0);
            st.scroll_x = f * max_scroll;
        }
    }
}

