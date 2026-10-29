//! The matched texts of Find All on their own: "Top values…" counts the different matches (or a
//! capture group of them) into a table, most frequent first, and "Save matches…" writes each
//! one on a line.

use std::sync::Arc;

use bigtext::extract;
use bigtext::jobs::{CancelToken, Progress};
use bigtext::search::{self, Matcher};

use super::filter_panel::{file_name, fmt_count};
use super::i18n::{tr, trf};
use super::tasks::{self, TaskOutcome};
use super::BigTextApp;

/// Picking the capture group, for a regular expression with groups.
pub struct ValuesDialog {
    save: bool,
    group: usize,
    /// Names of groups 1, 2, … ("" for unnamed ones).
    groups: Vec<String>,
}

impl BigTextApp {
    /// Top values… (`save` false) or Save matches… of the Find All results.
    pub(crate) fn open_values(&mut self, save: bool) {
        let Some(d) = self.docs.get(self.active) else { return };
        let Some(r) = &d.results else { return };
        let m = match Matcher::new(&r.query, d.buf.codec) {
            Ok(m) => m,
            Err(e) => {
                self.error = Some(e);
                return;
            }
        };
        let groups = search::group_names(&m);
        if groups.is_empty() {
            self.run_values(save, 0);
        } else {
            self.values = Some(ValuesDialog { save, group: 1, groups });
        }
    }

    pub(super) fn values_window(&mut self, ctx: &egui::Context) {
        let Some(v) = self.values.as_mut() else { return };
        let mut open = true;
        let mut go = false;
        let title = if v.save { tr("Save matches") } else { tr("Top values") };
        egui::Window::new(title).open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
            let name = |g: usize, groups: &[String]| match g {
                0 => tr("The whole match").to_string(),
                g if groups[g - 1].is_empty() => trf("Group {n}", &[("n", &g.to_string())]),
                g => trf("Group {n} ({name})", &[("n", &g.to_string()), ("name", &groups[g - 1])]),
            };
            egui::ComboBox::from_label(tr("Of")).selected_text(name(v.group, &v.groups)).show_ui(ui, |ui| {
                for g in 0..=v.groups.len() {
                    ui.selectable_value(&mut v.group, g, name(g, &v.groups));
                }
            });
            go = ui.button(tr("OK")).clicked();
        });
        if go {
            let (save, group) = (v.save, v.group);
            self.values = None;
            self.run_values(save, group);
        } else if !open {
            self.values = None;
        }
    }

    pub(crate) fn run_values(&mut self, save: bool, group: usize) {
        let Some(d) = self.docs.get(self.active) else { return };
        let Some(r) = &d.results else { return };
        let Ok(m) = Matcher::new(&r.query, d.buf.codec) else { return };
        let (snap, m, range, doc) = (d.buf.snapshot(), Arc::new(m), r.range.clone(), d.title.clone());
        if save {
            let Some(target) = self.pick_output_file(tr("Save matches"), &format!("{doc}.matches.txt")) else { return };
            self.spawn_task(tr("Saving matches").into(), move |progress: &Progress, cancel: &CancelToken| -> TaskOutcome {
                progress.set_total(range.end - range.start);
                tasks::write_via_temp(&target, |w| {
                    Ok(extract::write_values_to(&snap, &m, group, range, w, cancel, Some(progress))?
                        .map(|n| trf("Saved {n} matches to {file}.", &[("n", &fmt_count(n)), ("file", &file_name(&target))])))
                })
            });
            return;
        }
        let target = bigtext::decompress::temp_file("values.csv");
        let title = trf("Values of “{pattern}” ({name})", &[("pattern", &r.query.pattern), ("name", &doc)]);
        self.spawn_task(tr("Counting values").into(), move |progress: &Progress, cancel: &CancelToken| {
            progress.set_total(range.end - range.start);
            let r = (|| {
                let Some(t) = extract::count(&snap, &m, group, range, cancel, Some(progress))? else { return Ok(None) };
                if let Some(dir) = target.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                let mut w = std::io::BufWriter::new(std::fs::File::options().write(true).create_new(true).open(&target)?);
                extract::tally_to_csv(&t, &mut w)?;
                std::io::Write::flush(&mut w)?;
                Ok::<_, std::io::Error>(Some(t))
            })();
            match r {
                Ok(Some(t)) => {
                    let mut message = trf("{n} different values in {matches} matches.", &[("n", &fmt_count(t.values.len() as u64)), ("matches", &fmt_count(t.matches))]);
                    if t.other > 0 {
                        message.push(' ');
                        message.push_str(&trf("Only the first {max} different values are listed; {n} matches have others.", &[("max", &fmt_count(extract::VALUES_MAX as u64)), ("n", &fmt_count(t.other))]));
                    }
                    TaskOutcome::TempTable(Box::new(tasks::TempTable { path: target, title, message }))
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
