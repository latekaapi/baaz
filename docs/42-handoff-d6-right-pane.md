# Handoff — D6 the right pane, designed next (written 2026-10-16)

The owner moved **D6 · Right pane ahead of D5 · Settings** (2026-10-16). The sidebar and the transcript are
complete; the pane beside the transcript is the most visible screen still wearing pre-v2 design. Same loop as
D4 (docs/40 §4, §6): a Fable session mocks it on the canvas and writes the spec, the owner approves, an Opus
session builds it.

## 1. Open with the two D4 leftovers (short round, owner approves both)

1. **Bring the canvas "Baaz v3" (v42) up to the shipped D4.** The owner's two review rounds changed it after
   the canvas was approved: reading scale 16/26 (h1 24, h2 20, h3 17, blocks 14 / turns 28 apart); the
   composer in Claude Desktop's shape (field with send inside, controls row under it, focus edge ink 8→22 %,
   floating over the transcript with a fade and shadow, 20 px above the bottom); inline informative progress
   instead of a pinned status row; opaque title row + 24 px fade; sidebar list edge fades; the terminal dock
   as a surface; pickers 6 px above their own button, right-side ones right-aligned with submenus opening
   left. Source of truth: docs/41, section "Owner review of the live build", and the running app
   (`target/bundle/Baaz.app`). Capture the real app rather than redrawing from prose.
2. **Design the quote flow.** docs/41 assumes that selecting text in an answer brings up a bar with **Quote**
   (the quote then lands in the composer); Baaz has no selection bar. Mock the selection state, the bar, and
   the quoted block in the composer, and decide what happens with a selection across blocks and inside code.

## 2. D6 — what the pane is today

Code: `crates/baaz/src/right.rs` (≈4.2 k lines), kinds in `crates/baaz/src/layout.rs` `RightKind`:
**Browser**, **Changes** (this session's edits over the working tree; the old Diff and Git kinds are legacy
aliases of it), **Files** (tree + file preview). Toggled from the header button / ⌘⌥B / ⌘\\, width and kind
persisted per session. Probe entries to capture each state (`scripts/uiprobe.py`): `right-browser`,
`right-browser-page`, `right-tabs`, `right-changes`, `right-files`, `right-files-reveal`, `right-file-preview`,
`right-file-preview-rs`, `right-closed`.

Known gaps to design for, not around:
- File rows are still grey FF-secondary buttons; the file preview has the pre-D4 look.
- D4's tiles and "N more lines" rows open a **full output** view and an **image** view here; they need
  a proper home in the pane.
- aui workbench components size to their content rather than filling the pane; `diff_review`'s header
  clips below ~460 px while the pane's minimum is 280.
- Header icon buttons need accessible names (CLAUDE.md: every interactive element gets a role + label).
- The Browser kind in the pane vs Browse mode (docs/27): the pane's browser is the session's page; keep the
  two concepts distinct and few (memory `feedback-count-concepts-before-building-ui`: ≤6 concepts).

The owner's list for the pane: **file tree, diff, git (status, commit, PR), browser, file preview**, plus
whatever views the transcript opens (full output, images). Ask the owner for the reference product early
(the sidebar and transcript used Claude Desktop) and whether git actions (stage, commit, PR form) belong in
this pane or in Changes only.

## 3. Deliverables

1. Boards on "Baaz v3": the canvas refresh and the quote flow first; then every pane view at the quality of
   the existing screens, open/closed, narrow (280) and wide, empty/loading/error states, light and dark.
2. Owner iteration to approval.
3. `docs/43-d6-spec.md` in the docs/40 §6 format, including a live-check list for the build session, and
   committed. (D5 Settings then becomes docs/44.)

## 4. Prompt for the design session (Fable 5.1)

```
You are the designer for Baaz. Read, in order: harness docs/42-handoff-d6-right-pane.md, docs/40 (§4 and §6),
docs/41 (the D4 spec and its "Owner review of the live build"), memory project-d4-shipped-2026-10-15,
feedback-design-taste, feedback-ff-is-the-spec, feedback-mock-first-then-muse, feedback-fable-designs-opus-builds,
and the canvas "Baaz v3" (https://claude.ai/artifact/7SwC39oc2nEakd4TpCdp8e, project/canvas.json then the boards).

Design only — write no product code. First bring the canvas up to the shipped D4 and mock the quote flow
(docs/42 §1) and get my approval. Then design D6, the right pane (docs/42 §2): capture the real states with the
probe entries, mock every view on the canvas, iterate with me until I approve, then write docs/43-d6-spec.md to
the docs/40 §6 format and commit it. Never push without asking.
```

The build prompt is docs/40 §7's, with `docs/43-d6-spec.md` in place of docs/41.
