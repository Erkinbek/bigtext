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

/// `s` quoted for the remote POSIX shell; a leading `~/` stays outside the quotes to expand.
fn quote(s: &str) -> String {
    let (home, rest) = match s.strip_prefix("~/") {
        Some(r) => ("~/", r),
        None => ("", s),
    };
    format!("{home}'{}'", rest.replace('\'', r"'\''"))
}

/// The `ssh` command that sends the file: all of it, or its last `tail` bytes, then (when
/// `follow`) whatever is added to it.
pub fn ssh_command(target: &str, port: Option<u16>, path: &str, tail: Option<u64>, follow: bool) -> Command {
    let mut c = Command::new("ssh");
    // No password prompt (there is no terminal to type it in): keys and agents only.
    c.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=15", "-o", "ServerAliveInterval=30"]);
    if let Some(p) = port {
        c.arg("-p").arg(p.to_string());
    }
    let q = quote(path);
    let remote = match (tail, follow) {
        (None, false) => format!("cat -- {q}"),
        (None, true) => format!("tail -c +1 -F -- {q}"),
        (Some(n), true) => format!("tail -c {n} -F -- {q}"),
        (Some(n), false) => format!("tail -c {n} -- {q}"),
    };
    c.arg(target).arg("--").arg(remote);
    c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // A console program started from a window would open a console of its own.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

/// What went wrong with `ssh`, in words a user can act on.
fn ssh_error(stderr: &str, code: Option<i32>) -> String {
    let s = stderr.trim();
    let mut msg = if s.is_empty() { format!("ssh exited with code {}", code.unwrap_or(-1)) } else { s.lines().last().unwrap_or(s).to_string() };
    if s.contains("Permission denied") || s.contains("Host key verification failed") {
        msg = format!("{msg}\n{}", tr("BigText signs in with your SSH keys or agent (and known hosts); password sign-in is not supported."));
    }
    msg
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

/// The Open remote dialog and the followed files.
pub struct RemoteUi {
    pub open: bool,
    pub addr: String,
    /// Read the last [`Self::tail_mb`] MB and follow (SSH only).
    pub tail: bool,
    pub tail_mb: u32,
    pub error: Option<String>,
    focus: bool,
    follows: Vec<Follow>,
}

impl Default for RemoteUi {
    fn default() -> Self {
        RemoteUi { open: false, addr: String::new(), tail: false, tail_mb: 1, error: None, focus: false, follows: Vec::new() }
    }
}

/// A downloaded copy to open.
pub struct RemoteDone {
    pub path: PathBuf,
    pub title: String,
    pub bytes: u64,
}

impl BigTextApp {
    /// File → Open remote file or URL…
    pub(crate) fn open_remote_dialog(&mut self) {
        self.remote.open = true;
        self.remote.focus = true;
        self.remote.error = None;
    }

    /// Opens `addr`: downloads it whole, or (SSH, `tail_mb`) follows its last megabytes.
    pub(crate) fn open_remote(&mut self, addr: &str, tail_mb: Option<u32>) -> Result<(), String> {
        let src = parse(addr).ok_or_else(|| tr("Not an address BigText can open.").to_string())?;
        let title = src.title();
        let (path, mut file) = temp_file(&src)?;
        match (&src, tail_mb) {
            (Source::Ssh { target, port, path: rpath }, Some(mb)) => {
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
            ui.add_enabled_ui(ssh, |ui| {
                ui.radio_value(&mut r.tail, false, tr("The whole file"));
                ui.horizontal(|ui| {
                    ui.radio_value(&mut r.tail, true, tr("Its last"));
                    ui.add(egui::DragValue::new(&mut r.tail_mb).range(1..=4096).suffix(" MB"));
                    ui.label(tr("then follow what is added (like tail -F)"));
                });
            });
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
            let tail = (ssh && self.remote.tail).then_some(self.remote.tail_mb);
            match self.open_remote(&addr, tail) {
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
