# 43 — D6 spec: the right pane (plus the two D4 leftovers)

Written 2026-10-17 by the design session (Fable), after the owner locked the design ("Go with A + strip, redraw all
the boards and necessary components. We can lock this design", 2026-10-17). This is the handoff to the build session
(docs/40 §6). It is the source of truth; the canvas is the picture of it. **Any design question this file does not
answer goes back to the owner — never invented.**

Canvas **"Baaz v3"** https://claude.ai/artifact/7SwC39oc2nEakd4TpCdp8e, locked at **v49**. Generators (docs, not
product code) in `docs/34-refs/v3-redo/canvas/`: `gen_d4r.py` (the D4 refresh + quote flow, patches `gen_d4.py`) and
`gen_d6.py` (every D6 board; themes are CSS variables, `DARK` and `LIGHT` dicts at the top are the token tables).
Captures were taken from baaz 65f3698 with `target/debug/baaz --no-connect --login signed-in --steps '<probe steps>'
--screenshot`, `BAAZ_DETERMINISTIC=1`, a fresh `BAAZ_STATE_DIR`, `perl -e 'alarm 150; exec @ARGV'`; the probe
entries in `scripts/uiprobe.py` (`right-changes`, `right-files`, `right-files-reveal`, `right-file-preview`,
`right-file-preview-rs`, `right-browser`, `right-browser-page`, `right-closed`) plus `files-select:CLAUDE.md` and
`files-select:assets/icon-1024.png` for the Markdown and image cases.

## 0. The two D4 leftovers (docs/42 §1)

Published on the canvas at v45; **approved by the owner 2026-10-17** ("yes, both approved - go ahead").

- **Canvas refresh:** every D4 board now carries the docs/41 "Owner review of the live build" values (reading 16 / 26,
  the Claude-shaped composer, inline progress, opaque title row, list edge fades, right-side pickers' submenus open
  left). Nothing new to build: it records what shipped. "D4 · As shipped" holds the four captures it was measured from.
- **Quote flow** (row at y 17400: `D4QuoteSelect`, `D4QuoteCompose`, `D4Quote`; note n13) — new, to build:
  - **Selection bar:** on mouse-up after a selection of ≥ 1 character inside one turn, one bar appears: Floating
    #2C2C32, r 8, the edge + float drop, 32 tall, padding 3; one text control **Quote** (26 tall, r 6, padding
    0 / 10 / 0 / 8, quote glyph 14 muted, label 14.3 FG, hover 5 %). Role `toolbar`, label "Selection"; the button's
    label "Quote the selection in the composer". It hangs **6 px above the selection's first line**, its left edge at
    the selection's start, clamped inside the measure; if that would sit within 38 px of the sheet's top (under the
    title row) it hangs **6 px under the last line**. No Copy (⌘C), no shortcut.
  - Selection tint unchanged: Muse blue `#3D72F2` at 27 %. A selection never crosses a turn (C8b span selection is per
    turn); one inside a single code block or Well output is a **code quote**.
  - **Quote →** the selection clears, a quote block is added inside the composer field above the text: 2 px left rule
    white 18 %, padding 1 / 28 / 1 / 12, text 14.3 / 22 muted clamped to 2 lines, ✕ (20, r 4) top-right, label
    "Remove quote". Code quote: mono 12.65 / 20 at `#C9C9D0`, clamped to 3 lines, then `rust · 2 lines` in faint 12.1.
    Several quotes stack 8 apart in the order added; quoting text already quoted adds nothing. The field takes the
    keyboard (edge 22 %), caret after any draft.
  - A cross-block selection is one quote, its text kept as Markdown (list markers, a fence for code, `| cells |` for
    table rows); the block shows it flattened.
  - **Sent:** the D4 quote block at the top of the bubble, at the shipped scale: 2 px rule 18 %, 14.3 / 22 muted, 3
    lines, "from the reply above" faint 12.1 (−4 / 8 margins); code quotes mono, 4 lines, "from the reply above · rust".
    Clicking it scrolls to the source (ease-out 260 ms) and flashes the source span's tint once.
  - The bar rides with the text while scrolling, hides when its anchor is under the title row or the composer, and
    leaves on Esc, a click elsewhere or an empty selection. Enter 80 ms / exit 60 ms (`EnterExit::POPUP`).

