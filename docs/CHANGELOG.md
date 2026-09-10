# Harness changelog

## 2026-09-10 — Improvements (A) — shell: header and sidebar

The header shows the active session's label ("Harness" with nothing open) with
the provider mark and an overflow "…" menu (Rename, Fork, Archive); the shell
paints no traffic lights of its own, so only the window's native set remains.
There is no right-pane toggle and no right-header close button — the right
slot stays empty with `right_open = false` (the library always paints those
two icons, so the shell uses a plain header cell with the same title
construction instead of `centre_header`/`right_header`). The shell's drag
region wraps the header row: press-drag moves the window, double-click zooms,
and the buttons keep their clicks. Collapsed, the column is the library rail
(`flat(true)`: New and Search cells, a separator, one dot per running session,
the account avatar) at 48 px, and the centre title stands 14 px off so it
clears the native lights.

Above the Sessions caption sit New session (Plus, ⌘N) and Automations (Zap,
muted "Soon" tag — a placeholder that answers with a toast). The caption's
sliders icon opens the view menu: Show/Hide empty, Show/Hide hidden (legacy
`/hide` rows), Clear empty, Show/Hide archived. The footer is the library's
account row again — avatar, name, email, plan row, the tier probe's weekly
fraction as the usage meter (omitted while unknown, chevron always shown) —
and opens the account menu whose only row is Sign out.

`SessionMeta` gains `pinned`, `archived` and `last_summary` (all camelCase,
old files still read); row actions are Pin, Rename and Archive, with a Pinned
group first and a muted Archived tag on shown archives. Archive asks first
through a danger dialog and offers an eight-second Undo on a toast, sharing
one undo stack with hide/Clear-empty; archiving the open session opens the
newest remaining visible session (or the empty state), and archives stay out
of Clear-empty. Every row shows one muted second line — `last_summary`, else
the first prompt, else the derived title — written free from the in-memory
fold on `turn/completed` (first line, ≤ 120 chars). Rename uses the library's
`dense_field` in its 22 px wrapper, so the editing row keeps 30 px. Row
gutters measure 8 px left / 8 px right (±1 for radius) on the replay capture,
so no harness container change was needed beyond the library's row-idiom fix.
New `--steps` verbs: `sidebar`, `overflow`, `view-menu`, `account`, `pin`,
`archive`, `archive-confirm`, `show-archived`. No key bindings changed, so
`docs/08-keymap.md` is untouched. `docs/02-app.md` §4–§5 describe the header,
rail, nav rows, view menu, pin/archive and footer.

- Screenshots (all `--replay fixtures/msp/transcript-real.jsonl`, 15 s
  delay): `docs/images/improve-shell-{open,collapsed,overflow,viewmenu,
  account,rename,dialog,archived,pinned}-{dark,light}.png`.
- Covered by five new offline unit tests (`describe` precedence and cap,
  pin/archive/summary store retention and camelCase round-trip, weekly
  fraction); the description line itself is screenshot-proven only in
  structure — replay rows carry no summary, and writing one needs a live
  completed turn.
- Not done: rail dots cover running sessions only (the harness tracks no
  waiting state); header menus are click + Escape driven with no arrow-key
  selection; the view menu floats at a fixed offset under the caption rather
  than anchored to the sliders icon.

## 2026-09-09 — Improvements (E) — fork picker

## 2026-09-09 — Improvements (E) — fork picker

`/fork` used to fork the newest completed turn with no say in the matter; the
TUI picks from a list, and now so does the harness. Typed bare, or picked from
the `/` menu, `/fork` opens the turn picker over the command palette's
`PaletteKind::Fork`: the session's completed assistant turns, newest first,
each row the first line of the user prompt that started the turn plus the
turn's wall-clock time in footer words. Picking a row forks that turn through
the same `session/fork` call as before — the new session still opens only when
the server's resume envelope arrives, so nothing is optimistic. Typed with a
number (`/fork 2`), it skips the picker and forks the nth newest completed
turn directly (`1` is the newest); a number with no turn behind it is a banner,
never a fork of whatever the server thinks is newest. The picker rows come
from one `SessionView::fork_turns` helper that the direct path reuses, so the
picker and `/fork <n>` can never disagree about what "newest" means. Scripted
as `--steps fork-picker`, beside the window's other palette verbs.

- Screenshot: `docs/images/improve-fork-picker-dark.png` shows the open picker
  over a replayed real capture (`fixtures/msp/transcript-real.jsonl`), taken
  with `--replay` so it cost nothing.
- Covered by three offline unit tests (the `/fork [n]` parse, the row label,
  the row time); no test opens a real session.

## 2026-09-09 — Improvements (D) — silent reasoning footer

Improvement candidate 3 in `docs/09-handoff-improvements.md` §8: a turn can
bill reasoning tokens and emit no `reasoning` item, and the footer showed the
count with nothing saying the thinking happened off-screen. A finished
assistant turn with `reasoning_tokens > 0` and no `Block::Thinking` now gets
the same footer line with the reasoning cell reading
`419 reasoning, thought silently` — one line, in the footer's own style
(mono, `FS_11`, `ink_4`). A first cut drew it as a second row under the
footer, repeating the count and doubling the height; the fix folds the note
into the footer's own cell. Turns with a visible thinking card keep the
plain `419 reasoning` cell.

