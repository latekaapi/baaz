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
`provider-claude-code`, `provider-codex`, `provider-acp`, `muse-client`,
`muse-adapter`, `session-store` and `mcp-bridge`. `crates/baaz-cef-helper` is its own
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
| `provider-codex-pump`, `-stdin`, stderr, `codex-rate-limits`, `codex-effort` | `crates/provider-codex/src/child.rs`, `lib.rs` | one set per Codex child; stdin has one writer thread, so the pump never waits on a write |
| `provider-acp-pump`, `-stdin`, stderr | `crates/provider-acp/src/child.rs` | one set per ACP agent child; stdin has one writer thread |
| `pid-ledger-sweep` | `crates/provider-runtime/src/ledger.rs` | once at launch: ends the children a Baaz that died left running (only children proven its own) |
| `muse-bridge-N` | `crates/baaz/src/conn.rs` | one per lane: forwards a crossbeam receiver onto a futures channel the UI task awaits (the name is historical; every provider uses it) |
| `store:<file>` | `crates/baaz/src/store.rs` | one writer per `JsonStore` file, started on first save |
| terminal service | `crates/baaz/src/terminal/service.rs` | one listener plus one thread per socket connection; requests are queued and run on the UI thread by `TerminalService::drain` (a 15 ms pump while busy) |
| others | various | provider status probes (`provider_status.rs`), git (`git_ops.rs`), `utility.rs`, the stall watchdog, the log writer, quit signals, adblock load, the CEF clone sweep, the Muse billing-tier pty probe (`tier.rs`) |

Every provider event channel is an unbounded crossbeam channel. Every
provider child runs on one process runtime, `crates/provider-runtime`
(re-exported as `provider::runtime`): spawned in a new session and process
group, recorded in the pid ledger `<state>/run/children.json` (swept at
launch), hung up as a group (SIGTERM, SIGKILL after 250 ms), its stdout read
lossily with an 8 MB line cap, its stdin owned by one writer thread, its
requests held with per-request deadlines. Each pump folds one stdout line at
a time inside `provider::guard::guard_line`
(`crates/provider-runtime/src/guard.rs`): a panic in a fold is caught, the
poisoned lock is cleared and the session ends as `ConnectionLost`, so the
next send revives it instead of the row reading Working forever.

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
| `provider-acp` | any Agent Client Protocol agent (docs/82 5.7): one child per session over JSON-RPC stdio (`initialize`, `session/new`/`load`/`prompt`/`cancel`, `session/update`, `session/request_permission`), its capabilities from the agent's `initialize` answer. The agent is the command line in `BAAZ_ACP_AGENT`; unset, the provider is not offered. Tested from hand-written `fixtures/acp/*.jsonl` on a stand-in only |
| `transcript-core` | `TranscriptBuilder`, the one transcript builder under every fold (docs/82 5.3, 5.7): a fold names blocks by stable id (`tool:<id>`, `approval:<id>`, …) and the builder owns their positions and emits the positional `Delta`s; it also owns the tool-verb table, the baaz terminal-tool cards, diffs and their stats, the todo states, approval cards and child-loss settlement. Depends on `aui-protocol` (and `serde_json`) only |
| `session-store` | `SessionStore`: one session's transcript folded from neutral deltas, plus its side facts. Pure; every `SessionRuntime` holds one |
| `mcp-bridge` | a stdio MCP server exposing Baaz's terminal and browser tools to an agent, forwarding to the app's socket |

