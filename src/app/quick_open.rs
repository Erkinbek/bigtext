//! Search → Go to file (Ctrl+P): the files of the side bar's folder — or, with no folder, the
//! open tabs and recent files — found by a fuzzy match of their paths, as in Sublime Text.
//! `name:42` opens the file at line 42. The folder is listed on a worker, at most
//! [`MAX_FILES`] files, without build and version-control folders.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use egui::text::{LayoutJob, TextFormat};
use egui::{Key, Modifiers};

use super::i18n::{tr, trf};
use super::BigTextApp;

/// Files listed from a folder at most.
pub const MAX_FILES: usize = 200_000;
/// Results shown.
const SHOWN: usize = 60;
/// A listing older than this is made again when Go to file opens.
const STALE: Duration = Duration::from_secs(15);
/// Folders not listed: version control, dependencies, caches and build output.
const SKIP_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "CVS",
    "node_modules",
    "__pycache__",
    ".venv",
    "venv",
    ".idea",
    ".vs",
    ".mypy_cache",
    ".pytest_cache",
    ".gradle",
    "target",
];

/// The dialog's state.
#[derive(Default)]
pub struct QuickOpen {
    pub query: String,
    pub selected: usize,
    focus: bool,
    /// The results, for the name and the listing they were found for.
    found: Vec<(usize, Vec<usize>)>,
    found_for: Option<(String, u64)>,
}

/// The files of the side bar's folder (paths relative to it, with `/`).
#[derive(Default)]
pub struct FileIndex {
    pub root: Option<PathBuf>,
    pub files: Vec<String>,
    /// More files than [`MAX_FILES`] were found.
    pub truncated: bool,
    listed: Option<Instant>,
    /// Counts the listings taken (results are found again after one).
    pub generation: u64,
    rx: Option<mpsc::Receiver<(PathBuf, Vec<String>, bool)>>,
}

impl FileIndex {
    pub fn listing(&self) -> bool {
        self.rx.is_some()
    }

    /// Lists `root` again on a worker unless a fresh listing of it is at hand.
    pub fn refresh(&mut self, root: &Path, waker: bigtext::jobs::Waker, force: bool) {
        let fresh =
            self.root.as_deref() == Some(root) && self.listed.is_some_and(|t| t.elapsed() < STALE);
        if (fresh && !force) || self.rx.is_some() {
            return;
        }
        if self.root.as_deref() != Some(root) {
            self.files.clear();
            self.truncated = false;
        }
        self.root = Some(root.to_path_buf());
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        let root = root.to_path_buf();
        bigtext::jobs::spawn("list-files", move || {
            let (files, truncated) = walk(&root, MAX_FILES);
            let _ = tx.send((root, files, truncated));
            waker();
        });
    }

    /// Takes a finished listing.
    pub fn poll(&mut self) {
        let Some(rx) = &self.rx else { return };
        match rx.try_recv() {
            Ok((root, files, truncated)) => {
                self.rx = None;
                if self.root.as_deref() == Some(root.as_path()) {
                    self.files = files;
                    self.truncated = truncated;
                    self.generation += 1;
                    self.listed = Some(Instant::now());
                }
            }
            Err(mpsc::TryRecvError::Disconnected) => self.rx = None,
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }
}

/// The files under `root` (relative, `/`-separated, sorted), at most `max`; and whether there
/// were more. Symbolic links to folders are not followed (they can loop).
pub fn walk(root: &Path, max: usize) -> (Vec<String>, bool) {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, rel)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut subdirs = Vec::new();
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Ok(t) = e.file_type() else { continue };
            let path = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            if t.is_dir() {
                if !SKIP_DIRS.contains(&name.as_str()) {
                    subdirs.push((e.path(), path));
                }
            } else if t.is_file() || (t.is_symlink() && e.path().is_file()) {
                if matches!(name.as_str(), ".DS_Store" | "Thumbs.db" | "desktop.ini") {
                    continue;
                }
                if out.len() == max {
                    out.sort_by(|a: &String, b| super::folder_tree::natural(a, b));
                    return (out, true);
                }
                out.push(path);
            }
        }
        stack.extend(subdirs);
    }
    out.sort_by(|a, b| super::folder_tree::natural(a, b));
    (out, false)
}

