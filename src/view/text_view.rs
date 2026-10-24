//! Custom virtualized text view.
//!
//! Only the rows on screen are read and decoded, and of each row only the horizontal slice that
//! is visible. Rows come from [`bigtext::text::rows`], so every computation is bounded no matter
//! how long a line is. All positions are byte offsets; nothing here touches the whole file.

use std::ops::Range;

use egui::text::{LayoutJob, TextFormat};
use egui::{pos2, Align2, Color32, CursorIcon, Event, EventFilter, FontId, Key, Modifiers, PointerButton, Pos2, Rect, Sense, Ui};

use bigtext::text::rows::{self, CellKind, Pending, Row};
use bigtext::text::TextBuf;

use crate::app::document::Document;

const SCROLLBAR: f32 = 14.0;
/// Selections up to this size are copied synchronously; larger ones go through a worker.
pub const SYNC_COPY_MAX: u64 = 4 * 1024 * 1024;

pub enum ViewAction {
    /// Copy (or cut) a selection too large for the UI thread.
    CopyLarge { cut: bool, len: u64 },
}

pub struct ViewEnv<'a> {
    pub font_size: &'a mut f32,
    /// Returns matches to highlight within the given (visible) range.
    pub highlight: &'a dyn Fn(&TextBuf, Range<u64>) -> Vec<(u64, u64)>,
    pub actions: &'a mut Vec<ViewAction>,
    pub focus: bool,
    /// Syntax highlighting for the visible rows.
    pub lang: super::highlight::Lang,
    /// Which split pane this is (0 = primary).
    pub pane: u8,
}

#[derive(Clone, Copy)]
struct Metrics {
    cw: f32,
    rh: f32,
}

fn layout_rows(buf: &TextBuf, top: u64, n: usize, wrap: Option<usize>) -> Result<Vec<Row>, Pending> {
    let mut v = Vec::with_capacity(n + 1);
    v.push(rows::row_at(buf, top, wrap)?);
    while v.len() < n {
        match rows::next_row(buf, v.last().unwrap(), wrap)? {
            Some(r) => v.push(r),
            None => break,
        }
    }
    Ok(v)
}

fn digits(n: u64) -> usize {
    n.max(1).ilog10() as usize + 1
}