The decision is the pure `transcript::silent_reasoning` in
`crates/harness/src/transcript.rs` (assistant turn, count above zero, no
thinking card), covered by six unit tests; the fold already accumulated the
count (`TurnUsage::reasoning` → `TurnMeta::reasoning_tokens`), so no snapshot
changed. `fixtures/msp/transcript-real.jsonl` is the real capture that
exercises it: 419 billed reasoning tokens, no `reasoning` item. Screenshots
`docs/images/improve-silent-reasoning-{dark,light}.png` are `--replay` of that
capture. Footer documented in `docs/02-app.md` §6.

## 2026-09-09 — Task C — "Show full output" for truncated tool output

muse 1.1.1 keeps the full bytes of a truncated tool/user-shell output under an
`outputRef`, fetchable with `item/readOutput` (`docs/10-msp-1.1.1-diff.md`).
The fold now records that handle: a `toolCall`/`userShell` item completing
with `truncated: true` and an `outputRef` is kept in a per-session
`itemId → OutputRef` map on the fold (`MuseFold::stored_output`), keyed by the
id its `ToolCall` block renders as. The map is not part of the replay
snapshot, so no existing snapshot changed.

The affordance lives in the existing card's own action slot — no library
change. The tool card's fold row already emits `ToolCardIntent::Unfold`, so on
a card whose item truncated with an `outputRef`, that intent is "Show full
output": the app pages `item/readOutput` on a background task (02-app §3),
concatenates `utf8` pages (decoding `base64` binary pages first) up to a 2 MiB
cap, and replaces the card's body on the server's result (D4 — nothing is
optimistic). Past the cap the body ends with "…truncated at 2 MiB". A second
press mid-fetch is ignored; a failed fetch reports its banner and keeps the
truncated body; a replayed capture refuses the fetch read-only like every
other command. The row's label stays the library's ("N more lines") — the
library owns the card's text.

No capture carried an `outputRef`, so `fixtures/msp/synthetic-readoutput.jsonl`
is new: every `<-- ` line is hand-written to `msp.d.ts` (a completed `bash`
tool call with ten visible lines, `truncated: true` and an available
`outputRef`); only the item envelope mirrors the real bash item in
`transcript-real.jsonl`. It folds to a checked-in replay snapshot, and
`docs/images/improve-full-output-dark.png` replays it. Covered by a fold
integration test (`stored_output` keeps `out-syn-1`) and unit tests for page
concat, base64 decode, and the empty case in `full_output.rs`.

Compile note: the shared `agentic-ui` checkout already carries sibling Task A's
`effort-max` branch (`ReasoningEffort::Max`), so two exhaustive matches gained
one-line `Max` arms (`effort_wire`, `effort_detail`) to keep this branch
building. The picker tiers themselves are Task A's change, not this one's.

## 2026-09-09 — window-close probe kill (task B)

The tier-probe-leak fix below closed the `--screenshot` and `--print-tier`
exits but left the interactive ones: closing the window or quitting mid-probe
ran no hook, so the `muse` TUI child — its own session leader — was orphaned
(the leak entry names this as its residual). `main.rs` now registers both
hooks after opening the window: `on_window_should_close` for the red dot
(macOS does not quit when the last window closes) and `cx.on_app_quit` for
Cmd+Q, Dock quit and `cx.quit()`, each calling the same `kill_live_probes`
plus bounded 3 s `wait_for_probes_gone` pair as the screenshot path. Both are
idempotent and the quit proceeds when the wait expires. Verified by reading
the hook path against the gpui-pre 0.3.3 sources; no live probe was run, per
the spend rule.

## 2026-09-09 — effort picker offers max (task A)

Muse 1.1.1 added `max` to the closed MSP `ReasoningEffort` vocabulary between
`xhigh` and `ultra`, and the wire client already accepted it — but the effort
picker still omitted it because its tiers are the library's
`aui_protocol::ReasoningEffort`. The library (agentic-ui branch `effort-max`)
gained `ReasoningEffort::Max` with the `Max` picker label, the gallery sample
rows list it, and the protocol test pins the `"max"` wire string. The harness
maps it end to end: `EFFORTS` lists `Max` between `Xhigh` and `Ultra`
(8 → 9 slots with the leading Default), `effort_detail` describes it, and the
wire map sends `Wire::Max`. This retires the picker-omits-max tail of
`docs/01-transport.md` §4 item 6 and the out-of-scope note in
`docs/10-msp-1.1.1-diff.md`; `docs/03-composer.md` now lists `max` among the
effort rows. No live turn was spent: replay/unit gates only.

## 2026-09-09 — tier probe leak

The billing probe orphaned its `muse` TUI. The probe runs on a background
thread (`probe_tier` → `background_spawn`), while the process can leave
without it: a `--screenshot` run quits ~3 s in, long before the ~11–20 s
probe finishes. The quitting process kills the thread without running
`Pty`'s `Drop`, and the child — its own session leader since `pre_exec`'s
`setsid` — is reparented to pid 1 and lives on. Two such orphans were found
alive after 6 hours; they ignored SIGTERM and died on SIGKILL. Reproduced on
the screenshot boot path (orphan at ppid 1, confirmed); the `--print-tier`
path was always clean (synchronous probe, `Drop` runs before `exit`).

