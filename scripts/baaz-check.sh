#!/bin/bash
# The check build: a bundle of your own under the ID sh.baaz.check, and a launcher
# that always leaves nothing behind (docs/74 §1). The owner's target/bundle is
# never read, written or quit.
#
#   scripts/baaz-check.sh build            # target/check/Baaz.app
#   scripts/baaz-check.sh run [baaz args]  # foreground; own temp state unless
#                                          # BAAZ_STATE_DIR is set
#
# However `run` ends (normal exit, failure, Ctrl-C, its own SIGTERM), the trap
# stops the pid it started, gently first, then sweeps sh.baaz.check's dead
# clones and removes its temp state.
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DIR="$ROOT/target/check"
APP="$DIR/Baaz.app"
BIN="$APP/Contents/MacOS/Baaz"

case "${1:-}" in
build)
    BAAZ_BUNDLE_DIR="$DIR" BAAZ_BUNDLE_ID=sh.baaz.check exec "$ROOT/scripts/bundle.sh"
    ;;
run)
    shift
    [ -x "$BIN" ] || { echo "baaz-check: build first: scripts/baaz-check.sh build" >&2; exit 2; }
    OWN_STATE=""
    if [ -z "${BAAZ_STATE_DIR:-}" ]; then
        OWN_STATE="$(mktemp -d "${TMPDIR:-/tmp}/baaz-check-state.XXXXXX")"
        export BAAZ_STATE_DIR="$OWN_STATE"
    fi
    PID=""
    finish() {
        trap - EXIT INT TERM HUP
        if [ -n "$PID" ] && kill -0 "$PID" 2>/dev/null; then
            kill -TERM "$PID" 2>/dev/null
            for _ in $(seq 1 40); do kill -0 "$PID" 2>/dev/null || break; sleep 0.25; done
            kill -9 "$PID" 2>/dev/null
            wait "$PID" 2>/dev/null
        fi
        # The cleanup helper polls once a second for its parent to go.
        sleep 2
        "$BIN" --sweep-clones >/dev/null 2>&1
        [ -n "$OWN_STATE" ] && rm -rf "$OWN_STATE"
    }
    trap finish EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM HUP
    "$BIN" "$@" &
    PID=$!
    wait "$PID"
    code=$?
    PID=""
    exit "$code"
    ;;
*)
    sed -n 2,12p "$0" >&2
    exit 2
    ;;
esac
