//! Commands: everything the menus, the keyboard shortcuts and the command palette (Ctrl+Shift+P)
//! can do, in one list with stable ids, labels and default shortcuts. Shortcuts can be changed
//! in Help → Keyboard shortcuts and are kept in the session state.

use std::collections::HashMap;

use egui::{Key, KeyboardShortcut, Modifiers};

use bigtext::encoding::{Encoding, LineEnding};
use bigtext::file;
use bigtext::textops::Op;

use super::i18n::{tr, trf};
use super::{toggle_split, BigTextApp, GotoDialog, GotoMode};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Cmd {
    New,
    Open,
    Save,
    SaveAs,
    CloseTab,
    CloseOthers,
    PinTab,
    NextTab,
    PrevTab,
    NextTabBar,
    PrevTabBar,
    /// Ctrl+1 … Ctrl+9.
    GotoTab(u8),
    Exit,
    ExportLines,
    SaveSelection,
    SplitFile,
    Compare,
    CompareSideBySide,
    Properties,
    CompareSaved,
    CompareBackup,
    ApplyPatch,
    ColumnEditor,
    PasteHistory,
    RevealFile,
    OpenTerminal,
    OpenWithDefault,
    MergeLogs,
    JoinTables,
    CompareJson,
    ArrayToLines,
    LinesToArray,
    LogfmtTable,
    Undo,
    Redo,
    SelectAll,
    AddNextOccurrence,
    DuplicateLine,
    DeleteLine,
    CopyFileLine,
    ToggleBookmark,
    NextBookmark,
    PrevBookmark,
    ClearBookmarks,
    ReadOnly,
    Find,
    Replace,
    FindNext,
    FindPrev,
    FindAll,
    Filter,
    FilterWithin,
    FindInFiles,
    NextLevel,
    PrevLevel,
    NextDiff,
    OnlyIn,
    PrevDiff,
    NextResult,
    PrevResult,
    NavBack,
    NavForward,
    FollowId,
    IdPatterns,
    GotoLine,
    GotoOffset,
    GotoTime,
    Timeline,
    TimeGaps,
    Patterns,
    CheckJson,
    MatchBracket,
    WordWrap,
    HexView,
    TableView,
    JsonStructure,
    LogFields,
    JsonFields,
    CheckRows,
    JsonlTable,
    Follow,
    SplitView,
    Syntax,
    ShowWhitespace,
    DarkTheme,
    Highlights,
    LevelBar,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    PerfOverlay,
    Font,
    Palette,
    Shortcuts,
    ExportSettings,
    ImportSettings,
    Portable,
    UnpackDir,
    CheckUpdates,
    About,
    Text(Op),
}

const fn key(m: Modifiers, k: Key) -> Option<KeyboardShortcut> {
    Some(KeyboardShortcut::new(m, k))
}

const CTRL: Modifiers = Modifiers::COMMAND;
const CTRL_SHIFT: Modifiers = Modifiers { alt: false, ctrl: false, shift: true, mac_cmd: false, command: true };
const CTRL_SHIFT_RAW: Modifiers = Modifiers { alt: false, ctrl: true, shift: true, mac_cmd: false, command: false };
const CTRL_ALT: Modifiers = Modifiers { alt: true, ctrl: false, shift: false, mac_cmd: false, command: true };

impl Cmd {
    pub const ALL: [Cmd; 99] = [
        Cmd::New,
        Cmd::Open,
        Cmd::Save,
        Cmd::SaveAs,
        Cmd::CloseTab,
        Cmd::CloseOthers,
        Cmd::PinTab,
        Cmd::NextTab,
        Cmd::PrevTab,
        Cmd::NextTabBar,
        Cmd::PrevTabBar,
        Cmd::Exit,
        Cmd::ExportLines,
        Cmd::SaveSelection,
        Cmd::SplitFile,
        Cmd::Compare,
        Cmd::CompareSideBySide,
        Cmd::Properties,
        Cmd::CompareSaved,
        Cmd::CompareBackup,
        Cmd::ApplyPatch,
        Cmd::ColumnEditor,
        Cmd::PasteHistory,
        Cmd::RevealFile,
        Cmd::OpenTerminal,
        Cmd::OpenWithDefault,
        Cmd::MergeLogs,
        Cmd::JoinTables,
        Cmd::CompareJson,
        Cmd::ArrayToLines,
        Cmd::LinesToArray,
        Cmd::LogfmtTable,
        Cmd::Undo,
        Cmd::Redo,
        Cmd::SelectAll,
        Cmd::AddNextOccurrence,
        Cmd::DuplicateLine,
        Cmd::DeleteLine,
        Cmd::CopyFileLine,
        Cmd::ToggleBookmark,
        Cmd::NextBookmark,
        Cmd::PrevBookmark,
        Cmd::ClearBookmarks,
        Cmd::ReadOnly,
        Cmd::Find,
        Cmd::Replace,
        Cmd::FindNext,
        Cmd::FindPrev,
        Cmd::FindAll,
        Cmd::Filter,
        Cmd::FilterWithin,
        Cmd::FindInFiles,
        Cmd::NextLevel,
        Cmd::PrevLevel,
        Cmd::NextDiff,
        Cmd::OnlyIn,
        Cmd::PrevDiff,
        Cmd::NextResult,
        Cmd::PrevResult,
        Cmd::NavBack,
        Cmd::NavForward,
        Cmd::FollowId,
        Cmd::IdPatterns,
        Cmd::GotoLine,
        Cmd::GotoOffset,
        Cmd::GotoTime,
        Cmd::Timeline,
        Cmd::TimeGaps,
        Cmd::Patterns,
        Cmd::CheckJson,
        Cmd::MatchBracket,
        Cmd::WordWrap,
        Cmd::HexView,
        Cmd::TableView,
        Cmd::JsonStructure,
        Cmd::LogFields,
        Cmd::JsonFields,
        Cmd::CheckRows,
        Cmd::JsonlTable,
        Cmd::Follow,
        Cmd::SplitView,
        Cmd::Syntax,
        Cmd::ShowWhitespace,
        Cmd::DarkTheme,
        Cmd::Highlights,
        Cmd::LevelBar,
        Cmd::ZoomIn,
        Cmd::ZoomOut,
        Cmd::ZoomReset,
        Cmd::PerfOverlay,
        Cmd::Font,
        Cmd::Palette,
        Cmd::Shortcuts,
        Cmd::ExportSettings,
        Cmd::ImportSettings,
        Cmd::Portable,
        Cmd::UnpackDir,
        Cmd::CheckUpdates,
        Cmd::About,
    ];

    /// Every command, the text operations included.
    pub fn all() -> impl Iterator<Item = Cmd> {
        Cmd::ALL.into_iter().chain(Op::ALL.into_iter().map(Cmd::Text)).chain((1..=9).map(Cmd::GotoTab))
    }

