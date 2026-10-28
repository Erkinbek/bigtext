# Changelog

All notable changes to BigText. Each release on
[GitHub](https://github.com/Erkinbek/bigtext/releases) has the full notes and the downloads.

## Unreleased

### Logs

- **Level bar** above log files: **ERROR**, **WARN**, **INFO** and **DEBUG** buttons list only the
  lines of those levels in the filter view (click several to combine them). **F8** / **Shift+F8**
  jump to the next / previous error line, or to the next line of the levels shown. Upper-case
  level words count anywhere; `[error]`, `level=error` and `"level":"error"` count too, but
  "no error found" does not. View → Log level bar hides it.
- **File → Merge logs by time…**: the lines of several open logs (say `app.log` and
  `nginx.log`) in timestamp order in a new read-only document. Each line starts with its file's
  name in that file's colour (the status bar shows the legend); lines without a time, like
  stack traces, stay with the line above. Logs in different encodings can be merged, and the
  files are read once from front to back, so it works on logs of any size.
- **Search → Time histogram** (Ctrl+Shift+H): a bar chart under the text of how many lines fall
  in each second, minute, hour or day, with the errors (or the search matches) in colour. Hover
  a bar for its counts, click it to jump to its first line; the caret's time is marked. It is
  built in one pass with partial results on the way, and its buckets widen as needed, so it
  stays small for logs of any length.
- **Search → Find in files…** (Ctrl+Alt+F): the search pattern in every file of a folder whose
  name fits (`*.log *.txt`; sub-folders too, hidden ones like `.git` skipped), listed as
  `file:line` with the text around each match. Click a match to open the file with the match
  selected. Each file is searched with the streaming search in its own encoding; binary files
  are skipped and open tabs are searched with their unsaved edits.
- More timestamp formats for Go to time, Δt, the histogram and merging: **Unix times** (10-digit
  seconds with an optional fraction, or 13-, 16- and 19-digit milli-, micro- and nanoseconds)
  as a line's first word or as `"ts":`, `"time":`, `time=` values; **time zones** (`Z`,
  `+05:00`, `-0800`); 12-hour clocks with **AM/PM**; Java's `May 1, 2024 2:32:05 PM`, .NET's
  `5/1/2024 2:32:05 PM`, one-digit days and months, and `Wed May 01 14:32:05 UTC 2024`. Logs
  that all give their time zone are merged in UTC. Hovering a Unix time in the text shows it
  as a date and time (UTC).
- Files without a telling extension (`syslog`, `stdin`, `app.txt`) are recognised as logs when
  their lines start with a time, and as JSON or XML by their first line.

## 0.3.0 — 2026-10-28

### Logs

- **Filter view** (Ctrl+Shift+F, or **Filter** in the search bar): only the lines with a match,
  or with **!** only the lines without one, like `grep` over the whole file, in a list under
  the text. Click a line to jump to it. **Save lines…** writes every kept line to a new file;
  **Copy lines** copies them (up to 100 MB). Find All results have the same two buttons.
- **Highlights** (View → Highlights…, or **Mark** in the search bar): patterns painted in their
  own colours in every document, kept across sessions.
- **Scrollbar marks** for Find All matches, filter lines, bookmarks and the differences from
  File → Compare.
- **Go to time** (Ctrl+Shift+T): the first log line at or after `14:32:05` or
  `2024-05-01 14:32:05`, found with a binary search that reads a few hundred KB even in a 50 GB
  log. ISO 8601, `DD.MM.YYYY`, `MM/DD/YYYY`, Apache, syslog and bare times are recognised.
  A selection over timestamped lines shows the time between them (**Δt**) in the status bar.
- **Compressed files**: `.gz` and `.zst` (also several members or frames back to back) are
  unpacked into a temporary read-only copy, with progress and Cancel.

### Search and editing

- Other occurrences of a selected word are highlighted.
- **Search history**: the ▾ button next to the find field, or Up/Down inside it.
- Regular-expression errors are shown in full under the search field, with a `^` under the
  problem (before, only "error parsing pattern 0" was shown).
- **Column (block) selection** with Alt+drag: copy it line by line; typing, Backspace, Delete
  and Tab change every row at once (one undo step).
- **Edit → Text** on the selection (or a whole document up to 64 MB): sort lines (A→Z, Z→A,
  by number), reverse, remove duplicate lines, UPPER / lower case, trim trailing whitespace,
  tabs ↔ spaces, and JSON pretty-print / minify that keeps key order and numbers as written.

### Files

- **File → Export lines…** writes a line range to a new file, **Save selection as…** the
  selection, and **Split file…** cuts the document into `name.part001.ext`, … of a given size
  or number of lines, always at line breaks.
- **File → Save with encoding**: UTF-8 (with or without BOM), UTF-16 LE/BE, Windows-1251/1252
  or ISO-8859-1; characters the target cannot represent are counted in the message.

### CSV tables

- Right-click a column header: **sort** rows ascending or descending (numbers numerically,
  text ignoring case; files up to 1 GB), **Statistics…** (rows, empty, distinct values,
  sum / mean / min / max, the most frequent values), **Filter rows by this column**,
  **Rename…**, **Move left/right**, and **Export** columns to a new CSV. Files up to 64 MB
  change in one undo step; larger files are rewritten on disk with an optional `.bak`.

### Interface

- **Command palette** (Ctrl+Shift+P): run any command by typing part of its name.
- **Help → Keyboard shortcuts**: every command with its key; click one and press new keys to
  change it.
- **View → Font…**: any installed monospaced font or font file, with a preview.
- **View → Language**: English, Russian or Uzbek.
- The status bar shows the lines, characters and bytes of the selection.
- Background tasks (exports, splitting, statistics) show their progress with Cancel in the
  status bar and a notice with an **Open** link when they finish.

### Command line

- `bigtext app.log:120` (also `FILE:LINE:COLUMN`) opens at a line; `-n/--line N`,
  `--find TEXT`, `--regex RE`, `-f/--follow`, and `-` reads standard input
  (`journalctl -f | bigtext -`). `bigtext --help` lists them.

### Build

- The winget job of the release workflow skips itself until the package is in winget-pkgs,
  instead of failing the run.

## 0.2.0 — 2026-10-27

- **Updates from inside the app**: a daily check against GitHub releases (Help → Check for
  updates…), a dialog with the release notes, **Skip this version**, and an install that
  replaces the portable `bigtext.exe` or runs the installer silently, then restarts.
- **CSV and TSV as a table** (Ctrl+T): detected delimiter, pinned header, resizable columns,
  numbers aligned right; only the visible rows are read.
- **Delete CSV columns** with their values from every row (one undo step up to 64 MB,
  rewritten on disk above that).
- `bigtext.exe` no longer needs the Visual C++ runtime, and an error dialog explains when the
  window cannot open (OpenGL 2.0 is needed).
- `cargo install bigtext`; releases are published to crates.io.

## 0.1.1 — 2026-10-24

- The BigText icon in the executable, the window, the installer and the Explorer context menu.

## 0.1.0 — 2026-10-24

First release: a text editor for files of 1–50+ GB that never loads a whole file into memory.
Instant opening and go to line with a background line index, plain-text and linear-time regex
search (2.4 GB/s), Replace All streamed to disk for large jobs, piece-table editing with
undo/redo, crash-safe atomic saving, encoding and line-ending detection, a hex view, follow
mode, syntax highlighting, bookmarks, split view, a streaming file comparison, session
restore, and Windows, Linux and macOS builds.
