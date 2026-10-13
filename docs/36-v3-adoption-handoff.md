# 36 — Handoff: v3 adoption is built but the owner is not satisfied. Improve it.

Written 2026-10-12 (night) for a fresh session. Read this whole file first, then docs/35 (the v3 spec in values), then
the canvas **"Baaz v3"** https://claude.ai/artifact/7SwC39oc2nEakd4TpCdp8e (version 9: read `project/canvas.json`, then
the seven boards). Local-only plan notes: `docs/31-fluid-design.md` §9 and `docs/31a-ff-component-values.md` §0 "v3
overrides" (both git-excluded on purpose via `.git/info/exclude` — read them, don't try to commit them).

**The owner said "I'm not satisfied with what has been implemented so far" and did not say what.** The first thing to do
is ask, in one batch, what specifically falls short. Do not guess and rebuild. Useful prompts for that conversation are in
§6.

## 1. State (nothing pushed — never push)

| Repo | Branch | Head | Notes |
|---|---|---|---|
| agentic-ui | `design-v2` | 2f5002f | 106 commits ahead of main; relay lane A1 merged (edf9b8a) |
| harness (Baaz) | `feedback-mode` | 806974f (+ this doc) | 147 commits ahead of main |

Gates at those heads: `cargo test` + `cargo clippy --all-targets -- -D warnings` green in both repos. The 18-entry probe
(CLAUDE.md) runs and flags all 18 (baselines predate design-v2; regenerate only on main after merge).
Test bundle: `target/bundle-check/Baaz.app`, bundle id changed to `sh.baaz.check` ("BaazCheck") so it never collides with
the owner's running Baaz. Rebuild with a copy of `scripts/bundle.sh` whose `ROOT`/`OUT` point at harness and
`target/bundle-check` (the session's copy lived in a scratchpad and is gone), then
`PlistBuddy -c "Set :CFBundleIdentifier sh.baaz.check"` and `codesign -s - --force --deep`. **Gate on the script's exit
status** — two failed bundle builds went unnoticed this session.

## 2. What was built (so you know what exists before changing it)

Canvas: a polish pass (owner-approved, v9) — one edge on every surface, weights 400/600, title row on the lights' line,
composer on the 880 transcript measure with FF InputMessage padding, pill end caps 14, Browse open shares Sessions' chrome.

agentic-ui (`crates/…`):
- **aui-tokens / design/tokens/tokens.json** — dark ladder frame #121215 · sheet #1B1B1F · raised #232328 · raised-lit
  #2A2A30 · floating #2C2C32 · dialog #35353C · well #151518; text #F2F2F4/#9D9DA6/#6E6E78; overlays 5/9/12 %; `line` 7 %.
  `shadow-edge` / `shadow-float` / `shadow-dialog`; FF `shadow-N` mapped onto them in dark (1,2,4 edge · 3 float · 5–8
  dialog). `Palette::FLOATING_LEVEL`/`DIALOG_LEVEL`, `edge()`, `dialog_shadow()`; `elevation_shadow` = edge only (dark).
  Light is FF, untouched. Inset box-shadows are native in gpui-pre 0.3.3 — no adaptation needed.
- **aui-motion `open_close`** — one 0→1 value: spring open, tween closed (FF slow/slow.exit), `open_close_with` (tween
  enter), `open_close_jump` (put at rest). Executor clock.
- **aui shell `AppShell::sheets(true)`** (app_shell.rs, fn `sheets`) — sheet 6 px in, r 8, edge; 32 px title row
  (`header_centre`, `HeaderCell::bare`) over a 64 px fade (92 closed); right pane = second sheet; closed = two pills
  (`.pills(left, right)`) 36 tall, 18 from corners, fade 80/60 ms after the column closes; `.peek(el)` — 12 px strip,
  150 ms arm, 250 ms dismiss, moderate in/out, Floating card under the pills; `.native_lights` +
  `place_native_lights()` move the window's lights into the left pill; `.animate(false)` (instant); `.page_sheet`;
  `.on_overlay_bounds` (pill/peek rects each frame); `.peek_open` (capture aid). The old full-height mode still exists
  (Settings uses it) — retire it once Settings is redesigned.
- **ModeSwitch** (screens/sidebar/view.rs) — the Sessions/Browse switch as a component; track 5 % overlay, indicator
  Floating + edge. Sidebar footer rule removed.
- **Composer `raised(true)`** + `Chip::text_control()` — Composer A.
- **util::raised_block / raised_lit** — transcript/workbench/skills cards on Raised + edge (dark), light unchanged.
- **aui-webview/aui-cef `set_holes`** — rounded holes through the native page window (CAShapeLayer even-odd mask).
- **Gallery** `screens/shell-v3` — drivable card (both modes, open/closed, pills, peek).

Baaz (`crates/baaz/src`): `app.rs` root render — Settings keeps the old shell; everything else `.sheets(true)`;
`render_v3_chrome` (was `render_v3_pills`), `render_browse_pill_parts`, `session_header_actions`, `session_header_label`, `set_browse_holes`;
`browser/mod.rs` `sheet_page_rect`, `browse_page_card()` always in Browse, `browse_strip_shown()` → false (no tab strip);
`session/render.rs` composer band (no hairline, 12 above the sheet bottom); `TRANSCRIPT_PAD_TOP` 80; sidebar
`.transparent()`; step `peek`.

## 3. Decisions taken this session (the owner can reverse any)

- **Browse open/close is instant** (`animate(false)`): the native page's frame is computed from state, not the animated
  layout, and resizing Chromium per frame risks jank (owner rule: smooth or none). Sessions animates.
- **Pills/peek over the page are holes**, not overlay windows (overlay windows would hide the window's own lights).
  Consequence: over a live page the pills and peek have **no drop shadow**, and the peek strip is effectively 6 px.
- **Mode switch Sessions↔Browse stays instant** (the docs/33 cross-fade was not built).
- Transcript first turn at 80 in both states (canvas: 80 open, 100 closed) — one constant avoids re-measuring the list.
- Settings and the right pane only inherit tokens (owner's answer); light theme stays FF (owner's answer).

## 4. Known weak spots — likely sources of the owner's dissatisfaction

Nobody has seen these in the real window:
1. **Traffic lights inside the pill** (`set_traffic_light_position` at (32, 30)) — never seen; offscreen renders can't
   show native lights.
2. **Holes over a live Chromium page** — the CAShapeLayer mask, click-through on transparent pixels, the mask following
   the peek per frame — never seen live. If clicks don't pass through, the pills over a page are dead.
3. **Motion feel** — measured cadence only (frame trace: close 250 ms, open ~440 ms, ~120 Hz, gaps ≤ 10.3 ms). The trace
   says frames arrived on time; it does not prove the motion looks right (content reflow during the slide — the centre
   re-lays out every frame, docs/31 §7 item 3 — was not judged by eye).
4. **Right pane** — its file rows are heavy grey FF-secondary buttons with centred text (pre-existing since design-v2,
   looks worse on the v3 ladder). Header row inside the second sheet not designed.
5. **Settings** — old layout with new colours; title bar/header still the old shell.
6. **Composer** — chips for attachments still bordered; the context meter (ring + %) stands where the canvas shows
   "0 tokens" in faint mono; plan pill is a text control.
7. **Remaining cards** not yet raised: approval/status/question cards with status-colour borders are deliberately kept;
   terminal blocks, the right-pane workbench views, toasts' inner tiles, skills rows were not reviewed against v3.
8. The peek shows the whole sidebar including its own title bar (toggle + switch), duplicating the pill's controls.
9. The old AppShell modes (`full_height`, rail, `edge_control`, `centre_clear`) and Baaz's old Browse toolbar code paths
   remain as dead or Settings-only code.

## 5. Traps learned this session

- **The owner declined computer-use access** (gallery and BaazCheck) and was using the Mac. Do not `screencapture -R`
  the screen or launch windows with `open -F` while they work — one capture grabbed their unrelated Chrome document
  (deleted). Ask before any live driving; offscreen `--screenshot` renders (what the probe uses) are free and private.
- `bash bundle.sh > log; echo $?`-style commands hid two failed builds; the shim (`aui-cef/src/shim.m`) is only compiled
  by the release bundle, never by `cargo test`.
- Frame traces need the window frontmost (macOS stops ticking occluded windows) — otherwise one frame per toggle.
- Removing a raised block's 1 px border made a tool card render twice on its first frame (tests/diff_stat_chips) — keep
  the transparent border in `raised_block`.
- Captures: per-mode sidebar state persists per `BAAZ_STATE_DIR`; use a fresh dir per capture.
- Relay: the gate probe for aui takes ~10 min cold; muse lanes on Baaz need ~10+ GB per worktree target (disk was at
  16 GB free before a stale-cache cleanup this session).

## 6. Suggested start for the next session

1. Read this file, docs/35, the canvas, memory `project-v3-surfaces-2026-10-12`.
2. Ask the owner, in one batch: what specifically is unsatisfying (look, feel/motion, a specific screen, fidelity to
   the canvas, Browse behaviour), whether they want Browse motion back even at some risk, whether over-page shadows
   matter, and whether they will allow a live look (computer use on BaazCheck, or they drive while you watch captures).
3. Rebuild the bundle-check app, render the states offscreen (`--steps` with `sidebar`, `peek`, `mode:browse`, …), and
   compare against the canvas boards side by side before changing anything.
4. Fix in agentic-ui first (gallery card `screens/shell-v3`), then Baaz; gates per CLAUDE.md; commit per step; never push.

## 7. Update 2026-10-12 (later session): the owner's first complaints, fixed

Owner's complaints (with a Claude Desktop recording): BaazCheck "doesn't work properly at all" — the session/browse
toggle and the sidebar toggle "mixed up"; the pill with the lights drops from its open-state position (jarring); the
pill should be the sidebar's colour; hover on the toggle should bring the sidebar up (Claude Desktop); all four
mode × sidebar combinations must work and be tested; and a researched plan for Claude-Desktop-like depth gradients.

Done (agentic-ui design-v2 `437da5c` + `5493bf1`, harness feedback-mode `332436b` + this doc):
- **The chrome row never moves.** The shell draws lights (12,16), the host's toggle (x 84) and the mode switch
  (x 118) at the window's corner in both states; the sidebar screen has `title_bar(false)`. Closed, the row's ends
  are two **corner tabs on the frame colour** (one rounded corner r 8, the 6 % edge on the two sides that meet the
  sheet), left with the title / Browse page controls (FF popup fade in once the column settles), right with the
  actions at the exact spot the sheet row drew them. The pills, `PILL_*`, `PEEK_TOP` and the lights-in-pill move are
  gone; `place_native_lights(window, cx)` has no side. This is a **canvas change** (SessionsClosed / BrowseClosed
  boards show pills) — the owner asked for it; the boards are not yet updated.
- **Peek from the toggle**: resting on the toggle arms the peek like the left strip; one arm/dismiss clock across
  toggle, strip and card (`SheetsState::set_hover`, unit-tested). Opening out of a peek starts the column at rest.
- **One sidebar for both modes** (reverses B2a-H6): `set_mode` no longer restores a per-mode `sidebar_open`;
  `persist_layout` writes it into both slots. Offscreen captures proved the old fault: close in Sessions → switch to
  Browse rendered Browse-open byte for byte. Test `sidebar_state_is_shared_across_modes` covers the four
  combinations in both directions.
- Gates: aui tests + clippy `-D warnings` green; Baaz tests (1390 + suites) + clippy green; offscreen renders of
  the four states, the two cross-mode switches while closed, re-open, and the pinned peek (`--steps`), looked at.
- **Depth gradients: docs/37** — measured off the recording, three proposals, three owner decisions. Not built.

Not seen by anyone: the real lights in the corner tab (offscreen renders cannot show native lights), the hover
peek with a real pointer, the tabs as holes over a live page. BaazCheck was rebuilt from this tree (see §1 for how).

### §7 addendum (same night, later): depth built, canvas v10, tracker, QA blocked

- **Depth (docs/37) built** by my own decision as the owner delegated it: contact shadow `.16` over 20 px beside the
  open sidebar, the peek's drop turned sideways, a 24 px fade under the chrome row and at the peek card's top
  (aui `2de2fb3`, `448e0e2`). Measured on a capture: the frame goes 18 → 15 across the strip's last 16 px, the sheet's
  ring at 262 untouched.
- **Canvas v10** (gen_canvas.py, harness `54a4749`): the closed boards show the corner tabs, the open boards the contact
  shadow and list fade, notes n4/n5/n6 rewritten. **Two Tracks tracker v5** published: the sidebar is marked finished,
  D4 Transcript is "here now".
- **Trap found:** a capture that closes the sidebar on the same tick as `fixture-session:` (which also enters Browse)
  hangs forever in layout (sampled: taffy, no "capturing anyway"). `wait:800` before `sidebar` works, and
  `mode:browse;sidebar` works. Not reproduced live. Left as is; use a wait.
- **Visual QA blocked:** the Sonnet agent's `request_access` for BaazCheck was **denied** (user_denied) — no live look
  happened. BaazCheck is left running from `target/bundle-check` with the replayed transcript and a scratch state dir.
  Checked instead: offscreen renders of Sessions open/closed, Browse open/closed, the pinned peek, the four
  cross-mode switches, the contact shadow and the tabs (crops in `.relay/owner-2026-10-12/` and the scratchpad).
  Still unseen by anyone: real lights in the tab, hover peek with a real pointer, tabs as holes over a live page.
- State: agentic-ui design-v2 `448e0e2`, harness feedback-mode this commit. Gates green both repos. Nothing pushed.

## 8. Owner round 2 (2026-10-13, 2 AM): the bar — redone, not patched

Owner's notes with two side-by-side recordings (Claude Desktop vs BaazCheck): the collapse is janky, not Emil-smooth;
the peek never showed from the toggle; the peek must draw behind the lights like a real sidebar; "this header sticking
out of the bar is not looking good — make it a proper horizontal bar"; clean up the legacy code; why is the switch
beside the toggle and not at the sidebar's right edge; and the mode switch has no motion.

**What the recordings showed (120 fps, tiled every 2nd frame):** Baaz's close and open completed in 3–4 frames
(~50–70 ms) with big jumps — not a bad curve but **frame starvation**: every frame re-laid out the whole transcript
at a new width. Claude Desktop slides over ~15 frames (~250 ms). The lead's hover never armed the peek because the
toggle's own hover state swallowed the wrapper's report.

**Built (aui design-v2, harness feedback-mode, both unpushed):**
- `AppShell` rewritten (`crates/aui/src/shell/app_shell.rs`, ~700 lines, one layout): a **44 px bar** on the frame
  across the window; the sidebar column and the sheet (6 px in, from y 50) under it. Slots: `chrome_lead` (toggle, x 84),
  `chrome_switch` (at the sidebar's resting right edge, 12 in), `chrome_title` (x = sidebar width + 22, both states),
  `chrome_closed` (closed-only content in the title slot, 80/60 ms fade), `chrome_right` (16 from the right). The old
  shell (rail, `edge_control`, `centre_clear`, `full_height_sidebar`, `inset`, `header_sidebar`, `header_centre`,
  `sheets`, pills, corner tabs) is gone; `SidebarHeader` is gone; the sidebar screen has no title bar, no collapsed
  strip, no `collapsed` model flag, no `Collapse`/`SwitchMode` intents.
- **Motion**: column on an ease-out tween `cubic-bezier(.22,1,.36,1)`, 260 ms open / 220 ms close (Emil: ease-out for
  what the person asked for, no bounce on a panel that must land exactly — a deliberate deviation from FF `spring.slow`
  bounce .12). **The sheet's content is laid out once at the resting width of the target state and slides with the
  sheet's edge** (`content_w` in the shell), so no frame re-measures the transcript. The right sheet keeps its moderate
  spring with the same trick.
- **Peek**: card from y 6 to the bottom, behind the bar's controls and the window's lights, rows start under the bar
  (24 px fade), side drop; arms from the toggle by **geometry** (pointer vs the lead's last rect, frames requested while
  on it) and from the left strip; one arm/dismiss clock across toggle, strip and card.
- **Mode switch**: at the sidebar's right edge; the indicator slides on `moderate`; the sidebar's new rows fade in over
  80 ms (`Harness::mode_fade_since`, `MODE_FADE`), replacing the old translate spring.
- **Baaz**: `render_bar_chrome` → `BarChrome {lead, switch, title, closed_only, right}`; Settings on the same shell
  (`render_settings_chrome`: breadcrumb in the title slot, close at the right, no switch); `render_centre_header` →
  `render_session_title`; `render_browse_toolbar`, the horizontal tab strip (`render_browse_tab_strip`, chips, tiles,
  `BrowseTabDrag`, `BrowseTabGhost`, `reorder_browse_tab`, `Tabs::move_before`, `open_tab_menu`), the
  `BrowserTabs` setting (enum, layout field, Settings group, `settings-set:browser-tabs`, probe entry
  `browse-horizontal`) are deleted. The downloads button moved to the bar's right in Browse. `TRANSCRIPT_PAD_TOP` 80 → 32.
- Gallery: `shell/app-shell`, `shell/app-shell-native`, `screens/sidebar-collapsed` cards deleted; `screens/shell-v3`,
  the assistant screen, the browse screen and `examples/minimal.rs` ported. aui integration tests for the rail deleted.
- Canvas v11: the four screens drawn around the bar; notes n4/n5 rewritten.

**Measured (release bundle, frontmost, `BAAZ_FRAME_TRACE=1`, replayed transcript):** close = 36 frames over 282 ms,
mean gap 8.1 ms, longest 9.1 ms, longest draw 2.4 ms; open = 34 frames over 265 ms, mean 8.0 ms, longest 9.0 ms,
draw 2.2 ms. The previous build drew 3–4 frames per transition. Gates: tests + clippy green in both repos (aui
`9212e91`, harness `a18a5d8`); the 18-entry probe runs and flags all 18 (old baselines); bundle rebuilt and signed.
Note: the trace's `sidebar_w` column is the resting width, not the animated column — group by time gaps instead.

**Agent QA (2026-10-13, Sonnet with computer use on BaazCheck, access approved by the owner):** bar static across
⌘B (toggle, switch, title, lights at the same window px open and closed); the sheet grew into the column's place with
no wrap or jump at rest; the peek card showed from the toggle, behind the lights and the toggle, rows under the bar, no
toggle row inside, dismissed on leaving; hover-then-click opened the sidebar with no flash; Browse ↔ Sessions kept the
sidebar state both ways (closed stayed closed, open stayed open); `+` in the closed Browse bar made a second tab; the
contact shadow reads as a soft gradient. Not observable through the tool's ~0.5–1 s screenshot latency: the slide and
the switch's indicator/row motion — the frame trace above stands for those. One note, not a defect: on a blank new tab
the closed Browse bar shows the title "New tab" with no host, as designed for a tab without a URL.

## 9. Owner round 3 (2026-10-13, ~4 AM): the bar dropped; the sidebar's row and the sheet return; closed = a pill

Owner: "the header doesn't sit well with me. Drop it, revert back to the only sidebar and full right pane design. Just fix
the header: in the collapsed state there is just a pill/rounded-bar at the top [left] with just the traffic lights and
collapse sidebar and toggle view buttons. Title and rest can be shown when mouse is taken to the pill/bar."

**Built (aui design-v2 `4335fc9`, harness feedback-mode `97043f8`, unpushed; canvas v14 shows the hovered state):**
- Open = the v3 canvas: the sidebar column full height on the frame with the chrome row at its top (lights 12,16 ·
  toggle x 84 · switch ending 12 px before the column's edge — the owner's "right edge of the sidebar"); the sheet
  6 px in with its own 32 px title row (title at 16, actions at the right) on the lights' line, fade 64. Baaz's page
  rect, centre height and `TRANSCRIPT_PAD_TOP` (80) are back to the full-height sheet.
- Closed = the **pill**: the chrome row's ground becomes a 32-tall Floating pill with the one edge, 6 px from the
  corner, holding exactly lights · toggle · switch at the same x and y (nothing moves); it fades in with a 4 px rise
  (80 ms) once the column has settled and leaves (60 ms) before the column opens. The sheet's title row is gone when
  closed; the transcript runs under the pill through the 92 px fade.
- **Hover reveal:** resting on the pill extends it to the right (`PILL_REVEAL_W` 420 on `spring.moderate`) with the
  title — in Browse the ↩ chip, back / forward / reload and the address at rest (`chrome_closed`) — and the actions
  come in as a second pill at the top right (opacity + 4 px rise on the same spring). Leaving both lingers
  `PILL_LINGER` 250 ms, then `moderate.exit` 120 ms. Hover is read by geometry (pointer vs the pills' last rects;
  frames requested while on them). `AppShell::pill_revealed` / Baaz step `reveal` hold it for captures (instant).
- The toggle's 150 ms peek, the ease-out column (260 / 220, content laid out once), the contact shadow and the list
  fades carry over from round 2. The `chrome_*` builders are unchanged for hosts.
- Motion rules applied (FF + Emil): one spring tier for the reveal (moderate, no bounce), exits as plain tweens one
  tier quicker, nothing moves on the toggle itself, the pill's ground only fades/rises (never slides), reduced motion
  jumps.

**Gates:** tests + clippy green in both repos; captures of open / closed / closed+reveal / peek / Browse open /
Browse closed / Browse reveal / Settings looked at (`.relay/owner-2026-10-13/`); bundle rebuilt and signed.

**Open after this round:** the reveal's width is a fixed 420 px, not fitted to the title (a long title truncates,
a short one leaves room); the pill does not show the toggle's "Show sidebar" tooltip differently from open.

## 10. Owner round 4 (2026-10-13, ~5 AM): the pill is the frame; hover peeks the sidebar; no re-peek after a toggle

Owner, on a screenshot of round 3: the pill must be the sidebar's colour, "like it's a surface underneath everything
else"; drop the header-strip reveal on hover and show the whole sidebar (the peek) instead; and after a toggle, with
the pointer still on or near the button, the sidebar must not come back until the pointer moves over the button again.

**Diagnosis:** the pill sat on the Floating level with the float drop (the canvas v9 value for pills); the hover
extension was my round-3 addition; the peek re-armed the instant the column settled because the pointer was still on
the toggle it had just pressed — the shell had no notion of "left and came back".

**Built (aui design-v2 + harness feedback-mode, see the commits):**
- Pill ground = `surface_1` (the frame), no edge, no drop; still 32 tall, 6 from the corner, 80 ms fade + 4 px rise
  once the column settles, gone before it opens; over a native page it is a hole.
- Resting on the pill joins the toggle and the window's left strip as a peek source (one 150 ms clock); the reveal,
  the actions pill, `chrome_closed`, `pill_revealed` and the `reveal` step are gone. Closed, nothing but the pill is
  drawn; the peek card brings the whole sidebar (Browse: its header with the address and the controls).
- `ShellState::wait_for_leave`: when the host's open flag flips to closed while the pointer is on the toggle, the pill
  or the strip, the peek will not arm until all three are unhovered once.
- Baaz: `render_browse_bar_parts`, `browse_minimal_address_field`, the Browse back/forward/reload/downloads buttons,
  `browse_reload_or_stop`, `browse_reset_zoom`, `browse_chip_back`, the downloads badge helpers and the toolbar's
  `AddressBadge`/`AddressView`/`zoom_pill` are deleted with their tests (all were toolbar-only). **Homeless now:** the
  downloads button, the toast chip and the feedback slot in Browse (they lived in the Browse toolbar) — next session
  should give them a place in the Browse sidebar's header or the sheet row.
- Canvas v15: the closed boards show the frame-coloured pill alone; notes n4/n5 rewritten.

## 11. Owner round 5 (2026-10-13, ~6 AM): the pill is the sidebar's top strip

Owner, on a screenshot of the round-4 peek: "the pill and the sidebar should be the same width; the borders should
match the outer frame, no top borders; the rounded corners on the bottom same as the sidebar."

**Built:** the pill's ground is flush with the window's top and left, exactly the sidebar's resting width (256) and
the row's height (44), square on top, `rounded_bl` / `rounded_br` 8, the frame's colour — the sidebar's own top strip
left behind when the column goes (4 px rise + 80 ms fade as before; a hole over a native page). The peek card is the
sidebar's column: flush with the window's left, top and bottom, the sidebar's width, `rounded_br` 8, Floating with
the side drop, its rows starting under the pill. `PILL_H` is gone (the row's height is the pill's). Canvas v16.
