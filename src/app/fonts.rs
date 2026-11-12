//! View → Font…: the monospace font of the text, chosen from the fonts installed on the system
//! or any font file. The built-in font stays as the fallback for characters the chosen font
//! lacks.
//!
//! The built-in fonts have no Arabic, Hindi, Thai, Korean… When the text shows a character no
//! loaded font has, a system font that has it is looked for on a worker and added behind the
//! others, so the startup reads no big font files.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

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

/// Fonts with Chinese characters that come with the system, best first.
const CJK_FONTS: &[&str] = &[
    "msyh.ttc",
    "msyh.ttf",
    "Deng.ttf",
    "simhei.ttf",
    "simsun.ttc",
    "/System/Library/Fonts/PingFang.ttc",
    "/System/Library/Fonts/STHeiti Light.ttc",
    "/System/Library/Fonts/Hiragino Sans GB.ttc",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
    "/usr/share/fonts/wenquanyi/wqy-microhei/wqy-microhei.ttc",
    "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
];

/// System fonts with the characters of other scripts, in the order they are tried: names in
/// the Windows font folder, or macOS and Linux paths. The fonts in the usual font folders come
/// after them.
const FALLBACK_FONTS: &[&str] = &[
    // Latin beyond the built-in fonts (Vietnamese), Greek, Cyrillic, Arabic, Hebrew, Georgian,
    // Armenian.
    "segoeui.ttf",
    // Devanagari, Bengali, Tamil, Telugu, Gujarati, Gurmukhi, Kannada, Malayalam, Odia, Sinhala.
    "Nirmala.ttc",
    // Thai, Lao, Khmer.
    "LeelawUI.ttf",
    // Symbols, braille, mathematical letters, circled numbers.
    "seguisym.ttf",
    // Chinese, Korean, Japanese.
    "msyh.ttc",
    "simsun.ttc",
    "malgun.ttf",
    "YuGothR.ttc",
    "msgothic.ttc",
    // Ethiopic and N'Ko, Cherokee and Canadian syllabics, Tibetan, Mongolian, Myanmar, Thaana,
    // Syriac and historic scripts, Yi, Tai Le, New Tai Lue, Phags-pa, Javanese.
    "ebrima.ttf",
    "gadugi.ttf",
    "himalaya.ttf",
    "monbaiti.ttf",
    "mmrtext.ttf",
    "mvboli.ttf",
    "seguihis.ttf",
    "msyi.ttf",
    "taile.ttf",
    "ntailu.ttf",
    "phagspa.ttf",
    "javatext.ttf",
    "arial.ttf",
    "sylfaen.ttf",
    "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
    "/System/Library/Fonts/PingFang.ttc",
    "/System/Library/Fonts/AppleSDGothicNeo.ttc",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
];

/// Fonts tried first for Japanese text (kana in sight), so its kanji take Japanese shapes.
const JAPANESE_FONTS: &[&str] = &["YuGothR.ttc", "msgothic.ttc", "/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc"];

/// System fonts added so far for characters the other fonts lack, in the order they are tried.
static FALLBACKS: Mutex<Vec<(PathBuf, Arc<egui::FontData>)>> = Mutex::new(Vec::new());
/// A fallback was added since the fonts were last applied.
static FALLBACKS_CHANGED: AtomicBool = AtomicBool::new(false);

struct Lookup {
    /// Characters waiting for the worker.
    queue: Vec<char>,
    /// Characters asked for already (found or not).
    asked: Option<HashSet<char>>,
    /// Characters no system font had.
    misses: usize,
    running: bool,
}

static LOOKUP: Mutex<Lookup> = Mutex::new(Lookup { queue: Vec::new(), asked: None, misses: 0, running: false });

fn font_path(name: &str) -> Option<PathBuf> {
    if name.starts_with('/') {
        return Some(PathBuf::from(name));
    }
    std::env::var_os("WINDIR").map(|w| PathBuf::from(w).join("Fonts").join(name))
}

/// Reads the font at `path` if egui can use it (it stops on a font it cannot read) and it has
/// `ch`.
fn read_font_with(path: &Path, ch: char) -> Option<Vec<u8>> {
    let bytes = std::fs::read(path).ok()?;
    skrifa::FontRef::from_index(&bytes, 0).ok()?.charmap().map(ch)?;
    Some(bytes)
}

