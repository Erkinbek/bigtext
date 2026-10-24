#!/usr/bin/env sh
# Runs the core benchmark on the generated corpus and prints the results.
# Usage: scripts/bench.sh [DIR]
set -e
cd "$(dirname "$0")/.."
DIR="${1:-testdata}"
cargo build --release --bins
for f in random-lines-1g.txt random-lines-10g.txt single-line-2g.txt short-lines-100m.txt utf16le.txt cp1251.txt; do
  if [ -f "$DIR/$f" ]; then
    echo "=== $f"
    ./target/release/bigtext-bench "$DIR/$f"
  fi
done
