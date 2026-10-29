//! Interface languages. The English text in the code is the key: [`tr`] returns its translation
//! for the chosen language, or the English text when there is none.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    En,
    Ru,
    Uz,
}

impl Lang {
    pub const ALL: [Lang; 3] = [Lang::En, Lang::Ru, Lang::Uz];

    /// Native name, shown in the language menu.
    pub fn name(self) -> &'static str {
        match self {
            Lang::En => "English",
            Lang::Ru => "Русский",
            Lang::Uz => "O'zbekcha",
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Ru => "ru",
            Lang::Uz => "uz",
        }
    }

    pub fn from_code(s: &str) -> Option<Lang> {
        Lang::ALL.into_iter().find(|l| l.code() == s)
    }
}

static LANG: AtomicU8 = AtomicU8::new(0);

pub fn set_lang(l: Lang) {
    LANG.store(l as u8, Ordering::Relaxed);
}

pub fn lang() -> Lang {
    match LANG.load(Ordering::Relaxed) {
        1 => Lang::Ru,
        2 => Lang::Uz,
        _ => Lang::En,
    }
}

fn table(l: Lang) -> Option<&'static HashMap<&'static str, &'static str>> {
    static RU: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    static UZ: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    let build = |col: usize| TEXTS.iter().map(|t| (t[0], t[col])).filter(|(_, v)| !v.is_empty()).collect();
    match l {
        Lang::En => None,
        Lang::Ru => Some(RU.get_or_init(|| build(1))),
        Lang::Uz => Some(UZ.get_or_init(|| build(2))),
    }
}

