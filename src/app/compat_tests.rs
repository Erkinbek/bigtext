//! Upgrade compatibility: what older 0.x releases wrote must still be read right by this one,
//! and its first save must keep it. No file has a version field (only the journal's header), so
//! these tests are the only guard. The fixtures in `tests/compat/` are byte for byte what each
//! release's writer produced for a rich session (`tests/compat/README.md` names the tag and the
//! code each came from); only releases whose writers changed have one.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use bigtext::encoding::{Codec, Encoding, NlByte};
use bigtext::file::{OpenMode, Source};
use bigtext::index::LineIndex;
use bigtext::loglevel::{Level, Levels};
use bigtext::search::Query;
use bigtext::settings_file::{query_from, saved_searches, SavedSearch};
use bigtext::text::{Buf, TextBuf};
use bigtext::update::Version;

use super::alerts::{self, AlertsUi};
use super::commands::{key_from_string, key_to_string, Cmd};
use super::extras::{self, FileOpts, Persisted, TabState};
use super::i18n;
use super::log_sources::{HowMuch, SourcesUi, Tab};
use super::marks::Mark;
use super::recovery::{self, Journal};
use super::run_command::{ShellChoice, Tool};
use super::sessions::{self, SavedFilter};
use super::status_bar::StatusFields;
use super::theme::{CodeTheme, Theme};
use super::update::UpdateUi;
use crate::view::highlight::Lang;
use crate::view::table_view::{fnv1a, TableLayout};

macro_rules! fixture {
    ($path:literal) => {
        include_str!(concat!("../../tests/compat/", $path))
    };
}

const STATE_0_1_0: &str = fixture!("state/v0.1.0.txt");
const STATE_0_2_0: &str = fixture!("state/v0.2.0.txt");
const STATE_0_3_0: &str = fixture!("state/v0.3.0.txt");
const STATE_0_4_0: &str = fixture!("state/v0.4.0.txt");
const STATE_0_6_0: &str = fixture!("state/v0.6.0.txt");
const STATE_0_7_0: &str = fixture!("state/v0.7.0.txt");
const STATE_0_7_2: &str = fixture!("state/v0.7.2.txt");
const STATE_0_8_0: &str = fixture!("state/v0.8.0.txt");
const STATE_0_8_1: &str = fixture!("state/v0.8.1.txt");
const STATE_0_10_0: &str = fixture!("state/v0.10.0.txt");
const SETTINGS_0_4_0: &str = fixture!("settings/v0.4.0.txt");
const SETTINGS_0_6_0: &str = fixture!("settings/v0.6.0.txt");
const SETTINGS_0_7_0: &str = fixture!("settings/v0.7.0.txt");
const SESSION_0_4_0: &str = fixture!("session/v0.4.0.txt");
const JOURNAL_0_4_0: &str = fixture!("recovery/v0.4.0.txt");
const JOURNAL_0_4_0_ADD: &[u8] = include_bytes!("../../tests/compat/recovery/v0.4.0.add");
const UNTITLED_0_4_0: &str = fixture!("recovery/v0.4.0-untitled.txt");
const UNTITLED_0_4_0_ADD: &[u8] = include_bytes!("../../tests/compat/recovery/v0.4.0-untitled.add");

/// The file whose View → Highlight as was "INI / TOML / YAML" in 0.4–0.6 fixtures.
const INI_FILE: &str = r"C:\ProgramData\App\service.conf";

// ---------------------------------------------------------------------------------------------
// What the fixtures hold.

fn paths(v: &[&str]) -> Vec<PathBuf> {
    v.iter().map(PathBuf::from).collect()
}

fn tab(caret: u64, top: u64, wrap: bool, bookmarks: &[u64], path: &str) -> TabState {
    TabState {
        path: PathBuf::from(path),
        member: None,
        caret,
        top,
        wrap,
        bookmarks: bookmarks.to_vec(),
    }
}

/// The tabs of every fixture: a Windows path, a Unix one, and a non-ASCII name with positions
/// past 4 GB.
fn first_tabs() -> Vec<TabState> {
    vec![
        tab(
            48_213,
            48_100,
            false,
            &[0, 1520, 48_213],
            r"C:\logs\app.log",
        ),
        tab(0, 0, true, &[], "/var/log/syslog"),
        tab(
            9_000_000_000,
            8_999_999_950,
            false,
            &[9_000_000_000],
            r"D:\Отчёты\журнал_2026.log",
        ),
    ]
}

/// Since 0.4 also a file inside an archive (after U+001F) and a config file.
fn all_tabs() -> Vec<TabState> {
    let mut t = first_tabs();
    t.push(TabState {
        member: Some("app/server.log".into()),
        ..tab(120, 0, false, &[], r"D:\archives\logs-2026-10.zip")
    });
    t.push(tab(0, 0, false, &[], INI_FILE));
    t
}

const RECENT_0_1: [&str; 3] = [
    r"D:\Отчёты\журнал_2026.log",
    "/var/log/syslog",
    r"C:\logs\app.log",
];

