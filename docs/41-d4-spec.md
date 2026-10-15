# 41 — D4 spec: the transcript, the composer's states, and homes for the three controls

Written 2026-10-14 (00:30) by the design session, after the owner approved the canvas ("Looks good. I approve.",
2026-10-13 18:25). This is the handoff to the build session (docs/40 §6). It is the source of truth; the canvas is
the picture of it. **Any design question this file does not answer goes back to the owner — never invented.**

Canvas **"Baaz v3"** https://claude.ai/artifact/7SwC39oc2nEakd4TpCdp8e, approved at **v42** (the owner's own edits
to note positions are in it; keep them). Generator: `docs/34-refs/v3-redo/canvas/gen_d4.py` (boards + previews),
`merge_d4_index.py` (index merge that keeps the owner's placements), `render.py` in the session scratchpad was only a
preview tool. Captures the mocks were drawn from: `fixtures/msp/*.jsonl` (docs/40 §3's fourteen, plus
`synthetic-reading.jsonl`, new, real project prose), rendered with `target/debug/baaz --theme dark --replay <f>
--screenshot <png>` under `BAAZ_DETERMINISTIC=1`, a fresh `BAAZ_STATE_DIR`, `perl -e 'alarm 150; exec @ARGV'` (macOS has
no `timeout`); folds opened with `--steps 'wait:800;transcript:fold;wait:300;expand-groups;wait:500'`; composer
states with `draft:` / `image:` / `file:` / `drop` / `mention:`; pickers with `model` / `effort` / `provider`
(`mode` collides with the window-mode verb and fails — the approval-mode rows come from the library's sample).

## 1. Boards, by state

Rows on the canvas (y): sheets at 4040 (Blocks, Cards + homes), screens at 6500 / 7520 / 8660, round-3 sheets at
9800 (Reading, Composer states, More cards), round-4 screens at 12280 (Approval, Error, Several questions), round-5
sheet at 13300 (Pickers + send). Blue notes n7–n12 beside each row carry the rules in prose.

| State | Board | Fixture / source |
|---|---|---|
| Blocks, default + hover + open | `D4Blocks` | synthetic-markdown, transcript-real, toolgroup, toolshapes, diffstat, readoutput, stress-hetero (long code), composer attach captures |
| Cards that need you, resolved rows, live rows, homes | `D4Cards` | transcript-approve, question-error, userinput-clarify, toolshapes, todo-goal, error-retry, library samples |
| Answer (markdown) + composer seam | `D4Answer` | synthetic-markdown |
| Work (fold open, groups, failed card) | `D4Work` | synthetic-toolgroup |
| Question pending (needs you) | `D4Ask` | synthetic-question-error |
| Goal + tasks | `D4Progress` | synthetic-todo-goal |
| Homes, Sessions (toast + feedback above the composer) | `D4HomeSessions` | synthetic-diffstat |
| Homes, Browse (downloads button; toast + feedback over the page) | `D4HomeBrowse` | BrowseOpen capture |
| Reading (long rich text, image, quote, ordered list, rule) | `D4Reading` | synthetic-reading (captured first) |
| Composer: empty, typed, attachments, states, quote chip, drop, @, /, queued, running | `D4Composer` | attach/drop/draft captures; menus from the library sample |
| Plan, summary, search, web, browser, delegated, MCP, $ command, skill row, markers, needs-you banner, streaming, unknown kind, empty session | `D4More` | library sample data (aui-protocol `sample.rs`, gallery cards) |
| Approval pending screen | `D4Approve` | transcript-approve's command on the library's approval card |
| Error + retry screen | `D4Error` | synthetic-error-retry |
| Several questions (wizard) | `D4Grill` | fixtures' questions + gallery options |
| Pickers (provider, model, nested, effort list + slider, mode), send / stop | `D4Pickers` | effort/provider captured; models + modes from the gallery sample |

Not drawn, inherit the rule: light theme (not designed, docs/35 §4); the right pane's full-output view (D6); the
`+` menu (the one dropdown, rows as today).

## 2. Values

All colours are the docs/35 ladder. Text sizes are **FF roles × 1.1** (the app's `TEXT_SCALE`); write the FF role
in code and let the scale apply. Spacing, radii and strokes are unscaled.

### 2.1 Shared
- Measure **880**, centred in the sheet (x 408 at 1440). First turn at **80** from the sheet's top. Composer 880 wide,
  **12** above the sheet's bottom (18 from the window).
- Surfaces inside the sheet: **Raised #232328** (cards, blocks, user turn), **Raised-lit #2A2A30** (code header, table
  header row, nested rows, secondary buttons, tags, needs-you banner), **Well #151518** (shell output, commands, the
  Other input). Every surface carries the one edge `inset 0 1px 0 rgba(255,255,255,.07), inset 0 0 0 1px
  rgba(255,255,255,.06)`. No drop shadows inside the sheet. Floating #2C2C32 + the float drop only for menus,
  pickers, popovers, the jump pill, the toast and the feedback slot.
- Radii: **cards and blocks 8** (the sheet's own), **user turn and composer 12**, controls 6, option rows 6, tags 6,
  caps and pills full.
- Text: body 13/20 (→ 14.3/22), caption 12/16 (→ 13.2/18), subtitle 14/20 (→ 15.4/22, the composer field), title
  16/22 (→ 17.6/24, markdown h2), mono 11.5 (→ 12.65, line 20), footer mono 11 (→ 12.1). Colours FG #F2F2F4, muted
  #9D9DA6, faint #6E6E78 (mono counts and hints only). Weights 400 / 600.
- Status colours (dark tokens): success #86EFAC, warning #FCD34D, danger #FCA5A5; soft tints 12 % for backgrounds
  (`*-soft`).
- Lines inside a surface: white 7 %. Hover 5 %, selected 9 %, pressed 12 % (surface-relative).
- Gaps: blocks within a turn **8**; between turns **16**; a card's body rule 1 px 7 %.
- **No standing actions.** A hovered turn shows its buttons; cards carry only the choices the provider minted (or the
  card's own one action); tool cards have no buttons at all.

### 2.2 Turns
- **User turn** (`aui::transcript::user_turn`): Raised r 12, padding 9 / 13, body text, max width 78 % of the
  measure (686), right-aligned. Under it a 24 px rail whose height is always reserved: at rest empty; on hover of
  the turn: **edit · copy** (28 px icon buttons, glyph 12, muted → FG) then the age in faint footer mono, 6 px gap.
  Resend is removed.
- **Sent attachments** sit above the bubble, right-aligned, 6 apart, 4 above: images **64 px tiles** r 6 + edge
  (object-fit cover); a video tile = the same tile + play glyph 16 over a 25 % black scrim + its length as a 10 px
  mono cap (`rgba(18,18,21,.85)`, r 3) bottom-right; files = **30 px chips** (Raised, r 6, edge; file glyph 13,
  name, type · size in caption muted). Click a tile → open in the right pane. The wire's kinds are Image / File /
  Text; a video is a File with the video treatment by extension.
- **Quotation**: a selection in any answer offers **Quote** in its rail → a Text attachment in the composer (chip:
  edit/quote glyph + first words + "quote" meta) → sent as a quote block at the top of the bubble: 2 px left rule at
  white 18 %, caption muted, clamped to 3 lines, then "from the reply above" in faint 12.1 under it (−4 / 8 margins),
  then the words. Clicking the quote scrolls to its source. (New: no quote feature exists today.)
- **Annotations** (the Browse annotator batch) are one user turn: the page screenshot as a 120 px strip (r 6,
  object-position top) with numbered pins (18 px FG circles, 11 px 600 frame-coloured number, 1 px 40 % black ring);
  a caption line "N annotations · host/path" muted; one line per note: number cap (18, 9 %), selector in mono 12.1
  muted, the note in caption.
- **Assistant turn**: prose on the sheet at the full measure, no surface. At rest nothing under it. On hover:
  **one copy button** (28, muted) 8 below, then the footer in faint mono 12.1, 10 below: `model · time · tokens ·
  reasoning · age`, separated by ` · `. Retry and fork are removed (a retry is a typed ask; fork stays in the session
  menu).
- **Streaming**: a 2 px FG caret (16 tall, −3 baseline) after the last glyph; nothing else animates in prose.

### 2.3 Markdown inside an answer
- Paragraphs 10 apart (reading boards: 12). Body 14.3 / 22; long answers (D4Reading) use line-height **23** (1.6).
- h2 = title 17.6 / 24 600, 20 above, 8 below (0 above when first); h3 = 15.4 / 22 600, 16 above, 6 below.
- Lists indent 24 (22 in short answers), items 2 apart; ordered markers muted.
- Inline code: mono 12.65 on white 7 %, r 4, padding 1 / 5. Links: FG, underline 1 px with the underline colour
  faint, offset 3.
- Quote: 2 px left rule white 12 %, padding 2 / 0 / 2 / 14, text muted. Rule: 1 px 7 %, 16 above and below.
- **Image**: a Raised block r 8 + edge, the picture edge to edge (max height 480, cover), its caption inside in
  caption muted, 8 / 12. Missing picture: the same block with file glyph 16 + alt text + path in faint mono; never a
  broken image.
- **Code block**: Raised r 8 + edge. Header **32** on Raised-lit: file glyph 12, `code <lang>` (or the file name) in
  mono 12.65 muted; on hover one **copy** button (24, muted) at the right (wrap and edit removed). Body padding
  10 / 12, gutter **22** faint mono 12.1 right-aligned, 8 gap, code mono 12.65 / 20, syntax: keyword #B88DEA, type
  and call #7AA4EF (the app's highlighter as sampled). Past **12 lines** the block folds: a 24 px "N more lines" row
  (caption muted, chevron 11) on the same Raised ground under a 7 % rule, expands in place. The header never toggles.
- **Table**: Raised r 8 + edge, `border-collapse: separate`. Header row Raised-lit, 600, cells 10 / 14; body cells
  10 / 14 over 7 % rules; row hover 5 %.

### 2.4 Quiet rows (no surface)
Shared shell: **32 px**, 8 px side padding bleeding 8 px outside the measure (margin −8), r 6, gap 8, body text
muted, chevron 12 on the **left**; hover → 5 % overlay + FG text; the whole row toggles.
- **Fold row** `Worked for 2.1 s · edited 1` (+ the `+N −M` chip in mono success/danger, 4 after). Open: chevron down,
  the turn's cards follow on the measure, 8 apart, no indent; the row stays as the heading.
- **Thinking** `Thought for 4.2 s · <summary>` with the thinking glyph 14 muted after the chevron. Open: the trace in
  caption muted, padded 32 from the row's left (under the text), 34 px top fade while streaming.
- **Resolved rows**: `✓ Allowed once <cmd mono> · exit 0 · 0.0 s` (tail faint), `⟳ Approving… <cmd>` (spinner 12),
  `✓ Answered <pick as inline code>`, `✓ Clarified <text>`. Not clickable (no chevron).
- **Live activity row**: spinner 12, verb (shimmer: muted → FG → muted sweep, 1.6 s, text only), target mono, `+k
  earlier` faint, elapsed faint mono.
- **Skill loaded**: spark glyph, "Loaded skill", name in mono, `· source` faint.

### 2.5 Tool cards
- Frame: Raised r 8 + edge. **Header 34**: padding 0 / 8 / 0 / 12, gap 8: status glyph 14 (ok = success check 1.8
  stroke, failed = danger ✕, running = spinner), verb FG, target mono muted; right cluster gap 8: result (`exit 0`
  faint mono 12.1; `exit 2` in danger), duration faint mono, chevron 12 muted (+4 margin). No buttons. The header
  toggles the body.
- Body under a 1 px 7 % rule. **Shell output on the Well**, padding 10 / 12, mono 12.65 / 20, `white-space: pre`.
  Past 6 lines: a 24 px "N more lines" row on the Well (caption muted, chevron 11) expands in place; the whole output
  stays in the right pane's full-output view. "Run in terminal" / "open in terminal" removed.
- **Read**: header only (`N lines` faint); open → the file on the Well. **Edited**: `+N −M` chip + duration; body a
  unified diff on Raised: 6 / 0 padding, rows grid 36 / 20 / 1fr with 8 gap, padding 0 / 12 / 0 / 8, gutters faint,
  add rows success 10 %, delete rows danger 10 %.
- **Search**: one line per hit, `path:line` mono 12.65 muted + snippet mono. **Web**: 14 px favicon tile r 3 + title
  + domain muted, `+n more` faint; the title is the link. **Browser**: a 70 px screenshot strip (r 6, margins 8 / 10)
  with the click ring (18, 2 px FG, 1 px 40 % black outer) and the action in a dark cap (`rgba(18,18,21,.85)`,
  12.1). **Delegated**: the sub-agent's lines in two columns (who, 64 px muted · text) in caption, 10 / 12, no
  nested bubbles. **MCP**: verb `server · tool`, params as a two-column list (92 px key muted · mono value).
- **Your own `$` command** (userShell): the same card, verb `$`, failure as any card.
- **Tool group**: one surface. Closed: header (`Ran 3 commands`, chevron right) + a caption preview of the first
  two calls (glyph, verb, target mono) + `+k more` faint, padding 6 / 10 / 8. Open: every call is a **row** in the
  same card: the tool header's layout over a 7 % top rule; a row's output opens on the Well right under it. Nothing
  nests, no card inside a card.

### 2.6 Cards that need you
Raised r 8 with the **ring in the status colour at 70 %** replacing the white ring (keep the top highlight):
warning for approval and question, danger for failure. Nothing else is ever marked.
- **Approval, pending**: header 10 / 12 gap 10: tile 26 r 7 Raised-lit + edge with the terminal glyph 14; title
  "Allow this command?" 600; sub caption muted ("Muse wants to run a shell command in <project>"). The command on the
  Well r 6, 8 / 10, margins 0 / 12 / 10, mono. Footer 8 / 12 over a rule: key hints faint mono 12.1 (`⏎ allow · ⎋
  deny`), spacer, **Deny** and **Always allow** as text controls (28, r 6, muted → FG on hover), **Allow** primary
  (FG fill, frame text, 600). Resolved → the quiet row (2.4). Running → spinner row.
- **Question, pending** (one question): header: tile with `?`, the question's header in caption muted over the
  question 600, the auto-resolve tag right (caption, warning on warning-soft, r 6, 2 / 8). Options as rows 8 / 10 in
  an 8 px inset, r 6, gap 10: radio 16 (1.5 muted ring; selected FG ring + 8 px dot) or checkbox 16 r 4 (selected
  FG fill, frame tick); label 400 (600 when selected); description caption muted; the key in a faint mono cap
  (`.key`: 12.1 on 7 %, r 4, 1 / 6); "Preview" as a caption disclosure (chevron 11, padding 4 / 10 / 4 / 18).
  Other (when allowed): a 32 px Well input r 6, margins 0 / 8 / 8, placeholder muted with the edit glyph. Footer:
  key hints (`↑↓ move · ⏎ continue`), spacer, only the provider-minted choices (Skip text · Explain instead secondary
  = Raised-lit + edge · Continue primary). No "0 selected" count on a single choice.
- **Several questions = a wizard** (owner, 2026-10-13 18:19): one card, one question at a time. Header: topic 600,
  why in caption, `n of m` faint mono + the auto-resolve tag right; a **3 px progress bar** under the header (7 %
  track, muted fill); then the current question's header + question + options as above; footer: **Back** text
  control (back glyph) + key hints left, spacer, Skip, Explain instead, **Next** (reads **Continue** on the last).
  A single choice advances on its own; pick-all waits for Next. Once the card closes, one quiet Answered row per
  question.
- **Failure**: header 10 / 12 gap 10: danger ✕ 16, title 600, lines in caption muted (the code line in mono), one
  secondary **Retry now** at the right. Below it the retry status row (2.8) when the app retries by itself.

### 2.7 Progress cards
- **Tasks**: Raised r 8. Header 32 (0 / 8 / 0 / 12): list glyph 14 muted, "Tasks" 600, `n / m` faint mono 12.1,
  chevron right. **3 px bar**: 7 % track, muted fill. Rows 30, padding 0 / 12, gap 10, over 7 % rules; marks 15:
  pending = 1.5 muted ring; running = 1.5 FG ring + 6 px FG dot; done = success fill (+ frame tick); done text muted
  + strike 1 px.
- **Goal**: Raised r 8. Header 10 / 12: title 600 + the provider's status as a tag (caption on 7 %, r 6). Bar row:
  the 3 px bar (fill clamped at 100 %) + the provider's number in faint mono (shown as given, e.g. 120 %). Two-column
  list (92 px key muted · value) in caption, padding 0 / 12 / 12.
- **Plan**: Raised r 8. "Proposed plan" 600 + "Plan mode" tag; ordered list at body size, 0 / 12 / 10 / 30; footer:
  hints (`⏎ accept · e edit · ⎋ reject`), spacer, Reject · Edit text, **Accept plan** primary.
- **Summary**: Raised r 8. Header: ok glyph, headline 600, `time · cost` faint mono right. Files 2 apart, 24 px
  rows: status letter (mono 11, 14 wide, success for A else muted), path mono, `+N −M` right. Checks as one muted
  caption line with success ticks, 12 apart. Footer: spacer, **Review diff · Commit… · Create PR** — text controls,
  Create PR secondary; none primary (peers).
- **Unknown kind** (`generic_item_card`) and reminder child: a card with the kind as verb, status mono, the
  provider's fallback text as body (8 / 12), folded by default.

### 2.8 Rows around the composer
- **Status row**: 28 px, caption muted, 8 above the composer, left on the measure: spinner 12 (or braille), the
  label, ` · ` + key cap (`esc` in `.key`) + "to interrupt", trailing note faint (`memory reminders`, `12 s`, `·
  rate limited by the provider`). Retry: `Attempt 2/5 <retrying in 8 s mono> · reason`.
- **Needs-you banner** (when the card needing you is off screen): Raised-lit r 8 + edge, padding 8 / 10 / 8 / 12,
  gap 10: warning glyph 14, headline 600 + detail muted in one line, one **Jump ↑** text control (24). Leaves when
  the card is on screen.
- **Jump to latest pill**: Floating, full radius, 28 px, padding 0 / 12 / 0 / 10, edge + float drop: ↓ 12 + "Jump
  to latest" caption FG + count in a 9 % cap (10.5 mono, 2 / 6). Centred over the measure, 8 above the composer.
- **Queued message**: Raised-lit r 6 + edge, 32 px, 8 above the composer, padding 0 / 6 / 0 / 10: a **16 px**
  "queued" cap (10.5 mono muted on 9 %, full radius, 0 / 6), the text caption, ✕ (24).
- **Composer seam**: content scrolls under the composer; a **32 px fade** (transparent → sheet) ends at the
  composer's top edge and the sheet colour continues beneath it; the list's bottom inset = composer height + 24.
- **Markers**: one centred caption line (13.2 muted) between two 7 % rules, 10 above and below, gap 10: clock glyph
  for "session started"; "Context compacted · 41k → 12k tokens · view summary" (the link underlined); shield in
  warning for "Permission mode changed to **Bypass**"; **hand-off** = both provider marks (14) with →, "Handed off
  to Claude Code · opus 4.6 · Open the new session" (underlined link). **The hand-off card is gone** (owner,
  2026-10-13 18:20): the confirm dialog shows what crosses over, once.
- **Empty session**: the project mark on a 48 px Raised tile r 12, the project name as title 600, the composer's
  hint in body muted; 40 px vertical padding; no suggestion chips.

### 2.9 Composer (Composers board A, as approved) — states
- Card: Raised r 12 + edge, padding 8, gap 4. Field: subtitle 15.4 / 22, padding 8, placeholder muted, min 1 row,
  max 8 then scrolls. Controls row as approved (text controls 28 r 6; `+` icon button; token count faint mono).
- **Send / stop** (owner, 2026-10-13 18:02): the composer's **only solid fill**. Empty: 28 px round, Floating + edge,
  muted glyph (arrow 14, 1.8). Ready (a draft, an attachment or a queued line): **FG fill, frame-coloured arrow (2.0
  stroke), no edge**. Running: the same fill with a 10 px frame-coloured square. Fill cross-fades 80 ms; the glyph
  swaps on `spring.fast` (scale 0.6 → 1 + opacity). Nothing moves.
- **Attachment chips** above the field: inset 8, 6 apart, wrap; Raised-lit r 6 + edge, 32 tall. Image chip: 30 px
  thumbnail r 5 (1 px inset) + name + ✕ (20). File chip: file glyph 13 + name + `TYPE · size` caption muted + ✕.
  Uploading: thumbnail at 50 % + a 2 px FG sweep along the chip's bottom (35 % wide, 1.6 s period). Failed: name in
  danger, the reason as its meta, ✕ removes; no retry button. Quote chip: edit glyph + "“first words…”" + `quote`.
- **Drop overlay**: over the sheet, a 2 px dashed ring white 20 % inset 8 r 8; centred a 40 px Floating tile (edge +
  drop) with the ↑ glyph 16, 10 below it the title subtitle 600 "Drop to attach to this turn", then the hint caption
  muted ("images become screenshots the agent can see"). The sheet does not dim. Tile bobs 4 px, 1.4 s.
- **@ mention and / command menus**: the one dropdown, Floating r 10, edge + float drop, **padding 6**, up to 520
  wide, 8 above the composer, max height 560. Sections: 24 px caption muted (0 / 10 / 4). Rows 36, padding 0 / 10,
  r 6, gap 10: glyph 14 (or a 120 px mono command column for `/`), name, detail muted right (mono for symbols /
  paths), key or source tag faint; selected 9 %, hover 5 %. Footer under a rule: keys faint (`↑↓ move · ⏎ insert ·
  esc close`), 6 / 10 / 2, 4 above.

### 2.10 Pickers (provider · model · effort · approval mode)
- All on the one dropdown as 2.9: Floating r 10, edge + drop, **padding 6**, caption header 28 px; rows min 36,
  padding 6 / 10, r 6, gap 10; selected = 9 % + 600 + check (14, 1.8) at the right; hover 5 %; separators 1 px 7 %
  bleeding into the padding.
- **Provider** (owner, 2026-10-13 18:23): **just the providers**, mark (16, r 4; Muse blue M, Claude clay C, Codex
  dark O) + name, the current one checked, 240 wide. No details, no hand-off rows, no new-session rows. Picking
  another provider on a session with turns opens the hand-off confirm (a session cannot switch provider in place);
  "Hand off to…" also lives in the session's ⋯ menu.
- **Model**: id (mono-free, body), context window in faint mono (`1.0M ctx`) right, badges as 9 % caps (`active`,
  `default`, 11 / 16, 0 / 6), the check on the current; 360 wide. A row with a chevron opens a **nested** submenu
  **inside the menu's box**, 4 px right of the row, top-aligned (FF's rule; an outside submenu made every pick an
  outside press); hover opens after 150 ms, → enters, ← leaves.
- **Effort**: the nine levels with their one-line meaning (list, as today) **or** the slider (owner wanted it
  drawn; pick with the owner before building): 4 px track at 12 %, filled to the current stop in FG, 8 ticks (2 ×
  10, 18 %), 16 px FG thumb with a 1 px 40 % black ring, name 600 + meaning muted above, `None · Medium · Ultra`
  under the ends and middle in 11 faint; drag or arrows snap to a stop; Default stays a row under a separator.
- **Approval mode**: Full access · Auto · Ask · Read-only with their meanings; the chip shows the short name.

### 2.11 Homes for the three controls (owner's placements, docs/40 §3)
- **Toast chip**: Floating r 10, edge + float drop, 36 px, padding 0 / 6 / 0 / 12, gap 8, max 480: dot 8 in the
  status colour (success / warning / muted for neutral), title 600, body muted (truncates), `+n` caption faint when
  queued, an optional text action (24, e.g. Undo / Show in Finder), ✕ (24).
- **Feedback slot**: the same floating pill, padding 0 / 4: the toggle (28, message-plus glyph); active: toggle lit
  (9 % + FG), note count faint mono 12.1 (`3 notes`), **Done** primary (24), **Discard** text.
- **The corner stack**: 12 px from the sheet's right, newest toast above the feedback slot, 8 apart, right-aligned.
  **Sessions: the stack's bottom is 8 above the composer's top edge. Browse: 12 above the sheet's bottom**, as an
  overlay window over the native page (memory `feedback-native-page-above-gpui`).
- **Downloads**: a 28 px icon button (arrow-into-tray glyph) in the Browse sidebar's control row, **before ✦ and ⋯**
  (row: ← → ↻ + … ⬇ ✦ ⋯), with a 6 px FG dot at the glyph's top-right while a download runs; present only while
  downloads list (as before the cleanup); its popover is the menu at Floating.

## 3. Motion (Emil's rules: ease-out for what the person asked for; no bounce on anything that must land; smooth or none)
- Fold / thinking / card / group open-close: height on `spring.moderate` (0.16 s, bounce 0), chevron rotates 90° on
  the same; the content fades in over 80 ms after the height starts (FF ThinkingStep delay 0.08). Close = exit tween
  0.12 s. Nothing else in the row moves.
- Hover reveals (turn rails, code copy): opacity 80 ms in / 60 ms out, no translation.
- Row hover overlay: 80 ms colour. FF fluid hover glide (0.08 s) on option rows and menu rows.
- Send ↔ stop: fill 80 ms cross-fade; glyph swap on `spring.fast` 0.6 → 1 scale + opacity.
- Toast: enter rise 6 px + fade on `spring.moderate`, leave 0.12 s; stack reflows on `spring.moderate`.
- Menus / pickers: FF popup enter 80 / exit 60 ms (`EnterExit::POPUP`), submenu the same.
- Streaming: caret solid (no blink); live-row shimmer 1.6 s linear sweep on the verb only; the uploading sweep
  1.6 s; drop tile bob 4 px / 1.4 s.
- Nothing scrolls on its own except "Jump to latest" (ease-out 260 ms) and the quote's scroll-to-source (same).
- Must not move: the sheet, the composer, the measure, headers of cards when their body opens, the status row.

## 4. Behaviour
- Hover is per turn (the turn's own box, rail included); the rail's height is always reserved so nothing shifts.
  Keyboard: the rail's buttons are reachable in tab order while the turn has focus.
- Quiet rows and card headers toggle on click and on Space / Enter when focused; the chevron is a state indicator,
  not a separate target.
- A failed tool card stays Raised (no ring): a failure in the past needs nothing from you. Only pending approval,
  pending question and a failure with a retry carry the status ring, and the ring leaves the moment they resolve.
- The needs-you dot in the sidebar row (warning) and the needs-you banner above the composer appear together when a
  card needing you is off screen; both leave when it is answered.
- Fold rule as built (`transcript:fold` switch): finished turns fold; the fold row's chip opens the turn's diff in
  the right pane (`open_turn_diff`) — the only action on a quiet row.
- Attachments: a tile click opens the image in the right pane; a file chip click reveals it in Files. Drop anywhere
  on the sheet attaches to the draft. Paste of long text becomes a Text attachment (chip), as today.
- Quote: the selection rail (C8b span selection) gains **Quote**; the chip is removable with ✕; sending clears it.
- Wizard questions: ⏎ = Next / Continue, ⎋ = interrupt (the status row says so), ←/→ = Back / Next, 1–9 pick, Space
  toggles a checkbox, Tab moves into the Other input. Skip skips the current question; Explain instead opens the
  host's clarify field (as today) for the whole request.
- Over a native page (Browse): toast, feedback slot and their stack are an overlay window at the same place; the
  downloads popover is a window above the page (memory `feedback-native-page-above-gpui`).
- Both modes: the toast stack rule in 2.11; Sessions anchors above the composer, Browse above the sheet's bottom.

## 5. What exists, what is new
Existing (agentic-ui `crates/aui/src/transcript/*`, composer `crates/aui/src/composer/*`; Baaz
`crates/baaz/src/transcript.rs`, `session/*`, `dialogs.rs`, `feedback/ui.rs`, `browser/*`):
- `turns.rs` (user_turn with attachments/mentions, assistant turn, rails, footer, caret) — restyle: rails hover-only,
  copy/edit only, Raised r 12 bubble, tiles 64 / chips 30, footer on hover; add the quote block.
- `turn_fold.rs` (TurnFold, LiveActivityRow) — becomes the quiet row (no `transcript_card`).
- `thinking.rs` — quiet row + indented trace; `card.rs` (`transcript_card`) — keep for cards, r 8, edge, header 34.
- `tool_card.rs` (all bodies), `tool_group.rs` (rows inside one card, no nested cards), `code.rs` (header 32, copy
  on hover, fold at 12), `markdown.rs` (image block, quote, rule values), `approval.rs`, `question.rs` (+ the wizard
  mode for several questions, new), `todo.rs`, `plan.rs`, `summary.rs`, `status.rs` (status row, needs-you banner,
  jump pill, failure card), `item.rs` (goal, generic), `marker.rs` (+ hand-off marker with two marks, new; the
  `handoff.rs` card is retired from the transcript — keep `handoff_confirm`), `activity.rs`.
- Composer: `composer.rs` (send/stop states, new emphasis), `attachments.rs` (chips replace the 44 px rows; states),
  `menus.rs` (padding 6), `pickers.rs` (padding 6; provider rows reduced; nested submenu via `interact::Dropdown`'s
  in-box submenu; the effort slider is new if chosen), `queue.rs` (16 px cap).
- Baaz: `transcript.rs` `fold_header_row` / `live_run_row` / `thinking_card` / `tool_group_card` / the resolved
  approval and answered rows (`approval_block_card`, `question_block_card`) → the quiet rows; `dialogs.rs`
  `render_toast_chip` + `feedback/ui.rs` `render_feedback_header_slot` → the corner stack (overlay window in
  Browse); `browser/*` downloads button into the control row (`aui::browse::footer`/`rows`); `providers.rs` picker
  rows; the dead-code sweep from docs/40 §3 after.
New: quote attachment flow, wizard question card, hand-off marker, video tile treatment, effort slider (if chosen),
corner stack, fold-at-12 code blocks, group-as-rows.

## 6. Out of scope; the owner's decisions
Out of scope: light theme; the right pane (D6) incl. full-output and image views; Settings (D5); the `+` menu's
contents; the `/` command list contents; a video player; the sidebar byline (as built).
Decisions, with dates (all 2026-10-13, canvas comments unless noted):
1. 17:49 — no standing actions: Run in terminal, resend, retry, fork, wrap, open-in-editor removed; copy (and edit
   on yours) on hover only; cards keep only provider-minted choices. (owner: "no unnecessary / irrelevant / extraneous
   actions", standing rule in memory `feedback-design-taste`)
2. 17:52 — nothing nests: a tool group is one surface with rows.
3. 17:57 — more components and states: reading, attachments, composer states, the remaining cards (round 3).
4. 18:01 — approval, error and a several-questions screen (round 4); 18:19 — **several questions = a wizard**.
5. 18:02 — pickers drawn, nested menu, effort slider drawn as an option, **send/stop gets the only solid fill**.
6. 18:03 — long code blocks fold at 12 lines; short ones never; the header is not a toggle.
7. 18:20 — **no hand-off card**; a marker row instead (hand-off starts a fresh session; the dialog lists the pack).
8. 18:20 — image chips show the real thumbnail (a wrong crop read as a "flag").
9. 18:21 — menus keep **6 px** of panel around the row highlight (all menus, incl. Depth/Sessions boards' menus).
10. 18:21 — the queued cap is 16 px inside its 32 px row.
11. 18:22 — sent attachments (photo, video, file), quotation and annotation turns drawn.
12. 18:23 — **the provider picker shows only providers**; no wordy rows, no new-session rows.
13. 18:25 — approved as a whole (v42), with the owner's own note moves kept.
Proposed by design and accepted by the approval (not separately discussed): radius 8 on blocks/cards (12 on the
bubble and composer); the 70 % status ring on cards that need you; quiet rows instead of cards for Worked / Thought
/ resolved; the corner-stack rule; text at 1.1 scale; the 32 px composer-seam fade.
Open items, answered by the owner at the start of the build session (2026-10-14): **effort = the slider**
(2.10, as drawn on D4Pickers; Default stays a row under the separator); **no video capture row** in the `+` menu (a
video tile is only the rendering of an attached video File); the `+` menu's rows: owner left it to the build →
**today's rows unchanged** (contents out of scope; only the 6 px padding rule applies).

Answered later the same day, during the build: **sent extras are wired in Baaz** — the wire carries text only, so
the pictures, files, quotation and Browse annotation batch a turn went out with are kept by Baaz (`sent.rs`:
`support/sent/<session>.json`, pictures copied beside it) and paired back to the turn by its text; **one answer
rhythm** everywhere — line 23 (1.6), paragraphs 12, list indent 24 (the short/long split in 2.3 is dropped); the
**skill row keeps its click** into the Skills page, with no chevron.

Owner review of the live build, 2026-10-14 (annotated screenshots, Claude Desktop as the reference) — these
supersede the matching values above:
- **Reading scale 16 / 26**: body 16 px on 26 (rendered), paragraphs 16, list items 4 apart, h1 24 · h2 20 · h3 17
  (h2 28 above / 10 below, h3 22 / 8, rules 22), blocks of a turn 14 apart, turns 28 apart; the user bubble and
  the composer field at the same 16.
- **Composer in Claude's shape**: the field is its own Raised box (r 12, 1 px edge at ink 8 %, ink 22 % while it
  holds the keyboard), chips above the text, send inside at the right; the controls row (+ · provider · meter ·
  model · mode · effort) stands outside it, 6 below; 20 px under the controls to the sheet's edge.
- **Progress inline and informative**: no row pinned over the composer. Under the last message, scrolling with it:
  a running tool keeps its own live row (verb, target, timer) plus its newest output line; otherwise one status
  row (`Thinking…`, `Waiting for approval…`, `Finishing up…`, `Working…`, elapsed, queued); streaming text shows only
  its caret. Esc still interrupts; the hint is not printed.
- **The sheet's title row stays when the sidebar is closed** (Sessions only; Browse unchanged), opaque under the row
  with a 24 px fade below it; the sidebar list fades at its scroll edges.

## 7. Live-check list for the build session (BaazCheck, both modes, over a live page where it applies)
1. Replay synthetic-markdown: user bubble Raised r 12; nothing under either turn at rest; hover the user turn →
   edit · copy · age; hover the answer → copy + footer; code block header 32 with copy on hover only; table header
   Raised-lit; content runs under the composer with the 32 px fade.
2. Replay synthetic-stress-hetero: a code block folds at 12 with "N more lines"; expand in place; the header does
   not toggle.
3. Replay synthetic-toolgroup with `transcript:fold`: fold row is a quiet row with the chevron left; open shows
   the group as one card with rows; a row's output on the Well; failed card ring-less with `exit 2` in danger;
   resolved approval as a quiet row.
4. Replay synthetic-reasoning-text: thinking rows quiet; open shows the trace indented 32.
5. Replay synthetic-question-error: the warning ring; option rows, keys, Preview disclosures; footer without "0
   selected"; status row + stop button filled; the sidebar row's warning dot; answer → quiet Answered row.
6. A live multi-question `request_user_input` (needs a wire): the wizard — one at a time, progress bar, Back/Next,
   Continue on the last, one Answered row per question after.
7. A live approval (`shell:` step raises one): the warning ring, Deny / Always allow / Allow; after: quiet row.
8. Replay synthetic-error-retry: danger ring, Retry now, the retry status row; the send button shows stop filled.
9. Replay synthetic-todo-goal: tasks card marks and 3 px bar; goal card bar clamps, number stays 120 %.
10. Composer: type → send fills FG; clear → muted; attach an image and a file (`image:` / `file:` steps) → chips;
    drag a file over the window → the ring and tile; `@` and `/` → menus with 6 px padding and the selected row
    inset; queue a message while a turn runs → the 32 px row with the 16 px cap.
11. Pickers: provider shows three rows only; model rows with ctx + badges; effort (list or slider as decided); mode
    four rows; a nested submenu opens inside the panel.
12. Homes: raise a toast in Sessions → above the composer, right-aligned 12; in Browse over a live page → 12 above
    the sheet's bottom, over the page, dismissable; feedback mode on → the slot under the toast; downloads: start a
    download in Browse → the button appears before ✦ with the dot, its popover opens above the page.
13. Reading: replay synthetic-reading: line-height 23, quote rule, ordered list markers muted, the image block with
    caption (or the missing-image block), the rule, the table.
14. Both modes: open/close the sidebar with a toast up → the stack stays put relative to the sheet.
15. The 18-entry probe exits 0 after baselines are regenerated **on main** in their own commit.

## 8. Next session (build, Opus 5.5) — the prompt
```
You are the builder for Baaz. Read, in order: harness docs/40-handoff-after-sidebar.md, docs/41-d4-spec.md (the
approved spec — it is the source of truth), docs/35, memory feedback-verify-visually-first, and the approved boards
on the canvas "Baaz v3" (https://claude.ai/artifact/7SwC39oc2nEakd4TpCdp8e, v42: rows from y 4040 down).

Implement D4 exactly as specified, steps 4–7 of docs/40 §4: agentic-ui components + gallery cards → Baaz adopts →
gates (both repos, the 18-entry probe) → build BaazCheck and drive every case in docs/41 §7 yourself with computer
use (I approve the access prompt) → me. Ask me first about docs/41 §6's open items (effort list vs slider). Then
sweep the sidebar's dead code (docs/40 §3). Any design question the spec does not answer comes back to me; do not
invent. Commit per step. Never push without asking.
```