    /// Stable name for the saved key bindings.
    pub fn id(self) -> String {
        let s = match self {
            Cmd::Text(op) => return format!("text.{op:?}").to_lowercase(),
            Cmd::GotoTab(n) => return format!("goto_tab.{n}"),
            other => format!("{other:?}"),
        };
        // CamelCase → snake_case.
        let mut out = String::new();
        for (i, c) in s.chars().enumerate() {
            if c.is_uppercase() && i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        }
        out
    }

    pub fn from_id(id: &str) -> Option<Cmd> {
        Cmd::all().find(|c| c.id() == id)
    }

    /// English label (translate with `tr`).
    pub fn label(self) -> &'static str {
        match self {
            Cmd::New => "New",
            Cmd::Open => "Open…",
            Cmd::Save => "Save",
            Cmd::SaveAs => "Save As…",
            Cmd::CloseTab => "Close tab",
            Cmd::CloseOthers => "Close other tabs",
            Cmd::PinTab => "Pin / unpin tab",
            Cmd::NextTab => "Next tab",
            Cmd::PrevTab => "Previous tab",
            Cmd::NextTabBar => "Next tab in the bar",
            Cmd::PrevTabBar => "Previous tab in the bar",
            Cmd::GotoTab(n) => ["Tab 1", "Tab 2", "Tab 3", "Tab 4", "Tab 5", "Tab 6", "Tab 7", "Tab 8", "Last tab"][(n.clamp(1, 9) - 1) as usize],
            Cmd::Exit => "Exit",
            Cmd::ExportLines => "Export lines…",
            Cmd::SaveSelection => "Save selection as…",
            Cmd::SplitFile => "Split file…",
            Cmd::Compare => "Compare with…",
            Cmd::CompareSideBySide => "Compare side by side",
            Cmd::Properties => "Properties…",
            Cmd::CompareSaved => "Compare with the saved file",
            Cmd::CompareBackup => "Compare with the backup (.bak)",
            Cmd::ApplyPatch => "Apply patch…",
            Cmd::ColumnEditor => "Column editor…",
            Cmd::PasteHistory => "Paste from history…",
            Cmd::RevealFile => "Show in folder",
            Cmd::OpenTerminal => "Open terminal here",
            Cmd::OpenWithDefault => "Open with the default app",
            Cmd::MergeLogs => "Merge logs by time…",
            Cmd::JoinTables => "Join tables…",
            Cmd::CompareJson => "Compare JSON with…",
            Cmd::ArrayToLines => "JSON array → JSON Lines…",
            Cmd::LinesToArray => "JSON Lines → JSON array…",
            Cmd::LogfmtTable => "Open logfmt as table",
            Cmd::Undo => "Undo",
            Cmd::Redo => "Redo",
            Cmd::SelectAll => "Select all",
            Cmd::AddNextOccurrence => "Add next occurrence (another cursor)",
            Cmd::DuplicateLine => "Duplicate line",
            Cmd::DeleteLine => "Delete line",
            Cmd::CopyFileLine => "Copy file:line",
            Cmd::ToggleBookmark => "Toggle bookmark",
            Cmd::NextBookmark => "Next bookmark",
            Cmd::PrevBookmark => "Previous bookmark",
            Cmd::ClearBookmarks => "Clear bookmarks",
            Cmd::ReadOnly => "Read-only",
            Cmd::Find => "Find…",
            Cmd::Replace => "Replace…",
            Cmd::FindNext => "Find next",
            Cmd::FindPrev => "Find previous",
            Cmd::FindAll => "Find all",
            Cmd::Filter => "Filter lines",
            Cmd::FilterWithin => "Narrow the filter by the search text",
            Cmd::FindInFiles => "Find in files…",
            Cmd::NextLevel => "Next error line",
            Cmd::PrevLevel => "Previous error line",
            Cmd::NextDiff => "Next difference",
            Cmd::OnlyIn => "Lines only in this file…",
            Cmd::PrevDiff => "Previous difference",
            Cmd::NextResult => "Next entry of the list",
            Cmd::PrevResult => "Previous entry of the list",
            Cmd::NavBack => "Back",
            Cmd::NavForward => "Forward",
            Cmd::FollowId => "Follow id at caret",
            Cmd::IdPatterns => "Id patterns…",
            Cmd::GotoLine => "Go to line…",
            Cmd::GotoOffset => "Go to byte offset…",
            Cmd::GotoTime => "Go to time…",
            Cmd::Timeline => "Time histogram",
            Cmd::TimeGaps => "Find time gaps…",
            Cmd::Patterns => "Message patterns…",
            Cmd::CheckJson => "Check JSON",
            Cmd::MatchBracket => "Go to matching bracket",
            Cmd::WordWrap => "Word wrap",
            Cmd::HexView => "Hex view",
            Cmd::TableView => "Table view (CSV / TSV)",
            Cmd::JsonStructure => "Structure (JSON / XML)",
            Cmd::LogFields => "Log fields",
            Cmd::JsonFields => "JSON fields…",
            Cmd::CheckRows => "Check table rows…",
            Cmd::JsonlTable => "Open JSON Lines as table",
            Cmd::Follow => "Follow (tail) file changes",
            Cmd::SplitView => "Split view",
            Cmd::Syntax => "Syntax highlighting",
            Cmd::ShowWhitespace => "Show whitespace and line breaks",
            Cmd::DarkTheme => "Dark theme",
            Cmd::Highlights => "Highlights…",
            Cmd::LevelBar => "Log level bar",
            Cmd::ZoomIn => "Zoom in",
            Cmd::ZoomOut => "Zoom out",
            Cmd::ZoomReset => "Reset zoom",
            Cmd::PerfOverlay => "Performance overlay",
            Cmd::Font => "Font…",
            Cmd::Palette => "Command palette…",
            Cmd::Shortcuts => "Keyboard shortcuts",
            Cmd::ExportSettings => "Export settings…",
            Cmd::ImportSettings => "Import settings…",
            Cmd::Portable => "Portable mode…",
            Cmd::CheckUpdates => "Check for updates…",
            Cmd::About => "About BigText",
            Cmd::UnpackDir => "Unpacked copies folder…",
            Cmd::Text(op) => op.name(),
        }
    }

    /// Menu the command lives in (shown in the palette).
    pub fn group(self) -> &'static str {
        match self {
            Cmd::New | Cmd::Open | Cmd::Save | Cmd::SaveAs | Cmd::CloseTab | Cmd::CloseOthers | Cmd::PinTab | Cmd::NextTab | Cmd::PrevTab | Cmd::Exit | Cmd::ExportLines | Cmd::SaveSelection | Cmd::SplitFile | Cmd::Compare | Cmd::CompareSaved | Cmd::CompareBackup | Cmd::ApplyPatch | Cmd::Properties | Cmd::RevealFile | Cmd::OpenTerminal | Cmd::OpenWithDefault | Cmd::MergeLogs | Cmd::JoinTables | Cmd::CompareJson | Cmd::ArrayToLines | Cmd::LinesToArray | Cmd::LogfmtTable => "File",
            Cmd::Undo
            | Cmd::Redo
            | Cmd::SelectAll
            | Cmd::AddNextOccurrence
            | Cmd::DuplicateLine
            | Cmd::DeleteLine
            | Cmd::CopyFileLine
            | Cmd::ToggleBookmark
            | Cmd::NextBookmark
            | Cmd::PrevBookmark
            | Cmd::ClearBookmarks
            | Cmd::ColumnEditor
            | Cmd::PasteHistory
            | Cmd::ReadOnly
            | Cmd::Text(_) => "Edit",
            Cmd::Find | Cmd::Replace | Cmd::FindNext | Cmd::FindPrev | Cmd::FindAll | Cmd::Filter | Cmd::FilterWithin | Cmd::FindInFiles | Cmd::NextLevel | Cmd::PrevLevel | Cmd::FollowId | Cmd::IdPatterns | Cmd::NavBack | Cmd::NavForward | Cmd::NextResult | Cmd::PrevResult | Cmd::NextDiff | Cmd::PrevDiff | Cmd::OnlyIn | Cmd::GotoLine | Cmd::GotoOffset | Cmd::GotoTime | Cmd::Timeline | Cmd::TimeGaps | Cmd::Patterns | Cmd::CheckJson | Cmd::MatchBracket => "Search",
            Cmd::Palette | Cmd::Shortcuts | Cmd::ExportSettings | Cmd::ImportSettings | Cmd::Portable | Cmd::UnpackDir | Cmd::CheckUpdates | Cmd::About => "Help",
            _ => "View",
        }
    }

    pub fn default_key(self) -> Option<KeyboardShortcut> {
        match self {
            Cmd::New => key(CTRL, Key::N),
            Cmd::Open => key(CTRL, Key::O),
            Cmd::Save => key(CTRL, Key::S),
            Cmd::SaveAs => key(CTRL_SHIFT, Key::S),
            Cmd::CloseTab => key(CTRL, Key::W),
            Cmd::NextTab => key(Modifiers::CTRL, Key::Tab),
            Cmd::PrevTab => key(CTRL_SHIFT_RAW, Key::Tab),
            Cmd::Undo => key(CTRL, Key::Z),
            Cmd::Redo => key(CTRL, Key::Y),
            Cmd::SelectAll => key(CTRL, Key::A),
            Cmd::AddNextOccurrence => key(CTRL, Key::D),
            Cmd::DuplicateLine => key(CTRL_SHIFT, Key::D),
            Cmd::DeleteLine => key(CTRL_SHIFT, Key::K),
            Cmd::CopyFileLine => key(CTRL_SHIFT, Key::C),
            Cmd::ToggleBookmark => key(CTRL, Key::F2),
            Cmd::NextBookmark => key(Modifiers::NONE, Key::F2),
            Cmd::PrevBookmark => key(Modifiers::SHIFT, Key::F2),
            Cmd::Find => key(CTRL, Key::F),
            Cmd::Replace => key(CTRL, Key::H),
            Cmd::FindNext => key(Modifiers::NONE, Key::F3),
            Cmd::FindPrev => key(Modifiers::SHIFT, Key::F3),
            Cmd::Filter => key(CTRL_SHIFT, Key::F),
            Cmd::FindInFiles => key(CTRL_ALT, Key::F),
            Cmd::NextLevel => key(Modifiers::NONE, Key::F8),
            Cmd::PrevLevel => key(Modifiers::SHIFT, Key::F8),
            Cmd::FollowId => key(CTRL_SHIFT, Key::I),
            Cmd::Properties => key(CTRL, Key::I),
            Cmd::PasteHistory => key(CTRL_SHIFT, Key::V),
            Cmd::NextDiff => key(Modifiers::NONE, Key::F7),
            Cmd::PrevDiff => key(Modifiers::SHIFT, Key::F7),
            Cmd::NextResult => key(Modifiers::NONE, Key::F4),
            Cmd::PrevResult => key(Modifiers::SHIFT, Key::F4),
            Cmd::NextTabBar => key(CTRL, Key::PageDown),
            Cmd::PrevTabBar => key(CTRL, Key::PageUp),
            Cmd::GotoTab(n) => key(CTRL, [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9][(n.clamp(1, 9) - 1) as usize]),
            Cmd::NavBack => key(Modifiers::ALT, Key::ArrowLeft),
            Cmd::NavForward => key(Modifiers::ALT, Key::ArrowRight),
            Cmd::GotoLine => key(CTRL, Key::G),
            Cmd::GotoOffset => key(CTRL_SHIFT, Key::G),
            Cmd::GotoTime => key(CTRL_SHIFT, Key::T),
            Cmd::Timeline => key(CTRL_SHIFT, Key::H),
            Cmd::MatchBracket => key(CTRL, Key::CloseBracket),
            Cmd::WordWrap => key(Modifiers::ALT, Key::Z),
            Cmd::TableView => key(CTRL, Key::T),
            Cmd::JsonStructure => key(CTRL_SHIFT, Key::J),
            Cmd::LogFields => key(CTRL_SHIFT, Key::L),
            Cmd::SplitView => key(CTRL, Key::Backslash),
            Cmd::ZoomIn => key(CTRL, Key::Equals),
            Cmd::ZoomOut => key(CTRL, Key::Minus),
            Cmd::ZoomReset => key(CTRL, Key::Num0),
            Cmd::PerfOverlay => key(Modifiers::NONE, Key::F12),
            Cmd::Palette => key(CTRL_SHIFT, Key::P),
            _ => None,
        }
    }

    /// Editing keys the text view handles itself; they are listed but cannot be rebound.
    pub fn fixed_key(self) -> bool {
        matches!(self, Cmd::Undo | Cmd::Redo | Cmd::SelectAll | Cmd::DeleteLine)
    }
}

