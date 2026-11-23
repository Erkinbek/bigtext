# BigText — open, search and edit 50 GB text and log files, free

[![build](https://github.com/Erkinbek/bigtext/actions/workflows/build.yml/badge.svg)](https://github.com/Erkinbek/bigtext/actions/workflows/build.yml)
[![release](https://img.shields.io/github/v/release/Erkinbek/bigtext?sort=semver)](https://github.com/Erkinbek/bigtext/releases/latest)
[![license: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![website](https://img.shields.io/badge/website-erkinbek.github.io%2Fbigtext-2dd4bf)](https://erkinbek.github.io/bigtext/)

**Open, search and edit 1 GB, 10 GB and 50 GB text and log files without waiting for them to
load.** BigText is a free, open-source editor for **huge files** — logs, CSV exports, SQL dumps,
JSON — that never loads a whole file into memory: the first screen of a 50 GB file is drawn about
25 ms after you open it (from the OS cache) and the window keeps responding while the file is
indexed and searched.
A free alternative to EmEditor and UltraEdit, for Windows, Linux and macOS. Written in Rust, it
is one self-contained binary with no runtime to install.

**[Download](#download)** · [Website](https://erkinbek.github.io/bigtext/) · [All features](FEATURES.md) · [Usage](USAGE.md) · [Русский](https://erkinbek.github.io/bigtext/ru/) · [O'zbekcha](https://erkinbek.github.io/bigtext/uz/) · [中文](https://erkinbek.github.io/bigtext/zh/) · [Español](https://erkinbek.github.io/bigtext/es/) · [Português](https://erkinbek.github.io/bigtext/pt/) · [Deutsch](https://erkinbek.github.io/bigtext/de/) · [Türkçe](https://erkinbek.github.io/bigtext/tr/)

![BigText opens a 50 GB log file at once, jumps to its end and its middle, searches it with a regular expression while the matches come in, and follows a growing log](docs/img/demo.gif)

## What it is for

- **Finding what went wrong in gigabytes of logs.** Open the log, filter it to `ERROR` lines or a
  regex, jump to the time of the incident, see errors per minute in a histogram, group lines by
  message pattern, and follow the file as it grows. See
  [opening a 10 GB log file](https://erkinbek.github.io/bigtext/guides/open-large-log-file/).
- **Looking into a CSV or JSON export that is too big for Excel or an editor.** A CSV past
  Excel's 1,048,576 rows opens as a table you can filter, sort and summarise without importing
  it, and JSON Lines can be queried by field. See
  [a CSV larger than Excel](https://erkinbek.github.io/bigtext/guides/open-csv-larger-than-excel/).
- **Changing a huge file safely.** Edit anywhere, replace all matches in a 10 GB file with bounded
  memory, and save through a temporary file that replaces the original atomically; or take one
  table out of a SQL dump. See
  [one table out of a mysqldump](https://erkinbek.github.io/bigtext/guides/extract-table-from-mysqldump/).

What it does not do: there are no macros or plug-ins, and the known limits are listed in
[FEATURES.md](FEATURES.md#known-limitations). The numbers below are measured with the file in the
OS cache; how they were measured is under [Measured performance](#measured-performance).

## Highlights

- **Any size opens without waiting.** The first screen of a 50 GB file is drawn in about 25 ms,
  and the file stays at about 110 MB of memory. The line index builds in the background, and go to line works
  before it finishes.
- **Search at over 3 GB/s** (plain text and simple regular expressions, on a file in the OS
  cache). Plain text or regex, Find All, a filter
  view that works like `grep`, and Replace All streamed through a 10 GB file with bounded memory.
- **Real editing, safe saving.** Undo, multiple cursors, column selection, and sorting and text
  operations on files of any size. Saves write a temporary file and replace the original
  atomically, and a recovery journal brings unsaved edits back after a crash.
- **Made for logs.** Follow mode like `tail -F` — with alerts: a system notification and a sound
  when a line like `ERROR|FATAL` arrives — go to a time, a level bar, a time histogram, logs
  merged by time, and message patterns that group lines by template.
- **Web server logs, summed up.** nginx and Apache access logs, AWS load balancer logs, IIS logs
  and syslog open as a table with a column per field, and **Search → Web log report…** gives the
  top URLs, the 5xx errors per minute and the slowest requests from one pass (a 1 GB log in under
  2 seconds); a click shows the requests behind a number.
- **Tables and JSON.** CSV and TSV files open as tables with sorting, statistics, pivots and
  joins. JSON gets a structure view, folding and queries over JSON Lines.
- **SQL dumps.** **File → SQL dump tables…** lists the tables of a mysqldump, pg_dump or SQLite
  dump with their rows and sizes (a 1 GB dump in about a second), saves one table or several as
  a file that restores on its own, and splits the dump into a file per table.
- **Compare, hex and archives.** A streaming diff of two huge files, a hex view, and `.gz`,
  `.zst`, `.xz`, `.bz2`, `.zip` and `.tar` files opened without unpacking them by hand.
- **Logs on servers, without downloading them.** Open `user@server:/var/log/app.log` over SSH
  with your keys, or an `https://` or public `s3://` file (**Ctrl+Shift+U**): only what is on
  screen comes over, and over SSH searches run on the server. Or follow a log's last megabytes
  like `tail -F` (`bigtext -f user@server:/var/log/syslog`).
- **Container and service logs.** Pick a Docker container, a Kubernetes pod (or its run before
  the last crash) or a systemd service from a list, here or on a server over SSH, and its log
  opens and follows as it grows — no `docker logs -f … | bigtext -` to type.
- **Also a comfortable code editor.** Python, JavaScript, TypeScript, Java, C#, C/C++, Go, Rust,
  PHP, shell scripts, YAML and more are highlighted in One Dark, Monokai, GitHub or Solarized
  colours with indent guides; a folder side bar works as in Sublime Text; **Ctrl+P** finds a
  file, **Ctrl+R** lists functions and classes, **Ctrl+/** comments lines out, and brackets close
  themselves. Only the rows on screen are coloured, so a huge file stays just as fast.
- **Eight interface languages.** English, Russian, Uzbek, Chinese, Spanish, Portuguese, German
  and Turkish; a first start picks the system's language.

Scripts get the same streaming code without a window in `bigtext-cli` (see [USAGE.md](USAGE.md)).
Every feature is described in [FEATURES.md](FEATURES.md).

## Download

Latest release: [all files and release notes](https://github.com/Erkinbek/bigtext/releases/latest) · [changelog](CHANGELOG.md) · [Privacy](#privacy)

| Platform | Download |
| --- | --- |
| Windows 10 / 11 | [Installer](https://github.com/Erkinbek/bigtext/releases/latest/download/bigtext-setup-windows-x64.exe) (adds “Open with” and an Explorer context-menu entry) · [portable .zip](https://github.com/Erkinbek/bigtext/releases/latest/download/bigtext-windows-x64.zip) (unsigned: SmartScreen may ask, **More info → Run anyway**) |
| Linux x64 | [AppImage](https://github.com/Erkinbek/bigtext/releases/latest/download/bigtext-linux-x64.AppImage) · [.deb](https://github.com/Erkinbek/bigtext/releases/latest/download/bigtext-linux-x64.deb) (Debian, Ubuntu) · [.tar.gz](https://github.com/Erkinbek/bigtext/releases/latest/download/bigtext-linux-x64.tar.gz) |
| macOS | BigText.app for [Apple Silicon](https://github.com/Erkinbek/bigtext/releases/latest/download/bigtext-macos-arm64.app.zip) · [Intel](https://github.com/Erkinbek/bigtext/releases/latest/download/bigtext-macos-x64.app.zip); plain binaries: [Apple Silicon](https://github.com/Erkinbek/bigtext/releases/latest/download/bigtext-macos-arm64.tar.gz) · [Intel](https://github.com/Erkinbek/bigtext/releases/latest/download/bigtext-macos-x64.tar.gz) (unsigned: right-click → Open on first launch; the Intel builds have not been tried on an Intel Mac yet) |

With a package manager:

| Package manager | Command |
| --- | --- |
| [Scoop](https://scoop.sh/) (Windows) | `scoop bucket add bigtext https://github.com/Erkinbek/scoop-bigtext`, then `scoop install bigtext/bigtext` |
| [Homebrew](https://brew.sh/) (macOS, Linux) | `brew install erkinbek/tap/bigtext`; the macOS app: `brew install --cask erkinbek/tap/bigtext-app` |
| Cargo (any platform) | `cargo install bigtext` ([crates.io](https://crates.io/crates/bigtext); on Linux, first install the packages listed under [Building](CONTRIBUTING.md#linux)) |

Or build it yourself (see [Building](CONTRIBUTING.md#building)).

Since 0.11.0 every release file has a signed build provenance: `gh attestation verify FILE --repo
Erkinbek/bigtext` checks that it was built by this repository's workflow from the tagged source, and
`bigtext-sbom.cdx.json` lists the Rust libraries inside (CycloneDX).

## How it compares

Notepad++ and VS Code read the whole file into memory, so a 10 GB log needs at least 10 GB of
free RAM. The editors built for huge files are paid or read-only: EmEditor Professional is a
subscription (its free edition is for home and school use and has no large file controller or
filter), UltraEdit is sold as a subscription or a perpetual licence, and klogg views and searches
files without editing them. BigText opens, searches **and edits** files of any size on Windows,
Linux and macOS, and it is free and open source. How much memory klogg, Notepad++, VS Code and
BigText kept for a 1 GB and a 10 GB file on one PC, and how long each worked on it, is in
[BENCHMARKS.md](BENCHMARKS.md#comparing-with-other-editors).

| Editor | Price | Open source | Platforms | A 10 GB file | Editing huge files |
| --- | --- | --- | --- | --- | --- |
| **BigText** | Free | ✅ MIT | Windows, Linux, macOS | ✅ Reads only the part on screen, about 100 MB of RAM | ✅ Streaming replace, atomic save |
| EmEditor | Paid subscription (free edition for home and school use only) | ❌ | Windows; a macOS preview since November 2026 | ✅ | ✅ |
| UltraEdit | Paid (subscription or perpetual licence) | ❌ | Windows, Linux, macOS | ✅ | ✅ |
| klogg | Free | ✅ GPL-3.0 | Windows, Linux, macOS | ✅ | ❌ Views and searches only |
| Notepad++ | Free | ✅ GPL | Windows | ⚠️ Loads the whole file into RAM; the 32-bit build refuses files over 2 GB | ⚠️ Only what fits in RAM |
| VS Code | Free | ✅ MIT (Code - OSS) | Windows, Linux, macOS | ⚠️ Loads the whole file into RAM; by default 1.147 did not show a 1 GB file (it offers "Open Anyway") | ⚠️ Only what fits in RAM |

<sub>Checked on 23 November 2026 against each project's own pages:
[EmEditor](https://www.emeditor.com/), [EmEditor Free](https://www.emeditor.com/text-editor-features/emeditor-free/),
[EmEditor prices](https://www.emeditor.com/buy/),
[EmEditor 26.4: editing in Fast View](https://www.emeditor.com/emeditor-core/emeditor-v26-4-released-edit-huge-files-in-fast-view-mode-faster-startup-and-31-smaller-files/),
[EmEditor for Mac Preview 1](https://www.emeditor.com/emeditor-mac/emeditor-for-mac-preview-1-released/),
[UltraEdit large files](https://www.ultraedit.com/support/tutorials-power-tips/ultraedit/large-file-handling/),
[UltraEdit subscription](https://www.ultraedit.com/pricing/) and [perpetual](https://www.ultraedit.com/perpetual-pricing/) prices,
[klogg](https://github.com/variar/klogg),
[Notepad++ file loading](https://github.com/notepad-plus-plus/notepad-plus-plus/blob/master/PowerEditor/src/ScintillaComponent/Buffer.cpp),
[VS Code text buffer](https://code.visualstudio.com/blogs/2018/03/23/text-buffer-reimplementation).
Something out of date? [Open an issue](https://github.com/Erkinbek/bigtext/issues).</sub>

## Screenshots

| Regex search over 10 GB (2.5 GB/s) | Comparing two logs |
| --- | --- |
| ![Find All with a regular expression over a 10 GB log: 166 matches in 4 seconds](docs/img/search.webp) | ![Streaming diff of two log files with the differences listed below the text](docs/img/compare.webp) |
| **Hex view in the middle of a 10 GB file** | **Light theme, JSON, split view, bookmarks** |
| ![Hex view at offset 0x140000000 of a 10 GB file](docs/img/hex.webp) | ![JSON file with syntax highlighting in a split view with bookmarks](docs/img/json.webp) |

The screenshots are made by the app itself: `bigtext --screenshots DIR testdata/random-lines-10g.txt`.

## Usage

```
bigtext app.log:120                                     open a file at line 120
bigtext --find ERROR app.log                            open it and search
journalctl -f | bigtext -                               follow standard input
bigtext-cli filter -i timeout app.log -o timeouts.log   save the matching lines
```

The full command line, every `bigtext-cli` command and the keyboard shortcuts are in
[USAGE.md](USAGE.md).

## Measured performance

Measured with BigText 0.12.0 on 23 November 2026 on Windows 11 Pro, Intel Core Ultra 9 285
(24 cores), 191 GB RAM, NVMe SSD, release build, with the files in the OS page cache; each figure
is the range over 3 runs. A cold disk, Linux and macOS were not measured. GUI numbers come from
`bigtext --bench-ui` (frame time = CPU time of one egui frame including our layout), core numbers
from `bigtext-bench`. **[BENCHMARKS.md](BENCHMARKS.md)** has the method, the definitions, the test
file generator and how to repeat the run; the raw reports are in
[`bench/2026-11-23-windows/`](bench/2026-11-23-windows/).

### Targets

| Target | Goal | Measured | |
|---|---|---|---|
| Open → first screen of text | < 200 ms | 21.9–25.5 ms in the GUI to draw it (1 GB … 50 GB); presenting that first frame takes about 80 ms more on this PC, so it is on the monitor in about 0.1 s ([why](BENCHMARKS.md#summary)); open + detect on the worker 0.9–1.1 ms | ✅ |
| Jump to line, indexed (1 / 10 / 50 GB) | < 50 ms | frame incl. jump + layout: avg 1.11–1.20 ms, max 1.52 ms; core lookup + 50-row layout avg 0.03–0.05 ms | ✅ |
| Jump to line before indexing (estimate) | < 500 ms | frame avg 1.42–1.72 ms, max 2.49 ms (refined automatically when the index arrives) | ✅ |
| UI frame time while indexing / scrolling | < 16 ms | page-down every frame while indexing 50 GB: avg 1.10–1.13 ms, p99 1.40–1.52 ms, max 2.05 ms (600 frames); 2 GB single line: max 3.70 ms | ✅ |
| Literal search throughput | ≥ 1 GB/s | 3.58–4.11 GB/s (10 GB), 3.72–4.32 GB/s (50 GB), 3.10–4.25 GB/s (2 GB line); case-insensitive 2.38–2.85 GB/s | ✅ |
| Simple regex throughput | ≥ 200 MB/s | `id=99\d{4} ` 3.15–4.17 GB/s, `ERROR\] id=\d+ timeout` 2.85–3.73 GB/s; decoded UTF-16 / cp1251 regex 0.69–0.72 / 0.50–0.52 GB/s | ✅ |
| Replace All on 10 GB, private memory | < 300 MB | 22.2 M replacements, peak private 46.5–62.5 MB; 10 GB written in 16.8–28.9 s, the drive's own write speed at the time (see [BENCHMARKS.md](BENCHMARKS.md#summary)) | ✅ |
| Idle private memory, 50 GB file open | < 200 MB | 108 MB in the GUI after indexing (≈95 MB of it is the GUI/GL baseline); core process 12 MB | ✅ |
| Startup → first paint | < 300 ms | 138–150 ms (process start → end of first frame) | ✅ |

### Details

| File | Index time (throughput) | Index memory | Lines |
|---|---|---|---|
| random-lines-1g.txt | 0.41–0.42 s (2.40–2.46 GB/s) | 130 KB | 9,069,553 |
| random-lines-10g.txt | 3.77–3.90 s (2.57–2.65 GB/s) | 2.0 MB | 90,713,192 |
| random-lines-50g.txt | 18.6–19.3 s (2.59–2.70 GB/s) | 8.1 MB (0.016 % of file) | 453,592,927 |
| single-line-2g.txt | 0.76–0.80 s (2.50–2.62 GB/s) | 260 KB | 1 |
| short-lines-100m.txt | 0.18–0.20 s (2.28–2.53 GB/s) | 65 KB | 100,000,001 |
| utf16le.txt (64 MB) | 0.02–0.03 s | 8 KB | 690,320 |
| cp1251.txt (64 MB) | 0.03 s | 8 KB | 1,091,711 |

- Chunked fallback (forced, 1 GB): first screen 1.4–1.5 ms in the core, search 3.27–3.61 GB/s,
  private memory 63–68 MB (64 MB block cache).
- 2 GB single line: page-down frames avg 1.98–2.07 ms, go-to 0.44 ms after indexing.
- Closing the window during a Find All or a Replace All on 10 GB exits at once and leaves the file
  unchanged, with no temporary file behind.
- Not re-measured since 0.1.0 (24 October 2026): saving an edited 1 GB file (edit at the top while
  indexing was still running) took 0.9–1.1 s for write + fsync + atomic replace + reopen, and the
  no-literal regex `[a-z]{9,} [a-z]{9,}` ran at 672 MB/s.
- Memory design note: mapping a whole 50 GB file on Windows commits ~100 MB up front (file
  size / 512), so the UI maps a small LRU cache of 288 MB views and workers stream with
  positional reads into reused buffers. That keeps memory flat, and makes indexing faster than
  first-touch page faults (2.4 vs 1.6 GB/s when measured with 0.1.0); repeated searches run at
  ~4 GB/s instead of the ~10 GB/s an already-faulted full mapping would allow.

## Documentation

- [FEATURES.md](FEATURES.md): every feature in detail, and the known limitations.
- [USAGE.md](USAGE.md): the command line, `bigtext-cli` and the keyboard shortcuts.
- [CONTRIBUTING.md](CONTRIBUTING.md): building, architecture, tests, the website and releasing.
- [CHANGELOG.md](CHANGELOG.md): what changed in each release.
- [SECURITY.md](SECURITY.md): reporting a vulnerability privately.
- Guides on the website: [a CSV past Excel's 1,048,576 rows](https://erkinbek.github.io/bigtext/guides/open-csv-larger-than-excel/),
  [a 10 GB log file](https://erkinbek.github.io/bigtext/guides/open-large-log-file/),
  [one table out of a mysqldump](https://erkinbek.github.io/bigtext/guides/extract-table-from-mysqldump/).

## Privacy

This program will not transfer any information to other networked systems unless specifically
requested by the user or the person installing or operating it.

- **Update check**: only when you ask for it (**Help → Check for updates…**), or once a day if
  you agreed to automatic checks when BigText first started (the Help menu changes this). The
  request goes to GitHub's API (`api.github.com`) and carries BigText's version; nothing about
  you or your files. **Update now** downloads the new version from GitHub. GitHub's
  [privacy statement](https://docs.github.com/en/site-policy/privacy-policies/github-general-privacy-statement)
  applies to these requests.
- **Remote files**: **Open remote file or URL…** connects only to the server or address you
  type, with your own `ssh` and keys for SSH.
- BigText collects no telemetry, usage statistics or crash reports.

## Author

**Erkin Pardayev**

- GitHub: [github.com/Erkinbek](https://github.com/Erkinbek)
- Repository: [github.com/Erkinbek/bigtext](https://github.com/Erkinbek/bigtext)
- Telegram: [@ErkinPardayev](https://t.me/ErkinPardayev)
- E-mail: [erkin.pardayev@gmail.com](mailto:erkin.pardayev@gmail.com)

Licensed under the [MIT license](LICENSE).
