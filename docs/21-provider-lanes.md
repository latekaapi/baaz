# 21 — Provider lanes: how a Claude Code / Codex session actually runs in Baaz

Design doc (W0). Read `docs/20-provider-wiring-gap.md` first: the picker is
presentation, the adapters exist but are never constructed in `crates/baaz`.
This doc is the build plan that removes the danger named there — "two state
machines, one event stream" — by giving every session exactly one owning lane
for its whole life.

All anchors verified by code reading on 2026-09-26 (no model turn, no build,
no run — see §6 for what that leaves unproven).

## 1. Path map as it is today

New session → `session/start` → views → send → events → fold → render, and
where the chosen provider is dropped.

| step | where | anchor |
|---|---|---|
| `new` step / ⌘N | `Harness::step_new` → `new_session` → `new_session_in` | `crates/baaz/src/app/lifecycle.rs:923`, `:1091`, `:1105` |
| `session/start` issued | `new_session_in` builds `start_params` from `self.new_provider`, calls `client.session_start(&params)` via `wire_call_in` | `crates/baaz/src/app/lifecycle.rs:1146-1190` |
| no-project sibling | `new_session_in_root` — same wire call, default workspace | `crates/baaz/src/app/lifecycle.rs:1274` |
| view opens | `Harness::open(session_id, …)` constructs `SessionView`, clears `session_switch_pending` | `crates/baaz/src/app/lifecycle.rs:1677`, `:1732` |
| result seeded | `SessionView::seed_session` folds the start result as `session/started` | `crates/baaz/src/session.rs:1114` |
| send path | composer intents → `MuseClient` methods (`turn/start`, `turn/steer`, …) on `view.client: Option<Arc<MuseClient>>` | `crates/baaz/src/session.rs:424` (`client` field) |
| event pump (legacy) | `Harness::pump` drains `UnboundedReceiver<MuseEvent>` on a gpui task | `crates/baaz/src/app.rs:1224` |
| event route | `Harness::route` folds account/usage/turn/sidebar outcomes, hands the rest to the owning view | `crates/baaz/src/app.rs:1239` |
| fold | `MuseFold::apply(MuseEvent) -> Vec<Delta>`, `session()`, `append_client_block`, `resolve_approval` | `crates/muse-adapter/src/fold.rs:212`, `:234`, `:308`, `:337` |
| render | view renders a pure function of `fold.session(session_id)` + `SideState` | `crates/baaz/src/session.rs:3-12` (module doc) |
| provider observer (log only) | `Harness::observe_provider` drains `ProviderEvent`s and only logs `ConnectionLost` | `crates/baaz/src/app.rs:1212` |
| connection | `conn::connect` → `provider_muse::establish` → `Provider::new(adapter)` + `gate()` bridge; `Legacy` bundle keeps the same child's raw `MuseEvent` stream | `crates/baaz/src/conn.rs:109`, `:138`, `:148`, `:52-76` |

Where the chosen provider is dropped — three places, all presentation:

1. `SessionView::pick_provider` (`crates/baaz/src/session/composer.rs:308`)
   emits `SessionEvent::SwitchProvider` (fresh session) or
   `SessionEvent::NewSessionOnProvider` (session with turns). Both arms in
   `crates/baaz/src/app/lifecycle.rs:2050-2070` call `select_new_provider`
   (remembers the default) and then `this.new_session(...)` — which issues
   `session/start` **to muse again**. The pick never reaches an adapter.
2. `projects::start_params(…, &self.new_provider, …)`
   (`crates/baaz/src/app/lifecycle.rs:1151`) carries the provider only as a
   label/default, not as a lane choice.
3. `crates/baaz/src/providers.rs:127-199` (`capability_state`/`gate`) is a
   hand-copied mirror of each provider's `caps.rs` ("never re-probes"); the
   composer reads it to disable controls (`steer_gate`, `turn_gate`,
   `questions_gate` at `crates/baaz/src/session.rs:895-907`).

Groundwork already in the view for the lane (left by a previous task):

- `provider_id: String` + `provider_kind() -> ProviderId`, "no setter …
  a session is served by exactly one lane" (`crates/baaz/src/session.rs:876`).
- `external_approvals: ExternalApprovalStore`,
  `external_outbox: Vec<ProviderCommand>` + `take_external_outbox()` marked
  "No production caller yet" (`crates/baaz/src/session.rs:437`, `:916`).
