//! Edit → Column editor…: a number sequence or the same text in the block's column (or at the
//! caret, one row), on the block's rows as one undoable edit, or down to the end of the file as
//! a rewrite on disk.

use bigtext::column_edit::ColumnGen;
use bigtext::text::rows;

use super::i18n::{tr, trf};
use super::ops::WriteSpec;
use super::{BigTextApp, Confirm, ConfirmAction};
use crate::view::block::{block_rows, edit_block, Block, BlockEdit};

pub struct ColumnEditor {
    pub numbers: bool,
    pub start: String,
    pub step: String,
    pub width: String,
    pub hex: bool,
    pub text: String,
    /// Down to the end of the file rather than the block's rows.
    pub to_end: bool,
    pub error: Option<String>,
}

impl Default for ColumnEditor {
    fn default() -> Self {
        ColumnEditor { numbers: true, start: "1".into(), step: "1".into(), width: "".into(), hex: false, text: String::new(), to_end: false, error: None }
    }
}

impl ColumnEditor {
    fn gen(&self) -> Result<ColumnGen, String> {
        if !self.numbers {
            return Ok(ColumnGen::Text(self.text.clone()));
        }
        let num = |s: &str, what: &str| s.trim().parse::<i64>().map_err(|_| trf("{what} is not a whole number.", &[("what", what)]));
        let width = if self.width.trim().is_empty() { 0 } else { self.width.trim().parse::<usize>().map_err(|_| tr("The width is not a number.").to_string())?.min(64) };
        Ok(ColumnGen::Numbers { start: num(&self.start, tr("The first number"))?, step: num(&self.step, tr("The step"))?, width, hex: self.hex })
    }
}

impl BigTextApp {
    pub(crate) fn open_column_editor(&mut self) {
        match self.docs.get(self.active) {
            Some(d) if d.view.wrap => self.notify(tr("The column editor works without word wrap (View → Word wrap).").into(), None),
            Some(d) if d.editable() => self.column_editor = Some(ColumnEditor::default()),
            Some(_) => self.notify(tr("The document is read-only").into(), None),
            None => {}
        }
    }

    /// The block to fill: the selected one, or a column at the caret on its row.
    fn editor_block(&self) -> Option<Block> {
        let d = self.docs.get(self.active)?;
        if let Some(b) = d.view.block {
            return Some(b);
        }
        let row = rows::row_at(&d.buf, d.view.caret, None).ok()?;
        let cell = rows::cell_of_offset(&d.buf, &row, d.view.caret).ok()?;
        Some(Block::at(row.start, cell))
    }

    /// Fills the column; `false` when it could not.
    pub(crate) fn apply_column_edit(&mut self, gen: ColumnGen, to_end: bool) -> bool {
        let Some(mut b) = self.editor_block() else { return false };
        let Some(d) = self.docs.get_mut(self.active) else { return false };
        if !to_end {
            let Ok(n) = block_rows(d, &b).map(|r| r.len()) else { return false };
            let values = (0..n as u64).map(|i| gen.value(i)).collect();
            if !edit_block(d, &mut b, BlockEdit::PerRow(values)) {
                return false;
            }
            d.view.block = Some(b);
            return true;
        }
        if d.path.is_none() {
            self.error = Some(tr("Save the document to a file first; then the column is filled on disk.").into());
            return false;
        }
        let (from, cells) = (b.span().0, b.cells());
        let message = format!(
            "{}{}",
            trf("Every line from the block's first row to the end of “{file}” gets the column; the result atomically replaces the file and undo will not be available.", &[("file", &d.title)]),
            if d.is_modified() { tr(" Your unsaved edits are included.") } else { "" }
        );
        let doc = d.id;
        self.confirm = Some(Confirm {
            title: tr("Column editor").into(),
            message,
            action: ConfirmAction::ColumnEditOnDisk { doc, from, cells, gen },
            checkbox: Some((tr("Keep a backup (.bak)").into(), true)),
        });
        true
    }

    pub(super) fn column_edit_on_disk(&mut self, doc: u64, from: u64, cells: (u32, u32), gen: ColumnGen, backup: bool) {
        let Some(path) = self.docs.iter().find(|d| d.id == doc).and_then(|d| d.path.clone()) else { return };
        self.start_write(doc, path, WriteSpec::ColumnEdit { from, cells, gen }, backup);
    }

    pub(super) fn column_editor_window(&mut self, ctx: &egui::Context) {
        let Some(e) = self.column_editor.as_mut() else { return };
        let mut open = true;
        let mut go = false;
        egui::Window::new(tr("Column editor")).open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
            ui.weak(tr("Fills the block's column (Alt+drag or Shift+Alt+arrows), or a column at the caret."));
            ui.horizontal(|ui| {
                ui.radio_value(&mut e.numbers, true, tr("Numbers"));
                ui.radio_value(&mut e.numbers, false, tr("Text"));
            });
            if e.numbers {
                egui::Grid::new("column-numbers").num_columns(2).show(ui, |ui| {
                    ui.label(tr("First number"));
                    ui.add(egui::TextEdit::singleline(&mut e.start).desired_width(90.0));
                    ui.end_row();
                    ui.label(tr("Step"));
                    ui.add(egui::TextEdit::singleline(&mut e.step).desired_width(90.0));
                    ui.end_row();
                    ui.label(tr("Digits at least"));
                    ui.add(egui::TextEdit::singleline(&mut e.width).desired_width(90.0).hint_text("0"));
                    ui.end_row();
                });
                ui.checkbox(&mut e.hex, tr("Hexadecimal"));
            } else {
                ui.add(egui::TextEdit::singleline(&mut e.text).desired_width(240.0).hint_text(tr("the same text on every row")));
            }
            ui.separator();
            ui.radio_value(&mut e.to_end, false, tr("The block's rows (one undo step)"));
            ui.radio_value(&mut e.to_end, true, tr("Every line from there to the end of the file (rewritten on disk)"));
            if let Some(err) = &e.error {
                ui.colored_label(ui.visuals().error_fg_color, err);
            }
            if let Ok(g) = e.gen() {
                ui.weak(trf("Rows get: {values}, …", &[("values", &(0..3).map(|i| g.value(i)).collect::<Vec<_>>().join(", "))]));
            }
            go = ui.button(tr("Fill")).clicked();
        });
        if go {
            let (gen, to_end) = match self.column_editor.as_ref().map(|e| (e.gen(), e.to_end)) {
                Some((Ok(g), t)) => (g, t),
                Some((Err(err), _)) => {
                    if let Some(e) = self.column_editor.as_mut() {
                        e.error = Some(err);
                    }
                    return;
                }
                None => return,
            };
            if self.apply_column_edit(gen, to_end) {
                self.column_editor = None;
            }
        } else if !open {
            self.column_editor = None;
        }
    }
}
