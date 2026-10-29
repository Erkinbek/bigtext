//! View → Structure (Ctrl+Shift+J): the outline of a JSON or XML file in a side panel, opened
//! node by node (200 children at a time, so a root array of millions of items opens at once).
//! Click a node to go to its value; type a path like `$.items[1234].id` (or, for XML,
//! `/catalog/item[1234]/price`) to jump there; copy the path of the value at the caret. View → Open JSON Lines as table shows a `.jsonl` file
//! as a table (converted to a temporary CSV with a column per key).

use egui::RichText;

use bigtext::jobs::{self, CancelToken, Progress};
use bigtext::json::{self, Child, Kind, Page, Resume};

use super::filter_panel::fmt_count;
use super::i18n::{tr, trf};
use super::tasks::TaskOutcome;
use super::{BigTextApp, Msg};

/// Children listed per page.
pub const PAGE: usize = 200;

pub struct Node {
    pub child: Child,
    pub open: bool,
    pub kids: Vec<usize>,
    /// More children to list, and whether a page is loading.
    pub resume: Option<Resume>,
    pub loaded: bool,
    pub loading: bool,
}

/// The outline of one document.
pub struct JsonTree {
    pub version: u64,
    pub nodes: Vec<Node>,
    pub error: Option<String>,
    pub path: String,
    pub status: String,
    pub busy: Option<CancelToken>,
    /// The document is XML (elements instead of JSON values).
    pub xml: bool,
}

pub enum JsonMsg {
    Page { node: usize, page: Result<Page, String> },
    Goto(Result<(u64, u64), String>),
    PathAt(Result<String, String>),
}

pub struct JsonDone {
    pub doc: u64,
    pub version: u64,
    pub msg: JsonMsg,
}

fn summary(buf: &bigtext::text::TextBuf, c: &Child, xml: bool) -> String {
    if xml {
        // An element's text: between its start and end tags.
        if c.kind == Kind::Object {
            return "…".into();
        }
        let b = buf.read(c.start..c.end.min(c.start + 400)).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
        let inner = b.split_once('>').map(|x| x.1).unwrap_or("");
        let inner = inner.rsplit_once('<').map(|x| x.0).unwrap_or(inner);
        let t: String = inner.split_whitespace().collect::<Vec<_>>().join(" ");
        return if t.chars().count() > 80 { t.chars().take(79).chain(['…']).collect() } else { t };
    }
    match c.kind {
        Kind::Object => "{…}".into(),
        Kind::Array => "[…]".into(),
        _ => {
            let end = c.end.min(c.start + 120);
            let text = buf.read(c.start..end).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
            let mut t: String = text.chars().map(|ch| if ch.is_control() { ' ' } else { ch }).take(80).collect();
            if c.end > end || text.chars().count() > 80 {
                t.push('…');
            }
            t
        }
    }
}

impl BigTextApp {
    /// Shows or hides the JSON structure panel; the outline starts with the top-level value.
    pub(crate) fn toggle_json_panel(&mut self) {
        self.json_open = !self.json_open;
        if self.json_open {
            self.ensure_json_tree();
        }
    }

    fn ensure_json_tree(&mut self) {
        let Some(d) = self.docs.get_mut(self.active) else { return };
        if !d.is_ready() || d.json.as_ref().is_some_and(|t| t.version == d.version) {
            return;
        }
        let xml = d.effective_lang() == crate::view::highlight::Lang::Xml;
        let mut tree = JsonTree { version: d.version, nodes: Vec::new(), error: None, path: String::new(), status: String::new(), busy: None, xml };
        let root = if xml { bigtext::xml::root(&d.buf, &CancelToken::new()) } else { json::root(&d.buf, &CancelToken::new()) };
        match root {
            Ok(at) => {
                let kind = d.buf.read(at..at + 1).map(|b| match b[0] {
                    b'{' => Kind::Object,
                    b'[' => Kind::Array,
                    b'<' if xml => Kind::Object,
                    _ => Kind::Other,
                });
                let kind = kind.unwrap_or(Kind::Other);
                tree.nodes.push(Node { child: Child { key: None, index: 0, start: at, end: d.buf.len(), kind }, open: false, kids: Vec::new(), resume: None, loaded: false, loading: false });
                if kind == Kind::Other {
                    tree.error = Some(tr("This is not a JSON object or array.").into());
                }
            }
            Err(e) => tree.error = Some(e),
        }
        d.json = Some(tree);
        self.expand_json(0);
    }

