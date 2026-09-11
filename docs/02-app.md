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
| `--steps <a;b;c>` | drive the open session from the command line, so a screenshot is reproducible (`docs/03-composer.md` §1, `docs/04-approvals.md` §7). |
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
| — | | the `Overlays` entity the spec names is, in this phase, one `Option<Dialog>` field on `Harness`: there is one modal, no palette, no menus and no toasts yet. It becomes its own entity when Phase 3 adds the menus. |

Everything else is a pure function. `src/transcript.rs` turns a `Turn` or a
`Block` into elements; `src/sidebar.rs` turns the session list into the
library's date grouping; `src/conn.rs` classifies errors; `src/auth.rs` and
`src/index.rs` are I/O with no UI in them at all.

Rendering is a pure function of state each frame: the view reads
`MuseFold::session` and `MuseFold::side` and rebuilds the transcript. Nothing is
cached between frames except the scroll position and the fold set.

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

## 5. Sessions (spec §3.7)

The workspace is the app's own — `--workspace <path>`, defaulting to `$PWD` —
and `session/list` is filtered to it with `workspaceRoot`.

`session/list` gives identity and timestamps and nothing a person can read. The
words come from `~/.local/share/muse/session-index.db`, opened **read-only** and
treated as a cache: a missing file, a schema this build has never seen, and a
database another process has locked all yield an empty map, and the sidebar
falls back to `Session <first id group>`. Nothing ever writes to it.

The rows are the library's `SessionSummary`, grouped by **calendar day** —
Today / Yesterday / This week / This month / Earlier — and rendered by
`nav::sidebar_view` in its date grouping, with a **Pinned** group first
whenever a session is pinned (the library partitions `pinned` rows out of the
date buckets itself).

Every row carries one muted second line: `last_summary` when a turn completed
in this app, else the index's first prompt — but only when the row's label is
not that same prompt (a user-given name or a Muse title); otherwise the row
shows the "N turns" meta alone, never a repeated first line. Whatever shows
goes through the row's own one-line cap. `last_summary`
is written on `turn/completed` from the first line (≤ 120 chars) of the last
assistant text block: the fold is already in memory, so it costs no model
call, and it persists through `sessions.json` beside the name, the hidden and
archived flags, the pin and the derived title.

Above the Sessions caption sit two `nav_item` rows: **New session** (Plus,
the ⌘N in the sidebar) and **Automations** (Zap) with a muted "Soon" tag —
a placeholder with no destination yet, so it answers with a toast saying so.
The caption's sliders icon opens the **view menu**: Show empty (n) / Hide
empty, Show hidden (n) / Hide hidden (the legacy `/hide` rows), Clear empty,
and Show archived (n) / Hide archived. Toggles keep the menu open so the
check is seen to change; Clear empty closes it and hides through the same
`hidden = true` override as `/hide`, with the same eight-second Undo.
Archived sessions are excluded from the list and from Clear-empty; shown,
they carry a muted Archived tag and their Archive tray action puts them back.

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
New and Search cells, a separator, one dot per running session mirroring the
rows' selection and pulse, and the account avatar. The Search cell opens the
full-text search palette, exactly as ⌘⇧F does — there is no sidebar
quick-filter, so the two can never be open together. The window-level
`--steps` verbs for all of this are `sidebar` (toggle), `overflow`,
`view-menu`, `account`, `pin`, `archive`, `archive-confirm`, and
`show-archived`, beside the older `search`,
`palette`, `resume`, `fork-picker`, `rename`, `hidden` and `empty`.

Opening a session is `session/resume { excludeItems: true }` to attach, then
`view/page` forward from the beginning of the view, paging on `nextCursor` until
it runs out, feeding every event through the fold. A "Loading history…" status
row sits under the transcript while that runs. Resume attaches; `view/page` is
the path that is contiguous, ordered and bounded, and it never replays
`item/delta`, so a backfilled message arrives whole and the fold takes it that
way.

