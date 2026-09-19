#!/bin/sh
# The README's artwork, rendered from checked-in captures.
#
#   scripts/readme-shots.sh [out-dir] [baaz-binary]
#
# Defaults to docs/images, which is where the README reads them from. Every
# shot is a `--replay` of a fixture, so the whole set costs nothing: no
# server, no turn. `BAAZ_SHOT_NATIVE=1` keeps the display's own pixels
# rather than downsampling to logical ones, so the images stay crisp on a
# retina screen — the regression suite in `scripts/captures.sh` does the
# opposite, because logical pixels are what make two machines agree.
#
# The account row reads "Replay" in every one of them. That is what a
# replayed capture is, and the README says so rather than faking a login.
#
# Park the pointer outside the top-left 1440x900 first: the window renders
# there, and a hovered row or menu item leaks into the capture.
set -e
out="${1:-docs/images}"; bin="${2:-./target/debug/baaz}"
mkdir -p "$out"
export BAAZ_DETERMINISTIC=1 BAAZ_SHOT_NATIVE=1

# One window over three adopted projects, replaying a real session.
BASE="--workspace fixtures/ws/acme-web --replay fixtures/msp/transcript-real.jsonl --sidebar-fixture fixtures/sidebar/projects.json --screenshot-delay 9000"
ADOPT='project:fixtures/ws/acme-web;project:fixtures/ws/acme-internal;project:fixtures/ws/notes;project-colour:2'

shot() { # <name> <theme> <steps>
  name="$1"; theme="$2"; steps="$3"
  # shellcheck disable=SC2086
  BAAZ_STATE_DIR="$(mktemp -d)" $bin $BASE --theme "$theme" --steps "$steps" \
      --screenshot "$out/$name.png" >/dev/null 2>&1 || echo "FAILED $name"
}

shot readme-hero-dark      dark  "$ADOPT"
shot readme-hero-light     light "$ADOPT"
shot readme-projects-dark  dark  "$ADOPT;project-menu"
shot readme-composer-dark  dark  "$ADOPT;command"

# The approval card wants the capture that stops while it is still waiting.
for t in dark light; do
  BAAZ_STATE_DIR="$(mktemp -d)" "$bin" --replay fixtures/msp/transcript-approve-stage1.jsonl \
      --workspace fixtures/ws/acme-web --theme "$t" --screenshot-delay 5000 \
      --screenshot "$out/readme-approval-$t.png" >/dev/null 2>&1 || echo "FAILED approval $t"
done

# The welcome screen: signed out, no server.
BAAZ_STATE_DIR="$(mktemp -d)" "$bin" --no-connect --login choose --theme dark \
    --screenshot-delay 4000 --screenshot "$out/readme-welcome-dark.png" >/dev/null 2>&1 \
    || echo "FAILED welcome"

ls "$out"/readme-*.png
