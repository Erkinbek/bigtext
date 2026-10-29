//! Themes: Dark, Light and two high-contrast ones, colour-blind-safe colours (Okabe-Ito) for
//! everything BigText paints itself, and the interface scale.
//!
//! The stock themes keep their colours: a painted colour goes through [`role`], which only
//! changes it when colour-blind-safe colours or high contrast are on.

use std::sync::atomic::{AtomicU8, Ordering};

use egui::{Color32, Stroke, Visuals};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Theme {
    #[default]
    Dark,
    Light,
    HighContrastDark,
    HighContrastLight,
}

impl Theme {
    pub const ALL: [Theme; 4] = [Theme::Dark, Theme::Light, Theme::HighContrastDark, Theme::HighContrastLight];

    /// Stable name in the settings and key bindings.
    pub fn id(self) -> &'static str {
        match self {
            Theme::Dark => "dark",
            Theme::Light => "light",
            Theme::HighContrastDark => "hc-dark",
            Theme::HighContrastLight => "hc-light",
        }
    }

    pub fn from_id(id: &str) -> Option<Theme> {
        Theme::ALL.into_iter().find(|t| t.id() == id)
    }

    /// English label (translate with `tr`).
    pub fn label(self) -> &'static str {
        match self {
            Theme::Dark => "Dark",
            Theme::Light => "Light",
            Theme::HighContrastDark => "High contrast dark",
            Theme::HighContrastLight => "High contrast light",
        }
    }

    pub fn is_dark(self) -> bool {
        matches!(self, Theme::Dark | Theme::HighContrastDark)
    }

    pub fn high_contrast(self) -> bool {
        matches!(self, Theme::HighContrastDark | Theme::HighContrastLight)
    }

    /// The same contrast the other way round (View → Dark theme).
    pub fn flipped(self) -> Theme {
        match self {
            Theme::Dark => Theme::Light,
            Theme::Light => Theme::Dark,
            Theme::HighContrastDark => Theme::HighContrastLight,
            Theme::HighContrastLight => Theme::HighContrastDark,
        }
    }

    /// Built fresh each time: `set_visuals_of` keeps what it is given in the dark or light slot.
    pub fn visuals(self) -> Visuals {
        match self {
            Theme::Dark => Visuals::dark(),
            Theme::Light => Visuals::light(),
            Theme::HighContrastDark => high_contrast(Visuals::dark(), Color32::WHITE, Color32::BLACK, Color32::from_rgb(255, 210, 0), Color32::from_rgb(0, 90, 200)),
            Theme::HighContrastLight => high_contrast(Visuals::light(), Color32::BLACK, Color32::WHITE, Color32::from_rgb(0, 70, 200), Color32::from_rgb(0, 70, 200)),
        }
    }
}

/// Pure foreground on pure background, 1.5 px strokes, a bright focus colour and an opaque
/// selection.
fn high_contrast(mut v: Visuals, fg: Color32, bg: Color32, focus: Color32, selection: Color32) -> Visuals {
    let dark = v.dark_mode;
    let near = |x: u8| if dark { Color32::from_gray(x) } else { Color32::from_gray(255 - x) };
    v.override_text_color = Some(fg);
    v.weak_text_color = Some(near(200));
    v.panel_fill = bg;
    v.window_fill = bg;
    v.extreme_bg_color = bg;
    v.text_edit_bg_color = Some(bg);
    v.faint_bg_color = near(28);
    v.code_bg_color = near(24);
    v.window_stroke = Stroke::new(1.5, fg);
    v.hyperlink_color = if dark { Color32::from_rgb(120, 200, 255) } else { Color32::from_rgb(0, 60, 190) };
    v.warn_fg_color = if dark { Color32::from_rgb(255, 200, 0) } else { Color32::from_rgb(150, 90, 0) };
    v.error_fg_color = if dark { Color32::from_rgb(255, 120, 90) } else { Color32::from_rgb(190, 0, 0) };
    v.selection.bg_fill = selection;
    v.selection.stroke = Stroke::new(1.5, Color32::WHITE);
    let w = &mut v.widgets;
    for x in [&mut w.noninteractive, &mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
        x.fg_stroke = Stroke::new(1.5, fg);
    }
    w.noninteractive.bg_fill = bg;
    w.noninteractive.weak_bg_fill = bg;
    w.noninteractive.bg_stroke = Stroke::new(1.0, near(170));
    w.inactive.bg_fill = near(20);
    w.inactive.weak_bg_fill = near(20);
    w.inactive.bg_stroke = Stroke::new(1.5, near(190));
    for x in [&mut w.hovered, &mut w.active, &mut w.open] {
        x.bg_fill = near(45);
        x.weak_bg_fill = near(45);
        x.bg_stroke = Stroke::new(2.0, focus);
    }
    v
}

