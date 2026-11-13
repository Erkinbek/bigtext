//! File → Open container or service log…: a picker of Docker (or Podman) containers,
//! Kubernetes pods and systemd services, on this computer or on a server over SSH. The chosen
//! one's log opens read-only, followed as it grows (`docker logs -f`, `kubectl logs -f`,
//! `journalctl -f`) or read once; closing a followed tab ends its command. Only lists and logs
//! are read: nothing is started, stopped or changed.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bigtext::jobs::{self, CancelToken, Progress};
use bigtext::logsources::{self as ls, Amount, Container, Host, Pod, Target, Unit};

use super::i18n::{tr, trf};
use super::tasks::TaskOutcome;
use super::BigTextApp;

/// A listing command gives up after this long.
const LIST_TIMEOUT: Duration = Duration::from_secs(30);
/// The rows of the whole journal and of the kernel's messages.
const JOURNAL: &str = "\u{0}journal";
const KERNEL: &str = "\u{0}kernel";
/// Rows of the list shown before it scrolls.
const LIST_HEIGHT: f32 = 260.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Tab {
    #[default]
    Docker,
    Kubernetes,
    Journal,
}

impl Tab {
    fn id(self) -> &'static str {
        match self {
            Tab::Docker => "docker",
            Tab::Kubernetes => "kubernetes",
            Tab::Journal => "journal",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HowMuch {
    Lines,
    Minutes,
    All,
}

/// What a listing is of.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ListKey {
    host: Host,
    tab: Tab,
    tool: String,
    context: Option<String>,
}

#[derive(Default)]
pub struct Listing {
    pub loading: bool,
    pub error: Option<String>,
    pub containers: Vec<Container>,
    pub pods: Vec<Pod>,
    pub units: Vec<Unit>,
    pub contexts: Vec<String>,
    pub current_context: Option<String>,
    /// Listing every namespace was not allowed, so these are the context's own namespace's.
    pub one_namespace: bool,
}

/// A row of the list: what it chooses, its text and how it looks.
pub struct Row {
    pub key: String,
    pub text: String,
    pub look: Look,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Look {
    Normal,
    /// Stopped, done or waiting.
    Weak,
    /// Failing.
    Bad,
}

pub struct SourcesUi {
    pub open: bool,
    /// The server field as typed, and as used (after Enter or Refresh).
    pub host: String,
    applied: String,
    pub tab: Tab,
    /// `docker` or `podman`.
    pub tool: String,
    /// None: the kubeconfig's current context.
    pub context: Option<String>,
    /// None: all namespaces.
    pub namespace: Option<String>,
    pub filter: String,
    /// The chosen row: a container id, `namespace/pod`, a unit, or [`JOURNAL`] / [`KERNEL`].
    pub selected: Option<String>,
    /// The pod's container (None: all of them), for the pod in `container_of`.
    pub container: Option<String>,
    container_of: Option<String>,
    pub previous: bool,
    pub how: HowMuch,
    pub lines: u32,
    pub minutes: u32,
    pub follow: bool,
    pub timestamps: bool,
    key: Option<ListKey>,
    pub list: Arc<Mutex<Listing>>,
    pub error: Option<String>,
}

impl Default for SourcesUi {
    fn default() -> Self {
        SourcesUi {
            open: false,
            host: String::new(),
            applied: String::new(),
            tab: Tab::Docker,
            tool: "docker".into(),
            context: None,
            namespace: None,
            filter: String::new(),
            selected: None,
            container: None,
            container_of: None,
            previous: false,
            how: HowMuch::Lines,
            lines: 10_000,
            minutes: 60,
            follow: true,
            timestamps: false,
            key: None,
            list: Arc::default(),
            error: None,
        }
    }
}

impl SourcesUi {
    /// Self-test: the chosen container is the pod `key`'s.
    pub(crate) fn container_of_for_test(&mut self, key: &str) {
        self.container_of = Some(key.to_string());
    }

    /// The choices kept between runs, as one line: server|tab|tool|how|lines|minutes|follow|timestamps.
    pub fn to_line(&self) -> String {
        let how = match self.how {
            HowMuch::Lines => "lines",
            HowMuch::Minutes => "minutes",
            HowMuch::All => "all",
        };
        let b = |x: bool| if x { "1" } else { "0" };
        format!("{}|{}|{}|{how}|{}|{}|{}|{}", self.applied.replace('|', ""), self.tab.id(), self.tool, self.lines, self.minutes, b(self.follow), b(self.timestamps))
    }

