//! The page over an empty start (what to open, a 1 GB sample log to try BigText on, the main
//! keys) and the note after an update (what changed in this version, from CHANGELOG.md).

use std::io::{BufWriter, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bigtext::update::{parse_notes, Block, Version};
use egui::RichText;

use super::commands::Cmd;
use super::i18n::{tr, trf};
use super::BigTextApp;

/// The release notes of every version; the current one's are shown after an update.
const CHANGELOG: &str = include_str!("../../CHANGELOG.md");

/// Size of the sample log.
pub const SAMPLE_SIZE: u64 = 1 << 30;

pub struct Welcome {
    /// On in normal runs; the self-test, the benchmarks and the screenshot tour start without it.
    pub enabled: bool,
    /// The sample log being written.
    pub sample: Option<SampleJob>,
    pub sample_size: u64,
    /// Where the sample goes (the self-test's own folder; otherwise the temporary folder).
    pub sample_path: Option<PathBuf>,
    /// The versions since the one used before and their notes, newest first, while the note is
    /// open.
    pub whats_new: Option<Vec<(Version, Vec<Block>)>>,
}

/// Settings without the version that saved them come from before 0.8.0, which began noting it.
const FIRST_NOTING: Version = Version(0, 8, 0);
/// The note tells about at most this many versions.
const MAX_VERSIONS: usize = 8;

impl Default for Welcome {
    fn default() -> Welcome {
        Welcome {
            enabled: false,
            sample: None,
            sample_size: SAMPLE_SIZE,
            sample_path: None,
            whats_new: None,
        }
    }
}

pub struct SampleJob {
    pub path: PathBuf,
    pub size: u64,
    pub written: Arc<AtomicU64>,
    pub done: Arc<AtomicBool>,
    pub error: Arc<Mutex<Option<String>>>,
}

impl Welcome {
    /// At start, with the saved settings: after an update (settings saved by an older version,
    /// or by one that did not note its version yet) the note says what changed in every version
    /// since. A first start shows the page instead.
    pub fn after_start(&mut self, saved_before: bool, seen: Option<&str>) {
        let notes = changes(
            CHANGELOG,
            saved_before,
            seen.and_then(Version::parse),
            Version::current(),
        );
        if !notes.is_empty() {
            self.whats_new = Some(notes);
        }
    }
}

/// The versions in `md` (CHANGELOG.md) after `seen` up to `current`, with their notes, newest
/// first; without `seen`, settings saved before mean an update from before [`FIRST_NOTING`].
pub fn changes(
    md: &str,
    saved_before: bool,
    seen: Option<Version>,
    current: Version,
) -> Vec<(Version, Vec<Block>)> {
    let newer = |v: Version| match seen {
        Some(s) => v > s,
        None => saved_before && v >= FIRST_NOTING,
    };
    versions(md)
        .filter(|&v| v <= current && newer(v))
        .filter_map(|v| {
            section(md, v)
                .map(parse_notes)
                .filter(|b| !b.is_empty())
                .map(|b| (v, b))
        })
        .take(MAX_VERSIONS)
        .collect()
}

/// The versions with a "## X.Y.Z" heading in `md`, in their order (newest first).
fn versions(md: &str) -> impl Iterator<Item = Version> + '_ {
    md.lines()
        .filter_map(|l| l.strip_prefix("## "))
        .filter_map(|t| t.split_whitespace().next().and_then(Version::parse))
}

/// The section of `md` (CHANGELOG.md) for version `v`: from its "## X.Y.Z" heading to the next
/// one, without the heading.
pub fn section(md: &str, v: Version) -> Option<&str> {
    let mut start = None;
    let mut pos = 0;
    for line in md.split_inclusive('\n') {
        if let Some(title) = line.strip_prefix("## ") {
            if start.is_some() {
                return start.map(|s| md[s..pos].trim());
            }
            let ver = title.split_whitespace().next().and_then(Version::parse);
            if ver == Some(v) {
                start = Some(pos + line.len());
            }
        }
        pos += line.len();
    }
    start.map(|s| md[s..].trim())
}

pub fn default_sample_path() -> PathBuf {
    std::env::temp_dir().join("bigtext-sample-1gb.log")
}

