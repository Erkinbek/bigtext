//! View → Font…: the monospace font of the text, chosen from the fonts installed on the system
//! or any font file. The built-in font stays as the fallback for characters the chosen font
//! lacks.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use skrifa::instance::{LocationRef, Size};
use skrifa::MetadataProvider;

use super::i18n::{tr, trf};
use super::BigTextApp;

#[derive(Default)]
pub struct FontsUi {
    pub open: bool,
    /// Monospace fonts found on the system (name, file), scanned when the window opens.
    found: Option<Vec<(String, PathBuf)>>,
    pub error: Option<String>,
    /// The chosen font is not monospaced.
    pub proportional: bool,
}

/// Font files whose names say they are monospaced.
const MONO_HINTS: &[&str] = &["mono", "code", "consol", "hack", "iosevka", "inconsolata", "menlo", "monaco", "courier", "fixed", "terminal"];
/// Exact names of monospaced fonts that ship with Windows.
const WINDOWS_MONO: &[(&str, &str)] = &[("consola.ttf", "Consolas"), ("cour.ttf", "Courier New"), ("lucon.ttf", "Lucida Console")];
/// Styles other than regular.
const STYLES: &[&str] = &["bold", "italic", "oblique", "light", "thin", "black", "medium", "semibold", "heavy", "extra", "condensed", "nerd", "propo"];

fn font_dirs() -> Vec<(PathBuf, usize)> {
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
    let mut v = Vec::new();
    if cfg!(windows) {
        v.extend(env("WINDIR").map(|w| (w.join("Fonts"), 0)));
        v.extend(env("LOCALAPPDATA").map(|l| (l.join("Microsoft").join("Windows").join("Fonts"), 0)));
    } else if cfg!(target_os = "macos") {
        v.push((PathBuf::from("/System/Library/Fonts"), 1));
        v.push((PathBuf::from("/Library/Fonts"), 1));
        v.extend(env("HOME").map(|h| (h.join("Library").join("Fonts"), 1)));
    } else {
        v.push((PathBuf::from("/usr/share/fonts"), 4));
        v.push((PathBuf::from("/usr/local/share/fonts"), 4));
        v.extend(env("HOME").map(|h| (h.join(".local").join("share").join("fonts"), 4)));
        v.extend(env("HOME").map(|h| (h.join(".fonts"), 4)));
    }
    v
}

