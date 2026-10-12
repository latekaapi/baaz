# 35 — Handoff: v3 design is FINAL. Reconcile it with FF, agentic-ui and Baaz, then continue Two Tracks

Written 2026-10-12 (late). For a fresh session. Supersedes docs/34 (that proposal was rejected; this one was
accepted by the owner on the canvas, with two comment rounds folded in).

## 0. What is final

Canvas **"Baaz v3"**: https://claude.ai/artifact/7SwC39oc2nEakd4TpCdp8e (private; read it with the Artifact tool,
`project/canvas.json` first). Seven boards: `Main` (Tone, three ladders), `Depth` (four recipes + the ladder),
`Composers` (A chosen, B rejected), `SessionsOpen`, `SessionsClosed`, `BrowseOpen`, `BrowseClosed`. The four screens are
composed from captures of the built app (2026-10-12 test bundle, replayed transcript fixture
`fixtures/msp/synthetic-markdown.jsonl`, Browse on fluidfunctionalism.com/docs/surfaces), recoloured to the new ladder;
only the surfaces, chrome, pills and composer are drawn. Generator + assets: `docs/34-refs/v3-redo/canvas/`
(`gen_canvas.py` writes `project/*.dc.html` from `blobs.json`; `assets/` are the recoloured crops; the earlier
HTML-render kit is `docs/34-refs/v3-redo/gen.py`). Nothing is committed; never push.

### The system, in values (dark; light is not designed yet, see §4)

| Level | Hex | Used for |
|---|---|---|
| Frame | `#121215` | window background, the sidebar, every gap |
| Sheet | `#1B1B1F` | the work area: transcript, settings pages, the right pane's body; in Browse the page itself (white) |
| Raised | `#232328` | composer card, user turn, code block, tables and cards inside the transcript |
| Raised, lit | `#2A2A30` | code block header, hover on a raised surface |
| Floating | `#2C2C32` | menus, pills, popovers, toasts, the peek sidebar, the active mode-switch segment |
| Dialog | `#35353C` | dialogs over a 45 % black scrim |
| Well | `#151518` | an input or well cut into a raised surface |

- Text `#F2F2F4` / muted `#9D9DA6` / faint `#6E6E78` (mono counts only). Never an opacity step of text.
- Overlays, surface-relative: hover white 5 %, selected 9 %, pressed 12 %. Lines inside a surface (table rules,
  menu separators) white 7 %.
- **One edge for every surface:** `inset 0 1px 0 rgba(255,255,255,.07)` (light from above) +
  `inset 0 0 0 1px rgba(255,255,255,.06)`. No drawn grey borders anywhere. Exception: the white page sheet carries
  `0 0 0 1px rgba(0,0,0,.5)` because a white highlight cannot show on white.
- **Shadows:** nested surfaces cast none. Floating: the edge + `0 8px 24px -10px rgba(0,0,0,.4)`. Dialog: the edge +
  `0 24px 56px -16px rgba(0,0,0,.5)`. That is the whole shadow system (two drops).
- Radii: sheet 8, raised 12 (composer, user turn), menus 10, pills full, controls 6. Mode-switch segment 4 in a 6 box.
- Geometry: the sheet is 6 px in from the window on top, right and bottom, 6 px from the sidebar column (sidebar 256
  → sheet left at 262). **The sheet never moves**; open/close only removes the sidebar column.
- Sidebar: on the frame, no vertical divider, no footer rule, no edge of its own. Lights at (13,16), Hide-sidebar at
  x 85, the Sessions/Browse switch at x 186, rows as built.
- Sessions open: the session title row (32 px from the sheet top, so it is centred on the lights' line at window y 22;
  title semibold 13 at x 16 inside the sheet, mirroring the action glyphs 16 from the right; actions right) sits inside
  the sheet; the transcript scrolls under it through a 64 px fade to the sheet colour.
- Closed (both modes): two pills, 36 px tall, fully round, floating level, 18 px from the window corners. Left: lights
  (moved in via `set_traffic_light_position`), Show sidebar, the mode switch, title (semibold 600, as the open row).
  Pill padding 4; text and lights end 14 from a pill's end (4 + 10). Right: the header's
  actions. Browse adds back / forward / reload and the address at rest (shield · title semibold · host muted) to the
  left pill and Ask · ⋯ · + to the right. **No tab strip when closed**; tabs are one hover away in the peek.
  Content runs under the pills (fade 92 px).
