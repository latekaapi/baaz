# The app — shell, sessions, streaming

Phase 2 of the spec (`docs/00-spec.md` §5). `crates/harness` is the gpui
application: the window, the auth screen, the sessions sidebar, the transcript
and the composer. It sits on `muse-client` for the wire, `muse-adapter` for the
fold, and the `aui` component library for everything visible.

```
crates/muse-client    the `muse serve` child, NDJSON JSON-RPC        (phase 1)
crates/muse-adapter   MuseFold: MSP view events -> aui_protocol      (phase 1)
crates/harness        the gpui app                                   (this doc)
```

---

## 1. How to run it

```sh
export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"

cargo run -p harness                                  # workspace = $PWD, provider = meta
cargo run -p harness -- --workspace ~/code/thing      # a different workspace
HARNESS_PROVIDER=echo cargo run -p harness            # routed through echo — still a real turn
cargo run -p harness -- --replay fixtures/msp/transcript-approve.jsonl   # free
```

> **`echo` is not a free provider.** On a signed-in machine the session log
> records `provider_id: echo` at intake and then a metadata record naming
> `provider_id: meta, model_id: muse-spark-1.3-contributor`; the turn bills
> reasoning tokens and answers with real text. `--provider` picks a route, not a
> bill. Only `--replay` and `--no-connect` cost nothing — plus `session/start`
> and `session/userShell`, which make no model call. See `01-transport.md` §6.

| flag | meaning |
|---|---|
| `--workspace <path>` | the workspace every session in this window runs in; defaults to `$PWD`. `~/` is expanded. |
| `--provider <id>` | overrides the provider; `meta` by default. |
| `--theme light\|dark` | the theme the window opens in; dark by default. |
| `--session <id>` | reserved for opening a named session at boot. |
| `--screenshot <png>` | render the window off-screen once it settles, save a 1× PNG and quit. |
| `--screenshot-delay <ms>` | how long to wait first (default 600). |
| `--no-connect` | render the chrome without spawning `muse serve` — what a login-screen capture wants. |
| `--replay <capture.jsonl>` | fold a checked-in wire capture and render it, with no child process at all (implies `--no-connect`). Commands against a replayed session are refused with a banner. Free. |
| `--bench <capture.jsonl> [--bench-cadence-ms <ms>] [--bench-scroll top\|mid\|tail\|sweep\|wheel] [--bench-frames <n>] [--bench-out <file.json>]` | stream the capture through the fold on a timer while driving the transcript list, and print element / frame / fold-apply timing plus peak RSS (§6). Free: no child, no server. Cadence defaults to 4 ms, scroll to `sweep`, frames to 600. Implies `HARNESS_FRAME_STATS`. |
| `--steps <a;b;c>` | drive the open session from the command line, so a screenshot is reproducible (`docs/03-composer.md` §1, `docs/04-approvals.md` §7). Scripting only — its full verb table, with which steps cost a turn, lives in `main.rs`'s `Args::steps` doc comment; only `send:` and `steer:` bill. |
| `--login <state>` | which login-screen state `--no-connect` boots into for a capture: `choose` (the default), `device`, `apikey`, `apikey-error`, `validating` or `error`. Sample data only. |
| `--login-steps <a;b;c>` | drive the login screen from the command line, once the login screen is up on a live connection (never with `--no-connect` / `--replay`). After sign-in the ordinary `--steps` run as today. |

`--login-steps`, one step per item, the same `;`-separated parsing as
`--steps`:

| step | what it does |
|---|---|
| `account` | start the device flow (the browser opens) |
| `apikey` | open the API-key form |
| `key-from-env:<VAR>` | put the value of environment variable `VAR` into the API-key field |
| `submit` | submit the API-key form |
| `wait:<ms>` | let the wire catch up before the next step |

The key travels from the environment into the field and then into the wire
call: it never appears in argv, a log or a screenshot argument. If `VAR` is
unset the step fails with a stderr line naming the variable, not its value.
Every login-state transition prints one stderr line (`harness: login →
<state>`, `harness: account → <lane>`, no secrets), so a headless run can be
followed from a log: `--login-steps
'apikey;key-from-env:MUSE_TEST_KEY;submit;wait:8000' --screenshot …` captures
the signed-in shell on the API-key lane.
| `--approval-mode <mode>` | the mode every session this window **starts** in. Not the same as the `setmode:` step: `session/start` is the only surface that declares a session's policy, and on this server `session/setApprovalMode` does not reach `promptUnmatched`. |
| `--tier subscription\|payg\|unknown` | fake the billing probe, for a screenshot of the guard (`docs/06-billing.md`). |
| `--print-tier` | probe the billing tier, print it and exit, without opening a window. Free. |