pub fn show(ui: &mut Ui, doc: &mut Document, env: &mut ViewEnv) {
    let font = FontId::monospace(*env.font_size);
    let (cw, rh) = ui.ctx().fonts_mut(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
    let m = Metrics { cw: cw.max(1.0), rh: rh.max(1.0) };

    let full = ui.available_rect_before_wrap();
    let id = ui.id().with(("textview", doc.id, env.pane));
    let resp = ui.interact(full, id, Sense::click_and_drag());
    if env.focus && (resp.clicked() || resp.drag_started() || resp.is_pointer_button_down_on() || (env.pane == doc.active_pane && ui.memory(|mem| mem.focused().is_none()))) {
        resp.request_focus();
    }
    if resp.is_pointer_button_down_on() || resp.has_focus() {
        doc.active_pane = env.pane;
    }
    let active_pane = doc.active_pane == env.pane;
    ui.memory_mut(|mem| {
        mem.set_focus_lock_filter(id, EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: false })
    });
    let focused = resp.has_focus();
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::Text);
    }

    let (line_est, _) = doc.buf.line_count();
    let gutter_w = digits(line_est).max(3) as f32 * m.cw + 24.0;
    let show_h = !doc.view.wrap;
    let text_rect = Rect::from_min_max(
        pos2(full.left() + gutter_w + 6.0, full.top()),
        pos2(full.right() - SCROLLBAR, full.bottom() - if show_h { SCROLLBAR } else { 0.0 }),
    );
    let gutter_rect = Rect::from_min_max(full.min, pos2(full.left() + gutter_w, text_rect.bottom()));
    let vbar = Rect::from_min_max(pos2(text_rect.right(), full.top()), pos2(full.right(), text_rect.bottom()));
    let hbar = Rect::from_min_max(pos2(text_rect.left(), text_rect.bottom()), pos2(text_rect.right(), full.bottom()));
    doc.view.text_rect = Some(text_rect);
    doc.view.cell = egui::vec2(m.cw, m.rh);
    doc.view.rows_on_screen = ((text_rect.height() / m.rh).floor() as usize).max(1);
    doc.view.text_cols = ((text_rect.width() / m.cw).floor() as usize).max(4);
    let wrap = doc.view.wrap.then_some(doc.view.text_cols.saturating_sub(1).max(1));
    if doc.view.width_cache_version != doc.version {
        doc.view.width_cache.clear();
        doc.view.width_cache_version = doc.version;
    }

    let mut pending = false;
    if doc.is_ready() {
        // Input is handled against the current layout, then the layout is rebuilt for painting.
        if let Ok(rows) = layout_rows(&doc.buf, doc.view.top, doc.view.rows_on_screen + 1, wrap) {
            handle_wheel(ui, doc, &resp, m, wrap, env);
            handle_mouse(ui, doc, &resp, text_rect, m, wrap, &rows, active_pane);
            if focused {
                handle_keys(ui, doc, wrap, env);
                ime_output(ui, text_rect);
            }
        } else {
            pending = true;
        }
        if active_pane && (doc.view.scroll_to_caret || doc.view.center_caret) {
            let center = doc.view.center_caret;
            if ensure_caret_visible(doc, wrap, center, m, text_rect.width()).is_ok() {
                doc.view.scroll_to_caret = false;
                doc.view.center_caret = false;
            } else {
                pending = true;
            }
        }
    }

    let painter = ui.painter_at(full);
    let visuals = ui.visuals().clone();
    painter.rect_filled(full, 0.0, visuals.extreme_bg_color);
    painter.rect_filled(gutter_rect, 0.0, visuals.faint_bg_color);

    let rows = if doc.is_ready() {
        match layout_rows(&doc.buf, doc.view.top, doc.view.rows_on_screen + 1, wrap) {
            Ok(r) => r,
            Err(Pending) => {
                pending = true;
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    if let Some(first) = rows.first() {
        doc.view.top = first.start;
    }

    let text_color = visuals.text_color();
    let weak = visuals.weak_text_color();
    let sel_color = visuals.selection.bg_fill;
    let hl_color = if visuals.dark_mode { Color32::from_rgba_unmultiplied(200, 160, 0, 90) } else { Color32::from_rgba_unmultiplied(255, 200, 0, 120) };
    let cur_line_bg = if visuals.dark_mode { Color32::from_white_alpha(8) } else { Color32::from_black_alpha(10) };

    let first_cell = (doc.view.scroll_x / m.cw).floor().max(0.0) as u32;
    let vis_cells = doc.view.text_cols as u32 + 2;
    let x_of = |cell: u32| text_rect.left() + cell as f32 * m.cw - doc.view.scroll_x;
    let text_painter = painter.with_clip_rect(text_rect);
    let (sel_s, sel_e) = doc.selection();
    let caret = doc.view.caret;
    let highlights = match (rows.first(), rows.last()) {
        (Some(a), Some(b)) => (env.highlight)(&doc.buf, a.start..b.seg.next.max(b.end)),
        _ => Vec::new(),
    };

    let (mut line, line_exact) = rows.first().map(|r| doc.buf.line_of(r.start)).unwrap_or((0, true));
    let mut max_w = doc.view.max_cells;
    for (i, row) in rows.iter().enumerate() {
        let y = text_rect.top() + i as f32 * m.rh;
        if y > text_rect.bottom() {
            break;
        }
        let starts_line = row.is_first_in_seg() && row.seg.line_start;
        if i > 0 && starts_line {
            line += 1;
        }
        let Some(bytes) = doc.buf.read(row.start..row.end) else {
            pending = true;
            break;
        };

        // Visible slice of the row.
        let codec = doc.buf.codec;
        let mut job = LayoutJob::default();
        let mut vis: Vec<(u64, u32)> = Vec::new();
        let mut cur = first_cell;
        let mut reached_end = true;
        let mut end_cell = 0u32;
        // Visible text of the row plus the byte ranges of placeholder (special) glyphs.
        let mut run = String::new();
        let mut specials: Vec<(usize, usize)> = Vec::new();
        rows::for_each_cell(&codec, &bytes, row.start, |c| {
            end_cell = c.cell + c.width;
            if c.cell + c.width <= first_cell {
                return true;
            }
            if c.cell >= first_cell + vis_cells {
                reached_end = false;
                return false;
            }
            vis.push((c.off, c.cell));
            if c.cell > cur {
                for _ in cur..c.cell {
                    run.push(' ');
                }
            }
            let (s, sp): (String, bool) = match c.kind {
                CellKind::Tab => (" ".repeat((c.cell + c.width - c.cell.max(first_cell)) as usize), false),
                CellKind::Hidden => (String::new(), false),
                CellKind::Control => (control_glyph(c.ch).to_string(), true),
                CellKind::Invalid => ('\u{FFFD}'.to_string(), true),
                CellKind::Normal => (c.ch.to_string(), false),
            };
            if sp {
                specials.push((run.len(), run.len() + s.len()));
            }
            run.push_str(&s);
            cur = c.cell.max(first_cell) + c.width;
            true
        });
        build_job(&mut job, &run, &specials, env.lang, &font, text_color, weak, visuals.dark_mode);

        // Row width for the horizontal scrollbar (cached; rows are bounded so this is cheap).
        if show_h {
            let w = if reached_end {
                end_cell
            } else {
                *doc.view.width_cache.entry(row.start).or_insert_with(|| rows::cell_width(&codec, &bytes, row.start))
            };
            max_w = max_w.max(w);
        }

        let cell_for = |off: u64| -> u32 {
            if off <= row.start {
                return 0;
            }
            match vis.iter().find(|(o, _)| *o >= off) {
                Some(&(_, c)) => c,
                None => {
                    if reached_end {
                        end_cell
                    } else {
                        first_cell + vis_cells
                    }
                }
            }
        };
        let row_has_caret = caret >= row.start && (caret < row.end || (caret == row.end && (row.is_last_in_seg() || row.end == doc.buf.len())));
        if row_has_caret && focused && sel_s == sel_e {
            text_painter.rect_filled(Rect::from_min_max(pos2(text_rect.left(), y), pos2(text_rect.right(), y + m.rh)), 0.0, cur_line_bg);
        }
        for &(hs, he) in &highlights {
            if he > row.start && hs < row.end || (hs == he && hs >= row.start && hs < row.end) {
                let x0 = x_of(cell_for(hs.max(row.start)));
                let x1 = x_of(cell_for(he.min(row.end))).max(x0 + 2.0);
                text_painter.rect_filled(Rect::from_min_max(pos2(x0, y), pos2(x1, y + m.rh)), 2.0, hl_color);
            }
        }
        if sel_s < sel_e && sel_e > row.start && sel_s <= row.end {
            let x0 = x_of(cell_for(sel_s.max(row.start)));
            let mut x1 = x_of(cell_for(sel_e.min(row.end)));
            if sel_e > row.end && row.is_last_in_seg() && (row.seg.has_nl || !row.seg.line_start) {
                x1 += m.cw * 0.5;
            }
            if x1 > x0 {
                text_painter.rect_filled(Rect::from_min_max(pos2(x0, y), pos2(x1, y + m.rh)), 0.0, sel_color);
            }
        }
        if !job.text.is_empty() {
            let galley = ui.ctx().fonts_mut(|f| f.layout_job(job));
            text_painter.galley(pos2(x_of(first_cell), y), galley, text_color);
        }
        if row_has_caret {
            let x = x_of(cell_for(caret));
            let color = if focused { visuals.text_cursor.stroke.color } else { weak };
            text_painter.rect_filled(Rect::from_min_max(pos2(x - 0.5, y), pos2(x + 1.5, y + m.rh)), 0.0, color);
        }

        let label = if starts_line {
            if line_exact {
                format!("{}", line + 1)
            } else {
                format!("~{}", line + 1)
            }
        } else if row.seg.line_start {
            String::new()
        } else if row.is_first_in_seg() {
            "↪".to_string()
        } else {
            String::new()
        };
        if !label.is_empty() {
            painter.text(pos2(gutter_rect.right() - 8.0, y), Align2::RIGHT_TOP, label, font.clone(), weak);
        }
        if starts_line && doc.bookmarks.binary_search(&row.start).is_ok() {
            let r = Rect::from_min_size(pos2(gutter_rect.left() + 3.0, y + m.rh * 0.2), egui::vec2(5.0, m.rh * 0.6));
            painter.rect_filled(r, 2.0, visuals.selection.stroke.color);
        }
    }
    doc.view.max_cells = max_w;

    match &doc.state {
        crate::app::document::DocState::Loading => {
            painter.text(text_rect.center(), Align2::CENTER_CENTER, "Opening…", FontId::proportional(18.0), weak);
        }
        crate::app::document::DocState::Finalizing(msg) => {
            painter.text(text_rect.center(), Align2::CENTER_CENTER, msg, FontId::proportional(18.0), weak);
        }
        crate::app::document::DocState::Failed(e) => {
            painter.text(text_rect.center(), Align2::CENTER_CENTER, e, FontId::proportional(16.0), visuals.error_fg_color);
        }
        crate::app::document::DocState::Ready => {
            if pending {
                painter.text(text_rect.right_top() + egui::vec2(-8.0, 4.0), Align2::RIGHT_TOP, "Loading…", FontId::proportional(13.0), weak);
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(30));
            } else if doc.view.first_paint.is_none() && !rows.is_empty() {
                doc.view.first_paint = Some(std::time::Instant::now());
            }
        }
    }

    // Scrollbars.
    if doc.is_ready() {
        vertical_scrollbar(ui, doc, id, vbar, &rows, wrap);
        if show_h {
            horizontal_scrollbar(ui, doc, id, hbar, m, text_rect.width());
        }
    }
}

/// Turns the visible text of a row into layout sections: syntax colours, with placeholder
/// glyphs (controls, invalid bytes) always dimmed.
#[allow(clippy::too_many_arguments)]
fn build_job(job: &mut LayoutJob, text: &str, specials: &[(usize, usize)], lang: super::highlight::Lang, font: &FontId, normal: Color32, weak: Color32, dark: bool) {
    if text.is_empty() {
        return;
    }
    let mut colors = vec![normal; text.len()];
    let mut spans = Vec::new();
    super::highlight::spans(lang, text, &mut spans);
    for (a, b, k) in spans {
        let c = super::highlight::color(k, dark);
        for x in &mut colors[a.min(text.len())..b.min(text.len())] {
            *x = c;
        }
    }
    for &(a, b) in specials {
        for x in &mut colors[a..b] {
            *x = weak;
        }
    }
    let mut start = 0;
    let mut cur = colors[0];
    for (i, _) in text.char_indices().skip(1) {
        if colors[i] != cur {
            job.append(&text[start..i], 0.0, TextFormat { font_id: font.clone(), color: cur, ..Default::default() });
            start = i;
            cur = colors[i];
        }
    }
    job.append(&text[start..], 0.0, TextFormat { font_id: font.clone(), color: cur, ..Default::default() });
}

fn control_glyph(c: char) -> char {
    match c as u32 {
        0 => '∅',
        0x0D => '←',
        0x1B => '⎋',
        _ => '·',
    }
}

fn vertical_scrollbar(ui: &mut Ui, doc: &mut Document, id: egui::Id, bar: Rect, rows: &[Row], wrap: Option<usize>) {
    let len = doc.buf.len().max(1);
    let visible = rows.last().map(|r| r.seg.next.max(r.end)).unwrap_or(doc.view.top).saturating_sub(doc.view.top).max(1);
    let track_h = bar.height();
    let thumb_h = (track_h * (visible as f64 / len as f64) as f32).clamp(24.0_f32.min(track_h), track_h);
    let frac = doc.view.top as f64 / len as f64;
    let thumb_y = bar.top() + (track_h - thumb_h) * frac as f32;
    let thumb = Rect::from_min_max(pos2(bar.left() + 2.0, thumb_y), pos2(bar.right() - 2.0, thumb_y + thumb_h));
    let r = ui.interact(bar, id.with("vbar"), Sense::click_and_drag());
    let painter = ui.painter_at(bar);
    let v = ui.visuals();
    painter.rect_filled(bar, 0.0, v.faint_bg_color);
    let color = if r.dragged() || r.hovered() { v.widgets.hovered.bg_fill } else { v.widgets.inactive.bg_fill };
    painter.rect_filled(thumb, 4.0, color);
    if r.dragged() || r.is_pointer_button_down_on() {
        if let Some(p) = r.interact_pointer_pos() {
            let f = ((p.y - bar.top() - thumb_h / 2.0) / (track_h - thumb_h).max(1.0)).clamp(0.0, 1.0) as f64;
            let target = (f * len as f64) as u64;
            if let Ok(row) = rows::row_at(&doc.buf, target.min(doc.buf.len()), wrap) {
                doc.view.top = row.start;
            }
        }
    }
}

fn horizontal_scrollbar(ui: &mut Ui, doc: &mut Document, id: egui::Id, bar: Rect, m: Metrics, text_w: f32) {
    let content = doc.view.max_cells as f32 * m.cw + 4.0 * m.cw;
    let max_scroll = (content - text_w).max(0.0);
    doc.view.scroll_x = doc.view.scroll_x.clamp(0.0, max_scroll.max(doc.view.scroll_x.min(max_scroll + text_w)));
    let painter = ui.painter_at(bar);
    let v = ui.visuals();
    painter.rect_filled(bar, 0.0, v.faint_bg_color);
    if max_scroll <= 0.0 {
        return;
    }
    let track_w = bar.width();
    let thumb_w = (track_w * text_w / content).clamp(24.0_f32.min(track_w), track_w);
    let x = bar.left() + (track_w - thumb_w) * (doc.view.scroll_x / max_scroll).clamp(0.0, 1.0);
    let thumb = Rect::from_min_max(pos2(x, bar.top() + 2.0), pos2(x + thumb_w, bar.bottom() - 2.0));
    let r = ui.interact(bar, id.with("hbar"), Sense::click_and_drag());
    let color = if r.dragged() || r.hovered() { v.widgets.hovered.bg_fill } else { v.widgets.inactive.bg_fill };
    painter.rect_filled(thumb, 4.0, color);
    if r.dragged() || r.is_pointer_button_down_on() {
        if let Some(p) = r.interact_pointer_pos() {
            let f = ((p.x - bar.left() - thumb_w / 2.0) / (track_w - thumb_w).max(1.0)).clamp(0.0, 1.0);
            doc.view.scroll_x = f * max_scroll;
        }
    }
}

fn handle_wheel(ui: &mut Ui, doc: &mut Document, resp: &egui::Response, m: Metrics, wrap: Option<usize>, env: &mut ViewEnv) {
    if !resp.hovered() {
        return;
    }
    let (delta, zoom) = ui.input(|i| (i.smooth_scroll_delta(), i.zoom_delta()));
    if zoom != 1.0 {
        *env.font_size = (*env.font_size * zoom).clamp(6.0, 72.0);
        return;
    }
    if delta.y != 0.0 {
        let rows_f = -delta.y / m.rh + doc.view.wheel_rem;
        let n = rows_f.trunc() as i64;
        doc.view.wheel_rem = rows_f - n as f32;
        if n != 0 {
            if let Ok(top) = rows::row_at(&doc.buf, doc.view.top, wrap) {
                if let Ok(r) = rows::step_rows(&doc.buf, top, n, wrap) {
                    doc.view.top = r.start;
                }
            }
        }
    }
    if delta.x != 0.0 && wrap.is_none() {
        doc.view.scroll_x = (doc.view.scroll_x - delta.x).max(0.0);
    }
}

fn hit_test(doc: &Document, rows: &[Row], text_rect: Rect, m: Metrics, pos: Pos2) -> Option<u64> {
    if rows.is_empty() {
        return None;
    }
    let ri = ((pos.y - text_rect.top()) / m.rh).floor();
    let row = if ri < 0.0 {
        rows[0]
    } else {
        let i = (ri as usize).min(rows.len() - 1);
        rows[i]
    };
    let col = ((pos.x - text_rect.left() + doc.view.scroll_x) / m.cw).round().max(0.0) as u32;
    rows::offset_at_cell(&doc.buf, &row, col).ok()
}

#[allow(clippy::too_many_arguments)]
fn handle_mouse(ui: &mut Ui, doc: &mut Document, resp: &egui::Response, text_rect: Rect, m: Metrics, wrap: Option<usize>, rows: &[Row], active_pane: bool) {
    let (pressed, down, mods, pos, dbl, tpl) = ui.input(|i| {
        (
            i.pointer.primary_pressed(),
            i.pointer.primary_down(),
            i.modifiers,
            i.pointer.interact_pos(),
            i.pointer.button_double_clicked(PointerButton::Primary),
            i.pointer.button_triple_clicked(PointerButton::Primary),
        )
    });
    let Some(pos) = pos else { return };
    if pressed && text_rect.contains(pos) && resp.hovered() {
        if let Some(off) = hit_test(doc, rows, text_rect, m, pos) {
            doc.set_caret(off, mods.shift);
            doc.view.desired_cell = None;
            doc.view.dragging = true;
            doc.view.scroll_to_caret = false;
        }
    }
    if (dbl || tpl) && text_rect.contains(pos) {
        if let Some(off) = hit_test(doc, rows, text_rect, m, pos) {
            if tpl {
                doc.select_line(off);
            } else {
                select_word(doc, off, wrap);
            }
            doc.view.dragging = false;
        }
    }
    if doc.view.dragging && active_pane {
        if !down {
            doc.view.dragging = false;
        } else {
            if pos.y < text_rect.top() || pos.y > text_rect.bottom() {
                let n = if pos.y < text_rect.top() { -1 } else { 1 };
                if let Ok(top) = rows::row_at(&doc.buf, doc.view.top, wrap) {
                    if let Ok(r) = rows::step_rows(&doc.buf, top, n, wrap) {
                        doc.view.top = r.start;
                    }
                }
                ui.ctx().request_repaint();
            }
            let clamped = pos2(pos.x.max(text_rect.left()), pos.y.clamp(text_rect.top(), text_rect.bottom() - 1.0));
            if let Some(off) = hit_test(doc, rows, text_rect, m, clamped) {
                doc.view.caret = off.clamp(doc.min_caret(), doc.buf.len());
            }
        }
    }
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn select_word(doc: &mut Document, off: u64, wrap: Option<usize>) {
    let Ok(row) = rows::row_at(&doc.buf, off, wrap) else { return };
    let lo = off.saturating_sub(1024).max(row.seg.start);
    let hi = (off + 1024).min(row.seg.content_end);
    let Some(bytes) = doc.buf.read(lo..hi) else { return };
    let codec = doc.buf.codec;
    let mut chars: Vec<(u64, u64, char)> = Vec::new();
    rows::for_each_cell(&codec, &bytes, lo, |c| {
        chars.push((c.off, c.off + c.len as u64, c.ch));
        true
    });
    let Some(i) = chars.iter().position(|&(s, e, _)| off >= s && off < e).or_else(|| chars.len().checked_sub(1)) else { return };
    let word = is_word_char(chars[i].2);
    let mut a = i;
    while a > 0 && is_word_char(chars[a - 1].2) == word && word {
        a -= 1;
    }
    let mut b = i;
    while b + 1 < chars.len() && is_word_char(chars[b + 1].2) == word && word {
        b += 1;
    }
    doc.select(chars[a].0, chars[b].1);
    doc.view.scroll_to_caret = false;
}

fn ime_output(ui: &mut Ui, text_rect: Rect) {
    ui.ctx().output_mut(|o| {
        o.ime = Some(egui::output::IMEOutput {
            purpose: egui::IMEPurpose::Normal,
            rect: text_rect,
            cursor_rect: Rect::from_min_size(text_rect.min, egui::vec2(1.0, 16.0)),
            should_interrupt_composition: false,
        });
    });
}

fn handle_keys(ui: &mut Ui, doc: &mut Document, wrap: Option<usize>, env: &mut ViewEnv) {
    let events = ui.input(|i| i.events.clone());
    for ev in events {
        match ev {
            Event::Text(t) => {
                if doc.editable() && !t.chars().all(|c| c.is_control()) {
                    doc.insert_text(&t);
                }
            }
            Event::Ime(egui::ImeEvent::Commit(t)) => {
                if doc.editable() && !t.is_empty() {
                    doc.insert_text(&t);
                }
            }
            Event::Copy => copy(ui, doc, env, false),
            Event::Cut => {
                if doc.editable() {
                    copy(ui, doc, env, true)
                }
            }
            Event::Paste(s) => {
                if doc.editable() {
                    paste(doc, &s);
                }
            }
            Event::Key { key, pressed: true, modifiers, .. } => handle_key(doc, key, modifiers, wrap),
            _ => {}
        }
    }
}

fn copy(ui: &mut Ui, doc: &mut Document, env: &mut ViewEnv, cut: bool) {
    match doc.selection_text(SYNC_COPY_MAX) {
        Some(Ok(text)) => {
            ui.ctx().copy_text(text);
            if cut {
                doc.delete_selection();
            }
        }
        Some(Err(len)) => env.actions.push(ViewAction::CopyLarge { cut, len }),
        None => {}
    }
}

/// Inserts clipboard text, converting its line breaks to the document's style.
pub fn paste(doc: &mut Document, s: &str) {
    use bigtext::encoding::LineEnding;
    let normalized: std::borrow::Cow<str> = match doc.line_ending {
        LineEnding::CrLf => {
            let lf = s.replace("\r\n", "\n");
            std::borrow::Cow::Owned(lf.replace('\n', "\r\n"))
        }
        LineEnding::Lf => std::borrow::Cow::Owned(s.replace("\r\n", "\n")),
        LineEnding::Cr => std::borrow::Cow::Owned(s.replace("\r\n", "\r").replace('\n', "\r")),
        _ => std::borrow::Cow::Borrowed(s),
    };
    let (a, b) = doc.selection();
    let bytes = doc.buf.codec.encode(&normalized);
    doc.edit(a..b, &bytes, crate::app::document::EditKind::Other);
}

fn move_horizontal(doc: &Document, wrap: Option<usize>, right: bool, word: bool) -> u64 {
    let c = doc.view.caret;
    let Ok(row) = rows::row_at(&doc.buf, c, wrap) else { return c };
    if right {
        if c >= row.seg.content_end && row.is_last_in_seg() {
            return row.seg.next.min(doc.buf.len()).max(c);
        }
        if word {
            return word_boundary(doc, c, row.seg.content_end, true);
        }
        doc.buf.next_char(c)
    } else {
        if c == row.seg.start && row.seg.line_start && c > 0 {
            if let Ok(Some(prev)) = rows::prev_seg(&doc.buf, &row.seg) {
                return prev.content_end.max(doc.min_caret());
            }
        }
        if word {
            return word_boundary(doc, c, row.seg.start, false);
        }
        doc.buf.prev_char(c).max(doc.min_caret())
    }
}

fn word_boundary(doc: &Document, c: u64, limit: u64, right: bool) -> u64 {
    let (lo, hi) = if right { (c, limit.min(c + 4096)) } else { (limit.max(c.saturating_sub(4096)), c) };
    let Some(bytes) = doc.buf.read(lo..hi) else { return c };
    let mut chars: Vec<(u64, char)> = Vec::new();
    rows::for_each_cell(&doc.buf.codec, &bytes, lo, |x| {
        chars.push((x.off, x.ch));
        true
    });
    if right {
        let mut i = 0;
        while i < chars.len() && !is_word_char(chars[i].1) {
            i += 1;
        }
        while i < chars.len() && is_word_char(chars[i].1) {
            i += 1;
        }
        chars.get(i).map(|x| x.0).unwrap_or(hi)
    } else {
        let mut i = chars.len();
        while i > 0 && !is_word_char(chars[i - 1].1) {
            i -= 1;
        }
        while i > 0 && is_word_char(chars[i - 1].1) {
            i -= 1;
        }
        chars.get(i).map(|x| x.0).unwrap_or(lo).max(doc.min_caret())
    }
}

fn move_vertical(doc: &mut Document, n: i64, extend: bool, wrap: Option<usize>) {
    let c = doc.view.caret;
    let Ok(row) = rows::row_at(&doc.buf, c, wrap) else { return };
    let cell = match doc.view.desired_cell {
        Some(x) => x,
        None => rows::cell_of_offset(&doc.buf, &row, c).unwrap_or(0),
    };
    let Ok(target) = rows::step_rows(&doc.buf, row, n, wrap) else { return };
    let off = if target.start == row.start {
        if n < 0 {
            row.start
        } else {
            row.end
        }
    } else {
        rows::offset_at_cell(&doc.buf, &target, cell).unwrap_or(target.start)
    };
    doc.set_caret(off, extend);
    doc.view.desired_cell = Some(cell);
    if n.unsigned_abs() > 1 {
        // Page movement scrolls the view by the same amount.
        if let Ok(top) = rows::row_at(&doc.buf, doc.view.top, wrap) {
            if let Ok(t) = rows::step_rows(&doc.buf, top, n, wrap) {
                doc.view.top = t.start;
            }
        }
    }
}

fn smart_home(doc: &Document, wrap: Option<usize>) -> u64 {
    let c = doc.view.caret;
    let Ok(row) = rows::row_at(&doc.buf, c, wrap) else { return c };
    let mut first = row.start;
    if let Some(bytes) = doc.buf.read(row.start..row.end.min(row.start + 4096)) {
        rows::for_each_cell(&doc.buf.codec, &bytes, row.start, |x| {
            if x.ch == ' ' || x.ch == '\t' {
                first = x.off + x.len as u64;
                true
            } else {
                false
            }
        });
    }
    let first = first.max(doc.min_caret());
    if c == first {
        row.start.max(doc.min_caret())
    } else {
        first
    }
}

fn handle_key(doc: &mut Document, key: Key, m: Modifiers, wrap: Option<usize>) {
    let shift = m.shift;
    let cmd = m.command;
    let page = doc.view.rows_on_screen.saturating_sub(1).max(1) as i64;
    match key {
        Key::ArrowLeft | Key::ArrowRight => {
            let right = key == Key::ArrowRight;
            if !shift && doc.has_selection() && !cmd {
                let (s, e) = doc.selection();
                doc.set_caret(if right { e } else { s }, false);
            } else {
                let c = move_horizontal(doc, wrap, right, cmd);
                doc.set_caret(c, shift);
            }
            doc.view.desired_cell = None;
        }
        Key::ArrowUp => move_vertical(doc, -1, shift, wrap),
        Key::ArrowDown => move_vertical(doc, 1, shift, wrap),
        Key::PageUp => move_vertical(doc, -page, shift, wrap),
        Key::PageDown => move_vertical(doc, page, shift, wrap),
        Key::Home => {
            let c = if cmd { doc.min_caret() } else { smart_home(doc, wrap) };
            doc.set_caret(c, shift);
            doc.view.desired_cell = None;
        }
        Key::End => {
            let c = if cmd {
                doc.buf.len()
            } else {
                rows::row_at(&doc.buf, doc.view.caret, wrap).map(|r| r.end).unwrap_or(doc.view.caret)
            };
            doc.set_caret(c, shift);
            doc.view.desired_cell = None;
        }
        Key::Backspace if doc.editable() => {
            if cmd && !doc.has_selection() {
                let c = move_horizontal(doc, wrap, false, true);
                doc.view.anchor = c;
                doc.delete_selection();
            } else {
                doc.backspace();
            }
        }
        Key::Delete if doc.editable() => {
            if cmd && !doc.has_selection() {
                let c = move_horizontal(doc, wrap, true, true);
                doc.view.anchor = c;
                doc.delete_selection();
            } else {
                doc.delete_forward();
            }
        }
        Key::Enter if doc.editable() => doc.newline(),
        Key::Tab if doc.editable() && !shift && !cmd => doc.insert_text("\t"),
        Key::A if cmd => {
            let len = doc.buf.len();
            doc.view.anchor = doc.min_caret();
            doc.view.caret = len;
        }
        Key::Z if cmd => {
            if shift {
                doc.redo()
            } else {
                doc.undo()
            }
        }
        Key::Y if cmd => doc.redo(),
        Key::D if cmd => doc.duplicate_line(),
        Key::K if cmd && shift => doc.delete_line(),
        Key::Escape => {
            let c = doc.view.caret;
            doc.view.anchor = c;
        }
        _ => {}
    }
}

fn ensure_caret_visible(doc: &mut Document, wrap: Option<usize>, center: bool, m: Metrics, text_w: f32) -> Result<(), Pending> {
    let n = doc.view.rows_on_screen.max(1);
    let buf = &doc.buf;
    let crow = rows::row_at(buf, doc.view.caret, wrap)?;
    let top_row = rows::row_at(buf, doc.view.top, wrap)?;
    if center {
        doc.view.top = rows::step_rows(buf, crow, -((n / 2) as i64), wrap)?.start;
    } else if crow.start < top_row.start {
        doc.view.top = crow.start;
    } else {
        let mut r = top_row;
        let mut visible = r.start == crow.start;
        for _ in 1..n {
            if visible {
                break;
            }
            match rows::next_row(buf, &r, wrap)? {
                Some(x) => {
                    r = x;
                    visible = r.start == crow.start;
                }
                None => break,
            }
        }
        if !visible {
            doc.view.top = rows::step_rows(buf, crow, -((n as i64) - 1), wrap)?.start;
        }
    }
    if wrap.is_none() {
        let x = rows::cell_of_offset(buf, &crow, doc.view.caret)? as f32 * m.cw;
        if x < doc.view.scroll_x {
            doc.view.scroll_x = (x - 4.0 * m.cw).max(0.0);
        } else if x > doc.view.scroll_x + text_w - 2.0 * m.cw {
            doc.view.scroll_x = x - text_w + 6.0 * m.cw;
        }
    }
    Ok(())
}