- Composer (direction A): one raised card, radius 12, on the transcript measure (880 wide, flush with the code block
  and table edges), 18 px above the window bottom (12 inside the sheet; the pills sit 18 from the top). FF InputMessage
  padding: 8 around, the field padded 8 (text at 16, 14 px subtitle role), 4 between field and controls. Row 1
  the field. Row 2: `+` icon button, provider as mark + name + chevron (lit), token count in faint
  mono; right: model, mode, effort as text controls with chevrons (muted, lit on hover, 28 px, radius 6); one round
  28 px send button at floating level with the one edge. Controls gap 4, icon-to-label 6. **No bordered chips.**
- Peek: the sidebar as a floating card (edge + the floating drop), radius 8, under the chrome row, 6 px from the left
  and bottom, sliding on `spring.moderate` / exit tween (docs/33 §0 has FF's timings; unchanged).

### Polish pass (owner-approved 2026-10-12, canvas v9)
One edge on every surface incl. the send button and the mode-switch segment (no private drops; the switch track is
the 5 % overlay); weights 400/600 only; the title row on the lights' line; the composer on the 880 measure with FF
padding; pill end caps 14; Browse open shares Sessions' drawn chrome and footer. Values above already include it.

## 1. Why these values (so the reconciliation does not undo them)
- A seven-unit RGB step between near-black greys is ~3 L*; the eye needs 4–5 to see a level without a line. FF's
  ladder therefore needed its rings to read, and the rings read as borders. The v3 ladder starts darker (Claude
  Desktop's sidebar is #111111) and steps nine units, so fill alone separates; the edge is a hint.
- Two to three units of blue in every grey: dead grey → alive, never readable as blue (Claude Desktop's side panel is
  #131215; Linear's chrome sits in the same place).
- Only two kinds of depth: in (fill + top highlight) and floating (one soft drop). FF's stacked 18 % drops and
  black outer rings were what the owner called "too much dark border and shadows" (canvas comment, resolved).
- The owner answered the four tone questions "all of the above" (too little contrast, dead greys, edges drawn on,
  card framing wrong) and picked Claude Desktop as the composer reference.

## 2. Reconcile with FF (docs/31, docs/31a) — write the deltas down, do not re-derive
Keep from FF unchanged: the substrate + offset model (popup = substrate + 2, dialog = substrate + 4), surface-relative
hover/selected overlays, springs (fast/moderate/slow + exit tweens), fluid hover, type roles and weights, size ladder
(36/28 controls, 32 sidebar rows), scrollbar, tooltip (inverted, no shadow), focus ring #6B97FF, radii one step tighter
than FF (already an owner-accepted adaptation).

Replace: the eight-step neutral ladder (#171717…#484848) → the seven levels above; the five dark shadow recipes →
the one edge + two drops above; `--border` 12 % grey lines on surfaces → none (lines only inside a surface at 7 %);
sidebar `variant="sidebar"` hairline → none; FF inset card `m-2` radius 12 → 6 px inset radius 8; FF composer
"s2 + shadow-2" → raised card with text controls. Update docs/31a §0 with a "v3 overrides" table rather than editing
FF's measured values, so the source of each number stays clear.

## 3. Reconcile with agentic-ui and Baaz (what exists, where)
- Tokens: `agentic-ui/crates/aui-tokens/src/surface.rs` holds the FF `LADDER` (1–8), `lift(substrate, offset)` and
  `SHADOWS` (1–5). Map: frame = level 1, sheet = 2, raised = 3, raised-lit = 4, floating = 5, dialog = 6, well = a new
  entry (or level 0). Keep `lift()`; popups keep offset 2 (sheet → floating is two steps: 2 → 4? no: popups must land
  on **Floating** whatever the substrate, so make the popup level `max(substrate + 2, floating)` and the dialog level
  `dialog`). Shadows become two: `float` and `dialog`; the edge becomes part of every surface's paint.
- Theme JSON / gpui: **corrected 2026-10-12 (late)** — gpui-pre 0.3.3 *does* draw inset box-shadows
  (`BoxShadow::inset`, `shaders.metal:472/545`, painted after the background and before the border by
  `paint_inset_shadows`, `style.rs:740` — CSS order; zero blur is an exact 1 px SDF edge). FF's dark recipes have used
  it since F1. So the edge is the CSS value itself, two inset layers, no adaptation. (Also: the tokens are not in
  `aui-tokens/src/surface.rs`, which doesn't exist; they are `design/tokens/tokens.json` → `build.rs` → `Palette`.)
- Shell: `agentic-ui/crates/aui/src/shell/app_shell.rs` has `.inset(bool)` (Browse uses it, Sessions does not),
  `edge_control` (collapsed toggle at x 84) and `centre_clear`. v3: inset for both modes, 6 px, radius 8; the sheet's
  own header row inside it for Sessions; the collapsed state becomes the two pills (overlay windows in Browse, per
  memory `feedback-native-page-above-gpui`; in Sessions they can be gpui children). `Window::set_traffic_light_position`
  (gpui-pre 0.3.3 `window.rs:2740`) moves the lights into the pill at the end of the close and back at the start of the
  open; whether it tolerates per-frame stepping is untested.
- Sidebar: drop the right hairline and the footer rule (`sidebar` views in aui + Baaz `app.rs`); selected row stays
  the 9 % overlay.
- Composer: Baaz's composer chips (`ChipButton`/`cc` style: 28 px bordered) → text controls; the `+` stays an icon
  button; send becomes the round 28 px button. Picker menus unchanged (FF dropdown, now at Floating).
- Browse: page card inset 8 → 6, radius 10 → 8, its edge the 50 % black ring; closed = pills (overlay windows),
  no tab strip; the page window takes every frame of the open/close with the sidebar (docs/33).
- Transcript blocks (code block, table, user turn) take Raised with the edge; today they draw `--border` lines.
- Motion from docs/33 §0 (collapse on FF slow / slow.exit, peek on hover, mode-switch cross-fade) is unchanged and
  still unbuilt; fold it into the same build.
- Probe: `design/baseline/` predates design-v2; the 18-entry probe will flag every screen until baselines are
  regenerated **on main after merge** (CLAUDE.md). Expect that; do not regenerate in a worktree.

## 4. Open, deliberately
- Light theme: not designed. The rule to apply when it is: frame #F2F2F4-ish, sheet white, raised white + the drop
  shadows carry depth (FF's light rule), edge = 6 % black ring. Design it on the canvas before building.
- Right pane, Settings, ⌘K, approval cards, toasts, terminal, new tab: inherit the ladder (right pane = a second
  sheet 6 px to the right; ⌘K = dialog level; toast = floating) — not drawn yet. Draw the right pane and Settings on
  the canvas before their build; the rest can follow the rule.
- The code block well: shown as Raised in the screens (its captured pixels); the table says Well for inputs only.

## 5. Suggested order for the next session
1. Read: this doc, the canvas (all seven boards), docs/31 §6–8, docs/31a §0 + §9, docs/33 §0, memories
   `project-v3-surfaces-2026-10-12`, `feedback-ff-is-the-spec`, `feedback-prototype-in-agentic-ui`,
   `feedback-native-page-above-gpui`, `feedback-count-concepts-before-building-ui`.
2. Write the reconciliation: docs/31a "v3 overrides" table; docs/31 §9 "v3 as built" plan (tokens → shell → sidebar →
   composer → transcript blocks → Browse card → pills → peek/motion), each with its gate.
3. Build in agentic-ui first (tokens + a gallery card the owner can drive: both modes, open/closed, pills, peek), get
   the owner's eyes on it live, then Baaz. Gates per CLAUDE.md: `cargo test` both repos, clippy `-D warnings`,
   `cargo build -p baaz` then the 18-entry probe, a live check in the bundle with real input, front-most.
4. Commit per step (never push). Update the "Baaz Two Tracks" tracker, then continue it.

## 6. Traps carried forward
- Captures: the app remembers per-mode sidebar state across runs with the same `BAAZ_STATE_DIR`; "open" and "closed"
  captures can swap. Look before naming. Capture method: memory `project-v3-surfaces-2026-10-12`.
- The canvas refuses a publish until a plain `read` of the artifact has happened in the session; the owner's own
  edits bump the version — re-read `project/canvas.json` right before publishing and change only your keys.
- Chrome headless renders the boards for a look before publishing: `preview/*.html` from the generator, images
  from `assets/`; `--force-device-scale-factor=2` for the 2× look.
