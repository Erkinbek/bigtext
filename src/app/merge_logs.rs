//! File → Merge logs by time…: the lines of several open logs in time order in a new read-only
//! document. Each line starts with its file's name, painted in that file's colour, so one
//! request can be followed from the application log into the web server's log.

use std::collections::HashSet;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::Arc;

use bigtext::encoding::Codec;
use bigtext::jobs::{human_bytes, CancelToken, Progress};
use bigtext::merge::{self, Input};
use bigtext::search::{Matcher, Query};

use super::filter_panel::fmt_count;
use super::i18n::{tr, trf};
use super::tasks::TaskOutcome;
use super::BigTextApp;
use crate::view::highlight::Lang;

/// Labels longer than this are shortened.
const LABEL_MAX: usize = 32;

#[derive(Default)]
pub struct MergeUi {
    pub open: bool,
    /// Ticked documents (ids).
    pub picked: HashSet<u64>,
}

/// The labels of a merged document and matchers that find them at line starts (for colours).
pub struct MergeTags {
    pub labels: Vec<String>,
    pub matchers: Vec<Arc<Matcher>>,
}

impl MergeTags {
    pub fn new(labels: Vec<String>) -> MergeTags {
        let width = labels.iter().map(|l| l.chars().count()).max().unwrap_or(0);
        let matchers = labels
            .iter()
            .filter_map(|l| {
                let q = Query { pattern: format!("^{} │ ", regex_syntax::escape(&format!("{l:<width$}"))), regex: true, case_sensitive: true, whole_word: false, dot_all: false };
                Matcher::new(&q, Codec::utf8()).ok().map(Arc::new)
            })
            .collect();
        MergeTags { labels, matchers }
    }
}

pub struct MergedLogs {
    pub path: PathBuf,
    pub labels: Vec<String>,
    pub message: String,
}

/// File names as labels, shortened, with "(2)" added to repeated names.
pub fn labels_for(titles: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in titles {
        let mut l: String = t.chars().take(LABEL_MAX).collect();
        if t.chars().count() > LABEL_MAX {
            l.pop();
            l.push('…');
        }
        let mut n = 1;
        let base = l.clone();
        while out.contains(&l) {
            n += 1;
            l = format!("{base} ({n})");
        }
        out.push(l);
    }
    out
}

fn merge_job(inputs: Vec<Input>, target: PathBuf, progress: &Progress, cancel: &CancelToken) -> TaskOutcome {
    let labels: Vec<String> = inputs.iter().map(|i| i.label.clone()).collect();
    let r = (|| {
        if let Some(dir) = target.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = std::fs::File::options().write(true).create_new(true).open(&target)?;
        let mut w = BufWriter::with_capacity(8 << 20, file);
        let r = merge::merge_to(&inputs, &mut w, cancel, Some(progress))?;
        w.flush()?;
        Ok::<_, std::io::Error>(r)
    })();
    drop(inputs);
    match r {
        Ok(Some(m)) => {
            let mut message = trf("Merged {files} logs: {lines} lines ({size}).", &[("files", &labels.len().to_string()), ("lines", &fmt_count(m.lines)), ("size", &human_bytes(m.bytes))]);
            if m.without_times > 0 {
                message.push(' ');
                message.push_str(&trf("{n} of them had no times; their lines come first.", &[("n", &m.without_times.to_string())]));
            }
            TaskOutcome::Merged(Box::new(MergedLogs { path: target, labels, message }))
        }
        Ok(None) => {
            bigtext::decompress::remove_temp(&target);
            TaskOutcome::Cancelled
        }
        Err(e) => {
            bigtext::decompress::remove_temp(&target);
            TaskOutcome::Failed(trf("Could not merge the logs into {file}: {e}", &[("file", &bigtext::file::display_path(&target)), ("e", &e.to_string())]))
        }
    }
}

impl BigTextApp {
    pub(crate) fn open_merge(&mut self) {
        self.merge.open = true;
        self.merge.picked = self.docs.iter().filter(|d| d.is_ready() && d.effective_lang() == Lang::Log && !d.buf.is_empty()).map(|d| d.id).collect();
    }

    /// Merges the ticked documents in the background.
    pub(crate) fn start_merge(&mut self) {
        let docs: Vec<&super::document::Document> = self.docs.iter().filter(|d| self.merge.picked.contains(&d.id)).collect();
        if docs.len() < 2 || docs.iter().any(|d| !d.is_ready()) {
            return;
        }
        let titles: Vec<String> = docs.iter().map(|d| d.title.clone()).collect();
        let labels = labels_for(&titles);
        let inputs: Vec<Input> = docs.iter().zip(labels).map(|(d, label)| Input { buf: d.buf.snapshot(), label }).collect();
        let target = bigtext::decompress::temp_file("merged.log");
        self.merge.open = false;
        self.spawn_task(tr("Merging logs").into(), move |progress, cancel| merge_job(inputs, target, progress, cancel));
    }