    pub fn set_line(&mut self, line: &str) {
        let f: Vec<&str> = line.split('|').collect();
        if f.len() < 8 {
            return;
        }
        self.host = f[0].to_string();
        self.applied = f[0].to_string();
        self.tab = match f[1] {
            "kubernetes" => Tab::Kubernetes,
            "journal" => Tab::Journal,
            _ => Tab::Docker,
        };
        self.tool = if f[2] == "podman" { "podman".into() } else { "docker".into() };
        self.how = match f[3] {
            "minutes" => HowMuch::Minutes,
            "all" => HowMuch::All,
            _ => HowMuch::Lines,
        };
        self.lines = f[4].parse().unwrap_or(10_000).clamp(1, 100_000_000);
        self.minutes = f[5].parse().unwrap_or(60).clamp(1, 1_000_000);
        self.follow = f[6] != "0";
        self.timestamps = f[7] == "1";
    }

    /// The list's rows for the tab (the pods of the chosen namespace).
    pub fn rows(&self, l: &Listing) -> Vec<Row> {
        let mut rows = Vec::new();
        match self.tab {
            Tab::Docker => {
                for c in &l.containers {
                    let look = match c.state.as_str() {
                        "running" => Look::Normal,
                        "restarting" | "dead" => Look::Bad,
                        _ => Look::Weak,
                    };
                    rows.push(Row { key: c.id.clone(), text: format!("{:<32} {:<10} {:<26} {}", c.name, c.state, c.status, c.image), look });
                }
            }
            Tab::Kubernetes => {
                for p in l.pods.iter().filter(|p| self.namespace.as_ref().is_none_or(|n| *n == p.namespace)) {
                    let look = match p.status.as_str() {
                        "Running" => Look::Normal,
                        "Succeeded" | "Completed" | "Pending" | "Terminating" | "ContainerCreating" | "PodInitializing" => Look::Weak,
                        _ => Look::Bad,
                    };
                    let text = format!("{:<16} {:<40} {:>5} {:<18} {:>8}", p.namespace, p.name, format!("{}/{}", p.ready.0, p.ready.1), p.status, p.restarts);
                    rows.push(Row { key: format!("{}/{}", p.namespace, p.name), text, look });
                }
            }
            Tab::Journal => {
                rows.push(Row { key: JOURNAL.into(), text: tr("The whole journal").into(), look: Look::Normal });
                rows.push(Row { key: KERNEL.into(), text: tr("Kernel messages").into(), look: Look::Normal });
                for u in &l.units {
                    let look = match u.active.as_str() {
                        "active" => Look::Normal,
                        "failed" => Look::Bad,
                        _ => Look::Weak,
                    };
                    rows.push(Row { key: u.name.clone(), text: format!("{:<40} {:<9} {:<9} {}", u.name, u.active, u.sub, u.description), look });
                }
            }
        }
        rows
    }

    fn amount(&self) -> Amount {
        match self.how {
            HowMuch::Lines => Amount::Lines(self.lines),
            HowMuch::Minutes => Amount::Minutes(self.minutes),
            HowMuch::All => Amount::All,
        }
    }

    /// The pod's container as chosen: all of them, or by default its first one.
    fn pod_container(&self, p: &Pod) -> Option<String> {
        let key = format!("{}/{}", p.namespace, p.name);
        if self.container_of.as_deref() == Some(&key) {
            return self.container.clone();
        }
        p.containers.first().cloned()
    }

