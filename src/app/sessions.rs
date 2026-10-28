//! Named sessions (File → Sessions): the open files with their positions, bookmarks and pins,
//! each tab's filter, and the highlights, saved under a name such as "Incident 2026-10-28" and
//! opened again later. And Edit → Copy file:line, a `path:line` reference that
//! `bigtext path:line` opens at that line.

#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;

use bigtext::file;
use bigtext::loglevel::Levels;
use bigtext::search::Query;

use super::document::Restore;
use super::extras::{self, TabState};
use super::i18n::{tr, trf};
use super::marks::Mark;
use super::BigTextApp;

/// A tab's filter, to run again when the session opens.
#[derive(Clone, Debug, PartialEq)]
pub struct SavedFilter {
    pub query: Query,
    pub invert: bool,
    pub levels: Option<Levels>,
}

#[derive(Clone, Debug, Default)]
pub struct Session {
    pub tabs: Vec<(TabState, bool, Option<SavedFilter>)>,
    pub active: usize,
    pub marks: Vec<Mark>,
}

#[derive(Default)]
pub struct SessionsUi {
    pub name: String,
}

/// The folder sessions are kept in (next to the state file).
pub fn sessions_dir() -> Option<PathBuf> {
    Some(extras::state_file()?.parent()?.join("sessions"))
}

/// A file name for a session name (letters, digits, spaces, `-`, `_` and `.` kept).
fn file_for(name: &str) -> Option<PathBuf> {
    let safe: String = name.trim().chars().map(|c| if c.is_alphanumeric() || " -_.".contains(c) { c } else { '_' }).collect();
    let safe = safe.trim_matches(['.', ' ']).to_string();
    (!safe.is_empty()).then(|| sessions_dir().map(|d| d.join(format!("{safe}.session")))).flatten()
}

/// Saved sessions by name, newest first.
pub fn list() -> Vec<String> {
    let Some(dir) = sessions_dir() else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut v: Vec<(std::time::SystemTime, String)> = entries
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let name = p.file_stem()?.to_string_lossy().into_owned();
            (p.extension()? == "session").then(|| (e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH), name))
        })
        .collect();
    v.sort_by_key(|x| std::cmp::Reverse(x.0));
    v.into_iter().map(|x| x.1).collect()
}

pub fn serialize(s: &Session) -> String {
    let mut out = String::from("# BigText session\n");
    for (i, (t, pinned, filter)) in s.tabs.iter().enumerate() {
        out.push_str(&format!("tab={}\n", extras::tab_line(t)));
        if *pinned {
            out.push_str(&format!("pinned={i}\n"));
        }
        if let Some(f) = filter.as_ref().filter(|f| !f.query.pattern.contains(['\n', '\r'])) {
            let levels = f.levels.map(|l| l.bits().to_string()).unwrap_or_default();
            out.push_str(&format!("filter={i}|{levels}|{}|{}|{}\n", u8::from(f.invert), extras::query_flags(&f.query), f.query.pattern));
        }
    }
    for m in &s.marks {
        out.push_str(&format!("mark={}\n", extras::mark_line(m)));
    }
    out.push_str(&format!("active={}\n", s.active));
    out
}

pub fn parse(text: &str) -> Session {
    let mut s = Session::default();
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        match k {
            "tab" => s.tabs.extend(extras::parse_tab(v).map(|t| (t, false, None))),
            "pinned" => {
                if let Some(t) = v.parse::<usize>().ok().and_then(|i| s.tabs.get_mut(i)) {
                    t.1 = true;
                }
            }
            "filter" => {
                let mut it = v.splitn(5, '|');
                let (Some(i), Some(levels), Some(invert), Some(flags), Some(pattern)) = (it.next(), it.next(), it.next(), it.next(), it.next()) else { continue };
                let f = SavedFilter { query: extras::query_from(pattern.to_string(), flags), invert: invert == "1", levels: levels.parse().ok().map(Levels::from_bits) };
                if let Some(t) = i.parse::<usize>().ok().and_then(|i| s.tabs.get_mut(i)) {
                    t.2 = Some(f);
                }
            }
            "mark" => s.marks.extend(extras::parse_mark(v)),
            "active" => s.active = v.parse().unwrap_or(0),
            _ => {}
        }
    }
    s
}

