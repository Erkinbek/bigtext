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
# The newest glibc symbol version the binaries use: dpkg then refuses to install them on a
# system they cannot start on, instead of leaving a menu entry that does nothing.
glibc="$(objdump -T "$bin/bigtext" "$bin/bigtext-cli" | grep -o 'GLIBC_[0-9.]*' | cut -d_ -f2 | sort -uV | tail -n1)"
echo "The binaries need glibc $glibc"
# The window libraries are loaded at run time, not linked, so they are listed by hand: those
# for X11 (without libxkbcommon-x11, which a desktop without Qt or GNOME may lack, BigText
# quits at start), those for Wayland, and OpenGL. The AUR package names the same ones.
depends="libc6 (>= $glibc), libgcc-s1, libgl1, libegl1, libx11-6, libx11-xcb1, libxcursor1, libxi6"
depends="$depends, libxkbcommon0, libxkbcommon-x11-0, libwayland-client0, libwayland-egl1"
cat > "$root/DEBIAN/control" <<EOF
Package: bigtext
Version: $version
Section: editors
Priority: optional
Architecture: amd64
Depends: $depends
Installed-Size: $size_kb
Maintainer: Erkin Pardayev <erkin.pardayev@gmail.com>
Homepage: https://github.com/Erkinbek/bigtext
Description: Text editor for huge files
 BigText opens 1-50+ GB files instantly and never freezes: streaming search,
 replace and atomic saves, log tools (levels, time histogram, merging logs),
 CSV tables and JSON tools. Includes bigtext-cli for scripts.
EOF
# xz rather than the default zstd, so dpkg on Debian 11 and Ubuntu 20.04 can open it.
dpkg-deb --root-owner-group -Zxz --build "$root" "$out"
