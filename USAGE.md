# Using BigText

The command line, the `bigtext-cli` commands for scripts and the keyboard shortcuts. Every
feature is described in [FEATURES.md](FEATURES.md).

## Command line

```
bigtext [OPTIONS] [FILE[:LINE] ...]

  FILE:LINE        open FILE at line LINE (FILE:LINE:COLUMN, as compilers and grep -n
                   print it, works too)
  FOLDER           show FOLDER's files in the side bar
  -                read standard input, following it as it grows
                   (e.g.  journalctl -f | bigtext -)
  -n, --line N     go to line N in the last file
  --find TEXT      search for TEXT (from the start, or from --line)
  --regex RE       search for a regular expression
  -f, --follow     follow the files as they grow and are rotated, like tail -F
  --new-window     open a window of its own (files otherwise open in the
                   BigText already running)
  -h, --help       show this help
```

## Commands for scripts

For scripts, **`bigtext-cli`** (in every download; `bigtext --filter …` works too) runs the
same streaming code without a window:

```
bigtext-cli filter [-v] [-i] [-w] [-E] PATTERN FILE [-o OUT]   lines with (-v: without) a match
bigtext-cli levels ERROR,WARN FILE [-o OUT]                    lines of these log levels
bigtext-cli count [-v] [-i] [-w] [-E] PATTERN FILE             number of matching lines
bigtext-cli extract [-E] [-g N] [--count] PATTERN FILE          only the matched texts (or counts)
bigtext-cli split (--size 100MB | --lines N) FILE [--out-dir DIR]
bigtext-cli grep [-i] [-w] [-E] PATTERN FILES... | -r DIR      find in files (file:line:text)
bigtext-cli head [-n N] FILE / tail [-n N] FILE                the first / last N lines
bigtext-cli replace [-E] PATTERN REPLACEMENT FILE [--in-place [--bak]]   Replace All, streamed
bigtext-cli saved / filter --saved NAME [--settings FILE]     searches saved in the app
bigtext-cli slice FILE --lines A-B | --bytes A-B | --from T --to T   a part, by binary search
bigtext-cli diff A B [-w] [-i] [-t] [--mask RE] [-U N] [-q]    a unified diff
bigtext-cli only-in A B [-t] [--mask RE]                       lines of A nowhere in B
bigtext-cli diff-table A B --key COL                           tables compared by key
bigtext-cli sort [-r] [-n] [-i] [-u] FILE / uniq [-c] FILE     any size, through temp runs
bigtext-cli transform --op upper|lower|trim|tabs FILE          text operations, streamed
bigtext-cli format FILE --json pretty|minify                   JSON formatting, streamed
bigtext-cli stats FILE...                                      size, lines, encoding, first/last time
bigtext-cli merge FILE FILE... [-o OUT]                        logs merged by time
bigtext-cli convert FILE --to json|jsonl|md|xlsx|csv [-o OUT]  tables (and JSON Lines → CSV)
bigtext-cli validate FILE [--no-header] [--numeric 3,5]        malformed CSV / TSV rows
bigtext-cli csv FILE cut|drop COLS | sort --by COL | where COND... | stats COL | group COL
bigtext-cli values FIELD FILE [-o OUT]                         a JSON Lines / logfmt field's values
bigtext-cli gaps FILE [--min 30s]                              time gaps, with line numbers
bigtext-cli patterns FILE [--top N] [--rare] [-o OUT]          message templates with counts
```

`--json` gives one JSON record per line on filter, levels, grep, count, stats, replace and saved
(`{file, line, offset, text}`, errors as `{"error": …}`).
`filter`, `count` and `levels` take several files, `*.log` patterns (expanded on Windows too) and
`-r DIR --names '*.log;*.gz'`, printing `file:` before each line (count: per file and a total),
or `--out-dir DIR` for one output per input; compressed inputs are unpacked on the way.
`filter` and `count` take more patterns: `-e PAT` (each must match, or one of them with `--any`)
and `--not PAT`. `filter` and `levels` take `-A N` / `-B N` / `-C N` (lines after / before / around each match),
`--entries` (whole log entries) and `-n` (line numbers, `12:` a match and `13-` context), with
`--` between groups like grep, and `--from TIME` / `--to TIME` (only the lines between two
times). Exit codes: 0 done, 1 nothing matched, 2 error.