impl BigTextApp {
    /// The open files (not temporary ones), their state, and the highlights.
    pub(crate) fn current_session(&self) -> Session {
        let mut s = Session { marks: self.settings.marks.clone(), ..Default::default() };
        for (i, d) in self.docs.iter().enumerate() {
            if d.temporary && d.unpacked_from.is_none() {
                continue;
            }
            let Some(path) = d.unpacked_from.as_deref().or(d.path.as_deref()) else { continue };
            if i == self.active {
                s.active = s.tabs.len();
            }
            let tab = TabState { path: PathBuf::from(file::display_path(path)), caret: d.view.caret, top: d.view.top, wrap: d.view.wrap, bookmarks: d.bookmarks.clone() };
            let filter = d.filter.as_ref().map(|f| SavedFilter { query: f.query.clone(), invert: f.spec.invert, levels: f.levels });
            s.tabs.push((tab, d.pinned, filter));
        }
        s
    }

    /// Saves the current session under `name` (replacing one of that name).
    pub(crate) fn save_session(&mut self, name: &str) -> Result<PathBuf, String> {
        let path = file_for(name).ok_or_else(|| tr("Type a name for the session.").to_string())?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, serialize(&self.current_session())).map_err(|e| super::friendly_io(&e, &path))?;
        Ok(path)
    }

    /// Opens the files of session `name` (already open ones keep their tabs) at their
    /// positions, with their filters, and adds its highlights.
    pub(crate) fn open_session(&mut self, name: &str) {
        let Some(path) = file_for(name) else { return };
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                self.error = Some(super::friendly_io(&e, &path));
                return;
            }
        };
        self.apply_session(parse(&text));
        self.notify(trf("Opened the session “{name}”.", &[("name", name)]), None);
    }

    pub(crate) fn apply_session(&mut self, s: Session) {
        for m in s.marks {
            if !self.settings.marks.iter().any(|x| x.query == m.query) {
                self.settings.marks.push(m);
            }
        }
        let mut active = None;
        for (i, (t, pinned, filter)) in s.tabs.into_iter().enumerate() {
            self.open_path(t.path.clone());
            let Some(d) = self.docs.get_mut(self.active) else { continue };
            d.pinned |= pinned;
            if i == s.active {
                active = Some(d.id);
            }
            if d.is_ready() {
                let len = d.buf.len();
                d.set_caret(t.caret.min(len), false);
                d.view.top = t.top.min(len);
                d.view.wrap = t.wrap;
                d.bookmarks = t.bookmarks.into_iter().filter(|&b| b <= len).collect();
                d.pending_filter = filter;
            } else {
                d.restore = Some(Restore { caret: t.caret, top: t.top, wrap: t.wrap, bookmarks: t.bookmarks, filter, ..Default::default() });
            }
        }
        let current = self.docs.get(self.active).map(|d| d.id);
        self.docs.sort_by_key(|d| !d.pinned);
        if let Some(i) = active.or(current).and_then(|id| self.docs.iter().position(|d| d.id == id)) {
            self.active = i;
        }
        self.persist_state();
    }

    /// Runs the filters of tabs restored from a session once they have opened.
    pub(super) fn run_pending_filters(&mut self) {
        let Some(i) = self.docs.iter().position(|d| d.pending_filter.is_some() && d.is_ready()) else { return };
        let Some(f) = self.docs[i].pending_filter.take() else { return };
        let active = self.active;
        self.active = i;
        match f.levels {
            Some(lv) => self.filter_levels(lv),
            None => {
                if let Ok(m) = bigtext::search::Matcher::new(&f.query, self.docs[i].buf.codec) {
                    let spec = bigtext::filter::FilterSpec { invert: f.invert, column: None };
                    self.run_filter(f.query, std::sync::Arc::new(m), spec, None);
                }
            }
        }
        self.active = active;
    }

    pub(crate) fn delete_session(&mut self, name: &str) {
        if let Some(p) = file_for(name) {
            let _ = std::fs::remove_file(p);
        }
    }

    /// File → Sessions: save the current one, open or delete saved ones.
    pub(super) fn sessions_menu(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(280.0);
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.sessions.name).desired_width(170.0).hint_text(tr("Name")));
            if ui.add_enabled(!self.sessions.name.trim().is_empty(), egui::Button::new(tr("Save session"))).on_hover_text(tr("Saves the open files with their positions, bookmarks, pins and filters, and the highlights")).clicked() {
                let name = std::mem::take(&mut self.sessions.name);
                match self.save_session(&name) {
                    Ok(_) => self.notify(trf("Saved the session “{name}”.", &[("name", &name)]), None),
                    Err(e) => self.error = Some(e),
                }
            }
        });
        let names = list();
        if !names.is_empty() {
            ui.separator();
        }
        let (mut open, mut delete) = (None, None);
        for n in names {
            ui.horizontal(|ui| {
                if ui.button(&n).clicked() {
                    open = Some(n.clone());
                }
                if ui.small_button("×").on_hover_text(tr("Delete")).clicked() {
                    delete = Some(n.clone());
                }
            });
        }
        if let Some(n) = open {
            ui.close();
            self.open_session(&n);
        }
        if let Some(n) = delete {
            self.delete_session(&n);
        }
    }

    /// Edit → Copy file:line: "C:\logs\app.log:1234" for the caret's line.
    pub(crate) fn copy_file_line(&mut self, ctx: &egui::Context) {
        let Some(d) = self.docs.get(self.active) else { return };
        let Some(p) = d.unpacked_from.as_deref().or(d.path.as_deref()).filter(|_| !d.temporary || d.unpacked_from.is_some()) else {
            self.notify(tr("Save the document first: it has no file.").into(), None);
            return;
        };
        let (line, exact) = d.buf.line_of(d.view.caret);
        let text = format!("{}:{}", file::display_path(p), line + 1);
        ctx.copy_text(text.clone());
        let note = if exact { trf("Copied {text}", &[("text", &text)]) } else { trf("Copied {text} (the line number is an estimate until indexing ends)", &[("text", &text)]) };
        self.notify(note, None);
    }
}

