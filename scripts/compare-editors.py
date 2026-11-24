"""Opens the same big files in several editors and measures, from outside and the same way for
all of them, how long each works on the file and how much memory it keeps (Windows only).

    python scripts/compare-editors.py [--runs 3] [--sizes 1g,10g] [--out bench/DATE-compare]
                                      [--only BigText,klogg] [--save-frames DIR]

How it measures (BENCHMARKS.md, "Comparing with other editors"):
- The test files are testdata/random-lines-1g.txt and -10g.txt with 120 lines of full blocks
  in front (written once as testdata/compare-1g.txt and compare-10g.txt), so a look at the
  window shows at once whether the file's text is on screen.
- Each editor is started with the file. "Busy" is the time from the start until its processes
  together used under 3 % of one core for 2 s, counted from when its first window appeared: it
  has finished loading or indexing the file, or gave up. The memory is the private memory of
  all its processes at that moment; the peak is the highest seen before. The titles of its
  windows are kept (a "file too big" message is one).
- What each editor showed is checked by eye: with --save-frames DIR, its window is captured
  once at that moment with PrintWindow (only that window; it does not work for OpenGL windows
  such as BigText's, which come out blank).
- This does not time the first screen: that needs a screen recording of each editor, and the
  window captures tried for it were not reliable (blank OpenGL windows, half-painted windows).
- Editors run with settings of their own (VS Code: a new profile folder; Notepad++: its
  portable folder; BigText: BIGTEXT_NO_STATE; klogg's settings files are put back after), and
  every process is ended after each run.
"""

import ctypes
import ctypes.wintypes as wt
import datetime as dt
import json
import os
import platform
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import numpy as np
import psutil

ROOT = Path(__file__).resolve().parent.parent
TESTDATA = ROOT / "testdata"
# Full blocks, then an empty line: easy to tell in a window that the file's text is shown.
BLOCK_LINE = ("\u2588" * 60 + "\n\n").encode("utf-8")
TIMEOUT_S, IDLE_CPU, IDLE_S = 180.0, 3.0, 2.0

user32 = ctypes.WinDLL("user32", use_last_error=True)
gdi32 = ctypes.WinDLL("gdi32", use_last_error=True)
user32.SetProcessDpiAwarenessContext.argtypes = [ctypes.c_void_p]
user32.SetProcessDpiAwarenessContext(ctypes.c_void_p(-4))  # per-monitor aware v2: real pixels

WNDENUMPROC = ctypes.WINFUNCTYPE(wt.BOOL, wt.HWND, wt.LPARAM)


class BITMAPINFOHEADER(ctypes.Structure):
    _fields_ = [
        ("biSize", wt.DWORD), ("biWidth", wt.LONG), ("biHeight", wt.LONG), ("biPlanes", wt.WORD),
        ("biBitCount", wt.WORD), ("biCompression", wt.DWORD), ("biSizeImage", wt.DWORD),
        ("biXPelsPerMeter", wt.LONG), ("biYPelsPerMeter", wt.LONG), ("biClrUsed", wt.DWORD),
        ("biClrImportant", wt.DWORD),
    ]


def windows_of(pids):
    """Visible top-level windows of these processes, with their titles."""
    found = []

    def cb(hwnd, _):
        if user32.IsWindowVisible(hwnd):
            pid = wt.DWORD()
            user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
            if pid.value in pids:
                r = wt.RECT()
                user32.GetWindowRect(hwnd, ctypes.byref(r))
                if r.right - r.left > 200 and r.bottom - r.top > 150:
                    n = user32.GetWindowTextLengthW(hwnd)
                    buf = ctypes.create_unicode_buffer(n + 1)
                    user32.GetWindowTextW(hwnd, buf, n + 1)
                    found.append((hwnd, buf.value, (r.right - r.left) * (r.bottom - r.top)))
        return True

    user32.EnumWindows(WNDENUMPROC(cb), 0)
    return found


def capture(hwnd):
    """The window's client area as an (h, w) array of packed 0xRRGGBB colours, or None."""
    r = wt.RECT()
    user32.GetClientRect(hwnd, ctypes.byref(r))
    w, h = r.right, r.bottom
    if w < 50 or h < 50:
        return None
    hdc = user32.GetDC(hwnd)
    mem = gdi32.CreateCompatibleDC(hdc)
    bmp = gdi32.CreateCompatibleBitmap(hdc, w, h)
    old = gdi32.SelectObject(mem, bmp)
    # PW_CLIENTONLY | PW_RENDERFULLCONTENT: also windows drawn with the GPU.
    ok = user32.PrintWindow(hwnd, mem, 1 | 2)
    bi = BITMAPINFOHEADER(ctypes.sizeof(BITMAPINFOHEADER), w, -h, 1, 32, 0, 0, 0, 0, 0, 0)
    buf = (ctypes.c_uint32 * (w * h))()
    got = gdi32.GetDIBits(mem, bmp, 0, h, buf, ctypes.byref(bi), 0)
    gdi32.SelectObject(mem, old)
    gdi32.DeleteObject(bmp)
    gdi32.DeleteDC(mem)
    user32.ReleaseDC(hwnd, hdc)
    if not ok or got != h:
        return None
    return np.frombuffer(buf, dtype=np.uint32).reshape(h, w) & 0xFFFFFF


