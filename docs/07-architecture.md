# Architecture

How Baaz is built today: the processes, the provider seam, who owns which
state, what lands on disk, and how one turn travels from the composer to the
sidebar. It describes the code, not a plan. When the two disagree, the code
wins and this file is out of date.

---

## 1. What Baaz is

Baaz is a native macOS app for running coding agents. It drives three agent
CLIs as child processes: Muse Code (`muse serve`), Claude Code (`claude`) and
Codex (`codex app-server`). It shows each session as a transcript with a
composer, lists sessions in a sidebar grouped by project, and adds a right
pane (Browser, Changes, Files), a terminal dock and a Chromium-based browser
mode (Browse).

The code lives in two repositories:

| repo | what it holds |
|---|---|
| `harness` (this one, published as `baaz`) | the app crate `crates/baaz` and the provider crates |
| `agentic-ui` (sibling checkout `../agentic-ui`) | the UI library: `aui` (components, screens, shell), `aui-protocol` (the neutral session model), `aui-tokens`, `aui-motion`, `aui-icons`, `aui-terminal` (pty and TUI backends), `aui-webview` (the pane, WKWebView via wry), `aui-cef` (the Chromium backend), plus the `aui-gallery` binary |

The root `Cargo.toml` pins every `aui*` crate to one git tag of
`agentic-ui` (currently `v0.7.0`) and pins gpui (`gpui-pre =0.3.3`) to the
version the library resolves. To build against the local library checkout,
a git-ignored `.cargo/config.toml` patches the eight `aui*` crates to
`../agentic-ui/crates/*`; `CONTRIBUTING.md` has the snippet. A library change
ships by tagging `agentic-ui`, then bumping the tag here.

The workspace members are `baaz`, `provider`, `provider-muse`,
`provider-claude-code`, `provider-codex`, `muse-client`, `muse-adapter`,
`session-store` and `mcp-bridge`. `crates/baaz-cef-helper` is its own
workspace on purpose: building it pulls in CEF, so only
`scripts/bundle.sh` builds it. The `cef` feature of `baaz` is off by
default; without it the browser pane uses WKWebView.

## 2. Processes and threads

```
Baaz.app
├─ baaz                      the app: one gpui UI thread + background threads
├─ mcp-bridge  (per agent)   stdio MCP server an agent child launches; talks
│                            to baaz over <state>/run/terminal-<pid>.sock
├─ muse serve  (one)         shared by every Muse session
├─ claude      (per session) Claude Code; relaunched on mode/effort change
├─ codex app-server (per session)
└─ Baaz Helper (CEF)         Chromium's renderer/GPU/network subprocesses,
                             running baaz-cef-helper (cef builds only)
```

Threads in the `baaz` process, by job:

| thread | where | does |
|---|---|---|
| UI thread | gpui | every entity, every render, every fold of provider events |
| background executor | gpui `background_spawn` | every blocking provider request (`WireCall`, `crates/baaz/src/wire.rs`), sqlite work, file walks |
| `muse-reader`, `muse-writer`, `muse-stderr`, `muse-gapfill` | `crates/muse-client/src/client.rs` | NDJSON in/out of `muse serve`; transcript gap fill |
| `muse-hub-reader` | `crates/provider-muse/src/hub.rs` | fold Muse events and route them to per-session lanes |
| `provider-claude-code-pump`, `-stdin`, stderr | `crates/provider-claude-code/src/child.rs` | one set per `claude` child; stdin has one writer thread |
| `provider-codex-pump`, stderr, `codex-rate-limits`, `codex-effort` | `crates/provider-codex/src/child.rs`, `lib.rs` | one set per Codex child; writes go through a `Mutex<ChildStdin>`, no writer thread |
| `muse-bridge-N` | `crates/baaz/src/conn.rs` | one per lane: forwards a crossbeam receiver onto a futures channel the UI task awaits (the name is historical; every provider uses it) |
| `store:<file>` | `crates/baaz/src/store.rs` | one writer per `JsonStore` file, started on first save |
| terminal service | `crates/baaz/src/terminal/service.rs` | one listener plus one thread per socket connection; requests are queued and run on the UI thread by `TerminalService::drain` (a 15 ms pump while busy) |
| others | various | provider status probes (`provider_status.rs`), git (`git_ops.rs`), `utility.rs`, the stall watchdog, the log writer, quit signals, adblock load, the CEF clone sweep, the Muse billing-tier pty probe (`tier.rs`) |

