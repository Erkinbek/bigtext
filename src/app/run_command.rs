//! Edit → Run command on selection…: the selection (or the document) goes to a shell command's
//! standard input and its output replaces the selection, opens in a new tab or only its exit
//! code is shown. Input and output stream through pipes and a temporary file, so a 10 GB
//! selection never sits in memory. Edit → External tools runs saved commands with the file,
//! line and selection filled in.

use std::io::Write;
use std::ops::Range;
use std::path::PathBuf;

use bigtext::encoding::Encoding;
use bigtext::file;
use bigtext::jobs::{human_bytes, Progress};
use bigtext::pipe::{self, Shell};
use bigtext::text::TextBuf;

use super::document::EditKind;
use super::i18n::{tr, trf};
use super::tasks::TaskOutcome;
use super::BigTextApp;

/// Commands remembered for the window's history.
pub const HISTORY_MAX: usize = 30;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RunInput {
    #[default]
    Selection,
    Document,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RunOutput {
    #[default]
    Replace,
    NewTab,
    ExitCode,
}

/// Which shell runs the command.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ShellChoice {
    #[default]
    System,
    GitBash,
    Wsl,
}

impl ShellChoice {
    pub fn id(self) -> &'static str {
        match self {
            ShellChoice::System => "system",
            ShellChoice::GitBash => "git-bash",
            ShellChoice::Wsl => "wsl",
        }
    }

    pub fn from_id(v: &str) -> ShellChoice {
        [ShellChoice::GitBash, ShellChoice::Wsl].into_iter().find(|s| s.id() == v).unwrap_or_default()
    }

    fn shell(self) -> Result<Shell, String> {
        match self {
            ShellChoice::System => Ok(Shell::system()),
            ShellChoice::GitBash => Shell::git_bash().ok_or_else(|| tr("Git Bash was not found (it comes with Git for Windows).").to_string()),
            ShellChoice::Wsl => Ok(Shell::wsl()),
        }
    }

    fn label(self) -> String {
        match self {
            ShellChoice::System => {
                let s = Shell::system();
                format!("{} {}", s.program, s.args.join(" "))
            }
            ShellChoice::GitBash => "Git Bash".into(),
            ShellChoice::Wsl => "WSL".into(),
        }
    }
}

/// A saved command for Edit → External tools.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tool {
    pub name: String,
    /// With {file}, {dir}, {line}, {col} and {selection} filled in when it runs.
    pub command: String,
}

#[derive(Default)]
pub struct RunCommandUi {
    pub open: bool,
    pub command: String,
    pub input: RunInput,
    pub output: RunOutput,
    pub shell: ShellChoice,
    /// Newest first.
    pub history: Vec<String>,
    /// Where Up/Down is in the history.
    history_at: Option<usize>,
    tool_name: String,
    focus: bool,
}

/// What a finished command left.
pub struct CommandOutput {
    pub doc: u64,
    pub version: u64,
    pub range: Range<u64>,
    pub output: RunOutput,
    pub command: String,
    /// Its standard output.
    pub path: PathBuf,
    pub code: Option<i32>,
    pub stderr: String,
}

/// Writes `range` of `buf` as UTF-8 (without a byte order mark) to `w`.
fn write_utf8(buf: &TextBuf, range: Range<u64>, w: &mut dyn Write) -> std::io::Result<()> {
    let codec = buf.codec;
    let unit = codec.unit() as u64;
    let mut at = range.start.max(codec.bom_len as u64);
    while at < range.end {
        let mut to = (at + (1 << 20)).min(range.end);
        to -= (to - at) % unit;
        if to <= at {
            break;
        }
        let b = buf.read(at..to).ok_or_else(|| std::io::Error::other("could not read the document"))?;
        if codec.enc == Encoding::Utf8 {
            w.write_all(&b)?;
        } else {
            w.write_all(codec.decode_lossy(&b).as_bytes())?;
        }
        at = to;
    }
    Ok(())
}