- `pending_model: Option<String>` — muse lane never sets it; the provider
  lane's chip reads it (`crates/baaz/src/session.rs:629`, `:925`).
- `codex_efforts`, `effort`, model/effort menus already provider-aware
  (`crates/baaz/src/session/composer.rs:221-305`).

## 2. The lane model

### 2.1 Adapter lifetime: one `Provider` per session, owned by the view

- Non-muse sessions construct **one adapter per session, owned by its
  `SessionView`** (not one per provider, not one global child):
  `ClaudeCodeAdapter::new(…)` (`crates/provider-claude-code/src/lib.rs:337`
  `impl ProviderAdapter`) / `CodexAdapter::new(…)`
  (`crates/provider-codex/src/lib.rs:361`) wrapped immediately in
  `provider::Provider::new` (`crates/provider/src/traits.rs:129`) so every
  command passes the enforced gate (`Provider::send`,
  `crates/provider/src/traits.rs:159` — an inherent method, unshadowable).
- Rationale: both adapters are child processes with per-session identity.
  Claude Code is "one long-lived child, not one per turn"
  (`docs/18-claude-code.md` §1); Codex resume is per-thread
  (`docs/19-codex.md` §1-2). A shared child would multiplex sessions over
  one stdin/stdout pair with no demux key — the adapters expose no session
  handle on `events()` (`crates/provider/src/traits.rs:109`: competing
  consumers, exactly one consumer per stream).
- Construction site: `Harness::open_lane(provider, …)` (new, next to
  `Harness::open` at `crates/baaz/src/app/lifecycle.rs:1677`): spawn on the background executor
  (commands block up to 3 min per `crates/baaz/src/conn.rs:17-23`), then
  `Provider::connect(&connect_info())`, then `send(OpenSession{…})`, then
  hand the `Provider` + bridged event channel to `SessionView::open_lane`.
- Shutdown: `Provider::shutdown` (`crates/provider/src/traits.rs`) on view
  drop / session close / `ConnectionLost` → reconnect-then-`ResumeSession`
  (same shape as `SessionView::reconnected`, `crates/baaz/src/session.rs`
  `reconnected`). Idempotent; also runs on drop.

### 2.2 Event path: `ProviderEvent` → `Delta` → the same `Session` model

The view's read path stays one `Session` model (`aui_protocol::Session`).
`ProviderEvent::Deltas { session_id, deltas }`
(`crates/provider/src/event.rs`) carries **render-ready
`aui_protocol::Delta`s** — the adapters already fold wire frames into deltas
(`ClaudeCodeAdapter`: `crates/provider-claude-code/src/fold.rs:198`
`apply(&Frame) -> Vec<Delta>`; Codex: `crates/provider-codex/src/fold.rs`).
So the view needs no second fold type:

- Lane task (mirror of `Harness::pump`, `crates/baaz/src/app.rs:1224`):
  per-view gpui task draining the bridged `UnboundedReceiver<ProviderEvent>`
  (bridge via `conn::gate`-style forwarding thread; the crossbeam receiver is
  competing-consumers, exactly one consumer — `crates/provider/src/traits.rs:109`).
- `Deltas` → `session.apply(delta)` on the view's existing `Session`
  (`Session::new` + `apply`, as proven in the `connect_with` tripwire test,
  `crates/baaz/src/conn.rs` `the_connection_path_runs_on_a_non_muse_provider`).
  Muse views keep `MuseFold::apply(MuseEvent)`; lane views apply deltas
  directly. Both write the same `Session` struct — one writer per view (§3).
- `ApprovalRequested { session_id, approval_id, headline }` →
  `external_approvals` store (`crates/baaz/src/session.rs:437`); the full card
  already arrived as a delta. `QuestionRaised { … question_id, headline }` →
  same surface for questions. `ConnectionLost { reason }` → reconnect flow.
- `ListPending` (`Command::ListPending`) closes the missed-approval hole on
  (re)connect, mirroring the muse lane's approval pull dual noted on
  `SessionView::reconnected`.

### 2.3 Action routing: every user action through `Provider::send`

`required_capability` mapping is exhaustive in
`crates/provider/src/capability.rs:228-269`. `Unavailable` refuses in
`Provider::send` before adapter code runs; `Unverified`/`Emulated` proceed
(attempted, never refused). UI rule from `crates/baaz/src/providers.rs:197`:
`Unavailable` → control not offered (disabled with reason via
`crate::providers::gate`, as `steer_gate`/`turn_gate`/`questions_gate` do
today); `Unverified` → attempted everywhere, failure surfaced as a normal
error.