The fix, in `crates/harness/src/tier.rs` plus the two exit paths: the `Drop`
SIGKILLs explicitly (the TUI ignores SIGTERM) and reaps; every probe writes
its child's pid to `tier-probe/probe.pid` and removes it on drop, and each
new probe SIGKILLs a previous pid whose command line still names the probe
workspace — never on the pid alone. `--print-tier` joins a probe thread with
a bounded wait (`PROBE_CEILING` + 5 s) before exiting; the screenshot's quit
SIGKILLs live probes and waits, bounded, for their drops, so the pid file is
gone too. The pump stops on a dead child (`try_wait`), so a killed probe
finishes within a tick. Covered by three offline unit tests (pid-file parse,
sweep-only-a-probe, drop-SIGKILLs-`/bin/sleep`); no test opens the real
`muse`. Residual: closing the interactive window mid-probe still orphans —
the next probe sweeps it.

## 2026-09-09 — muse 1.1.1 schema

The `muse` CLI self-updated 1.0.3 → 1.1.1, so the MSP schema exports were
regenerated in place (`fixtures/msp/msp-ts/msp.d.ts`,
`fixtures/msp/msp/{manifest.json,msp.schema.json}`) and `schema.rs` absorbs the
whole diff. Fingerprint `sha256:0331…758b7` → `sha256:c669…03e6a4f` (manifest,
corroborated by a free `initialize`). The diff is purely additive: 2 new
methods (`item/readOutput`, `view/subscribe`), 1 new notification
(`session/modelRouteUnserved`), 6 new types, 3 new enum variants, 4 new fields —
including `ReasoningEffort.max`, which retires the §4 item 6 discrepancy in
`docs/01-transport.md`. Both new methods have typed params/results and
`MuseClient` wrappers; the new notification is indexed but deliberately has no
fold arm (the fold ignores unknown methods). No behaviour fix was needed:
framing, ids, `session/list`, approval/userInput shapes and error kinds are
unchanged. The 1.0.3 captures replay untouched, with no snapshot regeneration.
Full write-up: `docs/10-msp-1.1.1-diff.md`.

## 2026-09-09 — Improvements — sidebar noise

Screenshot and test runs leave dozens of sessions that never had a turn, so
the sidebar was mostly noise. Sessions with no turns are now hidden by
default. A session counts as *empty* when it has no turns, is not the open
session, is not running, and has no user-given name (`SessionMeta.name`) —
the rule lives in one pure function, `SessionEntry::is_empty`, and the open
session is never filtered, so a session just created stays visible.

- **"Show empty (n)"** in the sidebar footer, next to "Show hidden (n)",
  same style, shown only when n > 0. `/empty` (in the `/` menu, and typed in
  full) toggles the same filter.
- **"Clear empty"** on its own row below the toggle, shown only while the
  empty rows are on screen: it hides every currently-empty session through
  the existing `hidden = true` override, so it persists, with a toast whose
  one Undo restores the whole batch. Rows already hidden stay out of the
  batch. The footer buttons stack in short right-aligned rows (hidden row,
  empty-toggle row, clear row) because the plan label keeps a fixed width
  and truncates first: one shared row squeezed it into an ellipsis.
- While the toggle is on it reads **"Hide empty"** with no count — the rows
  are on screen, so the count is redundant, and the width is needed for the
  plan label. Off it stays "Show empty (n)".
- The sidebar's empty state names its own toggle when only empty sessions
  were filtered out: "Only empty sessions here" / "Turn on “Show empty”
  below to see them."
- Screenshots: `docs/images/improve-sidebar-empty-{dark,light}.png` show the
  footer with the toggle off; `docs/images/improve-sidebar-empty-on-{dark,light}.png`
  show it on, with "Hide empty" and "Clear empty" on their own rows next to
  the fully readable plan label. The on-state shots are taken in the
  `/private/tmp/harness-ws` scratch workspace: it holds stable empty
  sessions, while `muse serve` 1.1.1 prunes freshly started turn-less
  sessions before any later run can photograph them (see the footer note in
  `docs/02-app.md`).

## 2026-09-08
- Spec frozen: docs/00-spec.md. Wire captures and schema exports from muse 1.0.3 added under fixtures/msp.

## 2026-09-08 — Phase 1: transport and fold

Two crates, no UI. `crates/muse-client` (the `muse serve` child, NDJSON JSON-RPC,
a typed Rust surface for all 186 types in `msp.d.ts`) and `crates/muse-adapter`
(`MuseFold`: MSP view events → `aui_protocol::Delta` + `SideState`), plus the
`aui-protocol` extensions on agentic-ui's `muse-support` branch. Every capture in
`fixtures/msp/*.jsonl` replays through the fold to a checked-in snapshot with no
`Generic` fallbacks; `live_echo` drives a real `muse serve` on the free echo
provider. Full detail in `docs/01-transport.md`.

Discrepancies recorded per the spec's precedence rule (capture wins):

- **`muse serve --no-session-log` emits no view events.** The spec names it as
  the flag tests should use. Under an ephemeral host a `turn/start` is accepted
  and `session/started` is the only notification that ever arrives — no
  `turn/started`, no items, no `turn/completed`. Verified through `muse-client`
  and independently through `fixtures/msp/probe.py`. The probe and `live_echo`
  therefore run **durable**; `HARNESS_PROBE_EPHEMERAL=1` reproduces the silence.
- **An approval's `itemId` is its own id, not the gated item's.** The research
  doc §7.2 implies otherwise. The `userShell` item it gates has a different id
  and no back-pointer; only `toolCall.approvalId` joins the two, and only for
  model tool calls.
