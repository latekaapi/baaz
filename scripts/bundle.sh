#!/bin/sh
# Bundle the release harness binary as a macOS app.
#
# Usage:
#   scripts/bundle.sh
#
# Reads `assets/icon-1024.png` (a flat H tile, 1024x1024), builds
# `cargo build --release -p harness`, converts the icon to
# `Harness.icns` with `sips`/`iconutil`, writes `Info.plist`
# (`dev.harness.app`) and assembles `target/bundle/Harness.app`:
#
#   Harness.app/Contents/{Info.plist,MacOS/Harness,Resources/Harness.icns}
#
# The bundle is ad-hoc signed so it launches on this machine with `open`.
# Re-running the script rebuilds in place; it never touches the repo's
# sources. Costs nothing: no server, no turn.
set -eu

export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="$ROOT/assets/icon-1024.png"
OUT="$ROOT/target/bundle/Harness.app"
CONTENTS="$OUT/Contents"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -n 1)"

if [ ! -f "$SRC" ]; then
    echo "bundle: missing $SRC" >&2
    exit 1
fi

cargo build --release -p harness

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT INT TERM
SET="$WORK/Harness.iconset"
mkdir -p "$SET"
# Every slot iconutil wants, downsampled from the 1024 source.
for size in 16 32 64 128 256 512; do
    sips -z "$size" "$size" "$SRC" --out "$SET/icon_${size}x${size}.png" >/dev/null
    double=$((size * 2))
    sips -z "$double" "$double" "$SRC" --out "$SET/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$SET" -o "$WORK/Harness.icns"

mkdir -p "$CONTENTS/MacOS" "$CONTENTS/Resources"
cp "$ROOT/target/release/harness" "$CONTENTS/MacOS/Harness"
cp "$WORK/Harness.icns" "$CONTENTS/Resources/Harness.icns"
cat >"$CONTENTS/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key>
    <string>Harness</string>
    <key>CFBundleIdentifier</key>
    <string>dev.harness.app</string>
    <key>CFBundleName</key>
    <string>Harness</string>
    <key>CFBundleDisplayName</key>
    <string>Harness</string>
    <key>CFBundleIconFile</key>
    <string>Harness</string>
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
