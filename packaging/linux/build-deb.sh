#!/usr/bin/env bash
# Builds a .deb from release binaries: build-deb.sh VERSION BIN_DIR OUT.deb
# Installs bigtext and bigtext-cli to /usr/bin, the menu entry and the icon.
set -euo pipefail
version="$1"
bin="$2"
out="$3"
here="$(cd "$(dirname "$0")" && pwd)"
root="$(mktemp -d)"
trap 'rm -rf "$root"' EXIT

install -Dm755 "$bin/bigtext" "$root/usr/bin/bigtext"
install -Dm755 "$bin/bigtext-cli" "$root/usr/bin/bigtext-cli"
install -Dm644 "$here/bigtext.desktop" "$root/usr/share/applications/bigtext.desktop"
install -Dm644 "$here/../../assets/icon-256.png" "$root/usr/share/icons/hicolor/256x256/apps/bigtext.png"
install -Dm644 "$here/../../LICENSE" "$root/usr/share/doc/bigtext/copyright"

mkdir -p "$root/DEBIAN"
size_kb="$(du -sk "$root/usr" | cut -f1)"
cat > "$root/DEBIAN/control" <<EOF
Package: bigtext
Version: $version
Section: editors
Priority: optional
Architecture: amd64
Depends: libc6, libgl1, libxkbcommon0
Installed-Size: $size_kb
Maintainer: Erkin Pardayev <erkin.pardayev@gmail.com>
Homepage: https://github.com/Erkinbek/bigtext
Description: Text editor for huge files
 BigText opens 1-50+ GB files instantly and never freezes: streaming search,
 replace and atomic saves, log tools (levels, time histogram, merging logs),
 CSV tables and JSON tools. Includes bigtext-cli for scripts.
EOF
dpkg-deb --root-owner-group --build "$root" "$out"
