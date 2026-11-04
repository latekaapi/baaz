# 74 — Handoff: next milestone, the browser (R1 re-shell ∥ R2 agent core)

Written 31 Oct 2026, at the close of "close both tracks" (docs/69–73). Read this first in the next session.

## 0. Where things stand

| Repo | Head | Pushed | Notes |
|---|---|---|---|
| harness (Baaz) | `main`, pinned to agentic-ui **v0.6.4** | yes (31 Oct) | `.cargo/config.toml` patch parked again at `~/.baaz-dev/harness-cargo-config-patch.toml` |
| agentic-ui | tag **v0.6.4** | yes | |

Both tracks (providers equal, Zed-fast) are closed: docs/73 lists what landed, the defects found live, and what is
still open. The owner's Baaz is `target/bundle/Baaz.app`; `scripts/bundle.sh` now **parks** a new build at
`target/bundle/next/Baaz.app` while a Baaz is running (a mid-run swap mixed two builds' Chromium helpers and is the
likely cause of the 31 Oct hang); `scripts/bundle.sh --swap` after the owner quits. Baaz now writes
`<state>/logs/baaz.log` and a stall watchdog (`stall:` lines, `logs/stall-*.txt` samples) — read them first when the
owner reports a hang.

## 1. The milestone

docs/27 (browser redesign v2, decided by the owner 7 Oct) §8: **R1 re-shell** in harness and **R2 agent core** in the
library, in parallel, merging at R3. The owner asked for a clickable prototype in the agentic-ui gallery before wiring
(docs/27 §10). The canvas boards for Browse are on "Baaz v3" (`docs/34-refs/v3-redo/canvas/`).

- **R1 re-shell (harness):** docs/27 §4 cut list (delete the old Browse shell); overlay windows (the Field, menus, the
  line) over the native page; no header over the page — nav + address line in the sidebar, page full height; pins /
  your tabs / folders / task folders / Space footer; the Field (tabs + commands + search); ⌘S hide with the 28 px
  lights strip and edge reveal; the right pane shares the address component. Owner check: Browse looks like the mockup,
  nothing freezes, a session's dev-server tab shows as a task folder.
- **R2 agent core (library + seam):** `PageAutomation` on CEF; the §6.1 tools with `tab`; snapshot/refs/diff;
  console/network/errors/emulate; `browser_ask`; approvals (§6.2); the line's take-over / hand-back; session tabs render
  off-screen "as if visible". Owner check: from a coding session, "open :5173, click through checkout, show me console
  errors" works; "pay" stops at the approval bar.

Alternative the owner may pick instead: D7/D8 (Skills page, dialogs, app chrome) — the tracker's "Next" list.

## 2. Rules that bit (carry over)

- New UI → a board on the "Baaz v3" canvas and owner approval before building (feedback: mock first); FF values are the
  spec; count concepts (≤ 6).
- The native CEF page draws above all gpui UI; a page that is not on screen must take no input and draw nothing
  (fixed 31 Oct in aui-cef `shim.m`: event monitor, bubble order-out, mouse-leave on hide) — keep it that way.
- Gates every step: docs/69 §4 (tests, clippy, Tier V on every CLAUDE.md entry with the pointer parked, bundle + perf
  budgets). `idle.cpu_pct` can miss at ~1 % with the window visible: known display-link wake (docs/51 §9, docs/73 §5).
- Live checks: `computer_batch` aborted with "user interrupt" in **subagents** even with the Mac idle (343 s), three
  runs in a row, and worked first try from the **main session**. Background `app_*` clicks do not reliably drive gpui. So
  the main session drives the live test itself on `sh.baaz.app` with the owner's OK, or a cheap agent runs scripted
  `--screenshot` captures on a state copy that includes `claude-home`/`codex-home`. Offscreen captures cannot see the
  native page or real pointer input.
- Never write the owner's real store from a probe (captures fall back to a temp dir now); throwaway logins for
  anything that signs in; no Muse turns unless announced.
- Disk: keep ≥ 25 GB free; `target/debug` grows to 40+ GB — delete it when it does.
- Don't push until the owner says "push"; library changes ship as an agentic-ui tag + repin.

## 3. Prompt for the next session

> Drive the next Baaz milestone to completion: the browser, **R1 re-shell (harness) ∥ R2 agent core (library)** per
> docs/27 §8–§10. Read `docs/74-handoff-browser-r1-r2.md` first, then docs/27 (all of it), docs/73 (what just closed),
> and the live Baaz Tracker (https://claude.ai/artifact/Fr965AKBn8toZ7BcUk7wCP). Both repos are pushed (agentic-ui
> v0.6.4). Start with the clickable gallery prototype docs/27 §10 asks for and any board the design still lacks, and
> get my approval before wiring new UI. Restore the local patch from `~/.baaz-dev/` only when the library must change,
> and never commit `Cargo.lock` while it is on. Work on main, one commit per step, docs/69 §4's gates every step, and
> check every step visually on the live app (docs/74 §2 says who can drive it; ask me for computer use while I'm here). Anything the
> docs don't answer comes back to me. Don't push until I say "push". At the end: tracker (merge into the live version,
> short checklist), memory, handoff doc, report per step.
