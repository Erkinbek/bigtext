//! One window for every launch: a second `bigtext app.log:120` hands its files to the BigText
//! already running (which opens them in new tabs and comes to the front) instead of starting
//! another process. The running one listens on a loopback port written, with a random token,
//! to a lock file next to the settings, which only this user can read; `--new-window` skips it.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::Duration;

use bigtext::search::Query;

use crate::app::StartOptions;

const MAGIC: &str = "bigtext-open 1";

/// The lock file (`BIGTEXT_INSTANCE_DIR` for tests).
pub fn lock_path() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("BIGTEXT_INSTANCE_DIR") {
        return Some(PathBuf::from(d).join("instance.lock"));
    }
    Some(crate::app::extras::state_file()?.with_file_name("instance.lock"))
}

/// The request's lines: files with their lines, the search, follow.
pub fn encode(start: &StartOptions, token: &str) -> String {
    let mut s = format!("{MAGIC} {token}\n");
    for (p, line) in &start.files {
        let abs = std::path::absolute(p).unwrap_or_else(|_| p.clone());
        s.push_str(&format!("file\t{}\t{}\n", abs.display(), line.map(|l| l.to_string()).unwrap_or_default()));
    }
    if let Some(q) = &start.find {
        let flags = format!("{}{}{}", if q.regex { "r" } else { "" }, if q.case_sensitive { "c" } else { "" }, if q.whole_word { "w" } else { "" });
        s.push_str(&format!("find\t{flags}\t{}\n", q.pattern.replace(['\n', '\r'], " ")));
    }
    if start.follow {
        s.push_str("follow\n");
    }
    s.push_str(".\n");
    s
}

/// A request read back, when it carries `token`.
pub fn decode(lines: &[String], token: &str) -> Option<StartOptions> {
    let (head, rest) = lines.split_first()?;
    if head.strip_prefix(MAGIC)?.trim() != token {
        return None;
    }
    let mut start = StartOptions { files: Vec::new(), find: None, follow: false, stdin: None };
    for l in rest {
        let mut it = l.splitn(3, '\t');
        match it.next()? {
            "file" => {
                let path = PathBuf::from(it.next()?);
                let line = it.next().and_then(|n| n.parse().ok());
                start.files.push((path, line));
            }
            "find" => {
                let flags = it.next()?;
                let pattern = it.next()?.to_string();
                start.find = Some(Query { pattern, regex: flags.contains('r'), case_sensitive: flags.contains('c'), whole_word: flags.contains('w'), ..Default::default() });
            }
            "follow" => start.follow = true,
            "." => break,
            _ => return None,
        }
    }
    Some(start)
}

/// Hands `start` to the running BigText; true when it took it.
pub fn try_send(lock: &Path, start: &StartOptions) -> bool {
    let Ok(text) = std::fs::read_to_string(lock) else { return false };
    let mut parts = text.split_whitespace();
    let (Some(port), Some(token), pid) = (parts.next().and_then(|p| p.parse::<u16>().ok()), parts.next(), parts.next().and_then(|p| p.parse::<u32>().ok())) else {
        return false;
    };
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_millis(500)) else { return false };
    let _ = s.set_read_timeout(Some(Duration::from_millis(2000)));
    let _ = s.set_write_timeout(Some(Duration::from_millis(1000)));
    // Windows lets only the foreground process give the focus away.
    #[cfg(windows)]
    if let Some(pid) = pid {
        #[link(name = "user32")]
        extern "system" {
            fn AllowSetForegroundWindow(pid: u32) -> i32;
        }
        // SAFETY: a plain Win32 call with a process id.
        unsafe {
            AllowSetForegroundWindow(pid);
        }
    }
    #[cfg(not(windows))]
    let _ = pid;
    if s.write_all(encode(start, token).as_bytes()).is_err() {
        return false;
    }
    let mut reply = String::new();
    BufReader::new(&s).read_line(&mut reply).is_ok() && reply.trim() == "ok"
}

/// Listens for other launches; each request goes to `deliver`. The lock file names this
/// process and is removed by [`Server::drop`].
pub struct Server {
    lock: PathBuf,
    token: String,
}

impl Drop for Server {
    fn drop(&mut self) {
        // Only while it is still ours (another window may have taken over).
        if std::fs::read_to_string(&self.lock).is_ok_and(|t| t.contains(&self.token)) {
            let _ = std::fs::remove_file(&self.lock);
        }
    }
}

fn random_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
    h.write_u32(std::process::id());
    let a = h.finish();
    let mut h2 = std::collections::hash_map::RandomState::new().build_hasher();
    h2.write_u64(a);
    format!("{a:016x}{:016x}", h2.finish())
}

pub fn serve(lock: PathBuf, deliver: impl Fn(StartOptions) + Send + 'static) -> std::io::Result<Server> {
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))?;
    let port = listener.local_addr()?.port();
    let token = random_token();
    if let Some(dir) = lock.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let temp = lock.with_extension("lock.tmp");
    std::fs::write(&temp, format!("{port} {token} {}\n", std::process::id()))?;
    std::fs::rename(&temp, &lock)?;
    let tok = token.clone();
    bigtext::jobs::spawn("instance", move || {
        for conn in listener.incoming() {
            let Ok(mut s) = conn else { continue };
            let _ = s.set_read_timeout(Some(Duration::from_millis(2000)));
            let mut lines = Vec::new();
            let mut r = BufReader::new(&s);
            loop {
                let mut l = String::new();
                match r.read_line(&mut l) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let l = l.trim_end_matches(['\r', '\n']).to_string();
                        let end = l == ".";
                        lines.push(l);
                        if end || lines.len() > 10_000 {
                            break;
                        }
                    }
                }
            }
            if let Some(start) = decode(&lines, &tok) {
                deliver(start);
                let _ = s.write_all(b"ok\n");
            }
        }
    });
    Ok(Server { lock, token })
}

/// The channel side of [`serve`] for the app: requests become messages.
pub fn serve_app(lock: PathBuf, tx: Sender<crate::app::Msg>, waker: bigtext::jobs::Waker) -> Option<Server> {
    serve(lock, move |start| {
        let _ = tx.send(crate::app::Msg::OpenRequest(Box::new(start)));
        waker();
    })
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_launch_hands_over_its_files() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("instance.lock");
        let (tx, rx) = std::sync::mpsc::channel();
        let server = serve(lock.clone(), move |s| {
            let _ = tx.send(s);
        })
        .unwrap();
        let start = StartOptions {
            files: vec![(PathBuf::from("/logs/a b.log"), Some(120)), (PathBuf::from("/logs/c.log"), None)],
            find: Some(Query { pattern: "time\tout".into(), regex: true, case_sensitive: true, ..Default::default() }),
            follow: true,
            stdin: None,
        };
        assert!(try_send(&lock, &start));
        let got = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(got.files.len(), 2);
        assert_eq!(got.files[0].1, Some(120));
        assert!(got.files[0].0.ends_with("a b.log"));
        assert_eq!(got.find.as_ref().map(|q| (q.pattern.as_str(), q.regex, q.case_sensitive)), Some(("time\tout", true, true)));
        assert!(got.follow);
        // A wrong token is refused.
        let text = std::fs::read_to_string(&lock).unwrap();
        let port = text.split_whitespace().next().unwrap();
        std::fs::write(&lock, format!("{port} wrong {}", std::process::id())).unwrap();
        assert!(!try_send(&lock, &start));
        std::fs::write(&lock, text).unwrap();
        drop(server);
        assert!(!lock.exists());
        assert!(!try_send(&lock, &start));
    }
}
