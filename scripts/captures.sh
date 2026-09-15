#!/bin/sh
# Deterministic capture set: every replay fixture × both themes, plus the
# login screens. Byte-identical run to run under HARNESS_DETERMINISTIC=1
# (docs/02-app.md), so `cmp` between two runs is the regression proof.
# Park the pointer outside the top-left 1440x900 first: a capture renders
# there, and a hovered card (a code block's toolbar, a row) leaks into it.
#   scripts/captures.sh <out-dir> [harness-binary]
set -e
out="${1:?out dir}"; bin="${2:-./target/debug/harness}"
mkdir -p "$out"
export HARNESS_DETERMINISTIC=1
for f in fixtures/msp/transcript-*.jsonl fixtures/msp/synthetic-*.jsonl; do
  name=$(basename "$f" .jsonl)
  for t in dark light; do
    HARNESS_STATE_DIR="$(mktemp -d)" "$bin" --replay "$f" --theme "$t" --screenshot "$out/$name-$t.png" >/dev/null 2>&1 || echo "FAILED $name $t"
  done
done
for l in choose device apikey error apikey-error; do
  HARNESS_STATE_DIR="$(mktemp -d)" "$bin" --no-connect --login "$l" --theme dark --screenshot "$out/login-$l-dark.png" >/dev/null 2>&1 || echo "FAILED login $l"
done
# --- Projects, package 2 (docs/12-projects.md) ---
# One window over three adopted roots plus two strays, then each surface in
# turn. Every capture runs twice from a fresh state dir and `cmp`s the pair:
# byte-identical run to run is the gate. `set -e` is on, so the comparison
# guards with `||` rather than failing the run.
# shellcheck disable=SC2086
proj_base="--workspace fixtures/ws/acme-web --replay fixtures/msp/transcript-real.jsonl --sidebar-fixture fixtures/sidebar/projects.json --screenshot-delay 15000"
proj_steps='project:fixtures/ws/acme-web;project:fixtures/ws/acme-internal;project:fixtures/ws/notes;project-colour:2'
proj_shot() { # <name> <theme> <steps>
  name="$1"; theme="$2"; steps="$3"
  HARNESS_STATE_DIR="$(mktemp -d)" "$bin" $proj_base --theme "$theme" --steps "$steps" --screenshot "$out/$name.run1.png" >/dev/null 2>&1 || echo "FAILED $name run1"
  HARNESS_STATE_DIR="$(mktemp -d)" "$bin" $proj_base --theme "$theme" --steps "$steps" --screenshot "$out/$name.run2.png" >/dev/null 2>&1 || echo "FAILED $name run2"
  if cmp -s "$out/$name.run1.png" "$out/$name.run2.png"; then
    mv "$out/$name.run1.png" "$out/$name.png"
  else
    echo "NONDETERMINISTIC $name"
    mv "$out/$name.run1.png" "$out/$name.png"
  fi
  rm -f "$out/$name.run2.png"
}
proj_shot projects-sidebar-dark dark "$proj_steps"
proj_shot projects-sidebar-light light "$proj_steps"
proj_shot round2-pinned-dark dark "$proj_steps;pin:s-web-1"
proj_shot projects-header-menu-dark dark "$proj_steps;project-menu"
proj_shot projects-group-menu-dark dark "$proj_steps;project-menu:acme-internal"
proj_shot projects-palette-dark dark "$proj_steps;projects"
proj_shot projects-rail-dark dark "$proj_steps;sidebar"
proj_shot projects-remove-dark dark "$proj_steps;remove-project:notes"
proj_shot projects-search-dark dark "$proj_steps;search:acme"
# The hero: no projects at all, past the login screen on sample identity.
HARNESS_STATE_DIR="$(mktemp -d)" "$bin" --no-connect --login signed-in --no-project --theme dark --screenshot-delay 15000 --screenshot "$out/projects-hero-dark.run1.png" >/dev/null 2>&1 || echo "FAILED hero run1"
HARNESS_STATE_DIR="$(mktemp -d)" "$bin" --no-connect --login signed-in --no-project --theme dark --screenshot-delay 15000 --screenshot "$out/projects-hero-dark.run2.png" >/dev/null 2>&1 || echo "FAILED hero run2"
if cmp -s "$out/projects-hero-dark.run1.png" "$out/projects-hero-dark.run2.png"; then
  mv "$out/projects-hero-dark.run1.png" "$out/projects-hero-dark.png"
else
  echo "NONDETERMINISTIC hero"
  mv "$out/projects-hero-dark.run1.png" "$out/projects-hero-dark.png"
fi
rm -f "$out/projects-hero-dark.run2.png"
ls "$out" | wc -l
