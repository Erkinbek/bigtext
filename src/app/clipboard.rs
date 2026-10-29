//! Edit → Paste from history… (Ctrl+Shift+V): the last texts copied in BigText, to copy and
//! paste again. Texts over 1 MB are not kept; they are kept across restarts only when asked.

use std::collections::VecDeque;

use super::i18n::{tr, trf};
use super::BigTextApp;

/// Texts kept.
pub const CLIPS_MAX: usize = 30;
/// Longer copies are not kept.
pub const CLIP_BYTES_MAX: usize = 1 << 20;

#[derive(Default)]
pub struct ClipHistory {
    /// Newest first.
    pub items: VecDeque<String>,
    /// Kept in the settings file between sessions.
    pub keep: bool,
    /// The window is open, with this entry chosen.
    pub open: Option<usize>,
    /// Enter or Esc pressed this frame (taken before the text view sees them).
    keys: Option<bool>,
    moved: bool,
}

impl ClipHistory {
    pub fn record(&mut self, text: String) {
        if text.is_empty() || text.len() > CLIP_BYTES_MAX || self.items.front() == Some(&text) {
            return;
        }
        self.items.retain(|t| *t != text);
        self.items.push_front(text);
        self.items.truncate(CLIPS_MAX);
    }
}

/// An entry as it is listed: its first line, shortened, and its size.
fn label(text: &str) -> String {
    let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    let mut short: String = first.chars().take(80).collect();
    if short.len() < first.len() {
        short.push('…');
    }
    let lines = text.lines().count();
    if lines > 1 {
        trf("{text}  ({n} lines)", &[("text", &short), ("n", &lines.to_string())])
    } else {
        short
    }
}

/// Escapes a clip into one line of the settings file.
pub fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\n', "\\n").replace('\r', "\\r")
}

pub fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some(c) => out.push(c),
            None => out.push('\\'),
        }
    }
    out
}

impl BigTextApp {
    /// Keeps what this frame copied (every copy goes through egui's output).
    pub(super) fn record_copies(&mut self, ctx: &egui::Context) {
        let copied: Vec<String> = ctx.output(|o| {
            o.commands
                .iter()
                .filter_map(|c| match c {
                    egui::OutputCommand::CopyText(s) => Some(s.clone()),
                    _ => None,
                })
                .collect()
        });
        for t in copied {
            self.clips.record(t);
        }
    }

    pub(crate) fn open_paste_history(&mut self) {
        if self.clips.items.is_empty() {
            self.notify(tr("Nothing copied yet.").into(), None);
            return;
        }
        self.clips.open = Some(0);
    }

    /// Copies entry `i` again and pastes it into the document when it can be edited.
    pub(crate) fn paste_clip(&mut self, i: usize) {
        let Some(text) = self.clips.items.get(i).cloned() else { return };
        self.clips.open = None;
        if let Some(ctx) = self.ctx_clone() {
            ctx.copy_text(text.clone());
        }
        match self.docs.get_mut(self.active) {
            Some(d) if d.editable() => crate::view::text_view::paste(d, &text),
            _ => self.notify(tr("The document is read-only: the text was copied to the clipboard.").into(), None),
        }
    }

    /// The window's keys, taken at the start of the frame so the document does not get them.
    pub(super) fn paste_history_keys(&mut self, ctx: &egui::Context) {
        let Some(sel) = self.clips.open else { return };
        let n = self.clips.items.len();
        let (up, down, enter, esc) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
            )
        });
        let sel = if up { sel.saturating_sub(1) } else if down { (sel + 1).min(n.saturating_sub(1)) } else { sel };
        self.clips.open = Some(sel);
        self.clips.moved = up || down;
        self.clips.keys = if enter { Some(true) } else if esc { Some(false) } else { None };
    }

    pub(super) fn paste_history_window(&mut self, ctx: &egui::Context) {
        let Some(sel) = self.clips.open else { return };
        let keys = self.clips.keys.take();
        let moved = std::mem::take(&mut self.clips.moved);
        let (enter, esc) = (keys == Some(true), keys == Some(false));
        let mut pick = enter.then_some(sel);
        let mut keep = self.clips.keep;
        egui::Window::new(tr("Paste from history")).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_TOP, [0.0, 80.0]).show(ctx, |ui| {
            ui.weak(tr("↑ ↓ to choose, Enter to paste, Esc to close"));
            ui.separator();
            egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                for (i, t) in self.clips.items.iter().enumerate() {
                    let r = ui.selectable_label(i == sel, egui::RichText::new(label(t)).monospace());
                    if i == sel && moved {
                        r.scroll_to_me(None);
                    }
                    if r.clicked() {
                        pick = Some(i);
                    }
                }
            });
            ui.separator();
            ui.checkbox(&mut keep, tr("Keep across restarts")).on_hover_text(tr("Writes the copied texts into the settings file; off, they are forgotten when BigText closes"));
        });
        if keep != self.clips.keep {
            self.clips.keep = keep;
            self.persist_state();
        }
        if let Some(i) = pick {
            self.paste_clip(i);
        } else if esc {
            self.clips.open = None;
        } else {
            self.clips.open = Some(sel);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_first_without_repeats_or_huge_texts() {
        let mut h = ClipHistory::default();
        for t in ["a", "b", "a", "", "c"] {
            h.record(t.to_string());
        }
        assert_eq!(h.items, ["c", "a", "b"]);
        h.record("x".repeat(CLIP_BYTES_MAX + 1));
        assert_eq!(h.items.len(), 3);
        for i in 0..40 {
            h.record(i.to_string());
        }
        assert_eq!((h.items.len(), h.items[0].as_str()), (CLIPS_MAX, "39"));
        let s = "line 1\r\nback\\slash\n";
        assert_eq!(unescape(&escape(s)), s);
        assert!(!escape(s).contains('\n'));
    }
}