| user action | `provider::Command` | `Unavailable` UI |
|---|---|---|
| submit / queue | `SubmitInput { request_id, session_id, parts, display_text }` (`SubmissionPart::Text/Image`, `crates/provider/src/command.rs:17-30`) | n/a (all lanes `Native`) |
| steer running turn | `SteerInput { …, expected_turn, parts }` — turn-change race guard | disabled with reason (Claude Code: `SteerTurn Unverified` — attempted, not refused; `crates/provider-claude-code/src/caps.rs:51`) |
| stop button | `InterruptTurn { turn, retract }` | disabled with reason |
| non-urgent cancel | `CancelTurn { turn }` | same gate as stop (`TurnControl`) |
| reclaim queued row | `ReclaimQueued { turn }` — refused if already launched | same gate |
| approval decide | `DecideApproval { approval, choice, stage_token, feedback }` — `stage_token` is the stale-stage race guard; drain `take_external_outbox()` oldest-first (`crates/baaz/src/session.rs:916`) | approvals `Native` on both new lanes; no disabled state |
| question answer | `AnswerQuestion { question, answers: Vec<QuestionAnswer> }` | Claude Code `Questions Unavailable` (`crates/provider-claude-code/src/caps.rs:64`): question UI hidden, Coleman prose renders as text; Codex `Unverified` (`crates/baaz/src/providers.rs:184`): attempted |
| question dismiss / clarify | `DismissQuestion` / `ClarifyQuestion` | same as answer |
| model select | `SelectModel { session_id, model, model_provider }` + record `pending_model` until echo (`crates/baaz/src/session.rs:925`); catalog from `ListModels { session }` | catalog missing → `MODEL_UNAVAILABLE_ROW` stand-in (existing) |
| effort | client-side only (`effort` field; "rides the next turn"); Codex levels from folded catalog (`codex_effort_options`, `crates/baaz/src/session/composer.rs:241`); Claude Code unavailable with typed reason | menu shows reason, never empty |
| approval mode | `SelectApprovalMode { mode }` (forward-only; in-flight approval unaffected — `crates/provider/src/command.rs`) | n/a (`SessionConfig Native`) |
| `!` shell | `RunShell { command }` | Claude Code `Native`; Codex `Unverified` (`crates/baaz/src/providers.rs:178`): attempted, errors surfaced |
| attachments/images | `SubmissionPart::Image { base64_data, media_type }`; `@` file mentions stay inline text (no neutral spelling — `crates/provider/src/command.rs:17-22`); unattached-file extraction stays client-side as today | n/a |
| fork | `ForkSession { session_id, through_turn, metadata_only }` — Claude Code `Native` (`fixtures/claude-code/fork.jsonl`); Codex `Unverified` (`crates/baaz/src/providers.rs:175`): attempted, refusal surfaced | disabled only if a lane ever declares `Unavailable` |
| compact | `CompactSession { session_id, through_turn }` — Claude Code `Emulated` (`crates/provider-claude-code/src/caps.rs:41`); Codex `Unverified` | same rule |
| open / resume / read / list | `OpenSession` / `ResumeSession { cursor, metadata_only }` / `ReadSession` / `ListSessions` (§4) | n/a (`SessionLifecycle Native` on both) |
| transcript page/follow | `PageTranscript` / `FollowSession` / `UnfollowSession` / `ReadStoredOutput` | Codex `Transcript Unverified`: attempted |
| `/` commands, skills | client-side (`run_command_with`, `crates/baaz/src/session/composer.rs:340`); no neutral spelling — stay client-side, text in `SubmitInput` | n/a |

## 3. The one-writer invariant

**Statement.** Every `SessionView` is owned by exactly one lane for its whole
life: it folds **either** `MuseEvent`s through `MuseFold` (muse lane) **or**
`ProviderEvent`s into `Session` (provider lane), never both. No lane change
after creation — `provider_kind()` reads the host-fixed `provider_id` with no
setter "by design" (`crates/baaz/src/session.rs:876-884`); `pick_provider`
starts a *new* session instead (`SwitchProvider`/`NewSessionOnProvider`,
`crates/baaz/src/session.rs:300-325`, handled at
`crates/baaz/src/app/lifecycle.rs:2050-2070`).

