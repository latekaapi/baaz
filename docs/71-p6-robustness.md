# 71 — P6: robustness for every provider

30 Oct 2026. docs/50 §4 P6, docs/69 §3 S2. Built after S1 (docs/70), so Muse is already on the record/reopen path.

## 1. Where each lane stands (read from the code, 30 Oct)

| | Child dies mid-turn | Open fails | Approvals on exit |
|---|---|---|---|
| Claude Code | revives: the adapter `--resume`s on the next send, events continue on the same stream | inline `ProviderOpenError` + Retry (no stderr) | the view clears its cards on `ConnectionLost`; the adapter's pending permission requests are dropped with the child |
| Codex | **closes the session** (`SessionEvent::Closed`) with a "connection lost" banner; nothing brings it back | same inline error | view clears; the app-server's pending requests die with it |
| Muse | the hub respawns `muse serve` and re-attaches every lane (P4) | same | view clears; the hub re-reads pending on reattach |

So the gaps are: Codex reconnect-and-resume, a typed failure surface with the child's own words, approvals settled
on the adapter side (not only hidden in the view), and nothing that holds all three to the same behaviour.

## 2. Reconnect-and-resume, one rule

`providers::revives_lost_child(id)` (S1 may already have named it) becomes true for all three. On
`ConnectionLost` mid-turn the lane:
1. settles the turn as **Interrupted** (`TurnSettled { outcome: Interrupted }` semantics in the view: no Working,
   optimistic bubbles dropped, the partial answer kept) — already done in the view, keep it;
2. settles every pending approval and question **in the adapter** as cancelled, so a late `decide` cannot reach a
   new child with a stale request id, and emits `ApprovalSettled`/equivalent so every surface (row, header, badge)
   stands down — not just the view's lists;
3. shows the quiet banner "Codex stopped unexpectedly. It will pick up where it left off on your next message."
   (provider label from the registry) with **Retry now**, and keeps the session open;
4. respawns on the next send or on Retry: Codex starts a fresh app-server and `thread/resume`s the thread (the
   resume path `reopen_provider` already uses), then sends. Muse keeps the hub's path; Claude Code keeps its own.

A child that dies **twice within 30 s** without completing a turn stops retrying on its own and turns the banner
into the failure view (§3) — no restart loop.

## 3. "Couldn't start" view, typed

`ProviderOpenError` grows into one typed value used for both a failed open and a twice-dead child:
`LaunchFailure { provider, kind, message, stderr_excerpt, retry }`, `kind` from the status/advisory model already in
`provider_status` (NotInstalled, SignedOut, TooOld, Crashed, Refused(hint)). The view is the existing inline error
in the session's centre with: the title "Couldn't start Codex", one sentence from `kind`, the last ≤ 12 lines of the
child's stderr in a mono well (Claude: `child.rs` stderr tail; Codex: add the same ring buffer; Muse: `hub.stderr_tail`),
and the action the kind implies — **Retry** always, plus **Sign in** / **Update** / **Open Settings › Providers**
when the kind says so (each reusing the D5 flows). No new layout concept: it is the inline error that exists, with
the stderr well and typed actions. Accessibility roles and labels on every new control.

## 4. The conformance suite

`crates/provider/tests/conformance/` (or a `provider-conformance` dev crate if the adapters cannot be reached from
`provider`'s tests): one scenario list, run against each adapter on recorded fixtures — never a real CLI:

| Scenario | Claude Code fixture | Codex fixture | Muse fixture |
|---|---|---|---|
| open + first turn, text answer | `basic.jsonl` | `basic.jsonl` | an msp replay |
| tool call (read/edit) | `edit.jsonl` | `edit.jsonl` | 〃 |
| approval asked → allow / deny | `permission*.jsonl` | `approval*.jsonl` | 〃 |
| interrupt mid-turn | record one | `interrupt.jsonl` | 〃 |
| resume a stored session (history replays) | `resume-replay.jsonl` | rollout fixture | `ResumeSession` replay |
| child crash mid-turn → settled turn, settled approvals, revive on next send | scripted kill of the fake child | 〃 | 〃 |
| error turn | `error.jsonl` | `error.jsonl` | 〃 |

Each scenario asserts the **seam's** contract, not the adapter's internals: the ordered `ProviderEvent`s a view
depends on (TurnRunning → Deltas → TurnSettled with the right outcome; ApprovalRequested → settled exactly once;
ConnectionLost → nothing pending), and that the same `Command`s are accepted. Where an adapter cannot do a scenario
it says so through its capability set, and the suite asserts the refusal is typed. Fixtures that do not exist are
recorded once from the real CLIs by hand **outside the test run** (no turn is spent by `cargo test`), committed
under `fixtures/<provider>/`, and named in this note's §6.

## 5. Done when

Kill Codex's app-server mid-turn in a throwaway home (`CODEX_HOME` + `codex login --with-api-key` per docs/68 §4, or
the `BAAZ_CODEX` sleep wrapper so no turn is spent): the session stays, the banner says so, the next send resumes
the thread. Kill it twice fast: the typed failure view with stderr and Retry. A pending approval at the kill
settles everywhere. The conformance suite runs in `cargo test --workspace` against all three adapters. Gates as
docs/69 §4.

## 6. As built

(filled in by the build)
