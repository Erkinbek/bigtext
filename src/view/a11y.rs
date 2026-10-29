//! Screen readers (AccessKit). The text view is a multi-line text field whose rows on screen are
//! text runs, with the caret, the selection and "Ln N, Col M"; announcements go through a live
//! region. Nothing is built until an assistive technology asks for the tree.

use egui::accesskit::{self, Live, Role, TextPosition, TextSelection};
use egui::{Context, Id, Rect, Sense, Ui, UiBuilder};

/// AccessKit's word starts are `u8` indexes, so a run holds at most this many characters.
const RUN_MAX: usize = 255;

/// A row on screen, as a screen reader gets it.
pub struct Row {
    pub value: String,
    /// UTF-8 bytes of each character of `value`.
    lens: Vec<u8>,
    /// Left edge (from the row's left) and width of each character.
    xs: Vec<f32>,
    ws: Vec<f32>,
    /// Document offset of each character.
    offs: Vec<u64>,
    /// The row's bytes in the document.
    pub start: u64,
    pub end: u64,
    pub rect: Rect,
}

impl Row {
    pub fn new(start: u64, end: u64, rect: Rect) -> Row {
        Row { value: String::new(), lens: Vec::new(), xs: Vec::new(), ws: Vec::new(), offs: Vec::new(), start, end, rect }
    }

    pub fn push(&mut self, ch: char, off: u64, x: f32, w: f32) {
        let n = self.value.len();
        self.value.push(ch);
        self.lens.push((self.value.len() - n) as u8);
        self.xs.push(x);
        self.ws.push(w);
        self.offs.push(off);
    }

    /// The character index of document offset `off` in this row.
    fn index_of(&self, off: u64) -> usize {
        self.offs.iter().take_while(|&&o| o < off).count()
    }
}

/// Whether screen reader nodes are wanted this frame (cheap when they are not).
pub fn enabled(ctx: &Context, id: Id) -> bool {
    ctx.accesskit_node_builder(id, |_| ()).is_some()
}

/// The text view's node.
pub struct TextWidget<'a> {
    pub label: &'a str,
    /// "Ln 12, Col 5".
    pub position: String,
    pub read_only: bool,
    pub rows: &'a [Row],
    pub anchor: u64,
    pub caret: u64,
}

fn run_id(id: Id, row: usize, chunk: usize) -> Id {
    id.with(("a11y-run", row, chunk))
}

/// Row `row`, character `k` as a position in its runs.
fn position(id: Id, row: usize, k: usize) -> TextPosition {
    // At a chunk boundary the position is the end of the previous chunk.
    let chunk = if k > 0 && k.is_multiple_of(RUN_MAX) { k / RUN_MAX - 1 } else { k / RUN_MAX };
    TextPosition { node: run_id(id, row, chunk).accesskit_id(), character_index: k - chunk * RUN_MAX }
}

/// The row and character of document offset `off`, when it is on screen.
fn locate(rows: &[Row], off: u64) -> Option<(usize, usize)> {
    rows.iter().position(|r| off >= r.start && off <= r.end).map(|i| (i, rows[i].index_of(off)))
}

/// Fills the view's node `id` and adds a text run per row (split every 255 characters).
/// Returns what was exposed, for tests.
pub fn text_widget(ui: &mut Ui, id: Id, w: &TextWidget) -> String {
    let ctx = ui.ctx().clone();
    // The runs hang under the view's node (through a container: only a `Ui` can set a parent).
    let runs = ui.new_child(UiBuilder::new().accessibility_parent(id));
    let sel = locate(w.rows, w.caret).map(|focus| (locate(w.rows, w.anchor).unwrap_or(focus), focus));
    ctx.accesskit_node_builder(id, |b| {
        b.set_role(Role::MultilineTextInput);
        b.set_label(w.label);
        b.set_description(w.position.as_str());
        if w.read_only {
            b.set_read_only();
        }
        if let Some((a, f)) = sel {
            b.set_text_selection(TextSelection { anchor: position(id, a.0, a.1), focus: position(id, f.0, f.1) });
        }
    });
    for (i, r) in w.rows.iter().enumerate() {
        let chunks = r.lens.len().div_ceil(RUN_MAX).max(1);
        let mut byte = 0;
        for c in 0..chunks {
            let (a, z) = (c * RUN_MAX, ((c + 1) * RUN_MAX).min(r.lens.len()));
            let len: usize = r.lens[a..z].iter().map(|&l| l as usize).sum();
            let value = &r.value[byte..byte + len];
            byte += len;
            let rid = run_id(id, i, c);
            runs.interact(Rect::NOTHING, rid, Sense::hover());
            ctx.accesskit_node_builder(rid, |b| {
                b.set_role(Role::TextRun);
                b.set_text_direction(accesskit::TextDirection::LeftToRight);
                let x0 = r.xs.get(a).copied().unwrap_or(0.0);
                let x1 = if z > a { r.xs[z - 1] + r.ws[z - 1] } else { x0 };
                b.set_bounds(accesskit::Rect { x0: (r.rect.left() + x0).into(), y0: r.rect.top().into(), x1: (r.rect.left() + x1).into(), y1: r.rect.bottom().into() });
                b.set_value(value);
                b.set_character_lengths(&r.lens[a..z]);
                b.set_character_positions(r.xs[a..z].iter().map(|x| x - x0).collect::<Vec<f32>>());
                b.set_character_widths(&r.ws[a..z]);
                if c > 0 {
                    b.set_previous_on_line(run_id(id, i, c - 1).accesskit_id());
                }
                if c + 1 < chunks {
                    b.set_next_on_line(run_id(id, i, c + 1).accesskit_id());
                }
            });
        }
    }
    let first = w.rows.first().map(|r| r.value.trim_end_matches('\n')).unwrap_or_default();
    format!("{} rows; {}; first: {first}; selection {sel:?}", w.rows.len(), w.position)
}

/// A polite live region: screen readers say `text` when it changes.
pub fn live_region(ctx: &Context, id: Id, text: &str) {
    ctx.accesskit_node_builder(id, |b| {
        b.set_role(Role::Status);
        b.set_live(Live::Polite);
        b.set_value(text);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_split_long_rows() {
        let id = Id::new("v");
        assert_eq!(position(id, 0, 0).node, run_id(id, 0, 0).accesskit_id());
        assert_eq!((position(id, 2, 255).node, position(id, 2, 255).character_index), (run_id(id, 2, 0).accesskit_id(), 255));
        assert_eq!((position(id, 2, 256).node, position(id, 2, 256).character_index), (run_id(id, 2, 1).accesskit_id(), 1));
        let mut r = Row::new(10, 14, Rect::ZERO);
        for (i, ch) in "añb\n".chars().enumerate() {
            r.push(ch, 10 + i as u64, i as f32 * 8.0, 8.0);
        }
        assert_eq!(r.lens, [1, 2, 1, 1]);
        assert_eq!(locate(std::slice::from_ref(&r), 12), Some((0, 2)));
        assert_eq!(locate(std::slice::from_ref(&r), 99), None);
    }
}
