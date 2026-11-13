//! File → Open remote file or URL…: a file on a server over SSH (`user@host:/var/log/app.log`,
//! `ssh://user@host:2222/path`), read with the system's `ssh` and the user's keys or agent; or a
//! web address (`https://…`, `s3://bucket/key` for public objects). A whole file downloads as a
//! task with progress and Cancel in the status bar, then opens read-only; "the last N MB, then
//! follow" opens at once and keeps adding what `tail -F` sends, like a local log in follow
//! mode. The copies are temporary: they go away with their tabs, and closing a followed tab
//! ends its `ssh`.

use std::fs::File;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bigtext::jobs::{human_bytes, CancelToken, Progress};
use bigtext::remote::{quote, ssh_base, SshFile};

use super::i18n::{tr, trf};
use super::tasks::TaskOutcome;
use super::BigTextApp;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// `target` is `user@host` or `host`; a `path` without a leading `/` is in the home folder.
    Ssh { target: String, port: Option<u16>, path: String },
    Http { url: String },
}

impl Source {
    /// The name shown on the tab: `host:/path/name` → `name (host)`.
    pub fn title(&self) -> String {
        match self {
            Source::Ssh { target, path, .. } => {
                let host = target.rsplit('@').next().unwrap_or(target);
                format!("{} ({host})", path.rsplit('/').next().filter(|n| !n.is_empty()).unwrap_or(path))
            }
            Source::Http { url } => url.split(['?', '#']).next().unwrap_or(url).trim_end_matches('/').rsplit('/').next().unwrap_or(url).to_string(),
        }
    }