Every provider event channel is an unbounded crossbeam channel. Each pump
folds one stdout line at a time inside `provider::guard::guard_line`
(`crates/provider/src/guard.rs`): a panic in a fold is caught, the poisoned
lock is cleared and the session ends as `ConnectionLost`, so the next send
revives it instead of the row reading Working forever.

Only one Baaz runs per state directory. `crates/baaz/src/instance.rs` takes
an exclusive `flock` on `<state>/lock` at startup; a second launch on the
same directory brings the first window forward and exits. `--print-tier` and
`--bench` skip the lock.

## 3. The provider seam

`crates/provider` is the neutral vocabulary between the app and any agent.
It names no provider's wire.

| piece | file | what it is |
|---|---|---|
| `ProviderAdapter` | `traits.rs` | object-safe, blocking trait: `id`, `connect`, `capabilities`, `dispatch`, `events`, `shutdown`, plus the peeks `read_usage` and `stderr_tail` |
| `Provider` | `traits.rs` | the wrapper the app holds. Its inherent `send` is the gate: it looks up `Command::required_capability()` and refuses before `dispatch` runs |
| `Command` | `command.rs` | 29 variants in one flat enum (open/resume/fork/list sessions, submit, steer, interrupt, approvals, questions, models, account, login, transcript paging) |
| `Ack` | `ack.rs` | what `send` returns: admission only; outcomes arrive as events |
| `ProviderEvent` | `event.rs` | 20 variants: `Deltas` (render changes as `aui_protocol::Delta`), approval and question taps, `TurnRunning`/`TurnSettled`, account, usage, `ConnectionLost`/`ConnectionRestored`, and more |
| `CapabilitySet` | `capability.rs` | one state per each of 17 capabilities; a missing entry panics at construction |
| `CapabilityState` | `capability.rs` | `Native`, `Emulated { how }`, `Unavailable { reason }`, `Unverified`. `allows_attempt` is true only for `Native` and `Emulated`, so the gate refuses `Unverified` as well as `Unavailable`, and the composer hides a control the session's provider cannot honour |
| child helpers | `child_env.rs`, `env_path.rs`, `session_spawn.rs`, `stderr_tail.rs`, `version.rs` | env scrubbing, login-shell `PATH`, `posix_spawn` with `SETSID` (used by the login-shell `PATH` probe and the tier probe), stderr ring, version floors. `child_env::state_dir_override()` is the one reading of `BAAZ_STATE_DIR` |
| `scripted` | `scripted.rs` | a test adapter |

`ProviderId` in the seam is `aui_protocol::Provider`. The app also has its
own three-variant `providers::ProviderId` (Muse, ClaudeCode, Codex) in
`crates/baaz/src/providers.rs`.

The adapter crates:

| crate | role |
|---|---|
| `muse-client` | the `muse serve` transport: framing, typed MSP schema, reader/writer threads, blocking requests. No UI dependency |
| `muse-adapter` | `MuseFold`: Muse events in, `aui_protocol::Delta`s out, plus `SideState`. Pure; replayed against `fixtures/msp/*.jsonl` |
| `provider-muse` | the Muse adapter. `MuseHub` owns the one `muse serve` child; each session is a `MuseLane` adapter on it. The only seam crate allowed to depend on `muse-client`. `replay.rs` turns a capture into the same `ProviderEvent`s for `--replay` and `--bench` |
| `provider-claude-code` | one `claude` child per session over stream-json stdio, with its own fold, controls channel and `CLAUDE_CONFIG_DIR=<state>/claude-home` |
| `provider-codex` | one `codex app-server` child per session over JSON-RPC (it sends requests back, notably approvals), with `CODEX_HOME=<state>/codex-home` |
| `session-store` | `SessionStore`: one session's transcript folded from neutral deltas, plus its side facts. Pure; every `SessionView` holds one |
| `mcp-bridge` | a stdio MCP server exposing Baaz's terminal and browser tools to an agent, forwarding to the app's socket |

