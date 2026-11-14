//! Share safely (File menu): the selection, or the whole document, with secrets and personal data
//! hidden ([`bigtext::redact`]). The preview shows the first lines exactly as they will be written
//! (the same salt makes the same labels); then copy it (up to 4 MB) or save it to a file (any
//! size, streamed on a worker).

use std::io::Write;

use bigtext::jobs::{human_bytes, CancelToken};
use bigtext::redact::{self, Options, Redactor, KINDS};

use super::filter_panel::file_name;
use super::i18n::{tr, trf};
use super::tasks;
use super::BigTextApp;

/// Bytes read for the preview.
const PREVIEW_BYTES: u64 = 256 * 1024;
/// Lines the preview shows.
const PREVIEW_LINES: usize = 200;
/// The most that is copied to the clipboard; more is saved to a file.
const COPY_MAX: u64 = 4 << 20;

#[derive(Default)]
pub struct ShareUi {
    pub open: bool,
    opts: Options,
    /// More patterns, one per line.
    custom: String,
    salt: [u8; 16],
    /// What the preview was made from: document, version, range and options.
    made_for: Option<(u64, u64, u64, u64, Options)>,
    preview: String,
    counts: [u64; KINDS.len()],
    error: Option<String>,
    note: Option<String>,
}

impl ShareUi {
    /// The preview's text (the self-test reads it).
    pub fn preview(&self) -> &str {
        &self.preview
    }
}

/// `app.log` → `app.shared.log`.
fn shared_name(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem}.shared.{ext}"),
        _ => format!("{name}.shared.txt"),
    }
}

impl BigTextApp {
    pub(crate) fn open_share(&mut self) {
        if !self.docs.get(self.active).is_some_and(|d| d.is_ready()) {
            return;
        }
        self.share.open = true;
        self.share.salt = redact::random_salt();
        self.share.made_for = None;
        self.share.note = None;
    }

    /// What is shared: the selection, or the whole document after its byte order mark; and
    /// whether it is the selection.
    fn share_range(&self) -> Option<(u64, u64, bool)> {
        let d = self.docs.get(self.active)?;
        let (s, e) = d.selection();
        Some(if s < e {
            (s, e, true)
        } else {
            (d.buf.codec.bom_len as u64, d.buf.len(), false)
        })
    }

    fn share_options(&self) -> Options {
        Options {
            custom: self
                .share
                .custom
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect(),
            ..self.share.opts.clone()
        }
    }

    fn refresh_share_preview(&mut self) {
        let Some((s, e, _)) = self.share_range() else {
            return;
        };
        let opts = self.share_options();
        let Some(d) = self.docs.get(self.active) else {
            return;
        };
        let key = (d.id, d.version, s, e, opts.clone());
        if self.share.made_for.as_ref() == Some(&key) {
            return;
        }
        let end = e.min(s + PREVIEW_BYTES);
        // Not loaded yet (a slow drive): the next frame tries again.
        let Some(bytes) = d.buf.read(s..end) else {
            return;
        };
        let codec = d.buf.codec;
        match Redactor::new(&opts, self.share.salt) {
            Err(err) => {
                self.share.error = Some(err);
                self.share.preview.clear();
                self.share.counts = [0; KINDS.len()];
            }
            Ok(mut r) => {
                let mut out = Vec::new();
                let mut from = 0;
                while from < bytes.len() {
                    let stop = codec
                        .find_nl(&bytes[from..], s + from as u64)
                        .map(|i| from + i + codec.unit())
                        .unwrap_or(bytes.len());
                    r.line_in(&codec, &bytes[from..stop], &mut out);
                    from = stop;
                }
                let text = codec.decode_lossy(&out);
                self.share.preview = text
                    .lines()
                    .take(PREVIEW_LINES)
                    .collect::<Vec<_>>()
                    .join("\n");
                self.share.counts = r.counts();
                self.share.error = None;
            }
        }
        self.share.made_for = Some(key);
    }

