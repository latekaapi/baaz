# 73 — Handoff: both tracks closed (providers equal + Zed-fast)

Written 30 Oct 2026 at the end of the "close both tracks" milestone (docs/69). Read this first in the next session.

## 0. Where things stand

| Repo | Head | Pushed | Notes |
|---|---|---|---|
| harness (Baaz) | see `git log` (S1–S5 on `main` after `0c30406`) | **no** — waits for the owner's "push" | built against local agentic-ui through the `[patch]` (`.cargo/config.toml` ON); `Cargo.lock` shows modified and is **not** committed |
| agentic-ui | `a93d58d` on `main` (4 commits after v0.6.3) | **no**, not tagged | MarkdownStream, ApprovalState::Cancelled, checkbox tick, Banner::body |

**At "push" (in this order):** tag agentic-ui **v0.6.4** and push it; set every `aui*` pin in harness (including
`crates/baaz-cef-helper/Cargo.toml`) to v0.6.4; park the patch (`mv .cargo/config.toml ~/.baaz-dev/harness-cargo-config-patch.toml`);
`cargo update -p aui` (also in the helper's own workspace); regenerate the `right-files` / `right-files-reveal` baselines
with the patch off (the patch adds a `.cargo/` row to the listing — environmental, 459a228 has the patch-off version);
run docs/69 §4's gates; commit the lock + pins; push both.

## 1. What was built (one line each; details in the step docs)

| Step | Docs | Commits | Result |
|---|---|---|---|
| S1 · P4-B Muse records | docs/70 (§7 as built) | 7ff7f92, c81ab7f | Muse sessions get `ProviderSessionRecord`s (at start, and on the **first open in Baaz** of an old one); every reopen goes through `reopen_provider`; Muse rows follow the same turn events as Codex; ratchet `the_open_and_row_paths_never_compare_against_muse` at 0 (was 30); `provider-sessions.json` written off the UI thread |
| S2 · P6 robustness | docs/71 (§6) | 2f8157d, 7ba0ab0, 550939a | every lane revives a lost child (Codex: fresh app-server + `thread/resume`); quiet banner + Retry now; twice in 30 s → the banner grows in place with the stderr well; typed `LaunchFailure` view for a session that never opened; approvals settle in the adapter; failed-turn rows for all three; **conformance suite** (`crates/baaz/tests/conformance.rs`, 9 scenarios × 3 adapters on fixtures/fakes) |
| S3 · P7 import | docs/72 (§6) + board P7Import (canvas v61–62, approved) | 6086a8b, f8db600 | import **in place** (records + search words; files stay in ~/.claude / ~/.codex); imported records always resume `LegacyOwner`, never moved; once + "Import more…"; Undo; scripted runs skipped; `baaz --import-dry-run` |
| Muse private home | docs/72 §4 | 2afff48 | **deferred** (owner delegated the call): the only lever moves Muse's sessions, skills, plugins and catalog together |
| S4 · speed | docs/51 §10 | 4a26a75, 068b7ed, da4aaae (+ aui 64df13f) | a turn's arrival flat (830 → 16 µs at 3,000 turns); a finished turn's sidebar work 15 → 1.6 ms; long answers 2.31 → 0.54 ms p99 per chunk; each gated by a budget the old code fails |
| Fixes | — | 04bf6b6, f6b7dd0, b38faf0, e8e9004, 550939a, cf17020, 5c1c401…c05c802, 4d817ec, 9a27a0a, dfbcd58, 7c0d806, b764f21, 909adcc; aui 6b3bbfb/a93d58d | captures never write the owner's store; "Not run — <reason>" instead of "Denied"; a replayed reopen asks no model and never moves a row; Skills header and the feedback-mode blank page; imported rows reachable in Unfiled; missing history says so; a dead first start shows the typed view; **a resumed Claude Code child now runs in the session's own folder under the right config dir** (before: every resume ran tools in Baaz's cwd, and an imported session could not resume at all) |

## 2. Things the owner should know

- **Probe runs had been writing the owner's real store** (every Tier V run, since some entries named no state dir):
  a "hello handoff probe" composer-history entry and an `s-scripted` override row. Fixed (04bf6b6) and cleaned by
  hand; backups beside the files: `history.json.bak-2026-10-30`, `sessions.json.bak-2026-10-30`. One P6 agent probe
  also set `provider.json` to codex; it was set back to `muse` (inferred from the 29 Oct baselines — check the
  "New sessions start in" choice once).
- **Disk:** `harness/target/debug` (44 GB), `agentic-ui/target/debug` (29 GB) and `cockpit/target` (16 GB, owner OK'd)
  were deleted; ENOSPC had killed a subagent. Keep ≥ 25 GB free before long chains.
- **The import on this Mac:** 1,744 Claude Code sessions offered in 33 folders; only `harness` (66) is a Baaz project, so
  the defaults import 67. Codex offers none: all 83 of its sessions are `codex exec` runs.

## 2b. How it was checked

Every step: workspace tests (2,207 at the end), clippy, 42 Tier V entries (pointer parked), 27 perf budgets, all green.
Visually: two Sonnet passes on BaazCheck with scripted `--screenshot` captures on a copy of the owner's state (computer
use on `sh.baaz.check` is denied instantly when nobody is at the machine; the owner chose to wait until away). Not
seen: hover, real clicks on "Show in sidebar", native web views — those are on the owner's checklist.

## 3. Still open (not this milestone)

- The import sheet does not trap Tab focus (Tab reaches "New session" behind it).
- After a revive fails mid-turn, the banner and the "stopped unexpectedly" card show at once (redundant).

- A very long answer's **frames** grow to ~30 ms at 120 KB: gpui lays out and paints the whole growing markdown element
  per frame (docs/51 §10). Virtualising inside one block is the fix.
- `idle.cpu_pct` reads 0.6–1.0 % with the window visible (gpui's display link, docs/51 §9); the budget is 0.5.
- The sidebar grouping rebuild after a moved row is still O(rows) (~1 ms at 2,600).
- The Muse ledger row is now tagged `"muse"`; a Muse reopen keeps the previous view until the lane lands (docs/70 §7).

## 4. Next milestone (owner's pick)

Browser R1 → R3–R5, or D7/D8 (Skills page, dialogs, app chrome). The tracker's "Next" list has both.

## 5. Addendum, 31 Oct–4 Nov: the owner's test round and close-out (supersedes §0)

Pushed: agentic-ui **v0.6.4**, Baaz pinned to it, the local patch parked again. The next handoff is docs/74.

The owner's test of the finished build found what the gates had not. All fixed:

| Reported | Cause | Fix |
|---|---|---|
| Hidden Browse page's link bubbles over Sessions | a hidden page window was only transparent and stayed over its rect | aui `shim.m`: input monitor, bubble ordered out, mouse-leave on hide (2781cc4, 71dd4c2) |
| Effort submenu cut by the window | submenu hung from its row with no edge check | `aui::overlay::submenu_seat` (87f8bf9) |
| "Finishing up… memory reminders" holding the composer | everything followed `busy()` | `working()`: the reply reads finished, Send works, a send queues on Muse (d4dda96, 65bc735) |
| Code preview scrolling sideways | `code_view` never wrapped | soft wrap by default, Wrap button and ⌥Z, `codeWrap` (ac13131, 83b1d20) |
| App hang | **not reproduced.** Likely `bundle.sh` swapping under a running Baaz, so new Chromium helpers ran under the old engine (`observer_list` failures in `Browser/cef.log`). Unproven: the hang's time is unknown | `bundle.sh` parks at `target/bundle/next`, `--swap` after quit (1aa4884); `logs/baaz.log`, stall watchdog; per-row `realpath` off the UI thread (5ff8de1) |
| Back under the shade; no "New sessions start in" in Settings; question hidden on answered rows; loud field focus; Skills too wide | | 36a4aaf, 7e24f8e, aui 1c4dd37, 7a4f4ba, 878d111 |

The final live test (driven from the main session, see docs/74 §2) passed all 10 checks and found three more, also
fixed: a transient plan probe stuck as a false "Muse did not say which plan" banner (2859c50), the Jump pill over the
banner (e458888), and 434 false background `stall:` lines (3a9a3e3).

Not seen live, only under test clocks: the plan probe's automatic retry and wake path. Not proven: why that probe failed
(it is logged now).

**Correction:** 5930762's message says idle CPU is "back to 0". It is not a fix: `idle.cpu_pct` reads 0 or about 1 %
depending on machine state, on either side of that commit (control: 1.05–1.1 on both). This is the display-link wake in
docs/51 §9, still over the 0.5 budget when the window is visible, and it fails the perf gate on some runs.
