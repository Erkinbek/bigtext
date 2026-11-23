#!/usr/bin/env sh
# Generates the benchmark corpus (~14 GB with the 10 GB file; use --skip-big for ~1.8 GB;
# --only random-lines-50g adds the 50 GB file of BENCHMARKS.md).
# Usage: scripts/gen-testfiles.sh [--out DIR] [--skip-big] [--only name,...] [--force]
set -e
cd "$(dirname "$0")/.."
cargo run --release --bin gen-testfiles -- "$@"
