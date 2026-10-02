# r1002 — owner round of 2026-10-02: plan and decisions

The six diagnoses beside this file (`handoff.md`, `sidebar.md`, `rightpane.md`, `transcript.md`,
`isolation.md`, `reference-design.md`) are the evidence. They were written read-only against main @ 8bbbc41.
Where this file and a diagnosis disagree, **this file wins** — it records the owner's decisions.

## Owner decisions (2026-10-02)

1. **Settings is a full page that replaces the sidebar.** While Settings is open, the left column shows the
   settings section list (not the session sidebar); the centre+right area shows the section's content.
   A Back control at the top of the section list (and Escape, after it has first cancelled a shortcut
   recording / cleared search) returns to exactly the session, scroll, draft and panes that were showing.
   This overrides `reference-design.md` §1's "keep the sidebar" recommendation; everything else in §1
   (sections, single content column ≤ 680 px, constant height, Providers absorbing the Providers dialog,
   Shortcuts de-duplicated, the model-spending switches moved to General) stands.
2. **Diff review + Git changes merge into one "Changes" view.** The right pane has three tabs:
   Changes · Files · Browser. Changes shows this session's edits on top and the rest of the working tree below.
3. **Finished turns fold.** After a turn settles, the tool calls AND the agent's interim prose fold into one
   expandable "Worked for 2m 14s · read 12 files, edited 3, ran 5 commands" row; the final answer stays
   outside the fold. While a turn is live everything shows (interim prose verbatim, one live row per run of
   tool calls). Kept visible after settle: denied approvals, unrecovered failures, answered questions, plans,
   handoff dividers.
4. **Old provider sessions move into Baaz's own homes** (nothing deleted), after a dry-run list and an
   explicit confirmation. Codex threads already listed in Codex for Mac are NOT archived by Baaz (the owner's
   `~/.codex` index is never written). Unregistered probe/test leftovers stay where they are.

Decided without asking (state them in reports):
- Handoff never blocks on the model summary when the carried turns already cover the session; otherwise it
  opens with the extractive pack and adopts the summary only if it is already back.
- Project model/effort defaults are keyed by provider.
- Muse's 503 overload (the "hi" that failed after 10 × 60 s) is Muse's backend, not Baaz.

## Verified before planning (no model turns)
- `CLAUDE_CONFIG_DIR=<dir> CLAUDE_SECURESTORAGE_CONFIG_DIR= claude auth status` → `loggedIn: true`
  (without the empty `CLAUDE_SECURESTORAGE_CONFIG_DIR` → `loggedIn: false`). `email` reads null in a fresh dir.
- `CODEX_HOME=<dir with auth.json + config.toml symlinked> codex login status` → "Logged in using ChatGPT".

## Tasks

Library (agentic-ui, ships as v0.3.15, then baaz bumps the tag):
- L1 compact session rows (1 line / 2 lines) + active-row fill + current-project marker coexists with the
  running bar + hover card title clamp and max height
- L2 handoff card step list
- L3 transcript primitives: generic card collapsed + capped; Mcp/Search caps; live shell shows the last lines;
  group header without the duplicate count; TurnFold ("Worked for …") and a live activity row
- L4 right header with a tab strip (no title required)
- L5 aui-webview: no notify loop for hidden/unchanged webviews; native bounds only at rest, not per spring frame

Baaz wave 1 (no library dependency):
- B1 new-session row regression · B2 per-provider defaults + chip/menu agree · B3 handoff flow
- B4 isolation: env scrub + Baaz-owned CLAUDE/CODEX homes · B4M migration of old sessions (confirmed, dry-run)
- B5 row status agrees with the live view; approvals settle per tool, not per turn; Codex running state
- B6 usage refresh and persistence · B7 pane toggle cost (no settle/search rebuild; webview prebuilt; no close placeholder)

Baaz wave 2 (after v0.3.15):
- B8 right-pane tabs + merged Changes · B9 links → in-app browser, file/folder links → Files/preview, tree-sitter
- B10 active session/project marker + scroll-follow + compact rows wired + title clamps
- B11 Settings full page · B12 transcript fold wired for all three providers · B3P handoff card progress wired
