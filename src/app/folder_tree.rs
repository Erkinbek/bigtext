//! File → Open folder…: the folder's files as a tree in a side bar on the left, as in Sublime
//! Text. A folder is read only when it is expanded, and on a worker, so a folder of a million
//! files or a slow network drive never stalls the window; only the rows on screen are drawn.
//! A click opens a file (or the tab it is already open in). The open folder, whether the side
//! bar shows and the expanded folders are kept with the session; expanded folders are read again
//! when BigText comes back to the front.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};

use bigtext::file::display_path;
use bigtext::jobs::{self, human_bytes, Waker};

use super::i18n::{tr, trf};
use super::shell::ShellAction;
use super::BigTextApp;

/// Names never shown: version control and system clutter, as Sublime Text hides them.
const HIDDEN: &[&str] = &[".git", ".svn", ".hg", "CVS", ".DS_Store", "Thumbs.db", "desktop.ini"];
/// Expanded folders remembered at most.
pub const EXPANDED_MAX: usize = 200;
/// Indent of each level of the tree, in points.
const INDENT: f32 = 14.0;

/// A file or folder in a listed folder.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub dir: bool,
    pub size: u64,
}

enum Listing {
    Ready(Vec<Entry>),
    Failed(String),
}

/// One row of the tree as drawn.
#[derive(Clone, Debug, PartialEq)]
pub enum Row {
    Entry { depth: u16, entry: Entry, open: bool },
    /// A folder being read.
    Loading(u16),
    /// A folder that could not be read.
    Failed(u16, String),
    Empty(u16),
}

type Listed = (PathBuf, Result<Vec<Entry>, String>);

pub struct FolderUi {
    pub root: Option<PathBuf>,
    /// The side bar shows (the folder stays open when it is hidden).
    pub visible: bool,
    /// Expanded folders; the root always is.
    pub expanded: HashSet<PathBuf>,
    /// Shows only names with this text (in the folders read so far).
    pub filter: String,
    listings: HashMap<PathBuf, Listing>,
    /// Folders being read.
    pending: HashSet<PathBuf>,
    rows: Vec<Row>,
    dirty: bool,
    tx: Sender<Listed>,
    rx: Receiver<Listed>,
    /// The window had the focus last frame (coming back reads the folders again).
    was_focused: bool,
    /// Where each row was drawn last frame (the self-test clicks them).
    pub drawn: Vec<(PathBuf, egui::Rect)>,
}

impl Default for FolderUi {
    fn default() -> Self {
        let (tx, rx) = channel();
        FolderUi {
            root: None,
            visible: false,
            expanded: HashSet::new(),
            filter: String::new(),
            listings: HashMap::new(),
            pending: HashSet::new(),
            rows: Vec::new(),
            dirty: true,
            tx,
            rx,
            was_focused: true,
            drawn: Vec::new(),
        }
    }
}

/// Orders names as people do: folders first, then ignoring case, with numbers by value
/// (`file2` before `file10`).
pub fn natural(a: &str, b: &str) -> Ordering {
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let take = |it: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut s = String::new();
                    while let Some(c) = it.next_if(char::is_ascii_digit) {
                        s.push(c);
                    }
                    s
                };
                let (n, m) = (take(&mut x), take(&mut y));
                let (n, m) = (n.trim_start_matches('0'), m.trim_start_matches('0'));
                let o = n.len().cmp(&m.len()).then_with(|| n.cmp(m));
                if o != Ordering::Equal {
                    return o;
                }
            }
            (Some(c), Some(d)) => {
                let o = c.to_lowercase().cmp(d.to_lowercase());
                if o != Ordering::Equal {
                    return o;
                }
                x.next();
                y.next();
            }
        }
    }
}

/// The files and folders in `dir`, folders first.
pub fn list_dir(dir: &Path) -> Result<Vec<Entry>, String> {
    let rd = std::fs::read_dir(dir).map_err(|e| super::friendly_io(&e, dir))?;
    let mut v = Vec::new();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if HIDDEN.contains(&name.as_str()) {
            continue;
        }
        let path = e.path();
        // A link counts as what it points to.
        let meta = match e.file_type() {
            Ok(t) if t.is_symlink() => std::fs::metadata(&path).or_else(|_| e.metadata()),
            _ => e.metadata(),
        };
        let (dir, size) = meta.map(|m| (m.is_dir(), if m.is_dir() { 0 } else { m.len() })).unwrap_or((false, 0));
        v.push(Entry { name, path, dir, size });
    }
    v.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| natural(&a.name, &b.name)));
    Ok(v)
}