/// `{file}`, `{dir}`, `{line}`, `{col}` and `{selection}` in a tool's command.
pub fn expand(command: &str, file: Option<&std::path::Path>, line: u64, col: u64, selection: &str) -> String {
    let path = file.map(file::display_path).unwrap_or_default();
    let dir = file.and_then(|f| f.parent()).map(file::display_path).unwrap_or_default();
    command.replace("{file}", &path).replace("{dir}", &dir).replace("{line}", &line.to_string()).replace("{col}", &col.to_string()).replace("{selection}", selection)
}

impl BigTextApp {
    pub(crate) fn open_run_command(&mut self) {
        let ui = &mut self.run_cmd;
        ui.open = true;
        ui.focus = true;
        ui.history_at = None;
        // With nothing selected the whole document goes in.
        let selected = self.docs.get(self.active).is_some_and(|d| d.has_selection());
        ui.input = if selected { RunInput::Selection } else { RunInput::Document };
    }

    pub(super) fn run_command_window(&mut self, ctx: &egui::Context) {
        if !self.run_cmd.open {
            return;
        }
        let mut open = true;
        let mut run = false;
        let mut save_tool = false;
        let editable = self.docs.get(self.active).is_some_and(|d| d.editable());
        let selected = self.docs.get(self.active).is_some_and(|d| d.has_selection());
        egui::Window::new(tr("Run command")).open(&mut open).collapsible(false).default_width(560.0).show(ctx, |ui| {
            let r = &mut self.run_cmd;
            ui.label(tr("The text goes to the command's standard input; its standard output comes back. Up and Down go through earlier commands."));
            ui.add_space(4.0);
            let edit = ui.add(egui::TextEdit::singleline(&mut r.command).hint_text("sort | uniq -c").desired_width(f32::INFINITY).font(egui::TextStyle::Monospace));
            if std::mem::take(&mut r.focus) {
                edit.request_focus();
            }
            if edit.has_focus() {
                let (up, down) = ui.input(|i| (i.key_pressed(egui::Key::ArrowUp), i.key_pressed(egui::Key::ArrowDown)));
                if (up || down) && !r.history.is_empty() {
                    let at = match (r.history_at, up) {
                        (None, true) => Some(0),
                        (None, false) => None,
                        (Some(i), true) => Some((i + 1).min(r.history.len() - 1)),
                        (Some(0), false) => None,
                        (Some(i), false) => Some(i - 1),
                    };
                    r.history_at = at;
                    r.command = at.map(|i| r.history[i].clone()).unwrap_or_default();
                }
            }
            if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                run = true;
            }
            egui::Grid::new("run-command").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                ui.label(tr("Input"));
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(selected, |ui| ui.radio_value(&mut r.input, RunInput::Selection, tr("Selection")));
                    ui.radio_value(&mut r.input, RunInput::Document, tr("Whole document"));
                });
                ui.end_row();
                ui.label(tr("Output"));
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(editable, |ui| ui.radio_value(&mut r.output, RunOutput::Replace, tr("Replace the text"))).response.on_hover_text(tr("One undo step; output over 64 MB opens in a new tab instead"));
                    ui.radio_value(&mut r.output, RunOutput::NewTab, tr("New tab"));
                    ui.radio_value(&mut r.output, RunOutput::ExitCode, tr("Exit code only"));
                });
                ui.end_row();
                ui.label(tr("Shell"));
                let choices: &[ShellChoice] = if cfg!(windows) { &[ShellChoice::System, ShellChoice::GitBash, ShellChoice::Wsl] } else { &[ShellChoice::System] };
                egui::ComboBox::from_id_salt("run-shell").selected_text(r.shell.label()).show_ui(ui, |ui| {
                    for &c in choices {
                        ui.selectable_value(&mut r.shell, c, c.label());
                    }
                });
                ui.end_row();
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(!r.command.trim().is_empty(), egui::Button::new(tr("Run"))).clicked() {
                    run = true;
                }
                ui.separator();
                ui.add(egui::TextEdit::singleline(&mut r.tool_name).hint_text(tr("Tool name")).desired_width(140.0));
                let can_save = !r.command.trim().is_empty() && !r.tool_name.trim().is_empty();
                if ui.add_enabled(can_save, egui::Button::new(tr("Save as external tool"))).on_hover_text(tr("Edit → External tools runs it; {file}, {dir}, {line}, {col} and {selection} are filled in")).clicked() {
                    save_tool = true;
                }
            });
        });
        if save_tool {
            let name = self.run_cmd.tool_name.trim().to_string();
            let command = self.run_cmd.command.trim().to_string();
            self.settings.tools.retain(|t| t.name != name);
            self.settings.tools.push(Tool { name, command });
            self.run_cmd.tool_name.clear();
            self.persist_state();
        }
        if run && !self.run_cmd.command.trim().is_empty() {
            self.run_cmd.open = false;
            self.start_command();
        } else if !open {
            self.run_cmd.open = false;
        }
    }

    /// Runs the window's command on a worker (the status bar shows it, with Cancel).
    pub(crate) fn start_command(&mut self) {
        let command = self.run_cmd.command.trim().to_string();
        self.run_cmd.history.retain(|h| *h != command);
        self.run_cmd.history.insert(0, command.clone());
        self.run_cmd.history.truncate(HISTORY_MAX);
        self.persist_state();
        let shell = match self.run_cmd.shell.shell() {
            Ok(s) => s,
            Err(e) => {
                self.error = Some(e);
                return;
            }
        };
        let Some(d) = self.docs.get(self.active).filter(|d| d.is_ready()) else { return };
        let range = match self.run_cmd.input {
            RunInput::Selection if d.has_selection() => {
                let (s, e) = d.selection();
                s..e
            }
            _ => 0..d.buf.len(),
        };
        let mut output = self.run_cmd.output;
        if output == RunOutput::Replace && !d.editable() {
            output = RunOutput::NewTab;
        }
        let (snap, doc, version) = (d.buf.snapshot(), d.id, d.version);
        let label = trf("Running {cmd}", &[("cmd", &short(&command))]);
        self.spawn_task(label, move |progress: &Progress, cancel| {
            let path = bigtext::decompress::temp_file("output.txt");
            let file = path.parent().map(std::fs::create_dir_all).transpose().and_then(|_| std::fs::File::create(&path));
            let mut file = match file {
                Ok(f) => std::io::BufWriter::new(f),
                Err(e) => return TaskOutcome::Failed(e.to_string()),
            };
            let input = range.clone();
            let feed: pipe::Feed = Box::new(move |w| write_utf8(&snap, input, w));
            match pipe::run(&shell, &command, feed, &mut file, cancel, Some(progress)) {
                Ok(ran) => {
                    drop(file);
                    TaskOutcome::Command(Box::new(CommandOutput { doc, version, range, output, command, path, code: ran.code, stderr: ran.stderr }))
                }
                Err(e) => {
                    drop(file);
                    bigtext::decompress::remove_temp(&path);
                    if e.kind() == std::io::ErrorKind::Interrupted {
                        TaskOutcome::Cancelled
                    } else {
                        TaskOutcome::Failed(trf("Could not run “{cmd}”: {e}", &[("cmd", &command), ("e", &e.to_string())]))
                    }
                }
            }
        });
    }

    pub(super) fn on_command_output(&mut self, o: CommandOutput) {
        let code = o.code.map(|c| c.to_string()).unwrap_or_else(|| "?".into());
        if o.code != Some(0) {
            bigtext::decompress::remove_temp(&o.path);
            let err = o.stderr.trim();
            let mut msg = trf("“{cmd}” exited with {code}; nothing was changed.", &[("cmd", &o.command), ("code", &code)]);
            if !err.is_empty() {
                msg.push_str("\n\n");
                msg.push_str(err);
            }
            self.error = Some(msg);
            return;
        }
        let size = std::fs::metadata(&o.path).map(|m| m.len()).unwrap_or(0);
        let mut output = o.output;
        if output == RunOutput::Replace && size > super::text_tools::TEXT_OP_MAX {
            output = RunOutput::NewTab;
        }
        match output {
            RunOutput::ExitCode => {
                bigtext::decompress::remove_temp(&o.path);
                self.notify(trf("“{cmd}” exited with {code} and wrote {size}.", &[("cmd", &short(&o.command)), ("code", &code), ("size", &human_bytes(size))]), None);
            }
            RunOutput::Replace => {
                let text = std::fs::read(&o.path).map(|b| String::from_utf8_lossy(&b).into_owned());
                bigtext::decompress::remove_temp(&o.path);
                let Some(d) = self.docs.iter_mut().find(|d| d.id == o.doc) else { return };
                match text {
                    _ if d.version != o.version || !d.editable() => self.error = Some(tr("The document changed while the command ran; nothing was changed.").into()),
                    Ok(text) => {
                        let bytes = d.buf.codec.encode(&text);
                        let start = o.range.start;
                        d.edit(o.range, &bytes, EditKind::Other);
                        d.select(start, start + bytes.len() as u64);
                        self.notify(trf("“{cmd}”: done (Ctrl+Z undoes it).", &[("cmd", &short(&o.command))]), None);
                    }
                    Err(e) => self.error = Some(e.to_string()),
                }
            }
            RunOutput::NewTab => {
                self.open_path(o.path);
                if let Some(d) = self.docs.get_mut(self.active) {
                    d.title = short(&o.command);
                    d.temporary = true;
                }
            }
        }
    }

    /// Edit → External tools: runs `tool` with the active file, line and selection filled in.
    pub(crate) fn run_tool(&mut self, tool: &Tool) {
        let Some(d) = self.docs.get(self.active) else { return };
        let caret = d.view.caret;
        let (line, _) = d.buf.line_of(caret);
        let col = d.buf.line_start(line).map(|(ls, _)| caret.saturating_sub(ls) + 1).unwrap_or(1);
        let selection = d.selection_text(64 * 1024).and_then(|r| r.ok()).unwrap_or_default();
        let path = d.unpacked_from.as_deref().or(d.path.as_deref());
        let command = expand(&tool.command, path, line + 1, col, &selection);
        let shell = match self.run_cmd.shell.shell() {
            Ok(s) => s,
            Err(e) => {
                self.error = Some(e);
                return;
            }
        };
        let name = tool.name.clone();
        self.spawn_task(name.clone(), move |_, cancel| {
            let mut out = Vec::new();
            match pipe::run(&shell, &command, Box::new(|_| Ok(())), &mut out, cancel, None) {
                Ok(r) if r.code == Some(0) => TaskOutcome::Done { message: trf("{tool}: done.", &[("tool", &name)]), open: None },
                Ok(r) => TaskOutcome::Failed(trf("{tool} exited with {code}.", &[("tool", &name), ("code", &r.code.map(|c| c.to_string()).unwrap_or_else(|| "?".into()))]) + "\n\n" + r.stderr.trim()),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => TaskOutcome::Cancelled,
                Err(e) => TaskOutcome::Failed(e.to_string()),
            }
        });
    }

    /// Edit → External tools ▸
    pub(super) fn tools_menu(&mut self, ui: &mut egui::Ui) {
        let tools = self.settings.tools.clone();
        ui.add_enabled_ui(!tools.is_empty(), |ui| {
            ui.menu_button(tr("External tools"), |ui| {
                for t in &tools {
                    if ui.button(&t.name).on_hover_text(&t.command).clicked() {
                        self.run_tool(t);
                        ui.close();
                    }
                }
                ui.separator();
                ui.menu_button(tr("Remove"), |ui| {
                    for t in &tools {
                        if ui.button(&t.name).clicked() {
                            self.settings.tools.retain(|x| x.name != t.name);
                            self.persist_state();
                            ui.close();
                        }
                    }
                });
            });
        })
        .response
        .on_disabled_hover_text(tr("Save a command as a tool in Edit → Run command…"));
    }
}

/// A command cut for a label.
fn short(cmd: &str) -> String {
    if cmd.chars().count() > 40 {
        cmd.chars().take(39).chain(['…']).collect()
    } else {
        cmd.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_get_the_file_and_line() {
        let p = std::path::Path::new("/logs/app.log");
        assert_eq!(expand("code -g \"{file}:{line}:{col}\" # {dir} {selection}", Some(p), 12, 5, "x"), "code -g \"/logs/app.log:12:5\" # /logs x");
        assert_eq!(ShellChoice::from_id(ShellChoice::Wsl.id()), ShellChoice::Wsl);
        assert_eq!(ShellChoice::from_id("nope"), ShellChoice::System);
    }
}
