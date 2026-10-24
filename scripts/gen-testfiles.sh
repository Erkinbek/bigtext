#!/usr/bin/env sh
# Generates the benchmark corpus (~14 GB with the 10 GB file; use --skip-big for ~1.8 GB).
# Usage: scripts/gen-testfiles.sh [--out DIR] [--skip-big] [--only name,...] [--force]
set -e
cd "$(dirname "$0")/.."
cargo run --release --bin gen-testfiles -- "$@"