/// Key bindings: the defaults with the user's changes on top.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Keymap {
    /// Changed bindings; `None` removes the default.
    pub changed: HashMap<Cmd, Option<KeyboardShortcut>>,
}

impl Keymap {
    pub fn get(&self, c: Cmd) -> Option<KeyboardShortcut> {
        if c.fixed_key() {
            return c.default_key();
        }
        match self.changed.get(&c) {
            Some(k) => *k,
            None => c.default_key(),
        }
    }

    /// The bindings in matching order: more modifiers first, because egui matches a shortcut
    /// even when extra Shift is held.
    pub fn bound(&self) -> Vec<(Cmd, KeyboardShortcut)> {
        let mut v: Vec<(Cmd, KeyboardShortcut)> = Cmd::all().filter(|c| !c.fixed_key()).filter_map(|c| Some((c, self.get(c)?))).collect();
        let n = |m: Modifiers| m.shift as u8 + m.alt as u8 + (m.ctrl || m.command) as u8;
        v.sort_by_key(|(_, k)| std::cmp::Reverse(n(k.modifiers)));
        v
    }

    /// The command that `k` is bound to, if any.
    pub fn owner(&self, k: KeyboardShortcut) -> Option<Cmd> {
        Cmd::all().find(|&c| self.get(c) == Some(k))
    }
}