const RECENT_0_4: [&str; 5] = [
    INI_FILE,
    r"D:\archives\logs-2026-10.zip",
    r"D:\Отчёты\журнал_2026.log",
    "/var/log/syslog",
    r"C:\logs\app.log",
];

fn query(pattern: &str, flags: &str) -> Query {
    query_from(pattern.to_string(), flags)
}

fn marks() -> Vec<Mark> {
    vec![
        Mark {
            query: query(r"user=(\w+)|uid=\d+", "rc"),
            color: 0,
            enabled: true,
        },
        Mark {
            query: query("ERROR", ""),
            color: 3,
            enabled: false,
        },
        Mark {
            query: query("таймаут", "cw"),
            color: 5,
            enabled: true,
        },
    ]
}

fn saved() -> Vec<SavedSearch> {
    vec![
        // Its name had a `|`, which the writer turns into `/`.
        SavedSearch {
            name: "Timeouts / refused".into(),
            query: query(r"(timeout|refused) after \d+ms", "rc"),
            invert: false,
            column: None,
        },
        SavedSearch {
            name: "Not healthcheck".into(),
            query: query("healthcheck=ok", "c"),
            invert: true,
            column: Some(2),
        },
        SavedSearch {
            name: "Ошибки входа".into(),
            query: query("login=failed", "w"),
            invert: false,
            column: None,
        },
    ]
}

const HISTORY: [&str; 3] = ["level=(ERROR|FATAL)", "a|b=c", "таймаут"];

fn keys(v: &[(&str, &str)]) -> Vec<(String, String)> {
    v.iter()
        .map(|(c, k)| (c.to_string(), k.to_string()))
        .collect()
}

const KEYS_0_4: [(&str, &str); 3] = [
    ("goto_time", "ctrl+alt+T"),
    ("find_all", "none"),
    ("theme.hc-dark", "ctrl+shift+F12"),
];

fn opt(path: &str) -> FileOpts {
    FileOpts {
        path: PathBuf::from(path),
        ..Default::default()
    }
}

/// The remembered file choices (encoding, highlighting, view, table layout) of 0.4–0.6.
fn opts_0_4() -> Vec<FileOpts> {
    vec![
        FileOpts {
            enc: Some(Encoding::Windows1251),
            lang: Some(Lang::Log),
            follow: true,
            read_only: true,
            ..opt(r"C:\logs\app.log")
        },
        FileOpts {
            wrap: true,
            ..opt("/var/log/syslog")
        },
        FileOpts {
            lang: Some(Lang::Ini),
            ..opt(INI_FILE)
        },
        FileOpts {
            table: true,
            layout: Some(TableLayout {
                delim: b';',
                header: true,
                sig: 0x3e30_dba4_cd73_d926,
                fixed: vec![(0, 180.0), (3, 64.5)],
                scroll_x: 312.5,
            }),
            ..opt(r"D:\Отчёты\продажи.csv")
        },
        FileOpts {
            lang: Some(Lang::Csv(b'\t')),
            table: true,
            layout: Some(TableLayout {
                delim: b'\t',
                header: false,
                sig: 0x5bfb_cd10_bfcb_6717,
                fixed: Vec::new(),
                scroll_x: 0.0,
            }),
            ..opt("/srv/export/report.tsv")
        },
        FileOpts {
            hex: true,
            ..opt("/var/log/wtmp")
        },
    ]
}

/// Since 0.7: the INI choice under its new name, and languages added in 0.6 and 0.7.
fn opts_0_7() -> Vec<FileOpts> {
    let mut v = opts_0_4();
    v.insert(
        5,
        FileOpts {
            lang: Some(Lang::Yaml),
            ..opt("/etc/app/config.yml.dist")
        },
    );
    v.insert(
        6,
        FileOpts {
            lang: Some(Lang::C),
            ..opt(r"C:\projects\магазин\src\engine.inc")
        },
    );
    v
}

/// Leaves out the 0.4–0.6 choice of View → Highlight as "INI / TOML / YAML", a name gone since
/// 0.7: it is checked in `old_ini_highlighting_survives`.
fn but_old_ini(opts: &[FileOpts]) -> Vec<FileOpts> {
    opts.iter()
        .cloned()
        .map(|mut o| {
            if o.path == Path::new(INI_FILE) {
                o.lang = None;
            }
            o
        })
        .collect()
}

/// The same for the lines of a file: the INI file's `fileopt=` line without its language.
fn text_but_old_ini(text: &str) -> String {
    text.lines()
        .map(
            |l| match l.strip_prefix("fileopt=").filter(|r| r.ends_with(INI_FILE)) {
                Some(r) => {
                    let mut f: Vec<&str> = r.splitn(5, '|').collect();
                    f[1] = "";
                    format!("fileopt={}\n", f.join("|"))
                }
                None => format!("{l}\n"),
            },
        )
        .collect()
}

// ---------------------------------------------------------------------------------------------
// What this version does with them.