## Keyboard shortcuts

| Shortcut | Action |
|---|---|
| Ctrl+O / Ctrl+N | Open / new |
| Ctrl+Shift+O / Ctrl+Shift+E | Open a folder in the side bar / show or hide the side bar |
| Ctrl+P | Go to file in the open folder (`name:42` opens at line 42) |
| Ctrl+S / Ctrl+Shift+S | Save / save as |
| Ctrl+W | Close tab |
| Ctrl+1 … Ctrl+8, Ctrl+9 | Tab 1 … 8, the last tab |
| Ctrl+PageDown / Ctrl+PageUp | Next / previous tab in the bar |
| F4 / Shift+F4 | Next / previous entry of the list under the text (Find All, filter, Find in files) |
| Ctrl+Tab / Ctrl+Shift+Tab | Tabs in most-recently-used order (hold Ctrl, release to switch) |
| Ctrl+F / Ctrl+H | Find / replace (Enter = next, Shift+Enter = previous, Esc = close) |
| F3 / Shift+F3 | Find next / previous |
| Ctrl+Shift+F | Filter: show only the lines with a match |
| Ctrl+Alt+F | Find in files |
| F8 / Shift+F8 | Next / previous error line (or line of the levels shown) |
| Ctrl+Shift+H | Time histogram |
| Ctrl+Shift+I | Follow the id at the caret (or the selected text) |
| Ctrl+Shift+V | Paste from the history of copied texts |
| Ctrl+I | File properties (lines, line breaks, longest line, SHA-256) |
| Ctrl+Shift+L | Log fields of the caret's line |
| Ctrl+] | Go to the matching bracket or tag |
| Ctrl+Shift+J | JSON structure |
| Ctrl+Shift+C | Copy file:line |
| Ctrl+Shift+P | Command palette |
| Alt+Left / Alt+Right | Back to where the caret jumped from / forward again (also the mouse's side buttons) |
| Ctrl+G / Ctrl+Shift+G | Go to line / go to byte offset (decimal or `0x` hex; also `50%`, `+100` / `-100` from the caret, `$-100` from the end) |
| Ctrl+Shift+T | Go to time (first log line at or after a timestamp) |
| Ctrl+Alt+T | Go to this time in all tabs |
| Ctrl+Shift+R | Run a command on the selection |
| Ctrl+Shift+{ / Ctrl+Shift+} | Fold / unfold the object, array or element at the caret |
| Ctrl+Z / Ctrl+Y (Ctrl+Shift+Z) | Undo / redo |
| Ctrl+X / Ctrl+C / Ctrl+V | Cut / copy / paste |
| Ctrl+A | Select all |
| Ctrl+D | Select the word, then add its next occurrence as another cursor |
| Alt+click | Add a cursor (Esc: back to one cursor) |
| Ctrl+Shift+D / Ctrl+Shift+K | Duplicate line (or selection) / delete line |
| Ctrl+/ | Toggle comment on the selected lines (or the caret's) |
| Ctrl+←/→, Ctrl+Backspace/Delete | Word movement / delete word |
| Home / End, Ctrl+Home / Ctrl+End | Line start (smart) / end, document start / end |
| Ctrl+wheel, Ctrl+= / Ctrl+- / Ctrl+0 | Zoom |
| Alt+Z | Toggle word wrap |
| Ctrl+F2 / F2 / Shift+F2 | Toggle / next / previous bookmark |
| Ctrl+\\ | Split view |
| Double / triple click | Select word / line |
| Alt+drag, Shift+Alt+arrows | Column (block) selection |
| F12 | Performance overlay (frame time, private memory, index size) |

View → Hex view, View → Follow (tail), View → Syntax highlighting, File → Open recent,
File → Compare with…, File → Reopen with encoding, File → Line endings on save,
File → Keep backup (.bak) on save, Edit → Read-only, Help → About.