| environment | meaning |
|---|---|
| `HARNESS_PROVIDER=echo` | route turns through `echo`. **Not free** — see the note above; it is simply the cheapest route and the one scripted runs default to. The spec caps real turns at five per phase and every provider's turns count. |
| `HARNESS_MUSE=<path>` | the `muse` binary to drive; `muse` on `PATH` otherwise. |
| `HARNESS_DETERMINISTIC=1` | freeze the clocks and draw every card settled, so a `--replay … --screenshot` capture is byte-identical run to run. One clock (`src/clock.rs`): sidebar grouping/elapsed read "now" once per frame — under the flag "now" is the newest `updated` in the data, so the newest row reads `now` however old the fixture is — and every `Instant` behind a label or countdown is frozen, so elapsed cells vanish and countdowns show their full duration. The boot holds the platform's reduced-motion switch, so every motion primitive (spinner and shimmer loops, tweens, enter presence, springs, the streaming caret) resolves to its resting state — including components with no `at_rest` of their own, like the library's `StatusRow` or a pending approval's header spinner. On top of that the harness passes `at_rest` everywhere it constructs an animated component (turn reveals, the login card, dialogs, toasts, palettes, all composer menus, approval cards, suggestion chips); a capture never takes keyboard focus, so the composer's blinking caret (which honors no motion switch) never paints. |

The window is 1440×900 and titled **Harness**. It boots exactly as
`aui/examples/minimal.rs` does — `gpui_kit::application().with_assets(AuiAssets)`,
then `aui::init(theme, cx)`, then `AuiTheme::set_text_scale(scale::TEXT_SCALE)`,
then the window — because that order is the library's contract.

---

## 2. Entities

Three, as spec §2.3 asks for.

| entity | file | owns |
|---|---|---|
| `Harness` | `src/app.rs` | the `MuseClient`, the auth state and the login flow, the session list, the active session, the dialog, the shell |
| `SessionView` | `src/session.rs` | one Muse session: its `MuseFold`, the composer draft, the scroll position, which cards are folded, the running turn |
| `Overlays` | `src/overlays.rs` | state only: the modal, the open menu and its selection, the palette, the toasts, and the two lists the menus are built from |

Each entity keeps its **fields** in the file above and its methods next door,
one sibling module per section seam (C1 and C2, 2026-09-12). `docs/07-architecture.md` §2 is
the full map; the short version:

| file | owns |
|---|---|
| `src/app/lifecycle.rs` | the index, the list, and starting / resuming / opening a session |
| `src/app/list.rs` | rename, pin, hide, archive, clear-empty, undo |
| `src/app/find.rs` | the full-text search palette's query and rows |
| `src/sidebar_view.rs` | the sidebar column and the two popovers anchored to it |
| `src/dialogs.rs` | the modal, the palette, the toasts, the overflow menu |
| `src/billing.rs` | the tier probe's lifecycle and its banner |
| `src/resize.rs` | the sidebar divider's drag |
| `src/login.rs`, `src/steps.rs`, `src/wire.rs` | login, scripting, call-then-update |
| `src/session/*.rs` | one seam each: `events`, `commands`, `composer`, `approvals`, `questions`, `shell`, `clocks`, `scripting`, `render` |

Everything else is a pure function. `src/transcript.rs` turns a `Turn` or a
`Block` into elements — one `*_card` function per block kind, with `block` as
the dispatch; `src/sidebar.rs` turns the session list into the library's date
grouping; `src/conn.rs` classifies errors; `src/auth.rs` and `src/index.rs` are
I/O with no UI in them at all.

Rendering is a pure function of state each frame, and a frame decides before it
draws: `Harness::on_frame` is the one pre-pass that writes (window title, a
resize left armed, the deferred session swap, the one-shot composer focus), and
`SessionView::sync_render_cache` / `sync_virtual_list` are the only writes in a
transcript frame. Everything below them reads the cache — never the live fold —
so one frame draws one consistent snapshot. Nothing is cached between frames
except the scroll position, the fold set and that snapshot.

---

## 3. Thread model, and how an event reaches the UI

One `MuseClient` per process, held behind an `Arc` on `Harness`. The client
already runs its own reader and writer threads, so the pipe never touches the UI
thread. Two directions cross the boundary:

**Events in.** `MuseClient::events()` is a `crossbeam` receiver, which a gpui
foreground task cannot await. `conn::connect` therefore starts one bridging
thread — `muse-bridge` — that blocks on `recv()` and forwards to a `futures`
channel. `Harness::pump` is a single foreground task that drains that channel
with `StreamExt::next` and calls `SessionView::apply` for each event. So folding
happens **on the UI thread, in wire order**, and a frame always renders a
consistent transcript.

```
muse serve ──stdout──▶ muse-reader ──▶ crossbeam ──▶ muse-bridge ──▶ futures mpsc
                                                                          │
                                                              Harness::pump (foreground)
                                                                          │
                                                        SessionView::apply ──▶ MuseFold
```

**Commands out.** Every MSP request blocks — up to three minutes — so every one
of them runs on `cx.background_spawn` and returns to the entity through
`update`. The UI thread issues intents; it never waits on the wire. That is why
`send`, `interrupt`, `session/start`, `session/resume`, `session/list`,
`model/list`, the `account/*` calls and the sqlite read all have the same shape: a
background task, then one `update` that folds the answer in.

**Ack ≠ outcome.** Nothing gates folding on an ack. `turn/start` comes back with
a `turnId` and a disposition, and that is all it is used for: the identity, and
whether the submission was queued. What the turn is *doing* is always the view
event.

