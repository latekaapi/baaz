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
| `--bench <capture.jsonl> [--bench-cadence-ms <ms>] [--bench-scroll top\|mid\|tail\|sweep\|wheel] [--bench-frames <n>] [--bench-open-turn] [--bench-bare] [--bench-out <file.json>]` | stream the capture through the fold on a timer while driving the transcript list, and print element / draw / frame / fold-apply timing plus peak RSS (§6). Free: no child, no server. Cadence defaults to 4 ms, scroll to `sweep`, frames to 600. Drives the normal shell; `--bench-bare` drives the transcript alone. Implies `HARNESS_FRAME_STATS`. |
| `--steps <a;b;c>` | drive the open session from the command line, so a screenshot is reproducible (`docs/03-composer.md` §1, `docs/04-approvals.md` §7). Scripting only — its full verb table, with which steps cost a turn, lives in `main.rs`'s `Args::steps` doc comment; only `send:` and `steer:` bill. Runs exactly once, as soon as the wire is connected and a session is open; the boot session opens without waiting for `session/list`, and a step that cannot run logs instead of vanishing. |
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
| `HARNESS_DETERMINISTIC=1` | freeze the clocks and draw every card settled, so a `--replay … --screenshot` capture is byte-identical run to run. One clock (`src/clock.rs`): sidebar grouping/elapsed and turn ages read "now" once per frame — under the flag "now" is the newest stamp in the data (the sidebar's `updated`, the transcript's reported `recorded_at`), so the newest row reads `now` however old the fixture is — and every `Instant` behind a label or countdown is frozen, so elapsed cells vanish and countdowns show their full duration. The boot holds the platform's reduced-motion switch, so every motion primitive (spinner and shimmer loops, tweens, enter presence, springs, the streaming caret) resolves to its resting state — including components with no `at_rest` of their own, like the library's `StatusRow` or a pending approval's header spinner. On top of that the harness passes `at_rest` everywhere it constructs an animated component (turn reveals, the login card, dialogs, toasts, palettes, all composer menus, approval cards, suggestion chips); a capture never takes keyboard focus, so the composer's blinking caret (which honors no motion switch) never paints. |

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
the old function stays, unused, until the first live turn confirms it
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
root, and a prefix would file it under the wrong project. An adoption whose
root is gone (a deleted worktree, an unmounted volume) resolves nowhere at
any step: its sessions read as "Other workspaces" until the path comes back.
The adoption itself stays in `projects.json` — hiding is a listing rule, and
nothing in the listing path writes the store back. Existence is cached per
project with a 30 s TTL and rechecked for every adoption off the UI thread on
each list refresh. `session/list` is
unfiltered and paged (200/page to `nextCursor`), so every workspace's
sessions arrive; rows carry the canonicalized workspace and the resolved
project id, re-resolved after adoptions change.

