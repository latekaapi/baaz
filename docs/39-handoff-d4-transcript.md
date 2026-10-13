# 39 — Handoff: the sidebar is signed off and on main; next is D4, the transcript

Written 2026-10-13 (evening) at the end of the round-7 session. Supersedes docs/38. Read docs/36 §13–§14 for what
round 7 changed and why; the canvas "Baaz v3" (v19) is the spec; the tracker "Baaz Two Tracks" (v12) is the plan.

## Where the code is (merged and PUSHED)
| Repo | Branch | State |
|---|---|---|
| agentic-ui | `main` (the only branch) | **v0.4.1** tagged and pushed (563fd2a) |
| harness (Baaz) | `main` (the only branch) | pinned to agentic-ui v0.4.1 (c0c1908), probe baselines regenerated on main (cb3dd32), pushed |

**Git is clean (2026-10-13 evening):** both repos have only `main`, no stashes, no worktrees. Every unmerged branch
and the old stash were tagged `archive/<branch>` (dashes for slashes) and the tags pushed — 7 in agentic-ui
(orch/screens, the orch-R1..R4 lanes, D2, AUI-A11Y-2), 9 in Baaz (the muse-crates pins, MUSE-DISPLAYTEXT,
W8-TOOLJSON, Y1, `stash-relay-B8`). Restore one with `git switch -c <name> archive/<name>`. Merged remote branches
were deleted; `agentic-ui-relay-A1/` is gone.

Developing both at once: `.cargo/config.toml` (git-ignored) patches the eight aui crates to `../agentic-ui`; a build
with the patch rewrites `Cargo.lock` to the path form — **never commit the lock while patched** (`git checkout
Cargo.lock` before committing). Shipping a library change is two steps: tag agentic-ui, bump the tag in Baaz.

