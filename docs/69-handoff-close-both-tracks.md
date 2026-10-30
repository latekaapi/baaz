# 69 — Handoff: close both tracks (providers equal + Zed-fast)

Written 30 Oct 2026, at the close of the provider-screens milestone (docs/65–68). Read this first in the next session.

## 0. Where things stand

| Repo | Head | Pushed | Notes |
|---|---|---|---|
| harness (Baaz) | `842c1d9` | **yes** (30 Oct) | every aui pin at **v0.6.3**, `baaz-cef-helper` included; `Cargo.lock` committed and clean |
| agentic-ui | `52cae12`, tag **v0.6.3** | **yes** | |

- **The local `[patch]` is off.** It is parked at `~/.baaz-dev/harness-cargo-config-patch.toml`. To change both repos
  together, copy it back to `.cargo/config.toml` (git-ignored). From then on `Cargo.lock` shows modified: **never
  commit it while the patch is on.** At the next push: tag agentic-ui, set every `aui*` pin (including
  `crates/baaz-cef-helper/Cargo.toml`) to the new tag, park the patch, `cargo update -p aui` (also in the helper's
  own workspace), then gates, then push.
- The milestone just closed: the D5 provider screens, the owner's 29 Oct auth round (docs/68) and the Muse tier race
  (`/upgrade` read pay-as-you-go for about 6 s after launch). A Sonnet agent drove the owner's real app with computer
  use, and everything passed.
- The owner's Baaz is `target/bundle/Baaz.app` and is in use. Rebuilding the bundle is safe (stage + swap); tell the
  owner to quit and reopen.

## 1. Where the two tracks stand (docs/50 §4, docs/51 §3)

| Track | Done | Left (this milestone) |
|---|---|---|
| Providers equal | P1 ungate, P2 utility model, P3 registry + search, P4 Muse on the seam (commands), P5 detection (delivered by docs/65 S1–S6 and D5: probes, cache, version floors, path override, typed states, setup sheet, diagnostics) | **P4 finish** (open/reopen/row paths), **P6 robustness**, **P7 import** |
| Zed-fast | M budgets (21, gating), P0, P1, P2 (benches, frame counter) | **three follow-ups** the benchmarks found |

## 2. Owner decisions for this milestone (30 Oct)

1. **P4 finishes with option B:** Muse sessions get Baaz records like Claude Code and Codex, so open, reopen and row
   updates are one path. Option A (naming each difference in the registry) is rejected.
2. **Scope: close both tracks.** That means P4-B, P6, P7 and the three speed follow-ups. Browser (R1, R3–R5) and
   D7/D8 come after.
3. Still standing from 16 Oct (docs/50 §6): import is **yes**, Muse also gets a **Baaz-private home** like the other
   two, and titles use the session's own provider's cheapest model.

## 3. The work, in order (each step ships alone, gated, one commit or a short series)

### S1 · P4-B: Muse sessions get Baaz records (≈1 week)
docs/64 §4.1 has the inventory: about 24 `== ProviderId::Muse` sites in `app/lifecycle.rs`, plus `app.rs`,
`app/titles.rs` and `right_changes.rs`.
- A Muse session gets a record in the same store as the Claude/Codex records (`provider-sessions.json`, or wherever
  P3 put the registry). The record is created at start and on first sight in Muse's `session/list`, so the 2,661
  existing rows **migrate lazily**. Name the trigger precisely
  ([[feedback-lazy-migrations-need-their-trigger-named]]). The saved list cache stays readable.
- After that, reopen goes through `reopen_provider` for all three (no `ResumeSession` special case). Rows follow one
  event pair, and Muse start parameters and the catalog warm-up become registry properties or go away.
- **Done when:** `tests/seam_ratchet.rs` gains an absolute ratchet with no `ProviderId::Muse` comparisons in the
  open/row paths. A Muse session opens, reopens and updates its row with the same code as Codex. Live: reopen an old
  owner Muse session and one created before the migration.
- Write a short design note first (`docs/70-p4b-muse-records.md`: record shape, migration trigger, what dies). It is
  not an owner gate, but put it on the tracker.

### S2 · P6: robustness for every provider (3–4 days)
- Reconnect-and-resume for every lane. Muse has it through the hub; Claude Code and Codex lanes need it when their
  child dies mid-turn.
- A "Failed to launch" view in the session with stderr excerpt + Retry (typed, from the status/advisory model).
- Approvals settle (never left pending) when a child exits.
- **A conformance suite** every adapter must pass: the same scripted scenarios (start, turn, tool call, approval,
  interrupt, resume, child crash) against recorded fixtures per provider. Tests never reach real CLIs
  ([[feedback-tests-never-reach-real-clis]]).