    pub(crate) fn share_window(&mut self, ctx: &egui::Context) {
        if !self.share.open {
            return;
        }
        self.refresh_share_preview();
        let Some((s, e, selection)) = self.share_range() else {
            self.share.open = false;
            return;
        };
        let size = e - s;
        let mut open = true;
        let (mut copy, mut save) = (false, false);
        let st = &mut self.share;
        egui::Window::new(tr("Share safely"))
            .open(&mut open)
            .default_size([760.0, 520.0])
            .resizable(true)
            .collapsible(false)
            .show(ctx, |ui| {
                let what = if selection {
                    trf("The selection ({size})", &[("size", &human_bytes(size))])
                } else {
                    trf("The whole document ({size})", &[("size", &human_bytes(size))])
                };
                ui.strong(what);
                ui.horizontal_wrapped(|ui| {
                    ui.checkbox(&mut st.opts.tokens, tr("Tokens, Authorization and Cookie headers"));
                    ui.checkbox(&mut st.opts.urls, tr("Secrets in URLs"));
                    ui.checkbox(&mut st.opts.keys, tr("Named secrets (password=, \"secret\":)"));
                    ui.checkbox(&mut st.opts.emails, tr("E-mail addresses"));
                    ui.checkbox(&mut st.opts.ips, tr("IP addresses"));
                });
                ui.label(tr(
                    "More patterns (regular expressions, one per line; the first group is hidden):",
                ));
                ui.add(
                    egui::TextEdit::multiline(&mut st.custom)
                        .desired_rows(2)
                        .desired_width(f32::INFINITY)
                        .font(egui::TextStyle::Monospace),
                );
                if let Some(err) = &st.error {
                    ui.colored_label(ui.visuals().error_fg_color, err);
                }
                let hidden: Vec<String> = KINDS
                    .iter()
                    .zip(st.counts)
                    .filter(|(_, n)| *n > 0)
                    .map(|(k, n)| format!("{k} {n}"))
                    .collect();
                if hidden.is_empty() {
                    ui.label(tr("Nothing hidden in the preview."));
                } else {
                    ui.label(trf(
                        "Hidden in the preview: {list}",
                        &[("list", &hidden.join(", "))],
                    ));
                }
                ui.colored_label(
                    super::theme::warn_text(ui),
                    tr("The rules find common secrets, not every one: look at the preview before you share."),
                );
                let rest = (ui.available_height() - 36.0).max(80.0);
                egui::ScrollArea::both()
                    .max_height(rest)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add(
                            egui::Label::new(egui::RichText::new(&st.preview).monospace()).extend(),
                        );
                    });
                ui.horizontal(|ui| {
                    let fits = size <= COPY_MAX;
                    let b = ui.add_enabled(fits && st.error.is_none(), egui::Button::new(tr("Copy")));
                    let b = if fits {
                        b
                    } else {
                        b.on_disabled_hover_text(tr("Too large to copy; save it to a file instead."))
                    };
                    copy = b.clicked();
                    save = ui
                        .add_enabled(st.error.is_none(), egui::Button::new(tr("Save as…")))
                        .clicked();
                    if let Some(n) = &st.note {
                        ui.weak(n);
                    }
                });
            });
        if !open {
            self.share.open = false;
        }
        if copy {
            self.share_copy(ctx, s, e);
        }
        if save {
            self.share_save(s, e);
        }
    }

    fn share_copy(&mut self, ctx: &egui::Context, s: u64, e: u64) {
        let opts = self.share_options();
        let Some(d) = self.docs.get(self.active) else {
            return;
        };
        let Ok(mut r) = Redactor::new(&opts, self.share.salt) else {
            return;
        };
        let snap = d.buf.snapshot();
        let mut out = Vec::new();
        match redact::redact_to(&snap, s..e, &mut r, &mut out, &CancelToken::new(), None) {
            Ok(Some(n)) => {
                ctx.copy_text(snap.codec.decode_lossy(&out));
                self.share.note = Some(trf(
                    "Copied with {n} values hidden.",
                    &[("n", &n.to_string())],
                ));
            }
            _ => self.share.note = Some(tr("Could not read the document.").into()),
        }
    }

    fn share_save(&mut self, s: u64, e: u64) {
        let opts = self.share_options();
        let salt = self.share.salt;
        let Some(d) = self.docs.get(self.active) else {
            return;
        };
        let snap = d.buf.snapshot();
        let name = shared_name(&self.output_base_name());
        let Some(target) = self.pick_output_file(tr("Save safely as"), &name) else {
            return;
        };
        self.spawn_task(tr("Hiding secrets").into(), move |progress, cancel| {
            tasks::write_via_temp(&target, |w| {
                let bom = snap.codec.bom_len as u64;
                if bom > 0 {
                    if let Some(b) = snap.read(0..bom) {
                        w.write_all(&b)?;
                    }
                }
                let mut r = Redactor::new(&opts, salt).map_err(std::io::Error::other)?;
                Ok(
                    redact::redact_to(&snap, s..e, &mut r, w, cancel, Some(progress))?.map(|n| {
                        trf(
                            "Saved to {file}; {n} values hidden.",
                            &[("file", &file_name(&target)), ("n", &n.to_string())],
                        )
                    }),
                )
            })
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_names_keep_the_extension() {
        assert_eq!(shared_name("app.log"), "app.shared.log");
        assert_eq!(shared_name("data"), "data.shared.txt");
        assert_eq!(shared_name(".env"), ".env.shared.txt");
    }
}
