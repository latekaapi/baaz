# 40 — Handoff: the sidebar is COMPLETE. What remains, and how to run it

Written 2026-10-13 (night), when the owner marked the sidebar complete ("Everything looks good"). Supersedes docs/38 and
docs/39 as the entry point; read those only for history. This file is self-contained.

## 1. State — everything is on main and pushed

| Repo | Branch | State |
|---|---|---|
| agentic-ui (`~/Projects/agentic-ui`) | `main` only | **v0.4.2** (6f8b209), tagged and pushed |
| harness / Baaz (`~/Projects/harness`) | `main` only | pinned to agentic-ui **v0.4.2**, pushed; docs commit on top |

- **Git is clean.** No other branches, no stashes, no worktrees. Every old unmerged branch and the old stash are
  `archive/<branch>` tags on GitHub (7 in agentic-ui, 9 in Baaz). Restore one with `git switch -c <name> archive/<name>`.
- **Local leftovers are tidy.** Old relay run logs are packed in `.relay/runs-archive-2026-10-13.tar.gz` in each repo;
  the old `.relay/HANDOFF.md` files now point here. Build caches pruned (disk ~107 GB free).
- **Developing both repos at once:** `harness/.cargo/config.toml` (git-ignored) patches the eight aui crates to
  `../agentic-ui`. A build with the patch rewrites `Cargo.lock` to the path form — **never commit the lock while
  patched** (`git checkout Cargo.lock` first). Shipping a library change = tag agentic-ui, push the tag, bump the tag in
  Baaz's `Cargo.toml`, move `.cargo/config.toml` aside, `cargo metadata` (re-resolves the lock from git), `cargo check -p
  baaz`, commit, move the config back. Never push without the owner asking.
- **The app:** `scripts/bundle.sh` builds `target/bundle/Baaz.app` (run it from `~/Projects/harness`, not elsewhere).

## 2. The sidebar as completed (do not reopen without the owner)

Canvas **"Baaz v3"** https://claude.ai/artifact/7SwC39oc2nEakd4TpCdp8e (v23) is the spec. In words:
- **Open:** the sidebar column full height on the frame with its own 44 px row (traffic lights on the row's centre
  line · toggle at x 84 · Sessions/Browse switch ending 12 px before the column's edge). The work area is a sheet 6 px
  in, radius 8, with its own 32 px title row (title at 16, actions right) over a 64 px fade — Claude Desktop's shape;
  no app-wide header in either mode. Contact shadow beside the column; list fade under the row.
- **Closed:** nothing at all — no row, no toggle, the traffic lights hidden. A 150 ms rest on the window's **top-left
  corner** (sidebar width × row height), its top 6 px band or its left 12 px peeks the sidebar as the frame-coloured
  column with its row; gone 250 ms after the pointer leaves; no re-peek after a toggle until the pointer leaves.
  Collapse is an ease-out tween (260/220 ms) with the sheet's content laid out once; Browse is instant.
- **Window:** drag the sidebar row (or the peek's row, or the Sessions title row) to move the window; double-click to
  zoom/restore.
- **Over a native page (Browse):** the peek is a *hole* cut through the page window (holes union), mouse events in a
  hole are forwarded to Baaz, and the peek corner is a *hover zone* whose pointer moves reach Baaz. The page ground is
  opaque white; a covered page's stand-in picture never stretches.

## 3. What remains — in the owner's order (design first)

Tracker **"Baaz Two Tracks"** https://claude.ai/artifact/Fr965AKBn8toZ7BcUk7wCP (v14) has the full picture.

### Design track
1. **D4 · The transcript — next.** User turn, assistant turn, code block, table, tool cards, thinking, approval and
   question cards, the folded "Worked for…" row, the byline, the composer's seam. Inside the sheet only Raised /
   Raised-lit / Well are in play; one edge on every surface (docs/35). Fixtures for the captures: `fixtures/msp/
   {synthetic-toolshapes, synthetic-reasoning-text, synthetic-question-error, synthetic-userinput-clarify,
   synthetic-todo-goal, transcript-real, transcript-approve, synthetic-diffstat, synthetic-finishing,
   synthetic-error-retry, synthetic-toolgroup, synthetic-readoutput, transcript-terminal, transcript-1.2.1-shapes}.jsonl`.
   **In the same pass, the three homeless controls** (owner's placements): the Browse downloads button → the Browse
   sidebar's control row beside ✦ · ⋯; the toast chip and the feedback slot → the sheet's bottom-right in both modes
   (an overlay window over a native page). Then the **dead-code sweep** of the sidebar rounds (painted-lights gallery
   path, Browse's instant-only animate path, `render_toast_chip` / `render_feedback_header_slot` callers).
2. **D5 · Settings** (4 pages; Settings gets an Assistant page here, which R3 needs). Mock first.
3. **D6 · Right pane** (5 views; its file rows are still grey FF-secondary buttons). Mock first.
4. **D7 · Skills page and dialogs. D8 · App chrome.** Design v2 is done when Baaz draws nothing of its own.

### Browser track (slots in where a design screen is its prerequisite)
- **R1 · finish the Browse re-shell:** delete the old Browse shell code and the cut list (the Agents strip, the
  remaining old covers); check Browse matches the canvas and nothing freezes or blanks over a page.
- **R2 · agent core** — CDP page control, the tools, snapshots, approvals. Library only, no screens: can run in
  parallel with any design lane (good relay candidate).
- **R3 · the agent window** — ⌘J line, floating card, docked column; task folders, handoff to a project session.
- **R4 · daily driver** (history/bookmarks in the Field, Chrome import, ad blocking, Peek, Split) · **R5 · dev loop**
  (Developer Mode per port, console errors band, element picker into the composer).

### Decisions only the owner can make
- **H.264 video:** stock Chromium cannot play it; needs our own Chromium build + a licensing call (docs/24a).

## 4. The loop for every screen (agreed with the owner — do not skip steps)
1. **Capture** the real app (offscreen `--screenshot` with `BAAZ_DETERMINISTIC=1`, a fresh `BAAZ_STATE_DIR`, and a
   timeout around every capture — an unbounded capture once spun 20 h and froze the Mac).
2. **Mock on the v3 canvas** from the captures at the quality of the four screens. The canvas is the spec; **any
   change to it goes back to the owner first.** Generator: `docs/34-refs/v3-redo/canvas/gen_canvas.py` (read the
   published files before publishing over them).
3. **Owner approval** (canvas comments → Claude).
4. **Build in agentic-ui** (component + gallery card), then **Baaz adopts**. FF is the spec for values (docs/31a "v3
   overrides" for the deltas); Emil Kowalski's rules for motion (ease-out for what the person asked for; no bounce on
   panels that must land exactly; smooth or none).
5. **Gates:** `cargo test` + `cargo clippy --all-targets -- -D warnings` in both repos (aui-cef separately: `cd
   crates/aui-cef && cargo clippy --all-targets -- -D warnings`), `cargo build -p baaz`, then the 18-entry probe
   (CLAUDE.md). Exit 0 clean, 1 findings, 3 no adapter (never a pass).
6. **Drive the real app yourself** before any handover — this is the rule the owner enforced hardest. Build a test
   bundle (copy `scripts/bundle.sh` to the scratchpad with `ROOT`/`OUT` → `target/bundle-check`, run it **from
   ~/Projects/harness**, `PlistBuddy` id `sh.baaz.check` + name `BaazCheck`, `codesign -s - --force --deep`), launch
   with `env -i … BAAZ_STATE_DIR=$(mktemp -d "$TMPDIR/bz.XXXX") … --login signed-in --no-connect --steps '…'`,
   request computer-use access for `sh.baaz.check` (the owner approves), and run every case the owner would — both
   modes, over a live page. Delete the test bundle afterwards.
7. **Owner**, then commit per step; merge/tag/push only when asked.

## 5. Traps that cost time (all measured, all recent)
- **Offscreen renders are not a test.** They cannot show native lights, clicks or hover over a native page, tooltips.
- **gpui's `overflow_hidden` clips to a rectangle**, not the rounded box: a square child repaints rounded corners.
- **The native page is a separate window above gpui.** Anything over it is an overlay window or a hole; holes need
  event forwarding (clicks) or hover zones (moves). AppKit routes events by window, not by pixel.
- **A hidden page must still notify its host** when its address moves (the dark-at-launch bug).
- **AppKit resets the standard window buttons** to (9, 9) whenever their hidden state flips — place the lights after.
- **Measurement:** unfiltered `screencapture` catches other apps' windows over Baaz; sample via the computer-use
  screenshot or a window-bounded capture. The probe needs the pointer **outside the 1440×900 capture** (screen
  bottom-right) or hover states leak into baselines. A computer-use one-shot `left_click_drag` is too fast for a
  window move — press, move in steps, release.
- **This Mac (17 GB):** never run a release build, a test build and captures at once. Prune `target/*/deps`
  and `incremental` when disk runs low; `~/Projects/cockpit/target` is the owner's — never touch.
- One test flakes under concurrent builds: `terminal::service::tests::unknown_sessions_are_refused`; passes alone.

## 6. Prompt for the next session
```
Continue the Baaz plan. Read, in order: harness docs/40-handoff-after-sidebar.md, docs/35 (the v3 values), memory
project-v3-surfaces-2026-10-12 and feedback-verify-visually-first, and the canvas "Baaz v3"
(https://claude.ai/artifact/7SwC39oc2nEakd4TpCdp8e, project/canvas.json then the boards). The tracker is "Baaz Two
Tracks" (https://claude.ai/artifact/Fr965AKBn8toZ7BcUk7wCP).

State: the sidebar is COMPLETE. Both repos are on main only, pushed (agentic-ui v0.4.2, Baaz pinned to it). Never
push without asking.

Start D4 (the transcript) with the loop in docs/40 §4: capture → mock on the v3 canvas → my approval → agentic-ui →
Baaz → gates → build BaazCheck and drive it yourself with computer use (I approve the access prompt) → me. Give the
three homeless controls their homes in the same pass, then sweep the sidebar's dead code. Commit per step. If R2 (the
agent core, library only) can run in parallel via relay without slowing D4, propose it.
```