pub fn key_text(k: KeyboardShortcut) -> String {
    k.format(&egui::ModifierNames::NAMES, cfg!(target_os = "macos"))
}

/// Serialised binding: "ctrl+shift+F" style, as egui names keys. "ctrl" is the command key
/// (Ctrl, or Cmd on a Mac); "rawctrl" is the Control key itself (Ctrl+Tab).
pub fn key_to_string(k: Option<KeyboardShortcut>) -> String {
    let Some(k) = k else { return "none".into() };
    let m = k.modifiers;
    let mut s = String::new();
    for (on, name) in [(m.command, "ctrl+"), (m.ctrl && !m.command, "rawctrl+"), (m.alt, "alt+"), (m.shift, "shift+")] {
        if on {
            s.push_str(name);
        }
    }
    s.push_str(k.logical_key.name());
    s
}

pub fn key_from_string(s: &str) -> Option<Option<KeyboardShortcut>> {
    if s == "none" {
        return Some(None);
    }
    let mut m = Modifiers::NONE;
    let mut rest = s;
    loop {
        if let Some(r) = rest.strip_prefix("ctrl+") {
            m.command = true;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("rawctrl+") {
            m.ctrl = true;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("alt+") {
            m.alt = true;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("shift+") {
            m.shift = true;
            rest = r;
        } else {
            break;
        }
    }
    Some(Some(KeyboardShortcut::new(m, Key::from_name(rest)?)))
}

/// The command palette's state.
#[derive(Default)]
pub struct Palette {
    pub query: String,
    pub selected: usize,
    focus: bool,
}

/// Help → Keyboard shortcuts: which command waits for a new key.
#[derive(Default)]
pub struct ShortcutsUi {
    pub open: bool,
    pub capturing: Option<Cmd>,
    pub filter: String,
    pub conflict: Option<String>,
}

/// Keys that are not commands, for the shortcuts window.
const OTHER_KEYS: &[(&str, &str)] = &[
    ("Ctrl+←/→, Ctrl+Backspace/Delete", "Word movement / delete word"),
    ("Home / End, Ctrl+Home / Ctrl+End", "Line start (smart) / end, document start / end"),
    ("Ctrl+wheel", "Zoom"),
    ("Double / triple click", "Select word / line"),
    ("Alt+drag", "Column (block) selection"),
    ("Alt+click", "Add a cursor"),
    ("Esc", "Back to one cursor"),
    ("Up / Down in the find field", "Search history"),
];

/// True if every word of `query` occurs in `text` (case-insensitive).
fn matches(query: &str, text: &str) -> bool {
    let t = text.to_lowercase();
    query.to_lowercase().split_whitespace().all(|w| t.contains(w))
}

impl BigTextApp {
    pub(crate) fn run(&mut self, ctx: &egui::Context, cmd: Cmd) {
        fn doc(app: &mut BigTextApp) -> Option<&mut super::document::Document> {
            let i = app.active;
            app.docs.get_mut(i)
        }
        let active = self.active;
        match cmd {
            Cmd::New => self.new_doc(),
            Cmd::Open => self.open_dialog(),
            Cmd::Save => self.save(false),
            Cmd::SaveAs => self.save(true),
            Cmd::CloseTab => {
                if let Some(id) = self.docs.get(active).map(|d| d.id) {
                    self.close_tab(id, false);
                }
            }
            Cmd::NextTab => self.switch_tab(true),
            Cmd::PrevTab => self.switch_tab(false),
            Cmd::CloseOthers => {
                if let Some(id) = self.docs.get(active).map(|d| d.id) {
                    self.close_others(id);
                }
            }
            Cmd::PinTab => {
                if let Some(id) = self.docs.get(active).map(|d| d.id) {
                    self.toggle_pin(id);
                }
            }
            Cmd::Exit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Cmd::ExportLines => self.open_export_lines(),
            Cmd::SaveSelection => self.save_selection_as(),
            Cmd::SplitFile => self.open_split(),
            Cmd::Compare => self.compare_dialog(),
            Cmd::CompareSaved => self.start_diff_with(super::extras::DiffOther::Saved),
            Cmd::ApplyPatch => self.apply_patch_dialog(),
            Cmd::ColumnEditor => self.open_column_editor(),
            Cmd::PasteHistory => self.open_paste_history(),
            Cmd::RevealFile | Cmd::OpenTerminal | Cmd::OpenWithDefault => {
                let action = match cmd {
                    Cmd::RevealFile => super::shell::ShellAction::Reveal,
                    Cmd::OpenTerminal => super::shell::ShellAction::Terminal,
                    _ => super::shell::ShellAction::Default,
                };
                if let Some(id) = self.docs.get(self.active).map(|d| d.id) {
                    self.shell_doc(id, action);
                }
            }
            Cmd::Properties => self.open_properties(),
            Cmd::CompareBackup => {
                if let Some(p) = self.docs.get(self.active).and_then(|d| d.path.as_deref()) {
                    let bak = bigtext::save::backup_path_for(p);
                    self.start_diff_with(super::extras::DiffOther::Backup(bak));
                }
            }
            Cmd::MergeLogs => self.open_merge(),
            Cmd::JoinTables => self.open_join(),
            Cmd::CompareJson => self.compare_json_dialog(),
            Cmd::ArrayToLines => self.array_to_lines(),
            Cmd::LinesToArray => self.lines_to_array(),
            Cmd::LogfmtTable => self.open_logfmt_table(),
            Cmd::Undo => doc(self).into_iter().for_each(|d| d.undo()),
            Cmd::Redo => doc(self).into_iter().for_each(|d| d.redo()),
            Cmd::SelectAll => {
                if let Some(d) = doc(self) {
                    d.view.anchor = d.min_caret();
                    d.view.caret = d.buf.len();
                }
            }
            Cmd::AddNextOccurrence => self.add_next_occurrence(),
            Cmd::DuplicateLine => doc(self).into_iter().for_each(|d| d.duplicate_line()),
            Cmd::DeleteLine => doc(self).into_iter().for_each(|d| d.delete_line()),
            Cmd::CopyFileLine => self.copy_file_line(ctx),
            Cmd::ToggleBookmark => doc(self).into_iter().for_each(|d| d.toggle_bookmark()),
            Cmd::NextBookmark => doc(self).into_iter().for_each(|d| d.goto_bookmark(true)),
            Cmd::PrevBookmark => doc(self).into_iter().for_each(|d| d.goto_bookmark(false)),
            Cmd::ClearBookmarks => doc(self).into_iter().for_each(|d| d.bookmarks.clear()),
            Cmd::ReadOnly => doc(self).into_iter().for_each(|d| d.read_only = !d.read_only),
            Cmd::Find => self.open_search(false),
            Cmd::Replace => self.open_search(true),
            Cmd::FindNext => self.find_next(true),
            Cmd::FindPrev => self.find_next(false),
            Cmd::FindAll => {
                self.open_search(false);
                self.find_all();
            }
            Cmd::Filter => {
                self.open_search(false);
                self.start_filter();
            }
            Cmd::FilterWithin => self.filter_within(),
            Cmd::FindInFiles => self.open_find_in_files(),
            Cmd::NextLevel => self.next_level(true),
            Cmd::PrevLevel => self.next_level(false),
            Cmd::NextDiff => self.step_diff(true),
            Cmd::OnlyIn => self.open_only_in(),
            Cmd::PrevDiff => self.step_diff(false),
            Cmd::NextResult => self.step_result(true),
            Cmd::PrevResult => self.step_result(false),
            Cmd::NextTabBar => self.step_tab_bar(true),
            Cmd::PrevTabBar => self.step_tab_bar(false),
            Cmd::GotoTab(n) => self.goto_tab(n),
            Cmd::NavBack => self.nav_back(),
            Cmd::NavForward => self.nav_forward(),
            Cmd::FollowId => self.follow_id_at_caret(),
            Cmd::IdPatterns => self.open_id_patterns(),
            Cmd::GotoLine => self.goto = GotoDialog::new(GotoMode::Line),
            Cmd::GotoOffset => self.goto = GotoDialog::new(GotoMode::Offset),
            Cmd::GotoTime => self.goto = GotoDialog::new(GotoMode::Time),
            Cmd::MatchBracket => self.goto_matching_bracket(),
            Cmd::TimeGaps => self.open_gaps(),
            Cmd::Patterns => self.open_patterns(),
            Cmd::CheckJson => self.check_json(None),
            Cmd::Timeline => {
                if self.is_on(Cmd::Timeline) {
                    self.timeline.open = false;
                } else {
                    self.open_timeline();
                }
            }
            Cmd::WordWrap => doc(self).into_iter().for_each(|d| {
                d.view.wrap = !d.view.wrap;
                d.view.scroll_to_caret = true;
            }),
            Cmd::HexView => doc(self).into_iter().for_each(|d| {
                d.hex = !d.hex;
                d.view.scroll_to_caret = true;
            }),
            Cmd::TableView => doc(self).into_iter().for_each(|d| {
                d.table = !d.table;
                d.view.scroll_to_caret = true;
            }),
            Cmd::JsonStructure => self.toggle_json_panel(),
            Cmd::LogFields => self.fields.open = !self.fields.open,
            Cmd::JsonFields => self.start_json_fields(),
            Cmd::CheckRows => self.check_rows(),
            Cmd::JsonlTable => self.open_jsonl_table(),
            Cmd::Follow => doc(self).into_iter().for_each(|d| d.follow = !d.follow),
            Cmd::SplitView => doc(self).into_iter().for_each(toggle_split),
            Cmd::CompareSideBySide => {
                self.side_by_side = !self.side_by_side;
                if self.side_by_side && self.diff.is_none() {
                    self.notify(tr("Compare the file with another first (File → Compare with…).").into(), None);
                }
            }
            Cmd::Syntax => self.settings.syntax = !self.settings.syntax,
            Cmd::ShowWhitespace => {
                self.settings.show_ws = !self.settings.show_ws;
                self.persist_state();
            }
            Cmd::DarkTheme => {
                self.settings.dark = !self.settings.dark;
                ctx.set_theme(if self.settings.dark { egui::Theme::Dark } else { egui::Theme::Light });
            }
            Cmd::Highlights => self.marks.open = true,
            Cmd::LevelBar => {
                self.settings.level_bar = !self.settings.level_bar;
                self.persist_state();
            }
            Cmd::ZoomIn => self.settings.font_size = (self.settings.font_size + 1.0).min(72.0),
            Cmd::ZoomOut => self.settings.font_size = (self.settings.font_size - 1.0).max(6.0),
            Cmd::ZoomReset => self.settings.font_size = 14.0,
            Cmd::PerfOverlay => self.settings.show_perf = !self.settings.show_perf,
            Cmd::Font => self.fonts.open = true,
            Cmd::Palette => self.palette = Some(Palette { focus: true, ..Default::default() }),
            Cmd::Shortcuts => self.shortcuts_ui.open = true,
            Cmd::ExportSettings => self.export_settings(),
            Cmd::ImportSettings => self.import_settings(ctx),
            Cmd::Portable => self.portable.open = true,
            Cmd::CheckUpdates => self.check_for_updates(true),
            Cmd::About => self.show_about = true,
            Cmd::UnpackDir => self.unpack_dir_open = true,
            Cmd::Text(op) => self.text_op(op),
        }
    }

    /// Whether a toggle command is on (for menu check marks).
    fn is_on(&self, cmd: Cmd) -> bool {
        let d = self.docs.get(self.active);
        match cmd {
            Cmd::ReadOnly => d.is_some_and(|d| d.read_only),
            Cmd::WordWrap => d.is_some_and(|d| d.view.wrap),
            Cmd::HexView => d.is_some_and(|d| d.hex),
            Cmd::TableView => d.is_some_and(|d| d.table),
            Cmd::JsonStructure => self.json_open,
            Cmd::LogFields => self.fields.open,
            Cmd::Follow => d.is_some_and(|d| d.follow),
            Cmd::SplitView => d.is_some_and(|d| d.split.is_some()),
            Cmd::CompareSideBySide => self.side_by_side,
            Cmd::Syntax => self.settings.syntax,
            Cmd::ShowWhitespace => self.settings.show_ws,
            Cmd::DarkTheme => self.settings.dark,
            Cmd::PerfOverlay => self.settings.show_perf,
            Cmd::LevelBar => self.settings.level_bar,
            Cmd::Timeline => self.timeline.open && d.is_some_and(|d| d.timeline.is_some()),
            _ => false,
        }
    }

    fn is_toggle(cmd: Cmd) -> bool {
        matches!(cmd, Cmd::ReadOnly | Cmd::WordWrap | Cmd::HexView | Cmd::TableView | Cmd::JsonStructure | Cmd::LogFields | Cmd::Follow | Cmd::SplitView | Cmd::CompareSideBySide | Cmd::Syntax | Cmd::ShowWhitespace | Cmd::DarkTheme | Cmd::PerfOverlay | Cmd::LevelBar | Cmd::Timeline)
    }

    fn enabled(&self, cmd: Cmd) -> bool {
        let d = self.docs.get(self.active);
        match cmd {
            Cmd::Undo => d.is_some_and(|d| !d.undo.is_empty()),
            Cmd::NavBack => !self.nav.back.is_empty(),
            Cmd::NextDiff | Cmd::PrevDiff => self.diff.is_some(),
            Cmd::CompareSaved => d.is_some_and(|d| d.path.is_some() && d.is_ready() && d.is_modified()),
            Cmd::CompareBackup => d.and_then(|d| d.path.as_deref()).is_some_and(|p| bigtext::save::backup_path_for(p).exists()),
            Cmd::ApplyPatch => d.is_some_and(|d| d.is_ready() && d.editable()),
            Cmd::ColumnEditor => d.is_some_and(|d| d.is_ready() && d.editable() && !d.view.wrap),
            Cmd::RevealFile | Cmd::OpenTerminal | Cmd::OpenWithDefault => d.is_some_and(|d| d.path.is_some()),
            Cmd::PasteHistory => !self.clips.items.is_empty(),
            Cmd::Properties => d.is_some_and(|d| d.is_ready()),
            Cmd::CheckRows => d.is_some_and(|d| d.table),
            Cmd::NavForward => !self.nav.forward.is_empty(),
            Cmd::Redo => d.is_some_and(|d| !d.redo.is_empty()),
            Cmd::ClearBookmarks => d.is_some_and(|d| !d.bookmarks.is_empty()),
            Cmd::Text(_) => d.is_some_and(|d| d.editable()),
            _ => true,
        }
    }

    /// Keyboard shortcuts of all commands (when no dialog is open).
    pub(super) fn shortcuts(&mut self, ctx: &egui::Context) {
        for (cmd, k) in self.keymap.bound() {
            // Alt+arrows move by words in a text field.
            if matches!(cmd, Cmd::NavBack | Cmd::NavForward) && ctx.memory(|m| m.focused()).is_some_and(|id| egui::TextEdit::load_state(ctx, id).is_some()) {
                continue;
            }
            // Shift+Alt+arrows grow a column block (egui would match them to Alt+arrows).
            if matches!(cmd, Cmd::NavBack | Cmd::NavForward) && ctx.input(|i| i.modifiers.shift) {
                continue;
            }
            let hit = ctx.input_mut(|i| i.consume_shortcut(&k) || (cmd == Cmd::ZoomIn && k == Cmd::ZoomIn.default_key().unwrap() && i.consume_shortcut(&KeyboardShortcut::new(CTRL, Key::Plus))));
            if hit {
                self.run(ctx, cmd);
            }
        }
    }

    /// A menu entry for `cmd` with its shortcut; returns the command when clicked.
    fn item(&self, ui: &mut egui::Ui, cmd: Cmd) -> Option<Cmd> {
        let label = tr(cmd.label());
        let enabled = self.enabled(cmd);
        let clicked = if BigTextApp::is_toggle(cmd) {
            let mut on = self.is_on(cmd);
            let text = match self.keymap.get(cmd) {
                Some(k) => format!("{label}\t{}", key_text(k)),
                None => label.to_string(),
            };
            ui.add_enabled(enabled, egui::Checkbox::new(&mut on, text)).clicked()
        } else {
            let mut b = egui::Button::new(label);
            if let Some(k) = self.keymap.get(cmd) {
                b = b.shortcut_text(key_text(k));
            }
            ui.add_enabled(enabled, b).clicked()
        };
        clicked.then_some(cmd)
    }

    pub(super) fn menu_bar(&mut self, ui: &mut egui::Ui) {
        let mut run = None;
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button(tr("File"), |ui| {
                for c in [Cmd::New, Cmd::Open, Cmd::Save, Cmd::SaveAs] {
                    run = run.or(self.item(ui, c));
                }
                ui.checkbox(&mut self.settings.backup_on_save, tr("Keep backup (.bak) on save"));
                if ui.checkbox(&mut self.recovery.enabled, tr("Keep a recovery journal of unsaved edits")).on_hover_text(tr("After a crash or a power cut, the next start offers to put the edits back")).changed() {
                    self.persist_state();
                }
                if let Some(d) = self.docs.get_mut(self.active) {
                    ui.menu_button(tr("Line endings on save"), |ui| {
                        ui.radio_value(&mut d.eol_on_save, None, tr("Keep as is"));
                        ui.radio_value(&mut d.eol_on_save, Some(LineEnding::Lf), tr("Convert to LF"));
                        ui.radio_value(&mut d.eol_on_save, Some(LineEnding::CrLf), tr("Convert to CRLF"));
                        ui.radio_value(&mut d.eol_on_save, Some(LineEnding::Cr), tr("Convert to CR"));
                    });
                }
                ui.menu_button(tr("Save with encoding"), |ui| {
                    let choices = [
                        (Encoding::Utf8, false, "UTF-8"),
                        (Encoding::Utf8, true, "UTF-8 with BOM"),
                        (Encoding::Utf16Le, true, "UTF-16 LE"),
                        (Encoding::Utf16Be, true, "UTF-16 BE"),
                        (Encoding::Windows1251, false, "Windows-1251"),
                        (Encoding::Windows1252, false, "Windows-1252"),
                        (Encoding::Latin1, false, "ISO-8859-1"),
                    ];
                    for (enc, bom, label) in choices {
                        if ui.button(tr(label)).clicked() {
                            self.save_with(true, Some((enc, bom)));
                            ui.close();
                        }
                    }
                });
                ui.menu_button(tr("Reopen with encoding"), |ui| {
                    for enc in Encoding::ALL {
                        if ui.button(enc.name()).clicked() {
                            self.reopen_with(enc);
                        }
                    }
                });
                ui.menu_button(tr("Sessions"), |ui| self.sessions_menu(ui));
                let recent = self.recent.clone();
                ui.add_enabled_ui(!recent.is_empty(), |ui| {
                    ui.menu_button(tr("Open recent"), |ui| {
                        for p in recent {
                            if ui.button(file::display_path(&p)).clicked() {
                                self.open_path(p);
                            }
                        }
                        ui.separator();
                        if ui.button(tr("Clear list")).clicked() {
                            self.recent.clear();
                            self.persist_state();
                        }
                    });
                });
                ui.separator();
                for c in [Cmd::ExportLines, Cmd::SaveSelection, Cmd::SplitFile, Cmd::Compare] {
                    run = run.or(self.item(ui, c));
                }
                // The other open files, to compare with in one click.
                let others: Vec<(String, std::path::PathBuf)> = self
                    .docs
                    .iter()
                    .enumerate()
                    .filter(|(i, d)| *i != self.active && d.is_ready() && !d.temporary)
                    .filter_map(|(_, d)| Some((d.title.clone(), d.path.clone()?)))
                    .collect();
                let can = self.docs.get(self.active).is_some_and(|d| d.is_ready());
                ui.add_enabled_ui(can && !others.is_empty(), |ui| {
                    ui.menu_button(tr("Compare with tab"), |ui| {
                        for (title, path) in others {
                            if ui.button(title).clicked() {
                                self.start_diff(path);
                                ui.close();
                            }
                        }
                    });
                });
                for c in [Cmd::CompareSaved, Cmd::CompareBackup, Cmd::ApplyPatch, Cmd::Properties] {
                    run = run.or(self.item(ui, c));
                }
                ui.separator();
                for c in [Cmd::RevealFile, Cmd::OpenTerminal, Cmd::OpenWithDefault] {
                    run = run.or(self.item(ui, c));
                }
                for c in [Cmd::CompareJson, Cmd::MergeLogs, Cmd::JoinTables, Cmd::ArrayToLines, Cmd::LinesToArray, Cmd::LogfmtTable] {
                    run = run.or(self.item(ui, c));
                }
                let table = self.docs.get(self.active).is_some_and(|d| d.table && d.is_ready() && d.table_state.dialect.is_some());
                ui.add_enabled_ui(table, |ui| {
                    ui.menu_button(tr("Export table as"), |ui| {
                        for format in bigtext::table_export::Format::ALL {
                            if ui.button(format!("{}…", tr(format.name()))).clicked() {
                                ui.close();
                                self.export_table(None, format);
                            }
                        }
                    });
                });
                ui.separator();
                for c in [Cmd::CloseTab, Cmd::CloseOthers, Cmd::PinTab, Cmd::Exit] {
                    run = run.or(self.item(ui, c));
                }
            });
            ui.menu_button(tr("Edit"), |ui| {
                for c in [Cmd::Undo, Cmd::Redo] {
                    run = run.or(self.item(ui, c));
                }
                ui.separator();
                for c in [Cmd::SelectAll, Cmd::AddNextOccurrence, Cmd::DuplicateLine, Cmd::DeleteLine, Cmd::CopyFileLine] {
                    run = run.or(self.item(ui, c));
                }
                self.text_menu(ui);
                run = run.or(self.item(ui, Cmd::ColumnEditor));
                run = run.or(self.item(ui, Cmd::PasteHistory));
                ui.separator();
                for c in [Cmd::ToggleBookmark, Cmd::NextBookmark, Cmd::PrevBookmark, Cmd::ClearBookmarks] {
                    run = run.or(self.item(ui, c));
                }
                ui.separator();
                run = run.or(self.item(ui, Cmd::ReadOnly));
            });
            ui.menu_button(tr("Search"), |ui| {
                for c in [Cmd::Find, Cmd::Replace, Cmd::FindNext, Cmd::FindPrev, Cmd::FindAll, Cmd::Filter, Cmd::FilterWithin, Cmd::NextResult, Cmd::PrevResult, Cmd::FindInFiles, Cmd::NextLevel, Cmd::PrevLevel, Cmd::FollowId, Cmd::IdPatterns] {
                    run = run.or(self.item(ui, c));
                }
                ui.separator();
                for c in [Cmd::NextDiff, Cmd::PrevDiff, Cmd::OnlyIn, Cmd::NavBack, Cmd::NavForward, Cmd::GotoLine, Cmd::GotoOffset, Cmd::GotoTime, Cmd::MatchBracket, Cmd::Timeline, Cmd::TimeGaps, Cmd::Patterns, Cmd::CheckJson] {
                    run = run.or(self.item(ui, c));
                }
            });
            ui.menu_button(tr("View"), |ui| {
                for c in [Cmd::WordWrap, Cmd::HexView, Cmd::TableView, Cmd::CheckRows, Cmd::JsonStructure, Cmd::JsonFields, Cmd::LogFields, Cmd::JsonlTable, Cmd::Follow, Cmd::SplitView, Cmd::CompareSideBySide] {
                    run = run.or(self.item(ui, c));
                }
                ui.separator();
                for c in [Cmd::Syntax, Cmd::ShowWhitespace] {
                    run = run.or(self.item(ui, c));
                }
                if let Some(d) = self.docs.get_mut(self.active) {
                    let auto = d.auto_lang();
                    ui.menu_button(tr("Highlight as"), |ui| {
                        if ui.radio(d.lang.is_none(), trf("Auto ({lang})", &[("lang", auto.name())])).clicked() {
                            d.lang = None;
                        }
                        for l in crate::view::highlight::Lang::ALL {
                            if ui.radio(d.lang == Some(l), l.name()).clicked() {
                                d.lang = Some(l);
                            }
                        }
                    });
                }
                for c in [Cmd::Highlights, Cmd::LevelBar] {
                    run = run.or(self.item(ui, c));
                }
                ui.separator();
                run = run.or(self.item(ui, Cmd::DarkTheme));
                self.language_menu(ui);
                run = run.or(self.item(ui, Cmd::Font));
                for c in [Cmd::ZoomIn, Cmd::ZoomOut, Cmd::ZoomReset, Cmd::PerfOverlay] {
                    run = run.or(self.item(ui, c));
                }
            });
            ui.menu_button(tr("Help"), |ui| {
                for c in [Cmd::Palette, Cmd::Shortcuts] {
                    run = run.or(self.item(ui, c));
                }
                ui.separator();
                for c in [Cmd::ExportSettings, Cmd::ImportSettings, Cmd::Portable, Cmd::UnpackDir] {
                    run = run.or(self.item(ui, c));
                }
                ui.separator();
                run = run.or(self.item(ui, Cmd::CheckUpdates));
                if ui.checkbox(&mut self.update.auto, tr("Check for updates automatically")).changed() {
                    self.persist_state();
                }
                ui.separator();
                run = run.or(self.item(ui, Cmd::About));
            });
        });
        if let Some(c) = run {
            let ctx = ui.ctx().clone();
            self.run(&ctx, c);
        }
    }

    /// The command palette: type to find any command, Enter runs it.
    pub(super) fn palette_window(&mut self, ctx: &egui::Context) {
        let Some(p) = &mut self.palette else { return };
        let found: Vec<Cmd> = Cmd::all().filter(|&c| matches(&p.query, &format!("{} {} {}", tr(c.label()), c.label(), tr(c.group())))).collect();
        p.selected = p.selected.min(found.len().saturating_sub(1));
        let (up, down, enter, esc) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        if up {
            p.selected = p.selected.saturating_sub(1);
        }
        if down && p.selected + 1 < found.len() {
            p.selected += 1;
        }
        let mut chosen = enter.then(|| found.get(p.selected).copied()).flatten();
        let keymap = &self.keymap;
        let enabled: Vec<bool> = found.iter().map(|&c| self.enabled(c)).collect();
        let p = self.palette.as_mut().unwrap();
        let close = esc;
        egui::Window::new("palette")
            .title_bar(false)
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 60.0))
            .fixed_size(egui::vec2(520.0, 0.0))
            .show(ctx, |ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut p.query).hint_text(tr("Type a command")).desired_width(f32::INFINITY));
                if p.focus {
                    r.request_focus();
                    p.focus = false;
                }
                if r.changed() {
                    p.selected = 0;
                }
                ui.separator();
                egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                    for (i, &c) in found.iter().enumerate() {
                        let text = format!("{}: {}", tr(c.group()), tr(c.label()));
                        let mut b = egui::Button::new(text).selected(i == p.selected).frame(i == p.selected);
                        if let Some(k) = keymap.get(c) {
                            b = b.shortcut_text(key_text(k));
                        }
                        let r = ui.add_enabled(enabled[i], b.min_size(egui::vec2(ui.available_width(), 0.0)));
                        if i == p.selected {
                            r.scroll_to_me(None);
                        }
                        if r.clicked() {
                            chosen = Some(c);
                        }
                    }
                    if found.is_empty() {
                        ui.weak(tr("No command matches."));
                    }
                });
            });
        if let Some(c) = chosen.filter(|&c| self.enabled(c)) {
            self.palette = None;
            self.run(ctx, c);
        } else if close {
            self.palette = None;
        }
    }

    /// Help → Keyboard shortcuts: every command with its key; click one to change it.
    pub(super) fn shortcuts_window(&mut self, ctx: &egui::Context) {
        if !self.shortcuts_ui.open {
            return;
        }
        // A key pressed while capturing becomes the new binding.
        if let Some(cmd) = self.shortcuts_ui.capturing {
            let pressed = ctx.input(|i| {
                i.events.iter().find_map(|e| match e {
                    egui::Event::Key { key, pressed: true, modifiers, .. } => Some((*key, *modifiers)),
                    _ => None,
                })
            });
            if let Some((k, m)) = pressed {
                ctx.input_mut(|i| i.events.clear());
                if k == Key::Escape && m.is_none() {
                    self.shortcuts_ui.capturing = None;
                } else {
                    let m = Modifiers { alt: m.alt, shift: m.shift, command: m.command || m.ctrl, ctrl: false, mac_cmd: false };
                    let new = KeyboardShortcut::new(m, k);
                    match self.keymap.owner(new).filter(|&o| o != cmd) {
                        Some(o) if o.fixed_key() => {
                            self.shortcuts_ui.conflict = Some(trf("{key} is used by “{cmd}”, which cannot be changed.", &[("key", &key_text(new)), ("cmd", tr(o.label()))]));
                        }
                        other => {
                            if let Some(o) = other {
                                self.keymap.changed.insert(o, None);
                                self.shortcuts_ui.conflict = Some(trf("{key} was moved from “{cmd}”.", &[("key", &key_text(new)), ("cmd", tr(o.label()))]));
                            } else {
                                self.shortcuts_ui.conflict = None;
                            }
                            self.keymap.changed.insert(cmd, Some(new));
                            self.shortcuts_ui.capturing = None;
                            self.persist_state();
                        }
                    }
                }
            }
        }
        let mut open = true;
        let mut reset_all = false;
        egui::Window::new(tr("Keyboard shortcuts")).open(&mut open).default_size([560.0, 520.0]).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.shortcuts_ui.filter).hint_text(tr("Filter")).desired_width(200.0));
                if ui.button(tr("Reset all")).clicked() {
                    reset_all = true;
                }
            });
            ui.weak(tr("Click a shortcut, then press the new keys (Esc cancels). Editing keys of the text are fixed."));
            if let Some(c) = &self.shortcuts_ui.conflict {
                ui.colored_label(egui::Color32::from_rgb(220, 150, 60), c);
            }
            ui.separator();
            egui::ScrollArea::vertical().show(ui, |ui| {
                egui::Grid::new("keys").num_columns(3).striped(true).show(ui, |ui| {
                    let list: Vec<Cmd> = Cmd::all().filter(|&c| matches(&self.shortcuts_ui.filter, &format!("{} {}", tr(c.label()), c.label()))).collect();
                    for c in list {
                        ui.label(format!("{}: {}", tr(c.group()), tr(c.label())));
                        let current = self.keymap.get(c).map(key_text).unwrap_or_else(|| "—".into());
                        if c.fixed_key() {
                            ui.monospace(current);
                            ui.label("");
                        } else {
                            let capturing = self.shortcuts_ui.capturing == Some(c);
                            let text = if capturing { tr("press keys…").to_string() } else { current };
                            if ui.add(egui::Button::new(egui::RichText::new(text).monospace()).selected(capturing)).clicked() {
                                self.shortcuts_ui.capturing = Some(c);
                            }
                            ui.horizontal(|ui| {
                                if self.keymap.get(c).is_some() && ui.small_button("×").on_hover_text(tr("Remove the shortcut")).clicked() {
                                    self.keymap.changed.insert(c, None);
                                    self.persist_state();
                                }
                                if self.keymap.changed.contains_key(&c) && ui.small_button("↺").on_hover_text(tr("Default")).clicked() {
                                    self.keymap.changed.remove(&c);
                                    self.persist_state();
                                }
                            });
                        }
                        ui.end_row();
                    }
                    for (k, v) in OTHER_KEYS {
                        ui.label(tr(v));
                        ui.monospace(*k);
                        ui.label("");
                        ui.end_row();
                    }
                });
            });
        });
        if reset_all {
            self.keymap.changed.clear();
            self.shortcuts_ui.conflict = None;
            self.persist_state();
        }
        if !open {
            self.shortcuts_ui = ShortcutsUi::default();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_round_trip() {
        let ids: Vec<String> = Cmd::all().map(Cmd::id).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len());
        assert_eq!(Cmd::GotoOffset.id(), "goto_offset");
        assert_eq!(Cmd::from_id("text.jsonpretty"), Some(Cmd::Text(Op::JsonPretty)));
    }

    #[test]
    fn default_keys_do_not_clash_and_serialise() {
        let mut seen = HashMap::new();
        for c in Cmd::all() {
            if let Some(k) = c.default_key() {
                assert!(seen.insert(k, c).is_none(), "{c:?} clashes with {:?}", seen[&k]);
                assert_eq!(key_from_string(&key_to_string(Some(k))), Some(Some(k)), "{c:?}");
            }
        }
        assert_eq!(key_from_string("none"), Some(None));
    }

    #[test]
    fn shift_variants_are_matched_first() {
        let b = Keymap::default().bound();
        let pos = |c| b.iter().position(|x| x.0 == c).unwrap();
        assert!(pos(Cmd::SaveAs) < pos(Cmd::Save));
        assert!(pos(Cmd::FindPrev) < pos(Cmd::FindNext));
        assert!(pos(Cmd::Filter) < pos(Cmd::Find));
    }
}