## 1. Boards, by state (D6)

Rows from y 19400 (title t8). Blue notes n15 (the design in prose), n16 (widths and the defaults), n17 (options).

| State | Board | Source |
|---|---|---|
| The pane today (four captures) | `D6Today` | right-changes, right-files, right-file-preview-rs, right-browser |
| Choosing the pane: transcript-row toggles, pane rows, pushed-view row, narrow, light | `D6Header` | — |
| Changes with a file open + commit box | `D6Changes` | fixture git status (right-changes) |
| Files: tree, selected, hover | `D6Files` | right-files |
| File preview, revealed lines | `D6Preview` | right-file-preview-rs (`right.rs` 2949…) |
| Page with the agent pill | `D6Page` | right-browser-page + Browse page capture |
| Full output | `D6Output` | new (cargo test output) |
| Image | `D6Image` | new (an attachment) |
| Create PR | `D6PRForm` | new |
| Changes in light | `D6ChangesLight` | — |
| Wide 720 (sidebar closed), Narrow 280 | `D6Wide`, `D6Narrow` | right-width:720 / 280 |
| Files + preview: rest, hover, selected + keyboard ring, filter, capped folder, code reveal, Markdown, not shown, loading, gone, no project, loading tree | `D6StatesFiles` | |
| Changes + git: closed rows, file open, scrolled (sticky row), committing, hook failed, clean-not-pushed, pushed-no-PR, PR open, Create PR, loading, not a repo, no project | `D6StatesChanges` | |
| Page: page + pill, annotating, loading, no page, couldn't load, narrow | `D6StatesPage` | |
| Output: done, running, fetching · Image: shown, missing, narrow | `D6StatesViews` | |
| Light: Files, Preview, Changes, Output, hook failed, PR open, Create PR, Page | `D6Light` | |
| The strip: at rest, mid-slide; the rules | `D6Carousel` | — |
| Options A–D (record of the decision) | `D6OptA…D` | — |

## 2. Values

Dark is the docs/35 ladder; light is new (§2.8). Text sizes are written as rendered (FF roles × 1.1); spacing,
radii and strokes are unscaled.

### 2.1 Geometry
- The pane is a second sheet: `L1` #1B1B1F, r 8, the one edge, **6 px** right of the centre sheet, 6 from the
  window's top, right and bottom. Width **280–720, default 400** (`RIGHT_MIN_WIDTH` / `RIGHT_MAX_WIDTH` /
  `RIGHT_WIDTH`, unchanged), persisted per mode with the screen it shows (`layout.json` `rightKind` keeps
  `changes` / `files` / `browser`; the legacy aliases stay).
- With the pane open the transcript measure is `min(880, centre width − 80)`, centred (as shipped: 640 at a 440
  pane with the sidebar open).

### 2.2 Choosing the screen — no tab bar (owner, 2026-10-17)
- **Transcript title row** (the centre sheet's 32 px row): after the title, right-aligned, gap 2: **Changes ·
  Files · Page** toggles, then **Terminal** (as today), a 1 × 16 hairline at white 7 % with 4 px either side, then
  the session menu ⋯. The old "Show right pane" icon is gone (the three toggles replace it). Toggles: 28 × 28,
  r 6, glyph 14 (stroke 1.5), muted; hover 5 % + FG; **lit = 9 % + FG** = the screen the pane shows. Exactly one is
  lit while the pane is open; none when closed. Labels: "Changes", "Files", "Page" (role button, `aria-pressed`).
- **Pane row** (32, opaque L1, the 24 px fade under it): padding 0 6 / 0 12, gap 8: the screen's glyph 14 muted,
  its name 14.3 / 600, spacer, the screen's own actions (28 icon buttons, glyph 14), then **✕** (28, glyph 12,
  label "Close the pane"). Actions: Changes none; Files **filter** (search glyph, lit while the filter is open);
  Page none (its nav row is below).
- **Pushed view row** (same 32): ← (28, glyph `back2` 14, label "Back to …"), title 14.3 / 600 (ellipsis), context
  13.2 muted (ellipsis; hidden under 360 wide), ≤ 2 actions, ✕.
