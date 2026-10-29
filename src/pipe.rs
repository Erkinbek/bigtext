//! Running a shell command with text on its standard input and its standard output going to a
//! writer. Input, output and errors are pumped on their own threads, so a chatty command never
//! deadlocks, and cancelling kills it.

use std::io::{self, BufWriter, Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::jobs::{CancelToken, Progress};

/// The program a command line is given to, with the arguments before the command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shell {
    pub program: String,
    pub args: Vec<String>,
}

impl Shell {
    /// `cmd /C` on Windows, the login shell on macOS (so Homebrew tools are found), `/bin/sh -c`
    /// elsewhere.
    pub fn system() -> Shell {
        if cfg!(windows) {
            Shell { program: "cmd".into(), args: vec!["/C".into()] }
        } else if cfg!(target_os = "macos") {
            Shell { program: std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into()), args: vec!["-lc".into()] }
        } else {
            Shell { program: "/bin/sh".into(), args: vec!["-c".into()] }
        }
    }

    /// Git for Windows' bash, when it is installed.
    pub fn git_bash() -> Option<Shell> {
        let roots = ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)", "LOCALAPPDATA"];
        roots
            .iter()
            .filter_map(std::env::var_os)
            .flat_map(|r| [std::path::PathBuf::from(&r).join("Git").join("bin").join("bash.exe"), std::path::PathBuf::from(&r).join("Programs").join("Git").join("bin").join("bash.exe")])
            .find(|p| p.is_file())
            .map(|p| Shell { program: p.to_string_lossy().into_owned(), args: vec!["-c".into()] })
    }

    /// The Windows Subsystem for Linux's shell.
    pub fn wsl() -> Shell {
        Shell { program: "wsl".into(), args: vec!["-e".into(), "sh".into(), "-c".into()] }
    }

    fn command(&self, cmd: &str) -> Command {
        let mut c = Command::new(&self.program);
        c.args(&self.args);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // cmd.exe parses its own command line: hand it over as typed.
            if self.program.eq_ignore_ascii_case("cmd") {
                c.raw_arg(cmd);
            } else {
                c.arg(cmd);
            }
            // No console window flashing up from the windowed program.
            c.creation_flags(CREATE_NO_WINDOW);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // Its own process group, so cancelling stops the whole pipeline.
            c.process_group(0);
        }
        #[cfg(not(windows))]
        c.arg(cmd);
        c
    }
}

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Stops the shell and everything it started (killing the shell alone leaves `sort` running,
/// holding the output pipe open).
fn kill_tree(c: &mut std::process::Child) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = Command::new("taskkill").args(["/T", "/F", "/PID", &c.id().to_string()]).creation_flags(CREATE_NO_WINDOW).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
    #[cfg(unix)]
    // SAFETY: kill(2) on our own child's process group.
    unsafe {
        libc::kill(-(c.id() as i32), libc::SIGKILL);
    }
    let _ = c.kill();
}

/// How a command ended.
#[derive(Debug)]
pub struct Ran {
    /// The exit code (`None` when it was killed by a signal).
    pub code: Option<i32>,
    /// The end of what it wrote to standard error.
    pub stderr: String,
    pub out_bytes: u64,
}

/// What it wrote to standard error is kept up to this much (the end of it).
const STDERR_TAIL: usize = 8 * 1024;

/// The writer a command's standard input is fed through.
pub type Feed = Box<dyn FnOnce(&mut dyn Write) -> io::Result<()> + Send>;