/// Writes a web-service log of at least `size` bytes to `path` (through a `.part` file, so a
/// half-written sample is never opened), counting the bytes in `written`.
pub fn write_sample(path: &Path, size: u64, written: &AtomicU64) -> std::io::Result<()> {
    const PATHS: [&str; 6] = [
        "/api/orders",
        "/api/users",
        "/api/cart",
        "/api/search",
        "/health",
        "/static/app.js",
    ];
    const WORDS: [&str; 8] = [
        "cache", "retry", "timeout", "worker", "session", "upstream", "queue", "payload",
    ];
    let part = path.with_extension("part");
    let mut out = BufWriter::with_capacity(1 << 20, std::fs::File::create(&part)?);
    let (mut seed, mut ms, mut id, mut n) =
        (0x9E37_79B9_7F4A_7C15u64, 8 * 3_600_000u64, 100_000u64, 0u64);
    let mut rand = |m: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % m
    };
    let mut line = String::with_capacity(160);
    while n < size {
        ms += rand(25);
        id += 1;
        let day = 1 + ms / 86_400_000 % 28;
        let t = ms % 86_400_000;
        line.clear();
        use std::fmt::Write as _;
        let _ = write!(
            line,
            "2026-11-{day:02} {:02}:{:02}:{:02}.{:03} ",
            t / 3_600_000,
            t / 60_000 % 60,
            t / 1000 % 60,
            t % 1000
        );
        let _ = match rand(100) {
            0..=1 => writeln!(
                line,
                "ERROR [worker-{}] {} {} failed for order={id}: upstream returned 503",
                rand(16),
                WORDS[rand(8) as usize],
                WORDS[rand(8) as usize]
            ),
            2..=7 => writeln!(
                line,
                "WARN  [worker-{}] slow request {}ms {} id={id}",
                rand(16),
                400 + rand(2_000),
                PATHS[rand(6) as usize]
            ),
            8..=22 => writeln!(
                line,
                "DEBUG [worker-{}] {} hit key=user:{} ttl={}s",
                rand(16),
                WORDS[rand(8) as usize],
                rand(1_000_000),
                rand(3_600)
            ),
            _ => writeln!(
                line,
                "INFO  [worker-{}] GET {}/{id} 200 {}ms user={}",
                rand(16),
                PATHS[rand(6) as usize],
                1 + rand(80),
                rand(50_000)
            ),
        };
        out.write_all(line.as_bytes())?;
        n += line.len() as u64;
        if id % 65_536 == 0 {
            written.store(n, Ordering::Relaxed);
        }
    }
    out.flush()?;
    drop(out);
    written.store(n, Ordering::Relaxed);
    std::fs::rename(&part, path)
}

impl BigTextApp {
    /// The page shows over the empty document of a fresh start, until it gets text or a file
    /// is opened.
    pub(crate) fn welcome_visible(&self) -> bool {
        match self.docs.as_slice() {
            [d] => {
                self.welcome.enabled
                    && d.path.is_none()
                    && d.buf.is_empty()
                    && !d.is_modified()
                    && !d.hex
                    && !d.table
            }
            _ => false,
        }
    }

    /// Writes the sample log on a worker (or opens the one written before).
    pub(crate) fn start_sample(&mut self) {
        if self.welcome.sample.is_some() {
            return;
        }
        let path = self
            .welcome
            .sample_path
            .clone()
            .unwrap_or_else(default_sample_path);
        let size = self.welcome.sample_size;
        if std::fs::metadata(&path).is_ok_and(|m| m.len() >= size) {
            self.open_path(path);
            return;
        }
        let job = SampleJob {
            path: path.clone(),
            size,
            written: Arc::new(AtomicU64::new(0)),
            done: Arc::new(AtomicBool::new(false)),
            error: Arc::new(Mutex::new(None)),
        };
        let (written, done, error, waker) = (
            job.written.clone(),
            job.done.clone(),
            job.error.clone(),
            self.waker.clone(),
        );
        bigtext::jobs::spawn("sample-log", move || {
            if let Err(e) = write_sample(&path, size, &written) {
                *error.lock().unwrap() = Some(e.to_string());
            }
            done.store(true, Ordering::Release);
            waker();
        });
        self.welcome.sample = Some(job);
    }

