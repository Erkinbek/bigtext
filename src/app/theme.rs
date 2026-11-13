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

/// Switches to `theme`, or to the system's dark or light theme when `system`, with
/// colour-blind-safe colours when `cvd`.
pub fn apply(ctx: &egui::Context, theme: Theme, system: bool, cvd: bool) {
    if system {
        // egui picks the slot the system asks for each frame (Windows and macOS report it, also
        // when it changes); otherwise the fallback, from the Linux desktop or light.
        STATE.store(u8::from(cvd), Ordering::Relaxed);
        ctx.set_visuals_of(egui::Theme::Dark, Theme::Dark.visuals());
        ctx.set_visuals_of(egui::Theme::Light, Theme::Light.visuals());
        let fallback = fallback(std::time::Duration::from_millis(100));
        ctx.options_mut(|o| o.fallback_theme = fallback);
        ctx.set_theme(egui::ThemePreference::System);
        return;
    }
    STATE.store(u8::from(cvd) | (u8::from(theme.high_contrast()) << 1), Ordering::Relaxed);
    let t = if theme.is_dark() { egui::Theme::Dark } else { egui::Theme::Light };
    ctx.set_visuals_of(t, theme.visuals());
    ctx.set_theme(t);
}

/// The theme shown while BigText follows the system.
pub fn shown(ctx: &egui::Context) -> Theme {
    if ctx.theme() == egui::Theme::Dark {
        Theme::Dark
    } else {
        Theme::Light
    }
}

/// The theme for a system that does not say which it prefers: dark only when the Linux desktop
/// asks for it (waiting up to `wait` for [`probe_desktop`]), light otherwise.
pub fn fallback(wait: std::time::Duration) -> egui::Theme {
    if desktop_prefers_dark(wait) == Some(true) {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    }
}

#[cfg(target_os = "linux")]
static DESKTOP_DARK: std::sync::OnceLock<Option<bool>> = std::sync::OnceLock::new();
#[cfg(target_os = "linux")]
static PROBING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Starts asking the Linux desktop whether it prefers a dark theme: winit does not say on X11
/// or Wayland. Started before the window, so the answer is in before the first frame.
pub fn probe_desktop() {
    #[cfg(target_os = "linux")]
    if !PROBING.swap(true, Ordering::Relaxed) {
        std::thread::spawn(|| {
            let _ = DESKTOP_DARK.set(linux_desktop_dark());
        });
    }
}