def tree(pid, marker=None, cache=None):
    """The started process and its children; with `marker`, also the processes whose command
    line has it (VS Code's launcher hands over to a process of its own and exits)."""
    procs = []
    try:
        p = psutil.Process(pid)
        procs = [p] + p.children(recursive=True)
    except psutil.NoSuchProcess:
        pass
    if marker and cache is not None:
        now = time.perf_counter()
        if now - cache.get("t", 0) > 0.5:
            cache["t"] = now
            cache["pids"] = set()
            for q in psutil.process_iter(["cmdline"]):
                if marker in " ".join(q.info["cmdline"] or []):
                    cache["pids"].add(q.pid)
        for qp in cache.get("pids", ()):
            try:
                q = psutil.Process(qp)
                procs += [q] + q.children(recursive=True)
            except psutil.NoSuchProcess:
                pass
    seen, out = set(), []
    for q in procs:
        if q.pid not in seen:
            seen.add(q.pid)
            out.append(q)
    return out


def private_mb(procs):
    total = 0
    for p in procs:
        try:
            total += p.memory_info().private
        except psutil.Error:
            pass
    return total / 2**20


def kill(procs):
    for p in procs:
        try:
            p.kill()
        except psutil.Error:
            pass
    psutil.wait_procs(procs, timeout=10)


def make_files(sizes):
    files = {}
    for s in sizes:
        src, dst = TESTDATA / f"random-lines-{s}.txt", TESTDATA / f"compare-{s}.txt"
        want = src.stat().st_size + len(BLOCK_LINE) * 120
        if not dst.exists() or dst.stat().st_size != want:
            with open(dst, "wb") as out, open(src, "rb") as inp:
                out.write(BLOCK_LINE * 120)
                shutil.copyfileobj(inp, out, 16 << 20)
        files[s] = dst
    return files


def editors(work):
    npp = ROOT / "target" / "compare" / "npp" / "notepad++.exe"
    code = Path(os.environ.get("ProgramFiles", r"C:\Program Files")) / "Microsoft VS Code" / "Code.exe"
    klogg = Path(os.environ.get("ProgramFiles", r"C:\Program Files")) / "klogg" / "klogg.exe"
    big = ROOT / "target" / "release" / "bigtext.exe"
    vs_data, vs_ext = work / "vscode-data", work / "vscode-ext"
    return {
        "BigText": (big, lambda f: [str(big), "--new-window", str(f)], {"BIGTEXT_NO_STATE": "1", "BIGTEXT_RECOVERY_DIR": str(work / "bt-recovery")}, None),
        "Notepad++": (npp, lambda f: [str(npp), "-multiInst", "-nosession", "-notabbar", str(f)], {}, None),
        "VS Code": (code, lambda f: [str(code), "--new-window", "--user-data-dir", str(vs_data), "--extensions-dir", str(vs_ext), "--disable-extensions", "--disable-workspace-trust", "--skip-welcome", "--skip-release-notes", str(f)], {}, str(vs_data)),
        "klogg": (klogg, lambda f: [str(klogg), "-m", str(f)], {}, None),
    }


def version_of(name, exe):
    try:
        if name == "VS Code":
            return subprocess.run([str(exe.parent / "bin" / "code.cmd"), "--version"], capture_output=True, text=True, timeout=30).stdout.split()[0]
        if name == "BigText":
            sha = subprocess.run(["git", "rev-parse", "--short", "HEAD"], capture_output=True, text=True, cwd=ROOT).stdout.strip()
            return f"main {sha}"
        info = subprocess.run(["powershell", "-NoProfile", "-Command", f"(Get-Item '{exe}').VersionInfo.ProductVersion"], capture_output=True, text=True, timeout=30)
        return info.stdout.strip()
    except Exception as e:  # noqa: BLE001 - a version is only informative
        return f"unknown ({e})"


SAVE_FRAMES = None  # a folder: keep the frame that counted (or the last one) per run, to check


def save_frame(img, name):
    if SAVE_FRAMES is None or img is None:
        return
    from PIL import Image
    rgb = np.stack([(img >> 16) & 255, (img >> 8) & 255, img & 255], axis=-1).astype(np.uint8)
    Path(SAVE_FRAMES).mkdir(parents=True, exist_ok=True)
    Image.fromarray(rgb).save(Path(SAVE_FRAMES) / f"{name}.png")