    /// Opens node `i` and lists (more of) its children in the background.
    fn expand_json(&mut self, i: usize) {
        let Some(d) = self.docs.get_mut(self.active) else { return };
        let Some(t) = d.json.as_mut() else { return };
        let Some(n) = t.nodes.get_mut(i) else { return };
        if !matches!(n.child.kind, Kind::Object | Kind::Array) || n.loading {
            return;
        }
        n.open = true;
        if n.loaded && n.resume.is_none() {
            return;
        }
        n.loading = true;
        let (at, resume, xml) = (n.child.start, n.resume, t.xml);
        let (snap, id, version) = (d.buf.snapshot(), d.id, d.version);
        let (tx, waker) = (self.tx.clone(), self.waker.clone());
        jobs::spawn("json-children", move || {
            let page = if xml { bigtext::xml::children(&snap, at, resume, PAGE, &CancelToken::new()) } else { json::children(&snap, at, resume, PAGE, &CancelToken::new()) };
            let _ = tx.send(Msg::Json(Box::new(JsonDone { doc: id, version, msg: JsonMsg::Page { node: i, page } })));
            waker();
        });
    }

    fn json_job(&mut self, f: impl FnOnce(&bigtext::text::TextBuf, &CancelToken) -> JsonMsg + Send + 'static) {
        let Some(d) = self.docs.get_mut(self.active) else { return };
        let Some(t) = d.json.as_mut() else { return };
        if let Some(c) = t.busy.take() {
            c.cancel();
        }
        let cancel = CancelToken::new();
        t.busy = Some(cancel.clone());
        t.status.clear();
        let (snap, id, version) = (d.buf.snapshot(), d.id, d.version);
        let (tx, waker) = (self.tx.clone(), self.waker.clone());
        jobs::spawn("json-path", move || {
            let msg = f(&snap, &cancel);
            if !cancel.is_cancelled() {
                let _ = tx.send(Msg::Json(Box::new(JsonDone { doc: id, version, msg })));
                waker();
            }
        });
    }

    /// Goes to the value at the path typed in the panel.
    pub(crate) fn json_goto(&mut self, path: &str) {
        if self.docs.get(self.active).and_then(|d| d.json.as_ref()).is_some_and(|t| t.xml) {
            match bigtext::xml::parse_path(path) {
                Ok(steps) => self.json_job(move |buf, cancel| JsonMsg::Goto(bigtext::xml::resolve(buf, &steps, cancel))),
                Err(e) => {
                    if let Some(t) = self.docs.get_mut(self.active).and_then(|d| d.json.as_mut()) {
                        t.status = e;
                    }
                }
            }
            return;
        }
        let steps = match json::parse_path(path) {
            Ok(s) => s,
            Err(e) => {
                if let Some(t) = self.docs.get_mut(self.active).and_then(|d| d.json.as_mut()) {
                    t.status = e;
                }
                return;
            }
        };
        self.json_job(move |buf, cancel| JsonMsg::Goto(json::resolve(buf, &steps, cancel)));
    }

    /// Finds the path of the value at the caret (and copies it).
    pub(crate) fn json_path_at_caret(&mut self) {
        let Some(caret) = self.docs.get(self.active).map(|d| d.view.caret) else { return };
        if self.docs.get(self.active).and_then(|d| d.json.as_ref()).is_some_and(|t| t.xml) {
            self.json_job(move |buf, cancel| JsonMsg::PathAt(bigtext::xml::path_at(buf, caret, cancel).map(|s| bigtext::xml::format_path(&s))));
            return;
        }
        self.json_job(move |buf, cancel| JsonMsg::PathAt(json::path_at(buf, caret, cancel).map(|s| json::format_path(&s))));
    }

