//! Update check (automatically once a day, and from Help) and the update dialog. The dialog
//! shows the release notes first and installs only after the user agrees; "Skip this version"
//! keeps the automatic check from offering that version again.

use std::mem;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use egui::RichText;

use bigtext::jobs::{self, human_bytes, CancelToken, Progress};
use bigtext::update::{self, Block, Method, Span, Version};

use super::extras::{self, Persisted};
use super::i18n::{tr, trf};
use super::{BigTextApp, Msg};

/// Automatic checks run at most this often (seconds).
const CHECK_INTERVAL: u64 = 24 * 60 * 60;
/// The next try after an automatic check failed (offline, say).
const RETRY_AFTER: Duration = Duration::from_secs(60 * 60);
const CARGO_INSTALL: &str = "cargo install bigtext";

pub enum Event {
    Checked(Result<Option<Box<Offer>>, String>),
    Downloaded(Result<Pending, String>),
}

/// A newer release, ready to show.
pub struct Offer {
    version: Version,
    page: String,
    /// "What's new" of every release newer than the running one, newest first.
    notes: Vec<(Version, Vec<Block>)>,
    method: Method,
}

/// A downloaded update that takes effect when BigText restarts.
pub enum Pending {
    /// The executable was replaced; a restart runs the new one.
    Replaced { version: Version, exe: PathBuf },
    /// The installer runs when BigText exits.
    Installer { version: Version, setup: PathBuf },
}

#[derive(Default)]
enum Stage {
    #[default]
    Idle,
    /// A check the user asked for is running.
    Checking,
    Offer(Box<Offer>),
    Downloading { offer: Box<Offer>, progress: Progress, cancel: CancelToken },
    /// `UpdateUi::pending` is set: restart now or later.
    Ready,
    UpToDate,
    Failed { title: &'static str, message: String },
}

pub struct UpdateUi {
    stage: Stage,
    /// A check is running on a worker.
    running: bool,
    /// Check automatically (saved).
    pub auto: bool,
    /// "Skip this version" (saved).
    pub skipped: Option<Version>,
    /// Unix time of the last successful check (saved).
    pub last_check: u64,
    retry_at: Option<Instant>,
    pending: Option<Pending>,
    /// "Restart now" was chosen: on exit the update is applied and BigText starts again.
    pub restart: bool,
}

impl Default for UpdateUi {
    fn default() -> Self {
        UpdateUi { stage: Stage::Idle, running: false, auto: true, skipped: None, last_check: 0, retry_at: None, pending: None, restart: false }
    }
}

impl UpdateUi {
    pub fn restore(&mut self, p: &Persisted) {
        self.auto = p.update_check.unwrap_or(true);
        self.skipped = p.skip_version.as_deref().and_then(Version::parse);
        self.last_check = p.update_checked.unwrap_or(0);
    }