- Glyphs: Changes = the branch-merge glyph, Files = folder, Page = globe (`I['changes']`, `I['files']`, `I['page']`
  in `gen_d6.py`; the build uses the closest `aui-icons` names — Git, Folder, Globe — as today's tabs do).

### 2.3 The strip (owner, 2026-10-17)
- Changes · Files · Page are three screens side by side on one horizontal strip, in toggle order, 6 px apart (the
  frame colour shows between them while moving). The pane is a window onto one screen; its width never changes
  during a slide.
- Each screen keeps its own scroll position, open diffs, selection and pushed view.

### 2.4 Files
- Tree padding 4 / 6 / 12. **Rows 28**, r 6, padding-right 8, padding-left 2 + 16 × depth, gap 6: chevron 12 muted in
  a 16 slot (folders only; right / down), glyph 14 muted (folder, file, `{ }` for JSON, lock for lock files), name
  14.3 FG (ellipsis); git letter at the right in mono 12.1, 14 wide: **M muted, A success, D danger, U faint**.
  Ignored paths (gitignored / dot-folders the app already marks) in faint.
- Hover 5 %; **selected 9 %**; keyboard focus adds `inset 0 0 0 1px` grey at 45 %.
- **Filter:** the row's search action opens a 32 px Well field under the pane row (inset 6 / 8 / 2), edge 22 %
  while focused, search glyph 14 muted, the query, `3 of 1,204` faint mono 12.1 at the right. Matches stay inside
  their (expanded) folders; everything else leaves. Esc clears and closes.
- **Capped folder** (> 300 entries): one faint 13.2 line after its rows: "Showing the first 300 of N in this folder.
  Filter to find the rest." The footer count is gone; the refresh button is gone (the tree follows the disk —
  re-read on pane focus and when a turn ends, as the build sees fit; the owner sees no button).
- Empty: "No project" + "Open or add a project to see its files." Loading: skeleton rows (14 px glyph block +
  bar, both 9 %).

### 2.5 File preview (pushed view on Files)
- Row: ← "Back to files", file name, folder muted, action **open in default app** (external glyph), ✕.
- **Code:** on the sheet (no card), padding-top 12, rows grid 48 / 1fr, gap 12, padding-right 12; gutter faint mono
  12.1 right-aligned, code mono 12.65 / 20, no wrap (horizontal scroll); highlighter as the transcript's.
  **Reveal band:** the revealed lines at white 7 % full width, their numbers in FG; scrolled so the band sits 3
  lines from the top.
- **Markdown:** reads like an answer: padding 16 / 20 / 24, body 16 / 26, h1 24 / 32, h2 20 / 28 (28 above, 10
  below), inline code 14 mono on 9 %, code blocks Raised r 8. (Replaces the white serif page.)
- **Image file:** the image view (§2.10).
- **Not shown** (binary or > 1 MB): centred: 40 px Raised tile r 10 with the glyph 18, name 600, "<kind> · <size>.
  Baaz shows text, Markdown and images here." muted 13.2, one secondary **Open in default app** (28, Raised-lit +
  edge). Reveal in Finder is dropped.
- **Gone:** "This file is gone" + the path in mono and "was moved or deleted since you opened it."
- **Loading:** the row is real; the body is gutter + line bars at 9 %.

### 2.6 Changes (git lives here — owner, 2026-10-17)
- Padding 4 / 6, bottom inset clears the commit box. **Section captions** 32 (first 28): 13.2 muted, the count in
  faint mono 12.1: **This session** (files this session edited, in edit order), **Other changes**.
- **File rows 32**, r 6, gap 8: chevron 12 muted (16 slot, 4 in), status letter mono 12.1 (M muted, A success,
  D danger), file name 14.3 FG, folder 13.2 muted (ellipsis; hidden under 340 wide), `+N −M` mono 12.1 success /
  danger at the right; on hover one **open file** icon button (28) after the counts. Hover 5 %. The whole row
  toggles its diff (label "Show / Hide the diff for <path>", `aria-expanded`). Both sections expand the same way.
