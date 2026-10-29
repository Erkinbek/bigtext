# Changelog

All notable changes to BigText. Each release on
[GitHub](https://github.com/Erkinbek/bigtext/releases) has the full notes and the downloads.

## Unreleased

### Compare

- **F7 / Shift+F7** (Search → Next / Previous difference) step the caret through the differences
  of a comparison, around the ends, with "Difference 12 of 340"; the compare panel has the same
  as ◀ ▶ buttons and **Swap A↔B** to compare the other way round. **File → Compare with tab ▸**
  lists the other open files, so a comparison takes one click.
- The compare panel can ignore **white space**, **case**, the **timestamp** at the start of
  each line, and whatever a **mask** regular expression matches (`req=[0-9a-f]+`, durations), so
  two runs of the same job compare by what they did, not when; the header says what is ignored.
  On the command line, `bigtext diff A B [-w] [-i] [-t] [--mask RE]` prints `@@ -12,2 +12,3 @@`
  hunks and exits 0 when the files are the same, 1 when they differ.
- **File → Compare with the saved file** shows exactly what you changed before Ctrl+S, and
  **Compare with the backup (.bak)** what the last save changed (with Keep backup on). Every
  comparison now reads the document as you see it, unsaved edits included, so the scrollbar
  marks and jumps are right on edited files; the panel says "(outdated, run again)" after
  further edits, and a save closes a comparison that holds the file open.
- **View → Compare side by side** (or **Side by side** in the compare panel) opens the other file
  read-only beside the document: removed lines tinted red with a − in the gutter, added lines
  green with a +, the changed middle of a changed line brighter, and a line where the other side
  has extra lines. The panes keep corresponding lines at the top as you scroll either one; the
  difference list stays below for jumping.
- **Save as .diff…** in the compare panel writes a unified diff (`diff -u`, 3 lines of context)
  of files of any size, streamed from both sides, that GNU patch and git apply accept; for the
  saved file and the backup it records what was changed. **File → Apply patch…** applies a
  .diff/.patch to the document in one pass as a single undo step: each hunk where it says, or
  up to 4,096 lines away when it matches one place only; the hunks it cannot place are listed.
- `bigtext diff A B` now prints a unified diff with context (`-U N`, default 3), `-q` only says
  whether the files differ, `-o OUT` writes it to a file.
- **Search → Lines only in this file…** lists the lines of one open file that occur nowhere in
  another, in the file's order whatever the other's, with how many are in both; white space,
  case, timestamps and a mask can be ignored as in Compare. The list jumps to each line, and
  Save lines… / Copy lines write them out. Any size: past 25 million distinct lines the
  comparison splits both files by hash into partitions on disk. On the command line:
  `bigtext only-in A B [-w] [-i] [-t] [--mask RE] [--count] [-o OUT]`.
- **Compare table by this column…** (a column header's menu) pairs each row with the other open
  table's row with the same key, whatever the order (the n-th duplicate with the n-th), and
  opens the rows changed, removed and added as a new read-only table: status, key, the changed
  columns, and the values with `old → new` where they changed. Columns pair up by header name,
  encodings and line endings may differ; tables of any size are split by key hash into 64
  buckets on disk. On the command line: `bigtext diff-table A B --key COL [--key-b COL] [--all]`.
- Fixed: a comparison where one file's last line lost its line break ("b\n" against "b")
  reported a difference in the middle of the line.

### Follow

- **Follow mode survives log rotation**, like `tail -F`: when the followed file is truncated and
  written again (copytruncate) or renamed away and created again, BigText reopens it and goes on
  from its end, with a banner offering to open the rotated file (`app.log.1`) and FOLLOW ×n in
  the status bar. The file is told apart by its identity (volume and file number), not its
  creation time, and a path that vanishes for a moment is not taken as deleted.
- A followed file is read in chunks instead of mapped, so the writer can truncate it on Windows
  and a shrinking file cannot crash BigText with SIGBUS on Linux and macOS.

### Files

- **Archives open**: a `.zip`, `.tar`, `.tar.gz` / `.tgz`, `.tar.zst`, `.tar.xz` or `.tar.bz2`
  lists its files in a picker (name, size, date; type to filter, arrows and Enter to open), and
  the file picked is unpacked into a temporary read-only copy like a `.gz`, titled
  `bundle.zip › app.log`. A compressed file inside the archive is unpacked as well, an archive
  with a single file skips the picker, and sessions reopen the same file of the archive. A
  `.tgz` used to open as one unpacked `.tar`; it now lists its files.
- **`.xz` and `.bz2`** files open directly (pure-Rust decoders; several streams back to back,
  as `xz` and `bzip2` write them, are read as one). Zip members compressed with Deflate64,
  bzip2, LZMA or xz can be read too.
- **Saving compressed**: when the name of a Save lines…, Export lines…, Save selection as…,
  table, diff or value export ends in `.gz`, `.zst`, `.xz` or `.bz2`, the file is compressed on
  the way out, with the same progress and Cancel, and the notice gives the compressed size.
  Split file… has *Compress the parts* (`app.part001.log.gz`, …). On the command line every
  `-o OUT` with such a name is compressed, and `split` takes `--gz`, `--zst`, `--xz`, `--bz2`.
- **Free space is checked first**: before a compressed file or an archive member is unpacked
  (its size estimated from the gzip or zstd header, the archive's listing, or 12× the packed
  size) and before a save writes its copy, BigText compares the size with the free space on
  that drive and asks *Continue* / *Choose folder…* / *Cancel* instead of failing halfway with
  "disk full". **Help → Unpacked copies folder…** puts unpacked copies (and other temporary
  files) on another drive; the setting is kept, and leftovers in both folders are cleaned up.
- **File → Properties…** (Ctrl+I) shows the size, how the file is read (mapped or in chunks),
  the encoding, whether and why it is read-only, where an unpacked copy came from and when the
  file changed, and fills in from one background pass: the exact line count, LF / CRLF / CR
  counts, the longest line, NUL bytes and bytes that are not UTF-8 (each with Go to), whether
  it ends with a line break, and the first and last time. The SHA-256 is computed on request,
  with a Copy button. `bigtext-cli stats` prints the new figures too.
- **Recovery journal for unsaved edits**: a couple of seconds after a pause in editing (or
  every 30 s of typing) a modified document's piece list and the bytes typed into it are
  journaled next to the settings — a few KB for a thousand edits to a 50 GB file, written on a
  worker. After a crash or a power cut the next start lists the documents with Restore (once
  the file's size and time show it unchanged), Save text as… and Discard. Each running BigText
  holds a lock so another one's journals are left alone; journals go when the edits are saved,
  undone or discarded and on a clean exit. File → Keep a recovery journal of unsaved edits
  turns it off.
- **Delete these** and **Keep only these** in the filter panel change the open document instead
  of writing a copy — drop every DEBUG line, or keep only the errors, a level-bar filter
  included. Up to 50,000 runs of lines are one undoable edit; beyond that the document is
  rewritten on disk (temp file, atomic swap, optional `.bak`) like Replace All. They are off
  while the filter shows lines around matches, a column or a time range.
- **Text operations on files of any size**: UPPERCASE, lowercase, trimming trailing white
  space, tabs ↔ spaces and Format / Minify JSON no longer stop at 64 MB. A larger document or
  selection streams through them — whole lines at a time, each piece checked to survive its
  encoding — into a new file that replaces the document on disk, after asking, with progress,
  Cancel and an optional `.bak`. The JSON formatter now works a piece at a time, and the
  command line runs the same code: `bigtext-cli transform --op upper|lower|trim|tabs|untabs`
  and `bigtext-cli format FILE --json pretty|minify`.
- **Edit → Text decodes and encodes**: Base64 (standard or URL-safe, padded or not), URL
  `%xx` (`+` as a space in `a=b&c=d` forms), hex (spaces, `:` and `0x` allowed), HTML entities
  (numeric and the common named ones) and JSON string escapes (surrogate pairs included), on the
  selection or, with nothing selected, the word at the caret. Decoding a JWT shows its header and
  payload pretty-printed; bytes that are not text (as a hex dump) and characters the document's
  encoding lacks open in a result window with Copy and Open in a new tab instead of being put in.
- **Column blocks from the keyboard**: Shift+Alt+arrows (Home, End, PageUp, PageDown;
  Ctrl+Shift+Alt+Home/End to the first and last row) make and grow a column block from the
  caret, as Alt+drag does. **Edit → Column editor…** fills the block's column with a number
  sequence (first number, step, at least N digits, hexadecimal) or the same text on every row
  — on the block's rows as one undo step, or on every line from there to the end of a file of
  any size, streamed into a new file that replaces it (optional `.bak`).
- **Sorting without a size limit**: Sort lines (A → Z, Z → A, by number) and Remove duplicate
  lines on a whole document over 64 MB now sort it through temporary runs of 64 MB and a k-way
  merge (two levels past 256 runs) and replace it on disk, after asking, with an optional
  `.bak`. **Edit → Text → Count duplicate lines…** opens each line with how often it occurs,
  most frequent first, as a new table, for files of any size. On the command line:
  `bigtext-cli sort [-r] [-n] [-i] [-u] FILE` and `bigtext-cli uniq [-c] FILE`.

### Command line

- **Script commands without a window**, in the new console program **`bigtext-cli`** (also
  `bigtext --filter …`): `filter` (grep-like, `-v -i -w -E`, to a file in the original
  encoding with `-o` or to the console), `levels ERROR,WARN`, `count`, `split --size 100MB` /
  `--lines N`, `stats` (size, lines, encoding, line endings, first and last time), `merge` (logs
  in time order) and `convert --to json|jsonl|md|xlsx|csv`. Exit codes: 0 done, 1 nothing
  matched, 2 error. They stream like the editor, so they work on files of any size.
  `bigtext-cli.exe` comes with the Windows zip and installer, since `bigtext.exe` is a windowed
  program that consoles do not wait for.

### Packages

- **Linux**: a `.deb` package (`bigtext`, `bigtext-cli`, a menu entry and the icon) and an
  **AppImage**; the tarball now has `bigtext.desktop` and the icon too. The window identifies
  itself as `bigtext`, so desktops show its icon.
- **macOS**: `BigText.app` (zipped, for the Applications folder) next to the plain binaries.
- **Homebrew**: a cask (the app) and a formula (built from source, Linux and macOS) ready for a
  tap; see `packaging/README.md`.
- Releases carry `SHA256SUMS.txt`.

### Tabs

- **Keyboard-only moving around**: **Ctrl+1 … Ctrl+8** go to that tab and **Ctrl+9** to the last,
  **Ctrl+PageDown / Ctrl+PageUp** step through the tabs in bar order, and **F4 / Shift+F4** go to
  the next / previous entry of the list under the text — Find All (with "Match 3 of 41"), the
  filter's matches or Find in files — around the ends. All can be rebound.
- **Ctrl+Tab** shows the tabs in most-recently-used order: hold Ctrl, press Tab (Shift+Tab back)
  to step, release Ctrl to switch, so a quick Ctrl+Tab goes back to the previous tab.
- **Pinned tabs** (right-click a tab → Pin tab, or File → Pin / unpin tab) stay first, have no
  close button and are kept by **Close other tabs** and **Close tabs to the right** (also in the
  tab's right-click menu, which can copy the file's path too). Tabs with unsaved changes are
  never closed by these. Pinned tabs are restored as pinned.
- **Drag a tab** to move it.
- **File → Sessions**: save the open files with their positions, bookmarks, pins and filters,
  and the highlights, under a name ("Incident 2026-10-28"), and open them again later in one
  click. Sessions are small text files next to BigText's settings.
- **Portable mode** (Help → Portable mode…): with a `BigText-data` folder next to `bigtext.exe`,
  the settings, recent files and sessions are kept there, so BigText runs from a USB stick with
  its settings. The button creates the folder and moves the settings and sessions in.
- **Help → Export settings… / Import settings…**: highlights, key bindings, saved searches,
  theme, font, language and view options in one file, to take to another computer (no file
  names or tabs). Importing adds highlights and saved searches and replaces the rest.
- **Edit → Copy file:line** (Ctrl+Shift+C) copies `C:\logs\app.log:1234` for the caret's line, to
  send to a colleague: `bigtext C:\logs\app.log:1234` opens it right there.

### Logs

- **Context around filter matches**: **±N** in the filter view's header shows N lines before
  and after each match, dimmed, with a rule between groups (like `grep -C`), and **Whole entry**
  keeps each log entry together — the stack trace under an ERROR line, or, for a match inside a
  trace, the entry's first line above it. The level bar, **Save lines…** and **Copy lines** use
  the same lines (files get `--` between groups); the header counts the matches and the lines
  around them; scrollbar marks stay on the matches. `bigtext-cli filter` and `levels` take
  `-A` / `-B` / `-C N`, `--entries` and `-n` (line numbers, `12:` a match, `13-` context).
- **Time range**: the search bar's **Time** button opens "From / To" ("14:30",
  "2024-05-01 14:30:00"; either may be empty, To includes its second) with **±30 s** and
  **±5 min around the caret** buttons. While a range is set, Filter, Find All and the level bar
  read only the lines between those times — found with two binary searches, so a five-minute
  window of a 20 GB log is filtered in a moment — and the filter header says "between 14:30 and
  14:35". `bigtext-cli filter`, `count` and `levels` take `--from` / `--to`.
- **Narrow a filter**: the filter view's **Narrow** row adds a condition on top of the current
  filter — lines that also have `timeout`, or (with **without**) that do not have
  `healthcheck` — instead of replacing it. Conditions show as chips (`ERROR › “timeout” ›
  ¬ “healthcheck”`); × removes one, the first chip goes back to the filter alone. Narrowing a
  finished filter reads only the lines it kept, so it is quick on millions of lines. Works on
  level-bar filters (changing the levels keeps the conditions) and followed ids; sessions keep
  the conditions. **Search → Narrow the filter by the search text** adds the search box's
  pattern. `bigtext-cli filter` / `count` take `-e PAT` (each must match; `--any`: one of them)
  and `--not PAT`, both repeatable.
- **Follow an id**: the request, trace, span or session ids on the caret's line
  (`request_id=r-42`, `"traceId": "4bf9…"`, `[req_id: 7]`, or a bare UUID) appear as chips in
  the status bar. Click one, or press **Ctrl+Shift+I** (which takes the selected text when there
  is one), to filter the file to that id with whole log entries; the filter header shows when
  the id was first and last seen, the time between, and how many of its lines are errors, with
  a button to highlight the id everywhere. **Search → Id patterns…** edits the list of keys
  (kept with the settings).
- **View → Log fields** (Ctrl+Shift+L): a side panel with the caret's JSON Lines or logfmt
  (`key=value`, `key="quoted"`) line as a key → value table that follows the caret. Nested
  objects become `user.id`, escaped values (`"line1\nline2"`) are shown unescaped. Each field's
  menu filters the file to the lines with that value or without it, highlights it, copies it,
  or **counts the field's values** over the whole file in one streaming pass into a table
  (value, lines, percent, first and last time), most frequent first. `bigtext-cli values FIELD
  FILE` prints the same table.
- **Search → Message patterns…**: groups a log's lines by template in one streaming pass —
  the leading timestamp dropped and numbers, hex ids, UUIDs, IP addresses and paths replaced
  by `<n>`, `<hex>`, `<uuid>`, `<ip>` and `<path>` — and lists the templates with their line
  counts and levels, **most lines first** (what floods the log) or **rarest first** (the one
  new message). Click a template to show only its lines; right-click to hide them instead, go
  to the first one or copy it; hover for a sample line and its first and last time. **Only
  the filtered lines** builds the list from the current filter; **Export CSV…** saves it.
  `bigtext-cli patterns FILE [--top N] [--rare]` prints it as CSV.
- **Search → Find time gaps…**: a list under the text of every place where the next line with a
  timestamp is at least N seconds (30 by default) later than the one before — the service was
  down or quiet — or earlier, when the clock went back; largest first or in file order, with
  both times and the line; click one to go there. A single line with a stray time (a time
  inside a message) does not make a gap. The status bar shows **Δ** — the time since the
  previous timestamped line — for the caret's line in logs. `bigtext-cli gaps FILE --min 5m`
  lists them with line numbers.
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

### JSON

- **File → Compare JSON with…** compares the JSON document with another JSON file by structure:
  `$.items[1234].price: 10 → 12`, `$.users[7].name: added "x"`, `$.gone: removed true` — key
  order and formatting do not count. Both are flattened into sorted `$.path = value` lines on
  disk and compared with the streaming line diff, so memory stays bounded; click a change to
  select it in the document. `bigtext-cli jsondiff A B` prints the changes (exit code 1 when the
  files differ).
- **View → JSON fields…** (also **Fields…** in the structure panel): reads a JSON Lines file —
  or the items of a JSON array — once and lists every key path to three levels
  (`user.address.city`, array items as `tags[]`) with its types, how often it appears, nulls, the
  minimum, maximum and mean of numbers, the number of distinct values and examples; filter the
  list, **Copy as JSON Schema** (a sketch with `required` for keys every record has) or **Export
  CSV…**. `bigtext-cli stats --fields FILE` prints the same as CSV.
- **View → Structure (JSON / XML)** (Ctrl+Shift+J) now also outlines XML: the element tree in the
  same side panel, opened element by element (200 at a time), each shown as `item id="42"` with
  its text, click to go there; paths like `/catalog/item[1234]/price` (1-based, as in XPath) jump
  to an element, and **Copy path at caret** gives the element's path. Only the opened element's
  children are read, so a 20 GB dump stays quick. Comments, CDATA, the prolog and DOCTYPE are
  skipped; mismatched tags are reported.
- **Search → Check JSON**: the file checked strictly in the background — bad escapes, control
  characters in strings, trailing commas, NaN and other words, numbers like `01` or `1.`,
  brackets that do not match, invalid UTF-8, text after the value — as one document (stopping at
  the first problem) or as JSON Lines (every bad line), chosen from the file and switchable. The
  problems are listed under the text with their line and byte; click to go there; "Valid JSON:
  12,345,678 values" otherwise. `bigtext-cli validate` checks JSON files too (`--json`,
  `--lines`) and exits with 1 on a problem.
- **File → JSON array → JSON Lines…** writes each item of a JSON array — the top-level one, or
  the first array of the top-level object (`{"events": [...]}`) — on a line of its own, streamed
  at any size; **File → JSON Lines → JSON array…** does the opposite, and **File → Open logfmt
  as table** turns `key=value` log lines into a table (sort, group, filter by conditions,
  export). `bigtext-cli convert` takes these too: `--to jsonl` for a JSON array (`--path
  $.events`), `--to json` for JSON Lines, `--to csv` for logfmt, with `--from` when a file is
  not what it looks like.
- **Filter JSON Lines by fields**: with the search bar's **{}** switch (or in a JSON file, a
  pattern starting with a dot) the filter takes a field query instead of a pattern —
  `.level == "error" and .duration > 500`, `.user.id = 42`, `.msg contains timeout`,
  `.path ~ "^/api/"`, `.err exists`, `.trace missing` — and lists the lines that meet it, with
  **!**, context lines, **Narrow**, Save lines and Copy lines as usual. Numbers compare as
  numbers; values are found without parsing whole lines, so it streams at disk speed.
  `bigtext-cli jsonl FILE -q QUERY [--fields a,b.c] [--to csv] [--count]` filters and picks
  fields in one pass.
