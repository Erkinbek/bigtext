# BigText features

Everything BigText does, in detail. The [README](README.md) has the overview, the downloads and a
comparison with other editors; [USAGE.md](USAGE.md) has the command line, `bigtext-cli` and the
keyboard shortcuts.

- **Open anything**: File → Open, drag & drop, command line (`bigtext file.txt`), Windows
  "Open with" (registered by the installer), multiple tabs.
- **Never reads the whole file**: a small cache of memory-mapped views for the visible text,
  positional streaming reads for background work; network/removable drives (or mmap failure)
  fall back to buffered chunked reads served from a cache by a loader thread, so a slow drive
  can never stall the UI.
- **Sparse, incremental line index**: one `u64` per 64 KB, built in the background from the top.
  Go to line and the scrollbar work immediately (estimated positions, shown with `~`, refined
  when the index catches up).
- **Compressed files** (`.gz`, `.zst`, `.xz`, `.bz2`, also multi-member/multi-frame as log
  rotators write them): unpacked once, streaming, into a temporary copy that opens as soon as
  its first megabyte is there; the rest appears as it is unpacked (progress and Cancel in the
  status bar, which keeps what is unpacked). The copy is read-only; Save becomes Save As. It
  is deleted when the tab closes, and the session remembers the compressed file.
  Before unpacking (and before a save) BigText compares the size with the free space and asks
  first when it would not fit; **Help → Unpacked copies folder…** puts the copies on another drive.
- **Archives** (`.zip`, `.tar`, `.tar.gz` / `.tgz`, `.tar.zst`, `.tar.xz`, `.tar.bz2`): a picker
  lists the files with their sizes and dates (filter, arrows, Enter), and the one picked opens
  as `bundle.zip › app.log`; a compressed file inside (`app.log.gz`) is unpacked too. An archive
  with one file opens straight away, and the session remembers which file was open.
- **Compressed output**: Save lines…, Export lines…, Save selection as…, the table and diff
  exports write `.gz`, `.zst`, `.xz` or `.bz2` when the name ends so (streaming, with progress
  and Cancel); Split file… can compress its parts; on the command line `-o out.gz` and
  `split --gz`.
- **Encodings**: BOM, UTF-8, UTF-16 LE/BE (with or without BOM), heuristic Windows-1251 /
  Windows-1252 / ISO-8859-1; File → Reopen with encoding. Invalid bytes show as U+FFFD, control
  characters and NUL as dim placeholders. **File → Save with encoding** writes a copy in
  UTF-8 (with or without BOM), UTF-16 LE/BE, Windows-1251/1252 or ISO-8859-1, streaming
  (with the line-ending conversion if one is chosen); characters the target cannot represent
  become `?` and are counted in the message. Binary-looking files open as text with a one-click
  **hex view**.
- **Line endings**: LF / CRLF / CR / Mixed detection in the status bar, optional conversion on
  save.
- **Status bar you can click**: Ln/Col goes to a line, the offset to a byte offset, the encoding
  reopens in another one, the line ending picks the one to save with, READ-ONLY turns it off;
  right-click the bar (or …) to choose its fields, kept with the settings.
- **Huge lines**: a single 2 GB line, minified JSON, base64 — lines longer than 64 KB are
  soft-broken (`↪` in the gutter) so every row computation is bounded; horizontal
  virtualization renders only the visible slice.
- **Search** (Ctrl+F): plain text or regex (linear-time `regex-automata`), case sensitive,
  whole word, multiline (dot matches newline), search in selection. Streams over the mmap on a
  worker with a live counter and cancel; F3 / Shift+F3 work before any full search finishes;
  "Find All" results in a virtualized list (capped at 1,000,000 with a "more exist" note), with
  **Top values…** (the different matched texts or a capture group counted, most frequent first,
  like `grep -o | sort | uniq -c`) and **Save matches…** (only the matched texts);
  matches highlighted in the viewport only. Non-UTF-8 files are searched by transcoding the
  pattern (literal) or decoding windows (regex) with exact byte mapping. Matches spanning
  chunk or piece boundaries are found.
