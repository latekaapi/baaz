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

## 6. As built (30 Oct 2026)

Built on `main` as §2–§4 say, with the deviations listed below.

**Seam (`crates/provider`).** `ProviderAdapter::stderr_tail()` (default empty) and `Provider::stderr_tail()`:
a peek at the child's last ≤ `STDERR_TAIL_LINES` (12) stderr lines, never the wire. `provider::StderrTail`
is the one ring both CLI adapters drain every child into (one per adapter, reset per spawn, so a child that
dies on its first breath still leaves its words). `RefusalHint::{NotInstalled, TooOld}` plus
`ProviderError::launch_refusal(hint, title, reason)`: the factory's missing binary, a spawn's `ENOENT`, and both
adapters' version-floor refusals are now typed (they were `Unavailable`/`Rejected` text). `ConnectionLost`'s doc
states the contract (turn closed and approvals/questions settled before it, nothing pending after it, the
session kept), and `FollowSession` is "Retry now" on a lost child.

**1 · Reconnect-and-resume.** Every lane now drains past a loss; the registry's `lane_survives_lost_child` is
gone (it would be `true` for all three). Codex (`provider-codex`): the pump settles the dead child
(`settle_lost_child`: answerable approvals and questions dropped, `CodexFold::abandon_live_turn` refuses every
carded approval and closes the live turn with a `codex-child-exited` error card, then `ConnectionLost` naming the
last stderr line). A shutdown retires the child first, so a hang-up reports nothing. The next `SubmitInput`, or
`FollowSession`, revives: a fresh app-server, the handshake, `thread/resume` of the held thread (no history
re-fold — the view holds it), then `ConnectionRestored`. A revive whose spawn fails leaves `revive_owed`, so the
next try revives again. Claude Code keeps its `--resume`, now also on `FollowSession`, and says
`ConnectionRestored` after either. The view: no hang-up and no `Closed` on the first loss; the quiet banner
"<Label> stopped unexpectedly. It will pick up where it left off on your next message." with **Retry now**
(`BannerAction::RetryNow`, Info kind); `ConnectionRestored` takes it down. A second loss — or a failed revive —
within `LOST_TWICE_WINDOW` (30 s) with no live turn finished in between emits `SessionEvent::ChildFailed`
(and `Closed`, which fails a handoff destination as before); the window parks the view with its lane and lands
the failure view (`fail_lost_lane`), whose Retry (`ProviderOpenRetry::Revive`) brings the same view back and
sends `FollowSession`. Muse is unchanged: the hub's respawn, the window's banner (`shares_one_child`).

**2 · Approvals settle in the adapter.** Codex as above; Claude Code's `abandon_live_turns` now also clears its
pending queues when no turn is open. A late decision for a dead child's approval is refused
`unknown_approval`; `ListPending` lists nothing. The view clears its taps and emits `ProviderApprovalsChanged`,
so the row, header and badge re-read.

