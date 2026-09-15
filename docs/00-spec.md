# Harness — Muse Code chat slice: specification

Status: frozen 2026-09-08. Change only with the owner's agreement; record changes in
`docs/CHANGELOG.md`.

This repository is the agentic coding **harness**. Its first slice is a feature-complete
macOS chat interface to Meta's **Muse Code** agent (`muse` CLI 1.0.3, subscription, the
owner is already logged in), built on the `aui` component library at
`/Users/alex/Projects/agentic-ui` (gpui-pre 0.3.3 + gpui-kit 0.6, path dependencies).

Ground truth for everything about Muse is
`/Users/alex/Projects/agentic-ui/docs/10-muse-research.md` (the "research doc"). Wire
captures from live `muse serve` sessions are in `fixtures/msp/*.jsonl`, the exact schema for
this binary in `fixtures/msp/msp/` and `fixtures/msp/msp-ts/msp.d.ts`. When the research doc
and a capture disagree, the capture wins; when the schema and a capture disagree, the capture
wins and the discrepancy is written down in `docs/CHANGELOG.md`.

## 1. Goal and scope

One window. Left: the sessions sidebar. Centre: transcript + docked composer. No right pane
in this slice (the shell keeps the slot; `ToggleRightPane` is a no-op that stays wired).

In scope, all against the real `muse serve` backend:

- **Auth**: detect signed-in state, a login screen that drives `muse login` (device code),
  logout, "signed in as" chip.
- **Sessions**: list (grouped by date, filtered to a workspace), new, resume with full
  history backfill, fork, switch; rename/search/delete are client-side over the local index
  because they are not on the wire (see §3.7).
- **Composer**: send; queue or steer while busy with an editable queued strip; stop with
  retract; images (paste/drop/attach); `@path` mentions; `/` command menu (client commands +
  skills); model picker; reasoning-effort picker; approval-mode picker; plan-mode toggle;
  context meter with pressure states; manual compaction; prompt history.
- **Transcript**: streaming markdown with the library's syntax highlighting; reasoning blocks
  streamed and collapsible; tool cards by kind with streamed output; user-shell items;
  approval cards (server-minted choices, multi-stage, feedback, policy/judge resolutions);
  question cards (single/multi, previews, timeout, clarify); todo list; compaction markers with
  token counts; cancelled/retracted/retry-scheduled markers; fork-source marker; per-turn
  token footer; error cards with retry; view-gap notice; subagent and workflow items rendered
  generically (a card with role/objective/status; no drill-in this slice).
- **Errors**: inline banners for turn failures and recoverable wire errors; a modal dialog for
  session-identity and protocol errors; reconnect on child exit.
- **Keyboard**: full keyboard operation of composer, menus, approvals and questions (§3.9).

Out of scope: right pane (diffs/terminal/browser), subagent drill-in, workflows control,
voice, worktrees, enterprise config, any other provider.

## 2. Architecture

Cargo workspace, three crates, plus path dependencies on `aui`, `aui-protocol`, `aui-motion`,
`aui-tokens`, `aui-icons` from agentic-ui. Exactly one `gpui-pre` and one `gpui-kit` in
`cargo tree -d`; align to the library's versions, never the reverse.

```
crates/muse-client    transport only, no gpui, no aui.      MuseClient, MuseEvent, typed params/results
crates/muse-adapter   fold MSP events into aui_protocol.     MuseFold::apply(MuseEvent) -> Vec<Delta> + SideState
crates/harness        the gpui app.                          Entities, views, intents -> MuseClient calls
```

### 2.1 muse-client

- Spawns `muse serve --trust-workspace` (session log on by default; `--no-session-log` behind a
  flag for tests). One child per app process; sessions are multiplexed on it.
- NDJSON JSON-RPC 2.0: write `serde_json::to_string(&frame) + "\n"`, read `BufReader::lines()`.
  Reader thread → `crossbeam`/`std::sync::mpsc` channel of `MuseEvent`; writer thread; the
  UI thread never blocks on the pipe.
- Client request ids: monotonic `i64`. Server requests (`approval/request`,
  `userInput/request`) carry the server's ids and are surfaced as `MuseEvent::ServerRequest`;
  they are **never answered with a JSON-RPC result**, only via `approval/decide` /
  `userInput/*`. Deduplicate against the `…/requested` notification on `approvalId` /
  `userInputId`.
- Every command mints `commandId = Uuid::now_v7()`. The adapter keeps `commandId → composer
  text` so `turn/unqueued` and `turn/retracted` can restore the prompt.