Each adapter crate registers itself (docs/82 5.4): a `ProviderDescriptor`
(key, label, icon, binaries and env override — or, for a provider the person
configures, the env var holding its command line — install and update
commands, docs, version floor, account label, declared modes and traits) and
a `ProviderFactory` that opens lanes from a `SessionSpawn` (state dir, owner
home, MCP servers, owner-MCP opt-in) and folds its own recordings for
`--replay`; both ride one `provider::Registration`.
`crates/baaz-core/src/providers.rs` is the registry: its `REGISTRATIONS`
table lists one line per adapter (ACP joined with one, docs/82 5.7),
`ProviderId` is a handle on one entry by key, and the window's `Registry`
holds the factories — Muse's is also its hub's slot
(`SharedChild`), so a Muse open waits for the hub while Claude Code and
Codex spawn a child with the terminal relay and their Baaz-owned home
pinned. The app asks for traits (`ProviderTrait`), never "is it Muse"; an
unknown provider key is an error. `crates/baaz/tests/seam_ratchet.rs` keeps
`muse-client` out of the app crate, `ProviderId::Muse` out of shipped
`crates/baaz` code, and every adapter crate behind the registry (listed
exceptions only).

## 4. Entities and ownership

Production `Render` impls: `Harness` (`app.rs`), `SessionView`
(`session/render.rs`, over a `SessionRuntime` that draws nothing), `SidebarPane` (`sidebar_view.rs`, a view of its own
since task 4.7), the region
entities `RightPane` (`right_pane.rs`), `TerminalDock`
(`terminal/dock.rs`), `SkillsPage` (`skills_page.rs`), `SettingsPage`
(`settings.rs`), `ConnectScreen` (`connect.rs`) and `BrowserView`
(`browser/view.rs`), the setup sheet's
`SetupHost` (`setup.rs`) and `OverlayView` (`overlay.rs`); `BenchRoot` is bench-only. All paths in this section are under `crates/baaz/src/`.

### `Harness` — the window root

`Harness` (`app.rs`) has 45 fields. Its methods are spread over 62
`impl Harness` blocks in 53 files (`app/*.rs` — the sidebar's root half is
`app/sidebar_host.rs` — the
`browser/` seam files, `dialogs.rs`, `login.rs` and others). Roughly:

| group | examples |
|---|---|
| providers, sign-in, onboarding | `providers` (the window's `Registry`: factories and the shared child's slot, docs/82 5.4), `wire`, `auth`, `tier`; `login` (the `Login` entity, `login.rs`, task 4.9c: the sign-in flow's state and the API-key field, drawn by the Muse sheet, which redraws on its notify — the root only drives the `account/*` lane); `provider_model` (`ProviderStatusModel`, `provider_model.rs`, task 4.4: the statuses, the sign-out banners, the cards waiting on a press and the Codex sign-in, read and observed by every surface that shows them); `connect_screen` (a region entity, `connect.rs`) and `setup` (the setup sheet's entity, `setup.rs`) |
| session list | `session_list`: the `SessionList` model entity (`session_list.rs`, task 4.7) holding the rows, the stores they are joined from (`index`, `overrides`, `provider_sessions`, `projects`, `branches`), the list filters, the fetch state, the title bookkeeping and the caches (visible rows, grouping, chain index); a change emits `SessionListEvent` and notifies the list, never the root. `Harness` drives the fetch and the joins and reads through `session_list.read(cx)` |
| live session views | `active`, `registry`, `pending_id`, `drafts`, `unsent`, `starts_in_flight`, the switch/replace epochs and claims |
| handoff | `handoffs`, `handoff_epoch`, `pending_handoff`, `handoff_confirm` |
| sidebar | `sidebar_pane`: the `SidebarPane` view (task 4.7), which owns its scroll, hover and hover card, reveal, wheel, fade and Pinned disclosure, and (task 4.9c) the divider's width and drag (`ResizeDrag`, `resize.rs`); the `rename` field it shares with the header |
| right pane | `right_pane`: a region entity (`right_pane.rs`, task 4.3) that owns its cache, divider, refresh, watch, focus and menus |
| browser and Browse mode | `browser`: the `BrowserView` region entity (`browser/view.rs`, task 4.5) holding the tabs and webviews, the Field and find inputs, the Browse sidebar's menus and drags, three of the four overlay windows (`overlay`, `corner`, `page_float`: one slot each, holding the window once its open task lands — no `*_pending` twin), `data` (the `browser.db` service), `occlusion` (the one `NativeOcclusion` owner) and `agent_window` (`AgentWindowState`, `app/agent_window.rs`: the agent line and card and the card's float; the handoff picker is a `Picker` view on the overlay stack) |
| settings, skills, palettes | `settings_page` (a region entity, `settings.rs`, task 4.4, that owns its route, search, focus, the Shortcuts rows and recording), `skills_page` (a region entity, `skills_page.rs`, task 4.4), `import`, the search palette's `TextareaState` query field (the search reads it; the Commands and Projects palettes own theirs since task 4.6b) |
| terminal | `terminal_dock`: a region entity (`terminal/dock.rs`, task 4.4) that owns the tab host, its focus, the resize drag and the bridge providers; `terminal_service`, the agent's terminal and browser tools socket |
| focus, capture, plumbing | focus handles, `capture`, `args`, `tasks`; `window_sync` (`WindowSync`, `app/window_sync.rs`, task 4.9: the window's handle, the queued settle and floats passes, the composer's one-shot focus arm and the observations that drive them) |

