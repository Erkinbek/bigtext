//! Multiple cursors: Ctrl+D adds the next occurrence of the selection, Alt+click adds a cursor.
//! Typing, pasting, Backspace, Delete, Enter and Tab change the text at every cursor as one
//! undoable edit; Left/Right/Home/End move them all; Esc goes back to one cursor.

use egui::{Event, Key, Ui};

use bigtext::text::rows;

use crate::app::document::Document;

pub enum MultiEdit {
    Insert(String),
    /// One text per cursor, top to bottom (pasting as many lines as there are cursors).
    PerCursor(Vec<String>),
    Backspace,
    Delete,
}

/// Every selection as (start, end, primary), in text order, with overlapping ones merged.
pub fn selections(doc: &Document) -> Vec<(u64, u64, bool)> {
    let v = &doc.view;
    let mut all: Vec<(u64, u64, bool)> = std::iter::once((v.anchor, v.caret, true)).chain(v.extra.iter().map(|&(a, c)| (a, c, false))).map(|(a, c, p)| (a.min(c), a.max(c), p)).collect();
    all.sort_by_key(|x| (x.0, x.1));
    let mut out: Vec<(u64, u64, bool)> = Vec::with_capacity(all.len());
    for (s, e, p) in all {
        match out.last_mut() {
            Some(last) if s < last.1 || (s == last.0 && e == last.1) => {
                last.1 = last.1.max(e);
                last.2 |= p;
            }
            _ => out.push((s, e, p)),
        }
    }
    out
}

/// Puts the cursors back: the primary one into the view's caret, the others into `extra`.
fn set_cursors(doc: &mut Document, cursors: Vec<(u64, u64, bool)>) {
    let mut extra = Vec::with_capacity(cursors.len());
    let mut primary = None;
    for (a, c, p) in cursors {
        if p && primary.is_none() {
            primary = Some((a, c));
        } else {
            extra.push((a, c));
        }
    }
    let (a, c) = primary.or_else(|| extra.pop()).unwrap_or((doc.view.anchor, doc.view.caret));
    doc.view.anchor = a;
    doc.view.caret = c;
    doc.view.extra = extra;
    doc.view.extra_version = doc.version;
}

/// Adds a cursor (or selection), keeping the current one as an extra cursor.
pub fn add_cursor(doc: &mut Document, anchor: u64, caret: u64) {
    let len = doc.buf.len();
    let (anchor, caret) = (anchor.clamp(doc.min_caret(), len), caret.clamp(doc.min_caret(), len));
    if doc.view.extra.is_empty() {
        doc.view.extra_version = doc.version;
    }
    let old = (doc.view.anchor, doc.view.caret);
    if old != (anchor, caret) && !doc.view.extra.contains(&(anchor, caret)) {
        doc.view.extra.push(old);
    }
    doc.view.extra.retain(|x| *x != (anchor, caret));
    doc.view.anchor = anchor;
    doc.view.caret = caret;
}

/// Applies `e` at every cursor as one undoable edit. Returns false if nothing changed.
pub fn edit_all(doc: &mut Document, e: MultiEdit) -> bool {
    if !doc.editable() {
        return false;
    }
    let codec = doc.buf.codec;
    let (min, len) = (doc.min_caret(), doc.buf.len());
    let sels = selections(doc);
    let mut edits: Vec<(std::ops::Range<u64>, Vec<u8>)> = Vec::with_capacity(sels.len());
    let mut after: Vec<(u64, u64, bool)> = Vec::with_capacity(sels.len());
    let mut shift = 0i64;
    let mut prev_end = 0u64;
    for (i, &(s, end, primary)) in sels.iter().enumerate() {
        let (mut s, mut end) = (s, end);
        match &e {
            MultiEdit::Backspace if s == end && s > min => s = doc.buf.prev_char(s),
            MultiEdit::Delete if s == end && end < len => end = doc.buf.next_char(end),
            _ => {}
        }
        s = s.max(prev_end);
        end = end.max(s);
        let text = match &e {
            MultiEdit::Insert(t) => codec.encode(t),
            MultiEdit::PerCursor(v) => codec.encode(v.get(i).map(String::as_str).unwrap_or("")),
            _ => Vec::new(),
        };
        let pos = (s as i64 + shift) as u64 + text.len() as u64;
        after.push((pos, pos, primary));
        shift += text.len() as i64 - (end - s) as i64;
        prev_end = end;
        if s != end || !text.is_empty() {
            edits.push((s..end, text));
        }
    }
    if edits.is_empty() {
        return false;
    }
    let (pieces, add) = doc.buf.build_batch(&edits);
    let swap = doc.buf.apply_batch(pieces, &add);
    let p = after.iter().find(|x| x.2).map(|x| x.0).unwrap_or(0);
    doc.record(swap, (p, p));
    set_cursors(doc, after);
    true
}

