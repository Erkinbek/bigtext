# Upgrade-compatibility fixtures

Files that older 0.x releases wrote, byte for byte, for a rich session. Before 1.0 every one of
them must still be read right by the current code, and its first save must keep what they say.
None of these formats has a version field (only the journal's `bigtext-journal 1` header), so
the tests in `src/app/compat_tests.rs` are the only guard:

    cargo test --bin bigtext compat_tests

A fixture exists only for a release whose writer output changed. The files have no comment
lines, so that they stay exactly what the writer produced; this page says where each came from.

## Fixtures

| Fixture | Release | Writer (at that tag) | What the release added |
|---|---|---|---|
| `state/v0.1.0.txt` | v0.1.0 | `src/app/extras.rs` `serialize` | font_size, dark, backup_on_save, syntax, recent, tab, active |
| `state/v0.2.0.txt` | v0.2.0 | same | update_check, skip_version, update_checked |
| `state/v0.3.0.txt` | v0.3.0 | same | font, lang, mark, key, find |
| `state/v0.4.0.txt` | v0.4.0 | same, with `tab_line`, `mark_line`, `file_opts_line`; `status_bar.rs` `StatusFields::to_line`; `clipboard.rs` `escape` | theme, cvd, scale, id_keys, filter_ctx, show_ws, level_bar, status, gutter, wrap_column, ruler, json_crumb, tool, cmd, shell, clip, recovery, unpack_dir, saved, pinned, fileopt; the archive member after U+001F in `tab=` |
| `state/v0.6.0.txt` | v0.6.0 | same | show_ws_code, folder, folder_open, folder_expand |
| `state/v0.7.0.txt` | v0.7.0 | same | code_theme (and the highlighting name `INI / TOML / YAML` became `INI / TOML`) |
| `state/v0.7.2.txt` | v0.7.2 | same | update_asked |
| `state/v0.8.0.txt` | v0.8.0 | same | seen_version |
| `state/v0.8.1.txt` | v0.8.1 | same | `theme=system` |
| `state/v0.10.0.txt` | v0.10.0 | same; `log_sources.rs` `SourcesUi::to_line`; `alerts.rs` `options_line`, `query_line` | log_sources, alert_options, alert |
| `settings/v0.4.0.txt` | v0.4.0 | `src/app/portable.rs` `export_settings_to` (`serialize(shareable(..))`) | Help → Export settings |
| `settings/v0.6.0.txt` | v0.6.0 | same | show_ws_code |
| `settings/v0.7.0.txt` | v0.7.0 | same | code_theme |
| `session/v0.4.0.txt` | v0.4.0 | `src/app/sessions.rs` `serialize` | File → Sessions (tab, pinned, filter, filterterm, fileopt, mark, active) |
| `recovery/v0.4.0.txt` + `.add` | v0.4.0 | `src/app/recovery.rs` `format`, `journal_tick` (the `.add` bytes) | a file's journal: typed bytes between pieces of the file |
| `recovery/v0.4.0-untitled.txt` + `.add` | v0.4.0 | same | an untitled document's journal |

Releases without a fixture wrote the same as the one before them: v0.1.1, v0.4.1, v0.5.0, v0.7.1,
v0.7.3, v0.9.0, v0.12.0, and v0.11.0, whose changes to these files were rustfmt only. Sessions and
journals have not changed since v0.4.0, exported settings since v0.7.0, `state.txt` since v0.10.0.

## How they were made and checked

Each `state.txt` follows the key order of that tag's `serialize` and the values its `persisted()`
puts in: paths like `C:\logs\app.log` and `/var/log/syslog`, a non-ASCII file name, offsets past
4 GB, patterns with `|` and `=`, a saved search whose name had a `|` (written as `/`), key
bindings, highlights, clips with newlines and backslashes escaped, table layouts whose `sig` is
the FNV-1a of the header line. The tab lines from v0.4.0 on and `session/v0.4.0.txt` hold a raw
U+001F before the archive's file name. The `.add` files are the typed bytes, as written.

Every fixture was then read by its own release's parser and written back by that release's
writer (`git archive <tag>`, a small test appended to the module, `cargo test --bin bigtext`),
and came out byte for byte the same; a newer fixture fed to an older release does not.

## Found by these tests

v0.4.0 to v0.6.0 saved View → Highlight as INI as `INI / TOML / YAML` in `fileopt=` lines
(`state.txt` and sessions). v0.7.0 renamed it `INI / TOML`, and from then on the choice was
dropped on reading. `extras::parse_file_opts` reads the old name as INI since 2026-11-24
(`old_ini_highlighting_survives`).