- **An approval's `turnId`, for a user shell, is the shell item's `commandId`**
  even though the item's own `turnId` is `null`. Nothing in `msp.d.ts` says this;
  the fold relies on it to file the shell card and its approval in one turn.
- **Five schema-optional fields are always present on the wire**, sometimes as
  `null`: `Item.turnId`, `SessionTokenUsageParams.modelId`,
  `SessionBranchChangedParams.branch`, `SessionModelChangedParams.providerId`,
  `SessionGoalChangedParams.goal`. Modelled as required-nullable.
- `UnframedViewNotificationParams` needs a `serde(flatten)` catch-all or every
  `view/page` event loses its payload.
- Confirmations of the research doc, not contradictions: `onRequest` raises no
  approval for a model-issued `ls`; a turn can bill reasoning tokens and emit no
  `reasoning` item; `session/started`, `approval/request` and `userInput/request`
  are all on the wire and all absent from the published method index.

Spend: zero real-provider turns. Everything ran on `echo`.

## 2026-09-09 — Phase 2: shell, sessions, streaming

`crates/harness`, the gpui app. It boots as `aui/examples/minimal.rs` does, opens
one 1440×900 window titled "Harness", and drives a real `muse serve` through
`muse-client` and `MuseFold`. Auth probe and device-code login screen, the
sessions sidebar filtered to the workspace and enriched from the local index,
resume with full `view/page` backfill, a real streaming turn with tool cards and
the per-turn token footer, stop with retract, the reconnect procedure, and the
error banner/dialog split of §3.8. Full detail in `docs/02-app.md`.

Spend: **two real `meta` turns** (of the five the spec allows), both in
`docs/images/phase2-turn-*.png`. Everything else ran on `echo`
(`HARNESS_PROVIDER=echo`).

Findings and decisions worth keeping:

- **`session/list` filters `workspaceRoot` on exact string equality.** A session
  started in `/tmp/x` and one started in `/private/tmp/x` are two different
  workspaces to the wire, even though the index records the same
  `workspace_key`. The app therefore canonicalizes `--workspace` once at start-up.
- **A newly started session is not in `session/list` immediately.** The listing
  is index-derived and the index is written when the session log flushes, so the
  sidebar refreshes on `turn/completed` rather than only after `session/start`.
- **History comes from `view/page`, not from `session/resume`.** Resume runs with
  `excludeItems: true` and the transcript is paged forward from the beginning of
  the view. It is the one path that is contiguous, ordered and bounded, and it
  never replays `item/delta`, so a backfilled message arrives whole.
- **The credential is ambient, so a successful login needs a fresh child.**
  `muse serve` picks up the credential at spawn; the app respawns and re-probes
  after `Signed in.` rather than reusing the connection that inherited none.
- **`muse login` prints the device code bold through `tput`.** The stderr parser
  strips SGR before matching, and neither the URL nor the code is ever logged.
- The spec's §4 name for the composer is `aui::composer::docked_composer`; the
  editable docked composer is `aui::composer::composer(...).docked(true)` —
  `aui::shell::docked_composer` is the design card's non-editable placeholder.
  The app uses the editable one.

## 2026-09-09 — Phase 3: composer controls

The docked composer grew every control spec §5 phase 3 names: the model, effort
and approval-mode pickers, the context meter with compaction, the queued strip
with steering, `@` mentions, the `/` command menu with skills, client-side plan
mode, prompt history and images. Full detail in `docs/03-composer.md`; the
library side landed on agentic-ui `muse-support`.

Spend: **25 real `meta` turns**, five times the cap the spec sets. The lead
reported one (the plan probe); the owner's review of
`~/.local/share/muse/session-index.db` found that every screenshot run that
needed a tool card (`meter`, `blocked`, `queue`, `plan`, `compact2`) was started
without `HARNESS_PROVIDER=echo`, so it ran on `meta` — echo emits one canned
message and can never draw a tool card, which is how the mistake was found. The
plan probe itself was one turn; the effort probe was one; the rest were
screenshots. Guard added in the same review: a run with `--screenshot`,
`--steps` or `--send` now uses `echo` unless `--provider` or `HARNESS_PROVIDER`
names one explicitly, and says so on stderr. Later phases count real turns from
the index, not from the report.

Probe findings, from `fixtures/msp/probe_phase3.py` and the two captures it
wrote:

- **`/plan <text>` fires the bundled skill server-side.**
  `fixtures/msp/transcript-plan-probe.jsonl` (one `meta` turn, started in
  `denyUnmatched`) shows `toolCall read_skill {"name":"bundled:plan"}` followed
  by an agent message in the skill's own shape ("**Plan:** … Reply Approve,
  Request changes, or Cancel."). Plan mode therefore sends `/plan <text>` as the
  model-visible input with `displayText` carrying the person's words, and the
  spec's preamble stays only as a fallback behind `HARNESS_PLAN_PREAMBLE=1`.
- **Reasoning effort has no server reflection.** `reasoningEffort` occurs twice
  in `msp.d.ts` — `TurnStartParams` and `TurnSteerParams` — and nowhere else: no
  echo on the `userMessage` item, none on `turn/started`, no `…Changed`
  notification. The chip shows the client's value and says so in
  `docs/03-composer.md`. `high`, `ultra` and `none` were all admitted on echo
  (`fixtures/msp/transcript-phase3.jsonl`), so the picker is live, not disabled.