    pub(super) fn on_json(&mut self, m: JsonDone) {
        let ctx = self.ctx_clone();
        let Some(d) = self.docs.iter_mut().find(|d| d.id == m.doc) else { return };
        let len = d.buf.len();
        let Some(t) = d.json.as_mut().filter(|t| t.version == m.version) else { return };
        match m.msg {
            JsonMsg::Page { node, page } => {
                let Some(n) = t.nodes.get_mut(node) else { return };
                n.loading = false;
                n.loaded = true;
                match page {
                    Ok(p) => {
                        n.resume = p.resume;
                        let first = t.nodes.len();
                        let kids: Vec<Node> = p.items.into_iter().map(|child| Node { child, open: false, kids: Vec::new(), resume: None, loaded: false, loading: false }).collect();
                        let count = kids.len();
                        t.nodes.extend(kids);
                        t.nodes[node].kids.extend(first..first + count);
                    }
                    Err(e) => t.error = Some(e),
                }
            }
            JsonMsg::Goto(r) => {
                t.busy = None;
                match r {
                    Ok((s, e)) => {
                        d.select(s.min(len), e.min(len));
                        d.view.center();
                    }
                    Err(e) => t.status = e,
                }
            }
            JsonMsg::PathAt(r) => {
                t.busy = None;
                match r {
                    Ok(p) => {
                        if let Some(ctx) = ctx {
                            ctx.copy_text(p.clone());
                        }
                        t.status = trf("Copied {path}", &[("path", &p)]);
                        t.path = p;
                    }
                    Err(e) => t.status = e,
                }
            }
        }
    }

