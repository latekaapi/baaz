# 38 — Handoff: the sidebar is finished; next is D4, the transcript (the original plan)

Written 2026-10-13, ~7 AM, at the end of the six-round sidebar session. Read docs/36 §7–§12 for what each round
changed and why; the canvas "Baaz v3" (v17) is the spec; the tracker "Baaz Two Tracks" (v11) is the plan.

## Where the code is (nothing pushed; never push)
| Repo | Branch | Head | Gates |
|---|---|---|---|
| agentic-ui | `design-v2` | see `git log -1` (round 6: square strip, frame-coloured peek) | tests + clippy `-D warnings` green |
| harness | `feedback-mode` | see `git log -1` | tests + clippy green; 18-entry probe flags all 18 (baselines predate design-v2 — regenerate on main after merge) |

Test bundle: `target/bundle-check/Baaz.app` (bundle id `sh.baaz.check`, "BaazCheck"), built by a copy of
`scripts/bundle.sh` with `ROOT`/`OUT` set (the copy lives in the session scratchpad — recreate it), then PlistBuddy
+ `codesign -s - --force --deep`. **Gate on the script's exit status.**

## The sidebar, as it stands
- Open: the column on the frame with its chrome row (lights 12,16 · toggle x 84 · switch ending 12 before the
  column's edge); the sheet 6 px in with its own 32 px title row. Rows fade under the row (24 px); the sheet casts
  a contact shadow onto the frame beside the column (docs/37).
- Closed: the row stays as the sidebar's top strip — a plain rectangle, the column's width, 44 tall, the frame's
  colour, nothing else at the top. Column motion = ease-out 260 / 220 ms with the sheet's content laid out once at
  the resting width (the frame-starvation fix; 34–36 frames per toggle measured in the bundle).
- Peek: a 150 ms rest on the strip, the toggle or the window's left 12 px slides the frame-coloured column in
  (flush left/top/bottom, rounded 8 bottom-right, a drop to its right); gone 250 ms after the pointer leaves; after
  a toggle it waits for the pointer to leave and come back. One sidebar state across modes.
- Live checks: rounds 2 and 5 passed with the Sonnet agent on BaazCheck (owner approves the access prompt on
  request); round 6 is in captures only.

## Open items to carry
1. **Homeless after the toolbar cleanup:** the Browse downloads button, the toast chip and the feedback slot in
   Browse. Give them a place (the Browse sidebar's header, or the sheet row).
2. **Checkpoint merge to main** is overdue (owner pushes). Regenerate probe baselines on main after.
3. The old relay worktree `agentic-ui-relay-A1/` still exists (its target was deleted for disk); remove the
   worktree if the lane is merged (it is: A1 → edf9b8a).
4. Disk: `harness/target/debug/deps` was 65 GB of stale artifacts; `cockpit/target` is 22 GB (owner's project —
   ask before touching).

## D4 — the transcript: how to start (the agreed loop)
1. Capture a real session in BaazCheck (replay `fixtures/msp/synthetic-markdown.jsonl` or a richer fixture with
   tool cards, thinking, a question card, the byline) — offscreen `--screenshot` renders are fine for mocks.
2. Mock on the v3 canvas from those captures, at the quality of the four screens: user turn, assistant turn, code
   block, table, tool cards, thinking, approval/question cards, the folded "Worked for…" row, the composer's seam.
   Keep the ladder and the one edge (docs/35); only Raised/Raised-lit/Well are in play inside the sheet.
3. Owner approval on the canvas (comments → Claude).
4. Build in agentic-ui (`aui::transcript::*`, `util::raised_block`), gallery card, then Baaz adopts.
5. Gates per CLAUDE.md; captures; the Sonnet agent drives BaazCheck; then the owner.

## Prompt for the next session
```
Continue the Baaz design v2 plan. Read, in order: harness docs/38-handoff-d4-transcript.md, docs/36 §7–§12,
docs/35, docs/37, memory project-v3-surfaces-2026-10-12, and the canvas "Baaz v3"
(https://claude.ai/artifact/7SwC39oc2nEakd4TpCdp8e, project/canvas.json then the boards). The tracker is "Baaz Two
Tracks" (https://claude.ai/artifact/Fr965AKBn8toZ7BcUk7wCP).

State: agentic-ui design-v2 and harness feedback-mode at their heads, both clean, nothing pushed. Never push.

First, in one batch, ask me: (a) whether the sidebar is signed off after your own look at BaazCheck, (b) where the
Browse downloads button and the toast/feedback chips should live, (c) whether to do the checkpoint merge to main
now. Then start D4 (the transcript): capture → mock on the v3 canvas → my approval → agentic-ui → Baaz → gates →
the Sonnet agent drives BaazCheck (I approve the access prompt) → me. Commit per step. FF is the spec for values,
Emil's rules for motion; the canvas is the spec for screens and any change to it comes back to me first.
```
