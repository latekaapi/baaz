# 33 — Handoff: close out the fluid-design sidebar (collapse + mode-switch motion)

> **Read §0 first** (2026-10-12 session): the menu work is done and live-checked; the open/closed states are
> mocked on the canvas for iteration; nothing of the collapse/peek/mode switch is built yet. The sections after §0
> are the original handoff (still valid for the method, traps and the native-page constraint).

## 0. Session 2026-10-12 — where it stands

### State (nothing pushed — never push)
- **agentic-ui** `design-v2` @ `3497f46`; **harness** `feedback-mode` @ this doc's commit. Both trees clean.
- Gates at the last code commit: aui 664 · baaz 1502 · clippy `-D warnings` clean in both (the old gallery
  failure is fixed) · 18-entry probe = the same old-baseline findings as every round (baselines predate design-v2).
- A test bundle with all of it: `target/bundle-check/Baaz.app` (built from a copy of `scripts/bundle.sh` with
  `OUT=$ROOT/target/bundle-check/Baaz.app`, so the owner's running `target/bundle/Baaz.app` is never closed).
  The owner's `target/bundle` is still the pre-session build.

### Done this session (item 1 of the brief: the menu bug, owner screenshot 1:21 AM)
- **One dropdown** (aui `07b53ac`, `3497f46`): `aui::interact::Dropdown` is FF's dropdown and every menu renders
  through it — sidebar view options / account / Browse menus, legacy `ViewMenu`/`ViewSubmenu` (project, overflow,
  back-history, colour, skill mode), composer `+` and pickers; the `/` and `@` caret menus keep their rows but take
  the panel, labels, FluidList glide and timings. `EnterExit::POPUP` = FF 80 / 60 ms (was the v1 120). Legacy
  `AccountMenu` deleted. `Button::menu_open` = trigger holds its pressed look and drops its tooltip.
  Submenus are laid out *inside* the menu's box (an absolute submenu turned every pick into an outside press —
  Sort by / Group by never worked before).