    /// What the chosen row's log is.
    fn target(&self, l: &Listing) -> Option<Target> {
        let sel = self.selected.as_deref()?;
        Some(match self.tab {
            Tab::Docker => {
                let c = l.containers.iter().find(|c| c.id == sel)?;
                Target::Container { tool: self.tool.clone(), id: c.id.clone(), name: c.name.clone() }
            }
            Tab::Kubernetes => {
                let p = l.pods.iter().find(|p| format!("{}/{}", p.namespace, p.name) == sel)?;
                // One container needs no name (and no prefix on each line).
                let container = if p.containers.len() + p.init_containers.len() <= 1 { None } else { self.pod_container(p) };
                let context = self.context.clone();
                Target::Pod { context, namespace: p.namespace.clone(), pod: p.name.clone(), container, previous: self.previous }
            }
            Tab::Journal if sel == JOURNAL => Target::Journal { unit: None, kernel: false },
            Tab::Journal if sel == KERNEL => Target::Journal { unit: None, kernel: true },
            Tab::Journal => Target::Journal { unit: Some(sel.to_string()), kernel: false },
        })
    }
}

/// A listing error in the interface's language.
fn explain(e: String) -> String {
    match e.strip_suffix(" was not found") {
        Some("ssh") => tr("ssh was not found. Install the OpenSSH client (on Windows: Settings → System → Optional features).").to_string(),
        Some(p) if !p.contains(' ') => trf("{program} was not found on this computer. Install it, or give a server to run it on.", &[("program", p)]),
        _ => super::remote::ssh_error(&e, None),
    }
}

/// Reads a listing (in the background).
fn read_listing(key: &ListKey) -> Listing {
    let mut out = Listing::default();
    let starter = |p: &str| if matches!(key.host, Host::Ssh { .. }) { "ssh".to_string() } else { p.to_string() };
    let run = |program: &str, args: Vec<String>| ls::run_list(key.host.command(program, &args), &starter(program), LIST_TIMEOUT);
    match key.tab {
        Tab::Docker => match run(&key.tool, ls::containers_args()) {
            Ok(s) => out.containers = ls::parse_containers(&s),
            Err(e) => out.error = Some(e),
        },
        Tab::Kubernetes => {
            match run("kubectl", ls::contexts_args()) {
                Ok(s) => out.contexts = s.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect(),
                Err(e) => {
                    out.error = Some(e);
                    return out;
                }
            }
            if out.contexts.is_empty() {
                out.error = Some(tr("The kubeconfig has no contexts: connect kubectl to a cluster first.").to_string());
                return out;
            }
            out.current_context = run("kubectl", ls::current_context_args()).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
            let mut r = run("kubectl", ls::pods_args(key.context.as_deref(), true));
            if r.as_ref().is_err_and(|e| e.to_ascii_lowercase().contains("forbidden")) {
                r = run("kubectl", ls::pods_args(key.context.as_deref(), false));
                out.one_namespace = true;
            }
            match r.and_then(|s| ls::parse_pods(&s)) {
                Ok(p) => out.pods = p,
                Err(e) => out.error = Some(e),
            }
        }
        Tab::Journal => match run("systemctl", ls::units_args()) {
            Ok(s) => out.units = ls::parse_units(&s),
            Err(e) => out.error = Some(e),
        },
    }
    out
}

impl BigTextApp {
    /// File → Open container or service log…
    pub(crate) fn open_log_sources(&mut self) {
        self.log_sources.open = true;
        self.log_sources.error = None;
        if self.log_sources.key.is_none() {
            self.load_log_sources();
        }
    }

    /// Lists the containers, pods or services of the tab, on the server in the field.
    pub(crate) fn load_log_sources(&mut self) {
        let s = &mut self.log_sources;
        s.applied = s.host.trim().to_string();
        let Some(host) = Host::parse(&s.applied) else {
            s.error = Some(tr("Not a server address: user@server, server or user@server:2222.").into());
            return;
        };
        s.error = None;
        let key = ListKey { host, tab: s.tab, tool: s.tool.clone(), context: s.context.clone() };
        s.key = Some(key.clone());
        let list = Arc::new(Mutex::new(Listing { loading: true, ..Default::default() }));
        s.list = list.clone();
        let waker = self.waker.clone();
        jobs::spawn("log-sources", move || {
            let l = read_listing(&key);
            *list.lock().unwrap() = l;
            waker();
        });
    }

