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
`render_v3_pills`, `render_browse_pill_parts`, `session_header_actions`, `session_header_label`, `set_browse_holes`;
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