- **Diff** (full-bleed: −6 margins, 2 above / 6 below): mono 12.1 / 20, no wrap. Hunk header 24 on Raised-lit,
  muted, padding 0 12. Rows grid 36 / 14 / 1fr, padding-right 12: one gutter (new line numbers; old for deletions)
  faint right-aligned, the sign (+ success, − danger, space faint), the text. Add rows success 10 %, delete rows
  danger 10 %. Unchanged runs fold into a 24 px "N unchanged lines" row (chevron 11, 13.2 muted, 22 in) that
  expands in place. Unified at every width.
- **Sticky:** an open file's row sticks under the pane row while its diff scrolls.
- **The commit box** (pinned at the foot, padding 8, the 24 px fade above it): Raised r 10 + edge, padding 8, gap 6.
  Its single primary follows the branch:
  1. **Changes exist:** the message (drafted from the session; first line 600, rest muted; editable, min 2 lines,
     14.3 / 22, padding 2 / 4); a 28 row: branch glyph 12 + branch in mono 12.1 muted + `↑2` faint; spacer;
     **Commit N files** primary (28, r 6, FG fill, frame text, 600). Commits every listed file — **no staging**
     (default drawn; the owner locked the design with it). Empty message → placeholder "Describe the change",
     button disabled (45 %). Focused: edge 22 %.
  2. **Committing:** the button disabled with a 12 px spinner and "Committing…".
  3. **Hook failed:** above the box, a note r 8 Raised with the danger ring at 70 %: ✕ danger 14 + "pre-commit hook
     failed" 600; the first lines in mono 12.1 / 18 muted; two text controls (24, 13.2): **Show full output** (opens
     the output view, §2.9) and **Ask Muse to fix it** (puts the error in the composer as a quote block). The
     message is kept.
  4. **Clean, commits ahead:** one row: branch · "2 commits not pushed" faint; **Push** primary.
  5. **Up to date, no PR:** branch · "up to date"; **Create PR** primary (PR glyph) → the PR form.
  6. **PR open:** a card (Raised r 10, padding 10 / 12): PR glyph in success, title 600 (ellipsis), `#128` faint
     mono; second line 13.2 muted: 8 px status dot (success open) · "Open" · checks ("3 of 4 checks", spinner while
     running); **View** text control → opens the PR **in Browse** (default drawn; locked with the design).
  - When the tree is clean, the body shows the empty state ("No changes" / "The working tree is clean.") above the
    box. Behind the remote: "↓1" faint after the branch; no Pull (out of scope).