    /// A file name for the copy, keeping the extension (it picks the highlighting).
    fn file_name(&self) -> String {
        let raw = match self {
            Source::Ssh { path, .. } => path.rsplit('/').next().unwrap_or("remote"),
            Source::Http { url } => url.split(['?', '#']).next().unwrap_or(url).trim_end_matches('/').rsplit('/').next().unwrap_or("remote"),
        };
        let clean: String = raw.chars().map(|c| if c.is_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' }).collect();
        if clean.is_empty() || clean.contains("://") {
            "remote.txt".into()
        } else {
            clean
        }
    }
}

/// Reads an address: `ssh://[user@]host[:port]/path`, `user@host:path` (as scp writes it),
/// `http(s)://…` and `s3://bucket/key`.
pub fn parse(addr: &str) -> Option<Source> {
    let a = addr.trim();
    if a.starts_with("http://") || a.starts_with("https://") {
        return Some(Source::Http { url: a.to_string() });
    }
    if let Some(rest) = a.strip_prefix("s3://") {
        let (bucket, key) = rest.split_once('/')?;
        if bucket.is_empty() || key.is_empty() {
            return None;
        }
        return Some(Source::Http { url: format!("https://{bucket}.s3.amazonaws.com/{key}") });
    }
    if let Some(rest) = a.strip_prefix("ssh://").or_else(|| a.strip_prefix("sftp://")) {
        let (host_part, path) = rest.split_once('/')?;
        let (target, port) = match host_part.rsplit_once(':') {
            Some((h, p)) if p.bytes().all(|b| b.is_ascii_digit()) && !p.is_empty() => (h, p.parse().ok()),
            _ => (host_part, None),
        };
        if target.is_empty() || path.is_empty() {
            return None;
        }
        // ssh://host/~/x is in the home folder; ssh://host/var/x is /var/x.
        let path = if path.starts_with('~') { path.to_string() } else { format!("/{path}") };
        return Some(Source::Ssh { target: target.into(), port, path });
    }
    // user@host:path — the @ keeps a Windows path (C:\x) from looking like one.
    let (target, path) = a.split_once(':')?;
    if !target.contains('@') || target.contains(['/', '\\', ' ']) || path.is_empty() || target.starts_with('@') || target.ends_with('@') {
        return None;
    }
    Some(Source::Ssh { target: target.into(), port: None, path: path.into() })
}

/// The `ssh` command that sends the file: all of it, or its last `tail` bytes, then (when
/// `follow`) whatever is added to it.
pub fn ssh_command(target: &str, port: Option<u16>, path: &str, tail: Option<u64>, follow: bool) -> Command {
    let mut c = ssh_base(target, port);
    let q = quote(path);
    let remote = match (tail, follow) {
        (None, false) => format!("cat -- {q}"),
        (None, true) => format!("tail -c +1 -F -- {q}"),
        (Some(n), true) => format!("tail -c {n} -F -- {q}"),
        (Some(n), false) => format!("tail -c {n} -- {q}"),
    };
    c.arg(remote);
    c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    c
}

/// What went wrong with `ssh`, with the hint about signing in in the interface's language.
fn ssh_error(stderr: &str, code: Option<i32>) -> String {
    const HINT: &str = "BigText signs in with your SSH keys or agent (and known hosts); password sign-in is not supported.";
    bigtext::remote::ssh_error(stderr, code).replace(HINT, tr(HINT))
}

fn spawn_ssh(mut c: Command) -> Result<Child, String> {
    c.spawn().map_err(|e| if e.kind() == std::io::ErrorKind::NotFound { tr("ssh was not found. Install the OpenSSH client (on Windows: Settings → System → Optional features).").to_string() } else { e.to_string() })
}

/// A new temporary file for a copy of `src`.
fn temp_file(src: &Source) -> Result<(PathBuf, File), String> {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = bigtext::decompress::temp_dir().join(format!("{}-remote-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(src.file_name());
    let file = File::create(&path).map_err(|e| e.to_string())?;
    Ok((path, file))
}

/// Copies `from` into `to` until the end, `cancel` or an error; returns the bytes copied.
fn pump(mut from: impl Read, to: &mut File, progress: Option<&Progress>, cancel: Option<&CancelToken>) -> std::io::Result<u64> {
    let mut buf = vec![0u8; 256 * 1024];
    let mut n = 0u64;
    loop {
        if cancel.is_some_and(CancelToken::is_cancelled) {
            return Ok(n);
        }
        let got = from.read(&mut buf)?;
        if got == 0 {
            return Ok(n);
        }
        to.write_all(&buf[..got])?;
        to.flush()?;
        n += got as u64;
        if let Some(p) = progress {
            p.set_done(n);
        }
    }
}

/// Downloads all of `src` into `file` (a task: progress and cancel).
fn download(src: &Source, file: &mut File, progress: &Progress, cancel: &CancelToken) -> Result<u64, String> {
    match src {
        Source::Http { url } => {
            let agent = ureq::Agent::config_builder()
                .user_agent(format!("BigText/{}", env!("CARGO_PKG_VERSION")))
                .timeout_connect(Some(Duration::from_secs(15)))
                .timeout_per_read(Some(Duration::from_secs(60)))
                .build()
                .new_agent();
            let resp = agent.get(url).call().map_err(|e| match e {
                ureq::Error::StatusCode(code) => trf("The server answered with HTTP status {code}.", &[("code", &code.to_string())]),
                e => e.to_string(),
            })?;
            if let Some(len) = resp.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok()) {
                progress.set_total(len);
            }
            pump(resp.into_body().into_reader(), file, Some(progress), Some(cancel)).map_err(|e| e.to_string())
        }
        Source::Ssh { target, port, path } => {
            let mut child = spawn_ssh(ssh_command(target, *port, path, None, false))?;
            let stdout = child.stdout.take().ok_or("no output")?;
            let mut stderr = child.stderr.take().ok_or("no output")?;
            let err = std::thread::spawn(move || {
                let mut s = String::new();
                let _ = stderr.read_to_string(&mut s);
                s
            });
            let n = pump(stdout, file, Some(progress), Some(cancel));
            if cancel.is_cancelled() {
                let _ = child.kill();
            }
            let status = child.wait().map_err(|e| e.to_string())?;
            let stderr = err.join().unwrap_or_default();
            let n = n.map_err(|e| e.to_string())?;
            if status.success() || cancel.is_cancelled() {
                Ok(n)
            } else {
                Err(ssh_error(&stderr, status.code()))
            }
        }
    }
}

/// A followed remote file: its tab, its `ssh` and how it ended.
struct Follow {
    doc: u64,
    title: String,
    child: Arc<Mutex<Option<Child>>>,
    done: Receiver<Result<(), String>>,
}

impl Drop for Follow {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.lock().ok().and_then(|mut c| c.take()) {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// How a remote file opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum How {
    /// Read in parts as they are needed: what is shown, what a search reaches (a web server
    /// that sends only whole files gets a download instead).
    InPlace,
    /// Downloaded whole into a temporary copy.
    Whole,
    /// Its last MB, then what is added (SSH).
    Tail(u32),
}

/// The choice in the dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    InPlace,
    Whole,
    Tail,
}

/// The Open remote dialog and the followed files.
pub struct RemoteUi {
    pub open: bool,
    pub addr: String,
    pub choice: Choice,
    /// Megabytes of the end that [`Choice::Tail`] reads.
    pub tail_mb: u32,
    pub error: Option<String>,
    focus: bool,
    follows: Vec<Follow>,
}

impl Default for RemoteUi {
    fn default() -> Self {
        RemoteUi { open: false, addr: String::new(), choice: Choice::InPlace, tail_mb: 1, error: None, focus: false, follows: Vec::new() }
    }
}

/// A downloaded copy to open.
pub struct RemoteDone {
    pub path: PathBuf,
    pub title: String,
    pub bytes: u64,
}

/// A remote file ready to be read in place.
pub struct RemoteOpened {
    pub source: Arc<bigtext::file::Source>,
    pub detected: bigtext::encoding::Detected,
    /// Line counts of the whole 64 KB blocks of its first megabyte (an index prefix: line
    /// numbers further on are estimated from its average line length).
    pub prefix: Vec<u64>,
    pub ssh: Option<SshFile>,
}

/// Why a remote file could not be read in place.
#[derive(Debug)]
pub enum RemoteFail {
    /// The web server sends only whole files.
    NoRanges,
    Error(String),
}

/// Connects to `src`, learns its size and reads its first megabyte (for the encoding and the
/// length of its lines); nothing else is read until it is needed.
fn open_parts(src: &Source) -> Result<Box<RemoteOpened>, RemoteFail> {
    use bigtext::remote::{HttpOpen, HttpRange, SshRange};
    let (reader, len, ssh): (Box<dyn bigtext::file::RangeRead>, u64, Option<SshFile>) = match src {
        Source::Http { url } => match HttpRange::open(url).map_err(RemoteFail::Error)? {
            HttpOpen::Ranges(r, len) => (Box::new(r), len, None),
            HttpOpen::NoRanges => return Err(RemoteFail::NoRanges),
        },
        Source::Ssh { target, port, path } => {
            let file = SshFile { target: target.clone(), port: *port, path: path.clone() };
            let (r, len) = SshRange::open(&file).map_err(|e| RemoteFail::Error(e.replace("ssh was not found. Install the OpenSSH client (on Windows: Settings → System → Optional features).", tr("ssh was not found. Install the OpenSSH client (on Windows: Settings → System → Optional features)."))))?;
            (Box::new(r), len, Some(file))
        }
    };
    let source = Arc::new(bigtext::file::Source::remote(reader, len, None));
    let n = len.min(1 << 20);
    let sample = source.read(0..n).map_err(|e| RemoteFail::Error(e.to_string()))?;
    let detected = bigtext::encoding::detect(&sample, n == len);
    let block = bigtext::index::BLOCK;
    let blocks = if n == len { len.div_ceil(block) } else { n / block };
    let mut prefix = vec![0u64];
    for k in 0..blocks {
        let (s, e) = (k * block, ((k + 1) * block).min(len));
        let before = *prefix.last().unwrap_or(&0);
        prefix.push(before + detected.codec.count_nl(&sample[s as usize..e as usize], s));
    }
    Ok(Box::new(RemoteOpened { source, detected, prefix, ssh }))
}

impl BigTextApp {
    /// File → Open remote file or URL…
    pub(crate) fn open_remote_dialog(&mut self) {
        self.remote.open = true;
        self.remote.focus = true;
        self.remote.error = None;
    }

    /// Opens `addr` as `how` says.
    pub(crate) fn open_remote(&mut self, addr: &str, how: How) -> Result<(), String> {
        let src = parse(addr).ok_or_else(|| tr("Not an address BigText can open.").to_string())?;
        let title = src.title();
        if how == How::InPlace {
            self.open_in_place(src);
            return Ok(());
        }
        let (path, mut file) = temp_file(&src)?;
        match (&src, how) {
            (Source::Ssh { target, port, path: rpath }, How::Tail(mb)) => {
                let cmd = ssh_command(target, *port, rpath, Some(mb.max(1) as u64 * 1024 * 1024), true);
                self.follow_with(cmd, title, path, file)?;
            }
            _ => {
                let label = trf("Reading {name}…", &[("name", &title)]);
                self.spawn_task(label, move |progress, cancel| match download(&src, &mut file, progress, cancel) {
                    _ if cancel.is_cancelled() => {
                        drop(file);
                        bigtext::decompress::remove_temp(&path);
                        TaskOutcome::Cancelled
                    }
                    Ok(bytes) => TaskOutcome::Remote(Box::new(RemoteDone { path, title, bytes })),
                    Err(e) => {
                        drop(file);
                        bigtext::decompress::remove_temp(&path);
                        TaskOutcome::Failed(format!("{}: {e}", trf("Could not read {name}", &[("name", &title)])))
                    }
                });
            }
        }
        Ok(())
    }

    /// A tab for `src` read in place; it fills when the connection is made and the first
    /// megabyte is read.
    fn open_in_place(&mut self, src: Source) {
        if let Some(i) = self.docs.iter().position(|d| d.path.is_none() && d.temporary && d.title == src.title() && d.buf.source.is_remote()) {
            self.active = i;
            return;
        }
        // Replace a pristine untitled tab.
        if self.docs.len() == 1 && self.docs[0].path.is_none() && !self.docs[0].is_modified() && self.docs[0].buf.is_empty() && !self.docs[0].temporary {
            self.docs.clear();
        }
        let id = self.alloc_id();
        let mut d = super::document::Document::loading(id, PathBuf::new());
        d.path = None;
        d.title = src.title();
        d.temporary = true;
        d.read_only = true;
        d.file_read_only = true;
        self.docs.push(d);
        self.active = self.docs.len() - 1;
        let (tx, waker) = (self.tx.clone(), self.waker.clone());
        bigtext::jobs::spawn("remote-open", move || {
            let result = open_parts(&src);
            let _ = tx.send(super::Msg::RemoteOpened { doc: id, src, result });
            waker();
        });
    }

    /// The remote file of tab `doc` is open to be read in place, or falls back to a download,
    /// or failed.
    pub(super) fn remote_opened(&mut self, doc: u64, src: Source, result: Result<Box<RemoteOpened>, RemoteFail>) {
        let waker = self.waker.clone();
        match result {
            Ok(o) => {
                let Some(d) = self.doc_mut(doc) else { return };
                let ssh = o.ssh.is_some();
                d.install(o.source, o.detected, Some(o.prefix), waker);
                d.remote_ssh = o.ssh;
                d.banner = Some(super::document::Banner::Info(
                    if ssh {
                        tr("Read in place over SSH: only what is shown is transferred, and searches run on the server. Line numbers past the start are estimates (~). Read-only.")
                    } else {
                        tr("Read in place from the web: only what is shown is downloaded, and a search reads the file as it goes. Line numbers past the start are estimates (~). Read-only.")
                    }
                    .into(),
                ));
            }
            Err(fail) => {
                if self.doc_mut(doc).is_some() {
                    self.close_tab(doc, true);
                } else {
                    // The tab was closed while connecting.
                    return;
                }
                let name = src.title();
                match fail {
                    RemoteFail::NoRanges => {
                        self.notify(trf("{name}: this server sends only whole files, so it is downloaded.", &[("name", &name)]), None);
                        let addr = match &src {
                            Source::Http { url } => url.clone(),
                            Source::Ssh { .. } => return,
                        };
                        if let Err(e) = self.open_remote(&addr, How::Whole) {
                            self.error = Some(e);
                        }
                    }
                    RemoteFail::Error(e) => self.error = Some(format!("{}: {e}", trf("Could not open {name}", &[("name", &name)]))),
                }
            }
        }
    }

    /// Runs `cmd` (an `ssh … tail -F`), opens `path` followed and read-only, and keeps adding
    /// what `cmd` writes to `file` until the tab closes (which ends `cmd`).
    pub(crate) fn follow_with(&mut self, cmd: Command, title: String, path: PathBuf, mut file: File) -> Result<(), String> {
        let mut child = spawn_ssh(cmd)?;
        let stdout = child.stdout.take().ok_or("no output")?;
        let mut stderr = child.stderr.take().ok_or("no output")?;
        let child = Arc::new(Mutex::new(Some(child)));
        let (tx, rx) = channel();
        let (waker, held) = (self.waker.clone(), child.clone());
        bigtext::jobs::spawn("remote-follow", move || {
            let copied = pump(stdout, &mut file, None, None);
            let mut s = String::new();
            let _ = stderr.read_to_string(&mut s);
            // The output ended, so ssh is ending: its exit code, without holding the lock
            // a closing tab needs to kill it (gone: it was killed).
            let mut code = None;
            for _ in 0..100 {
                let state = held.lock().ok().map(|mut c| c.as_mut().map(|c| c.try_wait()));
                match state {
                    Some(Some(Ok(Some(st)))) => {
                        code = st.code();
                        break;
                    }
                    Some(Some(Ok(None))) => std::thread::sleep(Duration::from_millis(20)),
                    _ => break,
                }
            }
            let _ = tx.send(match copied {
                Err(e) => Err(e.to_string()),
                Ok(_) if code == Some(0) || code.is_none() => Ok(()),
                Ok(_) => Err(ssh_error(&s, code)),
            });
            waker();
        });
        self.open_path(path);
        let Some(d) = self.docs.get_mut(self.active) else { return Ok(()) };
        d.title = title.clone();
        d.temporary = true;
        d.read_only = true;
        d.follow = true;
        let doc = d.id;
        self.remote.follows.push(Follow { doc, title, child, done: rx });
        Ok(())
    }

    /// Self-test: follows what `cmd` prints, as a remote file named `name`.
    pub(crate) fn test_follow(&mut self, cmd: Command, name: &str) -> Result<(), String> {
        let src = Source::Http { url: format!("http://test/{name}") };
        let (path, file) = temp_file(&src)?;
        self.follow_with(cmd, name.to_string(), path, file)
    }

    /// A finished download opens read-only (the copy goes away with the tab).
    pub(super) fn open_remote_done(&mut self, r: RemoteDone) {
        self.open_path(r.path);
        if let Some(d) = self.docs.get_mut(self.active) {
            d.title = r.title.clone();
            d.temporary = true;
            d.read_only = true;
        }
        self.notify(trf("Read {name} ({size}).", &[("name", &r.title), ("size", &human_bytes(r.bytes))]), None);
    }

    /// Ends the `ssh` of closed tabs; reports a follow that stopped.
    pub(super) fn remote_tick(&mut self) {
        let open: Vec<u64> = self.docs.iter().map(|d| d.id).collect();
        self.remote.follows.retain(|f| open.contains(&f.doc));
        let mut ended = Vec::new();
        for (i, f) in self.remote.follows.iter().enumerate() {
            if let Ok(r) = f.done.try_recv() {
                ended.push((i, f.title.clone(), r));
            }
        }
        for (i, title, r) in ended.into_iter().rev() {
            self.remote.follows.remove(i);
            match r {
                Ok(()) => self.notify(trf("Following {name} stopped: the connection ended.", &[("name", &title)]), None),
                Err(e) => self.error = Some(format!("{}: {e}", trf("Could not follow {name}", &[("name", &title)]))),
            }
        }
    }

    pub(super) fn remote_window(&mut self, ctx: &egui::Context) {
        if !self.remote.open {
            return;
        }
        let r = &mut self.remote;
        let ssh = matches!(parse(&r.addr), Some(Source::Ssh { .. }));
        let (mut go, mut cancel) = (false, false);
        egui::Modal::new(egui::Id::new("remote")).show(ctx, |ui| {
            ui.set_width(520.0);
            ui.heading(tr("Open remote file or URL"));
            let t = ui.add(egui::TextEdit::singleline(&mut r.addr).hint_text("user@server:/var/log/app.log").desired_width(f32::INFINITY));
            if r.focus {
                t.request_focus();
                r.focus = false;
            }
            if t.changed() {
                r.error = None;
            }
            ui.weak(tr("user@server:/var/log/app.log, ssh://user@server:2222/path, https://example.com/big.log or s3://bucket/key (public)"));
            ui.add_space(4.0);
            ui.radio_value(&mut r.choice, Choice::InPlace, if ssh { tr("Read only what is shown; searches run on the server") } else { tr("Read only what is shown, without downloading it") });
            ui.radio_value(&mut r.choice, Choice::Whole, tr("The whole file"));
            ui.add_enabled_ui(ssh, |ui| {
                ui.horizontal(|ui| {
                    ui.radio_value(&mut r.choice, Choice::Tail, tr("Its last"));
                    ui.add(egui::DragValue::new(&mut r.tail_mb).range(1..=4096).suffix(" MB"));
                    ui.label(tr("then follow what is added (like tail -F)"));
                });
            });
            if !ssh && r.choice == Choice::Tail {
                r.choice = Choice::InPlace;
            }
            if ssh {
                ui.weak(tr("SSH uses your keys or agent, as the ssh command does."));
            }
            if let Some(e) = &r.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                go = ui.add_enabled(!r.addr.trim().is_empty(), egui::Button::new(tr("Open"))).clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter));
                cancel = ui.button(tr("Cancel")).clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape));
            });
        });
        if cancel {
            self.remote.open = false;
        } else if go && !self.remote.addr.trim().is_empty() {
            let addr = self.remote.addr.clone();
            let how = match self.remote.choice {
                Choice::InPlace => How::InPlace,
                Choice::Whole => How::Whole,
                Choice::Tail if ssh => How::Tail(self.remote.tail_mb),
                Choice::Tail => How::InPlace,
            };
            match self.open_remote(&addr, how) {
                Ok(()) => self.remote.open = false,
                Err(e) => self.remote.error = Some(e),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses() {
        assert_eq!(parse("ann@web1:/var/log/app.log"), Some(Source::Ssh { target: "ann@web1".into(), port: None, path: "/var/log/app.log".into() }));
        assert_eq!(parse("ssh://ann@web1:2222/var/log/x.log"), Some(Source::Ssh { target: "ann@web1".into(), port: Some(2222), path: "/var/log/x.log".into() }));
        assert_eq!(parse("ssh://web1/~/notes.md"), Some(Source::Ssh { target: "web1".into(), port: None, path: "~/notes.md".into() }));
        assert_eq!(parse("s3://logs/2024/a.gz"), Some(Source::Http { url: "https://logs.s3.amazonaws.com/2024/a.gz".into() }));
        assert_eq!(parse("https://x.org/a/big.log?sig=1"), Some(Source::Http { url: "https://x.org/a/big.log?sig=1".into() }));
        for not in [r"C:\logs\a.log", "a.log", "web1:/x", "ssh://web1", "@h:/x", "s3://bucket"] {
            assert_eq!(parse(not), None, "{not}");
        }
        assert_eq!(parse("ann@web1:/var/log/app.log").unwrap().title(), "app.log (web1)");
        assert_eq!(parse("https://x.org/a/big.log?sig=1").unwrap().file_name(), "big.log");
        assert_eq!(quote("/a/it's here"), r"'/a/it'\''s here'");
        assert_eq!(quote("~/x y"), "~/'x y'");
    }

    #[test]
    fn ssh_commands() {
        let c = ssh_command("ann@web1", Some(2222), "/var/log/app.log", Some(1024), true);
        let args: Vec<String> = c.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(c.get_program(), "ssh");
        assert!(args.windows(2).any(|w| w == ["-p", "2222"]) && args.contains(&"BatchMode=yes".to_string()), "{args:?}");
        assert_eq!(args[args.len() - 3..], ["ann@web1".to_string(), "--".into(), "tail -c 1024 -F -- '/var/log/app.log'".into()]);
        let whole = ssh_command("h", None, "a b", None, false);
        assert_eq!(whole.get_args().last().unwrap(), "cat -- 'a b'");
    }

    #[test]
    fn ssh_failure_is_explained() {
        // A closed port: ssh fails at once with its own message (skipped where ssh is missing).
        let src = parse("ssh://nobody@127.0.0.1:1/tmp/x.log").unwrap();
        let (path, mut file) = temp_file(&src).unwrap();
        let r = download(&src, &mut file, &Progress::default(), &CancelToken::new());
        drop(file);
        bigtext::decompress::remove_temp(&path);
        match r {
            Err(e) if e.contains("ssh was not found") => {}
            Err(e) => assert!(e.to_lowercase().contains("refused") || e.contains("port 1") || e.contains("connect"), "{e}"),
            Ok(n) => panic!("read {n} bytes from a closed port"),
        }
    }

    #[test]
    fn web_copy_streams_into_a_file() {
        // A local server answering one request.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = s.read(&mut buf);
                let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\nConnection: close\r\n\r\nline1\nline2");
            }
        });
        let src = parse(&format!("http://127.0.0.1:{port}/x/app.log")).unwrap();
        let (path, mut file) = temp_file(&src).unwrap();
        let n = download(&src, &mut file, &Progress::default(), &CancelToken::new()).unwrap();
        drop(file);
        assert_eq!((n, std::fs::read_to_string(&path).unwrap()), (11, "line1\nline2".to_string()));
        assert!(path.ends_with("app.log"));
        bigtext::decompress::remove_temp(&path);
    }
}