The **current project** is the open session's project, else the last used,
else the most recently opened adoption whose root is on disk: a missing root
is never current (the header crumb, the accent bar, boot, the removal
fallback and ⌘N all fall back past it), but it stays adopted, so it can
become current again when the path does. Boot adopts per D39 (`--workspace`
wins and is adopted if new; else the
stored current when its root is on disk; else the most recently opened one
that is; on a first run the launch
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
one collapsible group per adoption in sidebar order (pinned first, then name
case-insensitively, never recency, no drag reorder), each
a plain muted label with the count, a left-edge accent bar in the state
colour when any of its sessions runs (breathing with the shared pulse while
running, solid otherwise), and hover `+` and `…`; sessions inside run newest-first with pinned
first; then the muted "Other workspaces" group, always last and closed until
opened. The plain label's first glyph starts at the leading centre (x = 18,
the line the nav icons and the session dots sit on); with the chevron flag
the chevron takes that box and the label follows at `NAV_LABEL_X` like every
other label. Every project label reads the same muted `ink-3` — the current
project is never brighter; only the accent bar marks it, and
the bar alone never moves or recolours the label. The collapse chevron, the
current-project accent bar and the trailing
branch are layout flags (`group_chevron`, `group_bar`, `group_branch`, all
default off). The Settings dialog owns the switches:
⌘, (File → Settings…), the account footer menu's "Settings…" row, or
`--steps settings[:<section>]`; its Sidebar section holds those three plus
the two auto switches below — `auto_title` ("Name sessions
automatically") and `auto_summary` ("Summarise sessions in the sidebar"),
both default ON — and later sections add arms in
`Harness::settings_sections` (`crate::settings`). `--steps
group-chevron|group-bar|group-branch|auto-title|auto-summary` flips them
without opening the dialog.
Activating a session from outside the sidebar reveals it: the least scroll
that brings the row — or, when its group is closed or folded past the cut,
the group row, never auto-expanded — into view (O6). Past five a group folds: it shows
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
Other it opens the Projects palette. Scripted, `new:<project>` opens on the
`session/start` round-trip, after the following steps would run — so session
verbs (`name:`, `draft:`, `send:`) wait for the switch, bounded at 10 s,
instead of acting on the session that is still open; any activation clears
the wait, as does a failed start. Its `…` opens the project menu for that
project; on Other the menu carries the single row "Add as project…".
Without the wait, `new:demo` followed at once by `send:` could bill its
turn on the session that was open before.

Session rows carry no provider mark. Every row is three lines — title with
the elapsed time at the right, a context line, and a semibold status verb
line coloured by state — each truncating with an ellipsis at the sidebar's
width, never wrapped, so a brand-new session is exactly as tall as its
neighbours. The context line reads the pending approval's exact command or
the pending question when one exists, else the ask/result byline as one
line — the ask hugging its content, a single `·` separator, the result
taking the rest, each truncating with an ellipsis when squeezed — else a
one-line preview, else `project · branch`, else blank space
that still keeps the line's height; a generated title still in flight reads
`Naming this session…`. The status line reads `Working · 14m` for a running
turn (its own elapsed), `Needs approval`, `Asked: "…"` with the question's
words, `Settled · 12m · 5 turns`, `Failed · 1h`, or `No reply yet` —
waiting on a person outranks running, so an approval mid-turn never reads
`Working`. Hovering a row past a short beat opens the hover detail beside
it: the full title, ask and latest reply, the status with its detail (the
pending question, the approval command, the terminal error), project,
branch, turn count and last change — only what the app knows, the rest
omitted. The rows report hover enter/leave themselves carrying the row's own bounds,
so the delay arms even on a settled sidebar that re-renders nothing — and
the report seats the card from the row, because the timer's notify reaches
only the root, whose cached pane never re-renders to poll one. The card's
left edge sits at the laid-out sidebar pane's right edge plus the card gap,
top-aligned with the hovered row, sliding up near the window bottom — never
over the sidebar, wherever in the row the pointer is. The card takes
no focus and never covers its own row, so the
row's click still lands; it closes on leave, scroll and click. Scripted,
`row-detail:<session_id>` pins it open (pair with `click:` on the same id),
and `hover:<session_id>` delivers the row's own hover report, so the card
opens past the delay exactly as for a real pointer (pair with `click:` on
the same id and a `wait:` past the delay).
The attention states ride the wire's `Session.attention` (muse 1.3.0:
`approvalPending`, `inputPending`) carried through the sidebar join and
kept fresh off the `session/statusChanged` broadcast; the open session's
pending words come live from its own fold, which is the only place the
command and question text exist. `Failed` rides the last turn's terminal
error from `turn/completed`'s `error`, persisted in `sessions.json` beside
the name and the byline halves, cleared by the next turn's start or its
success. A sidebar click only re-attaches (`session/resume` +
`view/page`); it never sends `turn/start`. A `turn/started` the re-attach
re-delivers for a turn the view already saw complete never marks running —
the row keeps its terminal state instead of manufacturing a fresh
`Working`.
The preview half is `last_summary` when a turn completed in this app, else
the index's first prompt — but only when the row's label is not that same
prompt (a user-given name or a Muse title); otherwise the row shows the
"N turns" meta alone, never a repeated first line. Whatever shows goes
through the row's own one-line cap. `last_summary` is written on
`turn/completed` from the first meaningful line of the last assistant text
block (fenced code skipped, markdown markers stripped): the fold is already
in memory, so it costs no model call, and it persists through
`sessions.json` beside the name, the hidden and archived flags, the pin,
the derived title — and `last_ask`, the owner's last request in the same
free excerpt, which together with the summary is the row's two-line
byline. Only when the "Summarise sessions in the sidebar" switch is on AND
the free excerpt is poor (either half empty, fence-leading or code-only, or
the pair past twice the row's width) does one cheap model call rewrite the
two lines through the title side-session mechanism below: only once the
session is idle, at most one start per 30 s, skipping turns that changed
little.

A session is titled by what the person said, never by a command the agent
ran. The title order is the `/name` name, the generated title below, the
row's own name/title/prompt, the index's name/title/prompt (its literal "New
session" is a placeholder and counts as nothing), then the derived title,
then "New session". The generated title is one cheap model call
(`muse-spark-1.3` when listed, else the server default) on the first send,
run as one turn in a throwaway side session in the same workspace — a
bare-UUIDv7 client id (muse 1.3.0 rejects any `session/start` id that is not
its own shape), recorded in memory and as `side_session` in `sessions.json`
before the start runs, hidden from the first moment so it never reaches the
sidebar, the palette, the search index or the counts, including across a
restart mid-flight — asking for a 3–6 word
title for the user's first message and harvesting `turn/completed` with a
free `session/read`. It lands in `sessions.json` as `generated_title` (never
via `session/rename`), ranked as above; while in flight the row reads
`Naming this session…` and an untitled header crumb borrows it, and both
update in place when it lands. Failure (a 90 s timeout — grounded in the
2026-09-17 side-session logs, where the billed title turn ran 20.7 s
against the old 20 s ceiling while real turns ran 20.8–36.8 s — a wire
error, an empty reply) falls back to the first-prompt label with one log
line and at most one retry — a timeout never retries — and exactly one
generation ever runs per session: the persisted `title_attempted` marker
holds across resume, reconnect, replay and restart, and the "Name sessions
automatically" switch off means no title call ever. The timeout stands the
row down without giving the job up: the placeholder falls back to the first
prompt at the deadline, and a reply that lands later still harvests through
the same free read and lands exactly like an in-time answer — dropped only
if the session is gone or has since been named, never retried into a second
generation, since the turn is already paid for. `--steps title-pending` /
`title-timeout` / `title-land:<text>` stub the in-flight, stood-down and
landed states for captures, free.
The derived title is the transcript's earliest user prompt — earliest recorded
submission, then earliest folded user turn, first line, through the same
one-line cap — or the first shell command when the session has no user text
at all. It is written from the open transcript (live on every wire event, and
once for a replay whose capture is already folded), so a fresh row keeps the
label its first send gave it instead of falling back to "New session" with
the reply as its preview.

Above the Sessions caption sit three `nav_item` rows: **New session** (Plus,
the ⌘N in the sidebar), **Add project** (Folder, the ⌘⇧O in the sidebar) and
**Automations** (Zap) with a muted "Soon" tag — a placeholder with no
destination yet, so it answers with a toast saying so.
The Sessions caption is fixed above the scrolling list: the rows clip at the
list's own top edge, so the header and its spacing stay put at any scroll
offset — including after a reveal-on-activation scrolls the list — and no row
ever reaches the nav rows. The sessions area is the
library's virtualised list (`aui::nav::virtual_sidebar_view`): `render_sidebar` re-flattens the grouping into `SidebarRow`s every
frame (an index walk, no summaries cloned) and keeps a harness-owned
`ListState` in sync — `reset` after a regroup or filter change (the only sync
that drops the offset), `splice` after a local insert or remove (open/close,
fold expand, sessions arriving or leaving), `remeasure_items` after a
text-only height change under stable rows — so only the rows near the
viewport, plus a small overdraw runway, are ever built; a 200-session stress
sidebar builds the same handful of rows a 20-session one does. The list
scrolls like the transcript: a capture-phase canvas over the
list takes the wheel, accumulates it into the shared `sidebar_wheel` cell,
and the pane drains exactly one `ListState::scroll_by` per frame, notifying
only the sidebar pane; a 150 ms gesture horizon keeps presenting through the
momentum tail. Reveal-on-activation now steers the list itself
(`row_index_for_session` finds the flattened row, `ensure_row_visible` moves
the minimum to show it whole, or its group head when the row is folded away
or its group closed — never auto-expanded); a wheel or a resize drag disarms
any armed reveal and no reveal installs mid-gesture, mid-drag, or after the
user has scrolled, so the list never moves under the hand, and a sidebar
click never arms one at all. A scroll that lands under an open group menu
dismisses it (the calm option — the header menu, seated from the fixed
caption, is never touched); folds and group open/close snap instead of
springing under the virtual path, which reads as acceptable.
`sidebar-wheel:<dy>[,n]` is the scripted instrument (dispatches at a sidebar
point and logs the list's `item_ix`+`offset_in_item` plus pane/root renders
and drains); the render counters behind it are the sidebar analogue of the
transcript's wheel instruments. The `sbwheel` line also carries `centre=`:
transcript-column rebuilds since the last drain, so a sidebar burst reads
`centre=0` while the cached transcript reuses its retained subtree.
The caption's sliders icon opens the **view menu**: Group by project
(toggle), Show empty (n) / Hide empty, Show hidden (n) / Hide hidden (the
legacy `/hide` rows), Clear empty, Show archived (n) / Hide archived, and
Search all projects (toggle). Toggles keep the menu open so the
check is seen to change; Clear empty closes it and hides through the same
`hidden = true` override as `/hide`, with the same eight-second Undo.
Archived sessions are excluded from the list and from Clear-empty; shown,
they carry a muted Archived tag and their Archive tray action puts them back.

The **project menu** (`project ▾` in the header — the crumb reads
`project › session` with no marks, or a group row's `…`):
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
cell, `/project`): section Projects — every adoption with the folder glyph
(no coloured tiles anywhere), the root
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
of the visible list as titled tiles — each a plain initial on the surface
step with no label tint, running ones pulsing — and the
account avatar. The Search cell opens the full-text search palette, exactly
as ⌘⇧F does; the Projects cell opens the Projects palette, exactly as ⌘⇧O
does — there is no sidebar quick-filter, so the palettes can never share the
sidebar. The window-level
`--steps` verbs for all of this are `sidebar` (toggle), `overflow`,
`view-menu`, `account`, `pin`, `archive`, `archive-confirm`,
`show-archived`, `projects`, `project:<path>`, `project-menu[:<name>]`,
`project-colour:<n>`, `group-by:<date|project>`, `group-bar`,
`group-branch`, `group-chevron`, `auto-title`, `auto-summary`,
`title-pending`, `title-timeout`, `title-land:<text>`, `remove-project:<name>` and
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
the project crumb (`project ▾`, one click target opening the project
menu; "Add a project…" with no current project), a `·` separator, then the
active session's label with no provider mark before it,
and the overflow "…"
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

The scripted counterpart is the `--steps wheel:<dy>` verb: one synthetic
`ScrollWheelEvent` at the window centre, logging
`harness: wheel dy=<dy> list_px=<before>-><after>` (the transcript's pixel
offset on either side). Over the open palette the palette's list scrolls and
the transcript never moves — the library card occludes its own rect and stops
the wheel after its list scrolls, and the harness palette scrim occludes the
dimmed ground around it — while over the bare transcript the same wheels move
it. A wheel dispatched before the first frame lands on no listener, so the
proof runs put a `wait:` ahead of the wheels.

Auto-scroll is tail-follow, the same rule the block terminal uses: anything new
scrolls the list to the bottom, but only for a reader who was already within a
few dozen pixels of it.

The transcript list is virtualized (2026-09-10): `render_transcript` renders a
gpui `list()` with a persistent top-aligned `ListState`, one item per **row**
— a block of an assistant turn, a user bubble, or the silent-reasoning
footer (`transcript::turn_rows` / `turn_row`) — instead of building every
cell every frame. Per row rather than per turn: gpui lays a visible list item out whole every frame, and a real
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

**The transcript column is cached; the composer band is live.** The root embeds the active `SessionView` through gpui's
`.cached` (laid out with `flex_grow(1)` so it fills the centre above the
composer), and a sidebar-only frame — wheel, reveal, resize tick — reuses
the retained transcript instead of rebuilding it. The composer band and the
file-drop overlay compose live beside it at the root: the library composer
embeds its textarea state as a stateful child view, and gpui-base's input
element notifies that state on every paint, which would pin any cached
ancestor dirty on every frame after its first paint — so a cached entity
containing the composer rebuilds forever and the cache never reuses. The
transcript column holds no such self-notifying paint, and the textarea's
per-paint notify now lands on the never-cached root, where it is harmless.
A hidden drop overlay would likewise keep asking for the next frame while
its exit presence runs, so it mounts only while a drag is over the pane
(static via `at_rest` in deterministic captures).
`fixtures/msp/synthetic-stress-hetero.jsonl` (80 turns, generated by
`fixtures/msp/make-stress-hetero.py`: two 60-line shell cards, a 30-line
rust block and three paragraphs per turn) is the scroll-cadence capture:
scrollable *and* heterogeneous, so height-hint re-basing (H-1) can show
where the uniform capture and the short heterogeneous captures cannot.

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
— `bench-element`, `bench-apply`, `bench-draw` and `bench-frame` with
`n p50 p90 p99 max`
(element construction is the existing `render_transcript` timer; `bench-draw`
is the whole frame, from the start of the root render to the end of paint —
a zero-size canvas painted last in the root stamps the end, so the gap
between `bench-element` and `bench-draw` is row building, layout and paint;
frame time
is the interval between consecutive paints while frames are requested, and
`bench-frames` reports `frames fps dropped`, dropped meaning past 16.7 ms;
`--bench-out` also carries the intervals in frame order as `frame.series_us`,
so the stream's frames and a wheel phase's can be told apart) —
plus `bench-rss` (peak RSS) and `bench-idle`: the frames a settled
transcript requests over the next 2 s, which must be none. The window opens
only after 500 ms with no transcript constructions (bounded 5 s, so a true
loop fails loudly instead of hanging): trailing async work after the driver
stops — list measure, enter presences, a tier answer — lands in the first
~0.3 s and counting it flakes 0–32 run to run on the same binary. The line
also carries `root_2s`, the window's root renders (`--bench-bare` counts the
bench root, which is the window's root there), and `idle_root_2s` in
`--bench-out`. A settled transcript reads `0/0`; an open turn ticks its
1 Hz elapsed clock through both (≈2), and its running animations (braille
lead, label shimmer, activity spinners — all infinite while mounted) rebuild
the transcript every tick they paint, which is legitimate animation work,
not an idle loop: on a settled capture every commit reads
steady `0/0`. The registry input element's
paint-end state rewrite (`gpui-base-0.6.0/.../input/base/element.rs:2374`)
never self-drives: gpui-pre only wakes the platform outside the draw phase
(`gpui-pre-0.3.3/.../window.rs:167-193`) and clears the dirty set at draw
end, so an in-draw notify schedules nothing.
`--bench` drives the normal shell: the `Harness` root with the replayed
session active, so `bench-draw` covers the sidebar, header and composer.
The sidebar column is its own cached view (`Entity<SidebarPane>`, embedded
with gpui's `.cached(size_full)` and re-armed by `SidebarKey` from
`on_frame`), so a transcript notify rebuilds the centre but never the
column — a plain entity embed would re-render every frame, which is why the
cache call is there.
`--bench-bare` keeps the old `BenchRoot` — the transcript alone — for the
transcript-only number; shell ≈ bare + ≤ 1 ms is the goal (after
the fixes: ≤ 0.6 ms p50 in all 8 cells).
`--bench-scroll wheel` is the scroll-jank instrument rather than a frame
driver: the stream lands head-pinned (so, as after a real backfill, every
row above the tail is still unmeasured), the list is then pinned at the
tail, and the instrument dispatches real `ScrollWheelEvent`s at the
transcript's centre, one per frame — (a) a flick up of 6 × +600 px, (b) 90 × −40 px back down, (c) a slow
trackpad climb of 300 × +20 px, (d) its 300 × −20 px mirror — sampling the
list's `logical_scroll_top` after each. Each event waits at most 50 ms for
its frame (a settled transcript requests none, so the wait never sits out
the old 2 s timeout and its artifact). The capture handler never applies:
it accumulates the delta into `pending_wheel` and re-arms a 150 ms gesture
horizon, and `render_transcript` drains the sum into exactly one `scroll_by`
per frame — `bench-wheeldrain` prints the applied drains (`scroll.scroll_bys`
in `--bench-out`), 744 per wheel run against ~1008 per-event `scroll_by`s
before. While the horizon is open the transcript also requests a frame every
tick, defers any re-hint past the gesture, and never re-engages tail-follow.
Every dispatched event and every
frame also appends the absolute pixel offset (`bench_list_px`) to
`scroll.offset_px` in `--bench-out`: the per-frame series whose
first differences tell hint re-basing (steps as rows measure) apart from
frame overrun (uniform large steps). The other modes drive `scroll_to`
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

`HARNESS_FRAME_TRACE=1` traces the normal window instead of the bench, to
`$HARNESS_STATE_DIR/frame-trace.log` (the harness's own
`~/Library/Application Support/harness/` when the state dir is unset), so a
hand gesture on `--replay` becomes a measurement. An earlier version of the trace was v2, one row per *centre* paint
(`t_us,list_px,events_since_last_paint,gesture_active,centre_w,rehint,draw_us`)
— it went silent through a sidebar-only or resize-only gesture once parts
4/C1/C2 stopped those from touching the cached transcript column at all, so
it could no longer see the two things that mattered.
It was replaced with v3, one row per **root render**
(`Harness::on_frame`, which runs first in `Harness::render` — i.e. once per
display tick that renders anything at all, matching part C1's zero-idle
fix): `t_us,root,pane,centre,sidebar_ix,sidebar_off,sidebar_w,resize_active,
list_px,events_since_last_tick,gesture_active,rehint,draw_us`. `root` is
always 1 (one row per root render, by construction, kept for clarity);
`pane`/`centre` are how many times the sidebar pane and the cached
transcript column actually rebuilt since the previous row — mostly 0,
which is the whole point of the caches parts 4/C1/C2 built, and a
regression there would show as this column going persistently non-zero.
`sidebar_ix`+`sidebar_off` is the sidebar list's `ListOffset`, `sidebar_w`
the divider's width, `resize_active` whether a drag is in flight,
`list_px` the transcript list's pixel offset, `gesture_active` the OR of
all three interactions' own gesture flags. `draw_us` is the *previous*
frame's render-to-paint micros, unchanged by the split.
`python3 scripts/frame-trace.py <log> [--metric sidebar|transcript|resize]`
summarises a log per interaction: the fraction of gesture-active ticks that
actually moved the metric (excluding ticks pinned at the list's own
head/tail boundary — correct end-of-list behaviour, not a dropped tick,
the same distinction `bench-scroll` makes between "stalls" and "clamped"),
the longest silent gap both in display ticks and in raw wall-clock
milliseconds, whether the momentum tail was cut, and frame-ms percentiles.

Two free scripted steps drive a gesture at true display-tick pace without a
real pointer, for measuring cadence headlessly: `sidebar-scroll-sweep:
<dy,finger_ticks,tail_ticks>` and `transcript-scroll-sweep:<dy,finger_ticks,
tail_ticks>` (the transcript/sidebar twins of the
existing `resize-sweep:`), each pushing one wheel delta into the same
accumulator a real wheel event fills, once per rendered frame, for
`finger_ticks` frames at a constant `dy` then `tail_ticks` more decaying
exponentially to 5% of `dy` (the same shape the real-`CGEvent` tool below
posts, so the two are comparable). This is the sanctioned
fallback for a machine that cannot confirm a posted event's landing
window (see "Real-window cadence" below) — free, no turn, no wire, and it
respects vsync (`window.request_animation_frame()` per tick) rather than
applying a burst's whole travel synchronously the way `wheel:`/
`sidebar-wheel:` do.

### The running dot ticks at 20 Hz, not display rate

A looping pulse ring asks gpui for another frame on every render, and the
request notifies the enclosing view — the whole cached `SidebarPane` —
whose ancestors `mark_view_dirty` dirties too, so while any session ran
the pane and the root rebuilt on every display tick. Isolating the dot as
its own entity cannot help (a child notify still walks ancestors into the
dirty set, which busts the pane's `.cached` reuse), so the sidebar takes
option (b): its dots sample one caller-owned phase per frame
(`aui_motion::pulse_phase`, the exact loop curve) and request no frames of
their own. `Harness::pulse_task` notifies the pane every 50 ms, and only
while `pulse_needed` — some session running *and* its row, its project's
rolled-up head, or its rail cell visible — ending itself on the first tick
that answers no; `on_frame` restarts it when a need appears. Reduced
motion samples the resting phase and starts no timer. Unset phases loop as
before, so the gallery and the transcript are untouched.

Measured free, idle replay plus the running stress row
(`--replay fixtures/msp/synthetic-stress-hetero.jsonl --sidebar-fixture
fixtures/sidebar/stress.json`, `HARNESS_FRAME_TRACE=1`, 10 s): 184 root
ticks (17.8/s, the 20 Hz design rate minus scheduling slack), pane rebuilt
on 181, the cached transcript column on 4 boot rows only. The same run
with an all-idle fixture reads 5 boot rows and then silence — the timer
never starts. Two screenshots a second apart show the ring tight+bright,
then wide+faint: the dot still animates. Colour and size unchanged.

### Real-window cadence

For two known complaints — sidebar scroll and divider-drag jank, both
absent from the transcript's own scroll — one investigation prepared a small Swift
CGEvent poster (`postscroll`/`postdrag`/`winfind`, scratch tools, not
committed) to drive the real window: continuous trackpad-shaped scroll
(finger phase + decaying momentum tail, `.pixel` units,
`kCGScrollWheelEventIsContinuous`) and a real left-mouse divider drag.
**On the machine this investigation ran on, real `CGEvent` posting is
unreliable for confirming which window an event lands on**:
`AXIsProcessTrusted()` and `CGPreflightPostEventAccess()` both report
`true` (event-posting permission is granted), but
`NSRunningApplication.activate()` for the target process intermittently
returns `false`, `screencapture` returns a solid black image (no attached
compositor to capture), and two `harness` windows (a fresh `--replay`
window and another long-running one) were found reporting
**identical, exactly overlapping** `CGWindowListCopyWindowInfo` bounds with
no on-screen way to confirm which one a posted event actually reached.
Where activation happened to succeed and a small control nudge landed
cleanly in this window's own trace (confirmed before any larger gesture),
real-`CGEvent` numbers were taken; the in-process sweep steps above cover
the rest and are what a script can rely on.

Numbers (debug build, branch head, `--replay
fixtures/msp/synthetic-stress-hetero.jsonl --sidebar-fixture
fixtures/sidebar/stress.json`, `HARNESS_FRAME_TRACE=1`; the tick-based
percentage can under-count on this machine — with no real display link,
`request_animation_frame` can fire faster than any genuine 60/120 Hz
compositor would present, so two consecutive rows carrying the same value
are not necessarily a dropped tick. The wall-clock gap needs no tick
calibration and is the more trustworthy number here — see the value walks
below):

| gesture | driver | ticks_with_change | longest_gap_ticks | longest_gap_ms | note |
|---|---|---|---|---|---|
| sidebar scroll | in-process sweep | 65/65 (100%) | 1 | — | clean pass |
| divider drag | in-process sweep | 39/39 (100%) | 1 | — | clean pass |
| transcript scroll | in-process sweep | 55/56 (98.2%) | 6 | — | matches the pre-existing, already-documented "48 gaps of 53–62 ms in the burst tail" note above (baseline) — not a regression; the transcript is not the target here |
| sidebar scroll | real `CGEvent` | 63/186 (33.9%) | 5 | 20.7 | value itself walked smoothly and monotonically (`sidebar_ix`/`off` advancing by the exact per-event step with no skips) — read the row-count metric's low percentage as this environment's extra renders, not dropped input |
| divider drag | real `CGEvent` | 44/96 (45.8%) | 4 | 16.8 | same: `sidebar_w` advanced by a constant ~1.7 px per real change, monotonically, no jumps, no freeze over one real ~60 Hz frame |

**Not done**: this table across three builds
(`c14819d`, `b2d82e2`, branch head) with matching library worktrees.
`c14819d` carries no `HARNESS_FRAME_TRACE`/sweep instrumentation at all
(an earlier commit) and `b2d82e2` carries only the v2, centre-scoped trace
(a later one) — neither can produce a comparable v3 row for a sidebar-only or
resize-only tick without a same-shape scratch patch in a matching
`/tmp` worktree pair (library commit `0e8f708` for `c14819d`, `987318a`
for `b2d82e2`, per their commit dates), which was not reached.
Earlier bisected evidence — free scripted `pane=`/`root=`/`drains=` counts
across those same commits — is the "before" picture that exists; it is not
a per-tick trace and is not repeated here.

Read alongside the value walks (`grep`-able straight out of `frame-trace.log`):
on this machine, both real gestures track the pointer
smoothly with no multi-hundred-millisecond freeze and no large skips — the
opposite of the reporting machine's own evidence (25–157 px jumps, gaps of
2–9 recorded frames). That gap between "smooth here" and "janky there" is
itself the headline finding: **this environment cannot reproduce
the jank**, whether because it lacks a real compositor (see above) or
because the fixture's own backgrouand render load (below) differs from real
sessions elsewhere. A separate in-process synthetic-event rig
reached the same conclusion for resize (1 frame/move, ~3.4 ms draw,
flat) — two independent methods on two different rigs both fail to
reproduce the real-machine symptom, which argues for a cause outside
anything either rig exercises: real 60–120 Hz vsync pacing, GPU
compositing, or OS-level input coalescing under real system load, none of
which a scripted or headless rig can stand in for.

**A related, unresolved observation, found while chasing the above**: with
a session open and `fixtures/sidebar/stress.json` loaded, this build's
sidebar pane was seen rebuilding on very close to 100% of root renders
(measured 192/201 and 196/203 across separate runs) for tens of seconds at
a stretch with **no gesture active and no session actually running**
(`entry.running: false` on every fixture row; the replayed session's own
`turn/started`/`turn/completed` counts matched, so it is not mid-turn
either) — ruled out as the sidebar's own explicit notify path
(`sync_sidebar_pane`'s `SidebarKey` compared unchanged across 3 notifies in
5 s of the same continuous rebuilding, confirmed by a temporary debug
print, not committed). The cached pane's `.cached()` boundary
(gpui-pre-0.3.3 `view.rs:387-390`) can also miss on a bounds/content-mask/
text-style change with no explicit notify at all, which was not reached
before time ran out; a real, separately-verified mechanism in this
codebase that unconditionally requests a repaint every tick while active —
`gpui-base-0.6.0 motion.rs:380-388`'s `animate_keyframes`, called by every
pulsing status dot (`aui-motion pulse.rs:38`'s `looping()`, `aui data/
dot.rs:41`) whenever `AgentState::Running` — was confirmed **not** the
cause here (no row in this fixture is `running`), but is worth knowing
about for any future chase of this: `gpui-pre-0.3.3 window.rs:2525-2528`'s
`request_animation_frame()` notifies `self.current_view()`, which while
inside `SidebarPane`'s `.cached()` prepaint scope is the *whole pane*, not
just the pulsing dot — so on a real session with a running turn visible in
the sidebar, that mechanism alone would force a full sidebar rebuild every
tick, independent of any user gesture. Not patched (registry crate; would
need the dot to own its own inner cached scope so its self-requested
repaint stops at itself). Filed for a follow-up rather than chased further
at the time.

Baselines (before any fix, on an instrumented branch; each run
with its own throwaway `HARNESS_STATE_DIR`). `bench-draw` is whole-frame
render-to-paint in µs; `bench-element` beside it is 3–12 µs p50 everywhere,
so the two orders of magnitude between them are row building, layout and
paint — the blind spot the instrument closes:

| fixture | scroll | debug shell | debug bare | release shell | release bare |
|---|---|---|---|---|---|
| hetero | wheel | p50 2140 p90 2918 max 31398 | p50 1605 p90 2589 max 9057 | p50 1606 p90 2082 max 14417 | p50 1348 p90 2245 max 8684 |
| hetero | sweep | p50 2658 p90 3233 max 14003 | p50 2274 p90 2931 max 8464 | p50 2450 p90 3077 max 16924 | p50 2113 p90 2742 max 8117 |
| stress-300 | wheel | p50 2315 p90 2847 max 15982 | p50 1721 p90 2575 max 9190 | p50 1367 p90 2493 max 15258 | p50 1468 p90 2434 max 8088 |
| stress-300 | sweep | p50 2595 p90 2909 max 15247 | p50 2206 p90 2604 max 8859 | p50 2486 p90 2766 max 14156 | p50 1981 p90 2524 max 8008 |

Shell ≈ bare + 0.4–0.6 ms p50 in every cell — the sidebar/header/composer
rebuild costs half a millisecond on these replayed sessions, already near
the ≤ 1 ms goal at baseline. `bench-scroll` travel is identical
shell vs bare: 696 events, 793 frames (hetero) / 999 (stress-300), no
jumps/stalls/clamps, every burst kept 100 % — except stress-300
`down-clean` at 90.5 % in all eight runs, the known small asymmetry, unchanged. `bench-idle` is 0
everywhere; phase waits resolve on frames (793/999 frames for 696
events), stream 8–15 s with none of the old 2 s artifact.

The offset series (`scroll.offset_px`) shows H-1 on the hetero climb:
phase (c), 300 × +20 px upward over unmeasured rows, has first-difference
steps up to 1062 px against 20 px events — one event's travel applied
against a ruler that re-based as rows measured — while phase (d), the exact
mirror downward over now-measured rows, steps a clean 20.0 px max. Travel
is conserved ((d) returns to the tail), so the steps are re-basing, not
loss. Stress-300 shows the same shape small (111 px in (c), 20 px in (d)).
The ~6 k boundary steps are the drive re-centring for the bursts, not
scroll behaviour. Every wheel run also carries 48 gaps of 53–62 ms in the
burst tail (all profiles, both roots; `dropped=48`, sweep runs 0) — the
synchronous batch-dispatch pattern meets no frame within the 50 ms wait, so
each repeat's gap spans the timeout; the old 2 s timeout masked it.
Mechanism unresolved; p50/p90 are unaffected.

After the fixes (same-day same-machine before/after, each run with its
own throwaway `HARNESS_STATE_DIR`;
`bench-draw` and `bench-frame` are p50/p90/max, draw in µs, frame in ms):

| fixture | scroll | debug shell | debug bare | release shell | release bare |
|---|---|---|---|---|---|
| hetero | wheel | 1857/2962/17902 (was 2092) · frame 2/8/61 | 1559/2765/10801 (was 1601) · frame 1/8/61 | 1554/2778/17245 (was 1790) · frame 2/8/61 | 1343/2690/9293 (was 1338) · frame 1/8/60 |
| hetero | sweep | 2107/2504/12789 (was 2269) · frame 7/7/8 | 2046/2498/8382 (was 2036) · frame 7/7/8 | 1911/2320/12392 (was 2067) · frame 7/7/8 | 1969/2585/7950 (was 1925) · frame 7/7/8 |
| stress-300 | wheel | 2039/2762/14824 (was 2233) · frame 6/8/60 | 1798/2747/8149 (was 1842) · frame 6/8/59 | 1935/2778/13358 (was 2019) · frame 6/8/59 | 1672/2560/7836 (was 1592) · frame 6/8/58 |
| stress-300 | sweep | 2314/2758/14771 (was 2455) · frame 7/8/9 | 1740/2130/8126 (was 1770) · frame 6/7/8 | 2165/2613/13367 (was 2285) · frame 7/8/11 | 1589/2013/7984 (was 1607) · frame 6/7/10 |

Shell frames improve 84–236 µs p50 in all 8 cells (the cached column);
bare wheel frames move −44…+80 µs (fewer seeks, inside run noise); bare
sweep controls move ±44 µs at most — that path touches neither change, so
the methodology reads clean. Shell ≈ bare + ≤ 0.6 ms p50 everywhere; the
`ComposerPane` split is not needed. `bench-frame` is unchanged before/after
in every cell (the 53–62 ms maxes are the 48 burst-timeout gaps, the known
instrument artifact). `bench-scroll` phase indices are sample-identical
before/after on both captures and both roots (hetero `239 230 239 224 239`,
stress shell `622 581 622 554 622`, bare `621 581 621 553 621` — the one-row
shell/bare difference is the narrower transcript column wrapping
differently); every burst keeps what it kept before, including stress-300
`down-clean` at 90.5 %. `bench-idle` is 0 in all 32 runs. The offset
series is sample-identical too: phase (c) still steps 1062.0 px (hetero) /
111.0 px (stress-300) at the same sample, phase (d) a clean 20.0 px — H-1
re-basing is untouched, as it must be while gpui exposes no per-item hint
(§5), and travel is conserved. The bottom clamp needs no code: a burst into
the end stops at the end and the next upward event moves at once (zero
stalls/clamps across all phases).

What the bench cannot show is the cadence itself — one synthetic event per
frame earns one drain by construction, so the 6.5× application saving only
materialises where events share a frame (bursts, and any real gesture). The
hand gesture is the verdict: `HARNESS_FRAME_TRACE=1 cargo run -p
harness -- --replay fixtures/msp/synthetic-stress-hetero.jsonl`, scroll up
for 2 s, lift, wait for the tail; then `python3 scripts/frame-trace.py`.
This passes at paints/s ≥ 55 during the tail and ≥ 110 during the
finger phase on the 120 Hz panel, no gap > 2 ticks, last applied event =
last delivered. The trace plumbing is verified: a scripted
`--steps "wait:…;wheel:600;…"` run writes one row per paint with the wheel
events on their own rows (`events_applied=3 paints_with_events=3`), and the
script parses it.

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
they answer with a toast. Either role's Copy holds its button on the success
check for the library's `COPY_HOLD` (1.2 s, the same hold the code-block
header keeps on its own) and then clears it — at most one turn holds the
check at a time — so the tick reads as a confirmation and the copy glyph
returns. `copy:<turn>` (0-based, like `select-span`) presses the button for
a screenshot; it is free.

Every turn the wire timed carries its age beside that row: the user bubble's
caption under it, the assistant's last cell in the footer. The fold keeps the
item's `recorded_at` (RFC3339) on the turn — the latest revision of each item
wins, so live and backfilled transcripts agree, and the turn keeps the
earliest across its items — and the harness formats it in words (`just now`,
minutes, hours, `yesterday`, else the date) against one clock read once per
frame (`transcript_now_ms`: wall time, or the newest reported stamp under
`HARNESS_DETERMINISTIC=1`, the sibling of the sidebar's `grouping_now`). A
turn the wire never timed draws no caption and no extra cell, keeping its old
height.
Markdown links click through: URLs open in the browser, file paths open in
their default place — folders in Finder, files in their default app.
Absolute paths open as is, even outside the session workspace; relative
ones resolve against it. Only an existing path ever opens — through the OS
dispatch, never executed and never created (these paths come from model
output) — and a missing path toasts quietly. `Block::ToolGroup` renders through the library `tool_group`,
its open state in `Folds` keyed by the group's fold key. The transcript holds
one cross-block span per turn (keyed by turn id with its markdown source,
plus one drag session per turn): a drag that starts in one paragraph and
ends in another — or in a code block — highlights everything between through
the turn's `span_selection(..)`/`on_span_event(..)`, a plain click clears,
and a hover or pick that commits in one turn clears whatever another held,
so there is ever one span. ⌘C in the transcript context copies
`turn_span_selected_text` of the held turn — document order, a blank line
between blocks, list markers kept on wholly selected items, code byte-exact
(never from the composer or a card field) — and Escape clears it. Per turn
because the library scopes cell keys (`p0`, …) to the markdown view that
rendered them — one shared span would light up every turn at once; keyed,
not positional, so the span survives the transcript scrolling mid-drag, and
span mode adds no per-frame layout cost (the order walk is keys only, no
shaping). Link clicks and code-block copy buttons are unaffected: a press
opens the drag session without disturbing the held span. `--steps top`,
`end`, `expand-groups`, `select-text:<turn>:<from>-<to>` (a scripted hold
over the turn's first paragraph, now travelling the span path — verb,
arguments and screenshot unchanged) and `select-span:<turn>` (the whole turn
as one cross-block span) drive screenshots.

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

The status row while a turn runs is one calm line: the most specific phase
the fold can source truthfully, with the elapsed time and the `esc to
interrupt` hint, plus the queued count when there is one. A pending approval
reads "Waiting for approval…", an unanswered question "Waiting for your
answer…"; a running tool names its family ("Running command…", "Running
edit…", an MCP tool by the server's own tool name; a group with several in
flight reads "Running tools…"); a growing reasoning trace reads "Thinking…".
Nothing else is named — no guessed activity, no per-file verbs — so the row
never states what the wire did not say, and the default remains "Working…".
Once the running turn's reply has fully arrived but the turn is still open —
Muse runs `reminderChild` items (memory reminders) for 30–70 s after the
`agentMessage` before `turn/completed` — the row reads "Finishing up…"
instead, with the same clock and hint and a "memory reminders" note, so the
quiet tail does not read as stuck.

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
keymap. Harness (About, Services, Quit), File (Add Project…, New, Settings…
⌘,, Close),
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