⌘N starts a new session in the workspace; ⌘B toggles the sidebar rail.
⌘⇧F opens the full-text search palette (`PaletteKind::Search`): sessions by
transcript text plus the files the workspace's turns created, Enter resumes a
session and reveals a file in Finder. Full detail in `docs/12-search.md`.

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
the only set. The centre header shows the active session's label ("Harness"
with nothing open) with the provider mark and the overflow "…" menu (Rename,
Fork, Archive); the title flexes inside the header cell and elides to one
line, so a whole first prompt as the label can never push the overflow button
out. Renaming the open session swaps the title for the same dense single-line
field the sidebar row uses, through the same confirm/Escape path. There is no
right-pane toggle and no right-header close button — the right pane's slot
stays empty and `right_open` stays false. The
shell wraps the header row in its drag region, so press-drag moves the window
and double-click zooms while the buttons and the rename field keep their
clicks. Collapsed, the rail column is 48 px and the centre title stands 14 px
off so it clears the native lights (x 9–61), chosen from the collapsed
screenshot rather than from guesswork.

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

Auto-scroll is tail-follow, the same rule the block terminal uses: anything new
scrolls the list to the bottom, but only for a reader who was already within a
few dozen pixels of it.

The transcript list is virtualized (2026-09-10): `render_transcript` renders a
gpui `list()` with a persistent top-aligned `ListState`, one item per turn,
instead of building every cell every frame. Top, so a short transcript starts
at the top instead of leaving a void above it; tail-follow is the `follow`
flag (`scroll_to_end` when the reader was at the tail), never the alignment.
The list wrapper carries the pre-virtualised container's own gutters
(`TRANSCRIPT_PAD_TOP`/`TRANSCRIPT_PAD_X`, `SP_4` below), so the turns line up
with the status and banner rows. Fold changes `splice` the affected
range only, and only visible rows are laid out per frame, so per-frame cost
stays bounded as the transcript grows. `apply` notifies only when the fold
changed or view state changed (unchanged streaming deltas earn no frame), the
turn ticker runs at 1 Hz and notifies only when the displayed second changes,
and unchanged turns are never re-parsed (the library memoises markdown).
`HARNESS_FRAME_STATS=1` prints render-time percentiles to stderr; the
`synthetic-stress-300.jsonl` capture (~300 turns, generated by
`fixtures/msp/make-stress-300.py`) is the benchmark.

A session switch never flashes the empty state: `Harness::open` keeps the old
view rendered until the new session's first backfill batch applies (marked by
`SessionEvent::HistoryReady`), then swaps; with no old view a neutral loading
row stands in, and a failed switch keeps the old view with the error banner.

Turns carry an in-flow action row under the prose (`actions_bottom`) for both
roles. Assistant: Copy writes the turn's text to the clipboard, Retry resends
the user input behind the turn, Fork opens the turn picker; Pin is hidden on
turns through the library's `AssistantTurn::actions(..)` (it lives on sidebar
sessions, whose rows keep the Pin action). User: Copy, Edit (text into the
composer draft), Resend. Wire actions are live-only: in a replayed capture
they answer with a toast.
Markdown links click through: URLs open in the browser, workspace paths reveal
in Finder (escapes above the workspace are rejected with a toast, missing
paths toast). `Block::ToolGroup` renders through the library `tool_group`,
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
`esc to interrupt` hint, plus the queued count when there is one.

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
keymap. Harness (About, Services, Quit), File (New, Close), Edit (the
standard six with `OsAction`), View (sidebar, palette, search, theme),
Window (Minimize, Zoom), Help (Harness Documentation, which reveals the
`docs/` folder in Finder). ⌘W runs the tier-probe cleanup and
`remove_window`; ⌘Q runs the same cleanup and `cx.quit()` — the same pair
the window's should-close hook and the app-quit hook run
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