/// The theme shown after reading `p`, as `BigTextApp::restore_look` picks it (`None`: the
/// system's dark or light mode, also when the file names no theme).
fn shown_theme(p: &Persisted) -> Option<Theme> {
    if p.theme_system == Some(true) {
        return None;
    }
    p.theme
        .or(p.dark.map(|d| if d { Theme::Dark } else { Theme::Light }))
}

/// (asked, check automatically) after the update settings are read.
fn update_after(p: &Persisted) -> (bool, bool) {
    let mut u = UpdateUi::default();
    u.restore(p);
    (u.asked, u.auto)
}

/// Every value a user would miss compares equal.
fn assert_same(a: &Persisted, b: &Persisted) {
    macro_rules! same {
        ($($f:ident),* $(,)?) => {
            $(assert_eq!(a.$f, b.$f, concat!("`", stringify!($f), "` changed"));)*
        };
    }
    same!(
        recent,
        tabs,
        active,
        font_size,
        dark,
        theme,
        theme_system,
        cvd,
        scale,
        backup_on_save,
        syntax,
        update_check,
        update_asked,
        skip_version,
        update_checked,
        seen_version,
        marks,
        history,
        keys,
        lang,
        font,
        unpack_dir,
        recovery,
        clips,
        level_bar,
        saved,
        show_ws,
        show_ws_code,
        code_theme,
        pinned,
        filter_ctx,
        id_keys,
        file_opts,
        status,
        gutter,
        wrap_column,
        ruler,
        json_crumb,
        log_sources,
        alert_options,
        alert_recent,
        tools,
        commands,
        shell,
        folder,
        folder_open,
        folder_expanded,
    );
}

/// The first save by this version writes back everything the old file said, line for line
/// (the writer has only added keys since 0.1, in an order that keeps the old ones'), and reads
/// back the same. With `old_ini`, the 0.4–0.6 INI highlighting is left out of the comparison.
fn assert_first_save_keeps(p: &Persisted, old: &str, old_ini: bool) {
    let new = extras::serialize(p);
    let (new_cmp, old_cmp) = if old_ini {
        (text_but_old_ini(&new), text_but_old_ini(old))
    } else {
        (new.clone(), old.to_string())
    };
    assert_eq!(
        new_cmp.lines().collect::<Vec<_>>(),
        old_cmp.lines().collect::<Vec<_>>()
    );
    assert_same(p, &extras::parse(&new));
}

/// The values go into the app on start and come back out on its first save (`BigTextApp::new`,
/// `persisted`) through conversions that must not drop them.
fn assert_app_keeps(p: &Persisted) {
    for (id, k) in &p.keys {
        let c = Cmd::from_id(id).unwrap_or_else(|| panic!("command `{id}` is unknown now"));
        assert_eq!(&c.id(), id);
        let key = key_from_string(k).unwrap_or_else(|| panic!("key `{k}` of `{id}` is unread"));
        assert_eq!(&key_to_string(key), k);
    }
    if let Some(l) = &p.lang {
        assert_eq!(
            i18n::Lang::from_code(l).map(|x| x.code()),
            Some(l.as_str()),
            "language"
        );
    }
    if let Some(s) = &p.shell {
        assert_eq!(ShellChoice::from_id(s).id(), s.as_str());
    }
    if let Some(v) = &p.seen_version {
        assert_eq!(Version::parse(v).map(|v| v.to_string()).as_ref(), Some(v));
    }
    let mut u = UpdateUi::default();
    u.restore(p);
    assert_eq!(u.skipped.map(|v| v.to_string()), p.skip_version);
    assert_eq!(Some(u.last_check).filter(|&t| t > 0), p.update_checked);
    if let Some(f) = p.font_size {
        assert_eq!(f.clamp(6.0, 72.0), f);
    }
    if let Some((n, _)) = p.filter_ctx {
        assert!(n <= bigtext::filter::CONTEXT_MAX);
    }
    if let Some(l) = &p.log_sources {
        let mut ui = SourcesUi::default();
        ui.set_line(l);
        assert_eq!(&ui.to_line(), l);
    }
    if let Some(o) = &p.alert_options {
        let mut ui = AlertsUi::default();
        ui.set_options_line(o);
        assert_eq!(&ui.options_line(), o);
    }
    for a in &p.alert_recent {
        let q = alerts::parse_query_line(a).unwrap_or_else(|| panic!("alert `{a}` is unread"));
        assert_eq!(&alerts::query_line(&q), a);
    }
}

/// `bigtext-cli filter --saved NAME` finds the saved searches in the file too.
fn assert_cli_reads_saved(text: &str) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.txt");
    std::fs::write(&path, text).unwrap();
    assert_eq!(saved_searches(&path).unwrap(), saved());
}