**3 · The typed failure view.** `ProviderOpenError` → `app::LaunchFailure { session_id, provider, kind,
message, stderr_excerpt, retry }`; `provider_status::FailureKind { NotInstalled, SignedOut, TooOld, Crashed,
Refused(hint) }` (classified from the refusal's hint, then the latest status, else Crashed), with one sentence
and an implied `FailureAction { Install, SignIn, Update }` reusing D5 (`connect_provider_action` for
Install/Update, `banner_sign_in` for Sign in). The open path carries the child's stderr out with the refusal
(`app::OpenFailed`). Render: title, the kind's sentence (a twice-dead lane says so), the failure, a mono well
(`Role::Log`, labelled) of ≤ 12 lines, Retry · the kind's action · Providers…, each wrapper with a role and label.
New `--steps` verbs `launch-failed:<provider>[:<kind>]` and `child-lost`; new uiprobe entries
`launch-failure`, `launch-failure-signed-out`, `child-lost-banner`, `child-lost-twice` (no baselines yet).

**4 · Failed rows for Claude Code and Codex.** Both adapters settle a failed turn as `TurnSettled { Failed
{ message } }` after its finish: Claude Code from an error `result` (`frame::result_failure`: `is_error` or an
`error_*` subtype, its own words), Codex from `turn/completed` status `failed` (its `turn.error.message`). The
view turns every `TurnSettled` into `SessionEvent::ProviderTurnOutcome`, and a live finish on a provider that
does not report turn lifecycle (`reports_turn_lifecycle`, Claude Code/Codex) into a success outcome first; the
app calls `record_turn_outcome` (idempotent, so Muse's hub path recording too is harmless).

**5 · Conformance suite.** `crates/baaz/tests/conformance.rs`, 9 tests over all three adapters on fake children
(baaz already depends on every adapter, so no new crate and no lock change). Claude Code and Codex spawn
`crates/baaz/tests/fixtures/conformance/fake_agent.py`, which replays a fixture to whatever the adapter asks and
can die on cue with stderr; Muse spawns the provider-muse puppet through a real `MuseHub::connect` (so the hub's
respawn is exercised), with a new `--turn-shape=approval|failed|open|crash`. Scenarios: open + text turn; tool
call; approval allow/deny asked once, settled exactly once (a second decision refused typed); interrupt (ends the
turn, or a typed refusal on a non-Native turn control); resume; error turn; child crash mid-turn (turn closed and
card settled before the loss, nothing pending after, late decision refused, stderr kept, revive and next turn
runs); a child that cannot start (open fails, stderr kept); absent capability refused typed.

Fixtures: recorded ones are used as they are (`fixtures/claude-code/{basic,read-search,permission,
permission-deny,stored-history}.jsonl`, `fixtures/codex/{basic,read-search,approval,interrupt,resume}.jsonl`).
**Hand-written, never on a wire** (the rule was no recording and no turn): `crates/baaz/tests/fixtures/conformance/
claude-failed-turn.jsonl` (basic with an error `result`), `codex-failed-turn.jsonl` (basic with a `failed`
`turn/completed`), and the puppet's four turn shapes. The scripted crash is the fake's `#crash-at-request` /
the puppet's `crash` shape, not a fixture.

**Deviations, and why.**
1. *Cancelled* is `Denied` on the card: `aui_protocol::ApprovalState` has no cancelled state and agentic-ui was
   out of scope. Claude Code already settled this way.
2. No new `ApprovalSettled` event: the adapter's card delta is the settlement, and the view's
   `ProviderApprovalsChanged` on the loss makes every surface re-read.
3. Muse's approvals are not cancelled on a loss: they live in its durable session, not in the child, and the hub
   re-reads them on re-attach. While the child is down its lane answers `ListPending`/`DecideApproval`
   `Unavailable` (typed), which the suite asserts. The twice-in-30-s rule does not apply to Muse lanes (the
   window's reconnect owns the shared child).
4. Claude Code's interrupt has no fixture and gets none (no recording): its adapter refuses it typed (turn control
   is Unverified, never probed), and the suite asserts that refusal.
5. Muse runs on the puppet, not an msp replay: the msp transcripts predate 1.2's shapes and the puppet already
   drives the hub in provider-muse's own tests. The tool-call scenario does not run on Muse (the puppet has no
   tool shape; muse-adapter's fold tests cover the msp tool shapes). Muse's resume is metadata-only
   (`pages_history`), so its contract is the ack on the same id and a working turn after it.
6. A Claude Code child that dies at once does not fail the open (the open returns before the child speaks): it
   reads as a loss, and a revive that dies again lands the failure view.
7. A reopened Claude Code / Codex session replays its history as finished turns, which stands a row's "Failed"
   down (history carries no failure). Muse is unaffected (`reports_turn_lifecycle`).
8. The library banner's action takes no role, so the lost banner is wrapped in a `Role::Status` group labelled
   with its message and "Retry now".
9. Captures write the real store. The first `child-lost-*` probe runs (before they had their own state dir)
   wrote `provider.json` = codex and a "hello lost probe" history/row entry into
   `~/Library/Application Support/baaz`; reverted by hand (`provider.json` back to muse — what the 29 Oct
   baselines show — and the two entries removed). The P6 entries now carry a throwaway `BAAZ_STATE_DIR`; older
   entries that pick a provider or send (`composer-*`, `lane-*`, `reopen-failed`) still write the real store.

Tests added: `conformance.rs` (9); lane `a_lost_lane_stays_open_and_a_second_loss_hands_over_the_failure`,
`a_finished_turn_between_losses_restarts_the_count`; app `a_lane_lost_twice_lands_the_failure_view_and_retry_brings_it_back`,
`a_lane_turn_outcome_moves_the_rows_failed_state`, `a_missing_cli_fails_the_open_typed_not_installed`,
`a_launch_failure_is_typed_from_its_hint_then_the_status`; codex fold `a_dead_child_settles_its_cards_and_closes_its_turn`,
`a_failed_turn_settles_failed_with_its_words`; claude frame `an_error_result_reads_as_a_failure_in_its_own_words`;
provider `stderr_tail` (2). Changed: three lane tests that pinned the old hang-up (`nothing_surfaces_after_…`
became the stays-open test; two read the new banner).

**Superseded in the 30 Oct fix round (harness 550939a, aui a93d58d):** a lane lost twice no longer parks the view for
the full-centre failure view. The session stays with its transcript and its lost banner grows in place (`Banner::body`):
"<Provider> stopped twice in a row.", the kind's words, the stderr well, Retry and the kind's action. The full-centre
view is only for a session that never opened. The raw error line shows only when it adds something
(`provider_status::failure_detail`).