impl FolderUi {
    /// Reads `dir` on a worker (the old listing stays shown until the new one comes).
    fn request(&mut self, dir: PathBuf, waker: &Waker) {
        if !self.pending.insert(dir.clone()) {
            return;
        }
        self.dirty = true;
        let (tx, waker) = (self.tx.clone(), waker.clone());
        jobs::spawn("list-folder", move || {
            let r = list_dir(&dir);
            let _ = tx.send((dir, r));
            waker();
        });
    }

    /// Takes the folders the workers have read.
    fn poll(&mut self) {
        while let Ok((dir, r)) = self.rx.try_recv() {
            self.pending.remove(&dir);
            self.listings.insert(dir, r.map_or_else(Listing::Failed, Listing::Ready));
            self.dirty = true;
        }
    }

    /// The rows to draw (rebuilt only when something changed).
    pub fn rows(&mut self) -> &[Row] {
        if self.dirty {
            self.dirty = false;
            self.rows.clear();
            if let Some(root) = &self.root {
                let filter = self.filter.trim().to_lowercase();
                add_children(&self.listings, &self.expanded, root, 0, &filter, &mut self.rows);
            }
        }
        &self.rows
    }

    /// True while some folder is being read.
    pub fn busy(&self) -> bool {
        !self.pending.is_empty()
    }

    /// The expanded folders to remember: inside the open folder, the root left out.
    pub fn expanded_to_keep(&self) -> Vec<PathBuf> {
        let Some(root) = &self.root else { return Vec::new() };
        let mut v: Vec<PathBuf> = self.expanded.iter().filter(|p| *p != root && p.starts_with(root)).cloned().collect();
        v.sort();
        v.truncate(EXPANDED_MAX);
        v
    }
}

/// The rows of `dir`'s files at `depth`, and of its expanded folders below each. With a filter,
/// only matching names and the folders that lead to them; returns whether anything matched.
fn add_children(listings: &HashMap<PathBuf, Listing>, expanded: &HashSet<PathBuf>, dir: &Path, depth: u16, filter: &str, out: &mut Vec<Row>) -> bool {
    let entries = match listings.get(dir) {
        None => {
            if filter.is_empty() {
                out.push(Row::Loading(depth));
            }
            return false;
        }
        Some(Listing::Failed(e)) => {
            out.push(Row::Failed(depth, e.clone()));
            return false;
        }
        Some(Listing::Ready(v)) => v,
    };
    if entries.is_empty() && filter.is_empty() {
        out.push(Row::Empty(depth));
    }
    let mut any = false;
    for e in entries {
        let hit = filter.is_empty() || e.name.to_lowercase().contains(filter);
        if !e.dir {
            if hit {
                out.push(Row::Entry { depth, entry: e.clone(), open: false });
                any = true;
            }
            continue;
        }
        let at = out.len();
        let open = expanded.contains(&e.path);
        out.push(Row::Entry { depth, entry: e.clone(), open });
        // A filter looks into every folder read so far, open or not.
        let below = (open || (!filter.is_empty() && listings.contains_key(&e.path))) && add_children(listings, expanded, &e.path, depth + 1, filter, out);
        if !filter.is_empty() {
            if below {
                if let Some(Row::Entry { open, .. }) = out.get_mut(at) {
                    *open = true;
                }
            } else if !hit {
                out.truncate(at);
                continue;
            }
        }
        any = true;
    }
    any
}

/// What a click in the side bar asks for.
enum Act {
    Toggle(PathBuf),
    Open(PathBuf),
    Shell(PathBuf, ShellAction),
    CopyPath(PathBuf),
    FindIn(PathBuf),
    Refresh,
    CollapseAll,
    Hide,
    Close,
}

impl BigTextApp {
    /// File → Open folder…
    pub(crate) fn open_folder_dialog(&mut self) {
        let mut dlg = rfd::FileDialog::new().set_title(tr("Open folder"));
        if let Some(dir) = self.folder.root.as_deref().or_else(|| self.docs.get(self.active).and_then(|d| d.path.as_deref()).and_then(Path::parent)) {
            dlg = dlg.set_directory(dir);
        }
        if let Some(dir) = dlg.pick_folder() {
            self.open_folder(dir);
        }
    }