    pub(super) fn json_panel(&mut self, ui: &mut egui::Ui) {
        self.ensure_json_tree();
        let mut expand = None;
        let mut collapse = None;
        let mut jump = None;
        let mut goto = None;
        let mut path_at = false;
        let mut fields = false;
        {
            let Some(d) = self.docs.get_mut(self.active) else { return };
            let buf = d.buf.snapshot();
            let Some(t) = d.json.as_mut() else { return };
            ui.horizontal(|ui| {
                ui.strong(tr(if t.xml { "XML structure" } else { "JSON structure" }));
                if t.busy.is_some() {
                    ui.spinner();
                }
            });
            ui.horizontal(|ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut t.path).hint_text(if t.xml { "/catalog/item[1]/price" } else { "$.items[0].id" }).desired_width(ui.available_width() - 40.0).font(egui::TextStyle::Monospace));
                if (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) | ui.button(tr("Go")).clicked() {
                    goto = Some(t.path.clone());
                }
            });
            ui.horizontal(|ui| {
                if ui.button(tr("Copy path at caret")).clicked() {
                    path_at = true;
                }
                if !t.xml && ui.button(tr("Fields…")).on_hover_text(tr("Every key path of the records with its types and values")).clicked() {
                    fields = true;
                }
            });
            if !t.status.is_empty() {
                ui.label(RichText::new(&t.status).small());
            }
            if let Some(e) = &t.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            ui.separator();
            egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                // Depth-first over the open nodes.
                let mut stack: Vec<(usize, usize)> = if t.nodes.is_empty() { Vec::new() } else { vec![(0, 0)] };
                while let Some((i, depth)) = stack.pop() {
                    let n = &t.nodes[i];
                    ui.horizontal(|ui| {
                        ui.add_space(depth as f32 * 14.0);
                        let container = matches!(n.child.kind, Kind::Object | Kind::Array);
                        if container {
                            if ui.small_button(RichText::new(if n.open { "-" } else { "+" }).monospace()).clicked() {
                                if n.open {
                                    collapse = Some(i);
                                } else {
                                    expand = Some(i);
                                }
                            }
                        } else {
                            ui.add_space(18.0);
                        }
                        let name = match (&n.child.key, i) {
                            (_, 0) if t.xml => "/".to_string(),
                            (_, 0) => "$".to_string(),
                            (Some(k), _) => k.clone(),
                            (None, _) => format!("[{}]", n.child.index),
                        };
                        let text = format!("{name}: {}", summary(&buf, &n.child, t.xml));
                        if ui.selectable_label(false, RichText::new(text).monospace()).clicked() {
                            jump = Some(n.child.start);
                        }
                        if n.loading {
                            ui.spinner();
                        }
                    });
                    if n.open {
                        if n.resume.is_some() && !n.loading {
                            stack.push((usize::MAX - i, depth + 1));
                        }
                        for &k in n.kids.iter().rev() {
                            stack.push((k, depth + 1));
                        }
                    }
                    // "More" rows come out of the stack as usize::MAX - parent.
                    while let Some(&(m, md)) = stack.last() {
                        if m < t.nodes.len() {
                            break;
                        }
                        stack.pop();
                        let parent = usize::MAX - m;
                        ui.horizontal(|ui| {
                            ui.add_space(md as f32 * 14.0 + 18.0);
                            let shown = t.nodes[parent].kids.len();
                            if ui.small_button(trf("Show {n} more (after {shown})", &[("n", &PAGE.to_string()), ("shown", &fmt_count(shown as u64))])).clicked() {
                                expand = Some(parent);
                            }
                        });
                    }
                }
            });
            if let Some(i) = collapse {
                t.nodes[i].open = false;
            }
        }
        if let Some(i) = expand {
            self.expand_json(i);
        }
        if let Some(s) = jump {
            if let Some(d) = self.docs.get_mut(self.active) {
                d.goto_offset(s);
            }
        }
        if let Some(p) = goto {
            self.json_goto(&p);
        }
        if path_at {
            self.json_path_at_caret();
        }
        if fields {
            self.start_json_fields();
        }
    }

    /// View → Open JSON Lines as table: converts the document to a temporary CSV and opens it.
    pub(crate) fn open_jsonl_table(&mut self) {
        let Some(d) = self.docs.get(self.active) else { return };
        if !d.is_ready() {
            return;
        }
        let snap = d.buf.snapshot();
        let stem = std::path::Path::new(&d.title).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "lines".into());
        let target = bigtext::decompress::temp_file(&format!("{stem}.csv"));
        let title = trf("{name} (table)", &[("name", &d.title)]);
        self.spawn_task(tr("Converting JSON Lines").into(), move |progress: &Progress, cancel: &CancelToken| {
            let r = (|| {
                if let Some(dir) = target.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                let file = std::fs::File::options().write(true).create_new(true).open(&target)?;
                let mut w = std::io::BufWriter::with_capacity(8 << 20, file);
                let st = json::jsonl_to_csv(&snap, &mut w, cancel, Some(progress))?;
                std::io::Write::flush(&mut w)?;
                Ok::<_, std::io::Error>(st)
            })();
            match r {
                Ok(Some(st)) => {
                    let mut message = trf("{rows} rows, {cols} columns.", &[("rows", &fmt_count(st.rows)), ("cols", &st.columns.to_string())]);
                    if st.skipped > 0 {
                        message.push(' ');
                        message.push_str(&trf("{n} lines that are not JSON objects were left out.", &[("n", &fmt_count(st.skipped))]));
                    }
                    if st.late_keys > 0 {
                        message.push(' ');
                        message.push_str(&trf("{n} values of keys first seen after line {lines} were left out.", &[("n", &fmt_count(st.late_keys)), ("lines", &json::JSONL_SAMPLE.to_string())]));
                    }
                    TaskOutcome::TempTable(Box::new(super::tasks::TempTable { path: target, title, message }))
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