/// What every state file from 0.4 on holds in the fixtures (each version's own keys are checked
/// by its test).
fn assert_since_0_4(p: &Persisted) {
    assert_eq!(p.font_size, Some(14.0));
    assert_eq!((p.cvd, p.scale), (Some(true), Some(125)));
    assert_eq!((p.backup_on_save, p.syntax), (Some(true), Some(true)));
    assert_eq!(
        p.id_keys,
        Some(
            ["request_id", "trace_id", "user.id"]
                .map(String::from)
                .to_vec()
        )
    );
    assert_eq!(p.filter_ctx, Some((3, true)));
    assert_eq!(
        (p.show_ws, p.level_bar, p.gutter, p.json_crumb),
        (Some(false), Some(true), Some(true), Some(true))
    );
    assert_eq!(
        p.status,
        Some(StatusFields {
            offset: true,
            sel: true,
            dt: true,
            legend: true,
            kind: false,
            index_time: false,
            path: true,
        })
    );
    assert_eq!(
        p.tools,
        [
            Tool {
                name: "Open in VS Code".into(),
                command: r#"code -g "{file}:{line}:{col}""#.into(),
            },
            Tool {
                name: "jq / pretty".into(),
                command: r#"jq . "{file}" | less"#.into(),
            },
        ]
    );
    assert_eq!(
        p.commands,
        [
            r#"grep -c "level=ERROR" "{file}""#,
            "sort | uniq -c | sort -rn"
        ]
    );
    assert_eq!(
        p.shell.as_deref().map(ShellChoice::from_id),
        Some(ShellChoice::GitBash)
    );
    assert_eq!(p.font, Some(PathBuf::from(r"C:\Windows\Fonts\consola.ttf")));
    assert_eq!(
        p.clips,
        [
            "SELECT * FROM users\nWHERE id = 42;",
            r"C:\logs\app.log",
            "line1\r\nline2\r\n",
        ]
    );
    assert_eq!(p.recovery, Some(true));
    assert_eq!(p.unpack_dir, Some(PathBuf::from(r"D:\unpacked")));
    assert_eq!(p.recent, paths(&RECENT_0_4));
    assert_eq!(p.marks, marks());
    assert_eq!(p.saved, saved());
    assert_eq!(p.pinned, paths(&[r"C:\logs\app.log"]));
    assert_eq!(p.keys[..3], keys(&KEYS_0_4)[..]);
    assert_eq!(p.history, HISTORY);
    assert_eq!(p.tabs, all_tabs());
    assert_eq!(p.active, 3);
    // The table layouts are kept while the header line hashes the same.
    assert_eq!(
        fnv1a("дата;регион;сумма;валюта".as_bytes()),
        0x3e30_dba4_cd73_d926
    );
    assert_eq!(fnv1a(b"id\tname\tscore"), 0x5bfb_cd10_bfcb_6717);
}

/// File → Open folder's side bar (since 0.6).
fn assert_folder(p: &Persisted) {
    assert_eq!(p.folder, Some(PathBuf::from(r"C:\projects\магазин")));
    assert_eq!(p.folder_open, Some(true));
    assert_eq!(
        p.folder_expanded,
        paths(&[r"C:\projects\магазин\src", r"C:\projects\магазин\logs"])
    );
}

// ---------------------------------------------------------------------------------------------
// state.txt

#[test]
fn state_0_1_0() {
    let p = extras::parse(STATE_0_1_0);
    assert_eq!(p.font_size, Some(16.0));
    // Only `dark=` then: the dark theme, not the system's mode.
    assert_eq!(
        (shown_theme(&p), p.theme, p.theme_system),
        (Some(Theme::Dark), None, None)
    );
    assert_eq!((p.backup_on_save, p.syntax), (Some(false), Some(true)));
    assert_eq!(p.recent, paths(&RECENT_0_1));
    assert_eq!(p.tabs, first_tabs());
    assert_eq!(p.active, 2);
    assert_app_keeps(&p);
    assert_first_save_keeps(&p, STATE_0_1_0, false);
}

#[test]
fn state_0_2_0() {
    let p = extras::parse(STATE_0_2_0);
    assert_eq!(p.font_size, Some(15.0));
    assert_eq!(shown_theme(&p), Some(Theme::Light));
    assert_eq!((p.backup_on_save, p.syntax), (Some(true), Some(true)));
    assert_eq!(p.update_check, Some(false));
    // Turning the check off before BigText asked is an answer: it stays off, unasked.
    assert_eq!(update_after(&p), (true, false));
    assert_eq!(p.skip_version.as_deref(), Some("0.2.1"));
    assert_eq!(p.update_checked, Some(1_793_093_400));
    assert_eq!(p.recent, paths(&RECENT_0_1));
    assert_eq!(p.tabs, first_tabs());
    assert_eq!(p.active, 0);
    assert_app_keeps(&p);
    assert_first_save_keeps(&p, STATE_0_2_0, false);
}