/// The interface text `s` in the current language.
pub fn tr(s: &'static str) -> &'static str {
    match table(lang()) {
        Some(t) => t.get(s).copied().unwrap_or(s),
        None => s,
    }
}

/// [`tr`] with `{name}` placeholders filled in.
pub fn trf(s: &'static str, args: &[(&str, &str)]) -> String {
    let mut out = tr(s).to_string();
    for (k, v) in args {
        out = out.replace(&format!("{{{k}}}"), v);
    }
    out
}

use super::i18n_texts::TEXTS;

impl super::BigTextApp {
    /// View → Language.
    pub(super) fn language_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button(format!("{} (Language)", tr("Language")), |ui| {
            for l in Lang::ALL {
                if ui.radio(lang() == l, l.name()).clicked() {
                    set_lang(l);
                    self.persist_state();
                    ui.close();
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::commands::Cmd;

    macro_rules! sources {
        ($($dir:literal / $file:literal),* $(,)?) => {
            &[$((concat!($dir, "/", $file), include_str!(concat!("../", $dir, "/", $file)))),*]
        };
    }

    /// The scanned files (path under `src`, content).
    const SOURCES: &[(&str, &str)] = sources![
        "app" / "mod.rs",
        "app" / "archive.rs",
        "app" / "column_chart.rs",
        "app" / "columns.rs",
        "app" / "commands.rs",
        "app" / "convert.rs",
        "app" / "csv_tools.rs",
        "app" / "derive_column.rs",
        "app" / "document.rs",
        "app" / "export.rs",
        "app" / "extras.rs",
        "app" / "fields_panel.rs",
        "app" / "filter_panel.rs",
        "app" / "find_in_files.rs",
        "app" / "follow_id.rs",
        "app" / "gaps_panel.rs",
        "app" / "fonts.rs",
        "app" / "join_tables.rs",
        "app" / "json_diff_panel.rs",
        "app" / "json_fields_window.rs",
        "app" / "json_panel.rs",
        "app" / "json_validate.rs",
        "app" / "keynav.rs",
        "app" / "levels.rs",
        "app" / "marks.rs",
        "app" / "match_bracket.rs",
        "app" / "match_values.rs",
        "app" / "merge_logs.rs",
        "app" / "multi_cursor.rs",
        "app" / "nav.rs",
        "app" / "only_in.rs",
        "app" / "ops.rs",
        "app" / "patterns_window.rs",
        "app" / "pivot.rs",
        "app" / "portable.rs",
        "app" / "problems_panel.rs",
        "app" / "saved_searches.rs",
        "app" / "search_panel.rs",
        "app" / "table_compare.rs",
        "app" / "sessions.rs",
        "app" / "tabs.rs",
        "app" / "tasks.rs",
        "app" / "text_tools.rs",
        "app" / "time_range.rs",
        "app" / "timeline.rs",
        "app" / "update.rs",
        "view" / "block.rs",
        "view" / "hex_view.rs",
        "view" / "table_view.rs",
        "view" / "text_view.rs",
    ];

    #[test]
    fn every_file_with_texts_is_scanned() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        for dir in ["app", "view"] {
            for e in std::fs::read_dir(src.join(dir)).unwrap().flatten() {
                let name = format!("{dir}/{}", e.file_name().to_string_lossy());
                let text = std::fs::read_to_string(e.path()).unwrap_or_default();
                let uses = !keys_in(&text).is_empty();
                assert!(!uses || name == "app/i18n.rs" || SOURCES.iter().any(|s| s.0 == name), "add {name} to SOURCES in i18n.rs");
            }
        }
    }

    /// Texts translated through a variable rather than a literal at the call.
    const DYNAMIC: &[&str] = &[
        "Count rows",
        "Sum",
        "Mean",
        "Minimum",
        "Maximum",
        "Each value",
        "By hour",
        "By day",
        "By month",
        "Tab 1",
        "Tab 2",
        "Tab 3",
        "Tab 4",
        "Tab 5",
        "Tab 6",
        "Tab 7",
        "Tab 8",
        "Last tab",
        "Comma",
        "Semicolon",
        "Tab",
        "Pipe",
        "Other",
        "Discard",
        "Replace on disk",
        "Copy",
        "Delete",
        "Rewrite",
        "Could not check for updates",
        "The update failed",
        "Saving",
        "Replacing",
        "Deleting columns",
        "Rewriting",
        "Go to line",
        "Go to byte offset",
        "Go to time",
        "UTF-8 with BOM",
        "GitHub",
        "Repository",
        "Telegram",
        "E-mail",
        "Word movement / delete word",
        "Line start (smart) / end, document start / end",
        "Zoom",
        "Select word / line",
        "Column (block) selection",
        "Add a cursor",
        "Back to one cursor",
        "Search history",
        "Language",
        "Errors",
        "Search matches",
        "Lines only",
        "contains",
        "does not contain",
        "is",
        "is not",
        ">",
        "<",
        "is empty",
        "is not empty",
        "JSON",
        "JSON Lines",
        "Markdown table",
        "Excel workbook (.xlsx)",
        "Pin tab",
        "Unpin tab",
        "Close",
        "Close tabs to the right",
        "Copy path",
    ];

    /// String literals in `span` (escapes resolved).
    fn literals(span: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut it = span.chars().peekable();
        let mut prev = ' ';
        while let Some(c) = it.next() {
            if c == '"' && prev != '\'' {
                let mut s = String::new();
                while let Some(c) = it.next() {
                    match c {
                        '"' => break,
                        '\\' => match it.next() {
                            Some('n') => s.push('\n'),
                            Some('t') => s.push('\t'),
                            Some(o) => s.push(o),
                            None => {}
                        },
                        _ => s.push(c),
                    }
                }
                out.push(s);
                prev = '"';
            } else {
                prev = c;
            }
        }
        out
    }

    /// Texts passed to `tr(…)` (every literal) and `trf(…)` / `tr_f(…)` (the first argument).
    fn keys_in(src: &str) -> Vec<String> {
        let b: Vec<char> = src.chars().collect();
        let mut keys = Vec::new();
        let mut i = 0;
        while i < b.len() {
            let name_len = ["tr_f(", "trf(", "tr("].iter().find(|n| b[i..].starts_with(&n.chars().collect::<Vec<_>>())).map(|n| n.len());
            let word_start = i == 0 || !(b[i - 1].is_alphanumeric() || b[i - 1] == '_');
            let (Some(n), true) = (name_len, word_start) else {
                i += 1;
                continue;
            };
            let first_only = n > 3;
            let mut j = i + n;
            let (mut depth, mut in_str, mut first_end) = (1, false, None);
            while j < b.len() && depth > 0 {
                let c = b[j];
                if in_str {
                    if c == '\\' {
                        j += 1;
                    } else if c == '"' {
                        in_str = false;
                    }
                } else if c == '"' && b[j - 1] != '\'' {
                    in_str = true;
                } else if c == '(' || c == '[' || c == '{' {
                    depth += 1;
                } else if c == ')' || c == ']' || c == '}' {
                    depth -= 1;
                } else if c == ',' && depth == 1 && first_end.is_none() {
                    first_end = Some(j);
                }
                j += 1;
            }
            let end = if first_only { first_end.unwrap_or(j - 1) } else { j - 1 };
            keys.extend(literals(&b[i + n..end].iter().collect::<String>()).into_iter().filter(|s| !s.is_empty()));
            i += n;
        }
        keys
    }

    fn placeholders(s: &str) -> Vec<String> {
        let mut v: Vec<String> = s.split('{').skip(1).filter_map(|p| p.split_once('}').map(|x| x.0.to_string())).collect();
        v.sort();
        v.dedup();
        v
    }

    #[test]
    fn every_text_is_translated() {
        let mut keys: Vec<String> = SOURCES.iter().flat_map(|s| keys_in(s.1)).collect();
        keys.extend(Cmd::all().flat_map(|c| [c.label(), c.group()]).map(String::from));
        keys.extend(DYNAMIC.iter().map(|s| s.to_string()));
        let known: HashMap<&str, &[&str; 3]> = TEXTS.iter().map(|t| (t[0], t)).collect();
        let mut missing: Vec<&String> = keys.iter().filter(|k| !known.contains_key(k.as_str())).collect();
        missing.sort();
        missing.dedup();
        assert!(missing.is_empty(), "texts without a translation: {missing:#?}");
        assert!(keys.len() > 300, "the scan found only {} texts", keys.len());
    }

    #[test]
    fn translations_are_complete_and_keep_placeholders() {
        let mut seen = std::collections::HashSet::new();
        for t in TEXTS {
            assert!(seen.insert(t[0]), "duplicate entry {:?}", t[0]);
            for (i, x) in t.iter().enumerate().skip(1) {
                assert!(!x.trim().is_empty(), "{:?}: empty translation {i}", t[0]);
                assert_eq!(placeholders(x), placeholders(t[0]), "{:?} → {x:?}", t[0]);
            }
        }
    }

    #[test]
    fn lookup_and_fill() {
        set_lang(Lang::Ru);
        assert_eq!(tr("File"), "Файл");
        assert_eq!(trf("{n} lines", &[("n", "5")]), "строк: 5");
        set_lang(Lang::Uz);
        assert_eq!(tr("Not in the table"), "Not in the table");
        set_lang(Lang::En);
        assert_eq!(tr("File"), "File");
    }
}