- **Filter view** (Ctrl+Shift+F, or **Filter** in the search bar): only the lines with a match
  — or, with **!**, the lines without one — in a virtualized list under the text, like `grep`
  over the whole file. **±N** in its header adds N dimmed lines before and after each match
  (`grep -C`), and **Whole entry** keeps a log entry together: the stack trace under an ERROR
  line, or the entry's first line above a match inside a trace. The **Narrow** row adds
  conditions on top (`timeout`, then without `healthcheck`), shown as chips that can be removed;
  narrowing a finished filter reads only its lines. Click a line to jump to it.
  **Save lines…** streams every kept line to a
  new file (no limit); **Copy lines** puts them on the clipboard (up to 100 MB). Find All
  results offer the same two buttons. **Delete these** / **Keep only these** change the document
  itself (drop every DEBUG line, keep only the errors): one undo step up to 50,000 runs of
  lines, beyond that rewritten on disk like Replace All, with an optional `.bak`.
- **Highlights** (View → Highlights…, or **Mark** in the search bar): patterns painted in their
  own colours in every document — `ERROR` in red, a request id in blue — kept across sessions.
  Selecting a word (double click) also highlights its other occurrences on screen.
- **Scrollbar marks**: Find All matches, filter lines, bookmarks and the differences from
  File → Compare are marked on the vertical scrollbar, so you see where they cluster in a 50 GB
  file at a glance.
- **Logs in step**: **Search → Go to this time in all tabs** (Ctrl+Alt+T) moves every other open
  log to the caret line's time (marked ◆; UTC offsets are converted when both logs give them), and
  **View → Link scrolling with ▸** shows two logs side by side, the second following the first
  by time (with a skew in the status bar), line number or position in the file.
- **Go to time** (Ctrl+Shift+T): type `14:32:05` or `2024-05-01 14:32:05` to jump to the first
  log line at or after that time. A binary search over byte offsets reads only a few hundred
  KB, so it is instant even before indexing finishes. Recognises ISO 8601, `DD.MM.YYYY`,
  `MM/DD/YYYY`, Apache `[01/May/2024:14:32:05`, syslog `May  1 14:32:05` and bare times, with
  milliseconds. When a selection spans timestamped lines, the status bar shows the time
  between its first and last line (**Δt**).
- **Search history**: the ▾ button next to the find field, or Up/Down inside it, brings back
  recent patterns (kept across sessions).
- **Replace** (Ctrl+H): replace current / replace all with `$1`, `${name}`, `\n`, `\t`. Up to
  50,000 matches are applied as one undoable piece-table edit; beyond that the result is
  streamed to a temp file and swapped in atomically (optional `.bak`), with bounded memory and
  progress/cancel. Preview of the first 200 replacements.
- **Editing**: piece table over the mmap + add buffer; insert/delete, cut/copy/paste (copies
  above 100 MB ask first and run on a worker), undo/redo, multi-line selection, duplicate line
  (zero-copy), delete line, auto-indent, large-file-safe Select All.
- **Copy as Markdown / HTML** (Edit → Copy as): the selection as a fenced code block with its
  language and a `file:line` caption, or as HTML in the theme's syntax colours (with the plain
  text beside it) for mail, wikis and documents.
- **Paste from history** (Edit, Ctrl+Shift+V): the last 30 texts copied in BigText (up to 1 MB
  each), to paste again with ↑ ↓ and Enter; kept across restarts only when you tick it.