/// Runs `cmd` with `shell`: `feed` writes its standard input (on a thread of its own), its
/// standard output is copied to `out`. Cancelling kills the command and returns `Interrupted`.
pub fn run(shell: &Shell, cmd: &str, feed: Feed, out: &mut dyn Write, cancel: &CancelToken, progress: Option<&Progress>) -> io::Result<Ran> {
    let mut child = shell.command(cmd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let (Some(stdin), Some(mut stdout), Some(mut stderr)) = (child.stdin.take(), child.stdout.take(), child.stderr.take()) else {
        let _ = child.kill();
        return Err(io::Error::other("no pipes to the command"));
    };
    let feeder = std::thread::spawn(move || {
        let mut w = BufWriter::new(stdin);
        let r = feed(&mut w).and_then(|_| w.flush());
        drop(w);
        // A command that stops reading early (head, grep -m) is fine.
        match r {
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(()),
            r => r,
        }
    });
    let drainer = std::thread::spawn(move || {
        let mut tail: Vec<u8> = Vec::new();
        let mut buf = [0u8; 8192];
        while let Ok(n) = stderr.read(&mut buf) {
            if n == 0 {
                break;
            }
            tail.extend_from_slice(&buf[..n]);
            if tail.len() > 2 * STDERR_TAIL {
                tail.drain(..tail.len() - STDERR_TAIL);
            }
        }
        let from = tail.len().saturating_sub(STDERR_TAIL);
        String::from_utf8_lossy(&tail[from..]).into_owned()
    });
    let child = Arc::new(Mutex::new(child));
    let done = Arc::new(AtomicBool::new(false));
    let poller = {
        let (child, done, cancel) = (child.clone(), done.clone(), cancel.clone());
        std::thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                if cancel.is_cancelled() {
                    if let Ok(mut c) = child.lock() {
                        kill_tree(&mut c);
                    }
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        })
    };
    let mut out_bytes = 0u64;
    let mut buf = vec![0u8; 64 * 1024];
    let copied = loop {
        match stdout.read(&mut buf) {
            Ok(0) => break Ok(()),
            Ok(n) => {
                if let Err(e) = out.write_all(&buf[..n]) {
                    break Err(e);
                }
                out_bytes += n as u64;
                if let Some(p) = progress {
                    p.add(n as u64);
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => break Err(e),
        }
    };
    if copied.is_err() {
        if let Ok(mut c) = child.lock() {
            kill_tree(&mut c);
        }
    }
    let status = child.lock().map_err(|_| io::Error::other("lock"))?.wait();
    done.store(true, Ordering::Relaxed);
    let _ = poller.join();
    let fed = feeder.join().unwrap_or_else(|_| Err(io::Error::other("the input thread failed")));
    let stderr = drainer.join().unwrap_or_default();
    if cancel.is_cancelled() {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
    }
    copied?;
    out.flush()?;
    let status = status?;
    // Input that could not be written matters only when the command was happy.
    if status.success() {
        fed?;
    }
    Ok(Ran { code: status.code(), stderr, out_bytes })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(text: &'static str) -> Feed {
        Box::new(move |w| w.write_all(text.as_bytes()))
    }

    #[test]
    fn sorts_through_the_shell() {
        let mut out = Vec::new();
        let ran = run(&Shell::system(), "sort", feed("b\nc\na\n"), &mut out, &CancelToken::new(), None).unwrap();
        assert_eq!(ran.code, Some(0));
        let lines: Vec<&str> = std::str::from_utf8(&out).unwrap().lines().map(str::trim_end).collect();
        assert_eq!(lines, ["a", "b", "c"]);
    }

    #[test]
    fn exit_codes_and_errors() {
        let mut out = Vec::new();
        let cmd = if cfg!(windows) { "echo oops 1>&2 & exit 3" } else { "echo oops >&2; exit 3" };
        let ran = run(&Shell::system(), cmd, feed(""), &mut out, &CancelToken::new(), None).unwrap();
        assert_eq!(ran.code, Some(3));
        assert!(ran.stderr.contains("oops"), "{:?}", ran.stderr);
    }

    #[test]
    fn big_input_and_output_do_not_deadlock() {
        // 4 MB through a filter that echoes it all back.
        let text: &'static str = Box::leak((0..200_000).map(|i| format!("line {i:08}\n")).collect::<String>().into_boxed_str());
        let mut out = Vec::new();
        let cmd = if cfg!(windows) { "findstr \"^\"" } else { "cat" };
        let ran = run(&Shell::system(), cmd, feed(text), &mut out, &CancelToken::new(), None).unwrap();
        assert_eq!(ran.code, Some(0));
        assert_eq!(std::str::from_utf8(&out).unwrap().lines().count(), 200_000);
    }

    #[test]
    fn cancel_kills_it() {
        let cancel = CancelToken::new();
        let c2 = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            c2.cancel();
        });
        // Waits for input that never ends.
        let endless: Feed = Box::new(|w| loop {
            w.write_all(&[b'x'; 4096])?;
        });
        let cmd = if cfg!(windows) { "findstr zzz" } else { "grep zzz" };
        let r = run(&Shell::system(), cmd, endless, &mut Vec::new(), &cancel, None);
        assert_eq!(r.unwrap_err().kind(), io::ErrorKind::Interrupted);
    }
}
