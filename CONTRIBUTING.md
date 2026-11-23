# Building and contributing

Bug reports and ideas are welcome in the [issue tracker](https://github.com/Erkinbek/bigtext/issues).
This page explains how BigText is built, how it works inside, and how it is tested and released.

## First run

1. Install [Rust](https://rustup.rs) and keep it on the latest stable (`rustup update stable`):
   CI builds and lints with the latest stable, and an older clippy disagrees with it (see
   [Code style](#code-style)). Then the platform's build tools from [Building](#building): the
   Visual Studio Build Tools on Windows, a few X11/Wayland/GL libraries on Linux, the Xcode
   command line tools on macOS.
2. Build and run:

   ```
   git clone https://github.com/Erkinbek/bigtext && cd bigtext
   cargo run --release -- FILE                    # the editor (a release build: debug is slow on big files)
   cargo run --release --bin bigtext-cli -- --help
   scripts/gen-testfiles.sh --skip-big            # test files in testdata/, ~1.8 GB (Windows: scripts\gen-testfiles.ps1 -SkipBig)
   ```

   Without `--skip-big` the generator also writes a 10 GB file (~14 GB in all), which the
   benchmarks and the screenshots use.
3. Before a pull request, run what CI runs:

   ```
   cargo fmt
   cargo clippy --all-targets -- -D warnings
   cargo test --lib --bins
   ```

   For a change to the window (menus, the text view, dialogs), also run the GUI self-test
   (`target/release/bigtext --self-test --report report.txt`, see
   [Tests and benchmarks](#tests-and-benchmarks)) and try the change on a large file.

The core (`src/lib.rs`: file access, encodings, index, piece table, search, replace, save and the
log, CSV, JSON and SQL tools) is a library that all four binaries use; the window (`app/`,
`view/`, the self-test and the GUI benchmark) belongs to the `bigtext` binary. The
[architecture map](#architecture) says where each part lives, and
[the rules below](#rules-for-new-code) are the ones every change has to keep.

## Building

The workspace builds four binaries: `bigtext` (the editor), `bigtext-cli` (the script
commands), `bigtext-bench` (core benchmarks) and `gen-testfiles` (test corpus generator).
`packaging/` has the Linux `.deb` / AppImage and macOS `.app` scripts, the AUR package, and the
Scoop bucket and Homebrew tap (see [packaging/README.md](packaging/README.md)).

### Windows (MSVC)

1. Install [Rust](https://rustup.rs) (`stable-x86_64-pc-windows-msvc`) and the Visual Studio
   Build Tools with the "Desktop development with C++" workload.
2. `cargo build --release` → `target\release\bigtext.exe` is the portable executable.
3. Optional installer: install [Inno Setup 6](https://jrsoftware.org/isinfo.php), then
   `ISCC.exe /DSourceExe=%CD%\target\release\bigtext.exe installer\bigtext.iss`
   (adds Start-menu entry, "Open with" registration and an Explorer context-menu item).

### Linux

```
sudo apt-get install build-essential pkg-config libxkbcommon-dev libwayland-dev libxcb-render0-dev \
  libxcb-shape0-dev libxcb-xfixes0-dev libgl1-mesa-dev libegl1-mesa-dev
cargo build --release
```

These are the libraries CI installs on Ubuntu 22.04. Running under X11 also needs
`libxkbcommon-x11-0`, which desktop systems already have.

### macOS

Install the Xcode command line tools, then `cargo build --release` (Apple silicon), or
`rustup target add x86_64-apple-darwin && cargo build --release --target x86_64-apple-darwin`.

### CI

[.github/workflows/build.yml](.github/workflows/build.yml) builds Windows x64, Linux x64 and
macOS arm64/x64, runs the unit tests and uploads the binaries plus the Windows installer
(`bigtext-<version>-setup.exe`) as artifacts of every run. The installer was checked with a silent
per-user install (`/VERYSILENT /CURRENTUSER`): files, "Open with" and Explorer context-menu
entries and the Start-menu shortcut are created, the installed exe runs, and the uninstaller
removes all of it.

[.github/workflows/audit.yml](.github/workflows/audit.yml) checks `Cargo.lock` against the
[RustSec advisory database](https://rustsec.org) every Monday and whenever `Cargo.toml` or
`Cargo.lock` changes. Run the same check locally with `cargo install cargo-audit` and
`cargo audit`.

## Architecture

BigText is written in Rust with a custom virtualized text view on top of `egui`/`eframe` (glow
renderer).

```
src/
  file.rs           Source: cached mmap views (Windows) | pread blocks filled on demand (Unix) | chunked (block cache + loader) | empty
  encoding.rs       Encoding, Codec (newline search/count, decode/encode, char boundaries), detection
  index.rs          LineIndex: one u64 per 64 KB, background indexer, estimates
  text/buffer.rs    TextBuf: piece table (original mmap + add buffer), line<->offset mapping, undo swaps
  text/rows.rs      bounded visual segments (64 KB soft breaks), wrap, cell math
  search.rs         Matcher (memmem / regex-automata / decoded), windowed streaming search
  replace.rs        templates, batch edits, streaming replace-all
  save.rs           temp file + fsync + atomic replace (ReplaceFileW), EOL conversion
  diff.rs           streaming line diff (chunk compare + hashed re-synchronisation)
  jobs.rs           cancel tokens, progress, wakers
  filter.rs         grep-like filter view: the lines with (or without) a match, streamed
  lines.rs          reading every line front to back in large chunks, for whole-file jobs
  timestamp.rs      log timestamps near the line start, binary search for a time
  loglevel.rs, logformat.rs   log levels; nginx/Apache, load balancer, IIS and syslog formats as columns
  fields.rs, jsonq.rs         fields of JSON Lines / logfmt lines; field queries (.status >= 500 and ...)
  merge.rs, correlate.rs      merging logs by time; request/trace ids that tie lines together
  csv*.rs           CSV/TSV: detection, records with line breaks, checks, diff, join, pivot, expressions
  json*.rs, xml.rs  streaming JSON/XML structure, strict JSON check, structural JSON diff
  sqldump.rs        tables of mysqldump / pg_dump / SQLite dumps, one table saved on its own
  redact.rs         Share safely: secrets and personal data replaced by labels
  decompress.rs     .gz/.zst/.xz/.bz2 and zip/tar members, unpacked to temp files
  remote.rs         HTTP(S)/S3 range reads and files over ssh, read in parts
  headless.rs       bigtext-cli commands (filter, count, split, stats, query, redact, ...)
  update.rs         update check and self-update from GitHub releases
  robustness.rs     seeded random/broken input for the parsers (tests only)
  app/              egui shell: tabs, menus, search panel, results list, dialogs, save protocol,
                    session/recent files, compare panel, the tool panels and translations (i18n_*.rs)
  view/             custom virtualized text view, CSV table view, hex view, syntax highlighting
  cli.rs            the editor's command-line options (bigtext FILE:LINE, -f, --find, ...)
  self_test.rs      in-app GUI self-test driven by synthetic input
  bench_ui.rs       scripted GUI benchmark (--bench-ui)
  bin/              bigtext-cli, gen-testfiles, bigtext-bench
```

`app/`, `view/`, `cli.rs`, `self_test.rs` and `bench_ui.rs` are modules of the `bigtext` binary
(`src/main.rs`); everything else is the library in `src/lib.rs`. Each file starts with a `//!`
comment that says what it does.

Key decisions (details in [PLAN.md](PLAN.md)):

- The view is anchored to a **byte offset**, not a line number, so it never depends on the
  index. Scrollbars are proportional to bytes.
- **Rows are bounded**: lines are soft-broken every 64 KB, so finding the row around any offset
  reads at most 128 KB. Nothing on the UI thread scans unbounded data.
- **Threading**: the UI thread only reads the visible bytes. Opening + detection, indexing,
  search, find-next, replace, save, large copies and file watching run on workers that hold
  cheap `Arc` snapshots of the piece table and report through channels.
- **Saving on Windows** requires the file to be unmapped before it can be replaced: the worker
  writes and fsyncs the temp file from a snapshot, the UI then drops its mapping, and a
  finalize worker waits until no snapshot holds the old file, swaps it in and reopens it. If
  that fails the original (untouched) file is reopened and the edits are re-attached.
- Undo history is cleared after a save (the old mapping it refers to is released). Undoing
  back to the last saved state clears the modified marker.

### Rules for new code

These come from the original specification ([prompt.md](prompt.md)) and hold for every feature,
since a single whole-file read or blocking call undoes the point of the program:

- **Never read a whole file into memory.** Touch only the byte ranges needed; work that has to
  see the whole file streams it in bounded chunks. No per-line offset tables for big files:
  index memory stays under about 1 % of the file size.
- **The UI thread never blocks.** File I/O, indexing, searching, replacing, saving and encoding
  detection run on worker threads with progress and cancellation; no single UI-thread task takes
  more than about 16 ms.
- **Only what is on screen is decoded and laid out**, and only the visible part of a long line:
  a single 2 GB line must not be allocated whole.
- **The original file is not changed before a save**, and a save writes a temp file in the same
  folder, fsyncs it and atomically replaces the original; if anything fails, the original is
  untouched.
- **Everything that can be large is bounded:** result lists are capped, large clipboard copies
  ask first, match highlighting covers only the visible range.

The targets the code is measured against (first screen under 200 ms, frames under 16 ms while
background work runs, literal search at 1 GB/s or more, Replace All on 10 GB under 300 MB of
private memory) and the measured numbers are in
[README → Measured performance](README.md#measured-performance).

## Code style

The code is formatted with rustfmt's default settings and has no clippy warnings. CI checks both
on every push and pull request with the latest stable Rust, so run them with it too
(`rustup update stable`): an older clippy misses newer lints and reports `unknown lint` for the
ones the code allows.

```
cargo fmt                                   # formats the code (cargo fmt --check only checks)
cargo clippy --all-targets -- -D warnings   # any warning fails
```

The translation tables (`src/app/i18n_*.rs`) and the keyword lists
(`src/view/highlight/words.rs`) are marked `#[rustfmt::skip]`, so they keep one entry per line.
The code was formatted in one commit; `git config blame.ignoreRevsFile .git-blame-ignore-revs`
makes `git blame` look past it (GitHub does this by itself).

## Tests and benchmarks

```
cargo test --lib --bins              # the unit tests: index, piece table, rows, search, replace, save,
                                     # encodings, diff, highlighting, log formats, SQL dumps, CSV, ...
cargo test --lib headless            # the bigtext-cli commands (src/headless.rs), on temp files
cargo test --lib update -- --ignored # downloads and unpacks the latest release (needs the network)
BIGTEXT_FUZZ_ROUNDS=50 cargo test --release --lib robustness
                                     # random and broken input for the file parsers, 50 times as many
target/release/bigtext --self-test --report report.txt [--screenshot shots/x.png]
                                     # GUI checks with synthetic mouse/keyboard/clipboard input; PASS/FAIL per check in the report
scripts/gen-testfiles.ps1            # or scripts/gen-testfiles.sh; -SkipBig / --skip-big avoids the 10 GB file
BIGTEXT_SSH_CONFIG=test/ssh_config BIGTEXT_SELFTEST_SSH=root@test-host:/var/log/app/big.log \
target/release/bigtext --self-test ... # also reads a file in place over SSH and searches it on the server
BIGTEXT_SSH_LIVE="root@127.0.0.1 2222 /var/log/app/big.log KEY KNOWN_HOSTS" \
cargo test --lib ssh_live -- --ignored   # the SSH range reader and grep against a real server
BIGTEXT_SELFTEST_DOCKER="|CONTAINER|TEXT" BIGTEXT_SELFTEST_K8S="|POD|TEXT|CONTAINER" \
BIGTEXT_SELFTEST_K8S_PREVIOUS="|POD|TEXT" BIGTEXT_SELFTEST_JOURNAL="SERVER|UNIT|TEXT" \
target/release/bigtext --self-test ... # container and service logs against real Docker, Kubernetes, journald
BIGTEXT_SELFTEST_NOTIFY=1 target/release/bigtext --self-test ... # alerts show real notifications (otherwise kept)
target/release/bigtext-bench FILE    # core benchmarks (open, first screen, go-to, index, search, replace, memory)
target/release/bigtext --bench-ui FILE --report out.txt [--screenshot out.png]
        [--scenario search:TEXT|edit|view|wrap|hex|close-search|close-replace]
```

For the **SSH checks**, a throwaway server is enough: an `ubuntu:22.04` container with
`openssh-server`, a test key in `/root/.ssh/authorized_keys`, port 22 published on
127.0.0.1:2222, and a test log of 64-byte lines (`printf "%-63s\n"` of `line NNNNNNN of the
remote file`, with ` ERROR disk full` on every thousandth line from line 7). `BIGTEXT_SSH_CONFIG`
points BigText's `ssh` at a config of its own (`ssh -F`), with a `Host test-host` entry naming
the port, the key, `UserKnownHostsFile` and `StrictHostKeyChecking accept-new`, so your own
`~/.ssh` is not touched.

For the **container and service log checks** (`BIGTEXT_SELFTEST_*` above: server, empty for
this computer | text of the row to pick | text the log must show | container): a container that
prints to stdout and stderr (`docker run -d --name logtest ubuntu:22.04 sh -c 'while true; do
echo out; echo err >&2; sleep 1; done'`); a Kubernetes cluster in Docker
(`docker run -d --privileged -p 127.0.0.1:16443:6443 rancher/k3s:v1.31.4-k3s1 server`, its
`/etc/rancher/k3s/k3s.yaml` with the port changed as `KUBECONFIG`, so your own kubeconfig is not
touched) with a two-container pod and one that exits with an error (for `--previous`); and for
the journal, an `ubuntu:22.04` image with `systemd systemd-sysv openssh-server`, a small
service that prints a line a second, run with `/sbin/init` (`--privileged --cgroupns=host -v
/sys/fs/cgroup:/sys/fs/cgroup:rw`) and reached over SSH as above.

The **GUI self-test** drives the real window through `App::raw_input_hook`: clicks, drags,
double/triple clicks, shift-click, keys and shortcuts, typing, copy/paste, Ctrl+wheel. It checks
selection, smart Home/End, auto-indent, undo/redo back to the unmodified state, clipboard,
duplicate/delete line, zoom, find / F3 / Shift+F3 / Find All, replace all + undo, go to line and
offset, atomic save (no temp files left), the unsaved-changes prompt, wrap/hex/overlay toggles,
bookmarks, split view, highlighting, file comparison, the CSV table (column selection, delete
and undo), the About window, and these edge cases:
read-only files (edits blocked; saving over one fails cleanly and keeps the edits), a file
appended to while open (banner, then follow mode), a file renamed or deleted while open, a
path longer than 260 characters (open, edit, save), a Unicode path, the chunked read path
(forced, and via `\\localhost\E$` UNC), empty / no-trailing-newline / CR-only / BOM-in-the-middle
/ UTF-16 / Windows-1251 files, a binary file, and a 6 MB copy on a worker. The report's first line
counts passed, failed and skipped checks (the SSH, container and notification checks are
skipped without their variables) and gives the longest UI frame of the run; nothing may fail. The `close-search` / `close-replace` scenarios close the window
mid-job: the process exits within a second, no temp file is left and the original file's hash
is unchanged.

The generator writes `random-lines-1g.txt`, `random-lines-10g.txt`, `single-line-2g.txt`,
`utf16le.txt`, `cp1251.txt`, `binary.bin`, `mixed-eol.txt`, `short-lines-100m.txt` and small
edge cases (empty file, no trailing newline, CR-only, BOM in the middle, NUL + invalid UTF-8,
Unicode path).

## Website

The project website ([erkinbek.github.io/bigtext](https://erkinbek.github.io/bigtext/)) lives in `docs/` and is served by
GitHub Pages from the `main` branch. It is generated in the program's eight languages
(English, Russian and Uzbek in the script, Chinese, Spanish, Portuguese, German and Turkish in
`scripts/site_<lang>.py`; a check stops the build when a text is missing in one of them) by
`python scripts/build-site.py` (needs Pillow), which also converts the screenshots in
`target/shots` to WebP and draws the logo, favicons and the social preview image. The guides
(`docs/guides/…`, step-by-step pages for common searches) have their texts in
`scripts/site_guides.py`:

```
target/release/bigtext --screenshots target/shots testdata/random-lines-10g.txt
python scripts/build-site.py
```

The demo animation at the top of the README (`docs/img/demo.gif`) and the website
(`docs/img/demo.mp4`) is recorded by the app itself: `--demo` plays a scripted tour with
captions over a 50 GB file (the 10 GB test file copied five times), takes a frame every 66 ms
and writes them to `target/demo/frames`; `scripts/make-demo.py` (needs ffmpeg) turns them into
the GIF, the video and its poster. Set `DEMO_DATE` in `scripts/build-site.py` when you record a
new one.

```
copy /b testdata\random-lines-10g.txt+testdata\random-lines-10g.txt+testdata\random-lines-10g.txt+testdata\random-lines-10g.txt+testdata\random-lines-10g.txt testdata\random-lines-50g.txt
target/release/bigtext --demo target/demo testdata/random-lines-50g.txt
python scripts/make-demo.py
```

## Releasing

1. Bump the version in `Cargo.toml`, `installer/bigtext.iss` and `scripts/build-site.py`, add
   the release to [CHANGELOG.md](CHANGELOG.md), run `python scripts/build-site.py --no-images`
   and commit.
2. Push a tag: `git tag -a v0.1.2 -m "BigText 0.1.2"` and `git push origin v0.1.2`.
3. CI builds every platform and publishes a GitHub release with version-free asset names
   (so `releases/latest/download/<file>` links keep working, and the in-app updater finds
   `bigtext-setup-windows-x64.exe`, `bigtext-windows-x64.zip` and the `.tar.gz` files by these
   names). Write the changes under a `## What's new in 0.1.2` heading in the release notes: the
   update dialog shows that section (and the whole notes when there is no such heading). Then:
   - **crates.io** (`crates-io` job) publishes the crate with
     [trusted publishing](https://crates.io/docs/trusted-publishing), so no token is stored.
     It runs when the repository variable `CRATES_IO_PUBLISH` is `true`. One-time setup: the
     first version is published by hand (`cargo login`, then `cargo publish`); then on crates.io
     → bigtext → Settings → Trusted Publishing add the repository `Erkinbek/bigtext` with the
     workflow `build.yml`.
   - **winget** (`winget` job) opens a pull request with the new version at
     [microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs) through
     [WinGet Releaser](https://github.com/vedantmgoyal9/winget-releaser). It needs the secret
     `WINGET_TOKEN` (a classic token with the `public_repo` scope) and a fork of winget-pkgs
     under the repository owner, and it only updates a package that is already in winget: the
     first version is submitted by hand from the manifests in `packaging/winget/`, which pass
     `winget validate`. Until that submission is merged the job skips itself.
   - **AUR** (the `aur` workflow, after the build) builds and checks the package `bigtext-bin`
     and pushes it to the AUR once `AUR_PUBLISH` and `AUR_SSH_PRIVATE_KEY` are set
     ([packaging/README.md → AUR](packaging/README.md#aur)).
   - **Scoop and Homebrew** need nothing from the release: the bucket and the tap repositories
     (once they exist, see [packaging/README.md](packaging/README.md#scoop-and-homebrew)) pick
     up the new version by themselves within a few hours.
