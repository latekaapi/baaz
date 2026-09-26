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
  already arrived as a delta. The tap parks only as the decision-routing
  record (and the sidebar row's fallback): on a provider lane the strip
  above the composer stays empty (`external_strip`), so the fold's inline
  approval card — carrying the full choice set the adapter declares — is
  the one surface, decided through the same card via `DecideApproval`.
  `QuestionRaised { … question_id, headline }` → same surface for questions.
  `ConnectionLost { reason }` → reconnect flow.
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
| submit / queue | `SubmitInput { request_id, session_id, parts, display_text, effort }` (`SubmissionPart::Text/Image`, `crates/provider/src/command.rs:17-30`) — `effort` is the chip's neutral level id, `None` for Default | n/a (all lanes `Native`) |
| steer running turn | `SteerInput { …, expected_turn, parts }` — turn-change race guard | disabled with reason (Claude Code: `SteerTurn Unverified` — attempted, not refused; `crates/provider-claude-code/src/caps.rs:51`) |
| stop button | `InterruptTurn { turn, retract }` | disabled with reason |
| non-urgent cancel | `CancelTurn { turn }` | same gate as stop (`TurnControl`) |
| reclaim queued row | `ReclaimQueued { turn }` — refused if already launched | same gate |
| approval decide | `DecideApproval { approval, choice, stage_token, feedback }` — `stage_token` is the stale-stage race guard; drain `take_external_outbox()` oldest-first (`crates/baaz/src/session.rs:916`) | approvals `Native` on both new lanes; no disabled state |
| question answer | `AnswerQuestion { question, answers: Vec<QuestionAnswer> }` | Claude Code `Questions Unavailable` (`crates/provider-claude-code/src/caps.rs:64`): question UI hidden, Coleman prose renders as text; Codex `Unverified` (`crates/baaz/src/providers.rs:184`): attempted |
| question dismiss / clarify | `DismissQuestion` / `ClarifyQuestion` | same as answer |
| model select | `SelectModel { session_id, model, model_provider }` + record `pending_model` until echo (`crates/baaz/src/session.rs:925`); catalog from `ListModels { session }` | catalog missing → `MODEL_UNAVAILABLE_ROW` stand-in (existing) |
| effort | rides the next turn as `SubmitInput.effort`: muse maps it onto `turn/start` `reasoningEffort`; Codex onto `turn/start` `effort` (`child::turn_start_request`); Claude Code onto its `--effort` launch flag, relaunching with `--resume <id> --effort <new>` when the pick changed (`resume_launch_for_effort`). Codex levels from folded catalog (`codex_effort_options`); Claude Code lists `low, medium, high, xhigh, max` (`argv::CLAUDE_EFFORT_LEVELS`) | menu shows reason, never empty (a model with no row still does) |
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
| user prompt bubble | M | F (`edit.jsonl` echo + `fold::UserText`, test `user_echo_folds_to_exactly_one_user_turn`; `--replay-user-messages` now in `base_argv`, test `base_lane_replays_user_messages`). Pre-replay captures (`basic.jsonl`) echo nothing — no bubble can fold from them | F (`basic.jsonl` `userMessage`, unchanged) |
| reasoning/thinking | M | F-counts/U-text: `thinking.jsonl` (sonnet) folds the thinking block + `reasoning_tokens: 262` (test `thinking_fixture_folds_thinking_block_and_reasoning_tokens`); the thinking *text* is signature-only on this wire (empty), so no non-empty trace exists to fold | F-counts/U-text: `thinking.jsonl` (`effort: "max"` on `turn/start`, `reasoning_tokens` in footer, test `thinking_effort_recorded_and_reasoning_counted`); reasoning items arrive empty (`summary: []`, `content: []`), so no `Thinking` block is forged |
| tool: shell (output + inner command) | M | F (`read-search.jsonl`, `error.jsonl`; output lines + `Exit code N` parse, tests `read_search_…`, `error_fixture_…`) → W | F (`read-search.jsonl`, `error.jsonl`; `aggregatedOutput` join + `/bin/zsh -lc` unwrap, tests `shell_card_shows_inner_command_with_captured_output`, `failed_command_folds_to_error_card_with_output`) → W. No `outputDelta` frames observed (fast runs go started→completed); if they appear they are carried, not folded |
| tool: edit/write with diff chip | M | F (`edit.jsonl`: `structuredPatch`/created-content → `Edit` body + `+2/−1` / `+1/−0` chips, test `edit_fixture_folds_write_and_edit_cards_with_diff_stats`) → W | F (`edit.jsonl`: `fileChange` → Wrote card + `+2/−0` chip; `turn/diff/updated` carried, chips come from the item, test `file_change_folds_to_write_card_with_diff_stat`) → W |
| tool: read / search / web | M | F-read (`read-search.jsonl`: `Read` card with the wire's `numLines`, test `read_search_…`); search ran via Bash grep on a shell card. U-cards: `Grep`/`Glob` tool names stay `Generic` — no wire evidence for their result shapes (the model never called them) | F (`read-search.jsonl`: one shell card for read+search via `commandActions`, same shell tests) → W |
| tool: MCP | M | F (`fixtures/claude-code/mcp-rust.jsonl`; `docs/18-claude-code.md` addendum) → W | U: `ClientTools Unverified` (`crates/baaz/src/providers.rs:187`) — attempt, surface refusal |
| todo/plan | M (`Block::Plan`) | F (`todo.jsonl`: TaskCreate/TaskUpdate → one `Block::Todo`, test `todo_fixture_keeps_one_todo_card_with_three_done_items`) → W | F-prose/U-structured: `todo.jsonl` keeps plans in prose (`Todo update: …`, test `todo_updates_arrive_as_prose_without_structured_items`) — no plan item exists on the wire, so no `Todo` card is minted |
| subagents | M | F (`subagent.jsonl`: `Agent` + `parent_tool_use_id` linkage → nested `SubAgent` card, flushed before finish, test `subagent_fixture_nests_transcript_in_agent_card`) → W. Closes `SubagentTurns Unverified` for the transcript lane | F-markers/U-nested-transcript: `subagent.jsonl` (`subAgentActivity` → `Delegated` cards, `wait` → Generic, test `subagent_delegations_card_with_paths`) → W. The nested transcript lives on another thread and is never delivered here, so cards carry the delegation, never a forged transcript |
| approvals | M | F (`permission.jsonl`, `permission-deny.jsonl` + `approval-default.jsonl` ask-then-run, test `approval_default_fixture_cards_pending_approval_then_runs`) → W: `external_approvals` + `DecideApproval` + `ListPending` | F (`approval.jsonl` + `approval-default.jsonl` human-reason ask + accept + run, test `approval_default_flow_asks_then_runs`) → W: same. The request itself is pump routing (answered, never rendered); the fold cards the resulting change |
| questions | M | U: `Questions Unavailable` (`caps.rs:64`; "asks in prose, no id") — prose renders, no question UI | W: `Questions Unverified` (`providers.rs:184`) — wire `Answer/Dismiss/ClarifyQuestion`, surface refusal |
| errors | M | F (`partial.jsonl` + `error.jsonl` exit-code card) → W | F (`interrupt.jsonl` + `error.jsonl` failed-status card) → W |
| interrupted turn | M | W: `TurnControl Unverified` (`caps.rs:52`) — attempt `InterruptTurn`, fold what arrives | W: same (`interrupt.jsonl` proves the shape; gate `Unverified` → attempt) |
| usage + context meter + cost | M (`session/contextUsage`, `usage.rs`) | F (`tokens_in` = input + cache-read + cache-write, test `tokens_in_sums_bare_input_and_both_cache_legs`; cache legs stay informational) → W: same counters/meter | F (ledger-grade input, `docs/19-codex.md` §6) → W |
| ledger row in `baaz.db` | M | W (same writer, lane-tagged) | W (same writer, lane-tagged) |
| model/effort chips from live child | M (`session/modelChanged`) | W: `pending_model` + `ListModels` + `SelectModel`; effort rides `SubmitInput` onto `--effort`, relaunching with `--resume` when the pick changed (`composer.rs`, `argv.rs`, `lib.rs::resume_launch_for_effort`) | W: `pending_model` + `codex_effort_options`; effort rides `SubmitInput` into `turn/start` `effort` (`child::turn_start_request`), proven in `thinking.jsonl` as `effort: "max"` |
| images in | M | F both directions (`image.jsonl`: `user_content_line` sends base64 `source`, echo folds to an attachment; tests `image_turns_carry_base64_source_parts`, `image_fixture_accepts_image_part_on_user_turn`) → W: attach `images`/`files` chips to `SubmitInput.parts` | F-receive/U-send (`image.jsonl`: `localImage` echo → user-turn attachment, test `image_part_accepted_as_user_attachment`); send stays refused with reason — `turn/start` wants a local path but the seam carries bytes (stage the bytes to a file first) |
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

**Resolved 2026-09-26 (W6), reproduced against the real binary first.**
With the remembered default provider = Codex, one scripted `new` logged two
`provider lane open provider=codex` lines and the sidebar gained two "New
session" rows; `setprovider:claude-code` added a third. The muse-era
`session/start` 2–4× report is the same shape. Root causes, all in
`crates/baaz/src/app/lifecycle.rs`:

1. **Eager boot.** `ensure_boot_session` opened a boot session for any
   pending `--steps` list, and the list's own head `new` opened another:
   two distinct sessions from two distinct code paths, not one path
   firing twice. Fix: `boot_decision` stays `Idle` when the steps begin
   with `new`/`new-in` (`steps::steps_begin_with_new`).
2. **No idempotence while opening.** A second `new` arriving while
   `session_switch_pending` held started a second open, because the draft
   name is only recorded when the first open lands. Fix: `new_session_in`
   / `new_session_in_root` start nothing while a switch is in flight —
   the in-flight switch owns the next session.
3. **The switch's own replacement looked like a duplicate.** `SwitchProvider`
   / `NewSessionOnProvider` close synchronously but start the replacement
   on a task; the guard in (2) would eat it, and a following `send:` ran
   before the lane finished (`no open session`). Fix: the handlers set
   `session_switch_pending` synchronously and claim the next start
   (`switch_claim`), which the guard honours exactly once; `steps_ready_for`
   holds session verbs (never window verbs) while a switch is pending.
4. **Overlapping opens.** Two opens in flight both landed, and the earlier
   ask finishing last stole focus and the send
   (`new;setprovider:claude-code` sent to Codex). Fix: every open carries
   a `provider_open_epoch`, and `finish_provider_open` drops any finish
   that is no longer current (child shut down, no record, no row, pending
   untouched).

After the fix one scripted `new` logs one `provider lane open` line and
shows one row; `SwitchProvider` on an empty draft replaces it and leaves
exactly one row. Also fixed alongside: the doubled `baaz: baaz:` log
prefix, the lane-blind status/row (`Waiting for approval…` / `Needs
approval` for provider taps), the twice-rendered approval sentence, and
reopened sessions rendering settled with exchange (not increment) turn
counts — see the W6 commit.

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
  - **Built 2026-09-26:** `Harness::open_on_provider` connects (background
    `ProviderFactory`, default = resolved `claude`/`codex` binary → adapter →
    `connect` → `OpenSession`) and lands a `new_on_provider` lane view with
    drafts entry, project override, provisional sidebar row, and focus; the
    pick routes there from `new_session_in`/`new_session_in_root`, a fresh
    draft is replaced via `close_view`, failures dialog
    `Couldn't start <provider>`, and view drop / quit shuts the child down.
    Offline chrome opens the lane over a scripted provider and logs
    `baaz: provider lane open provider=<id> session=<id>`. W1 findings fixed:
    `apply_deltas` refuses unknown sessions, the drain loop stops after
    `ConnectionLost`, and the three lane guards have tests.
- **W3 — Send/steer/stop + approvals/questions.** Title: route submit, steer,
  interrupt, decide, answer through `Provider::send` with gate UI.
  Globs: `crates/baaz/src/session/*.rs`, `crates/baaz/src/app.rs`.
  Verification: `cargo test -p baaz session::`.
  - **Built 2026-09-26:** one dispatch point
    (`SessionView::provider_send`, `crates/baaz/src/session/lane.rs`)
    carries every provider-lane action as a `provider::Command` on the
    background executor via `ProviderCall`, with `Unsupported` reasons
    shown in the session banner, never swallowed. Submit sends text plus
    composer images as `SubmissionPart::Image` with `display_text` verbatim
    (queued-while-running submits, mirroring muse); steer sends
    `SteerInput` with `expected_turn`; stop sends `InterruptTurn` with
    `retract`; reclaim sends `ReclaimQueued` — all under the existing
    `steer_gate`/`turn_gate`. Running state (`busy()`, stop button,
    "working" indicator) derives from folded `TurnStarted`/`TurnFinished`
    deltas plus the `SubmitInput` ack, so interrupted/failed turns settle
    the view. Approval presses send `DecideApproval` (id, choice,
    `stage_token`, feedback) by draining `take_external_outbox()`
    oldest-first; the card waits for the delta/ack resolution
    (no-optimism), re-parks on refusal, and `ListPending` is pulled on
    every lane open. Questions answer/dismiss/clarify through their
    commands where `questions_gate` allows; Claude Code's `Unavailable`
    questions refuse with the registry reason (its asks stay prose).
    `!` shell sends `RunShell` under the `SessionShell` gate; `retry_turn`
    resubmits the remembered text through the same route. The tier banner
    and its send gate are muse-lane-only (`set_tier_banner` refuses and
    `render_tier_banner` hides on provider lanes). Verified by twelve
    lane tests over a recording double in baaz test code (the provider
    crate untouched): submit/steer/stop/decide/answer/shell/retry parts
    and ids, pending-until-resolved cards, `Unsupported` reason surfacing,
    and the tier-banner refusal — each failing with its arm removed.
- **W4 — Model/effort/mode/shell/fork/compact on the lane.** Title: config and
  lifecycle commands per capability table. Globs:
  `crates/baaz/src/session/composer.rs`, `crates/baaz/src/session.rs`,
  `crates/baaz/src/projects.rs`. Verification:
  `cargo test -p baaz composer`.
  - **Built 2026-09-26:** the registry reads each provider's table from
    its adapter crate's `caps` function (muse at the newest supported
    floor, 1.3.0 — Baaz never observes the connected version), so no
    hand-copied table can drift; a test pins all three tables equal to
    their adapters. A lane open asks `ListModels`: the menu lists what
    the child returns, the chip shows the effective model's human name
    (never the provider id), and a pick sends `SelectModel` at once —
    recorded in `pending_model` until the ack, un-recorded with a banner
    on refusal. Claude Code keeps Baaz's supplied alias list (no catalog
    surface was ever probed). **W4c update:** effort no longer stays
    client-side — the pick rides `SubmitInput.effort` and each adapter
    maps it onto its own channel: Codex `turn/start`'s `effort`
    (`child::turn_start_request`, omitted when Default), Claude Code's
    `--effort` launch flag with a `--resume` relaunch when the pick
    changed (`lib.rs::resume_launch_for_effort`); the baaz lane sends the
    chip's level on every submit (lane test
    `the_chips_effort_rides_submit_input`). The Codex per-model levels
    still derive from the catalog through the existing
    `codex_effort_options` path, and the Claude Code menu lists the
    flag's own five levels. Approval-mode picks send
    `SelectApprovalMode` with the seam's closed mode set, and plan mode
    attempts the same switch — both adapters refuse a mid-session
    change, so the refusal banners its reason and the plan pill stands
    back down. The meter on a lane sums the folded transcript's
    `TurnMeta` input/output tokens ("N tokens", no window — no adapter
    reports one); cache tokens are excluded from the sums, per
    `TurnMeta`'s own double-count warning. `CompactSession` and
    `ForkSession` travel where not `Unavailable`, refusals banner their
    reason, and a forked ack opens as a new lane view on the same
    provider (fresh child + `ResumeSession`). The strip names only
    `Unavailable` cells with their human reason; `Unverified` is
    attempted, never advertised. Known seam gap, stated not hidden: the
    `ModelCatalog` ack carries ids, labels and the active flag but no
    per-model reasoning levels, so a production Codex lane fills the
    model menu live while its effort menu still waits on the raw
    `model/list` answer — until the seam extends the ack, that menu
    shows the typed reason. Verified by lane tests over recording
    doubles in baaz test code (adapter crates untouched): catalog,
    pick, meter, compact/fork, mode/plan, strip, and the table-equality
    pin — each failing with its arm removed.