/// Bit 0: colour-blind-safe colours, bit 1: high contrast.
static STATE: AtomicU8 = AtomicU8::new(0);

pub fn cvd() -> bool {
    STATE.load(Ordering::Relaxed) & 1 != 0
}

pub fn high_contrast_on() -> bool {
    STATE.load(Ordering::Relaxed) & 2 != 0
}

/// Switches to `theme`, with colour-blind-safe colours when `cvd`.
pub fn apply(ctx: &egui::Context, theme: Theme, cvd: bool) {
    STATE.store(u8::from(cvd) | (u8::from(theme.high_contrast()) << 1), Ordering::Relaxed);
    let t = if theme.is_dark() { egui::Theme::Dark } else { egui::Theme::Light };
    ctx.set_visuals_of(t, theme.visuals());
    ctx.set_theme(t);
}

/// Interface scales offered (per cent).
pub const SCALES: [u16; 5] = [100, 125, 150, 175, 200];

/// What a painted colour means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// Valid, same, added, following.
    Ok,
    /// Stale results, read-only, a warning.
    Warn,
    /// Removed, different.
    Bad,
    /// Filters, tables, file names.
    Info,
    /// Search hits.
    Found,
    /// The caret's place on the scrollbar.
    Caret,
}

/// Okabe-Ito colours (orange, sky blue, bluish green, yellow, blue, vermillion, reddish purple)
/// for dark and light backgrounds; on light ones the light colours are darkened to stay readable.
fn cvd_tone(t: Tone, dark: bool) -> (u8, u8, u8) {
    match (t, dark) {
        (Tone::Ok, true) => (0, 170, 125),
        (Tone::Ok, false) => (0, 115, 80),
        (Tone::Warn, true) => (230, 159, 0),
        (Tone::Warn, false) => (170, 100, 0),
        (Tone::Bad, true) => (230, 110, 30),
        (Tone::Bad, false) => (200, 80, 0),
        (Tone::Info, true) => (86, 180, 233),
        (Tone::Info, false) => (0, 100, 170),
        (Tone::Found, true) => (240, 228, 66),
        (Tone::Found, false) => (180, 140, 0),
        (Tone::Caret, true) => (200, 200, 255),
        (Tone::Caret, false) => (0, 70, 140),
    }
}

/// Further from the background: lighter on dark, darker on light.
pub fn contrast(c: Color32, dark: bool) -> Color32 {
    let f = |x: u8| if dark { x + ((255 - x) as u16 * 3 / 10) as u8 } else { (x as u16 * 6 / 10) as u8 };
    Color32::from_rgb(f(c.r()), f(c.g()), f(c.b()))
}

/// Stronger fills: translucent ones fade on pure black and white.
fn fill_alpha(a: u8) -> u8 {
    if high_contrast_on() && a < 255 {
        (a as u16 * 3 / 2).min(230) as u8
    } else {
        a
    }
}

/// `normal` in the stock themes; with colour-blind-safe colours the Okabe-Ito colour of its
/// role `t`, and with high contrast further from the background (fills more opaque).
pub fn role(normal: Color32, t: Tone, dark: bool) -> Color32 {
    if STATE.load(Ordering::Relaxed) == 0 {
        return normal;
    }
    let [r, g, b, a] = normal.to_srgba_unmultiplied();
    let (r, g, b) = if cvd() { cvd_tone(t, dark) } else { (r, g, b) };
    let mut c = Color32::from_rgb(r, g, b);
    if high_contrast_on() && a == 255 {
        c = contrast(c, dark);
    }
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), fill_alpha(a))
}

