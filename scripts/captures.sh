#!/bin/sh
# Deterministic capture set: every replay fixture × both themes, plus the
# login screens. Byte-identical run to run under HARNESS_DETERMINISTIC=1
# (docs/02-app.md), so `cmp` between two runs is the regression proof.
#   scripts/captures.sh <out-dir> [harness-binary]
set -e
out="${1:?out dir}"; bin="${2:-./target/debug/harness}"
mkdir -p "$out"
export HARNESS_DETERMINISTIC=1
for f in fixtures/msp/transcript-*.jsonl fixtures/msp/synthetic-*.jsonl; do
  name=$(basename "$f" .jsonl)
  for t in dark light; do
    "$bin" --replay "$f" --theme "$t" --screenshot "$out/$name-$t.png" >/dev/null 2>&1 || echo "FAILED $name $t"
  done
done
for l in choose device apikey error apikey-error; do
  "$bin" --no-connect --login "$l" --theme dark --screenshot "$out/login-$l-dark.png" >/dev/null 2>&1 || echo "FAILED login $l"
done
ls "$out" | wc -l
