//! Background tasks that are not tied to a document's save slot (exports, splitting, column
//! statistics): each shows a progress bar with Cancel in the status bar, and a notice when done.

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
    /// JSON Lines converted to a temporary CSV, to open as a table.
    JsonlTable(Box<super::json_panel::JsonlTable>),
}

/// A short message in the status bar that disappears after a while.
pub struct Notice {
    pub text: String,
    pub open: Option<PathBuf>,
    pub at: Instant,
}

pub const NOTICE_TIME: Duration = Duration::from_secs(12);

/// Streams into a temp file next to `target` and moves it into place when `write` succeeds.
/// `write` returns the notice text, or `None` when it was cancelled.
pub fn write_via_temp(target: &Path, write: impl FnOnce(&mut BufWriter<&File>) -> io::Result<Option<String>>) -> TaskOutcome {
    let temp = save::temp_path_for(target);
    let file = match File::options().write(true).create_new(true).open(&temp) {
        Ok(f) => f,
        Err(e) => return TaskOutcome::Failed(save::describe(&e, target)),
    };
    let r = (|| {
        let mut w = BufWriter::with_capacity(8 << 20, &file);
        let r = write(&mut w)?;
        w.flush()?;
        drop(w);
        file.sync_all()?;
        Ok(r)
    })();
    drop(file);
    match r {
        Ok(Some(message)) => match save::finalize(&temp, target, false) {
            Ok(()) => TaskOutcome::Done { message, open: Some(target.to_path_buf()) },
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
            TaskOutcome::JsonlTable(t) => self.on_jsonl_table(*t),
        }
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
        let mut clear = false;
        if let Some(n) = &self.notice {
            if n.at.elapsed() > NOTICE_TIME {
                clear = true;
            } else {
                if ui.small_button("×").clicked() {
                    clear = true;
                }
                if let Some(p) = &n.open {
                    if ui.small_button(super::i18n::tr("Open")).clicked() {
                        open = Some(p.clone());
                        clear = true;
                    }
                }
                ui.label(egui::RichText::new(&n.text).color(egui::Color32::from_rgb(90, 180, 90)));
                ui.separator();
                ui.ctx().request_repaint_after(Duration::from_millis(500));
            }
        }
        if clear {
            self.notice = None;
        }
        if let Some(p) = open {
            self.open_path(p);
        }
    }
}
