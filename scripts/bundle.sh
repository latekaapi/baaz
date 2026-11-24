#!/bin/sh
# Bundle the release baaz binary as a macOS app.
#
# Usage:
#   scripts/bundle.sh
#
# Reads `assets/icon-1024.png` (the Baaz app icon: the mascot on a
# baked squircle, 1024x1024), builds
# `cargo build --release -p baaz --no-default-features` (no probe, below),
# converts the icon to
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
# `crates/baaz-core/src/terminal_relay.rs`).
#
# The bundle is ad-hoc signed so it launches on this machine with `open`.
# Re-running the script assembles the new app beside the old one and swaps
# it in with one rename at the end, so a Baaz that is running keeps the
# files it already has open (2026-10-21: overwriting the running binary in
# place got the owner's Baaz killed mid-session, no crash report). Quit and
# reopen to run the new build. It never touches the repo's sources. Costs
# nothing: no server, no turn.
#
# Never swap under a running Baaz (2026-10-31): a running app launches every
# new Chromium helper by path, so after a swap its fresh helpers came from the
# new build while its own engine was the old one — "observer_list reentrancy"
# failures in Browser/cef.log and an app that stopped responding. While a Baaz
# runs from OUT, the new build is parked at NEXT and swapped in by the next run
# (or `scripts/bundle.sh --swap`) once that Baaz has quit.
set -eu

export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="$ROOT/assets/icon-1024.png"
# A test build never lands on the owner's bundle: BAAZ_BUNDLE_DIR moves all
# three paths below, BAAZ_BUNDLE_ID renames the app and its helpers.
BUNDLE_DIR="${BAAZ_BUNDLE_DIR:-$ROOT/target/bundle}"
BUNDLE_ID="${BAAZ_BUNDLE_ID:-sh.baaz.app}"
OUT="$BUNDLE_DIR/Baaz.app"
# Assembled here, on the same volume, then renamed over OUT in one step.
STAGE="$BUNDLE_DIR/.Baaz.app.new"
# A finished build parked while a Baaz was running from OUT.
NEXT="$BUNDLE_DIR/next/Baaz.app"

running() { pgrep -f "$OUT/Contents/MacOS/Baaz" >/dev/null 2>&1; }

swap_in() {
    OLD="$OUT.old.$$"
    if [ -e "$OUT" ]; then
        mv "$OUT" "$OLD"
    fi
    mv "$1" "$OUT"
    rm -rf "$OLD"
}

if [ "${1:-}" = "--swap" ]; then
    if [ ! -d "$NEXT" ]; then
        echo "bundle: nothing parked at $NEXT"
        exit 0
    fi
    if running; then
        echo "bundle: a Baaz is still running from $OUT — quit it first" >&2
        exit 1
    fi
    swap_in "$NEXT"
    echo "bundle: $OUT"
    exit 0
fi
CONTENTS="$STAGE/Contents"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -n 1)"

if [ ! -f "$SRC" ]; then
    echo "bundle: missing $SRC" >&2
    exit 1
fi

# The probe (docs/82 4.10): the app a person runs ships without it — no
# `--steps`, `--screenshot`, `--replay` or `--bench`, no step verbs, no gpui
# test-support at all — so the build turns the default features off and names the ones it wants. `--no-default-features` is load-bearing: the `probe` feature
# is on by default for every dev build, and `crates/baaz/tests/probe_ratchet.rs`
# fails if a release build here stops passing it. The check build
# (`scripts/baaz-check.sh build`) sets BAAZ_PROBE=1: it is driven by `--steps`.
PROBE_FEATURE=""
if [ "${BAAZ_PROBE:-0}" = "1" ]; then
    PROBE_FEATURE="baaz/probe"
    echo "bundle: with the probe (BAAZ_PROBE=1)"
fi

# Chromium (docs/25-browser-design.md): embedded from the CEF binary
# distribution. CEF_PATH names its folder (default ~/.local/share/cef);
# BAAZ_CEF=0 builds a WebKit-only app.
CEF_DIR="${CEF_PATH:-$HOME/.local/share/cef}"

# The pinned CEF (docs/82 9.1): the minimal macOS arm64 distribution the `cef`
# crate in Cargo.lock is built against, fetched from the CEF project's own
# build server and checked against its SHA-1 before anything is unpacked. A
# folder that already holds this archive (its archive.json names it) is used
# as it is; a missing one is fetched, so a clean clone on a clean cache builds
# the full app. Bump all three together with the `cef` crate.
CEF_VERSION="154.0.33"
CEF_ARCHIVE="cef_binary_154.0.33+ga03e714+chromium-154.0.8037.94_macosarm64_minimal.tar.bz2"
CEF_SHA1="bfa2358a5fba8d0a016118941d9cda0bf2d06f20"