**Mechanical enforcement (two layers).**

1. **Type layer.** Split construction: `SessionView::open_muse(…,
   client: Arc<MuseClient>)` vs `SessionView::open_lane(…, provider: Provider,
   events: …)`. Each sets a private `enum Lane { Muse { client },
   External { provider: Provider } }` field; `client` becomes
   `Option<Arc<MuseClient>>` as today but the lane task is spawned exactly
   once per constructor. There is no `set_lane`, no `set_provider`, no public
   field — the compiler rejects a second writer because no code path can
   attach one. (`provider_id` already has no setter; this extends the same
   trick to the event subscription.)
2. **Test layer.** A unit test on the view (new
   `crates/baaz/src/session/lane.rs::tests`): construct a lane view over
   `provider::scripted::ScriptedProvider` (the seam's existing non-muse stand-in,
   cf. `crates/baaz/src/conn.rs::connect_with` test), deliver one `Deltas`
   batch, assert the transcript; then deliver a `MuseEvent` to the same view
   and assert it is **refused/ignored** (returns no deltas, transcript
   unchanged), and vice versa for a muse view. Plus a ratchet extension:
   `crates/baaz/tests/seam_ratchet.rs` (`CEILING = 20`, may only fall) gains an
   assertion that `session.rs`/`app.rs` reference no per-lane `MuseEvent`
   subscription inside lane constructors (e.g. forbid `pump(` calls on lane
   views) — a grep-test in the same style as the existing ceiling.

## 4. Session list and persistence

Today: `Harness::load_sessions` pages `client.session_list`
(`crates/baaz/src/app/lifecycle.rs:342-385`), merges via
`sidebar::merge_session_list` (`:469`), paints provisional rows from the local
index first (`:954`). `session/list` only knows muse sessions, so lane
sessions need a parallel record:

- **Record.** On `Ack::Session` from `OpenSession`, insert a local row
  (`SessionEntry::provisional`-style) tagged with its `ProviderId` into the
  existing store (`crates/baaz/src/store.rs`, `sessions.rs`) — the same local
  rows the merge already keeps ("keeps local rows only",
  `crates/baaz/src/app/lifecycle.rs:958`). Key: `(provider, session_id)`.
- **Reopen with history.** `ResumeSession { cursor: last_cursor }`
  (cursor from `SessionView::last_cursor`, `crates/baaz/src/session.rs`);
  Claude Code replays `~/.claude/projects/<slug>/<id>.jsonl`
  (`fixtures/claude-code/resume.jsonl`, `docs/18-claude-code.md` §3); Codex
  thread resume (`docs/19-codex.md` §1-2). `ReadSession` for preview without
  attach; `ListSessions` per lane for sidebar refresh (never muse's
  `session/list`).
- **Titles / byline / grouping.** Keep the existing writers (`titles.rs`,
  `byline.rs`, `usage.rs`, project override via `set_override`,
  `crates/baaz/src/app/lifecycle.rs:1205`) — they key off session id and the
  folded `Session`, both lane-neutral. Ledger rows in `baaz.db` keep their
  current shape; Codex supplies richer usage input
  (`docs/19-codex.md` §6) folded into the same counters.
- **Archive / delete.** Client-side store operations + lane `shutdown`; no
  provider delete command exists in the seam (no `DeleteSession` variant in
  `crates/provider/src/command.rs`) — archived lane sessions are hidden
  locally, identical to hidden muse rows.

## 5. Transcript parity matrix

Cell key: **F** = already folded by the adapter (fixture cited) — needs baaz
wiring only; **W** = needs baaz wiring (adapter emits, view must render/route);
**U** = genuinely unavailable (evidence cited); **M** = muse-lane reference.