    pub fn dialog_open(&self) -> bool {
        !matches!(self.stage, Stage::Idle)
    }
}

/// Removes what an earlier update left behind (on a worker: it touches the file system).
pub fn cleanup_in_background() {
    jobs::spawn("update-cleanup", || {
        if let Ok(exe) = update::current_exe() {
            update::cleanup(&exe);
        }
    });
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn find_update() -> Result<Option<Box<Offer>>, String> {
    let releases = update::check(Version::current())?;
    let Some(latest) = releases.first() else { return Ok(None) };
    let method = match update::current_exe() {
        Ok(exe) => update::method(latest, &exe),
        Err(_) => Method::Manual,
    };
    let notes = releases.iter().map(|r| (r.version, update::parse_notes(update::whats_new(&r.notes)))).collect();
    Ok(Some(Box::new(Offer { version: latest.version, page: latest.page.clone(), notes, method })))
}

enum Action {
    Update,
    Close,
    Skip,
    Cancel,
    Restart,
    Open(String),
    Copy(&'static str),
}

impl BigTextApp {
    /// Starts the automatic check when it is due; called every frame.
    pub(super) fn update_tick(&mut self) {
        let u = &self.update;
        // Debug builds (development), benchmarks and the self-test never check on their own.
        if !u.auto || !self.persist || cfg!(debug_assertions) || u.running || u.pending.is_some() || u.dialog_open() {
            return;
        }
        if u.retry_at.is_none_or(|t| Instant::now() >= t) && now().abs_diff(u.last_check) >= CHECK_INTERVAL {
            self.check_for_updates(false);
        }
    }

    /// Help > Check for updates (`manual`: the dialog shows the answer, even "up to date"),
    /// or the automatic check.
    pub(super) fn check_for_updates(&mut self, manual: bool) {
        let u = &mut self.update;
        if manual {
            if u.pending.is_some() {
                u.stage = Stage::Ready;
                return;
            }
            u.stage = Stage::Checking;
        }
        if u.running {
            return;
        }
        u.running = true;
        let (tx, waker) = (self.tx.clone(), self.waker.clone());
        jobs::spawn("update-check", move || {
            let _ = tx.send(Msg::Update(Event::Checked(find_update())));
            waker();
        });
    }

    pub(super) fn on_update_event(&mut self, event: Event) {
        let u = &mut self.update;
        match event {
            Event::Checked(result) => {
                u.running = false;
                // The user is waiting for this answer (Help > Check for updates).
                let waiting = matches!(u.stage, Stage::Checking);
                match result {
                    Ok(found) => {
                        u.last_check = now();
                        u.retry_at = None;
                        match found {
                            Some(offer) if waiting || (!u.dialog_open() && u.skipped != Some(offer.version)) => u.stage = Stage::Offer(offer),
                            None if waiting => u.stage = Stage::UpToDate,
                            _ => {}
                        }
                        self.persist_state();
                    }
                    Err(message) => {
                        u.retry_at = Some(Instant::now() + RETRY_AFTER);
                        if waiting {
                            u.stage = Stage::Failed { title: "Could not check for updates", message };
                        }
                    }
                }
            }
            Event::Downloaded(result) => {
                if !matches!(u.stage, Stage::Downloading { .. }) {
                    return;
                }
                let Stage::Downloading { offer, cancel, .. } = mem::take(&mut u.stage) else { return };
                u.stage = match result {
                    Ok(p) => {
                        u.pending = Some(p);
                        Stage::Ready
                    }
                    Err(_) if cancel.is_cancelled() => Stage::Offer(offer),
                    Err(message) => Stage::Failed { title: "The update failed", message },
                };
            }
        }
    }

    fn start_update(&mut self) {
        let Stage::Offer(offer) = mem::take(&mut self.update.stage) else { return };
        let (progress, cancel) = (Progress::default(), CancelToken::new());
        let (method, version) = (offer.method.clone(), offer.version);
        let (p, c, tx, waker) = (progress.clone(), cancel.clone(), self.tx.clone(), self.waker.clone());
        jobs::spawn("update-download", move || {
            let result = match method {
                Method::Replace { asset, exe } => update::update_exe(&asset, &exe, &p, &c).map(|()| Pending::Replaced { version, exe }),
                Method::Installer { asset } => {
                    let setup = update::download_dir().join(&asset.name);
                    update::download(&asset, &setup, &p, &c).map(|()| Pending::Installer { version, setup })
                }
                Method::Cargo | Method::Manual => Err(tr("This copy of BigText cannot update itself.").into()),
            };
            let _ = tx.send(Msg::Update(Event::Downloaded(result)));
            waker();
        });
        self.update.stage = Stage::Downloading { offer, progress, cancel };
    }

    /// On exit: runs the downloaded installer, or starts the new version after "Restart now".
    pub(super) fn apply_update_on_exit(&mut self) {
        let restart = self.update.restart;
        let Some(p) = self.update.pending.take() else { return };
        if matches!(p, Pending::Replaced { .. }) && !restart {
            return;
        }
        if restart && self.persist {
            // The new process restores this session, so it has to be on disk first.
            extras::save_now(self.persisted());
        }
        let result = match &p {
            Pending::Replaced { exe, .. } => update::relaunch(exe),
            Pending::Installer { setup, .. } => update::run_installer(setup, restart),
        };
        if let Err(e) = result {
            // The window is gone by now.
            rfd::MessageDialog::new()
                .set_level(rfd::MessageLevel::Error)
                .set_title("BigText")
                .set_description(trf("Could not start the update: {e}", &[("e", &e.to_string())]))
                .set_buttons(rfd::MessageButtons::Ok)
                .show();
        }
    }

    pub(super) fn update_dialog(&mut self, ctx: &egui::Context) {
        // Waits behind the other dialogs (unsaved changes, errors).
        if !self.update.dialog_open() || self.confirm.is_some() || self.error.is_some() {
            return;
        }
        let current = Version::current();
        let mut action = None;
        let u = &mut self.update;
        let auto = u.auto;
        let modal = egui::Modal::new(egui::Id::new("update")).show(ctx, |ui| {
            ui.set_width(560.0);
            match &u.stage {
                Stage::Idle => {}
                Stage::Checking => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(tr("Checking for updates…"));
                    });
                    ui.add_space(6.0);
                    if ui.button(tr("Cancel")).clicked() {
                        action = Some(Action::Close);
                    }
                }
                Stage::Offer(o) => {
                    ui.heading(tr("Update available"));
                    ui.label(trf("BigText {new} is available. You have {current}.", &[("new", &o.version.to_string()), ("current", &current.to_string())]));
                    ui.add_space(6.0);
                    notes_ui(ui, o);
                    ui.add_space(6.0);
                    match &o.method {
                        Method::Cargo => {
                            ui.label(tr("This copy was installed with cargo. To update it, run:"));
                            ui.horizontal(|ui| {
                                ui.code(CARGO_INSTALL);
                                if ui.small_button(tr("Copy")).clicked() {
                                    action = Some(Action::Copy(CARGO_INSTALL));
                                }
                            });
                        }
                        Method::Manual => {
                            ui.label(tr("This release has no download for your platform."));
                        }
                        Method::Replace { .. } | Method::Installer { .. } => {}
                    }
                    ui.horizontal(|ui| {
                        match &o.method {
                            Method::Replace { .. } | Method::Installer { .. } => {
                                if ui.button(RichText::new(tr("Update now")).strong()).clicked() {
                                    action = Some(Action::Update);
                                }
                            }
                            Method::Manual => {
                                if ui.button(tr("Open download page")).clicked() {
                                    action = Some(Action::Open(o.page.clone()));
                                }
                            }
                            Method::Cargo => {}
                        }
                        if ui.button(tr("Remind me later")).clicked() {
                            action = Some(Action::Close);
                        }
                        if ui.button(tr("Skip this version")).clicked() {
                            action = Some(Action::Skip);
                        }
                    });
                    ui.add_space(2.0);
                    ui.checkbox(&mut u.auto, tr("Check for updates automatically"));
                }
                Stage::Downloading { offer, progress, cancel } => {
                    ui.heading(trf("Downloading BigText {v}", &[("v", &offer.version.to_string())]));
                    ui.add_space(6.0);
                    let text = trf("{done} of {total}", &[("done", &human_bytes(progress.done())), ("total", &human_bytes(progress.total()))]);
                    ui.add(egui::ProgressBar::new(progress.fraction()).text(text));
                    ui.add_space(6.0);
                    if cancel.is_cancelled() {
                        ui.add_enabled(false, egui::Button::new(tr("Cancelling…")));
                    } else if ui.button(tr("Cancel")).clicked() {
                        action = Some(Action::Cancel);
                    }
                    ui.ctx().request_repaint_after(Duration::from_millis(100));
                }
                Stage::Ready => {
                    let (version, text) = match &u.pending {
                        Some(Pending::Replaced { version, .. }) => {
                            (*version, tr("The new version starts the next time you open BigText. Restart now to switch to it; your open files reopen."))
                        }
                        Some(Pending::Installer { version, .. }) => {
                            (*version, tr("The installer runs when BigText closes. Restart now to install it; BigText then reopens with your files."))
                        }
                        None => (current, ""),
                    };
                    ui.heading(trf("BigText {v} is ready", &[("v", &version.to_string())]));
                    ui.label(text);
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button(RichText::new(tr("Restart now")).strong()).clicked() {
                            action = Some(Action::Restart);
                        }
                        if ui.button(tr("Later")).clicked() {
                            action = Some(Action::Close);
                        }
                    });
                }
                Stage::UpToDate => {
                    ui.heading(tr("BigText is up to date"));
                    ui.label(trf("You have the latest version, {v}.", &[("v", &current.to_string())]));
                    ui.add_space(6.0);
                    if ui.button(tr("OK")).clicked() {
                        action = Some(Action::Close);
                    }
                }
                Stage::Failed { title, message } => {
                    ui.heading(tr(title));
                    ui.label(message);
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button(tr("Open download page")).clicked() {
                            action = Some(Action::Open(update::RELEASES_PAGE.into()));
                        }
                        if ui.button(tr("Close")).clicked() {
                            action = Some(Action::Close);
                        }
                    });
                }
            }
        });
        // Escape or a click outside: the same as "Remind me later" / "Later" / "Close".
        if action.is_none() && modal.should_close() && !matches!(u.stage, Stage::Downloading { .. }) {
            action = Some(Action::Close);
        }
        if u.auto != auto {
            self.persist_state();
        }
        match action {
            Some(Action::Update) => self.start_update(),
            Some(Action::Close) => self.update.stage = Stage::Idle,
            Some(Action::Skip) => {
                if let Stage::Offer(o) = mem::take(&mut self.update.stage) {
                    self.update.skipped = Some(o.version);
                    self.persist_state();
                }
            }
            Some(Action::Cancel) => {
                if let Stage::Downloading { cancel, .. } = &self.update.stage {
                    cancel.cancel();
                }
            }
            Some(Action::Restart) => {
                if self.writes_in_progress() {
                    self.error = Some(tr("A file is being saved. Restart BigText when saving has finished.").into());
                } else {
                    self.update.stage = Stage::Idle;
                    self.update.restart = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            Some(Action::Open(url)) => {
                ctx.open_url(egui::OpenUrl::new_tab(url));
                self.update.stage = Stage::Idle;
            }
            Some(Action::Copy(text)) => ctx.copy_text(text.into()),
            None => {}
        }
    }
}