- Typed structs for every method/notification in `msp.d.ts`, `#[serde(rename_all =
  "camelCase")]`, all string enums `#[serde(other)]`-tolerant (the schema calls most of them
  open). `initialize` with `clientInfo {name:"harness", version}`; compare
  `schema.fingerprint` with `fixtures/msp/msp/manifest.json` and **warn, never fail**.
- Ordering rule: view events may arrive before the command's ack. Never gate folding on acks.
- `view/gap`: buffer live events with cursor ≥ `next`, `view/page` the missing range
  (`limit: 1000`), splice, discard overlap.
- Child exit → `MuseEvent::Closed`; the app respawns, `initialize`, `session/resume` with the
  last observed `viewCursor` (history mode `none`) and shows a banner while reconnecting.
- Provider is per session: tests use `session/start { providerId: "echo" }`; the app uses
  `"meta"`. **`echo` is not free** (corrected in phase 4): on a signed-in machine the session
  log records `provider_id: echo` at intake and then a metadata record naming
  `provider_id: meta, model_id: muse-spark-1.3-contributor`, and the turn bills reasoning
  tokens. `--provider` picks a route, not a bill; the cap of five real turns per phase covers
  every provider. Only `--replay` and `--no-connect` cost nothing — plus `session/start`,
  `session/userShell`, `session/fork` and `approval/*`, which make no model call. See
  `01-transport.md` §6.

### 2.2 muse-adapter

`MuseFold` owns one `aui_protocol::Session` per Muse session plus the state the `Delta` enum
cannot carry:

```
SideState { context: Option<ContextUsage>, cumulative: CumulativeTokenUsage, goal: Option<Goal>,
            queued: Vec<QueuedTurn{turn_id, command_id, text}>, retry: Option<RetrySchedule>,
            pending_approvals: BTreeMap<ApprovalId, ApprovalRequestParams>,
            pending_inputs: BTreeMap<UserInputId, UserInputRequestParams>,
            model: EffectiveModel, approval_mode: ApprovalMode, last_cursor: String }
```

The mapping table in research doc §7.2 is normative. Items map by `itemId → (turn index,
block index)`. User messages open a `Turn::User`; agent messages, reasoning, tool calls,
user-shell, subagent, workflow, reminder-child and compaction items are blocks of the current
`Turn::Assistant`. Unknown item kinds render as a generic card: kind + status + `fallbackText`.

Pure and testable: `cargo test -p muse-adapter` replays every `fixtures/msp/*.jsonl` through
`MuseFold` and asserts on the resulting `Session` (snapshot tests with `insta` or plain
`assert_eq` on serialized JSON; pick one and stay with it).

### 2.3 harness (gpui)

Boot as `aui/examples/minimal.rs` does: `gpui_kit::application().with_assets(AuiAssets)`,
`aui::init`, text scale from `aui_tokens::scale`. Entities:

- `App` — client handle, auth state, session list, active session id, theme.
- `SessionView` per open session — `MuseFold`, composer draft, scroll state, focus.
- `Overlays` — palette, menus, dialog stack, toasts.

Rendering is a pure function of state each frame: build `Vec<Turn>` rows with
`aui::transcript::*`, dock `aui::composer::docked_composer`, and handle `Intent`s by calling
`MuseClient`. No component performs I/O. All motion through `aui_motion`; all colours,
sizes and durations through tokens. Both themes correct.

## 3. Decisions

### 3.1 Plan mode is client-side, and flagged as such

MSP has no plan mode; the TUI's `/plan` is a skill. Plan mode in the harness:

- Toggle with **Shift+Tab** or `/plan`; a "Plan" pill in the composer footer, click to exit.
- While on, a send does two things: `session/setApprovalMode { denyUnmatched }` (remember the
  previous mode) and prefixes the text part with the plan preamble in
  `crates/harness/src/plan.rs` ("Create a grounded, decision-complete plan for the request
  below, then stop and wait for approval. Do not edit files or run commands that change
  state."). `displayText` carries the user's text only, so the transcript shows what they typed.
- The reply renders as a normal assistant turn; when it completes, a `Block::Plan` is
  appended from the reply text (headings → steps) with Accept / Refine / Reject. Accept
  restores the previous approval mode and sends "Implement the plan above." Refine keeps plan
  mode and focuses the composer. Reject restores the mode and does nothing else.
- Phase 3 must first probe whether sending the literal text `/plan <prompt>` invokes the skill
  server-side (one real turn). If it does, use that instead of the preamble and keep
  `denyUnmatched`; record the finding in `docs/CHANGELOG.md`.

### 3.2 Auth

Superseded 2026-09-11 by docs/diagnosis/login.md (D22–D29).

- Signed-in probe at boot: `~/.config/muse/auth.json` has `providers.meta` **and**
  `model/list` reports `source: "providerCatalog"`. Either missing → login screen.
- Login screen spawns `muse login` with `MUSE_LOGIN=1`, parses stderr for the URL and the
  code (research doc §3.2), shows both with a "Open in browser" button, a spinner and the
  expiry; `Signed in.` → re-probe and enter the app; `muse: …` lines → error state with retry.
  Never log the URL or code.
- "Signed in as" shows `user_full_name` / `user_email` in the sidebar footer; logout runs
  `muse logout` and returns to the login screen. `META_API_KEY` in the environment is shown
  as "API key" identity.
- A `turn/completed` failure whose message contains `not authenticated` or whose kind is
  `configError` with an auth-looking message routes to the login screen via a modal ("Signed
  out of Muse") rather than an inline error.

### 3.3 Approvals live in the transcript, with a banner

Both reference apps dock approvals above the composer; the library's design places them in
the transcript with `needs_you_banner`. Keep the library's design. `approval_card` gains:
server-minted choice list (label, scope, rule preview), stage indicator `n/N` with argv,
badges for `protectedWrite` / `judgeEscalated`, a feedback field revealed by a choice with
`acceptsFeedback`, and resolved states for `policy` and `llmJudge` (allowed or denied) that
are never actionable. Digits 1–9 choose; `Enter` confirms a feedback choice; the banner's
action scrolls to the card.

### 3.4 Queue and steer

Enter while a turn runs queues (`ifBusy: "queue"`, the wire default); ⌘↩ steers
(`turn/steer` with `expectedTurnId` from the running turn). The queued strip sits above the
composer: each row has Edit (unqueue + restore), Remove (unqueue), Steer now. The strip
reorders by `turn/unqueued`/`turn/started`, never optimistically. Stop is
`turn/interrupt { retract: true }`; a retracted prompt is restored to the composer.

### 3.5 Reasoning effort and models

The effort picker is driven by the MSP enum `none…xhigh, ultra` and hides the catalog's
`max`. Model picker rows come from `model/list` (`label`, context limit, description,
default/active badges); `session/setModel` applies. Both are `aui` menus anchored to the
composer chips via `popover_layer`.

### 3.6 Approval mode picker

Four MSP modes with these labels and one-line descriptions:
`allowAll` "Full access", `onRequest` "Auto" (default), `promptUnmatched` "Ask", `denyUnmatched`
"Read-only". `aui_protocol::PermissionMode` is replaced by an enum with exactly these four
values plus `Plan` as a client-side overlay flag on the session, not a wire mode.

### 3.7 Session operations not on the wire

`session/list` gives identity and timestamps; `~/.local/share/muse/session-index.db` gives
`session_name`, `title`, `first_user_prompt`, `search_text` (read-only, `rusqlite`, opened
read-only, tolerate absence). Rename writes a harness-side override in
`~/Library/Application Support/harness/sessions.json` (never the muse index). Delete hides
the session in the same file ("Hide" in the menu, undo toast). Search filters the sidebar
over title + first prompt + search text.

### 3.8 Errors

- Wire errors on a command: `invalidParams`, `commandRejected`, `backpressured`,
  `sessionNotLoaded`, `approvalNotFound`, `approval*`, `userInput*` → inline banner over the
  composer with the message and a dismiss.
- `sessionInUse`, `sessionNotFound`, `sessionAmbiguous`, `parseError`, `notInitialized`,
  `experimentalRequired`, `internal`, `overloaded` → modal dialog with one primary action
  (Reconnect / Open another session / Dismiss).
- `turn/completed` `failed` → `Block::Error` with retry when `retryable`; retry resends the
  same input. `turn/retryScheduled` → a status row "attempt n/m · retrying in Ns · reason"
  with a local countdown.
- Child exit → banner "Muse disconnected, reconnecting…" then either recovered or a dialog.

### 3.9 Keyboard

| Action | Keys |
|---|---|
| Send / queue while running | Enter |
| Steer while running | ⌘Enter |
| New line | Shift+Enter |
| Stop (retract) | Esc with empty composer; ⌃C anywhere |
| Plan mode toggle | Shift+Tab |
| Command menu / mention picker | `/` at line start, `@` |
| Prompt history | ↑ / ↓ on the first/last line |
| Approval or question choice | 1–9; Enter confirms; Esc collapses |
| Command palette | ⌘K |
| Sidebar | ⌘B; ⌘N new session; ⌘⇧F search sessions |
| Model / effort / mode menus | ⌘⇧M / ⌘⇧E / ⌘⇧P |
| Compact | via `/compact` and the context meter |

### 3.10 Slash commands (client-side)

`/model`, `/effort`, `/mode`, `/plan`, `/compact`, `/fork`, `/name`, `/resume`, `/status`,
`/usage`, `/clear` (new session), `/logout`, `/help`. Skills from `muse skills list` appear in
the same menu tagged `skill`, and insert `/name ` as text.

## 4. Library changes (in agentic-ui)

Library changes are made in the `agentic-ui` checkout beside this repository, on a feature
branch, committed per phase with the library's gates (`cargo build --workspace`, `cargo test
--workspace`, clippy `-D warnings`, doc `-D warnings`, `docs/06-api.md` regenerated). Design
rules of the library apply (docs/00-agent-brief.md, docs/04-design-rules.md); new components
get a gallery entry with sample data.

`aui-protocol`: `Provider::Muse`; `PermissionMode` → the four MSP modes + plan overlay;
`Delta::{ThinkingDelta, ToolOutputDelta, TurnRemoved, BlockRemoved}`; `Block::Approval` carries
choices, stages, badges, feedback and the resolved-by; `Block::Question` gains header,
previews, timeout; `Block::{Goal, Generic}`; `MarkerKind::{TurnCancelled, TurnRetracted,
RetryScheduled, ViewGap, ForkedFrom}`; `Intent::{Steer, Unqueue, EditQueued, Compact,
SetModel, SetEffort, SetMode, Fork, Clarify, CancelQuestion, Login, Logout}`; `TurnMeta`
keeps prompt/output tokens and adds `reasoning_tokens`.

`aui`: `overlay::dialog` (modal, focus trap, primary/secondary, Esc); `screens::login`
(device-code layout); `composer::{model_menu, effort_menu, mode_menu}` on `plus_menu`'s
primitives; `data::context_meter` (ring + percent, `warning`/`blocked` states, no-denominator
mode, hover breakdown, compact action); `composer::queue_strip`; `transcript::retry_row`;
`transcript::approval_card` v2 (§3.3); `transcript::question_card` previews/timeout/clarify;
`transcript::generic_item_card`; `nav::session_row` rename affordance and a sidebar search
field; `feedback::banner` gains an action slot; `aui-icons` gains a Muse mark.

## 5. Phases and gates

Each phase was built against this spec and the research doc. Every gate was reviewed on
screenshots (light and dark, 1440×900) and a read of the diff before the next phase started.
Committed per phase in both repositories.

1. **Transport and fold.** `muse-client`, `muse-adapter`, the `aui-protocol` extensions, replay
   tests over every fixture, a live echo-provider integration test (`cargo test -p muse-client
   -- --ignored live_echo`), a `harness-probe` binary that starts a session, sends a turn and
   prints folded deltas. Gate: all fixtures fold without `Generic` fallbacks except
   `reminderChild`/`workflow`; the live echo test passes.
2. **Shell, sessions, streaming.** The app boots, auth probe and login screen, sidebar lists
   and resumes real sessions with backfill, a new session streams a real turn with markdown,
   reasoning, tool cards and per-turn tokens; stop/retract; reconnect banner. Gate: one real
   turn screenshotted in both themes; echo-provider flag for demos.
3. **Composer controls.** Model, effort and mode menus; context meter and compaction; queue
   strip with steer; mentions and command menu with skills; plan mode (§3.1 with the probe);
   prompt history; images. Gate: every control changes real session state and is reflected
   back from the server's notification, not optimistically.
4. **Approvals, questions, errors.** Approval card v2 with multi-stage and feedback; policy
   and judge resolutions; question card with previews/timeout/clarify; error banners, dialogs,
   retry and retry-scheduled; markers; fork; todo; goal. Gate: `transcript-approve.jsonl` and
   `transcript-real.jsonl` replay to the intended cards; one real multi-stage approval driven
   from the UI.
5. **Polish.** Motion review against the library's springs, focus rings and tab order, empty
   states, first-run copy, `docs/` (README, architecture, keymap), CI matching agentic-ui's.

Real-provider spend: at most five real turns per phase; everything else on `echo`.

## 6. Conventions

Inherited from agentic-ui `docs/00-agent-brief.md`: no literal colours/sizes/durations,
stateless `RenderOnce` components with intents out, `popover_layer` for anything that
overflows, `AuiStyled` text roles, both themes, `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"` before every cargo command, own `CARGO_TARGET_DIR` per
worktree when building from more than one at once. Rust 2021, `rust-version` matching agentic-ui, clippy
and rustdoc clean under `-D warnings`.