`Harness::render` is not cached, and it composes: it reads state and builds
elements, and it asks the one occlusion owner what covers the native pages
(`Harness::occlude_pages`, `browser/occlusion.rs`: decides and applies, in
memory). Its one instrument is the env-gated frame trace row. Nothing it
reaches in `app.rs` / `app/**` writes a file, opens a window, moves the
keyboard, touches sqlite or sets the title (`tests/render_ratchet.rs`, task
4.9). What a per-frame hook used to do runs from its cause
(`app/window_sync.rs`): the **settle** — the provider statuses, the
scripted boot and steps, the composer's focus arm, the URL field's owed
focus, the palette's keyboard and the overlay stack's restore, the agent
line — runs once at the end of any effect cycle in which the root, the
overlay stack, the Browse view, the right pane, the connect screen or the
layout changed, or the window was (de)activated; the **floats** (the
overlay, corner and Page float windows over the native page) follow on the
next frame, also queued by the Browse view's events (its pages, a moved page
rect), the window's bounds and the browser chat's turn; the **title**
follows the session list's events, the switch path and the project path;
focus lost hands the keyboard back at the end of the frame that unmounted
it; a `--replay` capture opens when the root is built. The browser's page reconcile (tabs,
history, popups, downloads, permissions, the tabs write) does not run from
render: `BrowserView` observes each page and runs it on the page's change
(`browser/sync.rs`, `tests/browser_render_ratchet.rs`). The right pane
is a region entity ([`region.rs`](../crates/baaz/src/region.rs)): the root
pushes it this frame's facts and composes its view, and the pane reconciles
its own reads in its own render. The terminal dock, the Skills page, the
Settings page (its section list in the sidebar column, its crumbs and close
in the bar, its surface in the centre) and the connect screen are composed
the same way.
The sidebar and the transcript are `.cached()` subtrees, so they skip work
while clean (except while assistive tech is on). The sidebar is re-rendered
by its own inputs only: the session list's events, the layout global, its
own hover and scroll, and the facts the root pushes as it composes the shell
(`Harness::compose_sidebar`: the open session, the footer, the open menus,
Browse's rows — a notify only when they moved). The window's lifecycle does
not touch it (`tests/sidebar_view_ratchet.rs`).

**Who notifies (task 4.9b).** A state change redraws through the entity
that owns it: the owner's mutation method changes the state and notifies
once, and callers do not notify after it. gpui-pre invalidates every window
whose last frame read a notified entity, and the root is never cached, so a
notify of anything the root reads each frame — the overlay stack, the
browser, the right pane, the Settings and Skills pages, the session list,
the connect screen, the provider model, the terminal dock — re-renders the root without the root being told
(`sidebar_view::an_owner_the_root_draws_redraws_it_without_a_root_notify`).
Task 4.9c moved the rest of the clusters: the sign-in flow is the `Login`
entity, the sidebar drag lives on `SidebarPane` (a child view's notify
dirties its ancestors, the root included), a layout write reaches the root
through its `LayoutStore` observer, and the runtime names its changes as
`RuntimeEvent`s (below)
(`sidebar_view::the_login_flow_redraws_its_sheet_without_notifying_the_root`,
`sidebar_view::a_sidebar_drag_redraws_the_shell_without_notifying_the_root`).
The root notifies only for its own fields. `tests/notify_ratchet.rs` refuses
a notify hidden behind a helper, a macro or another spelling, and holds the
count to a ceiling that only falls.

### `SessionRuntime` and `SessionView` — one session (task 4.8)

A session is two entities. `SessionRuntime` (`session/runtime.rs`, no
`Render`) is the model: its `SessionStore`, the lane (the `Provider` handle
behind its mutex and the drain task), the turn state (sending, running, the
queue, the optimistic bubbles, aliases), approvals and questions, the model,
effort and mode picks, plan mode, the outgoing attachments (every submit
path reads them, headless ones included), the prompt history, the banners,
the clocks, the start queue and the handoff state. `SessionView`
(`session.rs`, 68 fields) draws one runtime: the composer and the card
fields, the virtual-list bookkeeping and render caches, folds, spans and the
quote flow, the `@` rank (`MentionRank`), and the overlays handle. It holds
`Entity<SessionRuntime>` and redraws on every `RuntimeEvent` (never the
root), mapping the transcript's to one list operation
(`EntriesAppended` / `EntryUpdated(ix)` / `EntriesRemoved` /
`ApprovalRequested` → refresh from the change and keep the tail;
`ApprovalDecided` closes the field open on it; `CardsChanged` rebuilds the
card cache; `MenusStale` re-reads the open menu's check; `Toast`). The
runtime's own methods never `notify` (task 4.9c): the facets beside the
transcript are named too — `TurnChanged` (sending, running, the queue, a
stop), `Ticked` (the turn's clock, a countdown), `BannerChanged`,
`ComposerChanged` (attachments, effort, plan, a prompt handed back),
`QuestionsChanged`, `HistoryChanged` (the loading row) and
`HeaderChanged`. The view still observes the runtime, because a write made
from outside through `Entity::as_mut` notifies it as the borrow drops. The
person's intents go the other way: the view calls the runtime (submit,
interrupt, decide, answer, picks). Both halves' methods live under
`session/`: `lane.rs` (the provider lane), `commands.rs`, `composer.rs`,
`approvals.rs`, `questions.rs`, `shell.rs`, `clocks.rs`, `handoff.rs`,
`events.rs`, `scripting.rs` (each file's `impl SessionRuntime` and
`impl SessionView` blocks side by side) and `render.rs` (view only).

The lane (`session/lane.rs`) is fixed per runtime: `Lane::Opening` while a
new child answers `OpenSession`, then `Lane::Provider` exactly once. One gpui
task per runtime drains its event stream and calls `on_provider_event`, so
the store has one writer, with or without a view. Dropping the runtime (never
a view) calls `shutdown_lane`, which hangs up a Claude Code or Codex child.
The runtime reports upward by emitting `SessionEvent`, which
`Harness::on_session_event` (`app/sessions/events.rs`) handles; the view
emits the same type for what the person asked of the window (links, slash
commands, picks). A fold batch that lands with nothing in flight — a
reopen's replay — settles its finished turns in one
`SessionEvent::HistoryReplayed`, which the root settles once
(`settle_turns`: one seed compare-and-write, one batch of ledger rows, one
row rejoin; docs/81 A F14).

### `SessionRegistry` — every live session

A session lives in one of three places: the centre (`Harness::active`, a
view, still a field passed in), the parked MRU, or the chat on the Browse
agent line. `SessionRegistry` (`app/registry.rs`) owns the last two — each
parked entry a runtime with its view, or with none once evicted — and one
subscription per live runtime plus one per live view. Every question about
all sessions goes through it: `live` (views), `runtimes`, `find`,
`find_runtime`, `iter_running`, `resubscribe`, `shutdown_all`. It caps
parked *views* at 8 (`SESSION_CACHE_LIMIT` in `app/sessions/open.rs`);
drafts keep theirs. Past the cap a session with work in hand
(`SessionRuntime::must_stay_alive`: a turn running or starting, a pending
approval or question) loses only its view — its runtime runs on, settles
its row and lands its ledger row — and an idle one goes whole. Reopening a
view-less session builds a view over its runtime (`cache_take`). Quit
shuts runtimes down, not views.

### The rest

| owner | file | owns |
|---|---|---|
| `SidebarPane` | `sidebar_view.rs` | the sidebar column (task 4.7): the virtual list's state and its sync, hover and the hover card's arm, the one-shot reveal, the wheel accumulator and scripted sweep, the mode-switch fade, the Pinned disclosure, the group menus' seats, and the divider's width and drag (task 4.9c: a press and a move notify the pane, a release persists through the layout global). It subscribes to `SessionList`, observes the layout global, takes `SidebarFacts` from the root and emits `SidebarEvent`s the root maps (`app/sidebar_host.rs`); it holds no handle to the root |
| `SessionList` | `session_list.rs` | the session list as a model (task 4.7): rows, stores, filters, fetch state, title bookkeeping, the visible/grouping/chain caches keyed on the root's `ListFacts`, and the joins (`rejoin_scoped`, `merge_provider_rows`, `install_provisional_rows`) |
| `Overlays` | `overlays.rs`, `overlay_stack.rs` | its mutations notify (task 4.9b): `push`/`close`/`pop`/`set`, the menu, sheet and toast methods each change the stack and notify once when something changed, so no caller notifies after them; it also keeps the shell menus' anchors (`note_anchor`: the header `…` and the project menu). State: the overlay stack (task 4.6a) — every dialog, palette, menu, popover, sheet and inline rename is one typed entry carrying `covers_page`, `takes_keyboard`, `host` (the main window, an overlay panel, or — the dialog and the palette — wherever the mode puts them), its owner and its close hook; Escape pops it (`Harness::pop_overlay`), `Covers` reads its `covers_page`, and closing an entry gives back the focus it took (applied in the root's frame) — plus toasts and the `@`-picker walks. Since task 4.6b an entry may hold the view that draws it: the dialog (`DialogView`, `dialogs.rs`), the Muse sheet (`MuseSheetView`, `login.rs`), the setup sheet (`SetupHost`), the import and Chrome-import sheets (`ImportSheetView`, `ChromeImportView`), the bookmark popover (`BookmarkView`, `overlay/bookmark.rs`) and the pickers; each emits `DismissEvent`, which pops the stack newest-first (`Harness::dismiss_overlay_view`). The root pushes their facts in each frame and maps their typed events. The rest are drawn by `Harness`, the composer, the right pane, the Settings and Skills pages and the connect screen, each reading its own entry |
| `Picker<D>` | `picker.rs` | the one list picker (task 4.6b, Zed's `picker` read for the shape): query (its own field, a borrowed one, or typed elsewhere), selection, ↑ ↓ ↩ Escape, hover; a `PickerDelegate` gives the rows, the narrowing, the pick and the look (the library's own component). Delegates: the palette (`palette.rs`: ⌘K, `/resume`, `/fork`, search, Projects), the model picker and the `@` mention (`session/pickers.rs`), the hand-off picker (`app/browser_handoff.rs`) |
| `OverlayView` | `overlay.rs` | the borderless panel windows that float above the native page (the page draws above all gpui content); what they show follows the overlay stack's panel-hosted entries |
| `SettingsPage` | `settings.rs`, `settings_providers.rs` | the Settings page as a region entity: its route (`SettingsPageState`), search and focus, the Shortcuts rows and recording, its menus and folds; the root pushes `SettingsFacts` and handles `SettingsEvent`s |
| `BrowserData` | `browser/data.rs` | `browser.db` (task 4.5): the in-memory front (bookmarks, folders, excluded sites, history length, the Field's last history search) the UI reads, and the one worker task on the background executor that owns the connection; writes update the front at once and persist in order, landings notify; quit drains the queue, bounded (`drain_at_quit`); observed by `BrowserView` |
| `ProviderStatusModel` | `provider_model.rs` | the window's provider facts: statuses (following the status cache), sign-out banners, busy cards, the Codex sign-in flow; observed by `ConnectScreen`, `SetupHost` and `SettingsPage` |
| `ConnectScreen`, `SetupHost` | `connect.rs`, `setup.rs` | the first-run connect screen (a region entity: whether it stands; its footer menu is an overlay-stack entry) and the setup sheet (its terminals and focus; the open sheet is an overlay-stack entry) |
| `SkillsPage` | `skills_page.rs` | the Skills page as a region entity: catalog, filter, selection, dialogs, its fields and CLI calls |
| browser | `browser/` | `BrowserView` (`view.rs`: a region entity; one `WebviewState` per tab, CEF or WKWebView, a fake backend for captures; the Browse fields; it observes its pages, runs the page reconcile in `sync.rs` and emits `BrowserEvent`s the root maps; it draws the centre's loading line), `NativeOcclusion` (`occlusion.rs`: what covers the native page, which page shows, the holes, cover settling), the `impl Harness` seams (`tabs_actions.rs`, `pages.rs`, `sync.rs`, `occlusion.rs`, `omnibox.rs`, `find.rs`, `permissions.rs`, `downloads.rs`, `page_actions.rs`, `keys.rs`, `steps.rs`, `agent_tabs.rs`; `mod.rs` holds no `impl Harness`), tabs, `BrowserData` (`data.rs`: history and bookmarks in `browser.db`, run by one worker on the background executor behind an in-memory front; nothing else names SQLite, `tests/browser_data_ratchet.rs`), adblock, favicons, the agent tools in `browser/automation/` |
| terminal | `terminal/` | `TerminalDock` (the dock region entity), `TerminalHost` (an entity the dock holds: tabs by project root over `aui_terminal` ptys) and `TerminalService` (the socket, on `Harness`) |

Every action a browser agent takes passes `browser::automation::policy::decide`
(`browser/automation/policy.rs`), with no model in the loop: a site rule
first, then the person's-tab rule, then the always-ask kinds (Send, Pay,
Delete, Upload, Sign in), then the Ask mode.

## 5. State on disk

`<S>` is `~/Library/Application Support/baaz`, or `BAAZ_STATE_DIR` when set.
Unit tests and deterministic captures that set no dir use a per-process temp
dir. `store::migrate_legacy_support_dir` moves the pre-rename directory once.

`baaz.db` (`crates/baaz-core/src/db/`, task 4.12) holds every fact Baaz keeps
about a session: one `session` row per session keyed `(provider_key,
provider_session_id)` with Baaz's own uuid, its `turn` rows (the usage ledger
and the bubble text a submit showed), its `attachment` rows keyed by the user
turn, the prompt history, and the import and migration journals. One writer
thread owns every connection; work reaches it as a queued job, so the UI
thread never touches SQLite (`db::check_off_ui`). Migrations step
`PRAGMA user_version`; the file is in WAL mode; a multi-row write is one
transaction. The first open of a state dir's database imports the JSON stores
it replaces (`db/json_import.rs`, the only reader of those files) in one
transaction with its marker, and never renames, rewrites or deletes them: they
stay as the backup. The window keeps the session list in memory as two maps
(the facts per session, the provider record per session) and writes a
snapshot back; the writer writes only the rows that changed.

`JsonStore<T>` (`crates/baaz-core/src/store.rs`) is the one way the remaining
JSON files are written. Each store has one writer thread. `save` takes the generation and
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
| `baaz.db` | `db/` (one writer thread) | SQLite, WAL, `user_version` steps | `session` (facts, titles, record: provider, workspace, turns, origin), `turn` (usage ledger, bubble texts), `attachment`, `prompt_history`, `import_job`/`import_item`, `migration_journal`, `account` and `worktree` (empty until something fills them), `meta`. Canonical |
| `session-meta.json`, `session-cache.json`, `sessions.json`, `provider-sessions.json`, `history.json`, `import-journal.json`, `session-migration-journal.json`, `sent/<id>.json` | (retired) | read once by the first-launch import; never written or renamed (a forgotten session's `sent/<id>.json` goes with it) | the backup of what `baaz.db` imported |
| `projects.json` | `projects.rs` | JsonStore, with a migration | projects, defaults, last picks. Canonical |
| `provider.json` | `providers.rs` | JsonStore | the last provider picked |
| `layout.json` | `layout.rs` | JsonStore | window layout, mode, right pane, browser and agent prefs |
| `Browser/tabs.json` | `browser/tabs.rs` | JsonStore (`save_soon`) | open tabs and spaces |
| `search.db` | `search.rs` | SQLite FTS5 | `sessions_fts` (regenerable) and `files_fts` (files agents wrote; refilled only as turns complete) |
| `Browser/browser.db` | `browser/history.rs`, `browser/bookmarks.rs` | SQLite | browsing history and bookmarks |
| `Browser/Chromium/`, `Browser/cef.log`, `Browser/adblock/` | CEF, `browser/adblock.rs` | the engine's own | browser profile, log, filter lists |
| `tier.json`, `provider-status.json`, `index-cache.json`, `session-list-cache.json`, `provider-paths.json`, `claude-code-last-model.json`, `keymap.json` | `tier.rs`, `provider_status.rs`, `index.rs`, `list_cache.rs`, `details.rs`, `providers.rs`, `keymap.rs` | `write_atomic` directly | probe and list caches (regenerable), the path overrides and the keymap (authored) |
| `session-migration.consented`, `.dismissed` | `session_migration.rs` | marker files | the migration's consent and dismissal |
| `onboarding_completed` | `provider_status.rs` | marker file | first-run flag |
| `sent/<id>/` | `sent.rs` | files | the pictures each turn was sent with (the rows are `attachment` in `baaz.db`); removed when the session is forgotten |
| `handoff/<dest>.json` | `handoff_snapshot.rs` | `write_atomic` | the source transcript copied at handoff; removed when the destination is forgotten |
| `claude-home/`, `codex-home/` | the CLI children | the CLIs' own | Claude Code and Codex transcripts and config (with links into `~/.claude` / `~/.codex` for auth and settings). Canonical transcripts for those providers |
| `mcp/` | `terminal/relay.rs` | files | per-session MCP configs for Claude Code |
| `run/terminal-<pid>.sock`, `lock` | `terminal/service.rs`, `instance.rs` | socket, flock | runtime only |
| `side-sessions/`, `browser-chats/`, `agent-shots/`, `newtab/`, `favicons/`, `tier-probe/` | various | files | working dirs for side sessions and the browser chat, agent screenshots, caches |
| `logs/baaz.log` | `log.rs` | append, rotated | the log; the watchdog writes here too |

Outside `<S>`, Baaz reads Muse's `~/.local/share/muse/session-index.db`
(read-only, `index.rs`) and `~/.config/muse/auth.json` (`auth.rs`). New
sessions with no project default to `~/baaz-sessions`
(`projects::default_workspace`).

## 6. One turn, end to end

1. **Composer.** The view's send hands the words to the runtime, whose
   submit builds `Command::SubmitInput` (`session/commands.rs`) and calls
   `SessionRuntime::provider_send` (`session/lane.rs`).
2. **Off the UI thread.** `ProviderCall::provider_call` (`wire.rs`) runs
   `Provider::send` on the background executor under the runtime's
   `Arc<Mutex<Provider>>`. The gate checks the capability, then the adapter's
   `dispatch` writes to the child's stdin (Muse: through the hub's writer).
3. **Ack.** The completion returns through `update`. On success the runtime
   emits `SessionEvent::ProviderTurnAccepted`; on refusal it shows a banner.
4. **Events.** The child writes output; the adapter's pump folds each line
   into `ProviderEvent`s (Muse: `muse-reader` → hub fold → per-lane channel).
5. **Bridge.** The lane's `muse-bridge-N` thread moves them onto a futures
   channel; the runtime's lane task awaits it on the UI thread.
6. **Fold.** The runtime's `on_provider_event` handles taps (approvals,
   questions, banners) and passes `Deltas` to `SessionStore::apply_deltas`,
   then says which entry moved (`RuntimeEvent`). The view's handler redraws
   the view, so an event needs no notify of its own; a change beside the
   transcript (a banner, a chip, a clock) is an event too (`BannerChanged`,
   `ComposerChanged`, `Ticked`, …).