- **Create PR** (pushed view on Changes, "Back to changes"): padding 12, fields 12 apart, labels 13.2 muted:
  **Into** — base picker (32, Raised-lit + edge, mono 12.65, chevron) ← head branch in mono; **Title** (Well
  input 32+, edge, focus 22 %); **Description** (Well, min 150, drafted from the session, "Drafted from this
  session · edit freely" muted at its end); **Open as a draft** checkbox 16. Footer (8 / 12): Cancel text, **Create
  PR** primary.
- States: Loading (caption + skeleton rows), **Not a git repository** ("Changes needs git. Files still shows the
  project."), **No project**.

### 2.7 Page (the session's one page — owner, 2026-10-17)
- Under the pane row a **36 px nav row** (padding 0 6, gap 2): back, forward (disabled = faint 60 %), reload (28
  icon buttons); the **address** on the Well (28, r 6, edge, padding 0 8, gap 6, lock glyph 12 muted, host 13.2 FG,
  path muted, ellipsis); then **annotate** (pin glyph; lit while on), **screenshot to the composer** (camera),
  **open in Browse** (external). No tab strip, no console button. Under 300 wide back / forward leave.
- The page fills below (white ground) — the native page window.
- **Agent pill:** while the agent acts, one pill centred 16 above the bottom: Floating dark (#2C2C32, the edge +
  float drop, full radius, 32 tall, padding 0 14 / 0 10, gap 8): the provider mark 16, "Muse is clicking
  “Choose”" 13.2, a 12 px spinner.
- **Annotating:** hover outlines an element (1.5 px Muse blue `#3B6DF2`, r 4, 6 % fill); a click pins a numbered
  note (20 px dark circle, 11 / 600 white, 2 px white ring); notes collect in a Floating panel 8 in from the
  bottom (r 10, padding 6): rows 13.2 (number cap 18 at 9 %, selector mono 12.1 muted, the note), footer over a
  rule: "Click an element to pin a note" faint, **Clear** text, **Send N to Muse** primary — the batch becomes the
  D4 annotation turn.
- **Loading:** a 2 px muted bar under the address grows; the old page stays until the new one paints.
- **No page yet:** tile + "No page yet" + "When Muse opens a page for this session, it shows here. Or type an
  address." + a 32 px address field "Search or enter an address".
- **Couldn't load:** Baaz's own page on the sheet: "This page couldn’t load", the reason with the host in mono,
  secondary **Try again**.
- Everything drawn over the page (pill, pins, outline, notes panel) is an overlay window (memory
  `feedback-native-page-above-gpui`).
- **Recent pages** (owner, 2026-10-18; canvas v51 `D6PageRecent`): once the session's page has shown an address,
  the pane row's name is a menu button — "Page" + chevron 12 muted, 28 tall, r 6, hover 5 %, lit 9 % while open,
  label "Recent pages". It opens the app's one dropdown (FF; panel inset 6) 2 px under the row's top edge, 6 in:
  a "Recent pages" label, then one 36 px row per main-frame address of this session's page, newest first, each
  once, at most 8 — the title (the address when it had none), the host muted (hidden under 300 wide), the check on
  the page on screen. Width = pane − 12, at most 320. A row navigates the session's one page; Esc, a click
  outside, closing the pane, another screen or another session closes it. Over a native page it is drawn in the
  page's overlay window, which then covers the nav row and the page so an outside click closes it.

### 2.8 Light (new; locked with the design)
Frame #ECECEF · sheet #FAFAFB · Raised #FFFFFF · Raised-lit #F3F3F5 · Floating #FFFFFF · Well #F2F2F4 · FG #18181B
· muted #63636D · faint #9A9AA4 · hover black 4 % · selected black 7 % · lines black 7 % · edge `inset 0 0 0 1px
rgba(0,0,0,.08), 0 1px 2px rgba(0,0,0,.04)` · float `inset 0 0 0 1px rgba(0,0,0,.08), 0 8px 24px -10px
rgba(0,0,0,.18)` · focus ring black 28 % · success #15803D · warning #A16207 · danger #B91C1C · add tint 9 %,
delete tint 8 % · reveal band black 5 % · primary = FG fill with sheet-coloured text · syntax keyword #7C3AED,
type #1D4ED8, string #15803D. Same sizes as dark. The page's agent pill and the annotation panel stay dark in
both themes.

### 2.9 Full output (pushed view, new)
- Opened from a tool card's "N more lines" row or its header's full-output affordance (D4). Row: ←, the command in
  mono 12.65 (400, ellipsis), then exit in mono 12.1 (**danger when non-zero**, faint when 0), the time faint,
  **copy** (28), ✕.
- Body on the Well below the row: mono 12.65 / 20, padding 12 / 16 / 40, no wrap, ANSI colours mapped to the status
  tokens (green → success, red → danger, yellow → warning, dim → faint).
- Capped: a 28 px footer line on the Well over a rule, 12.1 faint: "N lines · capped at 10 000".
- Running: the header shows a 12 px spinner + "running · 12 s"; the tail follows new lines until the person
  scrolls up. Fetching: centred spinner + "Fetching the full output…"; copy disabled.

### 2.10 Image (pushed view, new)
- Opened from a sent attachment tile, a Markdown image or an image file. Row: ←, the name, open in default app, ✕.
- Body: padding 16, the image fitted (max width 100 %, max height minus 40), r 6, the edge; under it 10 px, mono 12.1
  faint: `1440 × 900 · PNG · 312 KB`. Click toggles fit ↔ 100 % (then drag to pan, cursor zoom-in / zoom-out).
- Missing: tile with the image glyph + "Image not available" + "<name> could not be read."

### 2.11 Shared
- Empty / state blocks: centred, padding 0 32, gap 6: 40 px Raised tile r 10 + edge with the glyph 18 muted (6
  below), title 600, line 13.2 / 18 muted (max 260), then at most one secondary action 8 below.
- Skeleton bars: 10 tall, r 5, at selected (9 % / 7 %). No spinners for list loading.
- Spinner 12: 1.5 px ring at 35 % muted with a muted top.

### 2.11 Settled by the build (owner: "decide, plan, settle them and implement", 2026-10-18)
1. **Commit box on a clean, pushed default branch:** one row, the branch and "· up to date" — no primary (a PR
   from the default branch into itself means nothing). When the remote is ahead: "· ↓N on the remote" (no Pull).
2. **A folder's git letter:** the one kind of change under it; a mix reads M; all untracked reads U.
3. **The hook-failure action** names the session's provider: "Ask Claude Code / Codex / Muse to fix it".
4. **A code quote in light** inks `#3F3F46` (between FG and muted, as `#C9C9D0` is on dark).
5. **The quote bar** anchors from the selection's own start line (press or release, whichever comes first in
   reading order); a word or line picked by double / triple click anchors at that click.
6. **The image view** shows the pointer cursor (gpui has no zoom cursors).
7. **"This page couldn't load"** sentences: refused the connection · could not be found · is out of reach: this
   Mac is offline · took too long to answer · closed the connection · has a certificate Baaz does not trust ·
   did not load (code).
8. **Esc** goes back from a pushed full output or image wherever the keyboard is (the window's Esc chain),
   after the Recent pages menu and the file preview.

### 2.12 One bar per screen (owner, 2026-10-21; canvas v53 `D6OneBar`)
Supersedes the Page's pane row + nav row (§2.2, §2.7) and the Files filter row (§2.4):
- **Page, with a page showing:** no pane row. The nav row is the screen's one bar at the pane row's metrics (32, opaque
  L1, padding 0 6, gap 2): back, forward, reload; the address on the Well (28, r 6, edge) with a 22 px **Recent pages**
  chevron (r 4, lit 9 % while open) at its right end; annotate, screenshot, open in Browse; a 1 × 16 hairline; ✕. The
  menu hangs 2 px under the bar from the address field's left edge and ends 6 in from the pane's right edge. No fade.
  "No page yet" keeps the pane row (glyph, "Page", ✕).
- **Files, filtering:** the bar itself is the field (Well 28, search 12, query, count faint mono), a hairline, ✕ —
  which closes the filter (as Esc); the next ✕ closes the pane. At rest the row is unchanged.
- Changes and the pushed views were one row already and are unchanged.
- Every right-pane scroll container is locked to its axis per gesture (a sideways swipe never scrolls a vertical list).
- HTML, SVG and PDF files (from Files or a transcript link) open in the Page; Files rows have a context menu (Open in
  default app, Reveal in Finder; View source for HTML, SVG, Markdown; folders Reveal only).

## 3. Motion (Emil's rules)
- **Strip slide** (a toggle while open, or **Shift** + a two-finger sideways swipe in the pane — one screen per swipe, snapping; owner 2026-10-21: a plain sideways swipe only scrolls the content under it and never switches screens):
  the screens translate together, **240 ms ease-out**; nothing fades, the pane's width and row do not move apart
  from the content. With reduced motion: an instant swap.
- The **Page** screen during a slide is a still picture of the page (the Browse cover technique); the live page
  window shows when the slide lands.
- **Open / close** the pane: as today (width tween, ease-out). Reopens on the screen it last showed.
- **Pushed view:** in = content slides 8 px from the right + fades in, 160 ms ease-out; back = 120 ms, reverse.
- Diff open / close and the unchanged-lines row: height on `spring.moderate` (0.16 s, bounce 0), chevron rotates
  90°; content fades in 80 ms after the height starts (as D4 folds).
- Row hover 80 ms colour; FF fluid hover glide on tree and change rows as on menu rows.
- Commit box state changes: cross-fade 120 ms; the box's height animates on `spring.moderate`.
- Must not move: the pane row, the transcript title row's toggles, the commit box's position, the nav row.

## 4. Behaviour
- **Toggles:** pane closed + click Files → the pane opens on Files. Open on Changes + click Files → slide to Files.
  Click the lit toggle → close. ✕ → close. ⌘⌥B / ⌘\ → toggle open / closed on the last screen. The terminal toggle
  is independent (bottom dock, as today): the right pane and the terminal can both be on.
- **Opened from the transcript:** the fold row's `+N −M` chip → Changes, scrolls to and opens that turn's files
  (`open_turn_diff`), rows flash 9 % once; a file link / sent file chip → Files, reveals + previews it (band on
  `#L…`); a page action → Page; "N more lines" / tool header → the full-output view pushed on the current screen;
  an image tile / Markdown image → the image view pushed on the current screen. With the pane closed, it opens on
  the target screen (or the last screen, for output / image).
- **Keyboard:** in Files, ↑ ↓ move, → opens a folder / enters, ← closes / goes to parent, ⏎ previews, ⌘F or / opens
  the filter, Esc closes the filter or pops a pushed view. In Changes, ↑ ↓ move between rows, ⏎ / Space toggle the
  diff. Pushed views: Esc and ⌘[ go back.
- **Commit:** ⌘⏎ in the message commits. Push and Create PR run git / `gh` through the existing actions
  (`GitAction::{Commit, Push}`, `PrAction::Create`) — today they reach `inert`; wiring them is part of D6. Base
  branch default = the repo's default branch. Draft = `gh pr create --draft`.
- **Ask Muse to fix it** puts the hook's error output into the composer as a quote block (§0) with no draft text.
- **Accessibility** (CLAUDE.md): every icon button carries a label (listed above); the pane is `complementary`
  "Right pane"; the tree is `tree` / `treeitem`; change rows are buttons with `aria-expanded`; toggles are buttons
  with `aria-pressed`.
- **Both modes:** the right pane belongs to Sessions mode. Browse mode is unchanged (out of scope).

## 5. What exists, what is new
Existing — agentic-ui `crates/aui/src/workbench/*`, Baaz `crates/baaz/src/right.rs` (≈4.2 k lines), `app.rs`
(the pane header ≈6430–6490 builds a plain `tab_strip`; the library `RightHeader` is unused), `layout.rs`
(`RightKind`), `aui-webview/src/view.rs` (nav row):
- `file_tree` / `FileTree` (`files.rs`): header 34 → replaced by the pane row; rows 24 → **28** with the values
  above; footer → removed (capped line instead); `FileTreeAction::Refresh` → unused (no button).
- `file_preview_pane` (`right.rs`): Back / Open grey buttons → the pushed-view row; `code_block` fill → the code
  view; `doc_pane` for Markdown → the reading view; `file_card` for binary → the state block.
- `changes_pane_from` (`right.rs`): `changes_heading`, `session_file_row`, `other_file_row` (grey `button`s) →
  caption + change rows; `code_block` diff → the inline diff; the "Commit or open a PR…" row + `pr_form` → the
  commit box states + the Create PR pushed view. `git_changes` / `diff_review` (library) are not used by the pane.
- Browser: `browser_tab_strip` / `browser_new_tab` / `browser_strip_tab` → **removed from the pane** (the session
  has one page); nav row → the 36 px row (drop the Console button; Screenshot and Annotate stay);
  `agent_action_pill`, `element_outline`, `annotation_pin`, `annotations_panel` (272 wide) → the pane-width
  notes panel.
- Empty / loading states (`empty_state`, `loading_state`) → the shared state block.
New: transcript-row toggles (replacing the tab strip and the "Show right pane" icon), the strip and its slide, the
pushed-view row, the full-output view (the fetch exists: `full_output.rs`, `show_full_output`), the image view, the
commit box state machine and real git / PR actions, the PR card, the filter field, sticky change rows, the light
tokens, the quote flow (§0).

## 6. Out of scope; the owner's decisions
Out of scope: Browse mode; Settings (D5); a Pull action; staging; split diffs; a terminal inside the pane; light
for the rest of the app beyond these views (the tokens apply app-wide when D8 comes); editing files in the preview.
Decisions (2026-10-17, chat and canvas):
1. Reference product: **Claude Desktop**.
2. Git actions (stage-free commit, push, PR) **in Changes only**; no Git tab.
3. The pane's browser is **the session's one page**; no tab strip; Browse stays the person's own browsing.
4. **No tab bar**: icon toggles in the transcript's title row choose the screen (Option A).
5. **A way to close the pane**: ✕ in the pane row (plus the lit toggle and ⌘⌥B).
6. **The strip**: panes are screens on a canvas strip; toggling slides to them (D6Carousel).
7. **Locked** ("Go with A + strip, redraw all the boards and necessary components. We can lock this design") —
   including the drawn defaults: commit takes every listed file (no staging), the PR card's View opens in Browse,
   the light values.
8. **D4 canvas refresh and the quote flow approved** ("yes, both approved - go ahead").

## 7. Live-check list for the build session (BaazCheck, Sessions mode)
1. Transcript row: three toggles + Terminal + hairline + ⋯; the old pane icon is gone; VoiceOver names each.
2. Click Files with the pane closed → opens on Files, Files lit. Click Changes → the strip slides (240 ms, no fade,
   pane width fixed); click Changes again → closes. ✕ closes; ⌘⌥B reopens on Changes.
3. Shift + a two-finger sideways swipe in the pane → one screen per swipe, snapping; a plain sideways swipe over a wide code line scrolls it and never switches screens. Reduced motion → instant swap.
4. Each screen keeps its scroll: scroll Files, slide to Changes and back → same place.
5. Files: rows 28, hover 5 %, selected 9 %, keyboard ring; ↑↓←→⏎; filter (⌘F) narrows and Esc restores; a capped
   folder ends with the faint line; no refresh button, no footer.
6. Preview: `files-reveal` a `.rs` with `#L` → band + scroll; CLAUDE.md reads at 16 / 26 on the sheet; a PNG opens
   the image view; a binary shows the state block; ← and Esc go back; ✕ closes.
7. Changes (a repo with edits from the session): sections, rows 32, open a diff → hunk headers, tints, "N unchanged
   lines" expands; the open row sticks while scrolling; the fold-row chip in the transcript opens that turn's files.
8. Commit: drafted message, ⌘⏎ commits for real in a scratch repo; a failing pre-commit hook shows the danger note,
   "Show full output" opens the output view, "Ask Muse to fix it" lands a quote block in the composer.
9. After commit: Push appears; after push: Create PR → the form → Create PR (draft) in a scratch GitHub repo; the
   PR card shows checks; View opens in Browse.
10. Page: the session's page with no tab strip; the nav row; the agent pill while a `browse:` step acts; annotate →
    outline, pins, the notes panel, Send → the annotation turn; a dead localhost → Baaz's error page; the slide to
    / from Page shows the still picture, then the live page.
11. Output: a long `shell:` output → "N more lines" → the output view on the Well, exit in danger for non-zero, copy;
    a running command tails.
12. Image: a sent image tile → the image view; click toggles fit / 100 %.
13. Widths: drag to 280 → nav back / forward hide, change rows drop folders; 720 → paths and diffs widen.
14. Light theme: every view above in light against `D6Light`.
15. Quote (§0): select across a paragraph and a list → the bar 6 above; Quote → the block in the composer; a code
    selection → a code quote; send → the bubble's quote; click it → scroll + flash; near the top → the bar below.
16. The 18-entry probe exits 0 after baselines are regenerated **on main** in their own commit (the right-* entries
    change by design). `right-tabs` is retired (owner, 2026-10-18).
17. Recent pages: visit three pages, open the title menu → newest first, the current one checked; pick an older
    one → the page goes there, the menu closes; Esc and a click on the page close it (§2.7).

## 8. Next session (build, Opus 5.5) — the prompt
```
You are the builder for Baaz. Read, in order: harness docs/40-handoff-after-sidebar.md, docs/43-d6-spec.md (the
approved spec — it is the source of truth), docs/35, memory feedback-verify-visually-first, and the locked boards on
the canvas "Baaz v3" (https://claude.ai/artifact/7SwC39oc2nEakd4TpCdp8e, v49: D6 rows from y 19400 down; the quote
flow at y 17400).

Implement D6 and the quote flow (§0) exactly as specified, steps 4–7 of docs/40 §4: agentic-ui components + gallery cards → Baaz adopts → gates (both repos, the
18-entry probe) → build BaazCheck and drive every case in docs/43 §7 yourself with computer use (I approve the
access prompt) → me. Any design question the spec does not answer comes back to me; do not invent. Commit per step.
Never push without asking.
```
