# 37 — Depth gradients: research and plan (2026-10-12, for the owner's decision)

The owner pointed at Claude Desktop: "faded gradients at the right edge of the sidebar and below the chat transcript
header — this gives it depth. I'm looking for something like this." This file is the research and the proposal.
**Nothing here is built.** The canvas "Baaz v3" is the spec; the values below go onto it only after the owner picks.

## 1. What Claude Desktop actually draws (measured off the owner's recording, 2× pixels → pt)

Recording: `CleanShot 2026-10-12 at 2.15.41 PM.mp4`, 3024×1898 @ 120 fps. Frames at 2.0 s (sidebar closed), 9.5 s
(hover peek), 18.4 s (sidebar open). Pixel scans with ffmpeg + a raw reader (no model involved).

| Thing | Value | Note |
|---|---|---|
| Content ground (transcript, title bar) | `#141414` (20) | flat, both states |
| Sidebar, open | `#1D1D1D` (29) | **lighter** than the content by 9 units — the same step size as the v3 ladder |
| Sidebar's right edge onto the content | content goes 15 → 20 over ~40 px (20 pt) | a contact shadow: ~25 % black at the edge, gone at 20 pt, ease-out (15,15,15,16,16,16,16,17,19,19 … 20) |
| Peek panel (hover on the toggle) | `#1D1D1D`, same right-edge shadow | the panel is the open sidebar, floated over the content |
| Below the title bar | scrolled text at 50 % brightness ~10 pt under the bar, 100 % at ~40 pt | a fade from the content colour to transparent, ~40 pt tall; the bar itself is the content colour, no line |
| Code block | `#181818` on `#141414`, 1 px `#262626` edge | a raised block with a drawn edge — the thing v3 replaced with the inset highlight |
| Lights / toggle | (24, 24) / x 95 pt, both states | they never move; the hover peek slides in under the bar |

So Claude Desktop has exactly **two** gradients: a contact shadow where the lighter panel meets the darker content,
and a scroll fade under the bar. Nothing else: no gradient on the sidebar's own ground, no gradient in the composer.

## 2. What v3 already has, and where the polarity differs

- v3 draws the **sheet lighter than the frame** (sheet `#1B1B1F` on frame `#121215`, 9 units), Claude Desktop draws
  the **sidebar lighter than the content**. Same step, opposite side. Claude's shadow therefore falls from the
  sidebar onto the content; the v3 equivalent falls from the sheet onto the frame — i.e. onto the sidebar's right
  edge. That is the gradient the owner saw, mirrored.
- v3 already has the scroll fade under the title row: `SHEET_FADE_OPEN` = 64 px from the sheet colour to clear
  (Claude: ~40 pt). It only shows once the transcript scrolls under the row, so an empty or short session never
  shows it — which is probably why it reads as missing.
- v3's rule (docs/35 §0, canvas note n2): "nested surfaces cast none; floating casts one drop". The sheet is nested
  in the frame, so a contact shadow is a change to the rule. FF (docs/31a) has no gradients at all: depth is the
  inset highlight + ring on dark and stacked 6 % drops on light. Fades are not in FF either (its lists clip).

## 3. Proposal — three gradients, one rule change

Rule amendment to put on the canvas if accepted: *a sheet casts one contact shadow onto the frame along the edge
where the sidebar meets it; everything else stays edge-only.* The 6 px gaps on top, right and bottom get nothing:
a gradient that short reads as a dark border, the thing the owner rejected in October.

| # | Where | What | Dark values | Light (FF rule: drops at 6 %) |
|---|---|---|---|---|
| G1 | The sidebar's right edge, open state only (the column's last 20 px, full height, under the chrome row) | contact shadow of the sheet onto the frame | linear, horizontal, `rgba(0,0,0,.20)` at the sheet's edge → `0` at 20 px, ease-out (stops .20 / .08 @ 8 px / 0 @ 20 px). Claude measures .25 at 4–5 L* contrast; our step is the same size, so .20 keeps it a touch quieter than theirs | `rgba(0,0,0,.06)` → 0 over 20 px |
| G2 | The peek card's right side (closed state, hover) | the floating drop becomes directional, so the card's right edge casts like Claude's panel | replace `0 8px 24px -10px rgba(0,0,0,.4)` with `8px 0 24px -8px rgba(0,0,0,.45)` on the peek only (menus/popovers keep the downward drop) | FF light float unchanged |
| G3 | Under the chrome row / title row, the scroll fade | already built (64 px, sheet colour → clear). Keep; add the same fade at the **top of the sidebar list** (frame colour → clear, 24 px, under the chrome row) so rows scrolling under the toggle fade like the transcript does | sheet `#1B1B1F` → clear, 64 px; sidebar: frame `#121215` → clear, 24 px | same geometry with the light grounds |

Not proposed: a gradient at the sheet's top edge in the closed state (the corner tabs already sit on the frame
colour, and a fade from the tab's bottom would double the tab's own edge), a composer shadow (it is Raised with the
edge; Claude's composer has none), any gradient on the right sheet (its left edge meets the frame gap only, 6 px).

## 4. How it would be built (after approval)

1. agentic-ui `AppShell::sheets`: G1 as a 20 px absolute strip at the sidebar column's right edge, drawn only while
   the column is open (`column > 0`), riding the column's width so it slides with the edge; token `shadow-contact`
   in `design/tokens/tokens.json` (dark `.20`, light `.06`) so the number lives with the ladder. G2 is a second
   float recipe `shadow-float-side` used by the peek card. G3b is one more fade div in the column, like the sheet's.
2. Gallery `screens/shell-v3` shows all three (open, closed + peek); the owner judges it there first
   (memory `feedback-prototype-in-agentic-ui`).
3. Baaz inherits; captures of the four states + peek beside the canvas boards; the canvas gets the rule amendment
   and the three values (owner publishes or approves the publish).

## 5. Decisions needed from the owner

1. Accept the rule amendment (one contact shadow, sidebar edge only)? Or keep "nested surfaces cast none" and take
   only G2 + G3 (floating card + fades), which need no rule change.
2. G1 strength: `.20` (proposed) or Claude's `.25`.
3. G3b (fade at the top of the sidebar list): yes / no.
