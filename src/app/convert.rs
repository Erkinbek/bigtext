//! File → JSON array → JSON Lines…, JSON Lines → JSON array…, and Open logfmt as table: the
//! active document converted in one streaming pass.

use bigtext::jobs::{CancelToken, Progress};
use bigtext::json;

use super::filter_panel::{file_name, fmt_count};
use super::i18n::{tr, trf};
use super::tasks::{self, TaskOutcome};
use super::BigTextApp;

impl BigTextApp {
    /// Writes the items of the document's JSON array (or of the first array of its top-level
    /// object) to a new JSON Lines file.
    pub(crate) fn array_to_lines(&mut self) {
        let Some(d) = self.docs.get(self.active).filter(|d| d.is_ready()) else { return };
        let snap = d.buf.snapshot();
        let stem = std::path::Path::new(&d.title).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "items".into());
        let Some(target) = self.pick_output_file(tr("Save as JSON Lines"), &format!("{stem}.jsonl")) else { return };
        self.spawn_task(tr("Converting to JSON Lines").into(), move |progress: &Progress, cancel: &CancelToken| -> TaskOutcome {
            let (at, path) = match json::find_array(&snap, cancel) {
                Ok(x) => x,
                Err(e) => return TaskOutcome::Failed(e),
            };
            let mut items = 0;
            let out = tasks::write_via_temp(&target, |w| match json::array_to_lines(&snap, at, w, cancel, Some(progress)) {
                Ok(Some(n)) => {
                    items = n;
                    Ok(Some(String::new()))
                }
                Ok(None) => Ok(None),
                Err(e) => Err(std::io::Error::other(e)),
            });
            match out {
                TaskOutcome::Done { open, .. } => TaskOutcome::Done { message: trf("Wrote the {n} items of {path} to {file}.", &[("n", &fmt_count(items)), ("path", &path), ("file", &file_name(&target))]), open },
                other => other,
            }
        });
    }

    /// Writes the document's JSON Lines as one JSON array to a new file.
    pub(crate) fn lines_to_array(&mut self) {
        let Some(d) = self.docs.get(self.active).filter(|d| d.is_ready()) else { return };
        let snap = d.buf.snapshot();
        let stem = std::path::Path::new(&d.title).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "items".into());
        let Some(target) = self.pick_output_file(tr("Save as a JSON array"), &format!("{stem}.json")) else { return };
        self.spawn_task(tr("Converting to a JSON array").into(), move |progress: &Progress, cancel: &CancelToken| -> TaskOutcome {
            tasks::write_via_temp(&target, |w| {
                Ok(json::lines_to_array(&snap, w, cancel, Some(progress))?.map(|n| trf("Wrote {n} items to {file}.", &[("n", &fmt_count(n)), ("file", &file_name(&target))])))
            })
        });
    }

    /// Opens the document's logfmt lines (key=value pairs) as a table.
    pub(crate) fn open_logfmt_table(&mut self) {
        let Some(d) = self.docs.get(self.active).filter(|d| d.is_ready()) else { return };
        let snap = d.buf.snapshot();
        let stem = std::path::Path::new(&d.title).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "log".into());
        let target = bigtext::decompress::temp_file(&format!("{stem}.csv"));
        let title = trf("{name} (table)", &[("name", &d.title)]);
        self.spawn_task(tr("Reading logfmt").into(), move |progress: &Progress, cancel: &CancelToken| {
            let r = (|| {
                if let Some(dir) = target.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                let mut w = std::io::BufWriter::with_capacity(8 << 20, std::fs::File::options().write(true).create_new(true).open(&target)?);
                let st = bigtext::fields::logfmt_to_csv(&snap, &mut w, cancel, Some(progress))?;
                std::io::Write::flush(&mut w)?;
                Ok::<_, std::io::Error>(st)
            })();
            match r {
                Ok(Some((rows, cols, skipped))) if rows > 0 => {
                    let mut message = trf("{rows} rows, {cols} columns.", &[("rows", &fmt_count(rows)), ("cols", &cols.to_string())]);
                    if skipped > 0 {
                        message.push(' ');
                        message.push_str(&trf("{n} lines without key=value pairs were left out.", &[("n", &fmt_count(skipped))]));
                    }
                    TaskOutcome::TempTable(Box::new(tasks::TempTable { path: target, title, message }))
                }
                Ok(Some(_)) => {
                    bigtext::decompress::remove_temp(&target);
                    TaskOutcome::Failed(tr("No line has key=value pairs (logfmt).").into())
                }
                Ok(None) => {
                    bigtext::decompress::remove_temp(&target);
                    TaskOutcome::Cancelled
                }
                Err(e) => {
                    bigtext::decompress::remove_temp(&target);
                    TaskOutcome::Failed(e.to_string())
                }
            }
        });
    }
}