/// True if one of the fallbacks added so far has `ch`.
fn fallbacks_have(ch: char) -> bool {
    FALLBACKS.lock().unwrap().iter().any(|(_, d)| skrifa::FontRef::from_index(&d.font[..], d.index).is_ok_and(|f| f.charmap().map(ch).is_some()))
}

/// Adds the font at `path` as a fallback (first: before the others) if it has `ch`.
fn add_fallback(path: &Path, ch: char, first: bool) -> bool {
    let Some(bytes) = read_font_with(path, ch) else { return false };
    let mut v = FALLBACKS.lock().unwrap();
    if !v.iter().any(|(p, _)| p == path) {
        let entry = (path.to_path_buf(), Arc::new(egui::FontData::from_owned(bytes)));
        if first {
            v.insert(0, entry);
        } else {
            v.push(entry);
        }
    }
    true
}

/// Characters worth a look for a font: not ASCII, not invisible, not private-use ones (their
/// meaning depends on the font).
fn wanted(c: char) -> bool {
    !c.is_ascii() && !c.is_whitespace() && !c.is_control() && bigtext::text::rows::char_cells(c) > 0 && !matches!(c as u32, 0xE000..=0xF8FF | 0xFFF0..=0xFFFF | 0xF0000..=0x10FFFF)
}

fn is_kana(c: char) -> bool {
    matches!(c as u32, 0x3040..=0x30FF | 0x31F0..=0x31FF | 0xFF66..=0xFF9F)
}

/// Looks for system fonts with the characters of `text` that `font` shows as the replacement
/// glyph. Cheap when there are none; the looking is done on a worker, and the fonts found are
/// applied on a later frame (see [`refresh`]).
pub fn note_missing(ctx: &egui::Context, font: &egui::FontId, text: &str) {
    if text.is_ascii() {
        return;
    }
    // egui's `has_glyph` says no for every character of the font with the replacement glyph;
    // a missing character is one with no width.
    let missing: Vec<char> = ctx.fonts_mut(|f| text.chars().filter(|&c| wanted(c) && f.glyph_width(font, c) == 0.0).collect());
    if missing.is_empty() {
        return;
    }
    let mut l = LOOKUP.lock().unwrap();
    let asked = l.asked.get_or_insert_with(HashSet::new);
    let new: Vec<char> = missing.into_iter().filter(|&c| asked.insert(c)).collect();
    if new.is_empty() {
        return;
    }
    l.queue.extend(new);
    if !l.running {
        l.running = true;
        let ctx = ctx.clone();
        std::thread::spawn(move || find_fallbacks(&ctx));
    }
}

/// The worker of [`note_missing`]: for each character asked for, the first font that has it,
/// trying the list above and then the fonts in the font folders.
fn find_fallbacks(ctx: &egui::Context) {
    // Fonts are checked mapped, so a big one is read only once it is chosen.
    let mut maps: std::collections::HashMap<PathBuf, Option<memmap2::Mmap>> = Default::default();
    let mut scanned: Option<Vec<PathBuf>> = None;
    loop {
        let (batch, give_up) = {
            let mut l = LOOKUP.lock().unwrap();
            if l.queue.is_empty() {
                l.running = false;
                return;
            }
            (std::mem::take(&mut l.queue), l.misses > 200)
        };
        let japanese = batch.iter().any(|&c| is_kana(c));
        let mut added = false;
        for ch in batch {
            if fallbacks_have(ch) {
                continue;
            }
            let listed: Vec<PathBuf> = JAPANESE_FONTS.iter().filter(|_| japanese).chain(FALLBACK_FONTS).filter_map(|n| font_path(n)).collect();
            let others = if give_up { &[][..] } else { scanned.get_or_insert_with(folder_fonts).as_slice() };
            let has = |p: &PathBuf, maps: &mut std::collections::HashMap<PathBuf, Option<memmap2::Mmap>>| {
                let map = maps.entry(p.clone()).or_insert_with(|| {
                    let file = std::fs::File::open(p).ok()?;
                    // SAFETY: font files are only read; one changed meanwhile is checked again
                    // when it is read in full.
                    unsafe { memmap2::Mmap::map(&file) }.ok()
                });
                map.as_deref().and_then(|b| skrifa::FontRef::from_index(b, 0).ok()).is_some_and(|f| f.charmap().map(ch).is_some())
            };
            match listed.iter().chain(others).find(|p| has(p, &mut maps)) {
                Some(p) => added |= add_fallback(p, ch, false),
                None => LOOKUP.lock().unwrap().misses += 1,
            }
        }
        if added {
            FALLBACKS_CHANGED.store(true, Ordering::Release);
            ctx.request_repaint();
        }
    }
}

