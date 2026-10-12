# 34 — Handoff: v3 surfaces (fluid design v3) — redo the proposal to the owner's polish bar

Written 2026-10-12. For a fresh Fable session. **The proposal described here was NOT accepted. The owner said
"it's not matching the polish I'm hoping for."** Treat §4 as a first draft to improve on, not a spec.

## 0. The ask (owner, 2026-10-12, verbatim in substance)
- From now on the design work is called **fluid design** or **v3**.
- Sessions uses vertical lines/dividers; Browse shows the page as a rounded **surface** on the base (with the sidebar).
  Pick **one common look**, built on **surfaces** — FF's guide: https://www.fluidfunctionalism.com/docs/surfaces.
- From that, design the rest of Baaz's components/surfaces, including how **dropdowns, popups, popovers, dialogs**
  look on top (shadows and all).
- **Collapsed sidebar:** the header bar should not always be visible. Ideally only the traffic lights plus a few
  icons / the key title in a **fixed pill-like surface**.
- Reference: a screen recording of Claude Desktop (`~/Documents/Screenshots/CleanShot 2026-10-12 at 2.15.41 PM.mp4`,
  29 s, 3024×1898 @120 fps). "Notice how the surfaces have different shades of colour and shadows to show depth."
  Look at it, go back to FF, then come up with a **polished** design.
- Once confirmed: implement properly in agentic-ui / Baaz, then continue the "Baaz Two Tracks" tracker.

## 1. State
- Code: nothing changed. agentic-ui `design-v2` @ `3497f46`, harness `feedback-mode` @ `c3349d9` + this doc. Never push.
- The sidebar close-out (docs/33) is still unbuilt: collapse motion, peek, mode-switch cross-fade. Its closed-state
  boards (canvas v38) are **superseded** by whatever v3 decides, because the closed state changes shape.
- Canvas "Baaz sidebar v2" https://claude.ai/artifact/GACTNjKyXA9necGvwPvovP is now **v40**. Row title `t8`
  ("v3 surfaces … for review") at y 12800, seven boards, notes `n10`–`n12`:

| Board | At (x, y) | What |
|---|---|---|
| `V3SessionsOpen.dc.html` | 0, 13040 | sidebar on base, session as a card, composer lifted |
| `V3SessionsClosed.dc.html` | 1520, 13040 | card fills window, left pill (lights · toggle · switch · title) + right pill (actions) |
| `V3SessionsPane.dc.html` | 3040, 13040 | + right pane as a sibling card, model picker open at surface-4 |
| `V3BrowseOpen.dc.html` | 0, 14060 | Browse with the same card |
| `V3BrowseClosed.dc.html` | 1520, 14060 | option A: pills over the page |
| `V3BrowseClosedStrip.dc.html` | 3040, 14060 | option C: pills on a 44 px base strip, page card below |
| `V3Depth.dc.html` | 0, 15080 | the ladder as Baaz uses it + a dialog with a select + tooltip |

- Previews (headless Chrome, 1440×900, 1×): `docs/34-refs/v3-boards/*.png`. Generator: `docs/34-refs/gen.py` (Python
  writes every board from shared CSS + pieces; edit and rerun, it writes `./project/*.dc.html` next to itself).
- Reference frames from the recording: `docs/34-refs/claude-desktop/` (open + Code session, collapsed with the
  artifact side panel, Chat home, sidebar mid-peek).
- Memory note: `project-v3-surfaces-2026-10-12`.

## 2. Facts gathered (don't re-research)

### Claude Desktop, dark (pixel-sampled at 2×)
- Base / window / sidebar: **#141414**. The sidebar has a 1 px **#242424** hairline on its inner edge when open.
- The title row has **no bar**: lights, toggle, back/forward and the title sit on the base.
- Content blocks (code block, composer, branch bar) are cards: **#181818** fill + 1 px **#262626** border, radius ~10.
  The composer's settings row sits on the base *below* the card.
- The artifact side panel is a floating card that is **darker and cooler than the base: #131215**, 1 px #242426
  border, radius ~10 pt, inset 8 pt top/right/bottom. Its header row is inside it.
- Shadows are effectively invisible in dark. **Depth comes from fill + a crisp 1 px border**, not from drops.
  Note this difference: FF's dark shadow-2 ring is 2 % white, which is far fainter than Claude Desktop's border.
  This may be a big part of why the v3 draft reads flat (§3).
