# Plan 87: close out Plan 85 (everything except 9b)

Written 26 Nov 2026 from an independent audit (a Fable advisor checked every task row, commit and evidence
file of Stages 10–17; notes kept with the session) and spot-checks by the build session.

**Verdict.** 10, 9a, 13 and 17 are complete to their own "done when". 11, 12, 14, 15 and 16 are built, but each has a gap against its "done when" or its gate. Two of these gaps block a clean close:
- Gates 14 and 15 were run without Claude Code while it was signed out.
- Neither gate was rerun after Claude was signed in again.

Everything else is should-fix or cosmetic. Nothing is pushed.

How to run this plan: exactly as docs/85-plan.md §0 says. The loop per task, the branches, status and tracker updates, live checks, and when to stop all carry over, and so do the standing rules in §1 and CLAUDE.md. Branch `plan/s18` from `main`; track rows as `18.<n>` in docs/85-status.md (a new "Stage 18 — Close-out" table) and on the tracker. The run's real-turn budget is **up to 20 Claude/Codex turns**. Muse turns need the owner's say. Live turns happen only in throwaway repos with redirected homes (rules 24, 25).

Correction to the audit: Claude's `skillOverrides` *was* probed live on 26 Nov. In `docs/85-evidence/16-claude/probe-out.txt`, `probe-home-on` is listed and `probe-home-off` (switched off) is not. Only the write-up is missing. Task 18.6 covers it.

---

## Tasks, in order

