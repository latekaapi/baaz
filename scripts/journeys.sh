#!/bin/sh
# Every user journey `--replay`, `--no-connect` and `--steps` can drive,
# scripted so it reruns. Free — nothing here reaches `turn/start`.
#
#   scripts/journeys.sh <out-dir> [baaz-binary]
#
# Each journey runs twice from a fresh state dir and the pair is compared:
# a journey passes when both runs produce a capture and the two are
# byte-identical, which is the only pass/fail a screenshot can give on its
# own. Park the pointer outside the top-left 1440x900 first — a capture
# renders there, and a hovered row leaks into it.
set -e
out="${1:?out dir}"; bin="${2:-./target/debug/baaz}"
mkdir -p "$out"
export BAAZ_DETERMINISTIC=1

pass=0; fail=0
report="$out/journeys.txt"; : > "$report"

WS_A=fixtures/ws/acme-web
WS_B=fixtures/ws/acme-internal
WS_C=fixtures/ws/notes
FIX=fixtures/sidebar/projects.json
REPLAY=fixtures/msp/transcript-real.jsonl

# journey <name> <steps> [extra args…]
# Runs the same steps twice from two fresh state dirs and compares.
journey() {
  name="$1"; steps="$2"; shift 2
  for run in 1 2; do
    BAAZ_STATE_DIR="$(mktemp -d)" "$bin" \
      --workspace "$WS_A" --replay "$REPLAY" --sidebar-fixture "$FIX" \
      --screenshot-delay 12000 --theme dark --steps "$steps" "$@" \
      --screenshot "$out/$name.run$run.png" >/dev/null 2>&1 || true
  done
  if [ ! -f "$out/$name.run1.png" ] || [ ! -f "$out/$name.run2.png" ]; then
    echo "FAIL  $name  (no capture)" >> "$report"; fail=$((fail+1))
    rm -f "$out/$name.run1.png" "$out/$name.run2.png"; return 0
  fi
  if cmp -s "$out/$name.run1.png" "$out/$name.run2.png"; then
    mv "$out/$name.run1.png" "$out/$name.png"; rm -f "$out/$name.run2.png"
    echo "PASS  $name  $out/$name.png" >> "$report"; pass=$((pass+1))
  else
    # Keep BOTH captures on a mismatch. A journey that fails here is usually
    # unreproducible on its own, so the pair is the only evidence of what
    # differed — deleting it costs a whole suite run to get back.
    mv "$out/$name.run1.png" "$out/$name.png"
    echo "FAIL  $name  (not byte-identical run to run)  $out/$name.png vs $out/$name.run2.png" >> "$report"
    fail=$((fail+1))
  fi
}

# --- boot and adoption -------------------------------------------------
# No project adopted: the hero is what a first run shows.
journey hero ""
journey adopt-one "project:$WS_A"
journey adopt-two "project:$WS_A;project:$WS_B"
journey adopt-three "project:$WS_A;project:$WS_B;project:$WS_C"

# --- switching ----------------------------------------------------------
PROJ="project:$WS_A;project:$WS_B;project:$WS_C"
journey switch-header-menu "$PROJ;project-menu"
journey switch-palette "$PROJ;projects"

# --- project edits ------------------------------------------------------
journey project-colour "$PROJ;project-colour:4"
journey project-menu-named "$PROJ;project-menu:acme-web"
journey remove-dialog "$PROJ;remove-project:notes"

# --- session rows -------------------------------------------------------
journey session-open "$PROJ;open:s-web-2"
journey session-pin "$PROJ;open:s-web-2;pin"
journey session-rename "$PROJ;open:s-web-2;rename:Renamed in a journey"
journey session-archive-dialog "$PROJ;open:s-web-2;archive"
journey session-archived "$PROJ;open:s-web-2;archive;archive-confirm"
journey session-show-archived "$PROJ;open:s-web-2;archive;archive-confirm;show-archived"
journey session-hidden "$PROJ;hidden"
journey session-empty "$PROJ;empty"

# --- grouping and search ------------------------------------------------
journey group-by-date "$PROJ;group-by:date"
journey group-by-project "$PROJ;group-by:project"
journey search-all "$PROJ;search:router"
journey search-empty "$PROJ;search:zzzznotathing"