7. **Render.** The view over it (if any) maps the event to one list
   operation and its cached subtree re-renders; the root is not notified.
   `transcript.rs` maps turns to `aui` elements on a virtual list.
8. **Settle.** `TurnSettled` ends the turn; the runtime emits
   `ProviderTurnOutcome` and `ProviderTurnFinished` (usage ledger row,
   title, byline) — or, for a reopen's replay, one `HistoryReplayed`.

**How a sidebar row learns.** The registry's subscription delivers each
`SessionEvent` to `Harness::on_session_event`, which updates the row in
the `SessionList` (`sync_row_live` in `app/list.rs`, `record_turn_outcome`)
and the overrides. The list says so (`SessionListEvent::RowChanged` or
`Changed`), and the cached `SidebarPane`, subscribed to it, re-renders. For Muse, the hub's connection stream
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
- **Render still decides some things per frame.** The per-frame hook left
  render in task 4.9, but the root is not cached, the native pages'
  occlusion is still decided (and applied) as the root composes, the facts
  only the root knows are still pushed into the sidebar and the right pane
  during its render, the pane's compose still builds the session's webview
  on first show, and the frame trace writes its row there. The floats ride
  the next frame after a change rather than the change itself, so a window
  that takes no frame (hidden, occluded) holds them where they were.