- Peek: the sidebar slides in over the content (frame `sidebar-peek-midway.png`).

### FF surfaces (values already in docs/31a §0; source github.com/mickadesign/fluid-functionalism @ bf9ece4)
- Dark ladder s1–s8: #171717 #1E1E1E #252525 #2C2C2C #333333 #3A3A3A #414141 #484848. Light ladder flattens to
  #FFFFFF from s3; shadow alone carries elevation there.
- Substrate rule: a container knows its level; whatever opens inside lifts from it. Popup = substrate + 2 with its
  shadow **pinned at 3**; dialog = substrate + 4, shadow-5; tabs indicator = + 3, shadow-4; composer = s2 + shadow-2
  on the page. Hover / selected = white 6 % / 10 % overlays.
- Dark shadow recipes 1–5: docs/31a lines 32–40 (inset top highlight + inset ring + outer black ring + stacked drops
  at 18 %).
- FF sidebar `variant="inset"`: shell on s1 with no border; content card `m-2` (left 0), radius 12, s2 + shadow-2.
  Baaz uses **radius 10** (canvas note t1: "radii one step tighter: rows 6, menus 10"), an owner-accepted adaptation.
- The docs page itself is demos; the token ladder is in globals.css. The page text is in docs/29-refs/06-fluid-surfaces.md.

### gpui / Baaz constraints
- **The native CEF page draws above all gpui UI** (memory `feedback-native-page-above-gpui`). Anything floating over
  a page (pills, peek, menus) must be an overlay child window. Menus already do this with a 6 px shadow margin.
- `Window::set_traffic_light_position` exists (gpui-pre 0.3.3 `window.rs:2740`), so the lights can move at runtime.
  Untested: whether it can be stepped per frame without flicker.
- aui `AppShell` already has `.inset(bool)` (`agentic-ui/crates/aui/src/shell/app_shell.rs:242`); Browse uses it,
  Sessions does not.
- Baaz is dark-only today; v3 should still say what light does (FF flattens to white + shadows).