`crates/baaz/src/providers.rs` builds providers through a `ProviderFactory`:
Muse resolves to a lane on the shared hub held in `MuseSlot`; Claude Code
and Codex spawn an adapter with the terminal relay and their Baaz-owned home
pinned, then `connect`. `crates/baaz/tests/seam_ratchet.rs` keeps
`muse-client` out of the app crate.

## 4. Entities and ownership

Production `Render` impls: `Harness` (`app.rs`), `SessionView`
(`session/render.rs`), `SidebarPane` (`sidebar_view.rs`), `OverlayView`
(`overlay.rs`) and `FeedbackTip` (`feedback/ui.rs`); `BenchRoot` is
bench-only. All paths in this section are under `crates/baaz/src/`.

### `Harness` — the window root

`Harness` (`app.rs`) has 241 fields. Its methods are spread over 52
`impl Harness` blocks in 38 files (`app/*.rs`, `sidebar_view.rs`,
`browser/mod.rs`, `dialogs.rs`, `login.rs` and others). Roughly:

| group | examples |
|---|---|
| providers, sign-in, onboarding | `muse_hub`, `muse_slot`, `provider_factory`, `wire`, `login`, `connect_statuses`, `codex_login`, `setup_sheet`, `provider_banners`, `tier` |
| session list and its caches | `sessions`, `list_cache`, `chain_index`, `index`, `overrides`, `provider_sessions`, `projects`, `saved_list`, `branches` |
| live session views | `active`, `registry`, `pending_id`, `drafts`, `unsent`, `starts_in_flight`, the switch/replace epochs and claims |
| handoff | `handoffs`, `handoff_epoch`, `pending_handoff`, `handoff_confirm` |
| sidebar view state | `sidebar_pane`, `sidebar_key`, `sidebar_list`, hover/reveal/rename fields, `sidebar_wheel` |
| right pane | `right_cache`, `right_resize`, `right_watch`, `right_files_focus` |
| browser and Browse mode | `browser` (`BrowserRegistry`), `browse_*` fields, the agent line and card (`line_ui`, `card_ui`), the four overlay windows (`overlay`, `corner`, `page_float`, `card_float`) |
| settings, skills, palettes, shortcuts | `settings_page`, `skills`, `import`, `chrome_import`, `shortcuts_cache`, several `TextareaState` query fields |
| terminal | `terminal_host`, `terminal_service`, `terminal_focus` |
| focus, capture, plumbing | focus handles, `capture`, scroll sweeps, `args`, `tasks`, `main_window` |

`Harness::render` is not cached. It first runs `on_frame` (window title,
overlay windows, provider status, right-pane cache, boot session, scripted
steps) and later `sync_browser` (`browser/mod.rs`), then composes the shell.
The sidebar and the transcript are `.cached()` subtrees, so they skip work
while clean (except while assistive tech is on).

### `SessionView` — one session

`SessionView` (`session.rs`, 157 fields) owns one session's
`SessionStore`, its lane, the composer and its menus, the virtual-list
bookkeeping, approvals and questions, quote/selection and handoff state.
Its methods live under `session/`: `lane.rs` (the provider lane),
`commands.rs`, `composer.rs`, `approvals.rs`, `questions.rs`, `shell.rs`,
`clocks.rs`, `handoff.rs`, `events.rs`, `scripting.rs` and `render.rs`.

The lane (`session/lane.rs`) is fixed per view: `Lane::Opening` while a new
child answers `OpenSession`, then `Lane::Provider` exactly once. One gpui
task per view drains its event stream and calls `on_provider_event`, so
the store has one writer. Dropping a view calls `shutdown_lane`, which hangs
up a Claude Code or Codex child. A view reports upward by emitting
`SessionEvent` (43 variants), which `Harness::on_session_event`
(`app/lifecycle.rs`) handles.

