//! Edit → Text: sorting, de-duplicating, case, trimming, indentation and JSON formatting of
//! the selection (or of a small whole document), as one undoable edit; decoding and encoding
//! (Base64, URL, hex, HTML, JSON strings) of the selection or the word at the caret, with a
//! result that is not text for this document shown in a window instead.

use std::ops::Range;

use bigtext::jobs::{self, human_bytes};
use bigtext::textops::{self, Op};

use super::document::EditKind;
use super::i18n::{tr, trf};
use super::{BigTextApp, Msg};

/// Text operations work in memory on at most this much text.
pub const TEXT_OP_MAX: u64 = 64 * 1024 * 1024;

/// What a text operation came to.
pub enum Outcome {
    Replace(Vec<u8>),
    Unchanged,
    /// Shown rather than put in the document: a JWT, bytes that are not text, or characters
    /// the document's encoding lacks.
    Show { text: String, bytes: Vec<u8>, binary: bool },
}

pub struct TextOpDone {
    pub doc: u64,
    pub version: u64,
    pub op: Op,
    pub range: Range<u64>,
    pub result: Result<Outcome, String>,
}

/// A decoded result in a window (Copy, Open in a new tab).
pub struct TextResult {
    pub op: Op,
    pub text: String,
    pub bytes: Vec<u8>,
    pub binary: bool,
}

/// Shown at most, of a long result.
const SHOW_MAX: usize = 256 * 1024;

/// Lines of 16 bytes: offset, hex, and the printable ones.
fn hex_dump(b: &[u8]) -> String {
    let mut out = String::new();
    for (i, row) in b.chunks(16).enumerate() {
        if out.len() > SHOW_MAX {
            break;
        }
        let hex: Vec<String> = row.iter().map(|x| format!("{x:02x}")).collect();
        let text: String = row.iter().map(|&x| if x.is_ascii_graphic() || x == b' ' { x as char } else { '.' }).collect();
        out.push_str(&format!("{:08x}  {:<48}  {text}\n", i * 16, hex.join(" ")));
    }
    out
}

/// A codec operation's result, judged: put back as text when it is clean text the document's
/// encoding can hold, shown otherwise.
fn judge(op: Op, text: &str, codec: bigtext::encoding::Codec, crlf: bool, original: &[u8]) -> Result<Outcome, String> {
    if op == Op::B64Decode {
        if let Some(j) = bigtext::codec::jwt(text) {
            return Ok(Outcome::Show { bytes: j.clone().into_bytes(), text: j, binary: false });
        }
    }
    let out = textops::codec_apply(op, text)?;
    match String::from_utf8(out) {
        Ok(s) if s.chars().all(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t')) => {
            // The document's own line breaks.
            let s = if crlf { s.replace("\r\n", "\n").replace('\n', "\r\n") } else { s };
            let enc = codec.encode(&s);
            if codec.decode_lossy(&enc) != s {
                return Ok(Outcome::Show { bytes: s.clone().into_bytes(), text: s, binary: false });
            }
            Ok(if enc == original { Outcome::Unchanged } else { Outcome::Replace(enc) })
        }
        Ok(s) => Ok(Outcome::Show { text: hex_dump(s.as_bytes()), bytes: s.into_bytes(), binary: true }),
        Err(e) => {
            let b = e.into_bytes();
            Ok(Outcome::Show { text: hex_dump(&b), bytes: b, binary: true })
        }
    }
}