#[test]
fn state_0_3_0() {
    let p = extras::parse(STATE_0_3_0);
    assert_eq!(p.font_size, Some(13.5));
    assert_eq!(shown_theme(&p), Some(Theme::Dark));
    assert_eq!((p.backup_on_save, p.syntax), (Some(false), Some(false)));
    // Left on (the old default) is not an answer: BigText asks (by design, `UpdateUi::restore`).
    assert_eq!(p.update_check, Some(true));
    assert_eq!(update_after(&p), (false, false));
    assert_eq!(p.skip_version.as_deref(), Some("0.3.1"));
    assert_eq!(p.update_checked, Some(1_793_179_800));
    assert_eq!(p.font, Some(PathBuf::from(r"C:\Windows\Fonts\consola.ttf")));
    assert_eq!(p.lang.as_deref(), Some("uz"));
    assert_eq!(p.recent, paths(&RECENT_0_1));
    assert_eq!(p.marks, marks());
    assert_eq!(
        p.keys,
        keys(&[
            ("goto_time", "ctrl+alt+T"),
            ("find_all", "none"),
            ("text.trimtrailing", "alt+shift+F5"),
        ])
    );
    assert_eq!(p.history, HISTORY);
    assert_eq!(p.tabs, first_tabs());
    assert_eq!(p.active, 1);
    assert_app_keeps(&p);
    assert_first_save_keeps(&p, STATE_0_3_0, false);
}

#[test]
fn state_0_4_0() {
    let p = extras::parse(STATE_0_4_0);
    assert_since_0_4(&p);
    assert_eq!(
        (shown_theme(&p), p.dark),
        (Some(Theme::HighContrastDark), Some(true))
    );
    assert_eq!(p.lang.as_deref(), Some("ru"));
    assert_eq!((p.wrap_column, p.ruler), (Some(Some(100)), Some(Some(120))));
    assert_eq!(p.keys, keys(&KEYS_0_4));
    assert_eq!(but_old_ini(&p.file_opts), but_old_ini(&opts_0_4()));
    assert_eq!((p.update_check, p.update_asked), (Some(false), None));
    assert_eq!(update_after(&p), (true, false));
    assert_eq!(p.skip_version.as_deref(), Some("0.4.1"));
    assert_eq!(p.update_checked, Some(1_793_266_200));
    assert_eq!((p.show_ws_code, p.code_theme), (None, None));
    assert_eq!((&p.folder, &p.seen_version), (&None, &None));
    assert_app_keeps(&p);
    assert_cli_reads_saved(STATE_0_4_0);
    assert_first_save_keeps(&p, STATE_0_4_0, true);
}

#[test]
fn state_0_6_0() {
    let p = extras::parse(STATE_0_6_0);
    assert_since_0_4(&p);
    assert_eq!((shown_theme(&p), p.dark), (Some(Theme::Dark), Some(true)));
    assert_eq!(p.lang.as_deref(), Some("en"));
    // 0: wrap at the window's width, no ruler.
    assert_eq!((p.wrap_column, p.ruler), (Some(None), Some(None)));
    assert_eq!(p.show_ws_code, Some(true));
    assert_folder(&p);
    assert_eq!(p.keys, keys(&KEYS_0_4));
    assert_eq!(but_old_ini(&p.file_opts), but_old_ini(&opts_0_4()));
    assert_eq!(update_after(&p), (true, false));
    assert_eq!(p.skip_version.as_deref(), Some("0.6.1"));
    assert_eq!(p.update_checked, Some(1_794_303_000));
    assert_app_keeps(&p);
    assert_cli_reads_saved(STATE_0_6_0);
    assert_first_save_keeps(&p, STATE_0_6_0, true);
}

#[test]
fn state_0_7_0() {
    let p = extras::parse(STATE_0_7_0);
    assert_since_0_4(&p);
    assert_eq!((shown_theme(&p), p.dark), (Some(Theme::Dark), Some(true)));
    assert_eq!(p.code_theme, Some(CodeTheme::Monokai));
    assert_eq!(p.lang.as_deref(), Some("en"));
    assert_eq!((p.wrap_column, p.ruler), (Some(Some(100)), Some(Some(120))));
    assert_eq!(p.show_ws_code, Some(true));
    assert_folder(&p);
    let mut k = keys(&KEYS_0_4);
    k.extend(keys(&[("code_theme.github-dark", "ctrl+alt+F9")]));
    assert_eq!(p.keys, k);
    assert_eq!(p.file_opts, opts_0_7());
    assert_eq!((p.update_check, p.update_asked), (Some(false), None));
    assert_eq!(update_after(&p), (true, false));
    assert_eq!(p.skip_version.as_deref(), Some("0.7.1"));
    assert_eq!(p.update_checked, Some(1_794_389_400));
    assert_app_keeps(&p);
    assert_cli_reads_saved(STATE_0_7_0);
    assert_first_save_keeps(&p, STATE_0_7_0, false);
}

