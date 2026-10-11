# 33 — Handoff: close out the fluid-design sidebar (collapse + mode-switch motion)

Written 2026-10-12 at the end of the 2026-10-11 owner rounds (docs/32 has the full history of those rounds).

## State (nothing pushed — never push)
- **agentic-ui** `design-v2` @ `8967653`. **harness** `feedback-mode` @ this doc's commit. Both trees clean.
- harness `Cargo.toml` still pins aui v0.3.21; the git-ignored `.cargo/config.toml` path-patches every `aui*` crate to
  `../agentic-ui`. aui-cef has its own target dir (`cd crates/aui-cef && cargo test`).
- Git cleaned 2026-10-12: 162 merged branches deleted in harness, 78 in agentic-ui; the clean `agentic-ui-orch`
  worktree removed (its branch `orch/screens` kept). Left for the owner (unmerged, not mine to drop): harness
  `muse-crates/v0.3.5`, `relay/MUSE-DISPLAYTEXT`, `relay/W8-TOOLJSON`, `relay/Y1`, `relay/muse-crates-v0.1.{1,4,5,8}`,
  one stash on `relay/B8`; agentic-ui `orch/screens`, `relay/orch-R1..R4`, `relay/AUI-A11Y-2`, `relay/D2`.
- Gates green at the last commit: `cargo test -p baaz` 1501 · `cargo test -p aui -p aui-webview` 642 · aui-cef 47 ·
  clippy `-D warnings` (gallery has its one old failure, `aui-gallery/src/browse/screen.rs:2235`). 18-entry probe:
  same findings as all round (baselines predate design-v2; regenerate only on main after the merge).
- Bundle `target/bundle/Baaz.app` = HEAD. Canvas "Baaz sidebar v2" https://claude.ai/artifact/GACTNjKyXA9necGvwPvovP **v31**.

## First thing: one live check the locked screen blocked
The Browse page controls' **⋯** now opens the Browse page menu (New tab, Find, Copy link, Safari, Annotate, Screenshot,
DevTools, Settings) as an overlay menu at the pointer (harness `ccff5d9`, aui `8967653`). Built, unit-tested, not yet seen:
click ⋯ over a loaded page → menu over the live page, traffic lights coloured, no rim; each row works.

## The task: close out the sidebar
1. **Open / collapse** — state, interaction and motion.
2. **Sessions ↔ Browse mode switch** — a subtle transition, Fluid Functionalism's way.

### What exists today (read before designing)
- Collapse: aui `shell/app_shell.rs` — the sidebar column animates its width; the sidebar's content is laid out at its
  resting width inside a clipping column (no re-truncation); `edge_control` pins the collapsed toggle at x 84 (the
  traffic-light gap); `centre_clear` pads the centre header by `max(0, 106 − animated width)`. The centre column
  reflows every frame (kept as FF-faithful in docs/31 §8). Toggle = ⌘B / the sidebar button (`Harness::toggle_sidebar`,
  `app.rs`). Canvas board "Collapsed · peek on hover" exists; peek is **not** built.
- docs/31 §7–§8: the earlier collapse jank diagnosis and fix. **Never reviewed frame by frame since the fix** — record it
  first (real input, front-most window; see traps) and contact-sheet it.
- Mode switch: the FF tabs indicator slides on the moderate spring (`view.rs`), but the sidebar's content and the centre
  swap instantly (`Harness::set_mode`).
- **Browse has a native page** (docs/32 "native page above gpui"): the CEF page is a child window over gpui. Any
  transition that moves, resizes or fades the centre must move/fade the page window in step: `aui_cef_set_hidden` is
  alpha 0/1 today (an animated `alphaValue` is the likely tool), and the page card turns square/full-bleed when the
  sidebar closes (`browse_page_card()`), which today happens at one frame. During a width animation each frame resizes
  the page (cheap since aui `b96b84c` removed per-resize screenshots), but Chromium re-lays the page out per frame;
  `WebviewState::set_animating` (origin follows, size waits for rest) exists for exactly this — evaluate it.

### How to work it
- **FF is the spec** (memory `feedback-ff-is-the-spec`): read FF's source for the sidebar's collapse (sidebar-core /
  sidebar component: width transition, duration, easing, icon-rail vs offcanvas) and for content transitions
  (`registry/default/lib/springs.ts`, presence/fade/translate presets) and copy the values; write any adaptation down.
- Ask the owner up front, in one batch, the design decisions (e.g. collapsed = hidden vs icon rail; peek on hover
  yes/no; what the mode switch animates — sidebar content only, or the centre too). **Mock on the canvas first**
  (motion as frame strips + a spec of durations/curves), owner approves, then build.
- You write the gpui code; Sonnet only for tests, mechanical edits and research.
- Gates per change: aui + baaz (+ aui-cef when touched) tests and clippy `-D warnings`, the 18-entry probe from CLAUDE.md
  (`cargo build -p baaz` first), then live in the bundle. Commit per fix. Update the canvas, this doc and memory.

## Live-check method (all proved 2026-10-11)
- Isolated run: `python3 <scratch>/tools/runts.py <bundle bin> "<steps>" <secs> BAAZ_STATE_DIR=$TMPDIR/bzvcheck ...`
  (timestamped stderr), bring it front with `osascript … set frontmost of (first process whose unix id is P)`, real input
  with a 15-line Swift CGEvent tool (move / click / right-click), keys with System Events, `screencapture -x` stills.
  The tools lived in the old session's scratchpad — rebuild them (sketches in docs/32 "Owner round 2026-10-11 evening").
- **A run with no real input barely draws** (23 ticks in 11 s); one real click and it runs at 60–100 fps. `screencapture`
  (still or video) and focus loss also pause a background window's frames. Measure motion front-most, after a click.
- `System Events click at` is an accessibility press gpui ignores — post CGEvents.
- Motion: `screencapture -x -V <s> -R x,y,w,h` while moving the real pointer, then ffmpeg contact sheets / per-frame
  brightness of two regions (it caught the ⌘K scrim's 20-frame lag).
- **Disk**: copying the real state copies `search.db` (1 GB) — copy once into one dir. `harness/target/debug` grew to
  100 GB: clear `target/debug/incremental` in both repos when `df` is low (no data in it). Never `pkill -f` the Baaz
  command line (kills your own shell). Rebuilding the bundle in place quits the owner's running Baaz.
- Locked screen = black captures and no frames: check `ioreg -n Root -d1 -a | grep -c CGSSessionScreenIsLocked`.