def cpu_seconds(procs, seen):
    """CPU time used by these processes since the last call (per process, so ones that end or
    start in between do not count twice or go negative)."""
    total = 0.0
    for p in procs:
        try:
            t = p.cpu_times()
        except psutil.Error:
            continue
        now = t.user + t.system
        total += max(0.0, now - seen.get(p.pid, now))
        seen[p.pid] = now
    return total


def run_once(argv, env_extra, marker, label="run"):
    # Run from a VS Code terminal, ELECTRON_RUN_AS_NODE would start VS Code as plain Node.
    env = {k: v for k, v in os.environ.items() if not k.startswith(("ELECTRON_", "VSCODE_"))}
    env.update(env_extra)
    cache, seen = {}, {}
    t0 = time.perf_counter()
    proc = subprocess.Popen(argv, env=env)
    tree_ = lambda: tree(proc.pid, marker, cache)
    titles, peak, window_at, quiet_since, busy, mem = set(), 0.0, None, None, None, None
    last = t0
    cpu_seconds(tree_(), seen)
    while time.perf_counter() - t0 < TIMEOUT_S:
        time.sleep(0.25)
        procs = tree_()
        now = time.perf_counter()
        used = cpu_seconds(procs, seen)
        quiet = used < IDLE_CPU / 100 * (now - last)
        last = now
        peak = max(peak, private_mb(procs))
        wins = windows_of({p.pid for p in procs})
        titles.update(t for _, t, _ in wins)
        if window_at is None:
            window_at = now if wins else None
            continue
        if not quiet:
            quiet_since = None
            continue
        quiet_since = quiet_since or now
        if now - quiet_since >= IDLE_S:
            busy, mem = quiet_since - t0, private_mb(procs)
            if SAVE_FRAMES is not None:
                for hwnd, _t, _a in sorted(wins, key=lambda w: -w[2])[:1]:
                    save_frame(capture(hwnd), label)
            break
    kill(tree_())
    return {"busy_s": busy, "memory_mb": mem, "peak_mb": peak, "titles": sorted(t for t in titles if t)}


def main():
    a = sys.argv[1:]
    opt = lambda k, d: a[a.index(k) + 1] if k in a else d
    runs = int(opt("--runs", "3"))
    sizes = opt("--sizes", "1g,10g").split(",")
    out = Path(opt("--out", ROOT / "bench" / f"{dt.date.today()}-compare"))
    only = opt("--only", "")
    global SAVE_FRAMES
    SAVE_FRAMES = opt("--save-frames", None)
    out.mkdir(parents=True, exist_ok=True)
    files = make_files(sizes)
    work = Path(tempfile.mkdtemp(prefix="bigtext-compare-"))
    eds = {k: v for k, v in editors(work).items() if v[0].exists() and (not only or k in only.split(","))}
    klogg_dir = Path(os.environ["APPDATA"]) / "klogg"
    saved = {f: f.read_bytes() for f in klogg_dir.glob("*.ini")} if klogg_dir.exists() else {}
    cpu = subprocess.run(["powershell", "-NoProfile", "-Command", "(Get-CimInstance Win32_Processor).Name"], capture_output=True, text=True).stdout.strip()
    machine = {
        "date": dt.datetime.now().isoformat(timespec="minutes"),
        "os": platform.platform(),
        "cpu": cpu,
        "ram_gb": round(psutil.virtual_memory().total / 2**30),
        "editors": {k: version_of(k, v[0]) for k, v in eds.items()},
        "files": {s: f"{f.name} ({f.stat().st_size} bytes)" for s, f in files.items()},
        "method": __doc__,
    }
    (out / "machine.json").write_text(json.dumps(machine, indent=2, ensure_ascii=False), encoding="utf-8")
    print(json.dumps({k: v for k, v in machine.items() if k != "method"}, indent=2, ensure_ascii=False))
    # Warm the OS cache, as in BENCHMARKS.md.
    for f in files.values():
        with open(f, "rb") as h:
            while h.read(64 << 20):
                pass
    results = out / "runs.jsonl"
    try:
        with open(results, "a", encoding="utf-8") as log:
            for s, f in files.items():
                for run in range(1, runs + 1):
                    names = list(eds)
                    names = names[run % len(names):] + names[: run % len(names)]  # a new order each run
                    for name in names:
                        exe, argv, env, marker = eds[name]
                        r = run_once(argv(f), env, marker, f"{name}-{s}-{run}".replace(" ", ""))
                        r.update({"editor": name, "file": s, "run": run})
                        log.write(json.dumps(r, ensure_ascii=False) + "\n")
                        log.flush()
                        print(name, s, run, r["busy_s"], r["memory_mb"], r["titles"][:2], flush=True)
                        time.sleep(2)
    finally:
        for f, b in saved.items():
            f.write_bytes(b)
        shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    main()
