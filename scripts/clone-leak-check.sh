#!/bin/bash
# Asserts Chromium's code-sign clones stay bounded across every way a run ends
# (docs/74 §1). Free: --no-connect captures, no window activation, no turn.
#
#   scripts/clone-leak-check.sh [--reps N] [path/to/Baaz.app]
#
# Default bundle: a copy of target/check/Baaz.app or target/bundle/Baaz.app. It
# is copied and renamed to the bundle ID sh.baaz.leaktest, so this only ever
# touches that ID's clone directory, and only ever kills pids it started.
# Exit 0 pass · 1 a scenario failed · 0 with SKIP where there is no CEF to test.
set -u

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REPS=3
SRC=""
while [ $# -gt 0 ]; do
    case "$1" in
        --reps) REPS="$2"; shift 2 ;;
        *) SRC="$1"; shift ;;
    esac
done

skip() { echo "SKIP: $1"; exit 0; }
[ "$(uname)" = Darwin ] || skip "not macOS"
if [ -z "$SRC" ]; then
    for candidate in "$ROOT/target/check/Baaz.app" "$ROOT/target/bundle/Baaz.app"; do
        [ -d "$candidate/Contents/Frameworks/Chromium Embedded Framework.framework" ] && SRC="$candidate" && break
    done
fi
[ -n "$SRC" ] && [ -d "$SRC/Contents/Frameworks/Chromium Embedded Framework.framework" ] || skip "no bundle with CEF"

ID=sh.baaz.leaktest
WORK="$(mktemp -d "${TMPDIR:-/tmp}/baaz-clone-check.XXXXXX")"
APP="$WORK/Baaz.app"
BIN="$APP/Contents/MacOS/Baaz"
TEMP="$(getconf DARWIN_USER_TEMP_DIR)"
DIR="$(cd "$TEMP/.." && pwd -P)/X/$ID.code_sign_clone"
PIDS=""
FAILED=0

cleanup() {
    for pid in $PIDS; do kill -9 "$pid" 2>/dev/null; done
    rm -rf "$DIR" "$WORK"
}
trap cleanup EXIT
trap 'exit 1' INT TERM

