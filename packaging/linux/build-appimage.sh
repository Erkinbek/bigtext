#!/usr/bin/env bash
# Builds an AppImage from release binaries: build-appimage.sh BIN_DIR OUT.AppImage
# Downloads appimagetool; runs it without FUSE (--appimage-extract-and-run).
set -euo pipefail
bin="$1"
out="$2"
here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
app="$work/BigText.AppDir"

install -Dm755 "$bin/bigtext" "$app/usr/bin/bigtext"
install -Dm755 "$bin/bigtext-cli" "$app/usr/bin/bigtext-cli"
install -Dm644 "$here/bigtext.desktop" "$app/usr/share/applications/bigtext.desktop"
install -Dm644 "$here/../../assets/icon-256.png" "$app/usr/share/icons/hicolor/256x256/apps/bigtext.png"
cp "$here/bigtext.desktop" "$app/bigtext.desktop"
cp "$here/../../assets/icon-256.png" "$app/bigtext.png"

# The window libraries are loaded at run time, not linked, so nothing lists them. A desktop
# without Qt or GNOME may have no libxkbcommon-x11, and BigText then quits at start on X11
# ("Library libxkbcommon-x11.so could not be loaded"), so it comes in the AppImage, with
# libxkbcommon (the two share private structures and must be one version) and libxcb-xkb, which
# it needs. libX11, libxcb, libGL and libwayland every desktop has; AppImages leave those to
# the system.
for lib in libxkbcommon.so.0 libxkbcommon-x11.so.0 libxcb-xkb.so.1; do
  src="$(ldconfig -p | awk -v n="$lib" '$1 == n && /x86-64/ { print $NF; exit }')"
  [ -n "$src" ] || { echo "build-appimage.sh: $lib is not installed (libxkbcommon-x11-0)" >&2; exit 1; }
  install -Dm644 "$(readlink -f "$src")" "$app/usr/lib/$lib"
  patchelf --set-rpath '$ORIGIN' "$app/usr/lib/$lib"
done
# Found through the binary's run path, not LD_LIBRARY_PATH, which the programs BigText starts
# (ssh, the browser, your own tools) would inherit and load these libraries too.
patchelf --set-rpath '$ORIGIN/../lib' "$app/usr/bin/bigtext"

cat > "$app/AppRun" <<'EOF'
#!/bin/sh
here="$(dirname "$(readlink -f "$0")")"
exec "$here/usr/bin/bigtext" "$@"
EOF
chmod +x "$app/AppRun"

tool="$work/appimagetool"
curl -fsSL -o "$tool" https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage
chmod +x "$tool"
ARCH=x86_64 "$tool" --appimage-extract-and-run "$app" "$out"
