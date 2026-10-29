//! Background tasks that are not tied to a document's save slot (exports, splitting, column
//! statistics): each shows a progress bar with Cancel in the status bar, and a notice when done.

use bigtext::decompress::Packer;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bigtext::jobs::{self, CancelToken, Progress};
use bigtext::save;

use super::{BigTextApp, Msg};

pub struct Task {
    pub id: u64,
    pub label: String,
    pub progress: Progress,
    pub cancel: CancelToken,
}

pub enum TaskOutcome {
    /// Finished; the message is shown as a notice, with an "Open" link when a file was written.
    Done { message: String, open: Option<PathBuf> },
    Cancelled,
    Failed(String),
    /// Column statistics (document title, figures) for the stats window.
    Stats(Box<(String, bigtext::csv::ColumnStats)>),
    /// Logs merged into a temporary file, to open as a document.
    Merged(Box<super::merge_logs::MergedLogs>),
    /// Rows per value of a column (document title, groups) for the groups window.
    Groups(Box<(String, bigtext::csv::Groups)>),
    /// A temporary CSV (converted JSON Lines, filtered rows) to open read-only as a table.
    TempTable(Box<TempTable>),
    /// The fields of JSON records (document title, fields) for the fields window.
    JsonFields(Box<(String, bigtext::json_fields::Fields)>),
    /// Where a patch's hunks go, to apply to the document.
    Patch(Box<super::extras::PatchDone>),
}

pub struct TempTable {
    pub path: PathBuf,
    pub title: String,
    pub message: String,
}

/// A short message in the status bar that disappears after a while.
pub struct Notice {
    pub text: String,
    pub open: Option<PathBuf>,
    pub at: Instant,
}

pub const NOTICE_TIME: Duration = Duration::from_secs(12);

/// Streams into a temp file next to `target` and moves it into place when `write` succeeds;
/// a `.gz`, `.zst`, `.xz` or `.bz2` target is compressed on the way. `write` returns the notice
/// text, or `None` when it was cancelled.
pub fn write_via_temp(target: &Path, write: impl FnOnce(&mut Packer<BufWriter<&File>>) -> io::Result<Option<String>>) -> TaskOutcome {
    let temp = save::temp_path_for(target);
    let file = match File::options().write(true).create_new(true).open(&temp) {
        Ok(f) => f,
        Err(e) => return TaskOutcome::Failed(save::describe(&e, target)),
    };
    let kind = bigtext::decompress::target_kind(target);
    let r = (|| {
        let mut w = Packer::new(BufWriter::with_capacity(8 << 20, &file), kind)?;
        let r = write(&mut w)?;
        // The end of the compressed stream must be written before the data is synced.
        let mut w = w.finish()?;
        w.flush()?;
        drop(w);
        file.sync_all()?;
        Ok(r)
    })();
    drop(file);
    match r {
        Ok(Some(mut message)) => match save::finalize(&temp, target, false) {
            Ok(()) => {
                if let (Some(_), Ok(m)) = (kind, std::fs::metadata(target)) {
                    message.push(' ');
                    message.push_str(&super::i18n::trf("Compressed to {size}.", &[("size", &bigtext::jobs::human_bytes(m.len()))]));
                }
                TaskOutcome::Done { message, open: Some(target.to_path_buf()) }
            }
            Err(e) => TaskOutcome::Failed(e),
        },
        Ok(None) => {
            let _ = std::fs::remove_file(&temp);
            TaskOutcome::Cancelled
        }
        Err(e) => {
            let _ = std::fs::remove_file(&temp);
            TaskOutcome::Failed(save::describe(&e, target))
        }
    }
}

impl BigTextApp {
    /// Runs `f` on a worker with its own progress and cancel button in the status bar.
    pub(crate) fn spawn_task(&mut self, label: String, f: impl FnOnce(&Progress, &CancelToken) -> TaskOutcome + Send + 'static) {
        self.next_task += 1;
        let id = self.next_task;
        let progress = Progress::default();
        let cancel = CancelToken::new();
        self.tasks.push(Task { id, label, progress: progress.clone(), cancel: cancel.clone() });
        let tx = self.tx.clone();
        let waker = self.waker.clone();
        jobs::spawn("task", move || {
            let outcome = f(&progress, &cancel);
            let _ = tx.send(Msg::TaskDone { id, outcome });
            waker();
        });
    }

    pub(super) fn on_task_done(&mut self, id: u64, outcome: TaskOutcome) {
        let label = self.tasks.iter().find(|t| t.id == id).map(|t| t.label.clone()).unwrap_or_default();
        self.tasks.retain(|t| t.id != id);
        match outcome {
            TaskOutcome::Done { message, open } => self.notify(message, open),
            TaskOutcome::Cancelled => self.notify(super::i18n::trf("{task}: cancelled.", &[("task", &label)]), None),
            TaskOutcome::Failed(e) => self.error = Some(e),
            TaskOutcome::Stats(s) => self.csv.stats = Some(*s),
            TaskOutcome::Merged(m) => self.on_merged(*m),
            TaskOutcome::TempTable(t) => self.open_temp_table(*t),
            TaskOutcome::Groups(g) => self.csv.groups = Some(*g),
            TaskOutcome::JsonFields(f) => self.json_fields.result = Some(*f),
            TaskOutcome::Patch(p) => self.on_patch_done(*p),
        }
    }

    /// Opens a temporary CSV as a read-only table (deleted when its tab closes).
    fn open_temp_table(&mut self, t: TempTable) {
        self.open_path(t.path);
        if let Some(d) = self.docs.get_mut(self.active) {
            d.title = t.title;
            d.temporary = true;
            d.read_only = true;
            d.table = true;
        }
        self.notify(t.message, None);
    }

    pub(crate) fn notify(&mut self, text: String, open: Option<PathBuf>) {
        self.notice = Some(Notice { text, open, at: Instant::now() });
    }

    /// Progress bars of running tasks and the latest notice (right-to-left status bar layout).
    pub(super) fn tasks_status(&mut self, ui: &mut egui::Ui) {
        for t in &self.tasks {
            if ui.small_button(super::i18n::tr("Cancel")).clicked() {
                t.cancel.cancel();
            }
            let f = t.progress.fraction();
            ui.add(egui::ProgressBar::new(f).desired_width(160.0).text(format!("{} {:.0}%", t.label, f * 100.0)));
            ui.separator();
        }
        let mut open = None;
        let mut reveal = None;
        let mut clear = false;
        if let Some(n) = &self.notice {
            if n.at.elapsed() > NOTICE_TIME {
                clear = true;
            } else {
                if ui.small_button("×").clicked() {
                    clear = true;
                }
                if let Some(p) = &n.open {
                    if ui.small_button(super::i18n::tr("Show in folder")).clicked() {
                        reveal = Some(p.clone());
                    }
                    if ui.small_button(super::i18n::tr("Open")).clicked() {
                        open = Some(p.clone());
                        clear = true;
                    }
                }
                ui.label(egui::RichText::new(&n.text).color(super::theme::ok_text(ui)));
                ui.separator();
                ui.ctx().request_repaint_after(Duration::from_millis(500));
            }
        }
        if clear {
            self.notice = None;
        }
        if let Some(p) = reveal {
            self.shell_file(&p, super::shell::ShellAction::Reveal);
        }
        // A spreadsheet opens in the spreadsheet app.
        if let Some(p) = open {
            if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("xlsx")) {
                self.shell_file(&p, super::shell::ShellAction::Default);
            } else {
                self.open_path(p);
            }
        }
    }
}