- **The app is not fully behind the seam.** About 730 references to a
  specific provider variant remain in `crates/baaz`, and
  `session/lane.rs` matches Claude Code control-card kinds by name.
- **Two `ProviderId` types** exist (the seam's alias and the app's own enum).
- **No process groups for agent children.** `claude`, `codex app-server` and
  `muse serve` are spawned with a plain `Command`; their own children are
  not killed with them.
- **`muse serve` is not shut down at quit.** Nothing calls
  `MuseHub::shutdown`; quit shuts down each runtime's lane, which ends Claude
  Code and Codex children only. After a Muse child exits, `reconnect` respawns
  it at once, with no backoff.
- **Not every state file is a `JsonStore`.** The caches, journals,
  `keymap.json` and `provider-paths.json` still call `write_atomic` directly
  and ignore its result in several places.
- **Some state never shrinks.** `sent/` and `handoff/` lose a session's
  files only when it is forgotten, and `files_fts` in `search.db` is lost on
  a schema bump.
- **The window's session list is keyed by the bare session id.** `baaz.db`
  keys a session by provider and id, but the in-memory maps (and every
  holder of a session id in the window) still use the id alone; with two
  rows for one id the list shows the one its record names.
- **Stale prose.** `wire.rs` still says the legacy lane carries every
  command; `providers.rs` calls Muse "the legacy pump"; the `login.rs` header
  describes a login screen that no longer exists; the `ProviderAdapter::dispatch`
  doc says `Unverified` proceeds through the gate, which `Provider::send`
  refuses.