    /// Shows `dir` in the side bar.
    pub(crate) fn open_folder(&mut self, dir: PathBuf) {
        let root = std::path::absolute(&dir).unwrap_or(dir);
        let f = &mut self.folder;
        if f.root.as_deref() != Some(root.as_path()) {
            f.listings.clear();
            f.expanded.clear();
            f.filter.clear();
            f.root = Some(root.clone());
        }
        f.visible = true;
        f.expanded.insert(root.clone());
        f.dirty = true;
        f.request(root.clone(), &self.waker);
        self.announce(opened_text(&root));
        self.persist_state();
    }

    /// Puts back the folder of the last session.
    pub(crate) fn restore_folder(&mut self, root: PathBuf, visible: bool, expanded: Vec<PathBuf>) {
        let f = &mut self.folder;
        f.root = Some(root.clone());
        f.visible = visible;
        f.expanded = expanded.into_iter().filter(|p| p.starts_with(&root)).take(EXPANDED_MAX).collect();
        f.expanded.insert(root);
        let dirs: Vec<PathBuf> = f.expanded.iter().cloned().collect();
        for d in dirs {
            self.folder.request(d, &self.waker);
        }
    }

    /// File → Close folder.
    pub(crate) fn close_folder(&mut self) {
        let f = &mut self.folder;
        f.root = None;
        f.visible = false;
        f.listings.clear();
        f.expanded.clear();
        f.filter.clear();
        f.dirty = true;
        self.persist_state();
    }

    /// View → Side bar.
    pub(crate) fn toggle_sidebar(&mut self) {
        if self.folder.root.is_none() {
            self.open_folder_dialog();
            return;
        }
        self.folder.visible = !self.folder.visible;
        self.persist_state();
    }

    /// Reads every expanded folder again.
    pub(crate) fn refresh_folder(&mut self) {
        if let Some(root) = self.folder.root.clone() {
            self.file_index.refresh(&root, self.waker.clone(), true);
        }
        let dirs: Vec<PathBuf> = self.folder.expanded.iter().cloned().collect();
        for d in dirs {
            self.folder.request(d, &self.waker);
        }
    }

    /// Takes finished listings; reads the folders again when the window comes back to the front.
    pub(super) fn folder_tick(&mut self, ctx: &egui::Context) {
        self.folder.poll();
        let focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
        if focused && !self.folder.was_focused && self.folder.root.is_some() {
            self.refresh_folder();
        }
        self.folder.was_focused = focused;
    }