fn notes_ui(ui: &mut egui::Ui, o: &Offer) {
    egui::Frame::new().fill(ui.visuals().extreme_bg_color).corner_radius(4.0).inner_margin(10.0).show(ui, |ui| {
        egui::ScrollArea::vertical().max_height(360.0).auto_shrink([false, true]).show(ui, |ui| {
            for (v, blocks) in &o.notes {
                ui.label(RichText::new(trf("What's new in {v}", &[("v", &v.to_string())])).strong().size(16.0));
                if blocks.is_empty() {
                    ui.weak(tr("No release notes."));
                }
                for b in blocks {
                    block_ui(ui, b);
                }
                ui.add_space(10.0);
            }
        });
    });
    ui.hyperlink_to(tr("Full release notes on GitHub"), &o.page);
}

fn block_ui(ui: &mut egui::Ui, b: &Block) {
    match b {
        Block::Heading(spans) => {
            ui.add_space(4.0);
            spans_ui(ui, spans, true);
        }
        Block::Paragraph(spans) => spans_ui(ui, spans, false),
        Block::Item(depth, spans) => {
            // Numbered items carry their number in the text.
            let numbered = matches!(spans.first(), Some(Span::Text(t)) if t.split_once(". ").is_some_and(|(n, _)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())));
            ui.horizontal(|ui| {
                ui.add_space(4.0 + 16.0 * *depth as f32);
                if !numbered {
                    ui.label("•");
                }
                spans_ui(ui, spans, false);
            });
        }
        Block::Code(text) => {
            ui.label(RichText::new(text.trim_end()).monospace());
        }
    }
}

/// One wrapped paragraph of inline spans.
fn spans_ui(ui: &mut egui::Ui, spans: &[Span], strong: bool) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for s in spans {
            match s {
                Span::Text(t) if strong => {
                    ui.label(RichText::new(t).strong());
                }
                Span::Text(t) => {
                    ui.label(t);
                }
                Span::Strong(t) => {
                    ui.label(RichText::new(t).strong());
                }
                Span::Code(t) => {
                    ui.label(RichText::new(t).code());
                }
                Span::Link(t, url) => {
                    ui.hyperlink_to(t, url);
                }
            }
        }
    });
}
