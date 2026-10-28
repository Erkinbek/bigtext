#!/usr/bin/env bash
# Builds BigText.app from release binaries and zips it: build-app.sh VERSION BIN_DIR OUT.zip
# (macOS only: uses sips, iconutil and ditto). The app is not signed.
set -euo pipefail
version="$1"
bin="$2"
mkdir -p "$(dirname "$3")"
out="$(cd "$(dirname "$3")" && pwd)/$(basename "$3")"
here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
app="$work/BigText.app"

mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$bin/bigtext" "$bin/bigtext-cli" "$app/Contents/MacOS/"
sed "s/@VERSION@/$version/g" "$here/Info.plist" > "$app/Contents/Info.plist"

# The icon, from the 256 px PNG.
png="$here/../../assets/icon-256.png"
iconset="$work/bigtext.iconset"
mkdir "$iconset"
for s in 16 32 128 256; do
    sips -z "$s" "$s" "$png" --out "$iconset/icon_${s}x${s}.png" > /dev/null
    d=$((s * 2))
    sips -z "$d" "$d" "$png" --out "$iconset/icon_${s}x${s}@2x.png" > /dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/bigtext.icns"

rm -f "$out"
(cd "$work" && ditto -c -k --keepParent BigText.app "$out")