- **⋯ page menu** (harness `627205c`, `fa730bd`): toggles (400 ms guard for the overlay's resign-key close),
  hangs 6 px under the button right-aligned, the collapsed-chrome ⋯ (was a dead placeholder) opens it too,
  ⌘B closes Browse menus, overlay windows keep a 6 px shadow margin (`SP_1` is **2 px**, not 4 — trap).
- Live-verified in the test bundle with real CGEvents, front-most: open/toggle/Escape/outside close, Copy link,
  Find, Open in Safari, collapsed ⋯, account toggle, view-options submenu pick persists, picker hover lights label.
- Trap: gpui-kit `presence` samples a never-opened element as *Exiting* — use `progress > 0`, not the phase
  (it drew closed composer menus into a deferred layer and broke cached views: arena panic in 6 tests).
- Not a bug, left alone: Claude Code chip "Opus 5" checks no picker row (full id vs alias — deliberate, providers.rs).

### Items 2 + 3 (open/collapse, mode switch): designed, NOT built
Owner decisions (asked up front): collapsed = **hidden, FF offcanvas** (no 16 px strip, both modes); **peek on
hover** (FF `peek="hover"`: 12 px edge strip, hairline on hover, arm 150 ms, dismiss 250 ms, card slides from
−108 % on `spring.moderate`, leaves on `moderate.exit` 0.12 s; ⌘B while peeking pins); mode-switch motion = my call
from FF, "needn't be overboard".

Owner corrections this session (memory `feedback-prototype-in-agentic-ui`): my first canvas boards were low-fi
frame strips with placeholder content — rejected ("why is this so different in quality?"). Motion is to be built in
agentic-ui itself and reviewed there; static states are mocked **from captures of the built app**, at the quality of
the other boards.

**Canvas "Baaz sidebar v2" v38** (https://claude.ai/artifact/GACTNjKyXA9necGvwPvovP), row "Close-out 2026-10-12":
| Board | What |
|---|---|
| `SidebarOpen.dc.html` | Sessions, sidebar open — the built screen (FF `variant="sidebar"`, hairline edge, no card; 44 px row centred on the lights at y 22; owl empty state; composer 824 wide) |
| `SidebarClosed.dc.html` | Sessions, closed — offcanvas; lights, toggle (x 85, "Show sidebar"), the Sessions/Browse switch parked at **x 115**, title from x 191, composer re-centred |
| `BrowseOpen.dc.html` | Browse, open — built sidebar (address, controls, Pinned, Today, folder, New tab, From sessions, spaces) + page in FF inset card (8 px, r 10) |
| `BrowseClosed.dc.html` | Browse, closed — row 1: lights, toggle, switch (Browse), back/fwd/reload, address at rest (title · host), Ask, ⋯, +; row 2: tab strip (as built); page full-bleed, square |
Notes `n8` / `n9` beside them carry the interaction text. The old `Collapsed.dc.html` (2026-10-09) is retitled superseded.
The old `Main.dc.html` shows Sessions in an inset card — the build does NOT (the card is Browse-only); boards follow the build.

**Open questions for the owner** (asked, unanswered): (a) the switch parked after the toggle when closed, both modes —
approve? (b) Browse closed: keep the tab strip (as built) or one row only (tabs via ⌘1–9 / the address)?

### FF facts gathered (don't re-research)
- FF sidebar (`registry/default/sidebar-core.tsx`, fetched 2026-10-12): `collapsible` = `offcanvas | none` only (no
  icon rail). Column `animate width: open ? width : 0`, AND the fixed-width panel inside animates `x: 0 ↔ −100 %` on the
  same transition (content slides with the edge, never wiped). Open = `spring.slow` (0.24 s, bounce 0.12 → aui
  `SpringKind::Slow` 685 / 46.1 / 1, ζ 0.88, peak +0.4 px at ~260 ms, within 0.5 px at 215 ms); close =
  `spring.slow.exit` = 0.16 s tween (motion's default curve for a duration-only transition: ease-in-out). Drag = 0,
  mid-drag flip = moderate. Reduced motion = 0.
- FF has **no** panel transition for tabs (TabPanel swaps); the closest primitive is the icon swap (leaving 60 ms
  ease-in, arriving 80 ms ease-out, simultaneous; + 4 px blur / 0.6 scale for glyphs). Proposed for the mode switch:
  opacity-only cross-fade of the sidebar body + centre at those timings; native page alpha follows; indicator on
  moderate (exists). Not yet shown to the owner as a running thing.
- Today's build: collapse uses the old non-FF `SpringKind::Layout` (360/32/1); content is clipped static (wiped);
  the sidebar header's mode tabs pop in late on expand (threshold) — frame review 2026-10-12 (~16 frames each way).

### How to iterate on the canvas boards
- Read the four boards with the Artifact tool (`path: project/<Board>.dc.html`), edit, publish with `root` + `files`.
  They are self-contained (no `dc-import`); the owl is asset `/_blob/32f8077c1893c70aa92dc89c186d75f3`
  (`crates/baaz/assets/mascot/hero/chill-ambient@2x.png`).
- **Look before publishing**: strip `support.js`, the `x-dc`/`helmet` tags and the script block, swap the blob for a
  `file://` path, then `qlmanage -t -s 1440 -o <dir> <file>.html` renders it (WebKit, ~1.4× crop). The canvas UI in
  the in-app browser pane is too small to judge boards.
- Reference captures of the build: launch `target/bundle-check` with `--steps "mode:sessions;new"` (or
  `mode:browse;wait:1500;tab-new:<url>`), bring front, `screencapture -R 36,41,1440,900` (window at 36,41), ⌘B for
  closed. The app remembers per-mode sidebar state — check which state you captured.
- Live-check tools were rebuilt in the session scratchpad (gone with it); sources survive in
  `/private/tmp/claude-501/-Users-latekaapi-Projects-harness/5d52b452-…/scratchpad/tools/{mouse,wins,click}.swift, runts.py`.
  `$TMPDIR/bzreal` holds one copy of real state (no search.db); delete Chromium `Singleton*` in it if CEF fails to init.

### Next
1. Iterate the four boards with the owner until approved (open questions above first).
2. Then build in agentic-ui (owner: real components, not mocks): AppShell offcanvas width + panel slide on FF slow /
   slow.exit (one value), switch parked after the toggle when closed, peek (Browse: overlay window over the page),
   mode-switch cross-fade, native page frame/alpha in step. A gallery card the owner can drive, then Baaz.
3. Update the "Baaz Two Tracks" tracker (not touched this session).

---

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
