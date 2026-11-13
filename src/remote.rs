//! Remote files read in parts, without downloading them: HTTP(S) range requests (S3 too) and an
//! SSH session that sends the byte ranges asked for, both behind [`crate::file::RangeRead`]; and
//! searches that run on the SSH server with `grep`, so a 50 GB log is searched where it is.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::encoding::{Codec, Encoding};
use crate::file::RangeRead;
use crate::jobs::CancelToken;
use crate::search::Query;

// ---------------------------------------------------------------------------------------------
// HTTP

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .user_agent(format!("BigText/{}", env!("CARGO_PKG_VERSION")))
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_per_read(Some(Duration::from_secs(60)))
        .build()
        .new_agent()
}

fn http_error(e: ureq::Error) -> String {
    match e {
        ureq::Error::StatusCode(code) => format!("The server answered with HTTP status {code}."),
        e => e.to_string(),
    }
}

/// A web file read with range requests.
pub struct HttpRange {
    agent: ureq::Agent,
    url: String,
    /// A strong ETag: sent as If-Match, so a file replaced on the server fails instead of
    /// mixing two versions.
    etag: Option<String>,
}

/// What the server said to the first range request.
pub enum HttpOpen {
    /// It sends parts: the reader and the file's length.
    Ranges(HttpRange, u64),
    /// It sends only the whole file.
    NoRanges,
}

/// `bytes 0-0/12345` → 12345.
fn total_of(content_range: &str) -> Option<u64> {
    content_range.trim().strip_prefix("bytes ")?.rsplit_once('/')?.1.trim().parse().ok()
}

impl HttpRange {
    /// Asks for the first byte: a 206 answer gives the length and shows that ranges work.
    pub fn open(url: &str) -> Result<HttpOpen, String> {
        let agent = agent();
        let resp = agent.get(url).header("Range", "bytes=0-0").header("Accept-Encoding", "identity").call().map_err(http_error)?;
        if resp.status().as_u16() != 206 {
            return Ok(HttpOpen::NoRanges);
        }
        let header = |n: &str| resp.headers().get(n).and_then(|v| v.to_str().ok()).map(str::to_owned);
        let Some(len) = header("content-range").as_deref().and_then(total_of) else { return Ok(HttpOpen::NoRanges) };
        let etag = header("etag").filter(|e| !e.starts_with("W/"));
        Ok(HttpOpen::Ranges(HttpRange { agent, url: url.to_string(), etag }, len))
    }
}