## 3. Why the draft fell short (my own read; verify against the owner's reaction)
1. **Not built from captures.** The boards are generated HTML with invented content (a fake transcript, a fake diff
   pane, right-pane tabs styled ad hoc). The owner rejected exactly this once before ("why is this so different in
   quality?"; memory `feedback-prototype-in-agentic-ui`). Prior rule: static states are mocked **from captures of the
   built app**, motion is prototyped in agentic-ui's gallery.
2. **Depth reads flat.** s2 (#1E1E1E) on s1 (#171717) with FF's 2 %-white ring gives almost no edge. Claude Desktop
   gets its crispness from a real 1 px border (#262626) and a *darker* side panel. Choosing between "FF exact" and
   "FF ladder + a visible edge" is a design decision to put to the owner with side-by-side crops. Don't silently pick.
3. **Pills look assembled, not designed.** 36 px pill, 28 px buttons, a tinted mode-switch box inside the pill, hairline
   separators and a semibold title make them crowded and boxy. There's no reference studied for floating window chrome
   (Arc, Safari compact tabs, Claude Desktop collapsed, Linear, Figma UI3's floating panels). Study and measure one or
   two before drawing.
4. **Radii aren't concentric.** Composer r12 inside card r10, pill full-round inside card r10, menu r10, buttons 6–8.
   Needs one nested-radius rule (inner = outer − inset).
5. **Depth board is a diagram, not a design.** A cramped dialog demo, a tooltip floating at random.
6. **Every board is closed, static and dark.** Nothing for hover/press, light theme or motion (collapse, pill
   appearing, peek).
7. **Coverage is thin.** "The rest of Baaz's components/surfaces" was asked for. Not covered: Settings, the command
   palette (⌘K), approval cards, terminal, tooltips over the page, toasts, the Field, the new-tab page, and the menus as
   built (canvas `Menus.dc.html`).

## 4. The draft proposal (starting point only)
- Base s1. Sidebar on base, no edge. Every work area is a card: s2 + shadow-2, r10, 8 px gutters. No line ever
  separates two surfaces; lines only inside one (menu separators, diff).
- Inside a card: composer / user turn / chips / pills = s3 + shadow-3; menus s4 with shadow 3; dialogs s5 + shadow-5
  over a 40 % scrim; a select in a dialog s7; tooltip inverted, no shadow.
- Right pane = a sibling card at s2.
- Collapsed: the card fills the window (8 px all round), with no header bar. Left pill: lights moved inside +
  show-sidebar + mode switch + title. Right pill: the header's actions. Content scrolls under the pills with a 32 px fade.
- Peek: a floating card, s3 + shadow-5, r10, inset 8, `spring.moderate` (docs/33 has the FF timings: arm 150 ms,
  dismiss 250 ms, slide from −108 %).

## 5. Open decisions (all unanswered; ask in one batch, with visuals)
1. **Edge treatment / depth strength.** FF-exact faint ring vs a visible 1 px border like Claude Desktop vs a
   slightly larger fill step. Show crops side by side at 2×.
2. **Browse, collapsed: where the window chrome lives.**
   - **A:** pills float over the page (overlay windows; they cover the site's top-left).
   - **A′:** pills hidden at rest, revealed when the pointer touches the top edge (and on ⌘L).
   - **C:** a 44 px base strip with the pills, page card below. This is effectively a header without a fill, which
     the owner said they didn't want.

   I leaned C for Browse and pills for Sessions.
3. **Traffic lights.** Move them into the pill on collapse (one jump each way, or animated if
   `set_traffic_light_position` tolerates per-frame calls) vs keep them fixed and let the pill straddle the card's
   corner (rendered; looked messier).
4. **Right pane level.** A sibling at s2 (FF) vs a darker/cooler panel like Claude Desktop's artifact pane.
5. **Lines.** Drop every inter-surface line, including the sidebar footer rule and the session header hairline?
   Content scrolling under a fade instead.
6. **Pill contents.**
   - Sessions: is the title enough, or also the project/branch?
   - Browse: back/forward/reload + title·host + Ask/⋯/+?
   - Is the mode switch in the pill at all, or only ⌥⌘B when collapsed?
7. **Collapsed card inset.** 8 px all round (a visible base frame) vs full bleed with only the pills floating.
8. **Composer placement.** A lifted card inside the session card (draft) vs Claude Desktop's split, where the input
   is a card and the settings row sits below it.
9. **Carried over from docs/33, still unanswered:**
   - (a) The mode switch parked after the toggle when closed.
   - (b) Browse closed keeps the tab strip or not. v3 may make (b) moot.
10. **Scope of v3 before build.** Which further surfaces must be designed before implementation starts: Settings,
    ⌘K, approvals, toasts, terminal, new tab, the Field?

## 6. Suggested method for the next pass
1. Read: this doc, docs/33 §0, docs/31 §6–§8, docs/31a §0 + §9, memory `feedback-ff-is-the-spec`,
   `feedback-prototype-in-agentic-ui`, `feedback-mock-first-then-muse`, `feedback-native-page-above-gpui`,
   `feedback-design-taste`.
2. Watch the recording yourself (`ffmpeg -ss <t> -frames:v 1` at full res; the frames in `docs/34-refs` are a start)
   and study one or two more floating-chrome references.
3. Capture the **built** app in both modes, open and closed (method: docs/33 §0 "Reference captures"; test bundle
   `target/bundle-check/Baaz.app`, `screencapture -R 36,41,1440,900`). Mock on top of real captures / real
   components, not invented content.
4. Settle the decisions in §5 with the owner up front, with visuals.
5. Then revise the v3 row on the canvas (read `canvas.json` first; publish with `root` + `files`, `file_path` = one
   changed file; the canvas refuses a publish until you have done a plain `read` of the artifact this session).
6. Only after approval: build in agentic-ui (gallery card first), then Baaz, folding in docs/33's collapse /
   mode-switch motion. Gates per CLAUDE.md (tests, clippy `-D warnings`, the 18-entry probe after `cargo build -p baaz`,
   a live check in the bundle).
7. Then update the "Baaz Two Tracks" tracker (not touched in this session or the last).

## 7. Traps
- The canvas boards must stay self-contained `.dc.html`: keep `<script src="./support.js">`, the `x-dc` / `helmet`
  tags and the `data-dc-script` block. The owl is asset `/_blob/32f8077c1893c70aa92dc89c186d75f3`.
- Preview locally: strip `support.js` / `x-dc` / `helmet` / the script block, swap the blob for
  `file:///…/crates/baaz/assets/mascot/hero/chill-ambient@2x.png`, then run
  `"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" --headless=new --hide-scrollbars --window-size=1440,900 --screenshot=…`.
  This is sharper than qlmanage.
- Disk: the data volume was at 96 % (21 GB free) this session. Clear `target/debug/incremental` before builds.