### S3 · P7: import (2–3 days, plus a UI gate)
- The engine: existing `~/.claude/projects` and `~/.codex/sessions` become resumable Baaz sessions (records + search
  rows), idempotent, dry-run first. The session-migration journaling executor already does a related job; reuse it.
- **The UI needs a mock first** ([[feedback-mock-first-then-muse]], [[feedback-ff-is-the-spec]]): a small board on
  the "Baaz v3" Design canvas (the import offer: where it appears, the count, progress and the result). Get owner
  approval before building the surface. Build the engine meanwhile.
- The Baaz-private Muse home (decision 3), if S1 did not already need it: Muse sessions started by Baaz live under
  Baaz's state, like `claude-home` / `codex-home`.

### S4 · Speed follow-ups (2–3 days), each with before/after numbers and a budget that gates it
1. A new turn costs more the longer the session: 0.2 ms at 300 turns, 1.1 ms at 3,000. Make it flat.
2. A finished Muse turn re-joins all 2,661 sidebar rows (64–144 ms). Update one row. This probably falls out of S1;
   measure again after S1 before doing anything.
3. A very long answer re-parses its markdown at 1.9 ms per chunk. Parse incrementally.

### S5 · Close-out
Owner checklist on the tracker, then "push", then a final tracker state that says both tracks are closed, then the
next milestone's handoff (Browser R1 → R3–R5, or D7/D8, owner's pick).

## 4. Gates (every step)

```bash
cargo test --workspace && cargo clippy --workspace --all-targets
# agentic-ui too when it changed: cargo test --workspace; cargo test -p aui-terminal --features pty,tui
cargo build -p baaz            # the probe runs the binary
python3 ~/.claude/skills/relay/scripts/relay_visual.py --repo . <every entry in CLAUDE.md, both blocks>
scripts/bundle.sh && python3 scripts/perf-budgets.py --binary target/release/baaz
```

Tier V exit 0 = clean; a finding is a real change until looked at. Regenerate baselines on main only, deliberately,
in their own commit. **Then drive it live:** BaazCheck or the owner's app, with a cheap computer-use agent (Sonnet)
on a short scripted list. Check the screen lock first. Use throwaway homes for anything that signs in or out: the
recipes are in docs/68 §4 (`CLAUDE_CONFIG_DIR`, `CODEX_HOME` + `codex login --with-api-key`, `XDG_CONFIG_HOME`, a
fake `open`, `BAAZ_CODEX` sleep wrapper). New `--steps` verbs from the last round: `provider-action:`,
`signout-confirm`, `crumb:`.

## 5. Rules that bit last time

- Before diagnosing an owner screenshot, check which build is running (`ps -o lstart` vs the binary's mtime).
- **No Muse turns** unless one is announced and needed. The login is on a subscription (High Usage), but turns
  still count; captures use `--no-connect`/`--replay`.
- Never the owner's real sign-in/out in a test. A computer-use agent on the real app gets hard rules: no Sign out,
  no Return in a terminal, no sends.
- Prune `target/*/incremental` before release chains (17 GB free at close).
- Don't push until the owner says "push".
- At the end: tracker (read the live version, merge, add a short checklist), memory, a handoff doc, and a report
  per step.

## 6. Prompt for the next session

> Drive the "close both tracks" milestone to completion. Read `docs/69-handoff-close-both-tracks.md` first (state,
> my decisions, the order S1–S5, gates, rules), then docs/64 §4 (the P4 remainder's inventory), docs/50 §4–§7 and
> docs/51 §3–§9, and the live Baaz Tracker (https://claude.ai/artifact/Fr965AKBn8toZ7BcUk7wCP). Both repos are
> pushed: harness `842c1d9`, agentic-ui v0.6.3. Restore the local patch from `~/.baaz-dev/` only if the library
> must change, and never commit `Cargo.lock` while it is on.
>
> Decided: P4 finishes with **option B** (Muse sessions get Baaz records, so open, reopen and rows are one path),
> then P6 robustness with a conformance suite for every adapter, then P7 import of my existing Claude Code and
> Codex sessions, then the three speed follow-ups with before/after numbers and gating budgets. Work on main, one
> commit per step, docs/69 §4's gates every step, and drive each step live (a cheap computer-use agent on BaazCheck
> or my app, throwaway logins for anything that signs in or out, no Muse turns unless announced). P7's import offer
> is new UI: put a small board on the "Baaz v3" canvas and get my approval before building it. Anything the docs
> don't answer comes back to me. Don't push until I say "push". At the end: update the tracker (merge into the live
> version, short checklist), memory, a handoff doc, and report per step.