| row | Muse | Claude Code | Codex |
|---|---|---|---|
| streaming text | M (baseline) | F (`fixtures/claude-code/basic.jsonl`) → W: delta→`Session` apply in lane task (§2.2) | F (`fixtures/codex/basic.jsonl`) → W: same |
| reasoning/thinking | M | F (`ReasoningTraces Native`, `caps.rs:71`) → W | U or W: check `caps.rs` in `crates/provider-codex`; if `Unverified`, attempt and mark W |
| tool: shell (+ diff chip `+N/−N`) | M | F (`basic.jsonl` tool frames; `fold.rs:295 apply_blocks`, `:354 apply_result`) → W | F (`basic.jsonl`) → W |
| tool: edit/write with diff chip | M | F (same fold path) → W | F → W |
| tool: read / search / web | M | F (`mcp.jsonl`, `mcp-rust.jsonl` for MCP; `docs/18-claude-code.md` §4) → W | F → W |
| tool: MCP | M | F (`fixtures/claude-code/mcp-rust.jsonl`; `docs/18-claude-code.md` addendum) → W | U: `ClientTools Unverified` (`crates/baaz/src/providers.rs:187`) — attempt, surface refusal |
| todo/plan | M (`Block::Plan`) | F (`apply_blocks` maps plan frames; `docs/18-claude-code.md` §2) → W | F → W |
| subagents | M | W: `SubagentTurns Unverified` (`caps.rs:74`, `parent_tool_use_id` read off frames, never executed) — render if deltas arrive, do not promise | U/W per `crates/provider-codex/src/caps.rs` (`SubagentTurns Unverified` in registry mirror, `providers.rs:189`) |
| approvals | M | F (`fixtures/claude-code/permission.jsonl`, `permission-deny.jsonl`; `fold.rs:394 apply_approval`, `:469 decide_approval`; `Approvals Native`) → W: `external_approvals` + `DecideApproval` + `ListPending` | F (`fixtures/codex/approval.jsonl`; `docs/19-codex.md` §3 decision vocabulary) → W: same |
| questions | M | U: `Questions Unavailable` (`caps.rs:64`; "asks in prose, no id") — prose renders, no question UI | W: `Questions Unverified` (`providers.rs:184`) — wire `Answer/Dismiss/ClarifyQuestion`, surface refusal |
| errors | M | F (`partial.jsonl`) → W | F (`interrupt.jsonl`) → W |
| interrupted turn | M | W: `TurnControl Unverified` (`caps.rs:52`) — attempt `InterruptTurn`, fold what arrives | W: same (`interrupt.jsonl` proves the shape; gate `Unverified` → attempt) |
| usage + context meter + cost | M (`session/contextUsage`, `usage.rs`) | F (`fold.rs:139 model`, usage frames per `docs/18-claude-code.md` §5) → W: same counters/meter | F (ledger-grade input, `docs/19-codex.md` §6) → W |
| ledger row in `baaz.db` | M | W (same writer, lane-tagged) | W (same writer, lane-tagged) |
| model/effort chips from live child | M (`session/modelChanged`) | W: `pending_model` + `ListModels` + `SelectModel`; effort U with typed reason (`argv::reasoning_effort_unavailable_reason`, `composer.rs:221`) | W: `pending_model` + `codex_effort_options` (`composer.rs:241`); effort from live catalog |
| images in | M | F (`SubmissionPart::Image`, `command.rs:17-30`) → W: attach `images`/`files` chips to `SubmitInput.parts` | same → W |
| `@` mentions | M (client-side picker) | W: client-side, inline text (no neutral spelling, `command.rs:17-22`) | same → W |
| `/` commands, skills | M (client-side) | W: client-side (`run_command_with`), text into `SubmitInput` | same → W |
| auto title | M (`titles.rs`) | W: same writer off folded `Session` | same → W |
| byline | M (`byline.rs`) | W: same writer | same → W |

Acceptance rule: every **F/W** cell closes only when the repo's own tests for
the touched area pass (§7); every **U** cell must keep its cited evidence and
its disabled-with-reason UI.

## 6. The duplicate `session/start` defect

Task brief states: one `new` issues 2 `session/start`s, `+setprovider` 3,
`+send` 4; reproduce offline with
`./target/debug/baaz --no-connect --login signed-in --steps 'new'` (or online
reading `baaz:` log lines).

**Status: NOT reproduced in this task.** No binary was built and no steps run
— this task's hard rules scope verification to reading code plus
`cargo check`/unit tests, and the single verification command below. What code
reading shows, as a hypothesis with anchors (not a proven root cause):

1. `step_new` (`lifecycle.rs:923`) → `new_session` (`:1091`) →
   `new_session_in` (`:1105`) sets `session_switch_pending = true` (`:1184`)
   and fires `session/start` via `wire_call_in`. Separately, boot opens a boot
   session (`send_scripted`/boot path, `lifecycle.rs:513-599`), and
   `SwitchProvider`/`NewSessionOnProvider` (`lifecycle.rs:2050-2070`) each
   spawn another `new_session`. Counting `baaz:` log lines across
   boot-`start` + step-`new` + `setprovider`-switch (+ `send` auto-starting a
   session when none is open) plausibly yields 2/3/4 — i.e. the "duplicates"
   may be *distinct sessions started by distinct code paths*, not one path
   firing twice.