---

## 4. Auth (superseded: `docs/diagnosis/login.md`, D22–D29)

Sign-in is on the wire. `conn.rs` sets `experimentalApi: true` at
`initialize` (without it every `account/*` method answers `-32601` /
`experimentalRequired`), and the boot probe is a single `account/read`:
`loggedOut` → the login screen, any other lane → signed in, sessions load,
and the tier is worked out (`accountLogin` runs the TUI probe; the key lanes
are pay-as-you-go by construction, no probe). `model/list` is not a sign-in
signal — it answers from the provider catalog while logged out.

The login screen (`aui::screens::login`) is one screen with two methods. The
idle state is the method choice — *Continue with Meta account* (primary; the
subscription lane) and *Use an API key* (pay-as-you-go) — with one line saying
which bills what. The device flow (`account/loginStart {deviceCode}`) returns
the URL and code in the **result**, shows them with a spinner, and opens the
browser once on entering the device state; *Cancel* runs `account/loginCancel`
and returns immediately. The API-key form is a masked field with a reveal
toggle: on submit the text is read once, trimmed, sent in
`account/loginStart {apiKey}`, and the field is cleared when the call returns,
whatever it returned. An empty key never sends (invalidParams needs a
non-empty key); a rejected key returns to the form with the message inline.

**The URL, the code and the key are never logged.** They travel from the
`account/loginStart` result or the masked field onto the screen, the URL
additionally into `open`'s argv, and nowhere else — not stderr, not a file,
not a fixture (`fixtures/msp/transcript-account.jsonl` carries the example
values).