    pub(super) fn folder_panel(&mut self, ui: &mut egui::Ui) {
        let Some(root) = self.folder.root.clone() else { return };
        let mut act = None;
        let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| display_path(&root));
        ui.horizontal(|ui| {
            let head = ui.add(egui::Label::new(egui::RichText::new(&name).strong()).truncate().sense(egui::Sense::click())).on_hover_text(display_path(&root));
            head.context_menu(|ui| folder_menu(ui, &root, &mut act));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("×").on_hover_text(tr("Hide the side bar")).clicked() {
                    act = Some(Act::Hide);
                }
                if ui.small_button("⟳").on_hover_text(tr("Read the folders again")).clicked() {
                    act = Some(Act::Refresh);
                }
                if ui.small_button("⊟").on_hover_text(tr("Collapse all folders")).clicked() {
                    act = Some(Act::CollapseAll);
                }
                if self.folder.busy() {
                    ui.spinner();
                }
            });
        });
        let filter = ui.add(egui::TextEdit::singleline(&mut self.folder.filter).hint_text(tr("Filter names")).desired_width(f32::INFINITY));
        if filter.changed() {
            self.folder.dirty = true;
        }
        ui.separator();
        let active = self.docs.get(self.active).and_then(|d| d.unpacked_from.clone().or_else(|| d.path.clone()));
        let row_h = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
        // Borrowed out for the frame (a folder may have hundreds of thousands of rows).
        self.folder.rows();
        let rows = std::mem::take(&mut self.folder.rows);
        if rows.is_empty() && !self.folder.filter.trim().is_empty() {
            ui.weak(tr("No names match the filter in the folders read so far."));
        }
        let mut drawn = Vec::new();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, row_h, rows.len(), |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for row in &rows[range] {
                let depth = match row {
                    Row::Entry { depth, .. } | Row::Loading(depth) | Row::Failed(depth, _) | Row::Empty(depth) => *depth,
                };
                ui.horizontal(|ui| {
                    ui.add_space(depth as f32 * INDENT);
                    match row {
                        Row::Entry { entry, open, .. } => {
                            // A folder's triangle (drawn: the fonts lack ▾), pointing down when open.
                            let (icon, icon_r) = ui.allocate_exact_size(egui::vec2(INDENT, row_h), egui::Sense::click());
                            if entry.dir {
                                let c = icon.center();
                                let s = 3.5;
                                let points = if *open { vec![c + egui::vec2(-s, -s * 0.6), c + egui::vec2(s, -s * 0.6), c + egui::vec2(0.0, s * 0.8)] } else { vec![c + egui::vec2(-s * 0.6, -s), c + egui::vec2(s * 0.8, 0.0), c + egui::vec2(-s * 0.6, s)] };
                                ui.painter().add(egui::Shape::convex_polygon(points, ui.visuals().weak_text_color(), egui::Stroke::NONE));
                                if icon_r.clicked() {
                                    act = Some(Act::Toggle(entry.path.clone()));
                                }
                            }
                            let width = ui.available_width();
                            let selected = !entry.dir && active.as_deref().is_some_and(|a| super::same_path(a, &entry.path));
                            let b = egui::Button::selectable(selected, entry.name.as_str()).truncate().min_size(egui::vec2(width, row_h));
                            // The size on the right (and, for a folder, nothing: the name stays on the left).
                            let b = b.shortcut_text(if entry.dir { String::new() } else { human_bytes(entry.size) });
                            let r = ui.add(b).on_hover_text(display_path(&entry.path));
                            if r.clicked() {
                                act = Some(if entry.dir { Act::Toggle(entry.path.clone()) } else { Act::Open(entry.path.clone()) });
                            }
                            r.context_menu(|ui| {
                                if entry.dir {
                                    folder_menu(ui, &entry.path, &mut act);
                                } else {
                                    file_menu(ui, &entry.path, &mut act);
                                }
                            });
                            drawn.push((entry.path.clone(), r.rect));
                        }
                        Row::Loading(_) | Row::Empty(_) | Row::Failed(..) => {
                            ui.add_space(INDENT);
                            let (text, color) = match row {
                                Row::Loading(_) => (tr("Reading…").to_string(), ui.visuals().weak_text_color()),
                                Row::Empty(_) => (tr("(empty)").to_string(), ui.visuals().weak_text_color()),
                                Row::Failed(_, e) => (e.clone(), ui.visuals().error_fg_color),
                                _ => unreachable!(),
                            };
                            let r = ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), row_h), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                ui.add(egui::Label::new(egui::RichText::new(&text).color(color)).truncate());
                            });
                            r.response.on_hover_text(text);
                        }
                    }
                });
            }
        });
        self.folder.rows = rows;
        self.folder.drawn = drawn;
        match act {
            Some(Act::Toggle(dir)) => {
                let f = &mut self.folder;
                if !f.expanded.remove(&dir) {
                    f.expanded.insert(dir.clone());
                    if !f.listings.contains_key(&dir) {
                        f.request(dir, &self.waker);
                    }
                }
                f.dirty = true;
                self.persist_state();
            }
            Some(Act::Open(p)) => self.open_path(p),
            Some(Act::Shell(p, a)) => self.shell_file(&p, a),
            Some(Act::CopyPath(p)) => {
                ui.ctx().copy_text(display_path(&p));
                self.notify(tr("Copied.").into(), None);
            }
            Some(Act::FindIn(dir)) => {
                self.files.dir = display_path(&dir);
                self.open_find_in_files();
            }
            Some(Act::Refresh) => self.refresh_folder(),
            Some(Act::CollapseAll) => {
                let f = &mut self.folder;
                f.expanded.retain(|p| Some(p) == f.root.as_ref());
                f.dirty = true;
                self.persist_state();
            }
            Some(Act::Hide) => {
                self.folder.visible = false;
                self.persist_state();
            }
            Some(Act::Close) => self.close_folder(),
            None => {}
        }
    }
}