#[test]
fn state_0_7_2() {
    let p = extras::parse(STATE_0_7_2);
    assert_since_0_4(&p);
    assert_eq!((shown_theme(&p), p.dark), (Some(Theme::Light), Some(false)));
    assert_eq!(p.code_theme, Some(CodeTheme::Monokai));
    assert_eq!(p.lang.as_deref(), Some("uz"));
    assert_eq!(p.show_ws_code, Some(true));
    assert_folder(&p);
    assert_eq!(p.keys, keys(&KEYS_0_4));
    assert_eq!(p.file_opts, opts_0_7());
    // Asked, and the answer was yes.
    assert_eq!((p.update_check, p.update_asked), (Some(true), Some(true)));
    assert_eq!(update_after(&p), (true, true));
    assert_eq!(p.skip_version.as_deref(), Some("0.7.3"));
    assert_eq!(p.update_checked, Some(1_794_475_800));
    assert_app_keeps(&p);
    assert_cli_reads_saved(STATE_0_7_2);
    assert_first_save_keeps(&p, STATE_0_7_2, false);
}

#[test]
fn state_0_8_0() {
    let p = extras::parse(STATE_0_8_0);
    assert_since_0_4(&p);
    assert_eq!((shown_theme(&p), p.dark), (Some(Theme::Dark), Some(true)));
    assert_eq!(p.code_theme, Some(CodeTheme::Monokai));
    assert_eq!(p.lang.as_deref(), Some("uz"));
    assert_folder(&p);
    assert_eq!(p.file_opts, opts_0_7());
    assert_eq!(update_after(&p), (true, true));
    assert_eq!(p.skip_version.as_deref(), Some("0.8.1"));
    // What's new is shown for the versions after this one.
    assert_eq!(p.seen_version.as_deref(), Some("0.8.0"));
    assert_eq!(p.update_checked, Some(1_794_562_200));
    assert_app_keeps(&p);
    assert_cli_reads_saved(STATE_0_8_0);
    assert_first_save_keeps(&p, STATE_0_8_0, false);
}

#[test]
fn state_0_8_1() {
    let p = extras::parse(STATE_0_8_1);
    assert_since_0_4(&p);
    // `theme=system`: follows the system; `dark=` is for older versions.
    assert_eq!(
        (shown_theme(&p), p.theme_system, p.theme, p.dark),
        (None, Some(true), None, Some(false))
    );
    assert_eq!(p.code_theme, Some(CodeTheme::Monokai));
    assert_folder(&p);
    assert_eq!(p.file_opts, opts_0_7());
    assert_eq!(update_after(&p), (true, true));
    assert_eq!(p.skip_version.as_deref(), Some("0.9.0"));
    assert_eq!(p.seen_version.as_deref(), Some("0.8.1"));
    assert_eq!(p.update_checked, Some(1_794_565_800));
    assert_app_keeps(&p);
    assert_cli_reads_saved(STATE_0_8_1);
    assert_first_save_keeps(&p, STATE_0_8_1, false);
}

#[test]
fn state_0_10_0() {
    let p = extras::parse(STATE_0_10_0);
    assert_since_0_4(&p);
    assert_eq!(
        (shown_theme(&p), p.dark),
        (Some(Theme::HighContrastLight), Some(false))
    );
    assert_eq!(p.code_theme, Some(CodeTheme::Monokai));
    assert_eq!(p.lang.as_deref(), Some("ru"));
    assert_folder(&p);
    assert_eq!(p.file_opts, opts_0_7());
    assert_eq!(update_after(&p), (true, true));
    assert_eq!(p.skip_version.as_deref(), Some("0.11.0"));
    assert_eq!(p.seen_version.as_deref(), Some("0.10.0"));
    assert_eq!(p.update_checked, Some(1_794_580_000));
    // File → Open container or service log…
    let mut ui = SourcesUi::default();
    ui.set_line(p.log_sources.as_deref().unwrap());
    assert_eq!(
        (ui.host.as_str(), ui.tab, ui.tool.as_str(), ui.how),
        ("ops@build-01", Tab::Kubernetes, "docker", HowMuch::Minutes)
    );
    assert_eq!(
        (ui.lines, ui.minutes, ui.follow, ui.timestamps),
        (10_000, 90, true, false)
    );
    // Alerts on new lines: notification, no sound, only in the background; recent patterns.
    let mut alerts_ui = AlertsUi::default();
    alerts_ui.set_options_line(p.alert_options.as_deref().unwrap());
    assert_eq!(
        (alerts_ui.notify, alerts_ui.sound, alerts_ui.background_only),
        (true, false, true)
    );
    let recent: Vec<Query> = p
        .alert_recent
        .iter()
        .filter_map(|l| alerts::parse_query_line(l))
        .collect();
    assert_eq!(
        recent,
        [
            query(r"(timeout|refused) after \d+ms", "rc"),
            query("panic=1", "w"),
        ]
    );
    assert_app_keeps(&p);
    assert_cli_reads_saved(STATE_0_10_0);
    assert_first_save_keeps(&p, STATE_0_10_0, false);
}

