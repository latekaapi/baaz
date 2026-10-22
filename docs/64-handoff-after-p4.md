# 64 — Handoff after P4: Muse onto the provider seam (2026-10-22)

Read this first in the next session. It replaces docs/62 as the "where are we"; docs/60 §4 (how to verify) and §5
(rules) still apply. The plan and the as-built record are docs/63 (§3 steps, §6 progress).

**Stage status: P4 BUILT, owner-tested, PUSHED (2026-10-22).** One P4 remainder waits on an owner decision (§4.1).

## 1. State of the code — pushed

| Repo | Head | Pushed | Notes |
|---|---|---|---|
| agentic-ui (library) | `c2ae1a1`, tag **v0.6.2** | yes | unchanged by P4 |
| harness (Baaz) | `82e6212` (P4 + fixes, repin `c049d76`, baselines `82e6212`) | yes | `Cargo.toml` pins **v0.6.2** |

- Main-only in both repos. The local `.cargo/config.toml` `[patch]` → `../agentic-ui` is back in place for work, so
  `Cargo.lock` shows modified again: **never commit it while the patch is on.** The repin recipe is docs/61 §1.
- Visual baselines regenerated on main at `82e6212` (right-files, right-files-reveal changed on purpose;
  right-files-scrolled new); the probe against them reports 0 changed regions. **Park the pointer** before a probe
  (warp it to a screen corner): a hover icon on the Changes fixture otherwise reads as a finding.
- The owner's Baaz runs `target/bundle/Baaz.app` built from `7872ad6` (same code as the push).

## 2. What P4 did

