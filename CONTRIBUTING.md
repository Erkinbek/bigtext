# Building and contributing

Bug reports and ideas are welcome in the [issue tracker](https://github.com/Erkinbek/bigtext/issues).
This page explains how BigText is built, how it works inside, and how it is tested and released.

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
sudo apt-get install libxkbcommon-dev libwayland-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libgl1-mesa-dev
cargo build --release
```

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

## Architecture

BigText is written in Rust with a custom virtualized text view on top of `egui`/`eframe` (glow
renderer).

```
src/
  file.rs           Source: cached mmap views + positional reads | chunked (block cache + loader) | empty
  encoding.rs       Encoding, Codec (newline search/count, decode/encode, char boundaries), detection
  index.rs          LineIndex: one u64 per 64 KB, background indexer, estimates
  text/buffer.rs    TextBuf: piece table (original mmap + add buffer), line<->offset mapping, undo swaps
  text/rows.rs      bounded visual segments (64 KB soft breaks), wrap, cell math
  search.rs         Matcher (memmem / regex-automata / decoded), windowed streaming search
  replace.rs        templates, batch edits, streaming replace-all
  save.rs           temp file + fsync + atomic replace (ReplaceFileW), EOL conversion
  diff.rs           streaming line diff (chunk compare + hashed re-synchronisation)
  jobs.rs           cancel tokens, progress, wakers
  app/              egui shell: tabs, menus, search panel, results list, dialogs, save protocol,
                    session/recent files, compare panel
  view/             custom virtualized text view, hex view, syntax highlighting
  self_test.rs      in-app GUI self-test driven by synthetic input
  bin/              gen-testfiles, bigtext-bench
```

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
cargo test --lib --bins              # 371 unit tests: index, piece table, rows, search, replace, save,
                                     # encodings, diff, highlighting, log formats, SQL dumps, CSV, ...
cargo test --lib update -- --ignored # downloads and unpacks the latest release (needs the network)
target/release/bigtext --self-test --report report.txt [--screenshot shots/x.png]
                                     # 261 GUI checks with synthetic mouse/keyboard/clipboard input
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
/ UTF-16 / Windows-1251 files, a binary file, and a 6 MB copy on a worker. Every run: 56/56
passed, max UI frame time 3–5 ms. The `close-search` / `close-replace` scenarios close the window
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