- **Show in folder**, **Open terminal here** and **Open with the default app** (File menu and a
  tab's right-click menu; for an unpacked copy, the compressed file it came from); the notice
  after a save or an export has a Show in folder link, and an exported `.xlsx` opens in the
  spreadsheet app.
- **One window**: opening a file while BigText runs (`bigtext app.log:120`, Explorer's Open with, a
  grep result) opens it as a tab of the running window, at its line, and brings that window to
  the front; `--new-window` starts another one. (On macOS only terminal launches are handed
  over.)
- **Recovery journal**: a couple of seconds after a pause in editing, the edits of a modified
  document (its piece list and the bytes typed — a few KB for a thousand edits to a 50 GB file)
  are written next to the settings; after a crash or a power cut the next start offers
  Restore / Save text as… / Discard, once the file on disk is checked to be unchanged
  (File → Keep a recovery journal of unsaved edits; the journal holds typed text in plain form).
- **Crash-safe save**: stream to a temp file in the same folder → fsync → atomic replace
  (`ReplaceFileW` on Windows, keeps attributes/ACLs). Any failure leaves the original intact;
  "file in use" / permission errors produce clear messages. Save As, optional `.bak` backup
  (File → Keep backup), unsaved-changes prompts. Closing the window during a save waits for a
  clean cancel or for the atomic swap to finish.
- **Status bar**: line and column of the caret, byte offset, the number of lines (exact once
  indexing has finished, `~` before), and for a selection its lines, characters (up to 4 MB)
  and bytes, the time it spans in a log (Δt) and the width of a block selection.
- **Screen readers**: menus, dialogs, the search bar and lists go to screen readers through
  AccessKit; the text view is a multi-line text field with the rows on screen, the caret, the
  selection and "Ln N, Col M", and finished work (file opened, saved, matches found) is announced.
- **Themes**: View → Theme offers Dark, Light, High contrast dark and High contrast light, and
  **Colour-blind-safe colours** (the Okabe-Ito palette) for everything BigText paints itself:
  syntax and log levels, highlights, merged logs, scrollbar marks, search hits, the histogram and
  the status bar. **View → Interface scale** (100–200 %) enlarges menus and dialogs, leaving the
  text at its own size.
- **Full screen and zen mode**: F11 for full screen; View → Zen mode (Ctrl+Shift+F11) hides the
  menu, tab, level and status bars (the menu comes back at the top edge, Esc leaves); View → Line
  numbers turns the gutter off in the text and table views.
- **Other**: line numbers, zoom (Ctrl+wheel), word wrap (off by default; View → Wrap at the window
  width or a column, View → Ruler at column), light/dark theme,
  read-only toggle (auto for read-only files), external change detection with Reload, and a
  **File → Properties…** (Ctrl+I): size, backing, encoding, origin, and from one background pass
  the exact line count, line breaks by kind, the longest line, NUL and non-UTF-8 bytes (with Go
  to), the first and last time, and on request the SHA-256 (`bigtext-cli stats` prints them too).
  **follow (tail) mode** for growing logs that resumes indexing where it stopped. Like
  `tail -F` it survives log rotation: when the file is truncated (copytruncate) or renamed away
  and created again, it goes on in the new file, with a button to open the rotated one
  (`app.log.1`); the status bar shows FOLLOW ×n.
- **Column (block) selection**: Alt+drag, or Shift+Alt+arrows (Home/End, PageUp/PageDown; with
  Ctrl too), selects a rectangle. Copy copies it line by line; typing, Backspace, Delete and Tab
  change every row at once (one undo step); pasting as many lines as the block has rows puts one
  on each row. **Edit → Column editor…** fills the column with a number sequence (start, step,
  zero padding, hex) or the same text, on the block's rows or down to the end of the file as a
  rewrite on disk. Works without word wrap, up to 10,000 rows in the editor.
- **Text operations** (Edit → Text) on the selection, or on the whole document up to 64 MB:
  sort lines (A→Z, Z→A, by leading number), reverse, remove duplicate lines, UPPER / lower
  case, trim trailing whitespace, tabs ↔ spaces, and JSON pretty-print / minify (token by
  token: key order, numbers and escapes stay as written; JSON Lines stay one value per line).
  Runs on a worker and is one undo step. Above 64 MB, case, trimming, tabs and JSON formatting
  stream through the whole file instead and replace it on disk (optional `.bak`); the same code
  runs as `bigtext-cli transform --op upper|lower|trim|tabs|untabs` and `format --json`.
- **Sorting files of any size**: with nothing selected, Sort lines and Remove duplicate lines on a
  document over 64 MB sort it through temporary runs and a k-way merge and replace it on disk;
  **Count duplicate lines…** opens each line with its count, most frequent first, as a table.
  `bigtext-cli sort [-r] [-n] [-i] [-u]` and `uniq [-c]` do the same.
- **Run a command on the text** (Edit → Run command on selection…, Ctrl+Shift+R): `sort | uniq
  -c`, `jq .` or `python fix.py` gets the selection or the document on its standard input, and
  its output replaces the selection (one undo step), opens in a new tab or only its exit code is
  shown; both stream through pipes and a temporary file, with Cancel in the status bar. The shell
  is cmd, Git Bash or WSL on Windows. Saved commands become **Edit → External tools** with
  `{file}`, `{dir}`, `{line}`, `{col}` and `{selection}` filled in (`code -g "{file}:{line}"`).
- **Decode and encode** (Edit → Text): Base64 (either alphabet; a JWT shows its header and payload),
  URL `%xx`, hex, HTML entities and JSON string escapes, on the selection or the word at the
  caret. A result that is not text for the document (bytes, characters its encoding lacks) opens
  in a window with Copy and Open in a new tab instead of replacing anything.
- **Font** (View → Font…): the monospaced fonts installed on the system (Consolas, Cascadia,
  JetBrains Mono, DejaVu Sans Mono, Menlo, …) or any `.ttf`/`.otf`/`.ttc` file, with a preview
  and a warning for fonts that are not monospaced; the built-in font stays as the fallback
  for missing characters. Kept across sessions.
- **Interface languages**: English, Russian and Uzbek (View → Language), kept across sessions.
  A unit test checks that every interface text has both translations with the same
  placeholders. Error details that come from the operating system stay in English.
- **Command palette** (Ctrl+Shift+P): type a few letters of any command (menus, views, text
  operations) and press Enter. **Help → Keyboard shortcuts** lists every command with its key;
  click a key and press a new one to change it (a key taken from another command is moved,
  with a note). Changes are kept across sessions.
- **Bookmarks** (Ctrl+F2, F2 / Shift+F2) that stay on their lines while you edit.
- **Split view** (Ctrl+\\): two panes over one document with independent scrolling.
- **Syntax highlighting** for logs, JSON, XML/HTML, CSV/TSV, SQL and INI/TOML/YAML, and for
  code in C/C++, C# and Java: keywords, strings, numbers, comments, types, function calls,
  constants, annotations and preprocessor lines — computed only for the visible slice of the
  visible rows; View → Highlight as overrides the extension. Comments, strings and tags that
  span lines are followed: each row starts in the state the row above left (remembered while
  you scroll), and after a jump the 64 KB above the screen are read to find it, starting at a
  line that begins a declaration.
- **CSV / TSV table view**: `.csv` and `.tsv` files open as a table (View → Table view or
  Ctrl+T switches any file between table and text). The delimiter (comma, semicolon, tab, pipe)
  is detected from the first lines and can be changed in the toolbar; quoted fields follow
  RFC 4180. The first row is a pinned header, numbers align right, columns fit their content
  (drag a header border to resize, double-click it to fit again). Like the text view it reads
  only the rows on screen, so a 10 GB CSV scrolls as fast as a small one. Click a cell to put
  the caret there, double-click it to edit it in the text view.
- **Delete CSV columns**: click column headers to select them (Ctrl+click adds, Shift+click
  selects a range), then **Delete columns** (or the Delete key) removes them and their values
  from every row, after a confirmation that names them. Files up to 64 MB change as one undoable
  edit (Ctrl+Z); larger files are rewritten on disk like Replace All (temp file, atomic swap,
  optional `.bak`) in one streaming pass with bounded memory.
- **Export and split** (File menu): **Export lines…** writes a line range (say 100,000–200,000)
  to a new file, **Save selection as…** writes the selection, and **Split file…** cuts the
  document into `name.part001.ext`, `name.part002.ext`, … of a given size in MB or number of
  lines, always at line breaks (UTF-16 parts each get the byte order mark). All of them
  stream on a worker with progress and Cancel in the status bar.
- **CSV column tools** (right-click a column header in the table view): sort rows ascending or
  descending (numbers numerically, text ignoring case, empty values last, header kept; files
  up to 1 GB), **Statistics…** (rows, empty, distinct values, numeric sum/mean/min/max and the
  most frequent values, in one streaming pass), **Filter rows by this column** (the filter
  then only counts matches inside that field), **Rename…**, **Move left/right**, and **Export**
  the column or the selected columns to a new CSV. Like Delete columns, changes to files up
  to 64 MB are one undo step; larger files are rewritten on disk (temp file, atomic swap,
  optional `.bak`).
- **Compare files** (File → Compare with…): streaming line diff of two huge files with bounded
  memory; identical stretches are skipped at memory speed (1 GB vs 1 GB with two changed lines:
  0.6 s, ~2 MB of memory). Click a difference to jump there in either file; F7 / Shift+F7 step
  through them; **Compare with tab ▸** picks another open file; Swap A↔B turns it round.
  It can ignore white space, case, timestamps and a mask regex (`bigtext diff -w -i -t --mask`).
  Unsaved edits are compared too; **Compare with the saved file** shows what you changed
  before Ctrl+S, **Compare with the backup (.bak)** what the last save changed.
  **View → Compare side by side** shows the other file beside the document: removed lines red
  with −, added green with +, the changed part of a changed line brighter, and the two panes
  scroll together (the pane under the pointer leads).
  **Save as .diff…** in the compare panel writes a standard unified diff (3 lines of context,
  streamed, any size); **File → Apply patch…** applies a .diff/.patch to the document as one undo
  step, finding a hunk up to 4,096 lines away when it matches only one place there and listing
  the hunks it could not place.
- **Lines only in this file…** (Search): the lines of one open file that occur nowhere in another,
  whatever their order, listed below the text with jumps, Save lines… and Copy lines; ignore
  timestamps and mask ids to compare two runs of a job. Works on files of any size (the other
  file's line hashes spill to disk past 25 million). `bigtext only-in A B -t --mask RE`.
- **Log tools**: a **level bar** above logs (ERROR / WARN / INFO / DEBUG lines in the filter view,
  F8 / Shift+F8 to the next / previous error); **Search → Time histogram** (Ctrl+Shift+H), a bar
  chart of lines and errors per second, minute, hour or day, click a bar to go there;
  **File → Merge logs by time…**, several logs in timestamp order with each line labelled in its
  file's colour. Timestamps: ISO 8601 with time zones, Unix seconds / milliseconds, syslog,
  Apache, Java and .NET formats with AM/PM; hovering a Unix time shows it as a date.
- **Follow an id**: request, trace, span and session ids on the caret's line (`request_id=…`,
  `"traceId": "…"`, bare UUIDs) show as chips in the status bar; a click or Ctrl+Shift+I filters
  the file to that id's whole log entries and shows when it was first and last seen, how long it
  took and how many errors it had. **Search → Id patterns…** edits the keys.
- **Log fields** (View, Ctrl+Shift+L): the caret's JSON Lines or logfmt (`key=value`) line as a
  key → value table that follows the caret (nested objects as `user.id`, escaped stack traces
  shown as lines). Each field can filter the file to its value or away from it, highlight it,
  or **count its values** over the whole file into a table (lines, share, first and last time).
- **Time range**: the search bar's **Time** button sets "from 14:30 to 14:35" (or ±30 s / ±5 min
  around the caret's line); Filter, Find All and the level bar then read only those lines, found
  with two binary searches instead of a full pass.
- **Message patterns** (Search → Message patterns…): the log's lines grouped by template —
  timestamps dropped, numbers, hex ids, UUIDs, IPs and paths replaced by `<n>`, `<hex>`,
  `<uuid>`, `<ip>`, `<path>` — with counts and levels, most lines first (the noise) or rarest
  first (the anomalies); a click shows only that template's lines.
- **Time gaps** (Search → Find time gaps…): every place where the next timestamp is at least N
  seconds later — or earlier, when the clock went back — largest first; click one to go there.
  The status bar shows **Δ** since the previous timestamped line for the caret's line.
- **Find in files** (Search, Ctrl+Alt+F): the pattern in every `*.log` (or any pattern) file of a
  folder and its sub-folders, listed as `file:line`; click to open the file there.
- **Saved searches** (★ in the search bar): a pattern with its options, `!` and column under a
  name, applied as a filter with one click.
- **Multiple cursors**: Ctrl+D adds the next occurrence of the selection, Alt+click adds a
  cursor; typing, paste, Backspace, Delete, Enter and Tab change the text at all of them.
- **Matching brackets and tags** highlighted next to the caret; Ctrl+] jumps to the partner.
  **View → Show whitespace and line breaks** shows spaces, tabs and LF / CRLF / CR.