#[cfg(target_os = "linux")]
fn desktop_prefers_dark(wait: std::time::Duration) -> Option<bool> {
    if !PROBING.load(Ordering::Relaxed) {
        return None;
    }
    let end = std::time::Instant::now() + wait;
    loop {
        if let Some(dark) = DESKTOP_DARK.get() {
            return *dark;
        }
        if std::time::Instant::now() >= end {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

#[cfg(not(target_os = "linux"))]
fn desktop_prefers_dark(_wait: std::time::Duration) -> Option<bool> {
    None
}

/// `GTK_THEME`, then the freedesktop portal (GNOME, KDE and most others), then GNOME's setting.
#[cfg(target_os = "linux")]
fn linux_desktop_dark() -> Option<bool> {
    use std::process::{Command, Stdio};
    if let Some(dark) = std::env::var("GTK_THEME").ok().and_then(|t| gtk_theme_dark(&t)) {
        return Some(dark);
    }
    let run = |cmd: &str, args: &[&str]| {
        Command::new(cmd).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
    };
    let portal = ["--session", "--print-reply=literal", "--reply-timeout=300", "--dest=org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop", "org.freedesktop.portal.Settings.Read", "string:org.freedesktop.appearance", "string:color-scheme"];
    if let Some(dark) = run("dbus-send", &portal).and_then(|out| portal_dark(&out)) {
        return Some(dark);
    }
    run("gsettings", &["get", "org.gnome.desktop.interface", "color-scheme"]).and_then(|out| gnome_dark(&out))
}

/// `GTK_THEME=Adwaita:dark` (or a `…-dark` theme) asks for dark; other values say nothing.
#[cfg(any(target_os = "linux", test))]
fn gtk_theme_dark(value: &str) -> Option<bool> {
    let v = value.to_ascii_lowercase();
    (v.ends_with(":dark") || v.ends_with("-dark")).then_some(true)
}

/// The portal's `color-scheme`: 1 prefers dark, 2 prefers light, 0 no preference.
#[cfg(any(target_os = "linux", test))]
fn portal_dark(reply: &str) -> Option<bool> {
    let n: u32 = reply.split_whitespace().skip_while(|w| *w != "uint32").nth(1)?.parse().ok()?;
    match n {
        1 => Some(true),
        2 => Some(false),
        _ => None,
    }
}

/// GNOME's `color-scheme`: `'prefer-dark'`, `'prefer-light'` or `'default'` (light).
#[cfg(any(target_os = "linux", test))]
fn gnome_dark(reply: &str) -> Option<bool> {
    let v = reply.trim().trim_matches('\'');
    match v {
        "prefer-dark" => Some(true),
        "prefer-light" | "default" => Some(false),
        _ => None,
    }
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

/// View → Theme → Code colours: the palette of code files' text areas, apart from the
/// interface's theme. `Auto` follows it (One Dark on dark themes, One Light on light ones).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum CodeTheme {
    #[default]
    Auto,
    OneDark,
    OneLight,
    Monokai,
    GitHubDark,
    GitHubLight,
    SolarizedDark,
    SolarizedLight,
}

impl CodeTheme {
    pub const ALL: [CodeTheme; 8] = [CodeTheme::Auto, CodeTheme::OneDark, CodeTheme::OneLight, CodeTheme::Monokai, CodeTheme::GitHubDark, CodeTheme::GitHubLight, CodeTheme::SolarizedDark, CodeTheme::SolarizedLight];

    /// Stable name in the settings and key bindings.
    pub fn id(self) -> &'static str {
        match self {
            CodeTheme::Auto => "auto",
            CodeTheme::OneDark => "one-dark",
            CodeTheme::OneLight => "one-light",
            CodeTheme::Monokai => "monokai",
            CodeTheme::GitHubDark => "github-dark",
            CodeTheme::GitHubLight => "github-light",
            CodeTheme::SolarizedDark => "solarized-dark",
            CodeTheme::SolarizedLight => "solarized-light",
        }
    }

    pub fn from_id(id: &str) -> Option<CodeTheme> {
        CodeTheme::ALL.into_iter().find(|t| t.id() == id)
    }

    /// The name (translate `Auto`'s with `tr`; the others are names).
    pub fn label(self) -> &'static str {
        match self {
            CodeTheme::Auto => "As the theme (One Dark / One Light)",
            CodeTheme::OneDark => "One Dark",
            CodeTheme::OneLight => "One Light",
            CodeTheme::Monokai => "Monokai",
            CodeTheme::GitHubDark => "GitHub Dark",
            CodeTheme::GitHubLight => "GitHub Light",
            CodeTheme::SolarizedDark => "Solarized Dark",
            CodeTheme::SolarizedLight => "Solarized Light",
        }
    }

    /// `Auto` made concrete for an interface that is `dark` or not.
    pub fn resolve(self, dark: bool) -> CodeTheme {
        match self {
            CodeTheme::Auto if dark => CodeTheme::OneDark,
            CodeTheme::Auto => CodeTheme::OneLight,
            t => t,
        }
    }

    pub fn is_dark(self) -> bool {
        matches!(self, CodeTheme::OneDark | CodeTheme::Monokai | CodeTheme::GitHubDark | CodeTheme::SolarizedDark)
    }
}

static CODE_THEME: AtomicU8 = AtomicU8::new(0);

pub fn set_code_theme(t: CodeTheme) {
    let i = CodeTheme::ALL.iter().position(|&x| x == t).unwrap_or(0);
    CODE_THEME.store(i as u8, Ordering::Relaxed);
}

pub fn code_theme() -> CodeTheme {
    CodeTheme::ALL.get(CODE_THEME.load(Ordering::Relaxed) as usize).copied().unwrap_or_default()
}

/// The text area of a code file, like a code editor's.
pub struct CodeColors {
    pub bg: Color32,
    /// Text that is not a token with its own colour (names, operators).
    pub fg: Color32,
    pub line_no: Color32,
    /// The caret's line number.
    pub line_no_active: Color32,
    /// Behind the caret's line.
    pub cur_line: Color32,
    pub guide: Color32,
    pub caret: Color32,
    /// The palette is a dark one (whatever the interface is).
    pub dark: bool,
}

fn hex(v: u32) -> Color32 {
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

/// The text area's colours with the chosen code colours, for an interface that is `dark`.
pub fn code_colors(dark: bool) -> CodeColors {
    let t = code_theme().resolve(dark);
    // bg, fg, line numbers, the caret's line number, its line, guides, caret
    let [bg, fg, line_no, active, cur, guide, caret] = match t {
        CodeTheme::OneDark | CodeTheme::Auto => [0x23272e, 0xabb2bf, 0x5f687a, 0xc8cdd7, 0x2c313c, 0x404650, 0xc8cdd7],
        CodeTheme::OneLight => [0xfafafa, 0x383a42, 0x9d9d9f, 0x383a42, 0xf0f0f2, 0xdedee0, 0x526fff],
        CodeTheme::Monokai => [0x272822, 0xf8f8f2, 0x90908a, 0xf8f8f2, 0x3e3d32, 0x464741, 0xf8f8f0],
        CodeTheme::GitHubDark => [0x0d1117, 0xe6edf3, 0x6e7681, 0xe6edf3, 0x161b22, 0x21262d, 0xe6edf3],
        CodeTheme::GitHubLight => [0xffffff, 0x1f2328, 0x8c959f, 0x1f2328, 0xf6f8fa, 0xd0d7de, 0x1f2328],
        CodeTheme::SolarizedDark => [0x002b36, 0x839496, 0x586e75, 0x93a1a1, 0x073642, 0x0e4250, 0x93a1a1],
        CodeTheme::SolarizedLight => [0xfdf6e3, 0x657b83, 0x93a1a1, 0x586e75, 0xeee8d5, 0xe4ddc8, 0x586e75],
    };
    CodeColors { bg: hex(bg), fg: hex(fg), line_no: hex(line_no), line_no_active: hex(active), cur_line: hex(cur), guide: hex(guide), caret: hex(caret), dark: t.is_dark() }
}

/// A token's colour with the chosen code colours (`None`: a kind code does not use, such as a
/// log level).
pub fn code_token(kind: crate::view::highlight::Kind, dark: bool) -> Option<Color32> {
    use crate::view::highlight::Kind;
    let t = code_theme().resolve(dark);
    // keyword, string, number, comment, type, function, variable, decorator, tag, attribute, key
    let p: [u32; 11] = match t {
        CodeTheme::OneDark | CodeTheme::Auto => [0xc678dd, 0x98c379, 0xd19a66, 0x7f848e, 0xe5c07b, 0x61afef, 0xe06c75, 0x56b6c2, 0xe06c75, 0xd19a66, 0xe06c75],
        // One Light, darkened to read well on white.
        CodeTheme::OneLight => [0x9818a0, 0x28781e, 0xaa5a00, 0x787878, 0x966400, 0x285fd2, 0xc83228, 0x0078a0, 0xc83228, 0xaa5a00, 0xc83228],
        CodeTheme::Monokai => [0xf92672, 0xe6db74, 0xae81ff, 0x88846f, 0x66d9ef, 0xa6e22e, 0xfd971f, 0xa6e22e, 0xf92672, 0xa6e22e, 0xf92672],
        CodeTheme::GitHubDark => [0xff7b72, 0xa5d6ff, 0x79c0ff, 0x8b949e, 0xffa657, 0xd2a8ff, 0xffa657, 0xd2a8ff, 0x7ee787, 0x79c0ff, 0x7ee787],
        CodeTheme::GitHubLight => [0xcf222e, 0x0a3069, 0x0550ae, 0x59636e, 0x953800, 0x8250df, 0x953800, 0x8250df, 0x116329, 0x0550ae, 0x116329],
        CodeTheme::SolarizedDark | CodeTheme::SolarizedLight => [0x859900, 0x2aa198, 0xd33682, 0x6c8a91, 0xb58900, 0x268bd2, 0xcb4b16, 0x6c71c4, 0x268bd2, 0xb58900, 0x268bd2],
    };
    let i = match kind {
        Kind::Keyword => 0,
        Kind::Str => 1,
        Kind::Number => 2,
        Kind::Comment => 3,
        Kind::Type => 4,
        Kind::Function => 5,
        Kind::Var => 6,
        Kind::Decorator => 7,
        Kind::Tag => 8,
        Kind::Attr => 9,
        Kind::Key => 10,
        _ => return None,
    };
    Some(hex(p[i]))
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
        Kind::Type => ((86, 180, 233), (0, 100, 170)),
        Kind::Function => ((240, 228, 66), (130, 110, 0)),
        Kind::Var => ((230, 110, 30), (200, 80, 0)),
        Kind::Decorator => ((204, 121, 167), (160, 60, 120)),
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

    #[test]
    fn desktop_answers_are_read() {
        assert_eq!(portal_dark("   variant       variant          uint32 1\n"), Some(true));
        assert_eq!(portal_dark("   variant       uint32 2\n"), Some(false));
        assert_eq!(portal_dark("   variant       variant          uint32 0\n"), None);
        assert_eq!(portal_dark(""), None);
        assert_eq!(gnome_dark("'prefer-dark'\n"), Some(true));
        assert_eq!(gnome_dark("'default'\n"), Some(false));
        assert_eq!(gnome_dark("No such key\n"), None);
        assert_eq!(gtk_theme_dark("Adwaita:dark"), Some(true));
        assert_eq!(gtk_theme_dark("Arc-Dark"), Some(true));
        assert_eq!(gtk_theme_dark("Adwaita"), None);
    }

    #[test]
    fn following_the_system_without_an_answer_is_light() {
        let ctx = egui::Context::default();
        apply(&ctx, Theme::HighContrastDark, true, false);
        assert_eq!(ctx.options(|o| o.theme_preference), egui::ThemePreference::System);
        assert_eq!(shown(&ctx), Theme::Light);
        assert!(!high_contrast_on());
        apply(&ctx, Theme::Dark, false, false);
        assert_eq!(shown(&ctx), Theme::Dark);
    }
}