# --- menus --------------------------------------------------------------
journey menu-overflow "$PROJ;overflow"
journey menu-view "$PROJ;view-menu"
journey menu-account "$PROJ;account"
journey menu-model "$PROJ;model"
journey menu-effort "$PROJ;effort"
journey menu-mode "$PROJ;mode"
journey menu-plus "$PROJ;plus"
journey menu-command "$PROJ;command:"
journey menu-mention "$PROJ;mention:"
journey palette-command "$PROJ;palette"

# --- sidebar shape ------------------------------------------------------
journey sidebar-collapsed "$PROJ;sidebar"
journey sidebar-narrow "$PROJ;sidebar-width:200"
journey sidebar-wide "$PROJ;sidebar-width:420"

# --- transcript ---------------------------------------------------------
journey transcript-top "$PROJ;top"
journey transcript-mid "$PROJ;mid"
journey transcript-end "$PROJ;end"
journey transcript-groups "$PROJ;expand-groups"
journey composer-draft "$PROJ;draft:a drafted prompt"

# --- approvals and questions, on their own captures ---------------------
# These replace the fixture above, so they take the journey body inline.
capture_replay() { # <name> <capture> <steps>
  name="$1"; capture="$2"; steps="$3"
  for run in 1 2; do
    BAAZ_STATE_DIR="$(mktemp -d)" "$bin" --replay "$capture" --theme dark \
      --screenshot-delay 12000 --steps "$steps" \
      --screenshot "$out/$name.run$run.png" >/dev/null 2>&1 || true
  done
  if [ -f "$out/$name.run1.png" ] && cmp -s "$out/$name.run1.png" "$out/$name.run2.png"; then
    mv "$out/$name.run1.png" "$out/$name.png"; rm -f "$out/$name.run2.png"
    echo "PASS  $name  $out/$name.png" >> "$report"; pass=$((pass+1))
  else
    [ -f "$out/$name.run1.png" ] && mv "$out/$name.run1.png" "$out/$name.png"
    rm -f "$out/$name.run2.png"
    echo "FAIL  $name" >> "$report"; fail=$((fail+1))
  fi
}

capture_replay approval-pending fixtures/msp/transcript-approve-stage1.jsonl "wait:3000"
capture_replay approval-chosen  fixtures/msp/transcript-approve-stage1.jsonl "wait:3000;choose:1"
capture_replay approval-settled fixtures/msp/transcript-approve.jsonl "wait:3000"
capture_replay question-open    fixtures/msp/transcript-real.jsonl "wait:3000"
capture_replay markdown         fixtures/msp/synthetic-markdown.jsonl "wait:3000"
capture_replay toolshapes       fixtures/msp/synthetic-toolshapes.jsonl "wait:3000"
capture_replay toolgroup        fixtures/msp/synthetic-toolgroup.jsonl "wait:3000;expand-groups"
capture_replay error-retry      fixtures/msp/synthetic-error-retry.jsonl "wait:3000"
capture_replay todo-goal        fixtures/msp/synthetic-todo-goal.jsonl "wait:3000"
capture_replay reasoning        fixtures/msp/synthetic-reasoning-text.jsonl "wait:3000"
capture_replay empty-session    fixtures/msp/synthetic-empty.jsonl "wait:3000"
capture_replay finishing        fixtures/msp/synthetic-finishing.jsonl "wait:3000"

# --- login, with no wire ------------------------------------------------
login_shot() { # <name> <login-state>
  name="$1"; state="$2"
  for run in 1 2; do
    BAAZ_STATE_DIR="$(mktemp -d)" "$bin" --no-connect --login "$state" --theme dark \
      --screenshot-delay 6000 --screenshot "$out/$name.run$run.png" >/dev/null 2>&1 || true
  done
  if [ -f "$out/$name.run1.png" ] && cmp -s "$out/$name.run1.png" "$out/$name.run2.png"; then
    mv "$out/$name.run1.png" "$out/$name.png"; rm -f "$out/$name.run2.png"
    echo "PASS  $name  $out/$name.png" >> "$report"; pass=$((pass+1))
  else
    [ -f "$out/$name.run1.png" ] && mv "$out/$name.run1.png" "$out/$name.png"
    rm -f "$out/$name.run2.png"
    echo "FAIL  $name" >> "$report"; fail=$((fail+1))
  fi
}
login_shot login-choose choose
login_shot login-device device
login_shot login-apikey apikey
login_shot login-error error

echo >> "$report"
echo "passed $pass, failed $fail" >> "$report"
cat "$report"
