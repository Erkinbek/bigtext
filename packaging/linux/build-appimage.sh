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
