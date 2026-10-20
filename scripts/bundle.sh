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
#   Baaz.app/Contents/{Info.plist,MacOS/{Baaz,mcp-bridge},Resources/Baaz.icns}
#   + Frameworks/{Chromium Embedded Framework.framework, Baaz Helper*.app}
#     when CEF is on disk (see below)
#
# `mcp-bridge` rides beside the baaz binary: it is the terminal relay every
# provider route spawns (`config.mcpServers`, `--mcp-config`, `-c
# mcp_servers.baaz…` all name the sibling — see
# `crates/baaz/src/terminal/relay.rs`).
#
# The bundle is ad-hoc signed so it launches on this machine with `open`.
# Re-running the script assembles the new app beside the old one and swaps
# it in with one rename at the end, so a Baaz that is running keeps the
# files it already has open (2026-10-21: overwriting the running binary in
# place got the owner's Baaz killed mid-session, no crash report). Quit and
# reopen to run the new build. It never touches the repo's sources. Costs
# nothing: no server, no turn.
set -eu

export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="$ROOT/assets/icon-1024.png"
OUT="$ROOT/target/bundle/Baaz.app"
# Assembled here, on the same volume, then renamed over OUT in one step.
STAGE="$ROOT/target/bundle/.Baaz.app.new"
CONTENTS="$STAGE/Contents"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -n 1)"

if [ ! -f "$SRC" ]; then
    echo "bundle: missing $SRC" >&2
    exit 1
fi

# Chromium (docs/25-browser-design.md): embedded when the CEF binary
# distribution is on disk. CEF_PATH names it (default ~/.local/share/cef,
# which cef-rs's `export-cef-dir` fills); BAAZ_CEF=0 builds a WebKit-only app.
CEF_DIR="${CEF_PATH:-$HOME/.local/share/cef}"
if [ "${BAAZ_CEF:-1}" != "0" ] && [ -d "$CEF_DIR/Chromium Embedded Framework.framework" ]; then
    WITH_CEF=1
    export CEF_PATH="$CEF_DIR"
    cargo build --release -p baaz -p mcp-bridge --features baaz/cef
    # The helper is its own workspace (CI never builds CEF); share the target
    # dir so the CEF wrapper compiles once.
    cargo build --release --manifest-path "$ROOT/crates/baaz-cef-helper/Cargo.toml" --target-dir "$ROOT/target"
else
    WITH_CEF=0
    echo "bundle: no CEF at $CEF_DIR (or BAAZ_CEF=0): the browser pane stays on WKWebView"
    cargo build --release -p baaz -p mcp-bridge
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK" "$STAGE"' EXIT INT TERM
rm -rf "$STAGE"
mkdir -p "$(dirname "$OUT")"
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
cp "$ROOT/target/release/mcp-bridge" "$CONTENTS/MacOS/mcp-bridge"
cp "$WORK/Baaz.icns" "$CONTENTS/Resources/Baaz.icns"

# The CEF framework and its five helper apps, named the way CEF derives them
# from the main bundle (`<Name> Helper (Renderer).app`, …).
rm -rf "$CONTENTS/Frameworks"
if [ "$WITH_CEF" = 1 ]; then
    mkdir -p "$CONTENTS/Frameworks"
    ditto "$CEF_DIR/Chromium Embedded Framework.framework" "$CONTENTS/Frameworks/Chromium Embedded Framework.framework"
    for kind in "" "GPU" "Renderer" "Plugin" "Alerts"; do
        if [ -n "$kind" ]; then
            name="Baaz Helper ($kind)"
            id="sh.baaz.app.helper.$(echo "$kind" | tr '[:upper:]' '[:lower:]')"
        else
            name="Baaz Helper"
            id="sh.baaz.app.helper"
        fi
        helper="$CONTENTS/Frameworks/$name.app/Contents"
        mkdir -p "$helper/MacOS"
        cp "$ROOT/target/release/baaz-cef-helper" "$helper/MacOS/$name"
        cat >"$helper/Info.plist" <<HELPER
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key>
    <string>$name</string>
    <key>CFBundleIdentifier</key>
    <string>$id</string>
    <key>CFBundleName</key>
    <string>$name</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>${VERSION}</string>
    <key>CFBundleVersion</key>
    <string>${VERSION}</string>
    <key>LSUIElement</key>
    <string>1</string>
    <key>LSEnvironment</key>
    <dict>
        <key>MallocNanoZone</key>
        <string>0</string>
    </dict>
    <key>NSSupportsAutomaticGraphicsSwitching</key>
    <true/>
    <key>NSCameraUsageDescription</key>
    <string>A page in Baaz's browser asked for the camera.</string>
    <key>NSMicrophoneUsageDescription</key>
    <string>A page in Baaz's browser asked for the microphone.</string>
</dict>
</plist>
HELPER
    done
fi
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
    <key>LSEnvironment</key>
    <dict>
        <key>MallocNanoZone</key>
        <string>0</string>
    </dict>
    <key>LSMinimumSystemVersion</key>
    <string>14.0</string>
    <key>CFBundleURLTypes</key>
    <array>
        <dict>
            <key>CFBundleURLName</key>
            <string>Web site URL</string>
            <key>CFBundleURLSchemes</key>
            <array>
                <string>http</string>
                <string>https</string>
            </array>
            <key>CFBundleTypeRole</key>
            <string>Viewer</string>
        </dict>
    </array>
    <key>CFBundleDocumentTypes</key>
    <array>
        <dict>
            <key>CFBundleTypeName</key>
            <string>HTML document</string>
            <key>CFBundleTypeRole</key>
            <string>Viewer</string>
            <key>LSItemContentTypes</key>
            <array>
                <string>public.html</string>
                <string>public.xhtml</string>
            </array>
        </dict>
    </array>
</dict>
</plist>
EOF

# Ad-hoc sign so Gatekeeper lets `open` launch it on this machine.
codesign --force --deep --sign - "$STAGE" >/dev/null 2>&1 || true

# Swap: the old app moves aside and is removed; a running Baaz keeps its
# open files (unlinked, not overwritten) until it quits.
OLD="$OUT.old.$$"
if [ -e "$OUT" ]; then
    mv "$OUT" "$OLD"
fi
mv "$STAGE" "$OUT"
rm -rf "$OLD"
if pgrep -f "$OUT/Contents/MacOS/Baaz" >/dev/null 2>&1; then
    echo "bundle: a Baaz is running from the previous build — quit and reopen it to run this one"
fi

echo "bundle: $OUT"
echo "bundle: launch with: open \"$OUT\""