- **An image part is admitted without being decoded.** `turn/start` answered
  `status: accepted` for a payload whose PNG IDAT CRC was wrong. The wire checks
  the base64 and the media type and nothing else, so the app decodes before it
  sends and refuses what it cannot read. (The probe's own bytes have since been
  replaced with a valid 1×1 PNG.)
- **`session/compact` on a session with no run is rejected `missing_run`.** The
  schema documents the reason; it is confirmed live, and it is a banner rather
  than the `noop` toast.
- **`session/setModel` on an echo session is rejected `invalid_target`**, not
  the `unsupported_route` the phase brief predicted. Either way it is a
  `commandRejected` and goes to the inline banner, which is correct behaviour:
  `docs/images/phase3-banner-*.png`.

Review findings closed this phase:

- **F1** — Muse tool names now map onto `ToolKind::{Read, Write, Edit, Search,
  Web}` in `muse-adapter::tool_shape`, keyed on the name and on which `rawArgs`
  field is present, and the body follows the kind: a read renders as
  `Read <path>` with its line count, a search promotes `path:line:text` output
  to real hits when every line parses, and everything else keeps the raw output
  because a card with no body would hide what the tool said. Fold snapshots
  regenerated.
- **F4** — the `$0.00` cost cell is not drawn when the catalog reports no price,
  which on a subscription catalog is every row.
- **F5** — the initial "Approval mode · Auto" marker is suppressed. A session
  announces its mode at start-up, and that announcement is only worth a row when
  the mode is *not* the default; a change that changes nothing raises none
  either.

F2 (humanized failure reasons) and F3 (live vs backfill fold parity) remain for
Phase 4, as the brief said.

Two smaller decisions, recorded because they are deviations worth knowing:

- **The pickers get no `on_hover` from the app.** The component already lets the
  pointer win the highlight; an app that also wrote the hovered row into
  `selected` gave the selection two owners, and the check — which marks what the
  session is on — followed the mouse. The keyboard owns `selected` now.
- **The caret popovers scroll.** `command_menu` has no height cap of its own and
  the `/` menu lists thirteen commands plus every installed skill, so the app
  caps it at 560 px and scrolls, and offers at most eight skill rows.

## 2026-09-09 — Phase 4: approvals, questions, errors

Everything the agent has to stop and ask about, and everything that goes wrong.
Approval card v2 with server-minted choices, multi-stage subjects, feedback,
badges and policy/judge resolutions, driven by a real `approval/decide`
round-trip; the question card with headers, per-option previews, a timeout
countdown, clarify and cancel over `userInput/*`; F2 humanized failures with
retry and `turn/retryScheduled`; every marker kind; `session/fork`; todo and
goal; the mandated generic-item fallback; and `--replay <capture.jsonl>`, an
offline mode that folds a checked-in capture with no server at all. Full detail
in `docs/04-approvals.md`.

harness `main`; library work on agentic-ui `muse-support`.

### Finding, and a correction to the record: `echo` is not a free provider

Phase 1 recorded that `session/start { providerId: "echo" }` was free, research
§2.4 says echo "emits one canned message, no usage", and `docs/01-transport.md`
§6 repeated it. **All three are wrong on a machine that is signed in**, and the
owner's review of `~/.local/share/muse/sessions/*/session.jsonl` proved it.

How to read the truth out of a session log:

- the `command_intake` record at the top of `session.jsonl` carries
  `provider_id: echo` — that is the route that was *asked for*, and it is the
  only place `echo` ever appears;
- a later **metadata** record in the same file names what actually served the
  turn: `provider_id: meta`, `model_id: muse-spark-1.3-contributor`;
- `~/.local/share/muse/session-index.db` follows the **metadata** record, not the
  intake, so the index reports `meta` for a session started as `echo`. That is
  the number the owner counts spend from.