Test bundle: **BaazCheck is removed** (`target/bundle-check` deleted, the owner asked). `target/bundle/Baaz.app` is
the real app, rebuilt from the final main. Rebuild when a live drive is
due: copy `scripts/bundle.sh` with `ROOT`/`OUT` set to harness and `target/bundle-check` (keep the copy in the
scratchpad), run it, then `PlistBuddy -c "Set :CFBundleIdentifier sh.baaz.check"` + `"Set :CFBundleName BaazCheck"`,
`codesign -s - --force --deep`. Gate on the script's exit status. Launch with `env -i … BAAZ_STATE_DIR=$(mktemp -d
"$TMPDIR/bz.XXXX") … --login signed-in --no-connect --steps '…'` and front it with System Events.

## The sidebar, as signed off (owner, 2026-10-13)
- Open: the column full height on the frame with its row (lights 12 · toggle x 84 · switch ending 12 before the
  column's edge); the sheet 6 px in with its own 32 px title row over a 64 px fade (Claude Desktop); no app-wide
  header in either mode. Lights on the row's centre line (22).
- Closed: **nothing** — no row, no toggle, the traffic lights hidden with the column (`native_lights.rs`). A 150 ms
  rest on the window's top edge (the 6 px frame band above the sheet) or its left 12 px peeks the whole sidebar as
  the frame-coloured column, flush left/top/bottom, rounded 8 bottom-right, side drop, its row riding in; gone 250 ms
  after the pointer leaves; no re-peek after a toggle until the pointer leaves and returns. Same in both modes.
- Over a native page the peek is a hole cut through the page window (holes add up: one chained mask per hole) and
  mouse events inside a hole are forwarded to Baaz's window (aui-cef shim), so the peek's toggle and switch work there.
- Driven live by Fable (computer use on BaazCheck): every open/closed case in both modes and over a live page — pass.

## Rules learned this session (also in memory)
1. **Drive the real app yourself before any handover.** Offscreen renders cannot show native lights, clicks over a
   native page, hover or tooltips. Request computer-use access for BaazCheck (the owner approves) and run every case
   the owner would. The click-through-a-hole defect lived for weeks because nobody clicked.
2. **Never run a release build, a test build and captures at once** on this 17 GB Mac, and wrap every offscreen
   capture in a timeout: a capture from an earlier session spun at 96 % CPU for 20 h and the Mac swapped itself
   "stuck". Prune `target/debug/deps` (65 GB) when the disk is low.
3. AppKit re-lays the standard window buttons out at (9, 9) whenever one's hidden state changes: place the lights
   after hiding/showing them. `BAAZ_LIGHTS_TRACE=1` prints the close light's real centre.

## Closed after the first write of this handoff (same evening, owner round 8)
- **Peek trigger enlarged:** the whole top-left corner (sidebar width × row height) arms the peek, not only the 6 px
  top band; over a native page the corner is a *hover zone* whose pointer moves are forwarded to Baaz
  (`AppShell::on_hover_zones` → `WebviewState::set_hover_zones` → shim). Driven live over a page.
- **Corners:** gpui's `overflow_hidden` clips to a rectangle, so square children painted the sheet's rounded corners
  back in — the Sessions fade (now `rounded_tl/tr`) and the webview's native ground (now rounded to the page radius);
  the page sheet's 1 px black ring is gone. Measured live: the pixels outside the page curve now read the frame.
- **Browse dark at launch** (pre-existing, 3 of 4 cold launches on v0.4.0): Baaz covers a page while its address is
  blank, and the webview only notified the host for visible pages, so a page that committed while covered stayed
  dark until any pointer move. A moved address now always notifies (test `a_moved_address_is_reported_to_the_host`);
  6 of 6 cold launches paint.
- Canvas v23: the page sheet has no ring. Baselines regenerated with the pointer parked (an old baseline had caught
  a hover state).

## Open items to carry
1. **Homeless controls** (owner's call, not built): the Browse downloads button → the Browse sidebar's control row
   beside ✦ · ⋯; the toast chip and the feedback slot → the sheet's bottom-right in both modes (an overlay window
   above a native page). Do it with D4.
2. A dead-code sweep across the sidebar's rounds (the painted-lights gallery path, Browse's instant-only animate
   path, `render_toast_chip`/`render_feedback_header_slot` callers) — fold into the homeless-controls work, since
   those callers are exactly what moves.
3. Flaky under concurrent builds: `terminal::service::tests::unknown_sessions_are_refused` (entity-map panic);
   passes alone and in a clean run. Disk: `cockpit/target` (22 GB) is the owner's project — not touched.

## D4 — the transcript: how to start (the agreed loop)
1. Captures of the richer fixtures are already rendered: `fixtures/msp/{synthetic-toolshapes, synthetic-reasoning-text,
   synthetic-question-error, synthetic-userinput-clarify, synthetic-todo-goal, transcript-real, transcript-approve,
   synthetic-diffstat, synthetic-finishing, synthetic-error-retry, synthetic-toolgroup, synthetic-readoutput,
   transcript-terminal, transcript-1.2.1-shapes}.jsonl` — re-render with `target/debug/baaz --theme dark --replay <f>
   --screenshot <png>` (BAAZ_DETERMINISTIC=1, a fresh BAAZ_STATE_DIR each, a timeout around each).
2. Mock on the v3 canvas from those captures, at the quality of the four screens: user turn, assistant turn, code
   block, table, tool cards, thinking, approval/question cards, the folded "Worked for…" row, the composer's seam.
   Keep the ladder and the one edge (docs/35); only Raised/Raised-lit/Well inside the sheet.
3. Owner approval on the canvas (comments → Claude).
4. Build in agentic-ui (`aui::transcript::*`, `util::raised_block`), gallery card, then Baaz adopts.
5. Gates per CLAUDE.md; rebuild BaazCheck; **drive it yourself** (both modes, a live page); then the owner.

## Prompt for the next session
```
Continue the Baaz design v2 plan. Read, in order: harness docs/39-handoff-d4-transcript.md, docs/36 §13–§14,
docs/35, memory project-v3-surfaces-2026-10-12 and feedback-verify-visually-first, and the canvas "Baaz v3"
(https://claude.ai/artifact/7SwC39oc2nEakd4TpCdp8e, project/canvas.json then the boards). The tracker is "Baaz Two
Tracks" (https://claude.ai/artifact/Fr965AKBn8toZ7BcUk7wCP).

State: both repos on main only, pushed (agentic-ui v0.4.1, Baaz pinned to it); old branches are archive/* tags. Never push without asking.

Start D4 (the transcript): capture → mock on the v3 canvas → my approval → agentic-ui → Baaz → gates → rebuild
BaazCheck and drive it yourself with computer use (I approve the access prompt) → me. Give the three homeless
controls their homes in the same pass. Commit per step. FF is the spec for values, Emil's rules for motion; the
canvas is the spec for screens and any change to it comes back to me first.
```