/// Whether `p` is inside the sessions folder.
#[cfg(test)]
fn in_sessions_dir(p: &Path) -> bool {
    sessions_dir().is_some_and(|d| p.starts_with(d))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bigtext::loglevel::Level;

    #[test]
    fn round_trip() {
        let s = Session {
            tabs: vec![
                (TabState { path: PathBuf::from(r"C:\logs\a|b.log"), caret: 10, top: 5, wrap: true, bookmarks: vec![1, 2] }, true, Some(SavedFilter { query: Query { pattern: "ERROR|WARN".into(), regex: true, case_sensitive: true, whole_word: false, dot_all: false }, invert: true, levels: None })),
                (TabState { path: PathBuf::from("/var/log/x.log"), caret: 0, top: 0, wrap: false, bookmarks: vec![] }, false, Some(SavedFilter { query: Query::default(), invert: false, levels: Some(Levels::only(Level::Error).toggled(Level::Warn)) })),
            ],
            active: 1,
            marks: vec![Mark { query: Query { pattern: "id=\\d+".into(), regex: true, ..Default::default() }, color: 2, enabled: true }],
        };
        let back = parse(&serialize(&s));
        assert_eq!(back.tabs.len(), 2);
        assert_eq!(back.tabs[0].0.path, s.tabs[0].0.path);
        assert_eq!((back.tabs[0].1, back.tabs[1].1), (true, false));
        assert_eq!(back.tabs[0].2, s.tabs[0].2);
        assert_eq!(back.tabs[1].2.as_ref().and_then(|f| f.levels), Some(Levels::only(Level::Error).toggled(Level::Warn)));
        assert_eq!((back.active, back.marks.clone()), (1, s.marks.clone()));
    }

    #[test]
    fn names_become_safe_file_names() {
        let Some(p) = file_for("Incident: 2026/10/28") else { return };
        assert_eq!(p.file_name().unwrap().to_string_lossy(), "Incident_ 2026_10_28.session");
        assert!(file_for(" .. ").is_none());
        assert!(in_sessions_dir(&p));
    }
}