/// Text that warns ("stale", READ-ONLY).
pub fn warn_text(ui: &egui::Ui) -> Color32 {
    role(Color32::from_rgb(220, 150, 60), Tone::Warn, ui.visuals().dark_mode)
}

/// Text that says all is well (valid, same, a finished task).
pub fn ok_text(ui: &egui::Ui) -> Color32 {
    role(Color32::from_rgb(90, 180, 90), Tone::Ok, ui.visuals().dark_mode)
}

/// Text that points somewhere (a column, a file, TABLE).
pub fn info_text(ui: &egui::Ui) -> Color32 {
    role(Color32::from_rgb(90, 160, 220), Tone::Info, ui.visuals().dark_mode)
}

/// The colours of highlight marks and merged logs: Okabe-Ito with colour-blind-safe colours.
pub fn mark_rgb(i: usize, normal: &[(u8, u8, u8); 8]) -> (u8, u8, u8) {
    const CVD: [(u8, u8, u8); 8] = [(230, 159, 0), (86, 180, 233), (0, 158, 115), (240, 228, 66), (0, 114, 178), (213, 94, 0), (204, 121, 167), (150, 150, 150)];
    if cvd() {
        CVD[i % 8]
    } else {
        normal[i % 8]
    }
}

/// The alpha of a mark's fill.
pub fn mark_alpha(dark: bool) -> u8 {
    fill_alpha(if dark { 95 } else { 110 })
}

/// Syntax colours with colour-blind-safe colours: levels differ in lightness as well (ERROR
/// vermillion, WARN yellow, INFO blue).
pub fn cvd_syntax(kind: crate::view::highlight::Kind, dark: bool) -> (u8, u8, u8) {
    use crate::view::highlight::Kind;
    let (d, l) = match kind {
        Kind::Keyword => ((204, 121, 167), (160, 60, 120)),
        Kind::Str => ((100, 200, 160), (0, 128, 95)),
        Kind::Number => ((230, 159, 0), (170, 100, 0)),
        Kind::Comment => ((128, 128, 128), (110, 110, 110)),
        Kind::Tag => ((86, 180, 233), (0, 100, 170)),
        Kind::Attr => ((240, 228, 66), (130, 110, 0)),
        Kind::Key => ((86, 180, 233), (0, 100, 170)),
        Kind::Error => ((230, 110, 30), (200, 80, 0)),
        Kind::Warn => ((240, 228, 66), (150, 120, 0)),
        Kind::Info => ((86, 180, 233), (0, 100, 170)),
        Kind::Debug => ((128, 128, 128), (120, 120, 120)),
        Kind::Time => ((0, 158, 115), (0, 110, 80)),
        Kind::Punct => ((171, 178, 191), (90, 90, 90)),
        Kind::Col(i) => {
            const DARK: [(u8, u8, u8); 6] = [(230, 159, 0), (86, 180, 233), (0, 158, 115), (204, 121, 167), (240, 228, 66), (230, 110, 30)];
            const LIGHT: [(u8, u8, u8); 6] = [(170, 100, 0), (0, 100, 170), (0, 115, 80), (160, 60, 120), (130, 110, 0), (200, 80, 0)];
            (DARK[i as usize % 6], LIGHT[i as usize % 6])
        }
    };
    if dark {
        d
    } else {
        l
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_and_flip_keeps_contrast() {
        for t in Theme::ALL {
            assert_eq!(Theme::from_id(t.id()), Some(t));
            assert_eq!(t.flipped().high_contrast(), t.high_contrast());
            assert_ne!(t.flipped().is_dark(), t.is_dark());
            assert_eq!(t.visuals().dark_mode, t.is_dark());
        }
        let v = Theme::HighContrastLight.visuals();
        assert_eq!((v.panel_fill, v.override_text_color), (Color32::WHITE, Some(Color32::BLACK)));
    }

    #[test]
    fn contrast_moves_away_from_the_background() {
        let c = Color32::from_rgb(100, 150, 200);
        let d = contrast(c, true);
        let l = contrast(c, false);
        assert!(d.r() > c.r() && d.g() > c.g() && d.b() > c.b());
        assert!(l.r() < c.r() && l.g() < c.g() && l.b() < c.b());
        assert_eq!(contrast(Color32::WHITE, true), Color32::WHITE);
    }
}
