"""Turns the frames of `bigtext --demo` into the demo animation: docs/img/demo.mp4 for the
website and docs/img/demo.gif for the README. Needs ffmpeg on the PATH.

    target/release/gen-testfiles --only random-lines-10g.txt
    copy /b testdata\\random-lines-10g.txt+...(five times) testdata\\random-lines-50g.txt
    target/release/bigtext --demo target/demo testdata/random-lines-50g.txt
    python scripts/make-demo.py [FRAMES_DIR]

The frames keep the times they were taken at (frames.ffconcat), so the animation runs at the
real speed of the app.
"""

import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "docs" / "img"


def ffmpeg(frames: Path, *args: str) -> None:
    subprocess.run(
        ["ffmpeg", "-hide_banner", "-loglevel", "error", "-y",
         "-f", "concat", "-safe", "0", "-i", str(frames / "frames.ffconcat"), *args],
        check=True,
    )


def main() -> None:
    frames = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "target" / "demo" / "frames"
    if not (frames / "frames.ffconcat").exists():
        sys.exit(f"{frames} has no frames.ffconcat; record them with bigtext --demo first")
    OUT.mkdir(parents=True, exist_ok=True)
    mp4, gif = OUT / "demo.mp4", OUT / "demo.gif"
    # Constant 30 fps for players; the text stays sharp at the recorded 1280 px.
    ffmpeg(frames, "-vf", "fps=30,format=yuv420p", "-c:v", "libx264", "-preset", "slow", "-crf", "26",
           "-movflags", "+faststart", "-an", str(mp4))
    # 8 fps and no dithering: the picture is mostly text on a flat background, which a shared
    # palette keeps crisp; diff_mode redraws only what changed.
    ffmpeg(frames, "-vf",
           "fps=8,scale=960:-1:flags=lanczos,split[a][b];"
           "[a]palettegen=max_colors=128:stats_mode=diff[p];"
           "[b][p]paletteuse=dither=none:diff_mode=rectangle",
           str(gif))
    # The website shows this until the video plays: the 50 GB file just opened, with its caption.
    poster = OUT / "demo-poster.webp"
    subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-ss", "4.5", "-i", str(mp4),
                    "-frames:v", "1", "-c:v", "libwebp", "-quality", "82", str(poster)], check=True)
    for f in (mp4, gif, poster):
        print(f"{f.relative_to(ROOT)}: {f.stat().st_size / 1e6:.2f} MB")


if __name__ == "__main__":
    main()