/// The right-click menu of a folder in the side bar.
fn folder_menu(ui: &mut egui::Ui, dir: &Path, act: &mut Option<Act>) {
    if ui.button(tr("Find in files…")).clicked() {
        *act = Some(Act::FindIn(dir.to_path_buf()));
    }
    if ui.button(tr("Open terminal here")).clicked() {
        *act = Some(Act::Shell(dir.to_path_buf(), ShellAction::Terminal));
    }
    if ui.button(tr("Show in folder")).clicked() {
        *act = Some(Act::Shell(dir.to_path_buf(), ShellAction::Reveal));
    }
    if ui.button(tr("Copy path")).clicked() {
        *act = Some(Act::CopyPath(dir.to_path_buf()));
    }
    ui.separator();
    if ui.button(tr("Read the folders again")).clicked() {
        *act = Some(Act::Refresh);
    }
    if ui.button(tr("Close folder")).clicked() {
        *act = Some(Act::Close);
    }
}

/// The right-click menu of a file in the side bar.
fn file_menu(ui: &mut egui::Ui, file: &Path, act: &mut Option<Act>) {
    if ui.button(tr("Open")).clicked() {
        *act = Some(Act::Open(file.to_path_buf()));
    }
    if ui.button(tr("Open with the default app")).clicked() {
        *act = Some(Act::Shell(file.to_path_buf(), ShellAction::Default));
    }
    if ui.button(tr("Show in folder")).clicked() {
        *act = Some(Act::Shell(file.to_path_buf(), ShellAction::Reveal));
    }
    if ui.button(tr("Open terminal here")).clicked() {
        *act = Some(Act::Shell(file.to_path_buf(), ShellAction::Terminal));
    }
    if ui.button(tr("Copy path")).clicked() {
        *act = Some(Act::CopyPath(file.to_path_buf()));
    }
}

/// "Opened folder {name}" for screen readers and the log.
pub(crate) fn opened_text(root: &Path) -> String {
    trf("Opened folder {name}", &[("name", &display_path(root))])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order() {
        let mut v = vec!["file10.log", "File2.log", "file1.log", "a", "B", "file02.log"];
        v.sort_by(|a, b| natural(a, b));
        assert_eq!(v, ["a", "B", "file1.log", "File2.log", "file02.log", "file10.log"]);
    }

    #[test]
    fn listing_and_rows() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        std::fs::create_dir_all(p.join("sub").join("deep")).unwrap();
        std::fs::create_dir(p.join(".git")).unwrap();
        std::fs::write(p.join("b.log"), "12345").unwrap();
        std::fs::write(p.join("a10.txt"), "").unwrap();
        std::fs::write(p.join("a9.txt"), "").unwrap();
        std::fs::write(p.join("sub").join("x.log"), "x").unwrap();
        let top = list_dir(p).unwrap();
        let names: Vec<&str> = top.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["sub", "a9.txt", "a10.txt", "b.log"]);
        assert_eq!((top[0].dir, top[3].size), (true, 5));
        assert!(list_dir(&p.join("missing")).is_err());

        let mut f = FolderUi { root: Some(p.to_path_buf()), ..Default::default() };
        f.expanded.insert(p.to_path_buf());
        assert_eq!(f.rows(), [Row::Loading(0)]);
        f.listings.insert(p.to_path_buf(), Listing::Ready(top));
        f.dirty = true;
        assert_eq!(f.rows().len(), 4);
        // Expanding a folder shows "Reading…" under it until it is read.
        f.expanded.insert(p.join("sub"));
        f.dirty = true;
        assert_eq!(f.rows()[1], Row::Loading(1));
        f.listings.insert(p.join("sub"), Listing::Ready(list_dir(&p.join("sub")).unwrap()));
        f.dirty = true;
        let rows = f.rows().to_vec();
        let shown: Vec<(u16, &str)> = rows.iter().filter_map(|r| if let Row::Entry { depth, entry, .. } = r { Some((*depth, entry.name.as_str())) } else { None }).collect();
        assert_eq!(shown, [(0, "sub"), (1, "deep"), (1, "x.log"), (0, "a9.txt"), (0, "a10.txt"), (0, "b.log")]);
        // A filter keeps the matches and the folders leading to them, open or not.
        f.expanded.remove(&p.join("sub"));
        f.filter = "X.L".into();
        f.dirty = true;
        let rows = f.rows().to_vec();
        assert_eq!(rows.len(), 2);
        assert!(matches!(&rows[0], Row::Entry { entry, open: true, .. } if entry.name == "sub"));
        assert!(matches!(&rows[1], Row::Entry { depth: 1, entry, .. } if entry.name == "x.log"));
        assert_eq!(f.expanded_to_keep(), Vec::<PathBuf>::new());
    }
}