| ID | Task | Done when |
|---|---|---|
| 18.1 | **Clippy clean.** Fix the warnings in `provider-claude-code`: the manual char comparison, `map_or(true, …)` at account.rs:212, and the two `.clone()`-to-slice in its tests. | `cargo clippy --workspace --all-targets` and `cargo clippy -p baaz --no-default-features` both print 0 warnings; logs saved to `docs/85-evidence/s18/`. |
| 18.2 | **Rule 28: entry lists.** CLAUDE.md's Tier V lists lack every entry added in Stages 13–16: `pane-show-*` (9), `pane-comment`, `pane-push-ask`, `revert-file-confirm`, `workers-*` (4), `goal-*` (4), `settings-usage`, `skills-folders`, `branches-message`. Add them. Add a small check (a test in `tests/` or a `--check-docs` mode in `scripts/uiprobe.py`) that fails when an entry in `uiprobe.py` is missing from CLAUDE.md. Add `revert-file-confirm` to the gate's `tierv.sh` lists. | The check passes. Planting a fake entry makes it fail (shown once in the log). |
| 18.3 | **Gate 14 with Claude.** Rerun Gate 14's three-step example on the check build: lead (Codex), **Claude Sonnet writer**, Codex reviewer. Use a throwaway repo and Claude's redirected home. | Captures of the sidebar, Workers block, pane and summary are in `docs/85-evidence/s14/claude/`; turns are counted in status. Any defect found is fixed with a test before the row closes. |
| 18.4 | **Gate 15 with Claude.** One real goal on Claude Code: "make this failing test pass" in a throwaway repo, reaching **Met**. | The pill capture and log are in `docs/85-evidence/s15/claude/`; turns are counted. |
| 18.5 | **13.7 rerun** (owed "at Gate 14"). Rerun the per-provider tool-cost measurement now that 14–16 are in. | The numbers are appended to `docs/85-evidence/13.7/RESULT.md` with a one-line comparison to the first run. |
| 18.6 | **16.2 write-up.** Record the `skillOverrides` result already in `probe-out.txt`. Then confirm it through Baaz: switch one skill off on the Skills page and run one Haiku turn in a throwaway repo. | `docs/85-evidence/16-claude/skilloverrides.md` quotes both: the probe line, and the live turn not offering the skill (init `skills` list saved). |
| 18.7 | **`session_read` on never-opened sessions** (11.1's one-time import was folded into "import on reopen"). Today `memory_tools.rs:115` refuses a session Baaz never opened. Pick one: (a) the planned one-time import per provider, run in the background on first launch after this change; or (b) a fallback in `session_read` that folds the provider file on demand and stores it. Prefer (b): it is smaller, and the store fills as it is used. | A test with a never-opened Claude fixture and a never-opened Codex fixture (real-shaped files, no CLI) returns turns. |
| 18.8 | **11.3 follow-through.** Make search run over the store's tables (11.3 built the tables). Delete `search.db`'s `sessions_fts`, the Muse `search_text` path and what's left of `provider_search.rs` (rule 11). If store search can't cover a case the old path covered, stop and ask (§0.5). Also confirm 11.2's "sidebar renders from one query". | The 11.3 tests still pass with the old paths deleted. The keystroke budget passes in release. A test asserts the sidebar list comes from one `session` query. |
| 18.9 | **16.3 worktree and non-git chip.** Two things are not built: (1) a branch starting from the turn's checkpoint in **its own worktree**; (2) the chip saying the branch shares the folder when the folder is not a git repo. Mock first is not needed: the chip uses the existing chip component. ACP has no branch. That's acceptable if the control is hidden on ACP (rule 13); confirm it is. | A scripted test shows a Codex and a Claude branch opening in a new worktree at that turn's checkpoint, with the files matching the checkpoint. The capture `branches-nongit` shows the chip; add it to `uiprobe.py` and CLAUDE.md. |
| 18.10 | **Small built-but-missing items:** (a) read Codex `thread/goal/updated` (provider-codex/src/lib.rs:1257 is only a comment), with a fixture test; (b) 12.1's ACP handoff-pack fallback (a test), or record that the owner waived it; (c) an "unofficial" label on Claude's limits source in Settings › Usage (16.1 asks for it), with a capture. | Each has its test or capture. |
| 18.11 | **Usage page two-card gap** (agentic-ui `crates/aui/src/screens/usage_page.rs:121`: fixed card width). Make two cards share the row equally. Tag aui `v0.7.12` locally and pin it. | The gallery `usage/page` capture shows two equal-width cards in both themes (`scripts/gallery-baseline.py` in aui; its baseline in its own aui commit), and harness `settings-usage` is re-baselined on main in its own commit. |
| 18.12 | **Docs and hygiene.** Fix the misleading evidence: s16's README still says "NOT closed", and its `gate-tests.txt` and `gate-clippy-nodefault.txt` are the aborted and failed runs. Add a one-line "superseded by …" note to s11 `final-gate.txt`, s12 `gate*.txt`, and s10/s9a (which have README claims only). Correct docs/85-handoff.md "Open items carried": Needs owner 10 and 11 are resolved; aui `main` is `f72bb61`. Archive the merged branches as tags `archive/plan-s<N>` and delete the branches: harness `plan/s9`…`plan/s17`; aui `plan/s12`, `s14`, `s15`, `s16`. | `git branch` shows only `main` (plus `codex/*` if still open) in both repos; the docs say what is true. |
| 18.13 | **Gate 18 on `main`** (after the merge): Tier V (all CLAUDE.md lists, populated set, and the new entries) **before** `cargo test --workspace`. Then both clippy runs and `scripts/flat-gate.py` with pixels. | All logs are in `docs/85-evidence/s18/`; Tier V has no new findings; tests are green; clippy is at 0; flat-gate passes. |

Order: 18.1–18.2 first (cheap, and they unblock a clean gate), then 18.7 → 18.8 (18.8 needs 18.7), 18.9, 18.10, 18.11. Do the live rows (18.3–18.6) together in one owner-away window. Check `HIDIdleTime` first; if the owner is at the machine, ask them to step away. Finish with 18.12, then 18.13.

**Stop and ask (§0.5) if:**
- 18.8 would lose a search case;
- 18.9 needs a new field on `Harness` or `SessionView`;
- a live gate finds a defect that needs a decision.

## Owner-only (not in the build session)
- **Push** harness and agentic-ui (with tags v0.7.9–0.7.12), then park the `.cargo/config.toml` patch and commit `Cargo.lock`. Until then a clean clone of `main` cannot build. That also blocks 9b.
- Needs owner 7 (Apple credentials → 9.2, 9.3, 9.6) and 8 (patch-cadence reminder).
- Accept or reject: the 11.1 import deviation if 18.7 takes option (b), and the 12.1 ACP fallback if waived.

## Accepted as logged, not reopened
These were never seen live: auto-compaction firing, the lint fixing a note, the Read stub (claude 2.1.317), the AGENTS.md plugin setting (2.1.277), and the API-key cache TTL. Each waits for a newer CLI or a long session, as Plan 85 recorded.
