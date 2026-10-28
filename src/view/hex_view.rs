//! Read-only hex view: 16 bytes per row, trivially virtualized by row index.

use egui::{pos2, Align2, Color32, CursorIcon, Event, EventFilter, FontId, Key, Rect, Sense, Ui};

use crate::app::document::Document;

const PER_ROW: u64 = 16;
const SCROLLBAR: f32 = 14.0;

pub fn show(ui: &mut Ui, doc: &mut Document, font_size: f32, take_focus: bool) {
    let font = FontId::monospace(font_size);
    let (cw, rh) = ui.ctx().fonts_mut(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
    let full = ui.available_rect_before_wrap();
    let id = ui.id().with(("hexview", doc.id));
    let resp = ui.interact(full, id, Sense::click_and_drag());
    if take_focus && (resp.clicked() || ui.memory(|m| m.focused().is_none())) {
        resp.request_focus();
    }
    ui.memory_mut(|m| m.set_focus_lock_filter(id, EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: false }));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::Text);
    }
    let len = doc.buf.len();
    let total_rows = len.div_ceil(PER_ROW).max(1);
    let text = Rect::from_min_max(full.min, pos2(full.right() - SCROLLBAR, full.bottom()));
    let bar = Rect::from_min_max(pos2(text.right(), full.top()), full.max);
    let n = ((text.height() / rh).floor() as u64).max(1);
    let off_digits = if len > 0xFF_FFFF_FFFF { 16 } else { 10 };
    let hex_x = text.left() + 6.0 + (off_digits as f32 + 2.0) * cw;
    let ascii_x = hex_x + (PER_ROW as f32 * 3.0 + 2.0) * cw;

    // Input.
    let mut caret = doc.view.caret.min(len.saturating_sub(1));
    let mut moved = false;
    if resp.hovered() {
        let d = ui.input(|i| i.smooth_scroll_delta()).y;
        if d != 0.0 {
            let rows = (-d / rh).round() as i64;
            doc.view.hex_top = (doc.view.hex_top as i64 + rows).clamp(0, total_rows.saturating_sub(1) as i64) as u64;
        }
    }
    if resp.has_focus() {
        for ev in ui.input(|i| i.events.clone()) {
            if let Event::Key { key, pressed: true, modifiers, .. } = ev {
                let delta: i64 = match key {
                    Key::ArrowLeft => -1,
                    Key::ArrowRight => 1,
                    Key::ArrowUp => -(PER_ROW as i64),
                    Key::ArrowDown => PER_ROW as i64,
                    Key::PageUp => -((n * PER_ROW) as i64),
                    Key::PageDown => (n * PER_ROW) as i64,
                    Key::Home if modifiers.command => i64::MIN / 2,
                    Key::End if modifiers.command => i64::MAX / 2,
                    _ => 0,
                };
                if delta != 0 {
                    caret = (caret as i64).saturating_add(delta).clamp(0, len.saturating_sub(1) as i64) as u64;
                    moved = true;
                }
            }
        }
    }
    if (resp.clicked() || resp.dragged()) && len > 0 {
        if let Some(p) = resp.interact_pointer_pos() {
            let row = doc.view.hex_top + ((p.y - text.top()) / rh).max(0.0) as u64;
            let col = if p.x >= ascii_x {
                ((p.x - ascii_x) / cw) as u64
            } else {
                ((p.x - hex_x) / (3.0 * cw)).max(0.0) as u64
            };
            caret = (row * PER_ROW + col.min(PER_ROW - 1)).min(len - 1);
            doc.view.anchor = caret;
        }
    }
    if moved || doc.view.scroll_to_caret || doc.view.center_caret {
        let crow = caret / PER_ROW;
        if doc.view.center_caret {
            doc.view.hex_top = crow.saturating_sub(n / 2);
        } else if crow < doc.view.hex_top {
            doc.view.hex_top = crow;
        } else if crow >= doc.view.hex_top + n {
            doc.view.hex_top = crow + 1 - n;
        }
        doc.view.scroll_to_caret = false;
        doc.view.center_caret = false;
    }
    doc.view.caret = caret;
    if moved {
        doc.view.anchor = caret;
    }

    // Paint.
    let painter = ui.painter_at(full);
    let v = ui.visuals().clone();
    painter.rect_filled(full, 0.0, v.extreme_bg_color);
    let start = doc.view.hex_top * PER_ROW;
    let end = (start + n * PER_ROW).min(len);
    let Some(bytes) = doc.buf.read(start..end) else {
        painter.text(text.center(), Align2::CENTER_CENTER, crate::app::i18n::tr("Loading…"), FontId::proportional(16.0), v.weak_text_color());
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(30));
        return;
    };
    let caret_bg = v.selection.bg_fill;
    for (i, chunk) in bytes.chunks(PER_ROW as usize).enumerate() {
        let y = text.top() + i as f32 * rh;
        let row_off = start + i as u64 * PER_ROW;
        painter.text(pos2(text.left() + 6.0, y), Align2::LEFT_TOP, format!("{row_off:0w$X}", w = off_digits), font.clone(), v.weak_text_color());
        let mut hex = String::with_capacity(PER_ROW as usize * 3);
        let mut ascii = String::with_capacity(PER_ROW as usize);
        for (j, &b) in chunk.iter().enumerate() {
            if row_off + j as u64 == caret {
                let x = hex_x + j as f32 * 3.0 * cw;
                painter.rect_filled(Rect::from_min_max(pos2(x - 1.0, y), pos2(x + 2.0 * cw + 1.0, y + rh)), 2.0, caret_bg);
                let ax = ascii_x + j as f32 * cw;
                painter.rect_filled(Rect::from_min_max(pos2(ax, y), pos2(ax + cw, y + rh)), 2.0, caret_bg);
            }
            hex.push_str(&format!("{b:02X} "));
            ascii.push(if (0x20..0x7F).contains(&b) { b as char } else { '.' });
        }
        painter.text(pos2(hex_x, y), Align2::LEFT_TOP, hex, font.clone(), v.text_color());
        painter.text(pos2(ascii_x, y), Align2::LEFT_TOP, ascii, font.clone(), v.text_color());
    }

    // Scrollbar.
    painter.rect_filled(bar, 0.0, v.faint_bg_color);
    let track = bar.height();
    let thumb_h = (track * (n as f32 / total_rows as f32)).clamp(24.0_f32.min(track), track);
    let frac = doc.view.hex_top as f64 / total_rows.max(1) as f64;
    let ty = bar.top() + (track - thumb_h) * frac as f32;
    let r = ui.interact(bar, id.with("bar"), Sense::click_and_drag());
    painter.rect_filled(Rect::from_min_max(pos2(bar.left() + 2.0, ty), pos2(bar.right() - 2.0, ty + thumb_h)), 4.0, if r.hovered() { v.widgets.hovered.bg_fill } else { v.widgets.inactive.bg_fill });
    if r.dragged() || r.is_pointer_button_down_on() {
        if let Some(p) = r.interact_pointer_pos() {
            let f = ((p.y - bar.top() - thumb_h / 2.0) / (track - thumb_h).max(1.0)).clamp(0.0, 1.0) as f64;
            doc.view.hex_top = (f * total_rows.saturating_sub(1) as f64) as u64;
        }
    }
    let _ = Color32::TRANSPARENT;
}
