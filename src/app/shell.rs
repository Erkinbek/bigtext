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
    path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."))
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
            c.arg("/K").current_dir(folder_of(path)).creation_flags(0x10);
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
            let term = std::env::var("TERMINAL").ok().filter(|t| !t.is_empty()).unwrap_or_else(|| "x-terminal-emulator".into());
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
                ShellAction::Terminal => tr("Could not open a terminal (set the TERMINAL variable to yours)"),
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
        (c.get_program().to_string_lossy().into_owned(), c.get_args().map(|a| a.to_string_lossy().into_owned()).collect())
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
            assert_eq!((prog.as_str(), args[0].as_str()), ("rundll32.exe", "url.dll,FileProtocolHandler"));
            assert!(args[1].ends_with("a b.log"));
            assert_eq!(rprog, "explorer.exe");
            assert!(rargs[0].starts_with("/select,\"") && rargs[0].ends_with("a b.log\""), "{rargs:?}");
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