### `SessionRegistry` — every live view

A view lives in one of three places: the centre (`Harness::active`, still a
field passed in), the parked MRU, or the chat on the Browse agent line.
`SessionRegistry` (`app/registry.rs`) owns the last two and one event
subscription per live view. Every question about all views goes through it:
`live`, `find`, `iter_running`, `resubscribe`, `shutdown_all`. It caps idle
parked views at 8 (`SESSION_CACHE_LIMIT` in `app/lifecycle.rs`); a view
with a draft or with work in hand (`SessionView::must_stay_alive`: a turn
running or starting, a pending approval or question) is never evicted.

### The rest

| owner | file | owns |
|---|---|---|
| `SidebarPane` | `sidebar_view.rs` | nothing: its render calls `Harness::render_sidebar`. It is invalidated by comparing a hand-built `SidebarKey` each frame |
| `Overlays` | `overlays.rs` | state only: the dialog, the open menu, the palette, toasts, the `@`-picker walks. Rendered by `Harness` and by the composer |
| `OverlayView` | `overlay.rs` | the borderless panel windows that float above the native page (the page draws above all gpui content) |
| settings | `settings.rs`, `settings_providers.rs`, `skills_page.rs` | `SettingsPageState` and the pages, as `Harness` fields |
| browser | `browser/` | `BrowserRegistry` (one `WebviewState` per tab, CEF or WKWebView; a fake backend for captures), tabs, history and bookmarks (`browser.db`), adblock, favicons, the agent tools in `browser/automation/` |
| terminal | `terminal/` | `TerminalHost` (an entity: tabs by project root over `aui_terminal` ptys) and `TerminalService` (the socket) |

Every action a browser agent takes passes `browser::automation::policy::decide`
(`browser/automation/policy.rs`), with no model in the loop: a site rule
first, then the person's-tab rule, then the always-ask kinds (Send, Pay,
Delete, Upload, Sign in), then the Ask mode.

## 5. State on disk

`<S>` is `~/Library/Application Support/baaz`, or `BAAZ_STATE_DIR` when set.
Unit tests and deterministic captures that set no dir use a per-process temp
dir. `store::migrate_legacy_support_dir` moves the pre-rename directory once.

`JsonStore<T>` (`crates/baaz/src/store.rs`) is the one way a ported JSON file
is written. Each store has one writer thread. `save` takes the generation and
resolves the path when the snapshot is taken, on the caller's thread, and the
newest snapshot wins. `save_soon` waits 750 ms to coalesce a burst.
`write_now` writes on the caller's thread, ordered against the writer.
`flush` blocks until the queue is on disk (quit and window close). Writes go
through `write_authored`: the current file is kept as `<name>.bak` (a hard
link, else a copy), then the new bytes are written to a temp file, fsynced
with their directory and renamed. Reads go through `read_migrated`: a
store's migration hook runs first, and a file that will not parse is renamed
to `<name>.corrupt-<unix secs>` and its `.bak` is read instead. Every
problem is queued and shown as one toast per problem.

