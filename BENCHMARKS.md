# Benchmarks

How BigText's speed and memory numbers are measured, so that anyone can repeat them and check
them. The numbers in the README's [Measured performance](README.md#measured-performance) section
come from here.

Latest run: **BigText 0.12.0 on Windows 11, 23 November 2026, files in the OS cache.** Raw reports:
[`bench/2026-11-23-windows/`](bench/2026-11-23-windows/).

## Summary

Each figure is the range over 3 runs (lowest to highest), unless a row says otherwise.

| What | 1 GB | 10 GB | 50 GB |
|---|---|---|---|
| Open → first screen of text drawn (GUI) | 24.5–25.5 ms | 21.9–23.2 ms | 23.9–24.4 ms |
| UI frame while scrolling during indexing (GUI), avg / max | 1.16–1.20 / 1.95 ms | 1.09 / 1.90 ms | 1.10–1.13 / 2.05 ms |
| Go to line before the index is done (GUI frame), avg / max | 1.42–1.48 / 2.16 ms | 1.53–1.72 / 2.49 ms | 1.51–1.59 / 2.17 ms |
| Background line index | 0.41–0.42 s | 3.77–3.90 s | 18.6–19.3 s |
| Index memory | 130 KB | 2.03 MB | 8.12 MB |
| Literal search, whole file | 3.19–3.47 GB/s | 3.58–4.11 GB/s | 3.72–4.32 GB/s |
| Regex `id=99\d{4} ` | 3.15–3.45 GB/s | 3.46–3.98 GB/s | 3.35–4.17 GB/s |
| Idle private memory, file open and indexed (GUI) | 98–100 MB | 101–104 MB | 108 MB |
| Replace All, 22.2 M replacements, peak private memory | | 46.5–62.5 MB | |

- **"First screen drawn" is the time to paint, not to the monitor.** The measurement stops when
  the text has been laid out and painted, before the frame is presented. On this PC the first
  frame that shows a document takes a further ~80 ms to reach the screen (found on
  10 November 2026 with a `glFinish` after the buffer swap; the wait is inside that first swap and
  its cause is not known yet), so text appears on the monitor about 0.1 s after the open
  request. A second file opened as a new tab paints in about 10 ms.
- Startup (process start → end of the first frame): 138–150 ms.
- Replace All on 10 GB took 16.8–28.9 s (354–611 MB/s including fsync). That is the speed of the
  drive at the time, not of BigText: writing the same 10 GB with `dd … conv=fsync` right after took
  14.7 s and 31.7 s (732 and 338 MB/s). The drive was 81 % full and had just written the 64 GB of
  test files. The memory figure is the one that depends on BigText.

## Machine and build

| | |
|---|---|
| CPU | Intel Core Ultra 9 285, 24 cores |
| RAM | 191 GB |
| OS | Windows 11 Pro 10.0.26300 |
| Drive | Samsung MZVL81T0HELB (NVMe SSD, 1 TB, NTFS), 81 % full |
| GPU / display | NVIDIA RTX 5090, 120 Hz (only matters for the GUI numbers) |
| BigText | 0.12.0, commit `74397fd` (the v0.12.0 tag plus text changes only) |
| Rust | 1.100.0, `cargo build --release --bins` (opt-level 3, thin LTO, codegen-units 4) |