/// The regular fonts in the usual font folders, Noto first.
fn folder_fonts() -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
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
            let stem = file.rsplit_once('.').map_or(file.as_str(), |s| s.0);
            if (file.ends_with(".ttf") || file.ends_with(".otf") || file.ends_with(".ttc")) && !STYLES.iter().any(|s| stem.contains(s)) {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    for (dir, depth) in font_dirs() {
        walk(&dir, depth, &mut out);
    }
    out.sort_by_key(|p| (!p.to_string_lossy().to_lowercase().contains("noto"), p.clone()));
    out
}

/// The fallbacks found since the last frame are applied (with the text's font at `path`).
pub fn refresh(ctx: &egui::Context, path: Option<&Path>) {
    if FALLBACKS_CHANGED.swap(false, Ordering::AcqRel) {
        // A text font that went missing meanwhile gives way to the built-in one.
        if apply_font(ctx, path).is_err() {
            let _ = apply_font(ctx, None);
        }
        ctx.request_repaint();
    }
}

/// Installs the font at `path` as the first monospace font (none: the built-in one only), and
/// behind the others the system fonts found for other scripts; in Chinese one with its
/// characters comes first of those. Returns whether the font is monospaced.
pub fn apply_font(ctx: &egui::Context, path: Option<&Path>) -> Result<bool, String> {
    let mut defs = egui::FontDefinitions::default();
    // The built-in interface font (Ubuntu Light) has no arrows, so the → of "Help → Check for
    // updates…" and other texts showed as a box; the built-in Hack has them.
    defs.families.entry(egui::FontFamily::Proportional).or_default().push("Hack".into());
    let mut mono = true;
    if let Some(p) = path {
        let bytes = std::fs::read(p).map_err(|e| super::friendly_io(&e, p))?;
        mono = check_font(&bytes)?;
        defs.font_data.insert("user-mono".into(), Arc::new(egui::FontData::from_owned(bytes)));
        defs.families.entry(egui::FontFamily::Monospace).or_default().insert(0, "user-mono".into());
    }
    if super::i18n::lang() == super::i18n::Lang::Zh {
        // The menus need it at once.
        let mut v = FALLBACKS.lock().unwrap();
        match v.iter().position(|(p, _)| CJK_FONTS.iter().any(|f| font_path(f).as_deref() == Some(p.as_path()))) {
            Some(i) => {
                let cjk = v.remove(i);
                v.insert(0, cjk);
            }
            None => {
                drop(v);
                let _ = CJK_FONTS.iter().filter_map(|f| font_path(f)).any(|p| add_fallback(&p, '中', true));
            }
        }
    }
    for (i, (_, data)) in FALLBACKS.lock().unwrap().iter().enumerate() {
        let name = format!("fallback-{i}");
        defs.font_data.insert(name.clone(), data.clone());
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            defs.families.entry(family).or_default().push(name.clone());
        }
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
    fn which_characters_are_looked_for() {
        assert!(wanted('ส') && wanted('中') && wanted('ệ'));
        // ASCII, spaces, accents (drawn on their letter), private-use icons.
        assert!(!wanted('a') && !wanted('\u{3000}') && !wanted('\u{301}') && !wanted('\u{E0A0}'));
        assert!(is_kana('こ') && is_kana('ｶ') && !is_kana('中'));
    }

    #[test]
    fn checks_fonts() {
        assert!(check_font(b"definitely not a font").is_err());
        let builtin = egui::FontDefinitions::default();
        assert_eq!(check_font(&builtin.font_data["Hack"].font), Ok(true));
        assert_eq!(check_font(&builtin.font_data["Ubuntu-Light"].font), Ok(false));
    }

    #[test]
    fn interface_font_has_arrows() {
        let ctx = egui::Context::default();
        let width = |ctx: &egui::Context| {
            ctx.run_ui(Default::default(), |_| {}).textures_delta.clear();
            ctx.fonts_mut(|f| f.glyph_width(&egui::FontId::proportional(14.0), '→'))
        };
        assert_eq!(width(&ctx), 0.0, "egui's own interface font got arrows; the Hack fallback can go");
        apply_font(&ctx, None).unwrap();
        assert!(width(&ctx) > 0.0);
    }
}