/// 0.4–0.6 wrote View → Highlight as "INI / TOML / YAML"; 0.7 gave YAML a language of its own
/// and renamed this one "INI / TOML". The old name is read as INI (it was dropped until then).
#[test]
fn old_ini_highlighting_survives() {
    for text in [STATE_0_4_0, STATE_0_6_0] {
        let p = extras::parse(text);
        let o = p.file_opts.iter().find(|o| o.path == Path::new(INI_FILE));
        assert_eq!(o.and_then(|o| o.lang), Some(Lang::Ini));
    }
    let s = sessions::parse(SESSION_0_4_0);
    let o = s.opts.iter().find(|o| o.path == Path::new(INI_FILE));
    assert_eq!(o.and_then(|o| o.lang), Some(Lang::Ini));
}

// ---------------------------------------------------------------------------------------------
// Help → Export settings (*.settings)

/// What every exported settings file holds in the fixtures: the shareable settings only.
fn assert_settings(p: &Persisted) {
    assert_eq!(p.font_size, Some(14.0));
    assert_eq!((p.cvd, p.scale), (Some(true), Some(125)));
    assert_eq!((p.backup_on_save, p.syntax), (Some(true), Some(true)));
    assert_eq!(
        (p.show_ws, p.level_bar, p.gutter, p.json_crumb),
        (Some(false), Some(true), Some(true), Some(true))
    );
    assert_eq!(
        p.status.map(|s| (s.offset, s.path, s.kind)),
        Some((true, true, false))
    );
    assert_eq!(p.font, Some(PathBuf::from(r"C:\Windows\Fonts\consola.ttf")));
    assert_eq!(p.marks, marks());
    assert_eq!(p.saved, saved());
    // Nothing about one computer's files or this copy's updates.
    assert!(p.recent.is_empty() && p.tabs.is_empty() && p.pinned.is_empty());
    assert!(p.file_opts.is_empty() && p.clips.is_empty() && p.history.is_empty());
    assert_eq!(
        (&p.skip_version, &p.shell, &p.folder),
        (&None, &None, &None)
    );
    assert_eq!((p.update_check, p.active), (None, 0));
}

/// Importing it and saving: the same settings, written back line for line.
fn assert_import_keeps(p: &Persisted, exported: &str) {
    assert_app_keeps(p);
    assert_cli_reads_saved(exported);
    let as_state = exported.replacen("# BigText settings", "# BigText session state", 1);
    assert_first_save_keeps(p, &as_state, false);
}

#[test]
fn settings_0_4_0() {
    let p = extras::parse(SETTINGS_0_4_0);
    assert_settings(&p);
    assert_eq!(
        (shown_theme(&p), p.dark),
        (Some(Theme::HighContrastDark), Some(true))
    );
    assert_eq!(p.lang.as_deref(), Some("ru"));
    assert_eq!((p.wrap_column, p.ruler), (Some(Some(100)), Some(Some(120))));
    assert_eq!((p.show_ws_code, p.code_theme), (None, None));
    assert_eq!(p.keys, keys(&KEYS_0_4));
    assert_import_keeps(&p, SETTINGS_0_4_0);
}

#[test]
fn settings_0_6_0() {
    let p = extras::parse(SETTINGS_0_6_0);
    assert_settings(&p);
    assert_eq!((shown_theme(&p), p.dark), (Some(Theme::Dark), Some(true)));
    assert_eq!(p.lang.as_deref(), Some("en"));
    assert_eq!((p.wrap_column, p.ruler), (Some(None), Some(None)));
    assert_eq!((p.show_ws_code, p.code_theme), (Some(true), None));
    assert_eq!(p.keys, keys(&KEYS_0_4));
    assert_import_keeps(&p, SETTINGS_0_6_0);
}

#[test]
fn settings_0_7_0() {
    let p = extras::parse(SETTINGS_0_7_0);
    assert_settings(&p);
    assert_eq!((shown_theme(&p), p.dark), (Some(Theme::Dark), Some(true)));
    assert_eq!(p.lang.as_deref(), Some("en"));
    assert_eq!((p.wrap_column, p.ruler), (Some(Some(100)), Some(Some(120))));
    assert_eq!(
        (p.show_ws_code, p.code_theme),
        (Some(true), Some(CodeTheme::Monokai))
    );
    let mut k = keys(&KEYS_0_4);
    k.extend(keys(&[("code_theme.github-dark", "ctrl+alt+F9")]));
    assert_eq!(p.keys, k);
    assert_import_keeps(&p, SETTINGS_0_7_0);
}

// ---------------------------------------------------------------------------------------------
// sessions/<name>.session