| file under `<S>` | writer | how | role |
|---|---|---|---|
| `session-meta.json` | `sessions.rs` | JsonStore | the person's facts per session: name, hidden, pinned, archived, project, handoff links. Canonical |
| `session-cache.json` | `sessions.rs` | JsonStore | titles, byline, last error, pane state. Regenerable |
| `sessions.json` | (legacy) | read once if neither half exists; never deleted | the pre-split file |
| `provider-sessions.json` | `provider_sessions.rs` | JsonStore | the list of Claude Code and Codex sessions: provider, workspace, project, origin. Canonical |
| `projects.json` | `projects.rs` | JsonStore, with a migration | projects, defaults, last picks. Canonical |
| `history.json` | `history.rs` | JsonStore | prompt history per workspace |
| `provider.json` | `providers.rs` | JsonStore | the last provider picked |
| `layout.json` | `layout.rs` | JsonStore | window layout, mode, right pane, browser and agent prefs |
| `Browser/tabs.json` | `browser/tabs.rs` | JsonStore (`save_soon`) | open tabs and spaces |
| `baaz.db` | `usage.rs` | SQLite, versioned migrations | the per-turn usage ledger |
| `search.db` | `search.rs` | SQLite FTS5 | `sessions_fts` (regenerable) and `files_fts` (files agents wrote; refilled only as turns complete) |
| `Browser/browser.db` | `browser/history.rs`, `browser/bookmarks.rs` | SQLite | browsing history and bookmarks |
| `Browser/Chromium/`, `Browser/cef.log`, `Browser/adblock/` | CEF, `browser/adblock.rs` | the engine's own | browser profile, log, filter lists |
| `tier.json`, `provider-status.json`, `index-cache.json`, `session-list-cache.json`, `provider-paths.json`, `claude-code-last-model.json`, `keymap.json` | `tier.rs`, `provider_status.rs`, `index.rs`, `list_cache.rs`, `details.rs`, `providers.rs`, `keymap.rs` | `write_atomic` directly | probe and list caches (regenerable), the path overrides and the keymap (authored) |
| `session-migration-journal.json` (+ `.consented`, `.dismissed`), `import-journal.json` | `session_migration.rs`, `import.rs` | `write_atomic` | migration and import journals |
| `onboarding_completed` | `provider_status.rs` | marker file | first-run flag |
| `sent/<id>.json`, `sent/<id>/` | `sent.rs` | files | the attachments each turn was sent with |
| `handoff/<dest>.json` | `handoff_snapshot.rs` | `write_atomic` | the source transcript copied at handoff |
| `claude-home/`, `codex-home/` | the CLI children | the CLIs' own | Claude Code and Codex transcripts and config (with links into `~/.claude` / `~/.codex` for auth and settings). Canonical transcripts for those providers |
| `mcp/` | `terminal/relay.rs` | files | per-session MCP configs for Claude Code |
| `run/terminal-<pid>.sock`, `lock` | `terminal/service.rs`, `instance.rs` | socket, flock | runtime only |
| `side-sessions/`, `browser-chats/`, `agent-shots/`, `newtab/`, `favicons/`, `tier-probe/`, `feedback/` | various | files | working dirs for side sessions and the browser chat, agent screenshots, caches, feedback reports |
| `logs/baaz.log` | `log.rs` | append, rotated | the log; the watchdog writes here too |

Outside `<S>`, Baaz reads Muse's `~/.local/share/muse/session-index.db`
(read-only, `index.rs`) and `~/.config/muse/auth.json` (`auth.rs`). New
sessions with no project default to `~/baaz-sessions`
(`projects::default_workspace`).

## 6. One turn, end to end

1. **Composer.** Submit builds `Command::SubmitInput` (`session/commands.rs`)
   and calls `SessionView::provider_send` (`session/lane.rs`).
2. **Off the UI thread.** `ProviderCall::provider_call` (`wire.rs`) runs
   `Provider::send` on the background executor under the view's
   `Arc<Mutex<Provider>>`. The gate checks the capability, then the adapter's
   `dispatch` writes to the child's stdin (Muse: through the hub's writer).
3. **Ack.** The completion returns through `update`. On success the view
   emits `SessionEvent::ProviderTurnAccepted`; on refusal it shows a banner.
4. **Events.** The child writes output; the adapter's pump folds each line
   into `ProviderEvent`s (Muse: `muse-reader` → hub fold → per-lane channel).
5. **Bridge.** The lane's `muse-bridge-N` thread moves them onto a futures
   channel; the view's lane task awaits it on the UI thread.
6. **Fold.** `on_provider_event` handles taps (approvals, questions, banners)
   and passes `Deltas` to `SessionStore::apply_deltas`, then `cx.notify()`.
7. **Render.** `SessionView`'s cached subtree re-renders; `transcript.rs`
   maps turns to `aui` elements on a virtual list.