- **JSON and XML**: **View → Structure** (Ctrl+Shift+J), an outline opened node by node (fine for a
  root array of millions of items, or an XML element tree), jumping to a path like `$.items[1234].id`
  (or `/catalog/item[1234]/price`) and copying the
  path at the caret; **View → Open JSON Lines as table**.
- **Folding**: View → Folding ▸ Fold (Ctrl+Shift+{) folds the object, array or XML element at
  the caret into one row with a "… 1,234 lines …" chip (a click opens it again); Fold all at
  level 1 or 2 folds a JSON document's values at that depth. The ranges are found in the
  background, so folding the root of a multi-GB array does not block.
- **JSON path in the status bar**: `$ › items › [1234] › city` for the value at the caret, live
  as it moves; a part jumps to that parent, a right-click copies the path. Deep in multi-GB files
  it is found from checkpoints one background pass records every 8 MB.
- **Field queries over JSON Lines** (the search bar's **{}**): `.level == "error" and
  .duration > 500` in the filter view (`bigtext-cli jsonl -q`).
- **Convert**: a JSON array to JSON Lines and back, and logfmt (`key=value`) logs to a table
  (File menu, `bigtext-cli convert`).
- **Check JSON** (Search): strict checking of a JSON document or of JSON Lines line by line, with
  the problems listed and clickable (`bigtext-cli validate`).
- **JSON fields…** (View): every key path of JSON records with types, counts, nulls, number
  ranges and examples; copy as a JSON Schema sketch (`bigtext-cli stats --fields`).
- **Compare JSON with…** (File): two JSON files compared by structure, key order and formatting
  ignored (`bigtext-cli jsondiff`).
- **Tables find their header**: Excel's `sep=;` line, comment and title lines before the table
  are skipped (and kept as they are by sorting and other rewrites), and a table of numbers is
  shown without a header row. The toolbar's **Table…** menu sets the lines to skip and the quote
  (`"` or `'`).
- **Freeze and hide columns** in tables (a header's right-click menu): frozen columns stay on the
  left while the rest scroll; hidden ones leave the view, not the file.
- **Join tables** (File → Join tables…): the rows of one table with the matching rows of another,
  by a key column, as a new table (`bigtext-cli join`).
- **Compare tables by key** (a column's menu → Compare table by this column…): two tables paired
  by a key whatever their row order; the rows changed (with the changed columns and
  `old → new` values), removed and added open as a new table. Columns pair up by header name;
  any size, through hash buckets on disk (`bigtext-cli diff-table A B --key id`).
- **Pivot tables** (a column's menu → Pivot by this column…): rows by one column (or by hour, day
  or month), columns by another, counts or sums in the cells (`bigtext-cli pivot`).
- **Add column…** from an expression over the row (`{price} * {qty}`, `substr({ts}, 0, 10)`,
  `if(…)`, `regex(…)`), previewed as you type (`bigtext-cli derive`).
- **Chart…** of a column: the distribution of its numbers, times or most frequent texts as bars
  under the table; a bar filters the rows to it.
- **Check rows…** (a column's menu, or View → Check table rows…): every record with another field
  count, a stray or unclosed quote, a control character or text in a number column, listed under
  the table with its line; **Save rows…** writes them to a file.
- **More table tools**: **Group rows by this column…** (counts, with sums of another column),
  **Filter rows by conditions…** (several conditions at once), and export as JSON, JSON Lines,
  Markdown or Excel (.xlsx).
- **Tabs and sessions**: Ctrl+Tab in most-recently-used order, pinned tabs, close other tabs /
  tabs to the right, drag to move; **File → Sessions** saves the open files with positions,
  bookmarks, pins, filters and highlights under a name; **Edit → Copy file:line**
  (Ctrl+Shift+C) for `bigtext path:line`.
- **Recent files and session restore**: tabs, positions, bookmarks and settings come back on
  the next start (state in `%APPDATA%\BigText\state.txt` / `~/.config/bigtext`). **Portable
  mode** keeps them in a `BigText-data` folder next to the program instead (Help → Portable
  mode…), and **Help → Export / Import settings** carries highlights, key bindings, saved
  searches and the look to another computer.
- **Per-file memory**: an encoding chosen with Reopen with encoding, Highlight as, table, hex,
  follow, read-only and wrap, and a table's delimiter, header switch, dragged column widths and
  scroll come back whenever the file opens again (Open recent, a session, the command line).
- **Updates**: once a day BigText asks GitHub for a newer release (Help → Check for updates
  does it on demand; Help → Check for updates automatically turns it off). A dialog shows the
  release notes of every newer version and installs nothing until you choose **Update now**;
  **Skip this version** stops the reminders until the next release. The download is checked
  against its size and the SHA-256 that GitHub publishes. An installed copy runs the new
  installer silently when BigText closes; a portable copy (zip, tar.gz) swaps its executable in
  place. **Restart now** reopens your tabs in the new version. A `cargo install` copy shows the
  command to run instead.

## Known limitations

- Regex matches are exact up to 1 MB of length across window edges; longer matches are found
  by re-searching with a growing window, capped at 256 MB (longer matches are reported
  truncated). A match longer than 1 MB that only ends past the overlap may be reported from a
  later start than strict leftmost-first semantics would give.
- Column numbers inside soft-broken lines (> 64 KB) are byte-based estimates (`Col ~N`).
- A CR-only line break is recognised only in files without LF line breaks (mixed CR is shown
  as a `←` placeholder).
- Wide (CJK) characters are drawn by the font but laid out as one cell; the default fonts do not
  include CJK glyphs.
- On Linux/macOS, another process truncating a file while it is mapped can raise SIGBUS
  (inherent to mmap; Windows forbids truncating mapped files). Followed files and network
  drives use chunked reads, so a followed log can be truncated safely.
- Syntax highlighting never reads a file from its start: after a jump it looks 64 KB above the
  screen, so a comment or string opened further up than that is not seen until you scroll
  through it from above.
- The CSV table shows one line per row, so a quoted field that contains line breaks appears as
  several rows (the text is unchanged). Deleting columns reads whole records and handles such
  fields correctly.
- The file comparison compares lines by 64-bit hash + length (a collision would hide a
  difference; astronomically unlikely) and re-synchronises within 4,096 lines / 16 MB. It
  reads the whole document again however small the edit, and a comparison does not follow
  later edits (the panel says when it is outdated).