#[test]
fn session_0_4_0() {
    let s = sessions::parse(SESSION_0_4_0);
    let tabs: Vec<TabState> = s.tabs.iter().map(|t| t.0.clone()).collect();
    assert_eq!(tabs, all_tabs());
    let pinned: Vec<bool> = s.tabs.iter().map(|t| t.1).collect();
    assert_eq!(pinned, [true, false, true, false, false]);
    assert_eq!(
        s.tabs[0].2,
        Some(SavedFilter {
            query: query("level=(ERROR|WARN)", "rc"),
            invert: false,
            levels: Some(Levels::only(Level::Error).toggled(Level::Warn)),
            terms: vec![
                (query("healthcheck", "c"), true),
                (query(r"user=\d+", "r"), false),
            ],
        })
    );
    assert_eq!(
        s.tabs[1].2,
        Some(SavedFilter {
            query: query("CRON", "w"),
            invert: true,
            levels: None,
            terms: Vec::new(),
        })
    );
    assert!(s.tabs[2..].iter().all(|t| t.2.is_none()));
    assert_eq!(but_old_ini(&s.opts), but_old_ini(&opts_0_4()[..3]));
    assert_eq!(s.marks, marks());
    assert_eq!(s.active, 3);
    // Saved again by this version: the same lines, read back the same.
    let again = sessions::serialize(&s);
    assert_eq!(
        text_but_old_ini(&again).lines().collect::<Vec<_>>(),
        text_but_old_ini(SESSION_0_4_0).lines().collect::<Vec<_>>()
    );
    let t = sessions::parse(&again);
    assert_eq!(t.tabs, s.tabs);
    assert_eq!(t.opts, s.opts);
    assert_eq!(t.marks, s.marks);
    assert_eq!(t.active, s.active);
}

// ---------------------------------------------------------------------------------------------
// recovery/<pid>-<doc>.pieces and .add

/// The file the 0.4.0 journal's edits were made to.
const JOURNAL_FILE: &str = "2026-10-29 10:00:01 INFO  start pid=4242\n\
                            2026-10-29 10:00:02 ERROR db: connection refused\n\
                            2026-10-29 10:00:03 INFO  retry 1/3\n\
                            2026-10-29 10:00:04 INFO  connected\n";

/// The text of journal `j` with typed bytes `add` over `file` (an empty source for an untitled
/// document), as `recovery::rebuild` puts it together.
fn rebuilt(j: &Journal, add: &[u8], file: Option<&Path>) -> String {
    let source = Arc::new(match file {
        Some(f) => Source::open(f, OpenMode::Auto).unwrap(),
        None => Source::empty(),
    });
    assert_eq!(source.len(), j.len);
    assert!(add.len() as u64 >= j.add_len);
    let add = add[..j.add_len as usize].to_vec();
    let index = LineIndex::new(source.len());
    let buf = TextBuf::with_pieces(source, index, j.codec, &j.pieces, add)
        .expect("a piece points outside the file or the typed bytes");
    String::from_utf8(buf.read(0..buf.len()).unwrap().to_vec()).unwrap()
}

#[test]
fn journal_0_4_0() {
    let j = recovery::parse("4242-3", JOURNAL_0_4_0).expect("not read as a journal");
    assert_eq!(j.title, "сервер.log");
    assert_eq!(j.path, Some(PathBuf::from(r"C:\logs\сервер.log")));
    assert_eq!(j.len, 162);
    assert_eq!(j.mtime, Some(Duration::new(1_793_266_185, 123_456_789)));
    assert_eq!(j.codec, Codec::new(Encoding::Utf8, NlByte::Lf, 0));
    assert_eq!(
        j.pieces,
        [
            (Buf::Add, 0, 37),
            (Buf::Orig, 0, 61),
            (Buf::Add, 37, 5),
            (Buf::Orig, 66, 96),
            (Buf::Add, 42, 41),
        ]
    );
    assert_eq!(j.add_len, 83);
    // Written again by this version: the same file.
    assert_eq!(recovery::format(&j), JOURNAL_0_4_0);
    assert_eq!(
        recovery::parse("4242-3", &recovery::format(&j)),
        Some(j.clone())
    );
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("сервер.log");
    std::fs::write(&file, JOURNAL_FILE).unwrap();
    assert_eq!(
        rebuilt(&j, JOURNAL_0_4_0_ADD, Some(&file)),
        "# проверено: Эркин ✓\n\
         2026-10-29 10:00:01 INFO  start pid=4242\n\
         2026-10-29 10:00:02 WARN  db: connection refused\n\
         2026-10-29 10:00:03 INFO  retry 1/3\n\
         2026-10-29 10:00:04 INFO  connected\n\
         2026-10-29 10:00:05 INFO  done | rows=12\n"
    );
}

#[test]
fn journal_0_4_0_untitled() {
    let j = recovery::parse("4242-7", UNTITLED_0_4_0).expect("not read as a journal");
    assert_eq!(j.title, "Без имени-2");
    assert_eq!((&j.path, j.len, j.mtime), (&None, 0, None));
    assert_eq!(j.codec, Codec::utf8());
    assert_eq!(j.pieces, [(Buf::Add, 0, 71), (Buf::Add, 88, 45)]);
    assert_eq!(j.add_len, 133);
    assert_eq!(recovery::format(&j), UNTITLED_0_4_0);
    assert_eq!(
        recovery::parse("4242-7", &recovery::format(&j)),
        Some(j.clone())
    );
    assert_eq!(
        rebuilt(&j, UNTITLED_0_4_0_ADD, None),
        "Заметки к инциденту\n\
         INC-1042: timeout=30s | retries=3\n\
         Вывод: перезапуск помог.\n"
    );
}