    /// Opens the chosen row's log.
    pub(crate) fn open_chosen_log(&mut self) -> Result<(), String> {
        let s = &self.log_sources;
        let host = Host::parse(&s.applied).ok_or_else(|| tr("Not a server address: user@server, server or user@server:2222.").to_string())?;
        let target = s.target(&s.list.lock().unwrap()).ok_or_else(|| tr("Choose a log first.").to_string())?;
        let (program, args) = ls::log_command(&target, s.amount(), s.follow, s.timestamps);
        let cmd = host.command(&program, &args);
        let starter = if matches!(host, Host::Ssh { .. }) { "ssh".to_string() } else { program };
        let title = target.title(&host);
        let (path, file) = super::remote::temp_copy(&target.file_name())?;
        if s.follow {
            return self.follow_process(cmd, title, path, file, &starter);
        }
        self.spawn_task(trf("Reading {name}…", &[("name", &title)]), move |progress: &Progress, cancel: &CancelToken| -> TaskOutcome {
            let mut cmd = cmd;
            let failed = |path: &std::path::Path, e: String| {
                bigtext::decompress::remove_temp(path);
                TaskOutcome::Failed(format!("{}: {e}", trf("Could not read {name}", &[("name", &title)])))
            };
            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => return failed(&path, super::remote::missing(&e, &starter)),
            };
            let (Some(out), Some(err)) = (child.stdout.take(), child.stderr.take()) else { return failed(&path, "no output".into()) };
            let child = Arc::new(Mutex::new(child));
            // Cancel kills the command.
            let done = Arc::new(AtomicBool::new(false));
            let (c2, d2, cancel2) = (child.clone(), done.clone(), cancel.clone());
            jobs::spawn("log-read-cancel", move || {
                while !d2.load(Ordering::Relaxed) {
                    if cancel2.is_cancelled() {
                        let _ = c2.lock().map(|mut c| c.kill());
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            });
            progress.set_total(0);
            let copied = ls::copy_merged(out, err, file, Some(progress.clone()));
            done.store(true, Ordering::Relaxed);
            let code = child.lock().ok().and_then(|mut c| c.wait().ok()).and_then(|st| st.code());
            if cancel.is_cancelled() {
                bigtext::decompress::remove_temp(&path);
                return TaskOutcome::Cancelled;
            }
            match copied {
                Ok(c) if code == Some(0) => TaskOutcome::Remote(Box::new(super::remote::RemoteDone { path, title: title.clone(), bytes: c.bytes })),
                Ok(c) => failed(&path, super::remote::ssh_error(&c.errors, code)),
                Err(e) => failed(&path, e.to_string()),
            }
        });
        Ok(())
    }

    pub(super) fn log_sources_window(&mut self, ctx: &egui::Context) {
        if !self.log_sources.open {
            return;
        }
        let mut open = true;
        let (mut reload, mut go) = (false, false);
        let list = self.log_sources.list.clone();
        let s = &mut self.log_sources;
        egui::Window::new(tr("Open container or service log")).open(&mut open).default_size([880.0, 560.0]).resizable(true).collapsible(false).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(tr("Server:"));
                let r = ui.add(egui::TextEdit::singleline(&mut s.host).hint_text(tr("empty: this computer; or user@server")).desired_width(260.0));
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    reload = true;
                }
                if ui.button(tr("Refresh")).clicked() {
                    reload = true;
                }
            });
            ui.weak(tr("Logs are read with docker, kubectl or journalctl there, with your keys and permissions; nothing is changed."));
            ui.add_space(4.0);
            let before = (s.tab, s.tool.clone(), s.context.clone());
            ui.horizontal(|ui| {
                ui.selectable_value(&mut s.tab, Tab::Docker, "Docker");
                ui.selectable_value(&mut s.tab, Tab::Kubernetes, "Kubernetes");
                ui.selectable_value(&mut s.tab, Tab::Journal, tr("systemd journal"));
            });
            ui.separator();
            let l = list.lock().unwrap();
            ui.horizontal(|ui| {
                match s.tab {
                    Tab::Docker => {
                        ui.radio_value(&mut s.tool, "docker".to_string(), "docker");
                        ui.radio_value(&mut s.tool, "podman".to_string(), "podman");
                    }
                    Tab::Kubernetes => {
                        ui.label(tr("Context:"));
                        let shown = s.context.clone().or_else(|| l.current_context.clone()).unwrap_or_default();
                        egui::ComboBox::from_id_salt("k8s-context").selected_text(shown).show_ui(ui, |ui| {
                            for c in &l.contexts {
                                let current = l.current_context.as_deref() == Some(c.as_str());
                                let label = if current { format!("{c} ✓") } else { c.clone() };
                                if ui.selectable_label(s.context.as_deref() == Some(c.as_str()) || (s.context.is_none() && current), label).clicked() {
                                    s.context = if current { None } else { Some(c.clone()) };
                                }
                            }
                        });
                        ui.label(tr("Namespace:"));
                        let mut spaces: Vec<&str> = l.pods.iter().map(|p| p.namespace.as_str()).collect();
                        spaces.sort_unstable();
                        spaces.dedup();
                        let all = tr("All namespaces");
                        egui::ComboBox::from_id_salt("k8s-namespace").selected_text(s.namespace.clone().unwrap_or_else(|| all.to_string())).show_ui(ui, |ui| {
                            ui.selectable_value(&mut s.namespace, None, all);
                            for n in spaces {
                                ui.selectable_value(&mut s.namespace, Some(n.to_string()), n);
                            }
                        });
                    }
                    Tab::Journal => {}
                }
                ui.label(tr("Filter:"));
                ui.add(egui::TextEdit::singleline(&mut s.filter).desired_width(160.0));
            });
            if (s.tab, s.tool.clone(), s.context.clone()) != before {
                s.selected = None;
                reload = true;
            }
            if l.loading {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(tr("Reading the list…"));
                });
            }
            if let Some(e) = &l.error {
                ui.colored_label(ui.visuals().error_fg_color, explain(e.clone()));
            }
            if l.one_namespace {
                ui.weak(tr("Listing all namespaces is not allowed, so the context's own namespace is listed."));
            }
            let font = egui::FontId::monospace(13.0);
            let needle = s.filter.trim().to_lowercase();
            let head = match s.tab {
                Tab::Docker => format!("{:<32} {:<10} {:<26} {}", tr("Name"), tr("State"), tr("Status"), tr("Image")),
                Tab::Kubernetes => format!("{:<16} {:<40} {:>5} {:<18} {:>8}", tr("Namespace"), tr("Pod"), tr("Ready"), tr("Status"), tr("Restarts")),
                Tab::Journal => format!("{:<40} {:<9} {:<9} {}", tr("Service"), tr("State"), "", tr("Description")),
            };
            ui.add(egui::Label::new(egui::RichText::new(head).font(font.clone()).weak()).extend());
            let rows = s.rows(&l);
            let shown: Vec<&Row> = rows.iter().filter(|r| needle.is_empty() || r.text.to_lowercase().contains(&needle)).collect();
            let (weak, bad) = (ui.visuals().weak_text_color(), ui.visuals().error_fg_color);
            egui::ScrollArea::both().id_salt("log-sources-rows").max_height(LIST_HEIGHT).auto_shrink([false, true]).show(ui, |ui| {
                if shown.is_empty() && !l.loading && l.error.is_none() {
                    ui.weak(tr("Nothing to list."));
                }
                for r in shown {
                    let rich = egui::RichText::new(&r.text).font(font.clone());
                    let rich = match r.look {
                        Look::Normal => rich,
                        Look::Weak => rich.color(weak),
                        Look::Bad => rich.color(bad),
                    };
                    let resp = ui.add(egui::Button::selectable(s.selected.as_deref() == Some(r.key.as_str()), rich).wrap_mode(egui::TextWrapMode::Extend));
                    if resp.clicked() {
                        s.selected = Some(r.key.clone());
                    }
                    if resp.double_clicked() {
                        s.selected = Some(r.key.clone());
                        go = true;
                    }
                }
            });
            ui.separator();
            // The chosen pod's container, and its previous run.
            if s.tab == Tab::Kubernetes {
                if let Some(p) = s.selected.as_ref().and_then(|k| l.pods.iter().find(|p| format!("{}/{}", p.namespace, p.name) == *k)) {
                    let key = format!("{}/{}", p.namespace, p.name);
                    ui.horizontal(|ui| {
                        if p.containers.len() + p.init_containers.len() > 1 {
                            ui.label(tr("Container:"));
                            let mut chosen = s.pod_container(p);
                            let all = tr("All containers");
                            egui::ComboBox::from_id_salt("k8s-container").selected_text(chosen.clone().unwrap_or_else(|| all.to_string())).show_ui(ui, |ui| {
                                ui.selectable_value(&mut chosen, None, all);
                                for c in &p.containers {
                                    ui.selectable_value(&mut chosen, Some(c.clone()), c);
                                }
                                for c in &p.init_containers {
                                    ui.selectable_value(&mut chosen, Some(c.clone()), format!("{c} (init)"));
                                }
                            });
                            if chosen != s.pod_container(p) {
                                s.container = chosen;
                                s.container_of = Some(key.clone());
                            }
                        }
                        ui.checkbox(&mut s.previous, tr("Its run before the last restart"));
                    });
                }
            }
            ui.horizontal(|ui| {
                ui.radio_value(&mut s.how, HowMuch::Lines, tr("The last lines:"));
                ui.add_enabled(s.how == HowMuch::Lines, egui::DragValue::new(&mut s.lines).range(1..=100_000_000).speed(100.0));
                ui.radio_value(&mut s.how, HowMuch::Minutes, tr("The last minutes:"));
                ui.add_enabled(s.how == HowMuch::Minutes, egui::DragValue::new(&mut s.minutes).range(1..=1_000_000));
                ui.radio_value(&mut s.how, HowMuch::All, tr("Everything"));
            });
            ui.horizontal(|ui| {
                ui.checkbox(&mut s.follow, tr("Follow new lines"));
                if s.tab != Tab::Journal {
                    ui.checkbox(&mut s.timestamps, tr("A timestamp before each line"));
                }
            });
            if let Some(t) = s.target(&l) {
                let (program, args) = ls::log_command(&t, s.amount(), s.follow, s.timestamps);
                let line = std::iter::once(program).chain(args).collect::<Vec<_>>().join(" ");
                let at = Host::parse(&s.applied).map(|h| h.label().to_string()).unwrap_or_default();
                ui.label(egui::RichText::new(if at.is_empty() { line } else { format!("{at}: {line}") }).font(font.clone()).weak());
            }
            if let Some(e) = &s.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            ui.horizontal(|ui| {
                if ui.add_enabled(s.target(&l).is_some(), egui::Button::new(tr("Open"))).clicked() {
                    go = true;
                }
                if ui.button(tr("Close")).clicked() {
                    s.open = false;
                }
            });
        });
        if !open {
            self.log_sources.open = false;
        }
        if reload {
            self.load_log_sources();
        }
        if go {
            match self.open_chosen_log() {
                Ok(()) => {
                    self.log_sources.open = false;
                    self.persist_state();
                }
                Err(e) => self.log_sources.error = Some(e),
            }
        }
    }

    /// Self-test: shows `listing` as if it had been read.
    pub(crate) fn test_log_listing(&mut self, tab: Tab, listing: Listing) {
        let s = &mut self.log_sources;
        s.open = true;
        s.tab = tab;
        s.key = Some(ListKey { host: Host::Local, tab, tool: s.tool.clone(), context: s.context.clone() });
        s.list = Arc::new(Mutex::new(listing));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choices_are_kept_as_a_line() {
        let mut s = SourcesUi { applied: "deploy@web1".into(), tab: Tab::Kubernetes, tool: "podman".into(), how: HowMuch::Minutes, lines: 500, minutes: 15, follow: false, timestamps: true, ..Default::default() };
        let line = s.to_line();
        assert_eq!(line, "deploy@web1|kubernetes|podman|minutes|500|15|0|1");
        s = SourcesUi::default();
        s.set_line(&line);
        assert_eq!((s.host.as_str(), s.tab, s.tool.as_str(), s.how, s.lines, s.minutes, s.follow, s.timestamps), ("deploy@web1", Tab::Kubernetes, "podman", HowMuch::Minutes, 500, 15, false, true));
        s.set_line("garbage");
        assert_eq!(s.tab, Tab::Kubernetes);
    }

    #[test]
    fn the_chosen_row_is_a_target() {
        let pod = |name: &str, containers: &[&str], init: &[&str]| Pod { namespace: "prod".into(), name: name.into(), containers: containers.iter().map(|c| c.to_string()).collect(), init_containers: init.iter().map(|c| c.to_string()).collect(), ..Default::default() };
        let l = Listing { pods: vec![pod("one", &["app"], &[]), pod("two", &["app", "proxy"], &["migrate"])], containers: vec![Container { id: "4a1".into(), name: "api".into(), ..Default::default() }], ..Default::default() };
        let mut s = SourcesUi { tab: Tab::Kubernetes, selected: Some("prod/one".into()), ..Default::default() };
        assert_eq!(s.target(&l), Some(Target::Pod { context: None, namespace: "prod".into(), pod: "one".into(), container: None, previous: false }));
        s.selected = Some("prod/two".into());
        assert!(matches!(s.target(&l), Some(Target::Pod { container: Some(c), .. }) if c == "app"));
        (s.container, s.container_of) = (None, Some("prod/two".into()));
        assert!(matches!(s.target(&l), Some(Target::Pod { container: None, .. })));
        s.tab = Tab::Docker;
        s.selected = Some("4a1".into());
        assert_eq!(s.target(&l), Some(Target::Container { tool: "docker".into(), id: "4a1".into(), name: "api".into() }));
        s.tab = Tab::Journal;
        s.selected = Some(KERNEL.into());
        assert_eq!(s.target(&l), Some(Target::Journal { unit: None, kernel: true }));
        s.selected = Some("nginx.service".into());
        assert_eq!(s.target(&l), Some(Target::Journal { unit: Some("nginx.service".into()), kernel: false }));
    }
}