/// `main.rs:42` → ("main.rs", Some(42)).
pub fn split_query(q: &str) -> (&str, Option<u64>) {
    let q = q.trim();
    match q.rsplit_once(':') {
        Some((name, n)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => {
            (name.trim_end(), n.parse().ok())
        }
        _ => (q, None),
    }
}

fn boundary(prev: Option<char>, c: char) -> bool {
    match prev {
        None => true,
        Some(p) => {
            matches!(p, '/' | '\\' | '_' | '-' | '.' | ' ')
                || (p.is_lowercase() && c.is_uppercase())
        }
    }
}

/// How well `query` matches `path` (higher is better), with the indexes of the matched
/// characters; `None` when the query's characters do not all occur in order. Matches in the file
/// name, at the start of a word and next to each other count more; shorter paths win ties.
pub fn fuzzy(query: &str, path: &str) -> Option<(i32, Vec<usize>)> {
    let q: Vec<char> = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    let p: Vec<char> = path.chars().collect();
    let name_at = path
        .rfind(['/', '\\'])
        .map_or(0, |i| path[..=i].chars().count());
    if q.is_empty() {
        return Some((-(p.len() as i32), Vec::new()));
    }
    if !subsequence(&q, path) {
        return None;
    }
    let lower: Vec<char> = p
        .iter()
        .map(|c| c.to_lowercase().next().unwrap_or(*c))
        .collect();
    // The whole query in the file name, else from the start of the path; at each step a word's
    // start is preferred to the next plain occurrence when there is one before the next match.
    let attempt = |from: usize| -> Option<Vec<usize>> {
        let mut pos = Vec::with_capacity(q.len());
        let mut i = from;
        for (k, &qc) in q.iter().enumerate() {
            let next = (i..p.len()).find(|&j| lower[j] == qc)?;
            // A later occurrence at a word start, if the rest still fits after it.
            let better = (next + 1..p.len())
                .find(|&j| lower[j] == qc && boundary(j.checked_sub(1).map(|b| p[b]), p[j]));
            let pick = match better {
                Some(b)
                    if !boundary(next.checked_sub(1).map(|x| p[x]), p[next])
                        && pos.last().is_none_or(|&l: &usize| l + 1 != next)
                        && fits(&lower, &q[k + 1..], b + 1) =>
                {
                    b
                }
                _ => next,
            };
            pos.push(pick);
            i = pick + 1;
        }
        Some(pos)
    };
    let pos = attempt(name_at).or_else(|| attempt(0))?;
    let mut score = 0i32;
    for (k, &j) in pos.iter().enumerate() {
        score += 1;
        if boundary(j.checked_sub(1).map(|b| p[b]), p[j]) {
            score += 8;
        }
        if k > 0 && pos[k - 1] + 1 == j {
            score += 5;
        }
        if j >= name_at {
            score += 3;
        }
    }
    let name: String = lower[name_at..].iter().collect();
    let ql: String = q.iter().collect();
    if name.starts_with(&ql) {
        score += 15;
    }
    score -= (p.len() as i32) / 8;
    Some((score, pos))
}

/// True if the (lower-case) characters `q` occur in `path` in order, ignoring case.
fn subsequence(q: &[char], path: &str) -> bool {
    let mut k = 0;
    for c in path.chars() {
        if k == q.len() {
            break;
        }
        if c == q[k] || c.to_lowercase().next() == Some(q[k]) {
            k += 1;
        }
    }
    k == q.len()
}

fn fits(lower: &[char], rest: &[char], from: usize) -> bool {
    let mut i = from;
    for &c in rest {
        match (i..lower.len()).find(|&j| lower[j] == c) {
            Some(j) => i = j + 1,
            None => return false,
        }
    }
    true
}

/// The best `n` of `candidates` for `query`: (index, score, matched characters).
pub fn best(query: &str, candidates: &[String], n: usize) -> Vec<(usize, Vec<usize>)> {
    let mut v: Vec<(usize, i32, Vec<usize>)> = candidates
        .iter()
        .enumerate()
        .filter_map(|(i, c)| fuzzy(query, c).map(|(s, p)| (i, s, p)))
        .collect();
    let by = |a: &(usize, i32, Vec<usize>), b: &(usize, i32, Vec<usize>)| {
        b.1.cmp(&a.1)
            .then_with(|| super::folder_tree::natural(&candidates[a.0], &candidates[b.0]))
    };
    if v.len() > n {
        v.select_nth_unstable_by(n, by);
        v.truncate(n);
    }
    v.sort_by(by);
    v.into_iter().map(|(i, _, p)| (i, p)).collect()
}

impl BigTextApp {
    /// Search → Go to file.
    pub(crate) fn open_quick_open(&mut self) {
        if let Some(root) = self.folder.root.clone() {
            self.file_index.refresh(&root, self.waker.clone(), false);
        }
        self.quick_open = Some(QuickOpen {
            focus: true,
            ..Default::default()
        });
    }

    /// With no folder, Go to file chooses from the open tabs and recent files (full paths).
    fn recent_candidates(&self) -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        for p in self
            .docs
            .iter()
            .filter_map(|d| d.path.as_deref())
            .chain(self.recent.iter().map(PathBuf::as_path))
        {
            let s = bigtext::file::display_path(p);
            if !v.contains(&s) {
                v.push(s);
            }
        }
        v
    }

    pub(super) fn quick_open_window(&mut self, ctx: &egui::Context) {
        self.file_index.poll();
        let Some(q) = &mut self.quick_open else {
            return;
        };
        let (up, down, enter, esc) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        let (name, mut line) = split_query(&q.query);
        let name = name.to_string();
        let root = self
            .folder
            .root
            .clone()
            .filter(|r| self.file_index.root.as_deref() == Some(r.as_path()));
        let recent;
        let (candidates, generation): (&[String], u64) = if root.is_some() {
            (&self.file_index.files, self.file_index.generation)
        } else {
            recent = self.recent_candidates();
            (&recent, u64::MAX - recent.len() as u64)
        };
        let q = self.quick_open.as_mut().unwrap();
        // Found again only when the name or the listing changes (a folder can hold 200,000 files).
        if q.found_for.as_ref() != Some(&(name.clone(), generation)) {
            q.found = best(&name, candidates, SHOWN);
            q.found_for = Some((name.clone(), generation));
        }
        let found = q.found.clone();
        q.selected = q.selected.min(found.len().saturating_sub(1));
        if up {
            q.selected = q.selected.saturating_sub(1);
        }
        if down && q.selected + 1 < found.len() {
            q.selected += 1;
        }
        let mut chosen = None;
        let listing = self.file_index.listing();
        let truncated = self.file_index.truncated && root.is_some();
        let no_folder = self.folder.root.is_none();
        egui::Window::new("quick-open")
            .title_bar(false)
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 60.0))
            .fixed_size(egui::vec2(600.0, 0.0))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut q.query)
                            .hint_text(tr("Go to file (name:line)"))
                            .desired_width(ui.available_width() - if listing { 24.0 } else { 0.0 }),
                    );
                    if q.focus {
                        r.request_focus();
                        q.focus = false;
                    }
                    if r.changed() {
                        q.selected = 0;
                    }
                    if listing {
                        ui.spinner().on_hover_text(tr("Reading…"));
                    }
                });
                ui.separator();
                let normal = ui.visuals().text_color();
                let weak = ui.visuals().weak_text_color();
                let hit = super::theme::info_text(ui);
                let font = egui::TextStyle::Body.resolve(ui.style());
                egui::ScrollArea::vertical()
                    .max_height(400.0)
                    .show(ui, |ui| {
                        for (k, (i, pos)) in found.iter().enumerate() {
                            let path = &candidates[*i];
                            let name_at = path.rfind(['/', '\\']).map_or(0, |n| n + 1);
                            let mut job = LayoutJob::default();
                            let char_at: Vec<usize> = path.char_indices().map(|(b, _)| b).collect();
                            let push =
                                |job: &mut LayoutJob,
                                 from: usize,
                                 to: usize,
                                 color: egui::Color32| {
                                    for (ci, &b) in char_at.iter().enumerate() {
                                        if b < from || b >= to {
                                            continue;
                                        }
                                        let e = char_at.get(ci + 1).copied().unwrap_or(path.len());
                                        let c = if pos.contains(&ci) { hit } else { color };
                                        job.append(
                                            &path[b..e],
                                            0.0,
                                            TextFormat {
                                                font_id: font.clone(),
                                                color: c,
                                                ..Default::default()
                                            },
                                        );
                                    }
                                };
                            push(&mut job, name_at, path.len(), normal);
                            if name_at > 0 {
                                job.append(
                                    "   ",
                                    0.0,
                                    TextFormat {
                                        font_id: font.clone(),
                                        color: weak,
                                        ..Default::default()
                                    },
                                );
                                push(&mut job, 0, name_at.saturating_sub(1), weak);
                            }
                            let r = ui.add(
                                egui::Button::selectable(k == q.selected, job)
                                    .min_size(egui::vec2(ui.available_width(), 0.0)),
                            );
                            if k == q.selected {
                                r.scroll_to_me(None);
                            }
                            if r.clicked() {
                                chosen = Some(*i);
                            }
                        }
                        if found.is_empty() && !listing {
                            ui.weak(if candidates.is_empty() && no_folder {
                                tr("Open a folder (Ctrl+Shift+O) to find its files here.")
                            } else {
                                tr("No file matches.")
                            });
                        }
                    });
                if truncated {
                    ui.weak(trf(
                        "Only the first {n} files of the folder are listed.",
                        &[("n", &super::filter_panel::fmt_count(MAX_FILES as u64))],
                    ));
                }
            });
        // Enter takes the text typed in this frame too.
        if enter && chosen.is_none() {
            let (name, l) = split_query(&q.query);
            if q.found_for.as_ref().map(|f| f.0.as_str()) != Some(name) {
                q.found = best(name, candidates, SHOWN);
                q.found_for = Some((name.to_string(), generation));
                q.selected = 0;
            }
            chosen = q.found.get(q.selected).map(|f| f.0);
            line = l;
        }
        if let Some(i) = chosen {
            let rel = candidates[i].clone();
            let path = match &root {
                Some(r) => r.join(rel),
                None => PathBuf::from(rel),
            };
            self.quick_open = None;
            self.open_path_at(path, line);
        } else if esc {
            self.quick_open = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_prefers_names_and_word_starts() {
        let files: Vec<String> = [
            "src/app/mod.rs",
            "src/app/folder_tree.rs",
            "src/view/text_view.rs",
            "docs/index.html",
            "src/app/find_in_files.rs",
            "README.md",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let top = |q: &str| best(q, &files, 3).first().map(|(i, _)| files[*i].clone());
        assert_eq!(top("ftree").as_deref(), Some("src/app/folder_tree.rs"));
        assert_eq!(top("tv").as_deref(), Some("src/view/text_view.rs"));
        assert_eq!(top("readme").as_deref(), Some("README.md"));
        assert_eq!(top("fif").as_deref(), Some("src/app/find_in_files.rs"));
        assert!(fuzzy("xyz", "src/app/mod.rs").is_none());
        let (_, pos) = fuzzy("mod", "src/app/mod.rs").unwrap();
        assert_eq!(pos, vec![8, 9, 10]);
        assert_eq!(best("", &files, 2).len(), 2);
    }

    #[test]
    fn line_suffix_and_listing() {
        assert_eq!(split_query("main.rs:42"), ("main.rs", Some(42)));
        assert_eq!(split_query("C:\\x"), ("C:\\x", None));
        assert_eq!(split_query(" a : "), ("a :", None));
        let dir = std::env::temp_dir().join(format!("bigtext-quick-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/sub")).unwrap();
        std::fs::create_dir_all(dir.join("node_modules/x")).unwrap();
        for f in ["a.txt", "src/b.rs", "src/sub/c.rs", "node_modules/x/d.js"] {
            std::fs::write(dir.join(f), "x").unwrap();
        }
        assert_eq!(
            walk(&dir, 10),
            (
                vec![
                    "a.txt".to_string(),
                    "src/b.rs".into(),
                    "src/sub/c.rs".into()
                ],
                false
            )
        );
        assert!(walk(&dir, 2).1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
