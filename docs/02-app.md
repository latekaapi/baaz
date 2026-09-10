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
`model/list`, the login child and the sqlite read all have the same shape: a
background task, then one `update` that folds the answer in.

**Ack ≠ outcome.** Nothing gates folding on an ack. `turn/start` comes back with
a `turnId` and a disposition, and that is all it is used for: the identity, and
whether the submission was queued. What the turn is *doing* is always the view
event.

---

## 4. Auth (spec §3.2)

The boot probe is two halves, and both must pass:

1. `~/.config/muse/auth.json` has `providers.meta` (or `META_API_KEY` is set,
   which takes priority and is reported as the "API key" identity);
2. `model/list` reports `source: "providerCatalog"`, which means the catalog was
   fetched with a live credential.

Either missing → the login screen (`aui::screens::login`).

The login screen spawns `muse login` with `MUSE_LOGIN=1` — without it the
launcher refuses to prompt when stderr is not a tty — and parses **stderr**,
which is where the device-code flow prints. The parser is a small state machine:
`Open this page to sign in:` claims the next non-empty line as the URL,
`Confirm this code matches:` / `Enter this code:` claims the next as the code,
`Waiting for approval (…)` carries the expiry, `Signed in.` is success and any
`muse: …` line is the failure. SGR escapes are stripped, because the launcher
bolds the code through `tput`.

**The URL and the code are never logged.** They go from the child's stderr into
the screen's state and onto the screen, and the URL additionally into `open`'s
argv. Nothing writes either to stdout, stderr or a file.

On success the app **respawns `muse serve`**: the credential is ambient and the
old child inherited none, so the connection has to be made again before it can
be used. Then the probe runs again and the app enters.

The sidebar footer shows `user_full_name` over `user_email` and carries a
"Sign out" button, which runs `muse logout` and returns to the login screen.
The plan row carries the list toggles: "Show hidden (n)" once something is
hidden, and "Show empty (n)" once a session with no turns exists — both
`ghost().xs()` buttons that appear only when their count is above zero. The
buttons stack in short right-aligned rows — hidden row, empty-toggle row,
"Clear empty" row — because the plan label keeps a fixed width and
truncates first. While the empty rows are on screen the toggle reads "Hide
empty" with no count, and "Clear empty" on its own row hides them all
through the same `hidden = true` override as `/hide`, with the same
eight-second Undo.

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
`nav::sidebar_view` in its date grouping.

Opening a session is `session/resume { excludeItems: true }` to attach, then
`view/page` forward from the beginning of the view, paging on `nextCursor` until
it runs out, feeding every event through the fold. A "Loading history…" status
row sits under the transcript while that runs. Resume attaches; `view/page` is
the path that is contiguous, ordered and bounded, and it never replays
`item/delta`, so a backfilled message arrives whole and the fold takes it that
way.

⌘N starts a new session in the workspace; ⌘B toggles the sidebar rail. Rename,
hide and search are Phase 5 and are not built.

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