- **View → JSON structure** (Ctrl+Shift+J): the outline of a JSON file in a side panel, opened
  node by node, 200 children at a time, so even a root array of millions of items opens at
  once. Click a node to go to its value. Type a path such as `$.items[1234].id` (or
  `items[3]["a b"]`) to jump there, and **Copy path at caret** gives the path of the value under
  the caret. Everything is read in a streaming pass in the background; nothing is held as a
  tree in memory.
- **View → Open JSON Lines as table**: a `.jsonl` / `.ndjson` file becomes a table with a column
  per key (keys from the first 1,000 lines, strings unescaped, nested values as JSON), so the
  table's sorting, statistics and filters work on it. It is converted into a temporary CSV in
  the background and opens read-only.

### CSV tables

- **Chart…** (a column header's menu): a panel under the table with how many rows have each
  value of the column — in ranges for numbers (which widen as needed, so outliers do not hide
  the rest), in periods for times, and the 50 most frequent values for text — filling in while
  the file is read. Click a bar to open **Filter rows by conditions…** set to its rows (or, for
  a period, to go to its first row).
- **Add column…** (a column header's menu): a new last column computed from each row by an
  expression — `{price} * {qty}`, `substr({ts}, 0, 10)`, `regex({url}, "id=([0-9]+)")`,
  `if({status} = "ok", 1, 0)` — previewed on the rows on screen as you type, with errors shown
  where they are. Arithmetic on numbers (an operand that is not a number gives an empty cell),
  `&` to join text, comparisons, `and` / `or` / `not`, and upper, lower, trim, len, substr,
  replace, regex, if, num, abs and round. Up to 64 MB it is one undo step; larger tables are
  rewritten on disk with the usual confirmation. `bigtext-cli derive FILE --name N --expr E`.
- **Pivot by this column…** (a column header's menu): a new read-only table with a row per value
  of that column — or per hour, day or month when it holds times — a column per value of another
  column, and in each cell the number of rows or the sum, mean, minimum or maximum of a third
  column, with a Total column; rows by time, or largest total first. `bigtext-cli pivot FILE
  --rows COL --cols COL [--value COL --agg sum] [--by day]` does the same.
- **File → Join tables…**: picks two open tables and a key column of each, and opens a new
  read-only table with each row of the first followed by the other columns of the lookup table's
  rows with the same key (all of them when there are several; empty fields, or with **Only the
  rows with a match** none, when there is none). The lookup table is held in memory (up to
  256 MB of values), the first one streams, so it can be of any size. `bigtext-cli join LEFT
  RIGHT --on COL` (or `--left-on` / `--right-on`, `--inner`) does the same.
- **Freeze and hide columns** (a column header's right-click menu): **Freeze columns up to here**
  keeps those columns on the left while the others scroll; **Hide from the view** leaves columns
  out of the table without touching the file, and the toolbar's "3 hidden · Show all" brings them
  back. Moving and deleting columns keep both in step.
- **Tables read their own layout**: Excel's `sep=;` first line sets the delimiter, and comment
  (`#`), blank and title lines before the table are skipped by the view, statistics, grouping,
  filters, checks and exports — and kept as they are by sorting, moving and deleting columns.
  **First row is header** is now decided from the data: a first row of numbers over rows of
  numbers is data. The toolbar's **Table…** menu sets **Skip lines before the table** and the
  **Quote** (`"` or `'`). `bigtext-cli convert` and `validate` detect the same and take
  `--header` / `--no-header`, `--skip N` and `--quote '`.
- **Check rows…** (in a column header's menu, and View → Check table rows…) streams over the table
  once and lists, under it, every record whose field count differs from the header's (or the
  usual one), with a stray quote or a quote that is never closed, with a control character, or
  with text in the columns typed into **Number columns** — "12 of 48,000,000 rows have
  problems". Click one to go there; **Save rows…** writes the header and those rows to a file.
  Quoted fields with line breaks are fine. `bigtext-cli validate FILE [--numeric 3,5]` prints
  `line: problem` and exits with 1 when something is wrong.
- **Group rows by this column…** (column header menu): the number of rows of each value, like
  `GROUP BY`, optionally with the sum, mean, minimum and maximum of another column; largest
  groups first, exportable to CSV.
- **Filter rows by conditions…**: several conditions on columns at once (contains, does not
  contain, is, is not, `>` and `<` on numbers, empty, not empty); the rows that meet all of
  them open as a new read-only table, which can be saved with Save As.
- **Export to other formats**: File → Export table as, or a column header's Export menu for some
  columns: **JSON** (an array of objects keyed by the header), **JSON Lines**, a **Markdown**
  table, or an **Excel workbook (.xlsx)** with numbers as numbers (`007` stays text). Streamed,
  so large tables export in the background; Excel's 1,048,576-row limit is reported.

### Search

- **Top values… and Save matches…** in the Find All results: count the different matched texts —
  or one capture group of a regular expression, picked by number or name — into a table, most
  frequent first (`grep -o | sort | uniq -c` in one pass; 100,000 different values at most), or
  save only the matched texts, one per line. `bigtext-cli extract [-g N] [--count] PATTERN FILE`
  does the same.
- **Back and forward** (Search → Back / Forward, **Alt+Left** / **Alt+Right**, or the mouse's
  side buttons): back to where the caret was before a jump — Go to, F3, F8, a bookmark, a
  bracket jump, a click in the filter, Find All, histogram, gaps, JSON or Find-in-files lists —
  or to the tab left, across tabs; the last 100 places are kept. In a text field Alt+arrows still
  move by words.
- **Go to** (Ctrl+G / Ctrl+Shift+G) also takes `50%` (a share of the file), `+1000` / `-250`
  (lines from the caret), `$` and `$-100` (from the last line); in byte-offset mode the same
  with bytes (`+0x100`, `$-4096`). Dragging the scrollbar shows `~Ln 45,357,933 · 48 %` next to
  the pointer.
- **Saved searches**: the ★ menu of the search bar keeps a pattern with its options, the **!**
  switch and the table column under a name ("Timeouts", "5xx responses"); one click applies it
  as a filter. They are kept across sessions.

### Editing

- **Multiple cursors.** **Ctrl+D** selects the word at the caret, and each further press adds
  the next occurrence of the selection as another cursor (searched in the background, so it
  never blocks on a large file). **Alt+click** adds a cursor where you click. Typing, pasting
  (one line per cursor when the counts match), Backspace, Delete, Enter and Tab change the text
  at every cursor as one undoable edit; Left/Right/Home/End move them all; copy joins their
  selections line by line; **Esc** goes back to one cursor. The status bar shows the count.
- **Duplicate line** moves to **Ctrl+Shift+D**, and can now be rebound.
- **Matching brackets and tags**: the `()`, `[]`, `{}` pair next to the caret is highlighted, and
  in XML / HTML the opening and closing tag names. **Ctrl+]** (Search → Go to matching bracket)
  jumps to the partner, or, away from a bracket, to the bracket around the caret. Brackets
  inside double-quoted strings do not count; the jump searches the whole file in the
  background, so it works from the first `[` of a gigabyte-long JSON array too.
- **View → Show whitespace and line breaks**: spaces as `·`, tabs as `→`, and each line's break
  as `↓` (LF), `↵` (CRLF) or `←` (CR), dimmed — to see which lines have tabs or mixed line
  endings.

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