/// "JetBrainsMono-Regular" → "JetBrains Mono".
fn display_name(stem: &str) -> String {
    let base = stem.trim_end_matches("-Regular").trim_end_matches("_Regular").trim_end_matches("-R").replace(['-', '_'], " ");
    let mut out = String::new();
    let chars: Vec<char> = base.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if i > 0 && c.is_uppercase() && chars[i - 1].is_lowercase() {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

/// The regular monospace fonts in the usual font folders.
pub fn system_fonts() -> Vec<(String, PathBuf)> {
    fn walk(dir: &Path, depth: usize, out: &mut Vec<(String, PathBuf)>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                if depth > 0 {
                    walk(&p, depth - 1, out);
                }
                continue;
            }
            let file = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
            if !(file.ends_with(".ttf") || file.ends_with(".otf") || file.ends_with(".ttc")) {
                continue;
            }
            if let Some((_, name)) = WINDOWS_MONO.iter().find(|(f, _)| *f == file) {
                out.push((name.to_string(), p));
                continue;
            }
            let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let low = stem.to_lowercase();
            if MONO_HINTS.iter().any(|h| low.contains(h)) && !STYLES.iter().any(|s| low.contains(s)) {
                out.push((display_name(&stem), p));
            }
        }
    }
    let mut out = Vec::new();
    for (dir, depth) in font_dirs() {
        walk(&dir, depth, &mut out);
    }
    out.sort_by_key(|a| a.0.to_lowercase());
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

/// Checks that egui can use the font (it panics on files it cannot parse) and whether it is
/// monospaced.
pub fn check_font(bytes: &[u8]) -> Result<bool, String> {
    let font = skrifa::FontRef::from_index(bytes, 0).map_err(|e| trf("Not a usable font file: {e}", &[("e", &e.to_string())]))?;
    let charmap = font.charmap();
    let (Some(i), Some(m)) = (charmap.map('i'), charmap.map('M')) else {
        return Err(tr("The font has no Latin letters.").into());
    };
    let metrics = font.glyph_metrics(Size::unscaled(), LocationRef::default());
    Ok(metrics.advance_width(i) == metrics.advance_width(m))
}

/// Installs the font at `path` as the first monospace font (none: the built-in one only).
/// Returns whether it is monospaced.
pub fn apply_font(ctx: &egui::Context, path: Option<&Path>) -> Result<bool, String> {
    let mut defs = egui::FontDefinitions::default();
    let mut mono = true;
    if let Some(p) = path {
        let bytes = std::fs::read(p).map_err(|e| super::friendly_io(&e, p))?;
        mono = check_font(&bytes)?;
        defs.font_data.insert("user-mono".into(), Arc::new(egui::FontData::from_owned(bytes)));
        defs.families.entry(egui::FontFamily::Monospace).or_default().insert(0, "user-mono".into());
    }
    ctx.set_fonts(defs);
    Ok(mono)
}

impl BigTextApp {
    /// Uses `path` for the text, or the built-in font; keeps the choice on success.
    pub(crate) fn set_font(&mut self, ctx: &egui::Context, path: Option<PathBuf>) {
        match apply_font(ctx, path.as_deref()) {
            Ok(mono) => {
                self.settings.font_path = path;
                self.fonts.error = None;
                self.fonts.proportional = !mono;
                self.persist_state();
            }
            Err(e) => self.fonts.error = Some(e),
        }
    }

    /// Self-test: [`Self::set_font`] without a frame at hand.
    pub(crate) fn test_set_font(&mut self, path: Option<PathBuf>) {
        if let Some(ctx) = self.ctx_clone() {
            self.set_font(&ctx, path);
        }
    }

    pub(super) fn fonts_window(&mut self, ctx: &egui::Context) {
        if !self.fonts.open {
            self.fonts.found = None;
            return;
        }
        let found = self.fonts.found.get_or_insert_with(system_fonts).clone();
        let current = self.settings.font_path.clone();
        let mut choose: Option<Option<PathBuf>> = None;
        let mut open = true;
        egui::Window::new(tr("Font")).open(&mut open).default_size([420.0, 460.0]).show(ctx, |ui| {
            ui.label(tr("Font of the text (monospaced fonts keep columns aligned):"));
            egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                if ui.radio(current.is_none(), tr("Built-in (Hack)")).clicked() {
                    choose = Some(None);
                }
                for (name, p) in &found {
                    let on = current.as_deref() == Some(p.as_path());
                    if ui.radio(on, name).on_hover_text(bigtext::file::display_path(p)).clicked() {
                        choose = Some(Some(p.clone()));
                    }
                }
                if let Some(p) = current.as_ref().filter(|p| !found.iter().any(|f| &f.1 == *p)) {
                    let _ = ui.radio(true, bigtext::file::display_path(p));
                }
            });
            if ui.button(tr("Other font file…")).clicked() {
                if let Some(p) = rfd::FileDialog::new().set_title(tr("Font")).add_filter(tr("Fonts"), &["ttf", "otf", "ttc"]).pick_file() {
                    choose = Some(Some(p));
                }
            }
            if let Some(e) = &self.fonts.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            } else if self.fonts.proportional {
                ui.colored_label(super::theme::warn_text(ui), tr("This font is not monospaced, so columns will not line up."));
            }
            ui.separator();
            ui.label(egui::RichText::new("0O 1lI| {}[] => != ~ -- ...\nThe quick brown fox · Привет · O'zbekcha").monospace().size(self.settings.font_size));
        });
        if let Some(p) = choose {
            self.set_font(ctx, p);
        }
        if !open {
            self.fonts.open = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(display_name("JetBrainsMono-Regular"), "Jet Brains Mono");
        assert_eq!(display_name("DejaVuSansMono"), "Deja Vu Sans Mono");
        assert_eq!(display_name("Hack-Regular"), "Hack");
    }

    #[test]
    fn checks_fonts() {
        assert!(check_font(b"definitely not a font").is_err());
        let builtin = egui::FontDefinitions::default();
        assert_eq!(check_font(&builtin.font_data["Hack"].font), Ok(true));
        assert_eq!(check_font(&builtin.font_data["Ubuntu-Light"].font), Ok(false));
    }
}