- **W5 — Sidebar/persistence/ledger for lane sessions.** Title: local rows,
  resume, titles, byline, ledger tagging. Globs:
  `crates/baaz/src/sessions.rs`, `crates/baaz/src/sidebar.rs`,
  `crates/baaz/src/store.rs`, `crates/baaz/src/titles.rs`,
  `crates/baaz/src/byline.rs`, `crates/baaz/src/usage.rs`.
  Verification: `cargo test -p baaz sidebar`.
  - **W5b (2026-09-26): reopened sessions show history.** Both adapters
    replay history as the lane's first event on `ResumeSession`: Claude
    Code folds its `~/.claude` jsonl (the stored file is not the stream
    frames — no `init`/`result`, camelCase ids, model+usage on the
    message; the finish is synthesised, `docs/18-claude-code.md` §3),
    Codex folds the `thread/resume` response's `thread.turns[]`
    (`fixtures/codex/resume.jsonl`, `docs/19-codex.md` §5). The chip
    reads the last finished turn's model, so a reopen names the session
    instead of the provider. A record with no settled turns holds no
    history: it opens fresh and the stale record leaves with it. The
    refusal that stood in for Codex resume is gone.
  - **Built 2026-09-26:** `provider-sessions.json` in the support dir
    records each lane session (provider, session id, workspace, project,
    created/updated, turns, ack title, first prompt) — written on open,
    touched on admission, bumped on every settled turn, removed on
    delete. The sidebar builds one row per record in its project group,
    ordered by recency with muse rows, wearing the provider mark
    (`SessionSummary::provider`), with running/settled/needs-you status
    from the live view; `session/list` merges never drop them and the
    unchanged check ignores them. Titles, bylines, rename, archive, pin
    and hide ride the existing `sessions.json` writers keyed by session
    id: a lane admission fires the same muse side-session auto-title
    (never a second provider child), a settle records the free byline
    excerpt and may earn the same debounced rewrite. Clicking a stored
    session with no live view connects a fresh child and sends
    `ResumeSession` (full resume, never metadata-only); the adapter's
    replayed deltas fold into the view. Each settled lane turn writes one
    `usage_turns` row tagged `claude-code`/`codex` (schema v3 adds the
    `provider` column with a default in one transaction; muse rows keep
    today's untagged shape), keyed `(session_id, turn_id)` so replays
    never double-count. Deleting a provider session drops its record,
    row, overrides and views together (shutting the child down); closing
    an unsent draft forgets its record too. No new controls were added,
    so no new accessibility labels were needed. Verified by record
    round-trip, sidebar mark/ladder/merge, reopen-`ResumeSession` with
    replayed transcript, tagged-ledger, migration, and generated-title
    tests — each failing with its arm removed.
- **W6 — Duplicate `session/start` fix + parity sweep.** Title: idempotent
  start and matrix close-out. Globs: `crates/baaz/src/app/lifecycle.rs`,
  `crates/baaz/src/steps.rs`, `crates/baaz/tests/*.rs`. Verification:
  `cargo test -p baaz`.
  - **Built 2026-09-26:** one session per `new` (boot skips lists headed
    by `new`, in-flight guard with switch claim, last-open-wins epoch),
    steps wait for an in-flight lane open, single `baaz:` prefix,
    `lane-claude-code`/`lane-codex` probe entries, needs-you status and
    row for lane approvals, single-sentence approval card, settled
    reopened turns with exchange turn counts, and `firstPrompt`-only
    records resuming. Root cause in §6 above.
- **W7b — One approval card per provider session.** Title: the fold's
  inline approval card carries the full choice set the adapter declares
  (allow/deny on Claude Code, the four tokens on Codex) with the
  number-key shortcuts; the strip above the composer stays empty on
  provider lanes (`external_strip`). The card title names the provider
  with a verb that fits the tool (`approval_title`); the face is per
  tool (Bash → command + cwd, Write → path + preview, Edit → path +
  diff, WebFetch → URL, MCP → server/tool + args — never raw JSON,
  never wire ids); gated tool cards wait in the present tense and read
  done only after running; empty thinking earns no card; the sidebar
  row re-reads pending words on every approval delta
  (`ProviderApprovalsChanged` → `sync_row_live`). Decided through the
  inline card as `DecideApproval` with the card's own choice id; the
  card settles only on the server's resolution, never on the press.

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
