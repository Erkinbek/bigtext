//! Edit → Text: sorting, de-duplicating, case, trimming, indentation and JSON formatting of
//! the selection (or of a small whole document), as one undoable edit.

use std::ops::Range;

use bigtext::jobs::{self, human_bytes};
use bigtext::textops::{self, Op};

use super::document::EditKind;
use super::i18n::{tr, trf};
use super::{BigTextApp, Msg};

/// Text operations work in memory on at most this much text.
pub const TEXT_OP_MAX: u64 = 64 * 1024 * 1024;

pub struct TextOpDone {
    pub doc: u64,
    pub version: u64,
    pub op: Op,
    pub range: Range<u64>,
    pub result: Result<Option<Vec<u8>>, String>,
}

impl BigTextApp {
    pub(crate) fn text_op(&mut self, op: Op) {
        let Some(d) = self.docs.get(self.active) else { return };
        if !d.editable() {
            self.error = Some(tr("The document is read-only (Edit → Read-only).").into());
            return;
        }
        let (s, e) = d.selection();
        let range = if s == e { d.min_caret()..d.buf.len() } else { s..e };
        if range.end - range.start > TEXT_OP_MAX {
            self.error = Some(trf("Text operations work on up to {size}. Select a smaller part.", &[("size", &human_bytes(TEXT_OP_MAX))]));
            return;
        }
        let (snap, doc, version) = (d.buf.snapshot(), d.id, d.version);
        let tx = self.tx.clone();
        let waker = self.waker.clone();
        self.search.status = trf("{op}…", &[("op", tr(op.name()))]);
        jobs::spawn("text-op", move || {
            let result = (|| {
                let bytes = snap.read(range.clone()).ok_or_else(|| tr("Could not read the text.").to_string())?;
                let text = snap.codec.decode_lossy(&bytes);
                if snap.codec.encode(&text) != *bytes {
                    return Err(trf("The text contains bytes that are not valid {enc}; the operation would change them.", &[("enc", snap.codec.enc.name())]));
                }
                let out = textops::apply(op, &text, bigtext::text::rows::TAB_WIDTH)?;
                Ok((out != text).then(|| snap.codec.encode(&out)))
            })();
            let _ = tx.send(Msg::TextOp(Box::new(TextOpDone { doc, version, op, range, result })));
            waker();
        });
    }

    pub(super) fn on_text_op(&mut self, r: TextOpDone) {
        self.search.status.clear();
        let Some(d) = self.docs.iter_mut().find(|d| d.id == r.doc) else { return };
        match r.result {
            _ if d.version != r.version => self.error = Some(tr("The document changed while the operation ran; nothing was changed.").into()),
            Ok(Some(bytes)) => {
                let start = r.range.start;
                d.edit(r.range, &bytes, EditKind::Other);
                d.select(start, start + bytes.len() as u64);
                self.notify(trf("{op}: done (Ctrl+Z undoes it).", &[("op", tr(r.op.name()))]), None);
            }
            Ok(None) => self.notify(trf("{op}: nothing to change.", &[("op", tr(r.op.name()))]), None),
            Err(e) => self.error = Some(e),
        }
    }

    pub(super) fn text_menu(&mut self, ui: &mut egui::Ui) {
        let editable = self.docs.get(self.active).is_some_and(|d| d.editable());
        ui.add_enabled_ui(editable, |ui| {
            ui.menu_button(tr("Text"), |ui| {
                ui.weak(tr("On the selection, or the whole document up to 64 MB"));
                let mut last_line_op = true;
                for op in Op::ALL {
                    if op.is_line_op() != last_line_op || op == Op::JsonPretty {
                        ui.separator();
                    }
                    last_line_op = op.is_line_op();
                    if ui.button(tr(op.name())).clicked() {
                        self.text_op(op);
                        ui.close();
                    }
                }
            });
        });
    }
}