/// Moves every cursor with `f(anchor, caret) -> (anchor, caret)`.
fn move_all(doc: &mut Document, f: impl Fn(&Document, u64, u64) -> (u64, u64)) {
    let v: Vec<(u64, u64, bool)> = std::iter::once((doc.view.anchor, doc.view.caret, true)).chain(doc.view.extra.iter().map(|&(a, c)| (a, c, false))).collect();
    let moved: Vec<(u64, u64, bool)> = v.into_iter().map(|(a, c, p)| {
        let (a, c) = f(doc, a, c);
        (a, c, p)
    }).collect();
    // Cursors that meet become one.
    let mut out: Vec<(u64, u64, bool)> = Vec::with_capacity(moved.len());
    for m in moved {
        match out.iter_mut().find(|x| x.0 == m.0 && x.1 == m.1) {
            Some(x) => x.2 |= m.2,
            None => out.push(m),
        }
    }
    set_cursors(doc, out);
    doc.view.scroll_to_caret = true;
}

/// The selected texts, top to bottom, one per line.
fn selected_text(doc: &Document) -> String {
    let eol = if doc.line_ending == bigtext::encoding::LineEnding::CrLf { "\r\n" } else { "\n" };
    selections(doc)
        .into_iter()
        .filter(|(s, e, _)| e > s && e - s <= 1 << 20)
        .filter_map(|(s, e, _)| doc.buf.read(s..e).map(|b| doc.buf.codec.decode_lossy(&b)))
        .collect::<Vec<_>>()
        .join(eol)
}

