#!/bin/sh
# Bundle the release baaz binary as a macOS app.
#
# Usage:
#   scripts/bundle.sh
#
# Reads `assets/icon-1024.png` (the Baaz app icon: the mascot on a
# baked squircle, 1024x1024), builds
# `cargo build --release -p baaz`, converts the icon to
# `Baaz.icns` with `sips`/`iconutil`, writes `Info.plist`
# (`sh.baaz.app`) and assembles `target/bundle/Baaz.app`:
#
#   Baaz.app/Contents/{Info.plist,MacOS/Baaz,Resources/Baaz.icns}
#
# The bundle is ad-hoc signed so it launches on this machine with `open`.
# Re-running the script rebuilds in place; it never touches the repo's
# sources. Costs nothing: no server, no turn.
set -eu

export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="$ROOT/assets/icon-1024.png"
OUT="$ROOT/target/bundle/Baaz.app"
CONTENTS="$OUT/Contents"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -n 1)"

if [ ! -f "$SRC" ]; then
    echo "bundle: missing $SRC" >&2
    exit 1
fi

cargo build --release -p baaz

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT INT TERM
SET="$WORK/Baaz.iconset"
mkdir -p "$SET"
# Every slot iconutil wants, downsampled from the 1024 source.
for size in 16 32 64 128 256 512; do
    sips -z "$size" "$size" "$SRC" --out "$SET/icon_${size}x${size}.png" >/dev/null
    double=$((size * 2))
    sips -z "$double" "$double" "$SRC" --out "$SET/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$SET" -o "$WORK/Baaz.icns"

mkdir -p "$CONTENTS/MacOS" "$CONTENTS/Resources"
cp "$ROOT/target/release/baaz" "$CONTENTS/MacOS/Baaz"
cp "$WORK/Baaz.icns" "$CONTENTS/Resources/Baaz.icns"
cat >"$CONTENTS/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key>
    <string>Baaz</string>
    <key>CFBundleIdentifier</key>
    <string>sh.baaz.app</string>
    <key>CFBundleName</key>
    <string>Baaz</string>
    <key>CFBundleDisplayName</key>
    <string>Baaz</string>
    <key>CFBundleIconFile</key>
    <string>Baaz</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>${VERSION}</string>
    <key>CFBundleVersion</key>
    <string>${VERSION}</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>LSMinimumSystemVersion</key>
    <string>14.0</string>
</dict>
</plist>
EOF

# Ad-hoc sign so Gatekeeper lets `open` launch it on this machine.
codesign --force --deep --sign - "$OUT" >/dev/null 2>&1 || true

echo "bundle: $OUT"
echo "bundle: launch with: open \"$OUT\""