Muse is a provider like the others. `provider-muse` owns the one `muse serve` child as a **hub**
(`provider_muse::MuseHub`): every open Muse session is a lane (`MuseLane`, a `ProviderAdapter`) with its own event
channel; a connection stream carries what belongs to no single view (row facts for every session, sign-in, usage,
the child's exit). `crates/baaz` depends on neither `muse-client` nor `muse-adapter`; `tests/seam_ratchet.rs`
fails if either comes back, or if a view forks on the lane it rides.

| Step | Commit | In one line |
|---|---|---|
| 1 | `adf3d28` | Views render from the neutral `session-store`. |
| 2 | `19ddb85` | The seam's vocabulary: session facts, settles, restored prompts, status, typed refusal hints, queued acks. |
| 3 | `eaa9c96` | The hub: one child, lanes per session, pinned folds, orphan buffer, reconnect-and-resume. |
| 4 | `ecc3634` | Muse sessions open, resume, fork and replay through the provider path; one command path for all three. |
| 5 | `1daa488` | The Muse lane and its fold are gone from the session view. |
| 6a | `686529f` | Sign-in, sign-out, API key, usage: neutral commands on a control lane + connection events. |
| 6b | `75ac929` | Titles, bylines, handoff summaries, drafts: `utility::complete` for every provider (`MuseHub::complete` for Muse). |
| 6c | `e1c45ca` | No raw pump, no `Harness.client`, no `conn::Legacy`; rows from the hub's events; reconnect on `ConnectionLost`. |
| 7 | `9684d3b` | Muse rows through `ListSessions`; `ListedRow` keeps the saved list readable; `SessionEntry.provider` always set. |
| 8 | `352ac09` | Last mentions; registry properties replace `uses_legacy_pump`; the ratchet is absolute. |

Live checks (BaazCheck, fresh state; the screen was locked, so logs plus the app's own `--screenshot`):
- Two **short live Muse turns**, each announced first: the step-4 demux (PONG) and the step-6b side completion on
  `muse-spark-1.3` (PONG, 14.8 s).
- Sign-in read, usage read ("nothing observed yet" on a fresh child), reconnect after SIGKILL of `muse serve`.
- The owner's saved list (2,661 rows, written by an older build) read as `ListedRow` and topped up with one page.
- A Claude Code session under `--approval-mode promptUnmatched`: real model menu, Ask chip, Muse rows beside it.

**Driven by hand on 22 Oct (screen unlocked, BaazCheck, full-screen control):** sidebar loads 2,400+ Muse rows
through the seam; the account menu shows Muse (pay-as-you-go), Claude Code (Max) and Codex usage; an old Muse
session reopens with its history, model chip, token meter and billing banner; Muse's model, approval-mode and
effort menus read the neutral rows (five models, Muse's mode wording, Minimal–Max with "default: Max").
**One defect found and fixed (`fix(P4)` commit after `b39891c`):** reopening a Muse session whose history ends on a
turn that never finished (a relay run cut off mid-turn) read "Working…" with a stop button for ever. The lane had
started inferring a running turn from history deltas, which only Claude Code / Codex need; Muse announces its
turns (`ProviderId::reports_turn_lifecycle`), and the resume envelope's active turn (when Muse sends one) now
announces a running turn. Test `a_muse_history_turn_that_never_finished_is_not_working` fails without the fix.
Not driven by hand: a new Muse session's generated title (costs a turn) and Muse sign-out/in (your real credential).

**Owner recording, 22 Oct evening ("not able to send", fixed in `7872ad6`):** the login is genuinely on
pay-as-you-go (a fresh `--print-tier` probe of Muse's plan card agrees; `tier.json` still remembers an earlier
"Subscription"), so the docs/06 §4 guard held every Muse send. It put the words back and said nothing, so the arrow
read as dead. A held send now shows "Not sent: this login bills every turn. Press Send anyway to send it." Two
P4 regressions found while reproducing it, also fixed: a new Muse session's row was built running ("Working · now"
before any turn), and the catalog invented a checked "muse" model row (the chip read "muse"). New `send-anyway`
step verb. **Four short Muse turns were spent by mistake** in a scripted check: a fresh state's cached
pay-as-you-go answer was older than 10 min, so the banner was re-verifying and (by design, B7) did not block.
Use `--tier payg` to script a held send. Park the pointer before probes (`tools/park` warps it to a corner): a
hover icon on the Changes fixture otherwise reads as a finding.

## 3. How to verify (unchanged gates)

```bash
cargo test --workspace
cargo clippy --workspace --all-targets
scripts/bundle.sh && python3 scripts/perf-budgets.py --binary target/release/baaz
cargo build -p baaz && python3 ~/.claude/skills/relay/scripts/relay_visual.py --repo . <the 18 entries in CLAUDE.md>
```

End state: 2,108 tests, 0 failed (9 ignored: the live-mode suites that spend turns, two real-copy perf timers, two
benches — both benches were run and pass); clippy clean; budgets 0 missed; probe shows only the two intended
Files-pane changes.

`BAAZ_TRACE=1` turns on the boot marks (`session/list-work-done … mode=Newer`), which is how the listing was checked.

## 4. What is left

1. **Muse still branches by name in the app's open and row paths** (≈24 `== ProviderId::Muse` in
   `app/lifecycle.rs`, a few in `app.rs`, `app/titles.rs`, `right_changes.rs`): reopen via `ResumeSession` because a
   Muse session has no Baaz record; Muse start parameters; the boot catalog warm-up; Muse rows following the hub's
   `TurnRunning`/`TurnSettled` instead of the view's `ProviderTurnAccepted`/`Finished`. Commands are one path; these
   are not yet. Two ways to finish: name each in the registry (cheap, cosmetic), or give Muse sessions Baaz records
   like Claude/Codex so open/reopen/rows become one path (real, bigger — a design call for the owner).
2. **A stray Muse session from the 6b live check**: "Reply with exactly one word: PONG" (Unfiled, side session id
   `01a16029-e4a7-7277-9dfa-4c0862411a24`). Archive it from the sidebar; it was not hidden because the scratch driver
   used a plain prompt and a temp workspace.
3. **Provider-status log race (pre-existing, seen since step 4):** a boot Codex probe logs `muse=Installed · sign-in
   not verified` after Muse already read Connected. The UI keeps Connected; only the logged line is stale.
4. **Background Muse rows stand down on the idle `statusChanged`**, not on the settle itself — the same as before P4
   (pinned by `the_hubs_connection_events_drive_muse_rows`).
5. **One launch listing took 5.2 s** (others 1.5–1.6 s, docs/51's cold-Muse band). Measured outside the app, the lane
   and the raw client list the same filtered page in 100–540 ms, so it is boot contention, not the seam.
6. `env_path::tests::a_timeout_kills_what_the_rc_started` flaked once under full load (passes alone 3/3).
7. Leftovers kept on purpose: `MuseAdapter` (the watch-all wrapper, used by provider-muse's own tests) and
   `muse_adapter::mirror` (no longer used by baaz).

## 5. Traps found this stage

- `muse serve` ignores SIGTERM: a reconnect test needs SIGKILL. And `pgrep -P <app> -f muse` also matches the tier
  probe's TUI child — match `muse.* serve`.
- `clientInfo.name` must match `[a-z0-9_]+` (`baaz-check` is refused at `initialize`).
- `session-list-cache.json`: the envelope is snake_case (`full_at_ms`), the rows camelCase (Muse's own keys).
- `rmfn.py`-style removals take the item's doc comment with it; an inserted item can split a doc from its struct
  (clippy's `empty_line_after_doc_comments` catches it).

## 6. Owner checklist (when Baaz is back in use)

- [ ] Sign in / sign out of Muse from the account menu; the footer and banner follow.
- [ ] A new Muse session: first send titles the row; the title arrives.
- [ ] Reopen an old Muse session: history pages in; model and context read right.
- [ ] Usage card in the account menu after a Muse turn.
- [ ] Archive the stray "Reply with exactly one word: PONG" row (§4.2).

## 7. Prompt for the next session

> Continue the Baaz build. Read `docs/64-handoff-after-p4.md` first (where we are), then `docs/63` §6 (P4 as built),
> `docs/50` §4/§6/§7 (the providers-equal plan and decisions) and `docs/51` (performance), and the live Baaz Tracker
> (https://claude.ai/artifact/Fr965AKBn8toZ7BcUk7wCP) before changing anything. Both repos are pushed (agentic-ui
> v0.6.2, Baaz `82e6212`); work on main with the local `[patch]`, never commit `Cargo.lock` while it is on.
>
> **First: finish P4 (docs/64 §4.1).** About 24 `== ProviderId::Muse` branches remain in `app/lifecycle.rs` (plus a
> few in `app.rs`, `app/titles.rs`, `right_changes.rs`) on the open/reopen paths, Muse start parameters, the boot
> catalog warm-up and the turn handlers whose Muse rows follow the hub's connection stream. The owner's decision:
> **[OWNER: fill in — A: give Muse sessions Baaz records like Claude Code / Codex so open, reopen and rows are one
> path; or B: name each remaining difference as a `ProviderId` registry property and keep the branches]**. Write a
> short plan doc (docs/65) first; show it to me only if it changes a docs/50 decision. Build it step by step, one
> commit each, with the same gates as P4: workspace tests, clippy, `python3 scripts/perf-budgets.py --binary
> target/release/baaz`, the 18-entry probe in CLAUDE.md (plus `right-files-scrolled`), and a look at BaazCheck
> (fresh state) for every step. Add a ratchet that counts `ProviderId::Muse` outside `providers.rs`, `login.rs`,
> `tier.rs`, `billing.rs` and Settings content, and drive it to the agreed number.
>
> **Then:** the next item on the tracker's plan (Settings D5 beside the agent core R2 is the default; ask me if the
> tracker says otherwise).
>
> Rules: tests never reach a real CLI or server. A Muse turn costs money on this login (pay-as-you-go): spend one
> only where a step cannot be seen any other way, say so first, keep it to one short turn — and script held sends
> with `--tier payg` (a cached pay-as-you-go answer older than 10 minutes re-verifies and does not block, which is
> how four turns were spent by accident). Check `HIDIdleTime` before windowed runs if I might be at the machine; never
> capture the full screen; prune `target/*/incremental` before long build chains (disk had 15 GB free). When the
> screen is unlocked you may drive BaazCheck with computer use (bundle id `sh.baaz.check`, granted while it runs; use
> full-screen control — background window captures miss popovers). Don't push until I say "push"; library changes ship
> as an agentic-ui tag plus a repin. Design questions the specs don't answer come back to me. At the end: update the
> tracker (merge into the live version, add a short checklist), memory, a handoff doc, and report per step.