Only the Windows code path was measured: Windows reads files through memory-mapped views. Linux
and macOS read 64 KB blocks with `pread` instead, so their numbers can differ; they have not been
measured yet (see [Not measured](#not-measured-yet)).

## Test files

All files come from `gen-testfiles` with fixed seeds, so the same command writes the same bytes on
any machine:

```
scripts/gen-testfiles.sh                         # ~14 GB (Windows: scripts\gen-testfiles.ps1)
scripts/gen-testfiles.sh --only random-lines-50g # the 50 GB file (-Only random-lines-50g)
```

| File | Size | Lines | Content |
|---|---|---|---|
| `random-lines-1g.txt` | 1 GiB | 9,069,553 | log-like lines: date, time, level, `id=`, random words |
| `random-lines-10g.txt` | 10 GiB | 90,713,192 | the same, 10× |
| `random-lines-50g.txt` | 50 GiB | 453,592,927 | the same, 50× |
| `single-line-2g.txt` | 2 GiB | 1 | one line of base64 with a marker in the middle |
| `short-lines-100m.txt` | 466 MiB | 100,000,001 | very short lines |
| `utf16le.txt` | 64 MiB | 690,320 | UTF-16 LE text |
| `cp1251.txt` | 64 MiB | 1,091,711 | Windows-1251 (Cyrillic) text |

Sizes and speeds use binary units, as the tools print them: 1 GB = 2³⁰ bytes, 1 GB/s = 2³⁰ bytes
per second.

## Cache state

All numbers are **warm**: the files were written just before the run and the machine's 191 GB of
RAM holds all of them in the OS page cache, so the drive is not read at all. This measures
BigText's own work.

A cold start (file not in memory, read from the drive) was **not measured**: this PC cannot evict
64 GB of cache between runs without a reboot. On a cold disk, opening is still a few small reads,
but indexing and searching run at the drive's read speed when that is slower than ~2.5 GB/s. To
measure it elsewhere, empty the cache before each run:

- Windows: Sysinternals RAMMap → Empty → Empty Standby List (as administrator), or reboot.
- Linux: `sync; echo 3 | sudo tee /proc/sys/vm/drop_caches`
- macOS: `sudo purge`

## What each number means

- **Open → first screen of text drawn** (`--bench-ui`): from the open request to the end of the
  egui frame that first paints text from the file, including the wait for that frame. See the
  note above on presenting.
- **UI frame time** (`--bench-ui`): CPU time of one egui frame including BigText's layout, while
  the benchmark presses Page Down every frame (600 frames on the 10 and 50 GB files, as many as fit
  in the indexing time on the 1 GB file). Average, 99th percentile and maximum are in the reports.
  A frame has 16 ms at 60 Hz.
- **Go to line** (`--bench-ui`): the frame that jumps to a pseudo-random line (fixed seed) and lays
  it out. Before the index is done the line's position is estimated, and corrected when the index
  arrives.
- **Background line index** (`bigtext-bench`): the sparse index of the whole file, one 8-byte
  checkpoint per 64 KB, built on a worker thread while the window stays usable.
- **Search** (`bigtext-bench`): one pass over the whole file counting every match, in bytes of the
  file per second. The file is searched in 16 MB windows; case-sensitive literal text uses
  `memchr::memmem` on the raw bytes, everything else the `regex` crate, with a 1 MB overlap between
  windows so a match on a window edge is not missed.
- **Replace All** (`bigtext-bench`): `timeout` → `TIMEOUT!` streamed into a new temporary file in
  the same folder, then fsync; wall time and the peak private memory sampled every 20 ms. The
  output is deleted afterwards.
- **Private memory**: the process's private bytes (Windows commit charge). About 95 MB of the GUI
  figure is the window, fonts and OpenGL; the core alone holds 12 MB with the 50 GB file open and
  indexed.

## How to run it

```
scripts\bench.ps1            # Windows: core and GUI, 3 runs, reports in target\bench\<date>\
scripts/bench.sh             # Linux/macOS: core only
```

The GUI benchmark opens a window and takes the keyboard focus for about five minutes: leave the
PC alone while it runs. One GUI run by hand:

```
target/release/bigtext --bench-ui testdata/random-lines-10g.txt --report report.txt
target/release/bigtext --bench-ui FILE --scenario "search:timeout connection" --report r.txt
target/release/bigtext --bench-ui FILE --scenario close-search --report r.txt
```

## Other results of this run

- **2 GB single line**: first screen drawn 24.4–26.0 ms; Page Down frames while indexing avg
  1.98–2.07 ms, max 3.70 ms; literal search 3.10–4.25 GB/s.
- **Encodings**: UTF-16 LE literal 2.07–2.33 GB/s and regex 0.69–0.72 GB/s; Windows-1251 literal
  2.67–2.68 GB/s and regex 0.50–0.52 GB/s (decoded text). Case-insensitive search on UTF-8
  2.38–2.85 GB/s.
- **Chunked reading** (forced on the 1 GB file; used on Windows for network, removable and optical
  drives): first screen 1.43–1.48 ms in the core, literal search 3.27–3.61 GB/s, 63–68 MB private
  memory (64 MB block cache).
- **Find All through the window** (one run each): 10 GB, 432,862 matches in 3.31 s (3.1 GB/s). On
  50 GB the list stops at its cap of 1,000,000 results, reached after 7.2 s.
- **Closing during work** (10 GB, at 5 % of a Find All and of a Replace All on disk): the program
  exits at once (each whole run, start included, took about 1 s), the file is unchanged and no
  temporary file is left.

## Linux, macOS and Windows on GitHub runners

The [bench workflow](.github/workflows/bench.yml) ran the core benchmark (`bigtext-bench`, no
window) with the published 0.12.0 binaries on four GitHub-hosted runners, 3 runs each, on
23 November 2026. Raw reports: [`bench/2026-11-23-ci/`](bench/2026-11-23-ci/). Runners are shared
virtual machines with 3–4 cores, so the numbers vary between runs (the ranges are wide) and are
lower than a desktop's. They show how the Linux and macOS reading path (`pread`) behaves, not
what a user's PC does.

| | Linux x64 | macOS arm64 | macOS x64 (Intel) | Windows x64 |
|---|---|---|---|---|
| Runner | AMD EPYC 7763, 4 vCPU, 16 GB | Apple M1 (virtual), 3 cores, 7 GB | Intel i7-8700B, 4 cores, 14 GB | Intel Xeon 6973P-C, 4 vCPU, 16 GB |
| Open → first screen laid out (core), 1 / 10 GB | 3.0 / 2.9–3.9 ms | 5.4–8.8 / 12.5–17.1 ms | 9.4–19.0 / 5.8–47.1 ms | 2.5–3.0 / 2.5–3.0 ms |
| Background line index, 1 GB | 0.84–0.87 s | 1.18–1.76 s | 2.37–5.03 s | 2.34–2.38 s |
| Background line index, 10 GB | 10.1–10.9 s | 11.5–12.9 s | 28.7–180 s | 21.3–24.5 s |
| Literal search, 1 / 10 GB | 2.07–2.11 / 2.22 GB/s | 1.10–4.29 / 0.85–1.25 GB/s | 1.32–2.53 / 0.15–0.74 GB/s | 3.05–3.61 / 3.26–3.59 GB/s |
| Regex `id=99\d{4} `, 1 / 10 GB | 1.94–2.04 / 2.05–2.09 GB/s | 3.42–3.92 / 0.82–1.37 GB/s | 2.01–2.92 / 0.38–0.88 GB/s | 2.98–3.63 / 3.33–3.70 GB/s |
| Replace All, 1 GB (2.2 M replacements, with fsync) | 3.62–4.02 s | 0.80–1.66 s | 3.12–8.65 s | 4.40–6.20 s, 43.4–43.7 MB peak |

- **"Open → first screen" here is the core figure** (the text of the first screen read and laid
  out, no window), not the GUI's "drawn" time in the summary above.
- **The 10 GB file does not fit in the macOS arm64 runner's 7 GB of memory**, so its 10 GB
  searches read the disk; the Intel runner's disk made its 10 GB runs vary from 28.7 to 180 s.
- **On 4 cores the background index is the slowest part**: 0.42–0.48 GB/s on the Windows runner
  and about 1 GB/s on Linux, against 2.4–2.7 GB/s on the 24-core PC above (50 GB would take from
  about 50 s to 2 minutes there). Scrolling and Go to line do not wait for it.
- **Memory** is not measured on macOS (`n/a` in the reports), and on Linux the figure changed
  from 1 MB to 71 MB between identical runs, so it is left out.
- **Intel Macs**: this is the first time the macOS x64 build ran on an Intel processor. The core
  benchmark ran on every test file; the window itself was not tested there.

## Earlier runs

| | 0.1.0, 24 Oct 2026 | 0.12.0, 23 Nov 2026 |
|---|---|---|
| Open → first screen drawn, 1–50 GB | 41–46 ms | 21.9–25.5 ms |
| Literal search, 10 and 50 GB | 2.4–2.5 GB/s | 3.58–4.32 GB/s |
| Index throughput | 2.2–2.7 GB/s | 2.4–2.7 GB/s |
| Idle memory, 50 GB open (GUI) | 107 MB | 108 MB |
| Startup → first frame | 159–185 ms | 138–150 ms |
| Replace All, 10 GB | 11.3 s, 46 MB peak | 16.8–28.9 s (drive-bound, see above), 46.5–62.5 MB peak |

Both on the same PC, in the same way. The 0.1.0 reports were not kept; from this run on they are in
`bench/`.

## Comparing with other editors

Not done yet. A fair comparison needs the same machine, files and cache state for every editor,
each editor's version, the same definition of "opened" for all of them (text visible on the
monitor, timed from a screen recording, since other editors have no benchmark mode), several runs,
and the raw data published. Until then, BigText's documents make no speed claims about other
editors.

## Not measured yet

- A cold disk, a hard disk, a network drive or a USB stick.
- Linux and macOS on a real PC (only on shared GitHub runners so far, see above), and the GUI
  benchmark on them.
- Files larger than RAM.
- The time to present the first frame, after its cause is understood.