make_app() {
    rm -rf "$APP"
    cp -cR "$SRC" "$APP"
    /usr/libexec/PlistBuddy -c "Set :CFBundleIdentifier $ID" "$APP/Contents/Info.plist"
    for helper in "$APP"/Contents/Frameworks/*Helper*.app; do
        old="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$helper/Contents/Info.plist")"
        /usr/libexec/PlistBuddy -c "Set :CFBundleIdentifier $ID.helper.${old##*.}" "$helper/Contents/Info.plist"
    done
    codesign --force --deep --sign - "$APP" >/dev/null 2>&1
}

count() { ls "$DIR" 2>/dev/null | grep -c '^code_sign_clone\.' ; }
# Clones newer than the launch sweep's grace stay, so scenarios start empty.
reset() { rm -rf "$DIR"; }
wait_count() { # at least N clones, or give up
    for _ in $(seq 1 80); do [ "$(count)" -ge "$1" ] && return 0; sleep 0.25; done
    return 1
}
need_count() { # like wait_count, but a miss is a failure: it means no clone to test
    wait_count "$1" || { printf 'FAIL  %s: never saw %s clones\n' "$2" "$1"; FAILED=1; }
}
settle() { sleep 3; }

launch() { # delay-ms [extra args...]; sets PID
    local delay="$1"; shift
    local state; state="$(mktemp -d "$WORK/state.XXXXXX")"
    BAAZ_STATE_DIR="$state" "$BIN" --workspace "$ROOT" --no-connect --screenshot "$WORK/shot.png" \
        --screenshot-delay "$delay" "$@" >>"$WORK/run.log" 2>&1 &
    PID=$!
    PIDS="$PIDS $PID"
}

# The cleanup helper starts after its parent exits, polls once a second, and
# Gatekeeper checks a freshly swapped binary first (measured: up to 5 s).
settle_to() { # wait for the count to fall to N, or give up after 20 s
    for _ in $(seq 1 80); do [ "$(count)" -le "$1" ] && return 0; sleep 0.25; done
    return 1
}

check() { # name expected-max actual
    if [ "$3" -le "$2" ]; then
        printf 'ok    %-32s clones after: %s (max %s)\n' "$1" "$3" "$2"
    else
        printf 'FAIL  %-32s clones after: %s (max %s)\n' "$1" "$3" "$2"
        FAILED=1
    fi
}

ends_clean() { # name, then how to end it: a signal name, or "wait"
    local name="$1" how="$2" rep
    for rep in $(seq 1 "$REPS"); do
        reset
        launch 9000
        wait_count 1 || { check "$name (no clone appeared)" 0 1; continue; }
        # Not in the instant Chromium installs its handlers: that window is
        # covered by the sweep below (crates/baaz/src/quit_signals.rs).
        sleep 0.5
        if [ "$how" != wait ]; then kill -"$how" "$PID"; fi
        wait "$PID" 2>/dev/null
        settle_to 0
        check "$name #$rep" 0 "$(count)"
    done
}

make_app
echo "bundle under test: $SRC"
echo "clone dir: $DIR"

ends_clean "normal quit" wait
ends_clean "SIGTERM" TERM
ends_clean "SIGINT" INT
ends_clean "SIGHUP" HUP

# A failed --steps run leaves through process::exit(1).
for rep in $(seq 1 "$REPS"); do
    reset
    launch 9000 --steps "no-such-verb:x"
    need_count 1 "failed steps"
    wait "$PID" 2>/dev/null
    settle_to 0
    check "failed steps exit #$rep" 0 "$(count)"
done

# A rebuild swapped in under a running instance, then a normal quit.
for rep in $(seq 1 "$REPS"); do
    reset
    launch 9000
    need_count 1 "rebuild"
    cp -cR "$APP" "$APP.new"; mv "$APP" "$APP.old"; mv "$APP.new" "$APP"; rm -rf "$APP.old"
    wait "$PID" 2>/dev/null
    settle_to 0
    check "rebuild under it, then quit #$rep" 0 "$(count)"
done

# Hard kills leave a clone by nature; the next sweep must remove them, and a
# running instance's clone must survive that same sweep.
reset
for rep in $(seq 1 "$REPS"); do
    launch 60000
    need_count "$rep" "SIGKILL"
    kill -9 "$PID"; wait "$PID" 2>/dev/null
done
settle
check "SIGKILL leaves one each (before sweep)" "$REPS" "$(count)"
[ "$(count)" -eq "$REPS" ] || { echo "FAIL  expected $REPS clones to sweep"; FAILED=1; }
"$BIN" --sweep-clones >>"$WORK/run.log"
check "sweep removes the dead ones" 0 "$(count)"

# A running instance of ANOTHER build: its clone stays, a dead one goes.
reset
launch 60000; KEEP=$PID; need_count 1 "running instance"
LIVE="$(ls "$DIR")"
# A second build (a new inode for the executable) whose dead clone must go. It
# must stay a real .app path, or Chromium fails its startup check.
mkdir -p "$WORK/b"; cp -cR "$APP" "$WORK/b/Baaz.app"
OTHER="$WORK/b/Baaz.app/Contents/MacOS/Baaz"
cp "$OTHER" "$OTHER.tmp"; mv "$OTHER.tmp" "$OTHER"
[ "$(stat -f %i "$OTHER")" != "$(stat -f %i "$BIN")" ] || { echo "FAIL  the second build shares an inode"; FAILED=1; }
BAK="$BIN"; BIN="$OTHER"
launch 60000; need_count 2 "second build's clone"; kill -9 "$PID"; wait "$PID" 2>/dev/null
BIN="$BAK"
"$OTHER" --sweep-clones >>"$WORK/run.log"
check "dead clone of another build goes" 1 "$(count)"
[ "$(count)" -eq 1 ] || { echo "FAIL  expected exactly the live clone left"; FAILED=1; }
if [ -d "$DIR/$LIVE" ]; then
    echo "ok    the running instance's clone survived"
else
    echo "FAIL  a sweep removed a running instance's clone"; FAILED=1
fi
# Its executable is still what the sweep protects: repeated sweeps stay safe.
for _ in 1 2 3; do "$BIN" --sweep-clones >>"$WORK/run.log" & done; wait $! 2>/dev/null
sleep 1
[ -d "$DIR/$LIVE" ] && echo "ok    still there after three sweeps at once" \
    || { echo "FAIL  concurrent sweeps removed it"; FAILED=1; }
kill -TERM "$KEEP"; wait "$KEEP" 2>/dev/null
settle_to 0
check "and that instance then quits clean" 0 "$(count)"
rm -rf "$WORK/b"

# Launches at once, each with the startup sweep running: nobody loses a clone.
reset
launch 12000; A=$PID; launch 12000; B=$PID; launch 12000; C=$PID
need_count 3 "three at once"
check "three at once: no more than one each" 3 "$(count)"
[ "$(count)" -eq 3 ] || { echo "FAIL  a concurrent launch lost its clone"; FAILED=1; }
wait "$A" 2>/dev/null; wait "$B" 2>/dev/null; wait "$C" 2>/dev/null
settle_to 0
check "three at once: all clean after" 0 "$(count)"

# Other apps' directories are never touched: their listings must not change.
X_PARENT="$(dirname "$DIR")"
BEFORE="$(ls -A "$X_PARENT" | grep code_sign_clone | grep -v '^sh\.baaz\.' | while read -r d; do echo "$d $(ls "$X_PARENT/$d" | wc -l)"; done)"
"$BIN" --sweep-clones >>"$WORK/run.log"
AFTER="$(ls -A "$X_PARENT" | grep code_sign_clone | grep -v '^sh\.baaz\.' | while read -r d; do echo "$d $(ls "$X_PARENT/$d" | wc -l)"; done)"
[ "$BEFORE" = "$AFTER" ] && echo "ok    other apps' clone directories unchanged" \
    || { echo "FAIL  another app's clone directory changed"; FAILED=1; }

if [ "$FAILED" -ne 0 ]; then
    echo "clone-leak-check: FAILED"; exit 1
fi
echo "clone-leak-check: passed"