8. **Settle.** `TurnSettled` ends the turn; the lane emits
   `ProviderTurnOutcome` and `ProviderTurnFinished` (usage ledger row,
   title, byline).

**How a sidebar row learns.** The registry's subscription delivers each
`SessionEvent` to `Harness::on_session_event`, which updates the row in
`Harness::sessions` (`sync_row_live` in `app/list.rs`, `record_turn_outcome`)
and the overrides. The changed row changes `SidebarKey`, so the cached
`SidebarPane` re-renders. For Muse, the hub's connection stream
(`Harness::on_hub_event`) also moves rows for sessions with no view in this
window.

## 7. Scripting and verification

| surface | where | what |
|---|---|---|
| `--steps a;b;c` | `crates/baaz/src/steps.rs` | about 70 verbs in three tables (window, session, login). Every handler returns `StepResult` (`Result<(), String>`); the runner stops at the first failed or unknown step, and a capture then exits 1 |
| `--screenshot` / `--screenshot-delay` | `crates/baaz/src/shot.rs` | render the window to a PNG and quit; uses the fake web backend and a throwaway state dir |
| `--no-connect --login signed-in` | `crates/baaz/src/main.rs` | an offline window with no provider child |
| `--replay <capture.jsonl>` | `crates/provider-muse/src/replay.rs` | fold a Muse capture through the live path, read-only, no child |
| `--bench`, `--perf-store`, `--perf-turn` | `crates/baaz/src/bench.rs`, `perf_budget.rs`, `perf_turn.rs` | timing runs |
| perf budgets | `scripts/perf-budgets.py`, `scripts/perf-budgets.json` | measure store, boot, open, switch, stream and turn costs against budgets |
| visual probe | `scripts/uiprobe.py` | the `uiprobe/1` adapter: about 120 named offline entries; Tier V diffs them against `design/baseline/` (see `CLAUDE.md`) |
| live checks | `scripts/baaz-check.sh`, `scripts/clone-leak-check.sh`, `scripts/journeys.sh` | scripted runs of a built app |
| tests | `crates/*/tests/`, inline `#[cfg(test)]` | fixtures under `fixtures/` (`msp`, `claude-code`, `codex`, `sidebar`, …) |

## 8. Known rough edges

These are true of the code today.

- **`Harness` is still the owner of nearly everything.** 241 fields and 52
  `impl` blocks. New features have added fields and files to it rather than
  their own entities.
- **Render is part of the event loop.** `on_frame` and `sync_browser` run
  inside `Harness::render` and reconcile overlay windows, provider status,
  webviews and tab persistence. Few things are observed, so a change is
  noticed when a frame happens, and the root is not cached.
- **`SidebarKey` is maintained by hand.** A field the sidebar reads but the
  key does not compare leaves the pane stale.
- **The app is not fully behind the seam.** About 730 references to a
  specific provider variant remain in `crates/baaz`, and
  `session/lane.rs` matches Claude Code control-card kinds by name.
- **Two `ProviderId` types** exist (the seam's alias and the app's own enum).
- **No process groups for agent children.** `claude`, `codex app-server` and
  `muse serve` are spawned with a plain `Command`; their own children are
  not killed with them.
- **`muse serve` is not shut down at quit.** Nothing calls
  `MuseHub::shutdown`; quit shuts down each view's lane, which ends Claude
  Code and Codex children only. After a Muse child exits, `reconnect` respawns
  it at once, with no backoff.
- **Not every state file is a `JsonStore`.** The caches, journals,
  `keymap.json` and `provider-paths.json` still call `write_atomic` directly
  and ignore its result in several places.
- **Some state never shrinks.** `sent/` and `handoff/` are not pruned, and
  `files_fts` in `search.db` is lost on a schema bump.
- **Stale prose.** `wire.rs` still says the legacy lane carries every
  command; `providers.rs` calls Muse "the legacy pump"; the `login.rs` header
  describes a login screen that no longer exists; the `ProviderAdapter::dispatch`
  doc says `Unverified` proceeds through the gate, which `Provider::send`
  refuses.
