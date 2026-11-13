#!/usr/bin/env bash
# Writes the AppStream metainfo with the newest releases from CHANGELOG.md:
# metainfo.sh OUT.metainfo.xml (used by build-appimage.sh and build-deb.sh).
set -euo pipefail
out="$1"
here="$(cd "$(dirname "$0")" && pwd)"
# "## 0.8.0 — 2026-11-13" becomes <release version="0.8.0" date="2026-11-13"/>.
releases="$(grep -E '^## [0-9]+\.[0-9]+\.[0-9]+ .* [0-9]{4}-[0-9]{2}-[0-9]{2}' "$here/../../CHANGELOG.md" | head -n 5 \
  | sed -E 's/^## ([0-9]+\.[0-9]+\.[0-9]+) .* ([0-9]{4}-[0-9]{2}-[0-9]{2}).*/    <release version="\1" date="\2"\/>/')"
[ -n "$releases" ] || { echo "metainfo.sh: no dated releases in CHANGELOG.md" >&2; exit 1; }
mkdir -p "$(dirname "$out")"
awk -v rel="$releases" '/metainfo.sh puts the newest releases/ { print rel; next } { print }' \
  "$here/io.github.erkinbek.bigtext.metainfo.xml" > "$out"