impl RangeRead for HttpRange {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        let last = offset + buf.len() as u64 - 1;
        let mut tries = 0;
        loop {
            tries += 1;
            let mut req = self.agent.get(&self.url).header("Range", format!("bytes={offset}-{last}")).header("Accept-Encoding", "identity");
            if let Some(e) = &self.etag {
                req = req.header("If-Match", e);
            }
            match req.call() {
                Ok(resp) if resp.status().as_u16() == 206 => {
                    let starts = resp.headers().get("content-range").and_then(|v| v.to_str().ok()).is_some_and(|v| v.trim().starts_with(&format!("bytes {offset}-")));
                    if !starts {
                        return Err(io::Error::other("The server sent another part of the file than the one asked for."));
                    }
                    return resp.into_body().into_reader().read_exact(buf);
                }
                Ok(resp) => return Err(io::Error::other(format!("The server answered a range request with HTTP status {}.", resp.status().as_u16()))),
                Err(ureq::Error::StatusCode(412)) => return Err(io::Error::other("The file changed on the server.")),
                // One more try for a dropped connection.
                Err(e) if tries < 2 && !matches!(e, ureq::Error::StatusCode(_)) => continue,
                Err(e) => return Err(io::Error::other(http_error(e))),
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// SSH

/// A file on an SSH server: `target` is `user@host` or `host`; a `path` without a leading `/`
/// is in the home folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshFile {
    pub target: String,
    pub port: Option<u16>,
    pub path: String,
}

/// `s` quoted for the remote POSIX shell; a leading `~/` stays outside the quotes to expand.
pub fn quote(s: &str) -> String {
    let (home, rest) = match s.strip_prefix("~/") {
        Some(r) => ("~/", r),
        None => ("", s),
    };
    format!("{home}{}", quote_all(rest))
}

/// `s` in single quotes for a POSIX shell.
fn quote_all(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `ssh` with keys or an agent only (there is no terminal to type a password in), to `target`;
/// the remote command follows.
pub fn ssh_base(target: &str, port: Option<u16>) -> Command {
    let mut c = Command::new("ssh");
    // An ssh configuration of its own instead of ~/.ssh/config (the self-test's test server).
    if let Some(config) = std::env::var_os("BIGTEXT_SSH_CONFIG") {
        c.arg("-F").arg(config);
    }
    c.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=15", "-o", "ServerAliveInterval=30"]);
    if let Some(p) = port {
        c.arg("-p").arg(p.to_string());
    }
    c.arg(target).arg("--");
    hide_console(&mut c);
    c
}

/// A console program started from a window would open a console of its own on Windows.
pub fn hide_console(c: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = c;
}

/// `script` run by `sh` on the server, whatever the login shell is (fish, csh…).
fn sh_c(script: &str) -> String {
    format!("sh -c {}", quote_all(script))
}

/// The script an [`SshRange`] session runs: the file's size on the first line, then for each
/// `offset length` line on its input exactly `length` bytes from `offset` (padded with zeros if
/// the file got shorter, so the stream never loses its place).
pub fn serve_script(path: &str) -> String {
    format!(
        "f={}; [ -f \"$f\" ] || {{ echo \"$f: No such file\" >&2; exit 3; }}; \
         s=$(stat -c %s -- \"$f\" 2>/dev/null || stat -f %z -- \"$f\" 2>/dev/null || wc -c < \"$f\") || exit 3; \
         echo $s; \
         while read o n; do {{ tail -c +$((o+1)) -- \"$f\" 2>/dev/null | head -c \"$n\"; head -c \"$n\" /dev/zero; }} | head -c \"$n\"; done",
        quote(path)
    )
}

/// A running session: the size line is read, ranges follow.
struct Session {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Session {
    /// Starts `cmd` (ssh running [`serve_script`]) and reads the size line.
    fn start(mut cmd: Command) -> Result<(Session, u64), String> {
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| if e.kind() == io::ErrorKind::NotFound { "ssh was not found. Install the OpenSSH client (on Windows: Settings → System → Optional features).".to_string() } else { e.to_string() })?;
        let stdin = child.stdin.take().ok_or("no input")?;
        let mut stdout = BufReader::new(child.stdout.take().ok_or("no output")?);
        let mut stderr = child.stderr.take().ok_or("no output")?;
        let err = std::thread::spawn(move || {
            let mut s = String::new();
            let _ = stderr.read_to_string(&mut s);
            s
        });
        let mut line = String::new();
        let got = stdout.read_line(&mut line);
        match line.trim().parse::<u64>() {
            Ok(len) if got.is_ok() => Ok((Session { child, stdin, stdout }, len)),
            _ => {
                let _ = child.kill();
                let status = child.wait().ok().and_then(|s| s.code());
                Err(ssh_error(&err.join().unwrap_or_default(), status))
            }
        }
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        writeln!(self.stdin, "{offset} {}", buf.len())?;
        self.stdin.flush()?;
        self.stdout.read_exact(buf)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// What went wrong with `ssh`, in words a user can act on.
pub fn ssh_error(stderr: &str, code: Option<i32>) -> String {
    let s = stderr.trim();
    let mut msg = if s.is_empty() { format!("ssh exited with code {}", code.unwrap_or(-1)) } else { s.lines().last().unwrap_or(s).to_string() };
    if s.contains("Permission denied") || s.contains("Host key verification failed") {
        msg = format!("{msg}\nBigText signs in with your SSH keys or agent (and known hosts); password sign-in is not supported.");
    }
    msg
}

/// A file on an SSH server read through one long-lived session (started again once if the
/// connection drops).
pub struct SshRange {
    session: Mutex<Option<Session>>,
    make: Box<dyn Fn() -> Command + Send + Sync>,
}

impl SshRange {
    /// Connects and learns the size of `file`.
    pub fn open(file: &SshFile) -> Result<(SshRange, u64), String> {
        let f = file.clone();
        Self::open_with(Box::new(move || {
            let mut c = ssh_base(&f.target, f.port);
            c.arg(sh_c(&serve_script(&f.path)));
            c
        }))
    }

    /// [`SshRange::open`] with the command made by `make` (tests run the script with a local
    /// `sh`).
    pub fn open_with(make: Box<dyn Fn() -> Command + Send + Sync>) -> Result<(SshRange, u64), String> {
        let (s, len) = Session::start(make())?;
        Ok((SshRange { session: Mutex::new(Some(s)), make }, len))
    }
}

impl RangeRead for SshRange {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let mut guard = self.session.lock().unwrap();
        for attempt in 0..2 {
            if guard.is_none() {
                *guard = Some(Session::start((self.make)()).map_err(io::Error::other)?.0);
            }
            match guard.as_mut().unwrap().read_at(offset, buf) {
                Ok(()) => return Ok(()),
                Err(e) if attempt == 0 => {
                    let _ = e;
                    *guard = None;
                }
                Err(e) => return Err(e),
            }
        }
        unreachable!()
    }
}

// ---------------------------------------------------------------------------------------------
// Searching on the SSH server

/// Whether `grep` on the server finds what BigText would: the file is UTF-8 or a single-byte
/// encoding (then only for an ASCII pattern), and the search does not span lines.
pub fn server_can_search(q: &Query, codec: &Codec) -> bool {
    let bytes_match = match codec.enc {
        Encoding::Utf8 => true,
        Encoding::Utf16Le | Encoding::Utf16Be => false,
        Encoding::Windows1252 | Encoding::Windows1251 | Encoding::Latin1 => q.pattern.is_ascii(),
    };
    bytes_match && !q.pattern.is_empty() && !q.dot_all && !q.pattern.contains('\n')
}

/// Which matches to report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    All,
    /// The first match at or after the start of the range.
    First,
    /// The last match in the range.
    Last,
}

/// The `grep` script that prints `offset:match` for each match of `q` in `range` of the file at
/// `path` (offsets from the start of the range), as [`Pick`] says.
pub fn grep_script(path: &str, q: &Query, range: Option<(u64, u64)>, pick: Pick) -> String {
    let mut flags = String::from("-a -b -o");
    if !q.case_sensitive {
        flags.push_str(" -i");
    }
    if q.whole_word {
        flags.push_str(" -w");
    }
    if pick == Pick::First {
        flags.push_str(" -m 1");
    }
    // Rust's regex syntax is closest to PCRE (\d, \w, lazy repeats); grep without -P gets ERE.
    // Plain text ignoring case goes as an escaped ERE: grep 3.0 (Git for Windows, older
    // servers) aborts on -o -i -F.
    let (mode, pattern) = match (q.regex, q.case_sensitive) {
        (true, _) => ("m=-E; echo | grep -qP '' 2>/dev/null && m=-P; ", q.pattern.clone()),
        (false, true) => ("m=-F; ", q.pattern.clone()),
        (false, false) => ("m=-E; ", ere_escape(&q.pattern)),
    };
    // Case-folding beyond ASCII needs a UTF-8 locale; byte offsets are bytes either way.
    let locale = if q.case_sensitive || q.pattern.is_ascii() { "C" } else { "C.UTF-8" };
    let pattern = quote_all(&pattern);
    let grep = match range {
        None => format!("grep {flags} $m -e {pattern} -- \"$f\""),
        Some((s, e)) => format!("tail -c +{} -- \"$f\" | head -c {} | grep {flags} $m -e {pattern}", s + 1, e.saturating_sub(s)),
    };
    // -m 1 stops after the first matching line, but -o prints every match on it.
    let tail = match pick {
        Pick::All => "",
        Pick::First => " | head -n 1",
        Pick::Last => " | tail -n 1",
    };
    format!("LC_ALL={locale}; export LC_ALL; f={}; {mode}{grep}{tail}", quote(path))
}

/// `s` as a POSIX extended regular expression matching it literally.
fn ere_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        if r"\.[]{}()*+?^$|".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// `offset:match` lines → (start, end) byte ranges, `base` added.
pub fn parse_grep_line(line: &[u8], base: u64) -> Option<(u64, u64)> {
    let colon = line.iter().position(|&b| b == b':')?;
    let off: u64 = std::str::from_utf8(&line[..colon]).ok()?.parse().ok()?;
    let len = (line.len() - colon - 1) as u64;
    Some((base + off, base + off + len))
}

/// Runs `cmd` (a [`grep_script`] through ssh) and calls `found` with each match until it returns
/// false, the output ends or `cancel` fires. Errors carry what ssh or grep said.
pub fn run_grep(mut cmd: Command, base: u64, cancel: &CancelToken, mut found: impl FnMut(u64, u64) -> bool) -> Result<(), String> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let child = cmd.spawn().map_err(|e| e.to_string())?;
    let child = Arc::new(Mutex::new(child));
    let stdout = child.lock().unwrap().stdout.take().ok_or("no output")?;
    let mut stderr = child.lock().unwrap().stderr.take().ok_or("no output")?;
    let err = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    // A search can take a while on the server before it prints anything: Cancel ends it.
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let watcher = {
        let (child, done, cancel) = (child.clone(), done.clone(), cancel.clone());
        std::thread::spawn(move || {
            while !done.load(std::sync::atomic::Ordering::Relaxed) {
                if cancel.is_cancelled() {
                    let _ = child.lock().unwrap().kill();
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        })
    };
    let mut stopped = false;
    for line in BufReader::new(stdout).split(b'\n') {
        let Ok(line) = line else { break };
        if let Some((s, e)) = parse_grep_line(&line, base) {
            if !found(s, e) {
                stopped = true;
                let _ = child.lock().unwrap().kill();
                break;
            }
        }
    }
    done.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = watcher.join();
    let status = child.lock().unwrap().wait().map_err(|e| e.to_string())?;
    let stderr = err.join().unwrap_or_default();
    // grep: 0 found, 1 nothing found; a killed process was stopped on purpose.
    match status.code() {
        Some(0 | 1) => Ok(()),
        _ if stopped || cancel.is_cancelled() => Ok(()),
        code => Err(ssh_error(&stderr, code)),
    }
}

/// The ssh command running `script` on the server of `file`.
pub fn ssh_script(file: &SshFile, script: &str) -> Command {
    let mut c = ssh_base(&file.target, file.port);
    c.arg(sh_c(script));
    c
}

// ---------------------------------------------------------------------------------------------
// A small range-serving web server, for the self-test and the tests.

/// Serves `path` on 127.0.0.1 with range requests (and keep-alive) until the process ends;
/// returns the URL. `ranges: false` answers every request with the whole file.
pub fn serve_for_test(path: PathBuf, ranges: bool) -> io::Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let path = path.clone();
            std::thread::spawn(move || {
                let _ = serve_conn(stream, &path, ranges);
            });
        }
    });
    Ok(format!("http://127.0.0.1:{port}/files/{name}"))
}