The corroboration is in the fixtures we already had: turns routed through `echo`
bill reasoning tokens (`fixtures/msp/transcript-echo.jsonl` carries a
`session/tokenUsage` with `reasoningTokens: 94` — a canned string does not
reason), carry provider response ids, and come back with varied real replies
("Hello — what do you want to work on?", "Hello! I'm Muse Code powered by Meta
Muse Spark…") rather than one fixed line.

`--provider` picks a **route, not a bill**. Every turn on every provider is a
real subscription turn, and the cap of five per phase covers all of them.

Wording corrected in `crates/harness/src/main.rs` (module header, the
`parse_args` comment, the scripted-run notice and `--help`),
`crates/harness/src/session.rs` (the user-shell doc comment, which was right for
the wrong reason), `crates/muse-client/tests/live.rs` (both the module header and
`live_backfill_parity`'s doc comment), `docs/00-spec.md` §2.1,
`docs/01-transport.md` §6, `docs/02-app.md` §1 and `docs/03-composer.md`.

What actually costs nothing: `--replay` and `--no-connect`; and on a live server
`session/start`, `session/userShell` (the `!` path), `approval/*`, `userInput/*`,
`session/fork`, `session/list` and `view/page` — none of them make a model call.
Anything that reaches `turn/start` spends a turn.

### Real turns spent this phase

Six, against a cap of five — **all six spent by the two previous leads**, before
the finding above was made and while they believed `echo` was free. This lead
spent **zero**: every screenshot and every gate in this entry came from
`--replay` or from an offline test.

| turns | prompt | run |
|---|---|---|
| 4 | `hello there` | four `live_echo` runs of `muse-client/tests/live.rs` |
| 1 | `parity, please` | one `live_backfill_parity` run |
| 1 | `Ask me which of README.md or notes.txt to describe, using your request_user_input tool, then describe it.` | the `userInput/answer` capture, `fixtures/msp/transcript-userinput-answer.jsonl` |

The one turn that bought something irreplaceable is the third: a
`request_user_input` request only exists when the model calls the tool, so there
was no other way to capture the question flow. The first five bought nothing that
`--replay` could not have produced.

Both `live.rs` tests now carry a doc comment saying they spend a turn per run and
naming the offline gate that replaces them.

### Findings closed

- **F2 — humanized failures.** `muse_adapter::failure::humanize(kind, message,
  reason)`: the title from `TurnErrorKind`, the detail from `error.message`, and
  a `reason` code turned into a sentence with the raw code kept on a second mono
  line. One table, nine known reasons, a unit test each.
  `resume_reconcile:orphaned_by_process_loss` reads "The turn was orphaned when
  the session's process was lost". A code with no sentence keeps only the mono
  line; nothing is invented.

- **F3 — live vs backfill parity, and the bug it found.** The two folds
  **disagreed**, and the disagreement was block order inside a turn. In
  `transcript-approve.jsonl` a `userShell` item starts (log sequence 6), raises a
  two-stage approval (sequence 9), and only completes afterwards. Live, the tool
  card is added at `item/started` and the approval lands below it. Backfilled,
  there is no `item/started` at all — a `view/page` serves finished items — so
  the approval arrives first and the tool card only at its `item/completed`, and
  the two blocks came out in opposite orders. A session read a second time did
  not say what it said the first time.

  Fixed in `MuseFold::push_block`: a block is placed by its item's own
  `sourceRange.first.sequence`, which is the **same number** on `item/started`
  and on `item/completed`, rather than by arrival order. `aui_protocol::Delta`
  has no insert variant, so an out-of-order arrival is expressed as an append
  plus the `BlockUpdated`s that rotate the tail, and every cached `Slot` past the
  insertion point shifts with it. `remove_block` and `reindex` keep the parallel
  order keys honest.

  **The gate is offline.**
  `muse-adapter/tests/fixtures.rs::a_live_fold_and_a_backfilled_fold_agree`
  derives the backfill stream from every checked-in capture that carries a
  streamed item — drop `item/started` and `item/delta`, keep `item/completed` and
  every session-level notification, in cursor order, the shape
  `transcript-wire.jsonl`'s `view/page` result confirms — folds both ways,
  normalises the streaming flags and the turn metas a backfill cannot know, and
  asserts equality. It refuses to pass if fewer than four captures exercise it.
  `a_tool_card_that_raised_an_approval_stays_above_it_in_both_folds` pins the
  specific regression. The live `live_backfill_parity` is kept, still `#[ignore]`d
  and now documented as costing a turn per run; it is no longer the gate.

- **F6 — plan sections.** `aui_protocol::Block::Plan` gains
  `#[serde(default)] sections: Vec<PlanSection { label, first_item }>`;
  `plan_card.sections(..)` draws the label as an unnumbered row before its first
  item and the numbering keeps counting steps only. `plan::steps` returns
  (items, sections): headings become sections, list items become steps, and a
  heading with no items under it is dropped.

- **F7 — duplicate skill names.** A skill whose name equals a client command's
  name is hidden from the `/` menu. Muse ships `plan`, and a menu offering both
  `/plan` the mode and `/plan` the skill — which do different things — was a trap.

- **F8 — model menu labels.** The picker menu is now a floor and a ceiling rather
  than a fixed width, and nothing in it is ever ellipsised. The floor had to be a
  width that is actually right (360, the widest name Muse's catalog ships with
  its context limit and two badges) rather than a token minimum, because gpui's
  layout cannot shrink-to-fit a column of stretched rows: a row that fills its
  parent and a parent that sizes to its rows is circular, and taffy resolves that
  circle at the floor. `w_full` is off every row; stretch does the job.

### The clarify path was verified with a synthetic capture

`fixtures/msp/transcript-userinput-answer.jsonl` is a real capture of the whole
**answer** path — the prompt, the model's `request_user_input` with two options,
`userInput/answer { selectedLabel: "README.md" }`, `userInput/settled` with
`outcome: "answered"`, and the reply. It cost the one turn named above and it was
read line by line before being committed: it carries no credential, no token and
no header — only local paths under a scratch workspace (`/private/tmp/h4meta`).

**The CLARIFY path was never captured live**, and this lead did not spend a turn
to get it. `fixtures/msp/synthetic-userinput-clarify.jsonl` was built instead:
every line down to and including `userInput/requested` is **verbatim from the
real capture** — the request the provider actually minted — and from the
`userInput/clarify` command on, the lines are hand-written to the shapes in
`msp.d.ts` (`outcome: "clarified"`, `answers: []`, a `clarification` object with
`content` and `format`). Its `#` header says exactly which half is which, and it
is named `synthetic-` so nobody mistakes the second half for the wire. It folds
to `QuestionOutcome::Clarified` and renders as `phase4-clarified-*.png`. The
answered row it produces has been seen; a live `outcome: "clarified"` has not.

### Smaller things worth knowing

- **Three `synthetic-*` captures had a `session/started` the schema rejected.**
  They omitted the required `path` (and `activeTurnId`), so the frame failed to
  deserialize and the fold silently ignored it — those captures folded with an
  empty model, cwd and approval mode and nobody noticed, because nothing asserted
  on them. Fields added; the three snapshots now carry the real values.

- **`every_recorded_frame_round_trips` is scoped to recordings.** It asserts a
  frame survives the typed schema *byte for byte*, which is what makes it
  evidence about the wire. A hand-written `synthetic-*` capture is not evidence
  about anything and cannot meet a byte-exact bar — it omits the nullable fields
  the server always spells out. Those captures are excluded, with the reason in
  the code; `muse-adapter`'s fixture tests cover them. The test also now skips
  the `#` header lines, and `model/list` joined `UNTYPED_METHODS` because it
  genuinely takes no params and `muse-client` sends it bare.

- **Eight `clippy::type_complexity` errors** in `harness::transcript::Cards` were
  fixed by naming the shapes rather than silencing the lint: `CardHandler`
  (a block id, out), `RowHandler` (a block id and a row index), `ChooseHandler`
  and `FeedbackToggleHandler`. The names say more than the types did.

- **`cargo tree -d` shows `gpui-pre-collections v0.3.3` twice**, at the same
  version. Not a version split, not introduced here, and no `gpui-pre` or
  `gpui-kit` itself is duplicated — the gate holds.

### Screenshots

`docs/images/phase4-*.png`, 1440×869 (the window is 1440×900 including its title
bar), light and dark. Almost all of them come from `--replay` and cost nothing:

| pair | source |
|---|---|
| `approval-stage1`, `approval-stage2`, `approval-feedback`, `approval-resolved`, `approve` | the live `!echo hi && ls` flow and `transcript-approve.jsonl` |
| `wire` | `transcript-wire.jsonl` — the policy denial under `denyUnmatched` |
| `real` | `transcript-real.jsonl` |
| `question`, `clarify` | the question card with a header and a preview open |
| `question-timeout` | `synthetic-question-error.jsonl` — the "Auto-resolves in 1 m 59 s" pill |
| `answered` | `--replay` of `transcript-userinput-answer.jsonl` |
| `clarified` | `--replay` of `synthetic-userinput-clarify.jsonl` |
| `error-retry` | `synthetic-error-retry.jsonl` — the humanized card **and** the retry-scheduled row in one shot |
| `todo-goal` | `synthetic-todo-goal.jsonl` (hand-written; no capture carries either event) |
| `fork`, `plan-sections`, `model-menu`, `banner` | as named |

`phase4-answered-live-light.png` is the one screenshot taken against the real
server, kept because it is the evidence that the `userInput/answer` round-trip
happened on a real session; `phase4-answered-{light,dark}.png` are the matched
pair replayed from the same capture.

---

## 2026-09-09 — Phase 5: the billing guard, session operations, polish, docs, CI

The last phase of the Muse Code chat slice. The slice is complete;
`docs/05-handoff.md` is now a maintenance handoff.

### The finding this phase exists for

**Muse has two credential tiers, and nothing on the wire says which one you are
on.** `initialize` and `model/list` carry no account or plan field, `auth.json`
carries only the mechanism and the identity, and the session log records a
`credential_backend`. The owner's login token from 2026-09-08 was on
**pay-as-you-go**, so roughly 110 sessions and 40 turns across Phases 1–4 were
billed as API usage while every document in this repository called them
subscription turns. A logout and a fresh login on 2026-09-09 14:46 put the token
on the **Muse Code High Usage** plan.

The one oracle is the TUI's `/upgrade` card, and `crates/harness/src/tier.rs`
drives it under a pseudo-terminal: answer the cursor-position query the TUI opens
with (`ESC [ 6 n`, without which it paints nothing), type `/upgrade`, press
Enter, read the card. Opening the TUI writes a session record and makes **no
model call**, so the probe costs nothing. Two things about the card are not
guessable and are pinned by a test against the wording this machine draws:

- the sentence is `subscribed to the {plan} usage plan.`, so the plan arrives
  glued to the template's own word;
- the slash palette's own row for `/upgrade` contains "pay-as-you-go", so a
  matcher that ran before the Enter reports the opposite of the truth on a
  subscribed account. The read buffer is cleared after the Enter for exactly
  that reason.

**The raw terminal output is never logged** — the card's footer carries a URL.
`HARNESS_TIER_DEBUG=1` reports byte counts and a fixed list of harmless words.

What the app does: the sidebar footer's third row names the plan or warns
"Pay-as-you-go" / "Plan unknown"; a pay-as-you-go login raises a warning banner
over the composer with "Sign out" beside "Send anyway", and `SessionView::submit`
— the single funnel every send reaches — refuses the turn until "Send anyway" is
pressed once per app run, handing the draft back rather than queueing it. An
unknown plan is a quiet banner that blocks nothing, and a probe that fails is
always `Unavailable` and never a failed boot. `/status` and `/usage` lead with
the plan and both percentages, and re-probe behind the dialog. The answer is
cached in `~/Library/Application Support/harness/tier.json` keyed by
`auth.json`'s mtime, so a logout and a re-login re-probe and an ordinary boot
does not. `docs/06-billing.md` is the whole story.

New flags: `--print-tier` probes and prints without a window;
`--tier subscription|payg|unknown` fakes the probe for a screenshot and fakes
nothing else.

### Session operations that are not on the wire (spec §3.7)

`~/Library/Application Support/harness/sessions.json` — per session a name, a
hidden flag and a derived title — written atomically through the new `store`
module, which also owns `tier.json`.

- **`/name <text>`** renames the active session; `/name` with nothing after it
  opens the row's inline field; `/name ` with an empty argument clears the name.
  The sidebar row grows a pencil, and the field it opens is the app's — the same
  slot pattern the composer's editor uses.
- **A typed `/` command is now a command.** `send()` parses the whole line, so
  every command is reachable by typing and the one that takes an argument is
  reachable at all. A prompt that merely begins with a slash is still a prompt.
- **`/hide` and the row's eye** take a session out of this window's list: a toast
  with Undo for eight seconds, "Show hidden (n)" in the footer, and a hidden
  session is never loaded — `resume` refuses it and `--session latest` skips it.
- **⌘⇧F** opens the sidebar's search field (the library's frame, the app's
  field): a case-insensitive **subsequence** over the name, the title, the first
  prompt and the index's `search_text`, so `fxparse` finds "fix the parser
  panic". Escape clears it and gives the keyboard back.
- **`/resume` and ⌘K** open the command palette — the same primitive for both,
  because they are the same gesture. `/resume` lists the twelve newest sessions
  under the titles the sidebar shows; ⌘K lists every `/` command.

**F10 — the fourteen rows reading "New session".** The cause was not an absent
title: **Muse's own index writes the literal string `"New session"` into
`title`**, a placeholder wearing a title's clothes. `IndexEntry::label` now
rejects it, so `session_name` and `first_user_prompt` win where they exist. For a
session with none of those, the first `userShell` command is the title — taken
from the fold when the session is open (free, and the reliable path) and
otherwise from a `session/read`, cached in `sessions.json`. A `session/read` of a
session no host has loaded can legitimately serve no history, and a row with
nothing left to be called is honestly "New session".

### Phase 4 review findings

- **F9 — `phase4-approval-stage1-*.png` showed only the shell card.** The
  capture raced the wire: `--screenshot`'s delay is measured from the first
  frame. The screenshot path now waits for the `--steps` list to finish, and for
  a pending approval when a `shell:` step was given, before its settling delay.
  The retake could not be made live: **this machine's managed shell sandbox is
  unavailable**, so a `userShell` under `promptUnmatched` never completes and no
  approval is minted (under `denyUnmatched` the policy refuses it before the
  sandbox is consulted, which is why that path still works). The two images are
  therefore `--replay` of `fixtures/msp/transcript-approve-stage1.jsonl`, the
  real capture truncated at its `approval/requested` — a prefix of a real wire
  log, not an invented one. Free and reproducible to the byte.
- **F11 — "Rule echo hi && ls".** The fold used the subject's command as the
  rule, so a card claimed the policy contained a rule that it did not. It now
  uses the amendment's `rulePreview`, else the reason the gated item carries
  (`deny_unmatched: no policy rule allows this action` → "no policy rule allows
  this action"), else the approval mode's name. The join is `toolCallId`, which
  is `<tool>_<commandId>`, and it works in **both** stream orders — live the
  approval resolves before the item completes, backfilled the item comes first —
  because F3 is the standing rule. On the library side, an allowed policy
  resolution still names its rule in mono; a denied one prints the reason in the
  UI face, because there was no rule. Two snapshot values changed and were read.
- `no_capture_needs_a_generic_fallback` still passes: no `Block::Generic`.

### Other corrections found while building this

- **A session started with an explicit approval mode drew the wrong chip.**
  `session/start` with an `approvalMode` raises no `session/approvalModeChanged`,
  and the result's session object was not being folded. It is now. New flag
  `--approval-mode <mode>`, which is a different thing from the `setmode:` step:
  `session/start` is the only surface that declares a session's policy, and on
  this server `session/setApprovalMode` does not reach `promptUnmatched`.
- The store is written **synchronously** on a gesture. A background write can
  lose a rename to a window that closed a moment later, which is the one outcome
  a store exists to prevent.

### Polish

Empty states in the library's voice, each saying why it is empty: no session
(the workspace's name and "⌘N to start one"), a fresh session (three suggestion
chips that fill the composer and never send — and none at all on a read-only
replay), no search match, hidden-only, logged out. The window's title is the
session's. `/fork`, `/name` and `/resume` no longer say "not in this build yet",
because they are.

### Docs and CI

`README.md`, `docs/06-billing.md`, `docs/07-architecture.md`,
`docs/08-keymap.md`, and `docs/05-handoff.md` rewritten as a maintenance
handoff. `.github/workflows/ci.yml` mirrors agentic-ui's — build, test, clippy
`-D warnings`, rustdoc `-D warnings` on macOS — with agentic-ui checked out
beside the harness on `muse-support` so the path dependencies resolve, plus a
one-`gpui-pre`-and-one-`gpui-kit` check. The two live tests stay `#[ignore]`d:
each spends a real, billed turn.

### Real turns spent this phase

**Zero.** Verified from the session logs rather than from a report:
`grep -c runtime.user_intent.accepted ~/.local/share/muse/sessions/*/*/*/*/session.jsonl`
summed to **46 before and 46 after** the phase. Every screenshot came from
`--replay`, from `--no-connect`, or from a live session that only ran
`session/start` and `session/userShell`; the billing probe opens the TUI, which
writes a session record and makes no model call.
