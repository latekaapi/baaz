# 61 — Handoff after the D6 follow-ups (2026-10-21)

Read this first in the next session. It replaces docs/60 as the "where are we"; docs/60 §4 (how to verify) and §5
(rules) still apply word for word and are not repeated here.

## 1. State of the code — the D6 stage is closed

| Repo | Head | Pushed | Notes |
|---|---|---|---|
| agentic-ui (library) | `7ab2008`, tag **v0.6.1** | yes | D6 follow-ups + the owner's last three fixes |
| harness (Baaz) | see `git log -1` (the v0.6.1 repin commit) | yes | `Cargo.toml` pins the library at `v0.6.1` |

- Main-only in both repos, no worktrees, no side branches.
- Local builds use the git-ignored `.cargo/config.toml` `[patch]` → `../agentic-ui`. With it on, `Cargo.lock` shows
  modified: **never commit it**. The repin recipe (used for v0.6.0 and v0.6.1): tag + push agentic-ui → move
  `.cargo/config.toml` aside → `sed` the tag in `Cargo.toml` → `cargo update -p aui -p aui-protocol -p aui-motion
  -p aui-tokens -p aui-icons -p aui-terminal -p aui-webview -p aui-cef` → build + test → commit `Cargo.toml` +
  `Cargo.lock` → push → put `.cargo/config.toml` back.
- Visual baselines (`design/baseline/`, regenerated at `c73df8c`) still match: the right-pane probe was clean after
  every change since. Park the pointer off the window before a probe run, or the toolbar hover tint is a finding.

## 2. What the stage finished (owner-visible)

D6 follow-ups (docs/60 §3, all four closed):
1. **Quote flash** — clicking a sent quote scrolls back and flashes the exact source span.
2. **Shell output selectable** — tool-card output quotes as "shell · N lines" code.
3. **Shift + sideways swipe over a live page** moves the pane strip (aui-cef wheel router). *Not seen live by the
   agent* — computer use cannot produce a Shift trackpad swipe; the owner's check is on the tracker.
4. **Pin notes** — a real field. Final design (after the owner's recording): while a note is open the Page is
   *covered* (its fresh picture), and the note + notes panel draw in Baaz's main window. A field in an overlay
   window over the CEF page never receives keys — don't try that again. The popover is clamped inside the page.

Found on the way: **quotes never reached the model** (only the bubble) — fixed in `session/commands.rs`.

Owner's last round (recordings + screenshot, 21 Oct):
- Pins/outlines "showing in the background" while sliding — gpui drew them *under* the native page; now never drawn
  by gpui for a native page.
- Note popover took no typing — fixed as in 4 above; seen live with real key events (scratchpad `typekeys`).
- After Send the annotations panel still listed the batch — Baaz now calls `WebviewState::clear_annotations` once
  the batch is in a session's composer (no session → toast, pins kept). Seen live.

## 3. Open items (carry, don't lose)

1. **"Page stops rendering after sliding"** (owner recording 21 Oct) — **not reproduced** (http, file://, the
   owner's kitchen-sink page, many Files↔Page slides, annotate on/off). The page was already blank at the start of
   the recording, so the trigger came earlier. Tracker check c53 asks the owner to note what preceded it. If it
   recurs, start from aui-webview `set_obscured` / `sync_visibility` / `drain` (cover_capture_until hand-off) and
   Baaz `apply_page_visibility` (`browser/mod.rs`), with `AUI_CEF_CAPTURE_TRACE=1`.
2. One probe run flagged `idle.cpu_pct = 0.6` against a 0.5 budget; the rerun was clean. Watch for it — if it
   repeats, it is real.
3. H.264 (owner's call, docs/24a) and Muse pay-as-you-go billing — unchanged.

## 4. Next stage (tracker order): finish speed P1, then P2

Source: `docs/51-performance.md` §3 (P1/P2) and the P1 results table (§7, "Looked at and left").

**4a · Muse list at launch (the rest of P1).** Muse `session/list` at boot is 27 serial pages of 200 (the server
maximum), ~5 s. The sidebar paints from the cache and nothing waits on it, but it is 5 s of work per launch.
- Probe Muse first (the version on this machine; `muse --version`): does `session/list` accept `updatedAfter`
  (or similar)? Record the answer in docs/51 before building.
- Build an incremental listing: remember the newest `updatedAt` seen; at boot ask only for newer rows and merge
  into the cached list. Keep a full re-list as a rare fallback (e.g. once a day or when the cache is missing).
- Also: the 60 Muse chain rows rewrite their search row once per launch because the boot label (index cache) and the
  list's label differ — make them agree so nothing is rewritten at rest.
- Measure before/after with the M harness (budgets in `scripts/uiprobe.py` / relay Tier V) and add a budget
  for the launch listing if it can be measured offline. No test may reach a real CLI or server
  (memory: tests never reach real CLIs); Muse turns cost money on this login.

**4b · P2 · hold the line.** Criterion benches for the fold, render cache, markdown and `visible_sessions`; a
frames-requested-by counter in `aui-motion` (would have caught the old always-rendering bug). Budgets already
run in relay Tier V.

After 4a+4b: **P4 — Muse onto the provider seam** (docs/50), ~1.5–2 weeks, then D5 Settings beside R2.

## 5. Process for the next session

- Owner wants each item: diagnose → fix → test → **seen live on BaazCheck** → one commit per fix.
- Don't push without the owner saying "push". Library changes ship as an agentic-ui tag + repin (§1).
- Update the **Baaz Tracker** (read the live version first, merge, publish) and memory at the end.
