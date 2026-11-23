#!/usr/bin/env sh
# Runs the core benchmarks of BENCHMARKS.md (no window) on the generated corpus and keeps every
# report in target/bench/<date>/.
# Usage: scripts/bench.sh [DIR] [RUNS]
# The GUI benchmark is separate: target/release/bigtext --bench-ui FILE --report OUT
# (it opens a window; see BENCHMARKS.md).
set -e
cd "$(dirname "$0")/.."
DIR="${1:-testdata}"
RUNS="${2:-3}"
cargo build --release --bins
OUT="target/bench/$(date +%Y-%m-%d)"
mkdir -p "$OUT"
run=1
while [ "$run" -le "$RUNS" ]; do
  for f in random-lines-1g.txt random-lines-10g.txt single-line-2g.txt short-lines-100m.txt utf16le.txt cp1251.txt; do
    if [ -f "$DIR/$f" ]; then
      ./target/release/bigtext-bench "$DIR/$f" > "$OUT/core-$f-run$run.txt"
    fi
  done
  # No Replace All on 50 GB: it would write another 50 GB.
  if [ -f "$DIR/random-lines-50g.txt" ]; then
    ./target/release/bigtext-bench "$DIR/random-lines-50g.txt" --no-replace > "$OUT/core-random-lines-50g.txt-run$run.txt"
  fi
  run=$((run + 1))
done
for r in "$OUT"/core-*; do
  echo "=== $r"
  cat "$r"
done
