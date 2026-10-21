# 63 — P4: Muse onto the provider seam (plan, 2026-10-21)

Status: **built** (all eight steps, 2026-10-22; §6). Implements docs/50 §4 P4 as decided there (§6 decisions unchanged). Nothing here changes a
docs/50 decision; it only fixes the order. Evidence: two read-only inventories of the tree at `b2fc333` plus reading
`session/lane.rs`, `session/commands.rs`, `session/events.rs`, `app.rs` connect/pump/route, `provider*/src`. Line
numbers are as of `b2fc333`.

## 1. Where Muse runs today

**One child, two consumers, one of them unused.** `conn::connect` (`conn.rs:120`) spawns `muse serve` once and hands
back a gated `Provider` (the `MuseAdapter`, `provider-muse/src/lib.rs:47`) **and** `conn::Legacy` (`conn.rs:64`: the
raw `Arc<MuseClient>` plus the raw `MuseEvent` stream). `Harness.provider` is written (`app.rs:2176, 2548`) but never
read; `observe_provider` (`app.rs:2250`) only logs `ConnectionLost`. Every Muse byte goes through `Harness.client`
(`app.rs:651`) and the raw pump.

| Path | Where | What it does |
|---|---|---|
| Event pump | `app.rs:2262` `pump`, `app.rs:2277` `route` | Every `MuseEvent` → account (`login::route_account`), `usage/changed` (`billing.rs:144`), row facts (`turn/started`, `turn/completed`, `session/statusChanged`, approval/userInput), side-session harvest (titles, bylines, drafts), then `active.apply(event)` — **the open view only**; parked Muse views get nothing |
| Reconnect | `app.rs:2523` `reconnect`, `:2578` `resume_after_reconnect` | `Closed` → respawn → re-seat the active view's client → `session/resume` from its cursor |
| View fold | `session/events.rs:14` `SessionView::apply` | Refuses on a provider lane; folds through the view's own `MuseFold::apply`; special-cases `turn/started`, `turn/completed` (running, `plan_completed`, `turn_failure` → SignedOut), `turn/unqueued` (restored prompt routing), cursor → usage ledger, created files |
| Lane fork | `session/lane.rs:27` `enum Lane { Muse, Provider, Opening }`; 43 `is_provider_lane()` sites; `providers::uses_legacy_pump` (`providers.rs:198`) at 8 more | Muse arm calls the raw client; provider arm sends a `Command` |
| Direct MSP calls in views | `commands.rs:186` turn/start, `:476` steer, `:561` interrupt, `:653` unqueue, `:731` setModel, `:1569`+`:1580` mode + profile resume, `:1149` compact, `:1226` model/list; `events.rs:326` view/page; `clocks.rs:82` listPending; `approvals.rs:62` decide; `questions.rs:192/268/334` answer/cancel/clarify; `shell.rs:85` userShell, `:158` fork; `handoff.rs:337` turn/start; `full_output.rs:62` item/readOutput | Typed MSP results and `MuseError::kind()` consumed in place |
| SideState reads | `session.rs:1424` cursor, `:1757` context, `:1541` model; `render.rs:1745,1949` queue; `clocks.rs:20,124` retry + countdowns; `approvals.rs:31` requirement id; `questions.rs:40,64,92` wizard | MSP-typed (`ContextUsage`, `ApprovalRequestParams`, …) |
| Opening | `lifecycle.rs:1745` `new_session_open`, `:2108` `new_session_in_root_open` (session/start), `:2581` `resume_inner` + `conn::resume_muse`, `:4721` `top_up`, `:5027` Forked → `seed_session` | Muse refused by `providers::open_provider` (`providers.rs:367`) and `reopen_provider` (`lifecycle.rs:3772`) |
| Harness-level MSP | `login.rs:151,318,383,429,453` account/*; `billing.rs:65,174` + `tier.rs:1130` usage/read; `app/titles.rs:152…1174` side sessions (model/list, session/start, turn/start, session/read); `app/list.rs:857` derive_titles session/read; `lifecycle.rs:585` session/list via `list_cache` | Raw client, MSP types |
| Rows | `sidebar.rs:256` `SessionEntry::join_cached` from MSP `Session`; `entry.provider: None` = Muse; `AttentionFlag` is a wire type on the row | The list is two stores read as one: Muse's listing + `provider-sessions.json` |
| Replay / bench | `session.rs:2190` `load_replay`, `:2620` `parse_replay_file`; `app.rs:4969`; `bench.rs:593` | Captures fold as raw `MuseEvent`s into the view |

Files naming `muse_client` (ratchet, `tests/seam_ratchet.rs`, CEILING 20): app.rs, app/lifecycle.rs, app/list.rs,
app/titles.rs, attachments.rs, auth.rs, bench.rs, conn.rs, full_output.rs, images.rs, login.rs, main.rs,
projects.rs, session.rs, session/commands.rs, session/events.rs, sidebar.rs, sidebar_view.rs, tier.rs, titles.rs.

## 2. Target (docs/50 §3)

- `provider-muse` owns the one `muse serve` child as a **hub**: it demuxes the child's events per session into one
  lane per open session (a `Provider` like Claude Code's and Codex's), and a connection stream for what belongs to no
  open view (account, usage, status of other rows, side sessions, connection loss). Reconnect-and-resume of attached
  lanes happens inside the hub.
- Every Muse-only reaction that today reads an MSP error kind or ack field (approval already resolved, stale stage,
  permission-profile moves, queued disposition, restored prompts, view/page backfill, stale sidecar retry) moves into
  the adapter, which answers with neutral events, acks and typed refusal hints.
- A neutral **`session-store`** crate holds what a view renders (session, side facts, dirty turns, client-authored
  turns, remembered prompts). `muse-adapter`'s `MuseFold` stays the MSP → delta translator inside `provider-muse`.
- `crates/baaz`: one command path per action, no `Lane::Muse`, no `Harness.client`, no pump/route, no
  `conn::Legacy`; Muse rows in Baaz's own registry beside Claude/Codex records, with `list_cache.rs` as Muse's source;
  no `muse_client` or `muse_adapter` dependency (ratchet CEILING → 0). Muse-only features (queue strip, retry clock,
  wizard questions, full stored output, context window) are offered by capability/fact, not by provider name.
  `login.rs` stays the Muse sign-in sheet; `tier.rs`/`billing.rs` stay behind Muse's own account status (docs/50 §3).

## 3. Steps — each ships alone, app working after each, one commit each

| # | Step | Ships when |
|---|---|---|
| 1 | **Neutral session store.** New crate `session-store`: `SessionStore` (session + neutral `Side` + dirty + client blocks + command text + restored prompt). Views read only the store. The provider lane folds into it; the Muse lane keeps a private `MuseFold` translator and mirrors deltas + header + side into it (`muse_adapter::neutral_side`). | Every `fixtures/msp/*.jsonl` replays to an identical session in store and fold (test); all tests; probe; budgets |
| 2 | **Seam vocabulary.** `ProviderEvent` gains session facts (context, tokens, queue, retry, goal, model, pending questions, cursor), turn settled (with failure / signed-out), prompt restored, session status (rows), account changed / login completed, usage changed, unhandled. `Ack::TurnAccepted.queued`; wider `Ack::Account`, `SessionSummary` (row fields) and `ListSessions.updated_after`; `Command::ReadUsage`; typed refusal hints on `ProviderError`; image size on `SubmissionPart::Image`. MuseAdapter emits/maps all of them; nothing consumes them yet. | provider + provider-muse unit tests; app unchanged |
| 3 | **Muse hub.** `provider_muse::MuseHub`: shared child, per-session lane `Provider`s with their own event channel, connection stream, fold pinned for attached sessions, reconnect + resume of attached lanes, adapter-side reactions (stage token fill, already-resolved resolution, profile moves on mode picks, queued rows, restored prompts, backfill paging on resume, stale-sidecar retry), one-shot `complete()` over a side session, a replay hub for `--replay`. Not wired in yet. | Hub tests against a scripted transport (never a real `muse`) |
| 4 | **Muse views ride lanes.** `providers::open_provider(Muse)` hands out hub lanes; new / resume / fork / top-up / replay of Muse sessions go through the provider open path; views of every provider take one command path. `Harness` still pumps raw events for account / usage / titles / rows only. | Tests; live on BaazCheck: replay captures + Claude Code; **one short live Muse turn** (the demux path cannot be seen otherwise) |
| 5 | **Delete the Muse lane in views.** `Lane::Muse`, `SessionView.client`, `apply(MuseEvent)`, `seed_session`, `reconnected`, `wire_client`, the view's `MuseFold`, every `is_provider_lane()` / `uses_legacy_pump` fork. | Ratchet down; tests; probe |
| 6 | **Harness off the raw transport.** Account/login through `Command`s + events; usage through `ReadUsage` + events; titles, bylines, drafts, handoff summaries through `utility::complete` → the hub; the session list through `ListSessions`; row status through events; one connection-event handler replaces `pump`/`route`/`observe_provider`; `Harness.client` and `conn::Legacy` deleted; reconnect through the hub. | Ratchet down; tests; live on BaazCheck (sign-in sheet offline states, titles via replay where possible) |
| 7 | **One registry.** Muse rows become registry records beside Claude/Codex ones, `list_cache.rs` as Muse's source; `SessionEntry.provider` always set; the `provider.is_none()` = Muse checks go. | Registry tests; sidebar + palette + search budgets; probe |
| 8 | **Last mentions.** Neutral types for images, attachments, projects' start params, main's mode flag, auth, sidebar attention, full output, bench, titles; `crates/baaz` drops `muse-client` + `muse-adapter`; ratchet CEILING 0 + a second ratchet on `is_provider_lane`. | Ratchet 0; tests; budgets; 18-entry probe |

Then: docs/64 handoff, tracker, memory. Baselines are regenerated on main at push (CLAUDE.md), not here.

## 4. Rules for every step (from the brief)

- Reproduce/measure → change → test → see it live on a fresh-state BaazCheck build → one commit.
- Tests never reach a real CLI or server: Muse behaviour is tested against scripted transports and `fixtures/msp`.
- Live Muse turns cost money on this login: only step 4 needs one, said first, one short turn. Everything else uses
  `--replay` fixtures and Claude Code (Max login).
- `scripts/perf-budgets.py --binary target/release/baaz` and the 18-entry `relay_visual.py` command pass after each
  step (idle CPU 0.6–0.8 % visible is the known gpui finding).
- The ratchet only goes down.

## 5. Risks seen up front

- **Fold eviction.** `MuseFold` keeps at most 8 sessions (`SESSION_CACHE_LIMIT`). In a shared hub, title side sessions
  and other rows could evict an open session and its next delta would index into an empty copy. The hub pins every
  attached session and evicts only unattached ones.
- **Double folding during steps 1–4.** The Muse lane folds in the view's translator and again in the adapter (as it
  already does today: docs/50 §1 "folded a second time into deltas nobody reads"). Gone in step 5; watched by the
  reopen/stream budgets meanwhile.
- **Behaviour that differs by lane today and must not change silently:** Muse sends show no optimistic bubble; the
  billing guard holds Muse sends; Muse plan cards come on `turn/completed`; Muse mode picks move a permission
  profile first. Each keeps its current behaviour unless a step says otherwise.
- **Parked Muse views** start receiving their own events once each has a lane (today only the open view does). That
  is the intended fix, but it is new behaviour: checked live in step 4.

## 6. Progress — ALL EIGHT STEPS BUILT (2026-10-21/22), committed on harness main, NOT pushed

| Step | Commit | As built |
|---|---|---|
| 1 Neutral session store | `adf3d28` | `crates/session-store`; views render from it. |
| 2 Seam vocabulary | `19ddb85` | facts / turn settled / prompt restored / status / notices; typed refusals; queued acks. |
| 3 Muse hub | `eaa9c96` | `provider_muse::MuseHub`: one child, a lane per session, fold pinned for bound sessions, orphan buffer, reconnect + resume of followed sessions. |
| 4 Views ride lanes | `ecc3634` | Muse new/resume/fork/replay through the provider open path; **one short live Muse turn** (PONG) proved the demux. |
| 5 Muse lane gone from views | `1daa488` | `Lane::Muse`, the view's `MuseFold`, every `is_provider_lane()` fork in production code. |
| 6a Account + usage | `686529f` | `AccountLane`, `LoginOutcome`, `ReadUsage`/`Ack::Usage`, `AccountChanged`/`LoginCompleted`/`UsageChanged` on the connection stream; tier decides from `UsageReport`. |
| 6b One-shot completions | `75ac929` | `MuseHub::complete` (side session, private lane, settle, read back); titles/bylines/summaries/drafts all through `utility::complete`; the job maps and harvests gone. **One short live side turn** (muse-spark-1.3, PONG, 14.8 s). |
| 6c Off the raw transport | `e1c45ca` | pump, route, `Harness.client`, `conn::Legacy`, `legacy_events`; rows from `TurnRunning`/`TurnSettled`/`StatusChanged`; reconnect on `ConnectionLost` (seen live: SIGKILL of `muse serve` → respawn). |
| 7 One registry | `9684d3b` | Muse rows through `ListSessions` (+`updated_after`) as a widened `SessionSummary`; Baaz-owned `ListedRow` keeps the saved list readable (live: owner's 2,661-row copy → 1 page, `mode=Newer`); derived titles via `ReadSession` → `Ack::SessionRead`; `SessionEntry.provider: ProviderId` always set; neutral `Attention`. |
| 8 Last mentions | `352ac09` | `crates/baaz` drops `muse-client` + `muse-adapter`; `PermissionMode` for modes; Baaz-owned model row; registry properties replace `uses_legacy_pump`; ratchet absolute (no mention, no manifest entry, no lane forks). |

Every step: workspace tests (2,108 at the end, 0 failed), clippy clean, all perf budgets, the 18-entry probe (only the
intended right-files / right-files-reveal changes), and a BaazCheck run (screen locked throughout: logs + the app's own
`--screenshot`).

**Not done — the honest remainder.** `crates/baaz` names no Muse wire, but about 24 app-level branches in
`app/lifecycle.rs` (plus a few in `app.rs`, `app/titles.rs`, `right_changes.rs`) still ask `== ProviderId::Muse`:
the open/reopen paths (a Muse session has no Baaz record; it reopens with `ResumeSession`), Muse's start parameters
(model provider, mode), the boot catalog warm-up, and the two turn handlers whose Muse rows now come from the hub's
connection stream rather than the view's events. Commands go through one path; these open and row paths still
branch by name. Folding them into registry properties (or making Muse keep Baaz records like the others) is the
next piece of P4 — see docs/64 §4.