2. Candidate true-duplicate mechanism if the counts are same-session: the
   draft-reuse check (`draft_decision`, `lifecycle.rs:1126-1144`) racing the
   async `wire_call_in` completion — `open()` (`:1190`) plus a retried/late
   second `new_session_in` when `session_switch_pending` is released on error
   (`:1255`) and steps re-fire. Unproven without the log lines.

**Fix direction (only after reproducing with the offline command and reading
which `reason=` tags in the `session/start project=… reason=…` log at
`lifecycle.rs:1242` repeat):** make `new_session_in` idempotent per project
while `session_switch_pending` is set (second `new` while a start is in flight
reuses the pending session instead of issuing another `session/start`), and
ensure the boot path does not start a session when `--steps` begins with
`new`. State the observed `reason=` sequence in the fixing commit; do not fix
blind.

## 7. Implementation plan (≤6 sequential tasks)

Each task compiles and leaves the app working; muse sessions stay on
`MuseClient`/`pump`/`route` throughout. Globs are exact change scopes.

- **W1 — Lane-owned view type + one-writer test.** Title: split view
  construction by lane with the invariant test. Globs:
  `crates/baaz/src/session.rs`, `crates/baaz/src/session/lane.rs` (new),
  `crates/baaz/src/session/composer.rs`. Verification:
  `cargo test -p baaz session::lane`.
- **W2 — Lane open path (no send yet).** Title: `OpenSession`+history onto a
  lane view with shutdown. Globs: `crates/baaz/src/app/lifecycle.rs`,
  `crates/baaz/src/conn.rs`, `crates/baaz/src/session.rs`. Verification:
  `cargo test -p baaz --test seam_ratchet`.
- **W3 — Send/steer/stop + approvals/questions.** Title: route submit, steer,
  interrupt, decide, answer through `Provider::send` with gate UI.
  Globs: `crates/baaz/src/session/*.rs`, `crates/baaz/src/app.rs`.
  Verification: `cargo test -p baaz session::`.
- **W4 — Model/effort/mode/shell/fork/compact on the lane.** Title: config and
  lifecycle commands per capability table. Globs:
  `crates/baaz/src/session/composer.rs`, `crates/baaz/src/session.rs`,
  `crates/baaz/src/projects.rs`. Verification:
  `cargo test -p baaz composer`.
- **W5 — Sidebar/persistence/ledger for lane sessions.** Title: local rows,
  resume, titles, byline, ledger tagging. Globs:
  `crates/baaz/src/sessions.rs`, `crates/baaz/src/sidebar.rs`,
  `crates/baaz/src/store.rs`, `crates/baaz/src/titles.rs`,
  `crates/baaz/src/byline.rs`, `crates/baaz/src/usage.rs`.
  Verification: `cargo test -p baaz sidebar`.
- **W6 — Duplicate `session/start` fix + parity sweep.** Title: idempotent
  start and matrix close-out. Globs: `crates/baaz/src/app/lifecycle.rs`,
  `crates/baaz/src/steps.rs`, `crates/baaz/tests/*.rs`. Verification:
  `cargo test -p baaz`.

## 8. Risks and what no automated test can prove

- **Process lifetimes.** Child spawn/exit/reconnect races, orphaned `claude`
  / `codex` children after crash, `ConnectionLost` mid-turn. Unit tests use
  `ScriptedProvider`; only `ps`-level observation proves cleanup.
- **Live auth.** `~/.codex/config.toml` (read-only here), Claude Code OAuth,
  sign-out banners — no test signs in. The `docs/19-codex.md` §7 live
  findings already show the backend surprising the client.
- **UI truth.** A green gate once certified five byte-identical right-pane
  screenshots (see repo notes on `uiprobe/1`); the parity matrix (§5) is only
  done when a human looks at the relay capture for each lane.
- **Fixture drift.** Adapters are tested against live captures
  (`fixtures/claude-code/`, `fixtures/codex/`); backends change wire shapes
  without notice. The drift gate (`docs/19-codex.md` §9) plus re-capture is
  the only detector.
- **Gate honesty for this doc.** The verification below proves the file exists
  with sections. It does NOT prove any anchor, fixture claim, or the §6
  hypothesis correct.