    /// Opens the sample once it is written.
    pub(super) fn welcome_tick(&mut self) {
        let Some(job) = &self.welcome.sample else {
            return;
        };
        if !job.done.load(Ordering::Acquire) {
            return;
        }
        let job = self.welcome.sample.take().unwrap();
        let error = job.error.lock().unwrap().take();
        match error {
            Some(e) => self.error = Some(trf("Could not write the sample log: {e}", &[("e", &e)])),
            None => self.open_path(job.path),
        }
    }

    /// The page, centred over `rect` (the editor's area).
    pub(super) fn welcome_ui(&mut self, ctx: &egui::Context, rect: egui::Rect) {
        let mut run = None;
        let mut open = None;
        let mut sample = false;
        let width = (rect.width() - 48.0).clamp(280.0, 600.0);
        let key = |c: Cmd| self.keymap.get(c).map(|k| ctx.format_shortcut(&k));
        egui::Area::new(egui::Id::new("welcome")).order(egui::Order::Middle).pivot(egui::Align2::CENTER_CENTER).fixed_pos(rect.center()).show(ctx, |ui| {
            egui::Frame::popup(ui.style()).inner_margin(22.0).show(ui, |ui| {
                ui.set_width(width);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("BigText").size(26.0).strong());
                    ui.weak(Version::current().to_string());
                });
                ui.label(tr("A text editor for huge files: it opens 1–50+ GB at once and never freezes."));
                ui.add_space(10.0);
                ui.horizontal_wrapped(|ui| {
                    for c in [Cmd::Open, Cmd::OpenFolder, Cmd::OpenRemote] {
                        let b = ui.button(tr(c.label()));
                        let b = match key(c) {
                            Some(k) => b.on_hover_text(k),
                            None => b,
                        };
                        if b.clicked() {
                            run = Some(c);
                        }
                    }
                });
                if !self.recent.is_empty() {
                    ui.add_space(10.0);
                    ui.label(RichText::new(tr("Open recent")).strong());
                    for p in self.recent.iter().take(5) {
                        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string());
                        ui.horizontal(|ui| {
                            if ui.link(name).clicked() {
                                open = Some(p.clone());
                            }
                            if let Some(dir) = p.parent() {
                                ui.weak(dir.display().to_string());
                            }
                        });
                    }
                }
                ui.add_space(8.0);
                ui.separator();
                match &self.welcome.sample {
                    None => {
                        ui.label(tr("No big file at hand? BigText can write a 1 GB sample log to try it on."));
                        if ui.button(tr("Create a 1 GB sample log")).clicked() {
                            sample = true;
                        }
                        let dir = self.welcome.sample_path.clone().unwrap_or_else(default_sample_path);
                        let dir = dir.parent().map(|d| d.display().to_string()).unwrap_or_default();
                        ui.weak(trf("It goes to {dir} and opens when it is written.", &[("dir", &dir)]));
                    }
                    Some(job) => {
                        let f = job.written.load(Ordering::Relaxed) as f32 / job.size.max(1) as f32;
                        let pct = format!("{:.0}", f.min(1.0) * 100.0);
                        ui.add(egui::ProgressBar::new(f.min(1.0)).text(trf("Writing the sample log… {p}%", &[("p", &pct)])));
                        ctx.request_repaint_after(Duration::from_millis(100));
                    }
                }
                ui.add_space(8.0);
                ui.separator();
                egui::Grid::new("welcome-keys").num_columns(2).spacing([18.0, 4.0]).show(ui, |ui| {
                    for c in [Cmd::Find, Cmd::FindInFiles, Cmd::GotoLine, Cmd::GotoFile, Cmd::Palette] {
                        if let Some(k) = key(c) {
                            ui.label(RichText::new(k).monospace());
                            ui.label(tr(c.label()));
                            ui.end_row();
                        }
                    }
                });
                ui.add_space(6.0);
                ui.weak(tr("Or just start typing: this page goes away by itself."));
            });
        });
        if let Some(c) = run {
            self.run(ctx, c);
        }
        if let Some(p) = open {
            self.open_path(p);
        }
        if sample {
            self.start_sample();
        }
    }

    /// The note after an update: what changed in each version since the one used before.
    pub(super) fn whats_new_ui(&mut self, ctx: &egui::Context) {
        let Some(notes) = &self.welcome.whats_new else {
            return;
        };
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::new("whats-new")).show(ctx, |ui| {
            ui.set_width(560.0);
            super::update::versions_ui(ui, notes, 420.0);
            ui.add_space(8.0);
            // One version: its release; several: the list of releases.
            let page = match notes.as_slice() {
                [(v, _)] => format!("https://github.com/Erkinbek/bigtext/releases/tag/v{v}"),
                _ => "https://github.com/Erkinbek/bigtext/releases".to_owned(),
            };
            ui.horizontal(|ui| {
                ui.hyperlink_to(tr("Full release notes on GitHub"), page);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    close = ui.button(tr("Close")).clicked();
                });
            });
        });
        if close || modal.should_close() {
            self.welcome.whats_new = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_current_version_has_notes() {
        let notes = section(CHANGELOG, Version::current())
            .map(parse_notes)
            .expect("CHANGELOG.md has a section for this version");
        assert!(
            notes.iter().any(|b| matches!(b, Block::Item(..))),
            "{notes:?}"
        );
    }

    #[test]
    fn every_version_since_the_one_used_before() {
        let md = "# Changelog\n\n## 0.9.0 — next\n\n- not out\n\n## 0.8.2\n\n- c\n\n## 0.8.1\n\n- b\n\n## 0.8.0\n\n- a\n\n## 0.7.3\n\n- old\n";
        let got = |saved: bool, seen: Option<Version>| {
            changes(md, saved, seen, Version(0, 8, 2))
                .into_iter()
                .map(|(v, _)| v.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(got(true, Some(Version(0, 8, 0))), ["0.8.2", "0.8.1"]);
        assert_eq!(got(true, Some(Version(0, 8, 1))), ["0.8.2"]);
        assert!(got(true, Some(Version(0, 8, 2))).is_empty());
        // Saved by a version before 0.8.0 (no version in the settings): 0.8.0 and later.
        assert_eq!(got(true, None), ["0.8.2", "0.8.1", "0.8.0"]);
        assert!(got(false, None).is_empty());
        // Many versions skipped: the newest few.
        let long: String = (0..20)
            .rev()
            .map(|p| format!("## 1.0.{p}\n\n- n\n\n"))
            .collect();
        assert_eq!(
            changes(&long, true, Some(Version(0, 9, 0)), Version(1, 0, 19)).len(),
            MAX_VERSIONS
        );
    }

    #[test]
    fn finds_a_version_section() {
        let md = "# Changelog\n\nIntro.\n\n## 1.2.0 — 2026-01-02\n\n### Fixes\n\n- **A**: one\n\n## 1.1.0 — 2026-01-01\n\n- old\n";
        assert_eq!(
            section(md, Version(1, 2, 0)),
            Some("### Fixes\n\n- **A**: one")
        );
        assert_eq!(section(md, Version(1, 1, 0)), Some("- old"));
        assert_eq!(section(md, Version(1, 0, 0)), None);
    }

    #[test]
    fn notes_after_an_update_only() {
        let current = Version::current();
        let older = Version(0, 0, 1).to_string();
        let mut w = Welcome::default();
        w.after_start(true, Some(&older));
        assert_eq!(
            w.whats_new.as_ref().and_then(|n| n.first()).map(|n| n.0),
            Some(current)
        );
        // Settings without a version were saved by a version before this note.
        let mut w = Welcome::default();
        w.after_start(true, None);
        assert!(w.whats_new.is_some());
        // The same version, or a first start: nothing to tell.
        let mut w = Welcome::default();
        w.after_start(true, Some(&current.to_string()));
        assert!(w.whats_new.is_none());
        let mut w = Welcome::default();
        w.after_start(false, None);
        assert!(w.whats_new.is_none());
    }

    #[test]
    fn writes_a_sample_log() {
        let dir = std::env::temp_dir().join(format!("bigtext-sample-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sample.log");
        let written = AtomicU64::new(0);
        write_sample(&path, 300_000, &written).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.len() >= 300_000 && text.len() < 301_000,
            "{}",
            text.len()
        );
        assert_eq!(written.load(Ordering::Relaxed), text.len() as u64);
        assert!(!path.with_extension("part").exists());
        for line in text.lines() {
            assert!(line.starts_with("2026-11-"), "{line}");
            let level = line.split_whitespace().nth(2).unwrap();
            assert!(
                ["INFO", "WARN", "DEBUG", "ERROR"].contains(&level),
                "{line}"
            );
        }
        assert!(text.contains(" ERROR ") && text.contains(" WARN "));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