    /// Opens a finished merge as a temporary read-only document.
    pub(super) fn on_merged(&mut self, m: MergedLogs) {
        self.open_path(m.path);
        if let Some(d) = self.docs.get_mut(self.active) {
            let mut title = format!("{} {}", tr("Merged:"), m.labels.join(" + "));
            if title.chars().count() > 60 {
                title = title.chars().take(59).chain(['…']).collect();
            }
            d.title = title;
            d.temporary = true;
            d.read_only = true;
            d.lang = Some(Lang::Log);
            d.merged = Some(MergeTags::new(m.labels));
        }
        self.notify(m.message, None);
    }

    pub(super) fn merge_window(&mut self, ctx: &egui::Context) {
        if !self.merge.open {
            return;
        }
        let mut open = true;
        let mut add = false;
        let mut merge = false;
        let mut close = false;
        egui::Window::new(tr("Merge logs by time")).open(&mut open).collapsible(false).default_width(520.0).show(ctx, |ui| {
            ui.label(tr("Puts the lines of the ticked logs in time order in a new read-only document. Lines without a time (stack traces) stay with the line above. Each line starts with its file's name in that file's colour."));
            ui.add_space(4.0);
            let mut ticked = 0;
            let mut waiting = false;
            egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                for d in &self.docs {
                    if d.path.is_none() && d.buf.is_empty() {
                        continue;
                    }
                    let mut on = self.merge.picked.contains(&d.id);
                    ui.horizontal(|ui| {
                        if ui.checkbox(&mut on, &d.title).changed() {
                            if on {
                                self.merge.picked.insert(d.id);
                            } else {
                                self.merge.picked.remove(&d.id);
                            }
                        }
                        if !d.is_ready() {
                            ui.spinner();
                            ui.weak(tr("opening…"));
                        } else {
                            ui.weak(human_bytes(d.buf.len()));
                        }
                    });
                    if on {
                        ticked += 1;
                        waiting |= !d.is_ready();
                    }
                }
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.button(tr("Add files…")).clicked() {
                    add = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(tr("Cancel")).clicked() {
                        close = true;
                    }
                    let ok = ticked >= 2 && !waiting;
                    if ui.add_enabled(ok, egui::Button::new(tr("Merge"))).on_disabled_hover_text(tr("Tick at least two logs that have finished opening")).clicked() {
                        merge = true;
                    }
                });
            });
        });
        if add {
            if let Some(files) = rfd::FileDialog::new().set_title(tr("Add files…")).pick_files() {
                for f in files {
                    self.open_path(f);
                    if let Some(d) = self.docs.get(self.active) {
                        self.merge.picked.insert(d.id);
                    }
                }
            }
        }
        if merge {
            self.start_merge();
        }
        if !open || close {
            self.merge.open = false;
        }
    }

    /// Test hook: ticks exactly these documents.
    pub(crate) fn test_pick_for_merge(&mut self, ids: &[u64]) {
        self.merge.picked = ids.iter().copied().collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_short_and_unique() {
        let long = "x".repeat(40);
        let l = labels_for(&["app.log".into(), "app.log".into(), long.clone(), "app.log".into()]);
        assert_eq!(l[0], "app.log");
        assert_eq!(l[1], "app.log (2)");
        assert_eq!(l[2].chars().count(), LABEL_MAX);
        assert!(l[2].ends_with('…'));
        assert_eq!(l[3], "app.log (3)");
    }

    #[test]
    fn tags_match_their_own_lines_only() {
        let t = MergeTags::new(vec!["a".into(), "a b".into()]);
        let line = |s: &str| -> Vec<bool> {
            t.matchers
                .iter()
                .map(|m| {
                    let mut hit = false;
                    m.find_in(s.as_bytes(), 0, 0, s.len(), s.len(), &mut None, &mut |_, _| {
                        hit = true;
                        false
                    });
                    hit
                })
                .collect()
        };
        assert_eq!(line("a   │ 10:00:00 x"), vec![true, false]);
        assert_eq!(line("a b │ 10:00:00 x"), vec![false, true]);
        assert_eq!(line("x a │ y"), vec![false, false]);
    }
}