fn serve_conn(stream: TcpStream, path: &PathBuf, ranges: bool) -> io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut out = stream;
    loop {
        let mut range = None;
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        loop {
            let mut h = String::new();
            if reader.read_line(&mut h)? == 0 || h == "\r\n" || h == "\n" {
                break;
            }
            if let Some(v) = h.to_ascii_lowercase().strip_prefix("range: bytes=") {
                if let Some((a, b)) = v.trim().split_once('-') {
                    range = Some((a.parse::<u64>().unwrap_or(0), b.parse::<u64>().ok()));
                }
            }
        }
        let mut f = std::fs::File::open(path)?;
        let len = f.metadata()?.len();
        match range.filter(|_| ranges) {
            Some((a, b)) => {
                let b = b.unwrap_or(len - 1).min(len - 1);
                let n = b + 1 - a;
                write!(out, "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {a}-{b}/{len}\r\nContent-Length: {n}\r\nETag: \"t1\"\r\nAccept-Ranges: bytes\r\n\r\n")?;
                let mut buf = vec![0u8; n as usize];
                crate::file::read_exact_at(&f, &mut buf, a)?;
                out.write_all(&buf)?;
            }
            None => {
                write!(out, "HTTP/1.1 200 OK\r\nContent-Length: {len}\r\n\r\n")?;
                io::copy(&mut f, &mut out)?;
            }
        }
        out.flush()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_ranges_read_parts_of_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.log");
        let data: Vec<u8> = (0..3_000_000u32).map(|i| b"abcdefghij\n"[(i % 11) as usize]).collect();
        std::fs::write(&path, &data).unwrap();
        let url = serve_for_test(path.clone(), true).unwrap();
        let HttpOpen::Ranges(r, len) = HttpRange::open(&url).unwrap() else { panic!("no ranges") };
        assert_eq!(len, data.len() as u64);
        let mut buf = vec![0u8; 5000];
        r.read_at(2_000_000, &mut buf).unwrap();
        assert_eq!(&buf[..], &data[2_000_000..2_005_000]);
        r.read_at(len - 10, &mut buf[..10]).unwrap();
        assert_eq!(&buf[..10], &data[data.len() - 10..]);
        // A server that sends only whole files is told apart.
        let whole = serve_for_test(path, false).unwrap();
        assert!(matches!(HttpRange::open(&whole).unwrap(), HttpOpen::NoRanges));
        assert_eq!(total_of("bytes 0-0/12345"), Some(12345));
        assert_eq!(total_of("bytes 0-0/*"), None);
    }

    #[test]
    fn grep_scripts_and_their_output() {
        let q = Query { pattern: "it's".into(), regex: false, case_sensitive: true, whole_word: false, dot_all: false };
        let s = grep_script("/var/log/a b.log", &q, None, Pick::All);
        assert!(s.contains("LC_ALL=C;") && s.contains("m=-F") && s.contains(r"-e 'it'\''s'") && s.contains("f='/var/log/a b.log'"), "{s}");
        let rq = Query { pattern: r"err\d+".into(), regex: true, case_sensitive: false, whole_word: true, dot_all: false };
        let s = grep_script("~/x.log", &rq, Some((100, 900)), Pick::First);
        assert!(s.contains("grep -qP") && s.contains("-i -w -m 1") && s.contains("tail -c +101") && s.contains("head -c 800") && s.contains("f=~/'x.log'"), "{s}");
        assert!(grep_script("x", &q, None, Pick::Last).ends_with("| tail -n 1"));
        assert_eq!(parse_grep_line(b"1234:ERROR", 100), Some((1334, 1339)));
        assert_eq!(parse_grep_line(b"7:a:b", 0), Some((7, 10)));
        assert_eq!(parse_grep_line(b"garbage", 0), None);
        let utf8 = Codec::utf8();
        assert!(server_can_search(&q, &utf8));
        assert!(!server_can_search(&Query { dot_all: true, ..q.clone() }, &utf8));
        assert!(!server_can_search(&Query { pattern: String::new(), ..q }, &utf8));
    }

    /// `sh` for running the server-side scripts locally (skipped where there is none).
    fn local_sh() -> Option<Command> {
        let c = Command::new("sh").arg("-c").arg("exit 0").status().ok()?;
        c.success().then(|| Command::new("sh"))
    }

    #[test]
    fn the_session_script_serves_ranges() {
        let Some(_) = local_sh() else { return };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.log");
        let data: Vec<u8> = (0..400_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &data).unwrap();
        // The script as sh on the server would run it, with this machine's path.
        let p = path.to_string_lossy().replace('\\', "/");
        let (r, len) = SshRange::open_with(Box::new(move || {
            let mut c = Command::new("sh");
            c.arg("-c").arg(serve_script(&p));
            c
        }))
        .unwrap();
        assert_eq!(len, data.len() as u64);
        let mut buf = vec![0u8; 70_000];
        r.read_at(123_456, &mut buf).unwrap();
        assert_eq!(&buf[..], &data[123_456..193_456]);
        r.read_at(0, &mut buf[..10]).unwrap();
        assert_eq!(&buf[..10], &data[..10]);
        // A missing file fails at once with the shell's words.
        let e = SshRange::open_with(Box::new(|| {
            let mut c = Command::new("sh");
            c.arg("-c").arg(serve_script("/nonexistent/x.log"));
            c
        }))
        .err()
        .unwrap();
        assert!(e.contains("No such file"), "{e}");
    }

    /// Against a real SSH server: `BIGTEXT_SSH_LIVE="user@host port /path/file key known_hosts"`
    /// (the file: 64-byte lines `line NNNNNNN of the remote file…`, ERROR on every thousandth
    /// from line 7), then `cargo test --lib ssh_live -- --ignored`.
    #[test]
    #[ignore]
    fn ssh_live() {
        let Ok(spec) = std::env::var("BIGTEXT_SSH_LIVE") else { return };
        let v: Vec<String> = spec.split_whitespace().map(str::to_owned).collect();
        let (target, port, path, key, known) = (v[0].clone(), v[1].clone(), v[2].clone(), v[3].clone(), v[4].clone());
        let ssh = {
            let (target, port, key, known) = (target.clone(), port.clone(), key.clone(), known.clone());
            move |script: &str| {
                let mut c = Command::new("ssh");
                c.args(["-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=accept-new", "-o"]).arg(format!("UserKnownHostsFile={known}")).arg("-i").arg(&key).arg("-p").arg(&port).arg(&target).arg("--").arg(sh_c(script));
                c
            }
        };
        let p = path.clone();
        let s2 = ssh.clone();
        let t = std::time::Instant::now();
        let (r, len) = SshRange::open_with(Box::new(move || s2(&serve_script(&p)))).unwrap();
        eprintln!("opened {len} bytes in {:?}", t.elapsed());
        let mut buf = vec![0u8; 64];
        for line in [0u64, 400_000, len / 64 - 1] {
            let t = std::time::Instant::now();
            r.read_at(line * 64, &mut buf).unwrap();
            let text = String::from_utf8_lossy(&buf);
            eprintln!("line {line} in {:?}: {}", t.elapsed(), text.trim_end());
            assert!(text.starts_with(&format!("line {line:07} of the remote file")), "{text}");
        }
        let mut big = vec![0u8; 256 * 1024];
        let t = std::time::Instant::now();
        r.read_at(10_000_000, &mut big).unwrap();
        eprintln!("256 KB in {:?}", t.elapsed());
        let q = Query { pattern: "ERROR disk".into(), regex: false, case_sensitive: true, whole_word: false, dot_all: false };
        let t = std::time::Instant::now();
        let mut n = 0;
        let mut first = None;
        run_grep(ssh(&grep_script(&path, &q, None, Pick::All)), 0, &CancelToken::new(), |s, e| {
            first.get_or_insert((s, e));
            n += 1;
            true
        })
        .unwrap();
        eprintln!("{n} matches on the server in {:?}, first {first:?}", t.elapsed());
        // "line 0000007 of the remote file" is 31 bytes, then " ERROR".
        assert_eq!(first, Some((7 * 64 + 32, 7 * 64 + 42)));
        let mut last = None;
        let rq = Query { pattern: r"line 0[0-9]+007 of the remote file ERROR".into(), regex: true, case_sensitive: false, ..q };
        run_grep(ssh(&grep_script(&path, &rq, Some((0, 500_000 * 64)), Pick::Last)), 0, &CancelToken::new(), |s, e| {
            last = Some((s, e));
            true
        })
        .unwrap();
        eprintln!("last before line 500000: {last:?}");
        assert_eq!(last.map(|l| l.0), Some(499_007 * 64));
    }

    #[test]
    fn grep_runs_and_reports_byte_offsets() {
        let Some(_) = local_sh() else { return };
        if Command::new("sh").arg("-c").arg("echo x | grep -q x").status().map(|s| !s.success()).unwrap_or(true) {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("g.log");
        std::fs::write(&path, "ok one\nERROR two\nok\nerror three ERROR\n").unwrap();
        let p = path.to_string_lossy().replace('\\', "/");
        let run = |q: &Query, range: Option<(u64, u64)>, pick: Pick| {
            let mut c = Command::new("sh");
            c.arg("-c").arg(grep_script(&p, q, range, pick));
            let mut v = Vec::new();
            run_grep(c, range.map_or(0, |r| r.0), &CancelToken::new(), |s, e| {
                v.push((s, e));
                true
            })
            .unwrap();
            v
        };
        // "ok one\n" 0-6, "ERROR two\n" 7-16, "ok\n" 17-19, "error three ERROR\n" 20-37.
        let q = Query { pattern: "ERROR".into(), regex: false, case_sensitive: true, whole_word: false, dot_all: false };
        assert_eq!(run(&q, None, Pick::All), vec![(7, 12), (32, 37)]);
        let qi = Query { case_sensitive: false, ..q.clone() };
        assert_eq!(run(&qi, None, Pick::All), vec![(7, 12), (20, 25), (32, 37)]);
        assert_eq!(run(&qi, Some((10, 38)), Pick::First), vec![(20, 25)]);
        assert_eq!(run(&qi, Some((0, 30)), Pick::Last), vec![(20, 25)]);
        let rq = Query { pattern: "e[a-z]+r".into(), regex: true, case_sensitive: true, ..q.clone() };
        assert_eq!(run(&rq, None, Pick::All), vec![(20, 25)]);
        assert!(run(&Query { pattern: "nothing".into(), ..q.clone() }, None, Pick::All).is_empty());
        // Plain text with regex characters, ignoring case, stays plain.
        let dots = Query { pattern: "E.R".into(), case_sensitive: false, ..q };
        assert!(run(&dots, None, Pick::All).is_empty());
        assert_eq!(ere_escape("a.b*(c)"), r"a\.b\*\(c\)");
    }
}