impl BigTextApp {
    pub(crate) fn text_op(&mut self, op: Op) {
        let Some(d) = self.docs.get(self.active) else { return };
        if !d.editable() {
            self.error = Some(tr("The document is read-only (Edit → Read-only).").into());
            return;
        }
        let (s, e) = d.selection();
        let range = match (s == e, op.is_codec_op()) {
            (false, _) => s..e,
            (true, false) => d.min_caret()..d.buf.len(),
            // Decoding takes the word at the caret, not the whole document.
            (true, true) => match d.token_at_caret() {
                Some(r) => r,
                None => {
                    self.notify(tr("Select the text, or put the caret on the word to change.").into(), None);
                    return;
                }
            },
        };
        if range.end - range.start > TEXT_OP_MAX {
            if !op.streams() {
                self.error = Some(trf("Sorting and removing duplicates work on up to {size}. Select a smaller part.", &[("size", &human_bytes(TEXT_OP_MAX))]));
            } else if d.path.is_none() {
                self.error = Some(trf("Text over {size} is changed on disk: save the document to a file first.", &[("size", &human_bytes(TEXT_OP_MAX))]));
            } else {
                // Too large to change in memory: written to a new file that replaces this one.
                let message = format!(
                    "{}{}",
                    trf(
                        "{op} on {size}: the result is written to a new file that atomically replaces “{file}”; undo will not be available.",
                        &[("op", tr(op.name())), ("size", &human_bytes(range.end - range.start)), ("file", &d.title)]
                    ),
                    if d.is_modified() { tr(" Your unsaved edits are included.") } else { "" }
                );
                let doc = d.id;
                self.confirm = Some(super::Confirm {
                    title: tr(op.name()).into(),
                    message,
                    action: super::ConfirmAction::TextOpOnDisk { doc, op, range },
                    checkbox: Some((tr("Keep a backup (.bak)").into(), true)),
                });
            }
            return;
        }
        let (snap, doc, version) = (d.buf.snapshot(), d.id, d.version);
        let crlf = d.line_ending == bigtext::encoding::LineEnding::CrLf;
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
                if op.is_codec_op() {
                    return judge(op, &text, snap.codec, crlf, &bytes);
                }
                let out = textops::apply(op, &text, bigtext::text::rows::TAB_WIDTH)?;
                Ok(if out != text { Outcome::Replace(snap.codec.encode(&out)) } else { Outcome::Unchanged })
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
            Ok(Outcome::Replace(bytes)) => {
                let start = r.range.start;
                d.edit(r.range, &bytes, EditKind::Other);
                d.select(start, start + bytes.len() as u64);
                self.notify(trf("{op}: done (Ctrl+Z undoes it).", &[("op", tr(r.op.name()))]), None);
            }
            Ok(Outcome::Unchanged) => self.notify(trf("{op}: nothing to change.", &[("op", tr(r.op.name()))]), None),
            Ok(Outcome::Show { text, bytes, binary }) => self.text_result = Some(TextResult { op: r.op, text, bytes, binary }),
            Err(e) => self.error = Some(e),
        }
    }

    pub(super) fn text_menu(&mut self, ui: &mut egui::Ui) {
        let editable = self.docs.get(self.active).is_some_and(|d| d.editable());
        ui.add_enabled_ui(editable, |ui| {
            ui.menu_button(tr("Text"), |ui| {
                ui.weak(tr("On the selection, or the whole document; above 64 MB rewritten on disk"));
                let mut last_line_op = true;
                for op in Op::ALL {
                    if op.is_line_op() != last_line_op || op == Op::JsonPretty || op == Op::B64Decode {
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

impl BigTextApp {
    pub(super) fn text_result_window(&mut self, ctx: &egui::Context) {
        let Some(r) = &self.text_result else { return };
        let mut open = true;
        let (mut copy, mut new_tab) = (false, false);
        egui::Window::new(trf("{op}: the result", &[("op", tr(r.op.name()))])).open(&mut open).default_size([560.0, 360.0]).show(ctx, |ui| {
            if r.binary {
                ui.weak(trf("{n} bytes that are not text, shown in hex.", &[("n", &r.bytes.len().to_string())]));
            } else {
                ui.weak(tr("Shown here rather than put in the document."));
            }
            let mut shown: &str = &r.text[..r.text.len().min(SHOW_MAX)];
            if shown.len() < r.text.len() {
                let mut cut = shown.len();
                while !r.text.is_char_boundary(cut) {
                    cut -= 1;
                }
                shown = &r.text[..cut];
            }
            egui::ScrollArea::both().max_height(300.0).show(ui, |ui| {
                ui.add(egui::TextEdit::multiline(&mut shown).font(egui::TextStyle::Monospace).desired_width(f32::INFINITY));
            });
            if shown.len() < r.text.len() {
                ui.weak(trf("Only the first {size} are shown.", &[("size", &human_bytes(SHOW_MAX as u64))]));
            }
            ui.horizontal(|ui| {
                copy = ui.button(tr("Copy")).clicked();
                new_tab = ui.button(tr("Open in a new tab")).clicked();
            });
        });
        if copy {
            ctx.copy_text(if r.binary { r.text.clone() } else { String::from_utf8_lossy(&r.bytes).into_owned() });
        }
        if new_tab {
            if let Some(r) = self.text_result.take() {
                self.new_doc();
                if let Some(d) = self.docs.get_mut(self.active) {
                    d.edit(0..0, &r.bytes, EditKind::Other);
                    d.hex = r.binary;
                }
            }
            return;
        }
        if !open {
            self.text_result = None;
        }
    }
}
