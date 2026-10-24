# Task: Build a fast, non-freezing large-file text editor (working name: "BigText")

## Goal
Build a desktop text editor whose single most important property is that it never freezes and never loads a whole file into memory. Files of 1 GB, 10 GB, 50 GB+ must open instantly and stay responsive. Core use case: open huge .txt / .log / .csv / .sql dumps — or ANY file as text — search by plain text or regex, find-and-replace, do light edits, save.

## Target platforms
- Primary: Windows 11 x64 (first-class, tested most).
- Also build and run on Linux x64 and macOS (arm64 + x64) from a single codebase.
- Single self-contained binary, no runtime dependencies (no .NET/JVM/Python needed on the user's machine).
- Deliver a portable .exe for Windows plus an optional installer.

## Tech stack (recommended — justify if you deviate, but never violate the architecture rules below)
- Rust (stable), 64-bit targets only.
- memmap2 for memory-mapped file access.
- regex crate (guaranteed linear time, no catastrophic backtracking) + memchr / aho-corasick for SIMD-accelerated literal search.
- GUI: egui/eframe (alternatives: iced, Slint, or C++/Qt if you can argue it's better). No Electron, no Tauri/webview for the text view, no browser-based rendering.
- Do NOT use any off-the-shelf editor widget as the main text view (QPlainTextEdit, AvaloniaEdit, Scintilla, Monaco, CodeMirror, HTML textarea, egui's TextEdit, etc.). All of them need the whole document in memory. Build a custom virtualized text view.

## Non-negotiable architecture rules
1. Never read the entire file into RAM. Map it with mmap and touch only the byte ranges needed. On network/UNC drives or if mmap fails, fall back transparently to buffered chunked reads.
2. Rendering is virtualized: only the lines currently on screen — and only the horizontal slice of those lines — are decoded and laid out. Memory use is O(visible content + index), never O(file size).
3. The UI thread never blocks. All file I/O, indexing, searching, replacing, saving and encoding detection run on worker threads with progress reporting and cancellation. Hard limit: no single UI-thread task over ~16 ms.
4. Line index is sparse and incremental: background checkpoints (e.g. every 64 KB: byte offset + line number), built from the start of the file so the top is navigable immediately. "Go to line" and the scrollbar must work before indexing finishes (estimate by byte offset, then refine). Full per-line offset tables are forbidden for big files (a 50 GB file with 1 billion lines would need 8 GB of index). Index memory stays under ~1% of file size.
5. Extremely long lines (a single 2 GB line, minified JSON, base64 blobs) must not freeze, crash or allocate the full line. Use horizontal virtualization and a bounded window around the caret.
6. Editing uses a piece table (or a rope of references into the mmap plus an in-memory add buffer). The original file is never modified in place until save. Undo/redo stack.
7. Saving streams the piece table to a temp file in the same directory, fsyncs, then atomically renames over the original. If anything fails, the original is untouched. Offer "Save As" and an optional .bak backup. Windows "file in use" / permission errors produce clear messages, never crashes.
8. Everything that can be large is bounded: search result lists are capped/paginated, clipboard copies above a threshold (e.g. 100 MB) ask for confirmation, match highlighting is computed only for the visible range.

## Features — must have

Opening / viewing
- Open via File menu, drag-and-drop, command-line argument (`bigtext file.txt`), and Windows "Open with". Multiple tabs.
- Open ANY file as text: encoding detection (BOM; UTF-8; UTF-16 LE/BE; heuristic fallback to Windows-1252 / Windows-1251 / Latin-1), a manual encoding override menu, and "Reopen with encoding…". Invalid byte sequences render as U+FFFD or hex escapes — never panic. Files that look binary (NUL-heavy) still open in the text view, with a one-click switch to a hex view.
- Line-ending detection (CRLF / LF / CR / mixed) shown in the status bar; optional conversion on save.
- Line numbers, monospace font, zoom (Ctrl+wheel), word-wrap toggle (off by default), light/dark theme, large-file-safe Select All.
- Go to line (Ctrl+G) and go to byte offset. Status bar: line, column, byte offset, selection length, encoding, line endings, file size, indexing/search progress.
- Read-only mode toggle. Detect external file changes and offer reload.

Search (Ctrl+F)
- Plain-text and regex modes. Options: case sensitive, whole word, regex, multiline / dot-matches-newline, search in selection.
- Streams over the mmap on a worker thread; results appear incrementally with a live match counter; cancel at any time. Search does not depend on the line index being complete — it works on byte offsets and maps hits to line numbers lazily.
- Find next / previous (F3 / Shift+F3) that works even before a full-file search finishes.
- "Find All" results panel (virtualized list): line number + context snippet, click to jump. Cap results (e.g. 1,000,000) with a clear "more exist" indicator.
- Highlight matches in the visible viewport only.
- Non-UTF-8 files must still be searchable (transcode the pattern for literal search on fixed-width encodings; otherwise stream-decode in chunks). Correct Unicode case folding for case-insensitive search.
- In the chunked-read fallback, handle matches that span chunk boundaries.
- Fast path: with no unsaved edits, search the mmap directly. With edits, search across piece-table pieces and handle matches spanning piece boundaries.
- Regex engine must be linear time. Show regex syntax errors inline.

Find and replace (Ctrl+H)
- Replace current, Replace all, with regex capture-group references ($1, ${name}) and escape sequences (\n, \t).
- Replace All on big files runs as a streaming pass: read mmap → write replaced output to a temp file → atomic swap. Bounded memory regardless of file size, progress bar, cancellable (cancel leaves the original untouched).
- If the match count is small (configurable threshold, e.g. 50,000), apply as piece-table edits so undo works; above it, warn that undo is unavailable and offer to keep a backup.
- Optional preview of the first N replacements before committing.

Editing
- Insert/delete text, cut/copy/paste, undo/redo, multi-line selection, duplicate line, delete line, basic auto-indent. This is a lightweight editor, not an IDE — no syntax highlighting required in v1.
- Save (Ctrl+S), Save As, unsaved-changes prompt on close.

## Features — nice to have (only after everything above works)
- Tail/follow mode for live log files.
- Bookmarks, split view, simple syntax highlighting for common formats.
- Streaming diff of two files.
- Recent files list, session restore.

## Performance targets (measure them and report real numbers)
- Open a file of any size and show the first screen of text in < 200 ms (mmap + scan only the first ~1 MB).
- Jump to any line/offset in a 20 GB file in < 50 ms once indexed; < 500 ms before indexing completes (estimated position).
- Scrolling at 60 fps; UI frame time < 16 ms while background indexing/search is running.
- Literal search throughput ≥ 1 GB/s on cached data on a modern desktop CPU; simple regex ≥ 200 MB/s.
- Replace All on a 10 GB file: private memory stays < 300 MB, throughput limited only by disk.
- Idle private (non-file-backed) memory with a 50 GB file open: < 200 MB.
- Startup to first paint < 300 ms.

## Edge cases that must not crash or hang
- Empty file; no trailing newline; one gigantic line; millions of 1-byte lines.
- Mixed line endings; mixed or invalid encodings; BOM in the middle; NUL bytes.
- File being appended by another process; file deleted/moved while open; slow network drive or USB stick; read-only file; Unicode paths; paths > 260 characters on Windows.
- Regex matching the empty string; regex matching the whole file; a replace that makes the file grow or shrink massively.
- App closed mid-search or mid-replace → clean cancellation, no corrupted output.

## Deliverables
1. Complete source with a clear module layout (file access, index, text model / piece table, search, replace, encoding, view, app shell).
2. Build instructions for Windows (MSVC toolchain), Linux and macOS, plus a GitHub Actions workflow that builds all three and uploads artifacts.
3. A script that generates test files: 1 GB and 10 GB random-line files, a single-line 2 GB file, a UTF-16LE file, a Windows-1251 file, a binary file, a mixed CRLF/LF file, a 100-million-short-lines file.
4. Unit tests for the line index, piece table, search (literal + regex), replace (capture groups, empty matches) and encoding detection. Benchmarks for search and replace throughput.
5. README with usage, keyboard shortcuts and the measured performance numbers.

## Process
- Start with a short written plan (modules, data structures, threading model). Get the architecture right before writing any UI code.
- Build in runnable milestones: (1) open + view + scroll + go-to with mmap and sparse index; (2) search + results; (3) streaming replace-all; (4) editing + save; (5) encodings, hex view, polish.
- After each milestone, run the generated test files and report real numbers against the targets. If a target is missed, fix it before moving on.
- Ask only when something is genuinely ambiguous; otherwise pick a sensible default and note it.
- Prefer boring, proven solutions over clever ones. Correctness and responsiveness beat feature count.