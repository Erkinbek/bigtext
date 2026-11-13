//! Showing a file in the system's file manager, opening a terminal in its folder, and opening it
//! with its default program. The commands are only started, never waited for on the UI thread.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::i18n::tr;
use super::BigTextApp;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellAction {
    /// The file selected in Explorer / Finder / the file manager.
    Reveal,
    /// A terminal in the file's folder.
    Terminal,
    /// The file in the program the system opens it with.
    Default,
}

fn folder_of(path: &Path) -> PathBuf {
    if path.is_dir() {
        return path.to_path_buf();
    }
    path.parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// The command that does `action` for `path` here.
#[cfg(windows)]
pub fn command(action: ShellAction, path: &Path) -> Command {
    use std::os::windows::process::CommandExt;
    let shown = bigtext::file::display_path(path);
    match action {
        ShellAction::Reveal if path.exists() => {
            let mut c = Command::new("explorer.exe");
            c.raw_arg(format!("/select,\"{shown}\""));
            c
        }
        ShellAction::Reveal => {
            let mut c = Command::new("explorer.exe");
            c.arg(bigtext::file::display_path(&folder_of(path)));
            c
        }
        ShellAction::Terminal => {
            // CREATE_NEW_CONSOLE: its own window.
            let mut c = Command::new("cmd.exe");
            c.arg("/K")
                .current_dir(folder_of(path))
                .creation_flags(0x10);
            c
        }
        ShellAction::Default => {
            let mut c = Command::new("rundll32.exe");
            c.arg("url.dll,FileProtocolHandler").arg(shown);
            c
        }
    }
}

#[cfg(target_os = "macos")]
pub fn command(action: ShellAction, path: &Path) -> Command {
    let mut c = Command::new("open");
    match action {
        ShellAction::Reveal if path.exists() => c.arg("-R").arg(path),
        ShellAction::Reveal => c.arg(folder_of(path)),
        ShellAction::Terminal => c.args(["-a", "Terminal"]).arg(folder_of(path)),
        ShellAction::Default => c.arg(path),
    };
    c
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn command(action: ShellAction, path: &Path) -> Command {
    match action {
        // The folder (file managers differ in how to select a file in it).
        ShellAction::Reveal => {
            let mut c = Command::new("xdg-open");
            c.arg(folder_of(path));
            c
        }
        ShellAction::Terminal => {
            let term = std::env::var("TERMINAL")
                .ok()
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| "x-terminal-emulator".into());
            let mut c = Command::new(term);
            c.current_dir(folder_of(path));
            c
        }
        ShellAction::Default => {
            let mut c = Command::new("xdg-open");
            c.arg(path);
            c
        }
    }
}

/// Moves `path` (a file or a folder) to the Recycle Bin, so it can be restored. Blocks until
/// done: call it on a worker.
#[cfg(windows)]
pub fn move_to_trash(path: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::{
        SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FO_DELETE,
        SHFILEOPSTRUCTW,
    };
    // The shell takes neither `\\?\` paths nor relative ones; the list ends with two NULs.
    let full = std::path::absolute(path).map_err(|e| e.to_string())?;
    let mut from: Vec<u16> = std::ffi::OsStr::new(&bigtext::file::display_path(&full))
        .encode_wide()
        .collect();
    from.extend([0, 0]);
    // SAFETY: the struct is plain data, zero is a valid value for every field, and `from`
    // outlives the call.
    let code = unsafe {
        let mut op: SHFILEOPSTRUCTW = std::mem::zeroed();
        op.wFunc = FO_DELETE as _;
        op.pFrom = from.as_ptr();
        op.fFlags = (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI) as _;
        let code = SHFileOperationW(&mut op);
        if code == 0 && op.fAnyOperationsAborted != 0 {
            -1
        } else {
            code
        }
    };
    match code {
        0 => Ok(()),
        -1 => Err(tr("The operation was cancelled.").into()),
        c => Err(format!(
            "{} (0x{c:X})",
            tr("Windows could not move it to the Recycle Bin")
        )),
    }
}

/// Moves `path` to the Trash with the Finder, so it can be put back.
#[cfg(target_os = "macos")]
pub fn move_to_trash(path: &Path) -> Result<(), String> {
    let full = std::path::absolute(path).map_err(|e| e.to_string())?;
    let p = full
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let out = Command::new("osascript")
        .arg("-e")
        .arg(format!(
            "tell application \"Finder\" to delete POSIX file \"{p}\""
        ))
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Moves `path` to the desktop's trash: with `gio trash`, else into the home trash as the
/// freedesktop.org specification describes (same file system only).
#[cfg(all(unix, not(target_os = "macos")))]
pub fn move_to_trash(path: &Path) -> Result<(), String> {
    let full = std::path::absolute(path).map_err(|e| e.to_string())?;
    if Command::new("gio")
        .arg("trash")
        .arg("--")
        .arg(&full)
        .output()
        .is_ok_and(|o| o.status.success())
    {
        return Ok(());
    }
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share")))
        .ok_or("no home folder")?;
    let trash = data.join("Trash");
    let (files, info) = (trash.join("files"), trash.join("info"));
    std::fs::create_dir_all(&files)
        .and_then(|_| std::fs::create_dir_all(&info))
        .map_err(|e| e.to_string())?;
    let name = full
        .file_name()
        .ok_or("no file name")?
        .to_string_lossy()
        .into_owned();
    let mut n = 0;
    let target = loop {
        let candidate = if n == 0 {
            name.clone()
        } else {
            format!("{name}.{n}")
        };
        if !files.join(&candidate).exists() && !info.join(format!("{candidate}.trashinfo")).exists()
        {
            break candidate;
        }
        n += 1;
    };
    let encoded: String = full
        .to_string_lossy()
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // In UTC: the specification asks for local time, which needs no more than a label here.
    let date = bigtext::timestamp::format_key(secs as i64 * 1000, true, true).replace(' ', "T");
    std::fs::write(
        info.join(format!("{target}.trashinfo")),
        format!("[Trash Info]\nPath={encoded}\nDeletionDate={date}\n"),
    )
    .map_err(|e| e.to_string())?;
    std::fs::rename(&full, files.join(&target)).map_err(|e| {
        let _ = std::fs::remove_file(info.join(format!("{target}.trashinfo")));
        e.to_string()
    })
}

/// Starts `action` for `path`; reaps the process on a worker.
pub fn run(action: ShellAction, path: &Path) -> std::io::Result<()> {
    let mut child = command(action, path).spawn()?;
    bigtext::jobs::spawn("shell", move || {
        let _ = child.wait();
    });
    Ok(())
}

impl BigTextApp {
    /// The file a document stands for: the compressed file or archive of an unpacked copy.
    pub(crate) fn shell_path(&self, id: u64) -> Option<PathBuf> {
        let d = self.docs.iter().find(|d| d.id == id)?;
        d.unpacked_from.clone().or_else(|| d.path.clone())
    }

    pub(crate) fn shell_doc(&mut self, id: u64, action: ShellAction) {
        let Some(p) = self.shell_path(id) else { return };
        self.shell_file(&p, action);
    }

    pub(crate) fn shell_file(&mut self, path: &Path, action: ShellAction) {
        if let Err(e) = run(action, path) {
            let what = match action {
                ShellAction::Reveal => tr("Could not show the file in its folder"),
                ShellAction::Terminal => {
                    tr("Could not open a terminal (set the TERMINAL variable to yours)")
                }
                ShellAction::Default => tr("Could not open the file with its default app"),
            };
            self.error = Some(format!("{what}: {e}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(c: &Command) -> (String, Vec<String>) {
        (
            c.get_program().to_string_lossy().into_owned(),
            c.get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect(),
        )
    }

    #[test]
    fn commands_for_this_system() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a b.log");
        std::fs::write(&f, "x").unwrap();
        let (prog, args) = argv(&command(ShellAction::Default, &f));
        let (rprog, rargs) = argv(&command(ShellAction::Reveal, &f));
        let gone = argv(&command(ShellAction::Reveal, &dir.path().join("gone.log")));
        let term = command(ShellAction::Terminal, &f);
        assert_eq!(term.get_current_dir(), Some(dir.path()));
        if cfg!(windows) {
            assert_eq!(
                (prog.as_str(), args[0].as_str()),
                ("rundll32.exe", "url.dll,FileProtocolHandler")
            );
            assert!(args[1].ends_with("a b.log"));
            assert_eq!(rprog, "explorer.exe");
            assert!(
                rargs[0].starts_with("/select,\"") && rargs[0].ends_with("a b.log\""),
                "{rargs:?}"
            );
            assert!(!gone.1[0].starts_with("/select"));
        } else if cfg!(target_os = "macos") {
            assert_eq!((prog.as_str(), rargs[0].as_str()), ("open", "-R"));
        } else {
            assert_eq!((prog.as_str(), rprog.as_str()), ("xdg-open", "xdg-open"));
            assert_eq!(PathBuf::from(&rargs[0]), dir.path());
        }
        let _ = gone;
    }
}