`account/loginCompleted` advances the screen (`granted` → Success, `denied` /
`expired` / `failed` → the error card, or the key form for an API-key
`failed`; `cancelled` → the method choice), and `account/changed` rebuilds
`Auth` exactly as the probe does: a signed-in lane while on the login screen
enters the app with **no reconnect** (the flow is host-owned, so the `muse
serve` that ran it already holds the credential), and `loggedOut` while
signed in clears the shell and raises the "Signed out of Muse" dialog ("The
credential was removed outside the app."). There is no reconnect-after-login:
the old function stays, unused, until the owner's first live turn confirms it
is not needed. Escape walks the login states (`Cancel` in `Starting` /
`Device`, `Back` in `ApiKey`, `ChooseAnother` in `Error`).

`auth.json` is read-only now, only for the two display strings (`user_full_name`
/ `user_email`) when the wire's `label` is absent. The sidebar footer is the
library's account row: the wire identity over the stored email, the plan row
(`Weekly …` label, warning-tinted when it is not a plan in force; the key
lanes read "Pay-as-you-go · API key" without a probe), and the provider usage
meter fed by the tier probe's weekly fraction — omitted while the probe has
said nothing usable. The whole footer opens the account menu, whose only row
is **Sign out** (`account/logout`, back to the login screen) — or **Sign out
(set by META_API_KEY)** on the environment lane, which a sign-out cannot clear
(the toast says to unset it and relaunch). No list-management buttons live in
the footer any more; they moved to the Sessions caption's view menu (see §5).

The on-state screenshots live in a scratch workspace
(`/private/tmp/harness-ws`): freshly started turn-less sessions are pruned
by `muse serve` 1.1.1 before any later run can list them, so only a
workspace with older stable empties can show the toggle on.

A `turn/completed` failure whose message reads like a credential problem
(`not authenticated`, `no credential`, …) on a `modelError` / `configError` /
`environmentError` raises the "Signed out of Muse" dialog, whose primary action
goes to the login screen. There is no auth error kind on the wire at all, so the
message is the only signal there is; the classifier is deliberately narrow, and
its tests are in `conn.rs`.

---

## 5. Projects and sessions (spec §3.7)

One window over several workspaces (design `docs/12-projects.md`). A
**project** is an adopted root with an identity of its own — a UUID, a stable
colour (slot 1–8 on the library's label ramp), a name that renames without
touching the folder, and the last-used model, effort and approval mode new
sessions start with. The store is `projects.json` under the harness support
dir (version 1, camelCase, atomic write, best-effort read); `sessions.json`
`SessionMeta` carries `project: Option<String>`, written at `session/start`.

A session resolves to a project in one order only: its stored project id when
that adoption still exists, else the adoption whose canonical root equals the
row's `workspace_root`, else "Other workspaces". Never by prefix, never by
the current project — a worktree session's folder differs from its project's
root, and a prefix would file it under the wrong project. `session/list` is
unfiltered and paged (200/page to `nextCursor`), so every workspace's
sessions arrive; rows carry the canonicalized workspace and the resolved
project id, re-resolved after adoptions change.

The **current project** is the open session's project, else the last used.
Boot adopts per D39 (`--workspace` wins and is adopted if new; else the
stored current; else the most recently opened; on a first run the launch
directory unless it is `/` or `$HOME`, in which case the window opens with no
project and the hero owns the empty state). ⌘N starts in the current project
with its root and defaults; picking model, effort or approval mode in a
session stores it on that session's project. The window title is
`session — project`, or the project alone.

`session/list` gives identity and timestamps and nothing a person can read. The
words come from `~/.local/share/muse/session-index.db`, opened **read-only** and
treated as a cache: a missing file, a schema this build has never seen, and a
database another process has locked all yield an empty map, and the sidebar
falls back to `Session <first id group>`. Nothing ever writes to it.

The rows are the library's `SessionSummary`, grouped by **calendar day** —
Today / Yesterday / This week / This month / Earlier — or by **project**:
one collapsible group per adoption in sidebar order (pinned projects first,
then newest session activity, no drag reorder), each with its mark, the
branch in mono, a running dot and the count, hover `+` and `…`; sessions
inside run newest-first with pinned first; then the muted "Other workspaces"
group, always last and closed until opened. Past five a group folds: it shows
its pinned rows, then the five most recent others, and holds the rest behind
a "Show N more" / "Show less" row (`expanded_groups` in the layout file,
persisted like `closed_groups`); the open session is always among the
visible ones even when older than the fifth. With one project and nothing in
Other the list stays the date view: the grouping follows the data until the
view menu persists a choice. An unsent session has no sidebar row. The row appears at the top of its
project when the first message is accepted (`turn/started`), titled from the
prompt, and the wire replaces it once the log flushes on `turn/completed`
(the wire lists a session only then). Repeated ⌘N never creates more than
one session per project: the draft is per project (see `docs/03-composer.md`),
and the parked draft view is never evicted from the MRU.

A group row's `+` starts a session in that project (making it current); on
Other it opens the Projects palette. Its `…` opens the project menu for that
project; on Other the menu carries the single row "Add as project…".

Every row carries one muted second line: `last_summary` when a turn completed
in this app, else the index's first prompt — but only when the row's label is
not that same prompt (a user-given name or a Muse title); otherwise the row
shows the "N turns" meta alone, never a repeated first line. Whatever shows
goes through the row's own one-line cap. `last_summary`
is written on `turn/completed` from the first line (≤ 120 chars) of the last
assistant text block: the fold is already in memory, so it costs no model
call, and it persists through `sessions.json` beside the name, the hidden and
archived flags, the pin and the derived title.

Above the Sessions caption sit three `nav_item` rows: **New session** (Plus,
the ⌘N in the sidebar), **Add project** (Folder, the ⌘⇧O in the sidebar) and
**Automations** (Zap) with a muted "Soon" tag — a placeholder with no
destination yet, so it answers with a toast saying so.
The caption's sliders icon opens the **view menu**: Group by project
(toggle), Show empty (n) / Hide empty, Show hidden (n) / Hide hidden (the
legacy `/hide` rows), Clear empty, Show archived (n) / Hide archived, and
Search all projects (toggle). Toggles keep the menu open so the
check is seen to change; Clear empty closes it and hides through the same
`hidden = true` override as `/hide`, with the same eight-second Undo.
Archived sessions are excluded from the list and from Clear-empty; shown,
they carry a muted Archived tag and their Archive tray action puts them back.

The **project menu** (`mark project ▾` in the header, or a group row's `…`):
one toggle row per project in sidebar order, checked for the menu's project —
picking one replaces a still-empty unnamed active session (hidden locally)
and otherwise starts a sibling session there — then New session here, Rename
project, a Colour submenu of eight swatches, Pin/Unpin project, Reveal in
Finder, and Remove from sidebar. Renaming swaps the crumb's name for the
dense field (Enter writes, Escape cancels, empty reverts to the folder name).
**Remove from sidebar** asks first ("Its n sessions stay on disk and move to
Other workspaces. Nothing in the folder changes."); on confirm the adoption
is forgotten, its sessions' stored project is cleared (a later re-add
resolves them by root), current passes to the most recently opened remaining
adoption, and there is no Undo — re-adding is one click in the palette.

The **Projects palette** (⌘⇧O, File › Add Project…, the nav row, the rail
cell, `/project`): section Projects — every adoption with its mark, the root
with `~` for home, and the visible session count; picking one starts a
session there — then section Add, whose head is the library's
`folder_drop_card` ("Drop a folder here / or click to choose one", ⌘⇧O
keycap): a click opens the native folder panel (directories only), a drop
adopts every dropped directory with the first becoming current. Below the
card, recent Muse workspaces from the index (not yet adopted, still on disk,
newest first, at most twelve, badged with their session count); adopting
never starts a session. The card is not a row, so the keyboard walks past
it; ↩ on an empty Projects palette opens the panel. The query filters both
sections by name and path. With no current project the crumb reads "Add a
project…" and opens this palette; with no project at all the transcript
column shows the **hero** ("Add a project", "Muse works inside a folder. Add
one to start.", Choose folder… and Recent workspaces — the second opens the
palette on its Add section; the whole hero column also takes a drop, same
rule as the card).

Row actions are Pin, Rename and Archive. Pin regroups the list around the
Pinned group. Rename opens the dense inline field — the library's
`dense_field` in its 22 px bordered wrapper, so the editing row keeps the 30
px row height and siblings never move — always single-line (no wrap,
horizontal overflow hidden, caret visible), committing through the existing
`/name` path and cancelling on Escape. Archive opens a danger dialog
("Archive \"<label>\"? — archived sessions stay on disk and return through
the Sessions menu"); on confirm the session leaves the list, the newest
remaining visible session opens (or the empty state), and a toast offers Undo
for eight seconds.

Collapsed (⌘B), the sidebar column is the library `rail` (`flat(true)`):
New, Search and Projects cells, a separator, the open session and up to eight
of the visible list as titled tiles — each tinted with its project's colour
(sessions in Other keep the default ink), running ones pulsing — and the
account avatar. The Search cell opens the full-text search palette, exactly
as ⌘⇧F does; the Projects cell opens the Projects palette, exactly as ⌘⇧O
does — there is no sidebar quick-filter, so the palettes can never share the
sidebar. The window-level
`--steps` verbs for all of this are `sidebar` (toggle), `overflow`,
`view-menu`, `account`, `pin`, `archive`, `archive-confirm`,
`show-archived`, `projects`, `project:<path>`, `project-menu[:<name>]`,
`project-colour:<n>`, `group-by:<date|project>`, `remove-project:<name>` and
`remove-confirm`, beside the older `search`,
`palette`, `resume`, `fork-picker`, `rename`, `hidden` and `empty`.

Opening a session is `session/resume { excludeItems: true }` to attach, then
`view/page` forward from the beginning of the view, paging on `nextCursor`
until it runs out. Every page folds in its own UI update as it arrives, so the
first page draws before the second is requested; `HistoryReady` fires after
page 1 (the cue that pins the tail), not at the end. A "Loading history…"
status row sits under the transcript while that runs. Resume attaches;
`view/page` is the path that is contiguous, ordered and bounded, and it never
replays `item/delta`, so a backfilled message arrives whole and the fold takes
it that way.

Reopening keeps what was opened: the harness holds an MRU of the last eight
session views (fold, scroll position and draft riding along in the parked
entity; its event subscription dropped). The reopened view shows at once and
tops up with `session/resume { cursor: <last observed viewCursor> }` — the
reconnect procedure of `docs/01-transport.md` §3, which serves `history.mode:
"none"` and streams only the suffix through the live stream. A forward
`view/page` anchored at the cached head is not the top-up path: it answers
`notFound/missingAnchor`. Hidden, archived and replayed views are never
cached; eviction drops the view.

⌘N starts a new session in the current project; ⌘B toggles the sidebar rail.
⌘⇧F opens the full-text search palette (`PaletteKind::Search`): sessions by
transcript text plus the files the workspace's turns created, Enter resumes a
session and reveals a file in Finder. Every hit wears its project's display
name (the folder name for sessions in Other); with "Search all projects" off
the query is scoped to the current project and the card reads
"Search {project}…". Full detail in `docs/12-search.md`. ⌘⇧O opens the
Projects palette.

The sidebar is resizable: a 6 px transparent strip over the sidebar/centre
divider carries the horizontal-resize cursor, and dragging it sets the width
to `clamp(start_w + dx, 180, 420)` (`aui::shell` tokens). Mid-drag a
full-window capture overlay owns every move and the release, and the shell
skips its layout spring so the divider tracks the pointer; the spring
re-arms on release. The drag clears on mouse-up anywhere and on window
blur. The settled width persists globally in `layout.json` under the
harness support dir (restored at boot, clamped on load); double-clicking
the handle resets to the 252 px default. `--steps sidebar-width:<px>`
scripts a settled width for screenshots.

The shell paints no traffic lights of its own — the window's native ones are
the only set. The sidebar header reserves their footprint (`native_lights`)
and the window positions them with `aui::shell::traffic_light_position`, so
they sit centred in the header row at every density. The centre header shows
the project crumb (`mark project ▾`, one click target opening the project
menu; "Add a project…" with no current project), a `·` separator, then the
active session's label with the provider mark before it, and the overflow "…"
menu (Rename, Fork, Archive); the title flexes inside the header cell and
elides to one line, so a whole first prompt as the label can never push the
overflow button out. Renaming the open session swaps the session label for
the same dense single-line field the sidebar row uses, and renaming the
project swaps the crumb's name, both through the same confirm/Escape path.
There is no
right-pane toggle and no right-header close button — the right pane's slot
stays empty; the shell is always given `right_open(false)`, and there is no
per-session state for it any more (B-DEAD-3, 2026-09-12). The
shell wraps the header row in its drag region, so press-drag moves the window
and double-click zooms while the buttons and the rename field keep their
clicks. Collapsed (⌘B), only the pane below becomes the 48 px rail: the header
row stands still (`header_follows_sidebar(false)`), so the sidebar cell keeps
its width — reservation, toggle and search included — and the centre header
never slides under the native lights. There is no expand button in the centre
header; the toggle lives in the sidebar header in both states.

---

## 6. The centre pane

The transcript is rendered from the fold's `Session`, block by block, through
`aui::transcript::*`: streaming text with the caret, thinking blocks, tool cards
with their streamed shell output, approval and question cards (**read-only** in
this phase — the wire round-trip for a decision is Phase 4), plan, todo,
summary, error, goal, the mandated generic fallback card, and marker rows. Every
newly arrived block fades in through `aui_motion::stream_reveal`, and only the
newest turn animates.

The per-turn token footer is the turn's closing text block: `assistant_turn`
carries the `TurnMeta`, so the footer sits under the reply where it belongs.
A finished turn that billed reasoning tokens (`TurnMeta::reasoning_tokens > 0`)
but shows no thinking card gets the same footer line with the reasoning cell
reading `419 reasoning, thought silently` — one line, in the footer's own
style (mono, `FS_11`, `ink_4`). The count alone never said the thinking
happened off-screen; the decision is the pure
`transcript::silent_reasoning` (assistant turn, count above zero, no
`Block::Thinking`), and `fixtures/msp/transcript-real.jsonl` is the real
capture that exercises it (419 billed reasoning tokens, no `reasoning`
item). The library draws the footer from `TurnMeta` with no per-cell hook,
so a silent turn carries no library footer and the harness draws the row
itself, mirroring the library's cells. A turn with a visible thinking card
keeps the library's plain `419 reasoning` cell.

**The wheel does not go through `list()`.** A zero-size canvas over the list
takes every `ScrollWheelEvent` in the *capture* phase and drives
`ListState::scroll_by` with that one event's delta
(`session/render.rs`, `wheel_capture`). `list()`'s own handler sums a frame's
deltas with `ScrollDelta::coalesce` and applies the running sum against the
offset it captured at paint; `coalesce` overrides rather than sums on a sign
change, and an exactly-zero delta counts as positive, so a zero sample —
which AppKit emits constantly — discarded a frame's accumulated upward travel
and none of its downward travel. Capture is the only phase that can win:
`Interactivity::paint` registers a container's listeners before painting its
children, and bubble runs in reverse registration order, so the list always
beats anything wrapping it there. The hitbox is gpui's own, so an `occlude()`
overlay above the transcript takes the wheel instead; a gesture more
horizontal than vertical is left alone, so a wide markdown table still
scrolls sideways. Measured before and after in
`docs/diagnosis/scroll-research-2026-09-13.md`.

Auto-scroll is tail-follow, the same rule the block terminal uses: anything new
scrolls the list to the bottom, but only for a reader who was already within a
few dozen pixels of it.

The transcript list is virtualized (2026-09-10): `render_transcript` renders a
gpui `list()` with a persistent top-aligned `ListState`, one item per **row**
— a block of an assistant turn, a user bubble, or the silent-reasoning
footer (`transcript::turn_rows` / `turn_row`) — instead of building every
cell every frame. Per row rather than per turn since the owner round of
2026-09-13: gpui lays a visible list item out whole every frame, and a real
turn runs to hundreds of blocks, so per-turn items cost a frame whatever the
biggest visible turn cost. Every row carries a height hint (`ROW_HEIGHT_HINT`)
until it is measured: on the first fill, again after each history page lands
(`rehint_rows`: measured rows keep their heights as hints, the scroll position
is put back, and one frame of wheel events is dropped, which is the price of
gpui's `reset`), and again on the frame after the list's width changed
(`note_list_width`, fed by the wrapper's `on_children_prepainted`), because
gpui forgets every height and hint on a width change. An unhinted row counts
as 0 px in the list's sum tree, and a flick over a stack of them lands on the
head (H2). A change in one turn's row count splices from that turn's first
row (`sync_virtual_list` diffs the per-turn `(id, rows)` list), so the rows
above stay measured. Top, so a short transcript starts
at the top instead of leaving a void above it; tail-follow is the `follow`
flag (`scroll_to_end` when the reader was at the tail), never the alignment.
The list wrapper carries the pre-virtualised container's own gutters
(`TRANSCRIPT_PAD_TOP`/`TRANSCRIPT_PAD_X`, `SP_4` below), so the turns line up
with the status and banner rows. Inside those gutters every row is centred on
an 880 px measure (`TRANSCRIPT_MEASURE`: the design's ~760–800 px column at
the 1.1 text scale) — the list, the loading and status rows, the banners, the
caret menus, the queue strip, and the composer's content, whose docked band
still spans the pane. Blocks inside a turn sit 8 px apart (the design's
`.grp2{gap:8px}`) and turns 16 px apart (its `.tr{gap:16px}`). Fold changes `splice` the affected
range only, and only visible rows are laid out per frame, so per-frame cost
stays bounded as the transcript grows. `apply` notifies only when the fold
changed or view state changed (unchanged streaming deltas earn no frame), the
turn ticker runs at 1 Hz and notifies only when the displayed second changes,
and unchanged turns are never re-parsed (the library memoises markdown).
`HARNESS_FRAME_STATS=1` prints render-time percentiles to stderr; the
`synthetic-stress-300.jsonl` capture (~300 turns, generated by
`fixtures/msp/make-stress-300.py`) is the benchmark.

The collapsed rail (2026-09-13 follow-up) is not a strip of two glyphs: after
New session and Search it shows the open session and up to eight of the
visible list (pinned first, then newest) as titled tiles — the initial, the
state dot in the corner, the title as the tooltip — so a person switches
sessions from the rail without expanding it. The search palette's query
editor lives inside the card's own query row (the library's `query_slot`);
the list is bounded and scrolls; nothing floats above the card.

A turn's terminal settles the cards it left open (`muse-adapter`
`settle_open_blocks`): Muse can end a turn with a shell call still
`inProgress` (a live shell it closes on a later turn), and a card must not
spin under a finished reply. Running calls read done on a completed turn,
cancelled on a cancelled or failed one; an approval still "approving" reads
allowed. This is the one place the fold moves a status without an item event,
and it moves it only on the server's own terminal.

### Measuring

`harness --bench <capture.jsonl>` streams the capture's `<--` lines — a
`view/page` result's `events` unpacked into the notifications they stand
for, so a `MUSE_CAPTURE` of a real open path benches as the live stream
would — through the fold on a timer at `--bench-cadence-ms` (default 4, so streaming cost is
real) while driving the transcript `ListState` programmatically:
`--bench-scroll top` pins the first turn, `mid` re-centres, `tail` follows
the tail, `sweep` (the default) runs top→tail→top over the stream. It runs at
least `--bench-frames` frames (default 600), then prints one line per metric
— `bench-element`, `bench-apply` and `bench-frame` with `n p50 p90 p99 max`
(element construction is the existing `render_transcript` timer; frame time
is the interval between consecutive paints while frames are requested, and
`bench-frames` reports `frames fps dropped`, dropped meaning past 16.7 ms;
`--bench-out` also carries the intervals in frame order as `frame.series_us`,
so the stream's frames and a wheel phase's can be told apart) —
plus `bench-rss` (peak RSS) and `bench-idle`: the frames a settled
transcript requests over the next 2 s, which must be none.
`--bench-scroll wheel` is the scroll-jank instrument rather than a frame
driver: the stream lands head-pinned (so, as after a real backfill, every
row above the tail is still unmeasured), the list is then pinned at the
tail, and the instrument dispatches real `ScrollWheelEvent`s at the
transcript's centre, one per frame — (a) a flick up of 6 × +600 px, (b) 90 × −40 px back down, (c) a slow
trackpad climb of 300 × +20 px, (d) its 300 × −20 px mirror — sampling the
list's `logical_scroll_top` after each. The other modes drive `scroll_to`
while the stream lands and never exercise the wheel's pixel-delta path
through the sum tree's heights, which is where the jank lived. `wheel`
prints one `bench-scroll` line (`events frames jumps stalls clamped` plus
the `item_ix` after each phase: `jumps` counts frames where `item_ix` moved
across more rows than the event's travel over one-line rows explains (the
H2 teleport is tens of rows at once), `stalls` frames where an event left the position unchanged
short of a scroll limit, `clamped` frames where the event ran into the head
or the tail limit instead — correct end-of-list behaviour, so `stalls +
clamped` is every no-move frame) and carries the same numbers in
`--bench-out`'s `scroll` object; its phases are the run's frames, so it
skips the `--bench-frames` sweep.
`--bench-open-turn` stops the stream before the capture's last
`turn/completed` instead, so the same window is measured with a turn
still running: the only clock that may still ask for frames is the 1 Hz
elapsed row, so `bench-idle frames_2s` must be 2 and never more
(finding `performance-13`). `--bench-out
<file.json>` writes the same numbers plus the command, the capture, the
build profile and the git short hash, for tracking across runs. `--bench`
implies `HARNESS_FRAME_STATS` (read once, so disabled builds pay one relaxed
load per frame) and replaces the old `bench:<n>` step's role — the step still
works, driving N frames on a static replay for the stderr percentiles.

A session switch answers on the click's own frame: `resume` records the
target id at once (the sidebar row highlights and the centre header labels
from it, never from the view), and the centre swaps at once to the new view —
the cached one when the MRU holds it, otherwise a fresh view showing its
neutral loading row, never the empty state. Pages stream in behind it, each
folding in its own update. A failed `session/resume` keeps the new view and
reports.

Turns carry an in-flow action row under the prose (`actions_bottom`) for both
roles. Assistant: Copy writes the turn's text to the clipboard, Retry resends
the user input behind the turn, Fork opens the turn picker; Pin is hidden on
turns through the library's `AssistantTurn::actions(..)` (it lives on sidebar
sessions, whose rows keep the Pin action). User: Copy, Edit (text into the
composer draft), Resend. Wire actions are live-only: in a replayed capture
they answer with a toast.
Markdown links click through: URLs open in the browser, workspace paths open in
their default place — folders in Finder, files in their default app (escapes
above the workspace are rejected with a toast, missing paths toast). `Block::ToolGroup` renders through the library `tool_group`,
its open state in `Folds` keyed by the group's fold key. The transcript holds
one `TextSelection` per turn (keyed by turn id with its markdown source):
dragging or word/paragraph-picking in a turn highlights it through the turn's
`selection(..)`/`on_selection_change(..)`, a plain click elsewhere clears that
turn, ⌘C in the transcript context copies `turn_selected_text` of the held
turn (never from the composer or a card field), and Escape clears it. Per
turn because the library scopes cell keys (`p0`, …) to the markdown view that
rendered them — one shared cell would light up every turn at once. `--steps
top`, `end`, `expand-groups` and `select-text:<turn>:<from>-<to>` (a scripted
hold over the turn's first paragraph) drive screenshots.

`aui::composer::composer(...).docked(true)` is the composer. **Enter** sends,
**Shift+Enter** makes a newline, **Escape on an empty composer** and **⌃C**
interrupt. Enter is an action bound in the `HarnessComposer` key context, so it
wins over the textarea; Shift+Enter matches no binding and falls through to the
editor, which is exactly the split §3.9 asks for.

Sending is `turn/start` with the configured provider and `displayText` set to
what the person typed, verbatim — that is what the transcript shows, and it
stays their words even when a later phase prefixes the model-visible input. Stop
is `turn/interrupt { retract: true }`; when the retraction lands, the fold hands
the prompt back through `take_restored_prompt` and it goes into the composer.

The status row while a turn runs is "Working…" with the elapsed time and the
`esc to interrupt` hint, plus the queued count when there is one. Once the
running turn's reply has fully arrived but the turn is still open — Muse runs
`reminderChild` items (memory reminders) for 30–70 s after the `agentMessage`
before `turn/completed` — the row reads "Finishing up…" instead, with the same
clock and hint and a "memory reminders" note, so the quiet tail does not read
as stuck.

The model, effort, mode and context chips render the **server's** current values,
read back out of `SideState`; their menus arrived in Phase 3
(`docs/03-composer.md`).

---

## 7. Errors (spec §3.8)

Classification is on `error.data.kind`, never on the message.

| where | which |
|---|---|
| inline banner over the composer, with a dismiss | every other command error: `invalidParams`, `commandRejected`, `backpressured`, `sessionNotLoaded`, the approval and userInput families |
| modal dialog with one primary action | `sessionInUse`, `sessionNotFound`, `sessionAmbiguous`, `parseError`, `notInitialized`, `experimentalRequired`, `internal`, `overloaded`, and every transport-level failure: a dead child, a timeout, an unframable line |
| the "Muse disconnected, reconnecting…" banner | `MuseEvent::Closed` |
| the "Signed out of Muse" dialog → login screen | a turn failure with a credential-shaped message |

A failed `turn/start` also puts the person's words back in the composer: the
turn never left, so they should not lose them.

The reconnect procedure is the one in `docs/01-transport.md` §3: respawn
`muse serve`, `initialize`, then `session/resume` with the last observed
`viewCursor`, which serves `history.mode: "none"` and streams only the suffix.
The banner stays up until the new child answers.

---

## 8. What is deliberately not here

- **No right pane.** The shell keeps the column and `ToggleRightPane` stays
  bound as a no-op, so nothing has to move when Phase 5 fills it.
- **Approvals and questions are read-only.** A card that looked actionable and
  did nothing would be worse than one that plainly is not. Stop still works.
- **No queue strip, no menus, no context meter, no plan mode, no mentions, no
  images** — in *this* phase. Phase 3 built all of them; `docs/03-composer.md`
  describes them, and the `Overlays` entity that the dialog field became.
- **No rename, hide or search.** Phase 5, and none of them are on the wire.
- **The `Overlays` entity is one field.** There is one modal and nothing else to
  stack yet.

---

## 9. Native menus and the app bundle

The menu bar is built in `crate::app::set_menus`, called from `main.rs`
after `bind_keys` — after, because macOS reads each item's shortcut from the
keymap. Harness (About, Services, Quit), File (Add Project…, New, Close),
Edit (the standard six with `OsAction`), View (sidebar, palette, search,
theme),
Window (Minimize, Zoom), Help (Harness Documentation, which reveals the
`docs/` folder in Finder). The red dot and ⌘W both hide the app (`cx.hide()`
after the tier-probe cleanup): the window and the `muse serve` child survive,
so the Dock icon and ⌘-Tab bring the same session back, and `on_reopen`
rebuilds the window through the shared `open_shell_window` if it was removed
some other way. ⌘Q runs the cleanup and `cx.quit()` through the app-quit hook
(`tier::cleanup_probes`), so every exit path is one function. The docs
lookup (`find_docs_dir`, unit-tested in `app.rs`) tries the working
directory's `docs/` first, then three ancestors above the executable, so it
works from `cargo run` and from a debug target; a bundle moved away from
the repo logs instead of pretending.

`scripts/bundle.sh` assembles `target/bundle/Harness.app` from the release
binary, `assets/icon-1024.png` (a flat H tile drawn by the checked-in
`assets/make-icon.py`, converted to `Harness.icns` with `sips`/`iconutil`)
and a generated `Info.plist` (`CFBundleIdentifier dev.harness.app`),
ad-hoc signed so `open target/bundle/Harness.app` launches it on this
machine. Signing/notarisation are out of scope.