pinned_cef_matches_lock() {
    grep -A1 '^name = "cef"$' "$ROOT/Cargo.lock" | grep -q "+$CEF_VERSION\""
}

fetch_cef() {
    if [ -f "$CEF_DIR/archive.json" ] && grep -q "\"$CEF_ARCHIVE\"" "$CEF_DIR/archive.json" \
        && [ -d "$CEF_DIR/Chromium Embedded Framework.framework" ]; then
        return 0
    fi
    if [ -e "$CEF_DIR" ] && [ -n "$(ls -A "$CEF_DIR" 2>/dev/null)" ]; then
        echo "bundle: $CEF_DIR holds another CEF than $CEF_VERSION; move it aside or set CEF_PATH" >&2
        return 1
    fi
    url="https://cef-builds.spotifycdn.com/$(printf '%s' "$CEF_ARCHIVE" | sed 's/+/%2B/g')"
    work="$(mktemp -d "${TMPDIR:-/tmp}/baaz-cef.XXXXXX")"
    echo "bundle: fetching CEF $CEF_VERSION ($url)"
    if ! curl -fL --retry 3 -o "$work/cef.tar.bz2" "$url"; then
        rm -rf "$work"; echo "bundle: the CEF download failed" >&2; return 1
    fi
    got="$(shasum -a 1 "$work/cef.tar.bz2" | cut -d' ' -f1)"
    if [ "$got" != "$CEF_SHA1" ]; then
        rm -rf "$work"; echo "bundle: CEF checksum mismatch (got $got, want $CEF_SHA1); nothing unpacked" >&2; return 1
    fi
    mkdir -p "$work/x" && tar -xjf "$work/cef.tar.bz2" -C "$work/x"
    top="$(ls "$work/x")"
    mkdir -p "$(dirname "$CEF_DIR")" && rm -rf "$CEF_DIR" && mv "$work/x/$top" "$CEF_DIR"
    printf '{\n  "type": "minimal",\n  "name": "%s",\n  "sha1": "%s"\n}' "$CEF_ARCHIVE" "$CEF_SHA1" > "$CEF_DIR/archive.json"
    rm -rf "$work"
    echo "bundle: CEF $CEF_VERSION at $CEF_DIR"
}

if [ "${BAAZ_CEF:-1}" != "0" ]; then
    pinned_cef_matches_lock || { echo "bundle: the CEF pin ($CEF_VERSION) does not match the cef crate in Cargo.lock; bump the pin" >&2; exit 1; }
    fetch_cef
fi
if [ "${BAAZ_CEF:-1}" != "0" ] && [ -d "$CEF_DIR/Chromium Embedded Framework.framework" ]; then
    WITH_CEF=1
    export CEF_PATH="$CEF_DIR"
    cargo build --release -p baaz -p mcp-bridge --no-default-features --features "baaz/cef${PROBE_FEATURE:+,$PROBE_FEATURE}"
    # The helper is its own workspace (CI never builds CEF); share the target
    # dir so the CEF wrapper compiles once.
    cargo build --release --manifest-path "$ROOT/crates/baaz-cef-helper/Cargo.toml" --target-dir "$ROOT/target"
else
    WITH_CEF=0
    echo "bundle: no CEF at $CEF_DIR (or BAAZ_CEF=0): the browser pane stays on WKWebView"
    cargo build --release -p baaz -p mcp-bridge --no-default-features ${PROBE_FEATURE:+--features "$PROBE_FEATURE"}
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
            id="$BUNDLE_ID.helper.$(echo "$kind" | tr '[:upper:]' '[:lower:]')"
        else
            name="Baaz Helper"
            id="$BUNDLE_ID.helper"
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
    <string>$BUNDLE_ID</string>
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

# Swap — unless a Baaz runs from OUT: then park the build at NEXT and leave
# the running app's files (and the helpers it launches by path) alone.
if running; then
    rm -rf "$NEXT"
    mkdir -p "$(dirname "$NEXT")"
    mv "$STAGE" "$NEXT"
    echo "bundle: a Baaz is running from $OUT, so the new build is parked at $NEXT"
    echo "bundle: quit Baaz, then run: scripts/bundle.sh --swap   (or launch $NEXT directly)"
    exit 0
fi
rm -rf "$NEXT"
swap_in "$STAGE"

echo "bundle: $OUT"
echo "bundle: launch with: open \"$OUT\""