/// Handles an input event when there are several cursors; returns true if it was used.
pub fn handle_event(ui: &Ui, doc: &mut Document, ev: &Event, wrap: Option<usize>) -> bool {
    if doc.view.extra.is_empty() {
        return false;
    }
    match ev {
        Event::Text(t) if !t.chars().all(|c| c.is_control()) => {
            edit_all(doc, MultiEdit::Insert(t.clone()));
        }
        Event::Ime(egui::ImeEvent::Commit(t)) if !t.is_empty() => {
            edit_all(doc, MultiEdit::Insert(t.clone()));
        }
        Event::Paste(s) => {
            let lines: Vec<String> = s.lines().map(str::to_string).collect();
            let n = selections(doc).len();
            let e = if n > 1 && lines.len() == n { MultiEdit::PerCursor(lines) } else { MultiEdit::Insert(s.clone()) };
            edit_all(doc, e);
        }
        Event::Copy | Event::Cut => {
            ui.ctx().copy_text(selected_text(doc));
            if matches!(ev, Event::Cut) {
                edit_all(doc, MultiEdit::Insert(String::new()));
            }
        }
        Event::Key { key, pressed: true, modifiers: m, .. } => {
            let (shift, cmd) = (m.shift, m.command);
            match key {
                Key::Backspace if !cmd => {
                    edit_all(doc, MultiEdit::Backspace);
                }
                Key::Delete if !cmd => {
                    edit_all(doc, MultiEdit::Delete);
                }
                Key::Enter => {
                    let nl = doc.buf.codec.decode_lossy(&doc.buf.codec.line_break(doc.line_ending));
                    edit_all(doc, MultiEdit::Insert(nl));
                }
                Key::Tab if !shift && !cmd => {
                    edit_all(doc, MultiEdit::Insert("\t".into()));
                }
                Key::ArrowLeft | Key::ArrowRight if !cmd => {
                    let right = *key == Key::ArrowRight;
                    move_all(doc, |d, a, c| {
                        if !shift && a != c {
                            let p = if right { a.max(c) } else { a.min(c) };
                            return (p, p);
                        }
                        let n = if right { d.buf.next_char(c) } else { d.buf.prev_char(c).max(d.min_caret()) };
                        (if shift { a } else { n }, n)
                    });
                }
                Key::Home | Key::End if !cmd => {
                    let end = *key == Key::End;
                    move_all(doc, |d, a, c| {
                        let n = match rows::row_at(&d.buf, c, wrap) {
                            Ok(r) if end => r.end,
                            Ok(r) => r.start.max(d.min_caret()),
                            Err(_) => c,
                        };
                        (if shift { a } else { n }, n)
                    });
                }
                Key::Escape => {
                    doc.view.extra.clear();
                }
                // Everything else works on the main cursor only.
                _ => {
                    doc.view.extra.clear();
                    return false;
                }
            }
        }
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(text: &str) -> (tempfile::TempDir, Document) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.txt");
        std::fs::write(&path, text).unwrap();
        let src = std::sync::Arc::new(bigtext::file::Source::open(&path, bigtext::file::OpenMode::Auto).unwrap());
        let det = bigtext::encoding::detect(&src.read(0..src.len()).unwrap(), true);
        let mut d = Document::loading(1, path);
        d.install(src, det, None, bigtext::jobs::noop_waker());
        (dir, d)
    }

    fn text(d: &Document) -> String {
        String::from_utf8(d.buf.read(0..d.buf.len()).unwrap().to_vec()).unwrap()
    }

    #[test]
    fn typing_and_deleting_at_every_cursor() {
        let (_t, mut d) = doc("foo x foo y foo\n");
        // Three cursors after each "foo".
        d.select(3, 3);
        add_cursor(&mut d, 9, 9);
        add_cursor(&mut d, 15, 15);
        assert!(edit_all(&mut d, MultiEdit::Insert("!".into())));
        assert_eq!(text(&d), "foo! x foo! y foo!\n");
        assert_eq!(selections(&d).iter().map(|x| x.0).collect::<Vec<_>>(), vec![4, 11, 18]);
        assert!(edit_all(&mut d, MultiEdit::Backspace));
        assert!(edit_all(&mut d, MultiEdit::Backspace));
        assert_eq!(text(&d), "fo x fo y fo\n");
        // Each change at all cursors is one undo step.
        d.undo();
        d.undo();
        assert_eq!(text(&d), "foo! x foo! y foo!\n");
        d.undo();
        assert_eq!(text(&d), "foo x foo y foo\n");
    }

    #[test]
    fn selections_are_replaced_and_overlaps_merge() {
        let (_t, mut d) = doc("aa bb aa bb\n");
        d.select(0, 2);
        add_cursor(&mut d, 6, 8);
        add_cursor(&mut d, 6, 8);
        assert_eq!(selections(&d).len(), 2);
        assert!(edit_all(&mut d, MultiEdit::PerCursor(vec!["1".into(), "2".into()])));
        assert_eq!(text(&d), "1 bb 2 bb\n");
        assert!(edit_all(&mut d, MultiEdit::Delete));
        assert_eq!(text(&d), "1bb 2bb\n");
    }

    #[test]
    fn cursors_at_the_ends_stay_put() {
        let (_t, mut d) = doc("ab");
        d.select(0, 0);
        add_cursor(&mut d, 2, 2);
        assert!(edit_all(&mut d, MultiEdit::Backspace));
        assert_eq!(text(&d), "a");
        assert!(edit_all(&mut d, MultiEdit::Delete));
        assert_eq!(text(&d), "");
        assert!(!edit_all(&mut d, MultiEdit::Delete));
    }
}
