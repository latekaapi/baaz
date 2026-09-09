# The composer — controls, menus and plan mode

Phase 3 of the spec (`docs/00-spec.md` §5). Everything that hangs off the docked
composer: the model, effort and approval-mode pickers, the context meter and
compaction, the queued strip with steering, `@` mentions, the `/` command menu
with skills, client-side plan mode, prompt history and images.

The gate for this phase is one sentence: **every control changes real session
state, and the chip is drawn from the server's notification, not from an
optimistic local write.** The table below is that gate, control by control.

| control | what the app sends | what moves the chip |
|---|---|---|
| model | `session/setModel { model: {modelId, providerId, profileId} }` | `session/modelChanged` → `SideState::model` |
| approval mode | `session/setApprovalMode { mode }` | `session/approvalModeChanged` → `Session::mode`, and the marker row |
| reasoning effort | `reasoningEffort` on `turn/start` and `turn/steer` | **nothing** — see below |
| context meter | — | `session/contextUsage` + `session/tokenUsage.cumulative` |
| compaction | `session/compact {}` | the `compaction` item's marker; a `noop` ack is a toast |
| queue | `turn/start` (wire default `ifBusy: queue`) | the ack's `disposition: queued` → `SideState::queued` |
| edit / remove / steer a queued row | `turn/unqueue { turnId }` | `turn/unqueued` — the row leaves then, never before |
| steer | `turn/steer { expectedTurnId }` | the running turn's own items |
| plan mode | `session/setApprovalMode { denyUnmatched }` + `/plan ` on the text | `session/approvalModeChanged`; the plan card is client-authored |

**Reasoning effort is the one control with no server reflection.**
`reasoningEffort` appears in `msp.d.ts` exactly twice — on `TurnStartParams` and
on `TurnSteerParams` — and nowhere else. There is no echo on the `userMessage`
item, none on `turn/started`, and no `session/…Changed` for it. The chip
therefore shows the client's own value, and says "Default" when the field is
omitted. The probe (`fixtures/msp/probe_phase3.py`, capture
`fixtures/msp/transcript-phase3.jsonl`) confirmed `high`, `ultra` and `none` are
all admitted on the echo provider, so the picker is live rather than disabled.

---

## 1. How to drive it from a command line

Every screenshot in `docs/images/phase3-*.png` is one invocation. `--steps`
takes `;`-separated steps (a `;` rather than a comma, because a step's payload
carries paths and prose), applied to the session the moment it opens; naming any
step is enough to start a session, so `--send` is no longer the only way in.

```sh
export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"

HARNESS_PROVIDER=echo cargo run -p harness -- --workspace /tmp/ws --theme dark \
  --screenshot /tmp/model.png --screenshot-delay 6000 --steps 'model'
```

| step | what it does |
|---|---|
| `draft:<text>` | put text in the composer, caret at the end |
| `send:<text>` | send a turn |
| `steer:<text>` | `turn/steer` into the running turn |
| `model` / `effort` / `mode` | open that chip picker |
| `confirm` | activate the open menu's selected row |
| `setmodel:<id>` | `session/setModel` without waiting for the catalog |
| `compact` | `session/compact` |
| `command:<filter>` / `mention:<filter>` | open the caret popover with that filter typed |
| `meter` | pin the context meter's breakdown open |
| `context:<used>/<window>/<level>` | a **synthetic** `session/contextUsage` |
| `plan` | turn plan mode on |
| `image:<path>` | attach an image |
| `plus` / `drop` | open the `+` menu; raise the drop overlay |

`context:` is the one step that invents a fact, and it exists for one reason:
the meter's `warning` and `blocked` states are the server's to declare, the
thresholds are the server's, and no prompt anyone would want to send fills a
one-million-token window in a turn anyone would want to spend. `docs/images/phase3-blocked-*.png`
is therefore a synthetic pressure level over a real session's real cumulative
counters. Everything else in every capture came off the wire.

---

## 2. The pickers

`aui::composer::{model_menu, effort_menu, mode_menu}` are one component with
three data sets, anchored to the chip that opened them through
`Composer::chip_menu(ComposerChipAnchor, …)` and painted on
`aui::overlay::popover_layer`. ⌘⇧M / ⌘⇧E / ⌘⇧P open them; ↑/↓ move, Enter picks,
Escape closes; `/model`, `/effort` and `/mode` open the same three.

- **Model** rows come from `model/list { sessionId }`, fetched on every open —
  MSP has no catalog subscription, so a cached list would be a stale one. Each
  row carries its context limit and any of `default` / `active`; MSP says a
  client must not assume exactly one row wears either, so both are badges rather
  than states.
- **Effort** rows are the MSP enum plus a leading "Default" that omits the
  field. The catalog's `max` tier is never offered: MSP rejects it.
- **Mode** rows are the four MSP modes with the labels from spec §3.6.

None of the three is given an `on_hover` handler. The component already lets the
pointer win the highlight while it is over a row; an app that also wrote the
pointer's row into `selected` would give the selection two owners, and the check
— which marks what the session is actually on — would wander with the mouse.
The keyboard owns `selected`; the pointer owns its highlight and reports a click.

On the **echo** provider `session/setModel` is refused, and that refusal is
correct behaviour rather than a bug: `docs/images/phase3-banner-*.png` is the
inline banner showing it. The rejection reason on this build is
`invalid_target`, not the `unsupported_route` the phase brief predicted.

## 3. The context meter and compaction

`aui::data::context_meter`: a stroked ring, the percentage, and a hover
breakdown carrying used / window / session prompt / session output / session
total and a **Compact** button. Pressure — not the fraction — picks the colour,
because the thresholds belong to the server. A basis with no limit renders
`N tokens` against an empty ring, which is what a fresh session shows.

`blocked` does two things: the ring and number go danger-coloured, the meter
offers **Compact** inline rather than only on hover, and the composer refuses to
send (`can_send(false)`). Compaction is `session/compact {}`; an ack of `noop`
is a success and becomes the toast "Nothing to compact · <reason>", while a
session with no resolvable run is a real wire error (`commandRejected`, reason
`missing_run`) and goes to the banner — which is what `--steps compact` on a
fresh session shows, because steps run the moment the session opens.

Toasts hang from the top right, under the window header: the library's stack
lays its cards out **downward** from its own box, so anchoring it to the bottom
of the window would draw the newest one off the end.
`docs/images/phase3-toast-*.png` is the "not in this build yet" toast, which is
also the proof that the `/` menu's Enter path reaches a command.

## 4. The queue and steering

Enter while a turn runs queues, because `ifBusy` defaults to `queue` on the
wire; the ack's `disposition` says which happened and the ack's `turnId` is the
row's identity. ⌘↩ steers instead, with `expectedTurnId` set to the running
turn so input meant for turn A can never land in turn B.

The strip above the composer renders `SideState::queued` **exactly as given, in
server order**. Each row offers Steer now, Edit and Remove, and all three send
the same `turn/unqueue`; what differs is what happens when `turn/unqueued`
arrives:

- **Edit** — the fold hands the text back and it goes into the composer. Until
  the wire confirms, the row stays, wearing an `editing` badge.
- **Remove** — the text is dropped.
- **Steer now** — the text goes straight back out as `turn/steer`.

Nothing is reordered or removed optimistically. A reclaim that loses the race is
a wire error, the row stays, and the banner says so.

## 5. `/` commands and `@` mentions

The `/` menu opens on a slash at the start of a line and filters as you type;
the `@` picker opens on an at-sign anywhere a word starts. ↑/↓/Enter/Escape
drive both. They are the same two library components the design cards use
(`command_menu`, `mention_picker`), floated above the composer on
`popover_layer` inside a max-height scroller — a workspace with twenty skills
would otherwise push the list past the top of the window.

**Commands** are the spec §3.10 list, all client-side. `/model` `/effort`
`/mode` open pickers; `/plan` toggles plan mode; `/compact` compacts; `/clear`
starts a new session; `/logout` runs `muse logout`; `/status` and `/usage` open
one dialog built out of `SideState` (model, mode, effort, plan, context,
cumulative tokens, queue depth, session id, workspace, branch) under the
billing tier and its two percentages; `/help` reopens the menu unfiltered.
`/fork` landed in Phase 4; `/name`, `/hide` and `/resume` landed in Phase 5 —
nothing in the list says "not in this build yet" any more.

A command can also be **typed in full and sent**. `send()` parses the whole
line, so `/name Fix the parser` renames the session rather than asking Muse
about it — which is the only way to reach the one command that takes an
argument. A prompt that merely begins with a slash is still a prompt: the head
has to be a command this build knows.

**Skills** come from `muse skills list --json`, run once at boot on a background
thread, tagged with the scope prefix of the skill's `id`
(`bundled` / `user` / `project` / `plugin`). Picking one inserts `/name ` as
text and the server resolves it — which is exactly what the plan probe proved.

**Mentions** are workspace files, walked once in the background with the
`ignore` crate (respects `.gitignore`, skips `.git`, capped at 5 000 entries)
and matched as a subsequence, so `mainrs` finds `src/main.rs`. Picking one
inserts `@relative/path ` as **plain text inside the text part**: MSP has no
structured mention part, and research §1.5 says the text is where a mention
lives.

## 6. Plan mode — and the probe that decided it

Spec §3.1 required one real turn to settle whether sending the literal text
`/plan <prompt>` fires the bundled skill server-side or is just text. It fires.
From `fixtures/msp/transcript-plan-probe.jsonl` (one `meta` turn,
`approvalMode: denyUnmatched`):

```text
item/started    toolCall  read_skill  {"name":"bundled:plan"}
item/completed  toolCall  read_skill  {"name":"bundled:plan"}
item/completed  agentMessage  "**Plan:** Print `hello` to stdout … Reply
                               Approve, Request changes, or Cancel."
```

So plan mode sends `/plan <text>` as the model-visible input with `displayText`
set to what the person typed, and asks for `denyUnmatched` for the duration.
`crates/harness/src/plan.rs` still carries the spec's preamble as the fallback
for the day a Muse build stops shipping the skill; `HARNESS_PLAN_PREAMBLE=1`
reaches it without a rebuild, because a fallback nobody can reach is not one.

The rest is as §3.1 says. Shift+Tab or `/plan` toggles; a **Plan** pill sits in
the composer footer and its `×` leaves; the previous approval mode is remembered
and restored, and the chip only moves when `approvalModeChanged` says it did.
When the reply completes, its headings and list items become a `Block::Plan`
appended through `MuseFold::append_client_block` — the one block the client
authors — with Accept, Edit and Reject. Accept restores the mode and sends
"Implement the plan above."; Edit keeps plan mode and focuses the composer;
Reject restores the mode and stops there.

## 7. Prompt history

Every sent prompt is appended to
`~/Library/Application Support/harness/history.json`, keyed by the canonical
workspace path — the same canonicalization `session/list` needs, so `/tmp/x` and
`/private/tmp/x` are one history. The newest 200 are kept and a prompt identical
to the one before it is not stored twice.

↑ on the draft's first line and ↓ on its last line walk it, with the draft kept
as the newest slot so walking up and back down returns exactly what was there.
gpui-kit's `TextareaState` does expose the caret's line (`cursor_position()`),
so the fallback the brief allowed — "↑ at offset 0" — was not needed. The keymap
does this with a key context rather than a guess: the composer's holder wears
`histup` / `histdown` when the caret is on the first / last line and `menu` when
a popover is open, and the bindings are predicated on them, so ↑, ↓ and ↩ mean
the menu, the history or the editor without any of the three being taken away.

## 8. Images

Paste (⌘V with an image on the clipboard), drop (`ExternalPaths` onto the centre
pane, with the library's drop overlay) and attach (the `+` menu). Each becomes a
removable chip and a `TurnInputPart::Image { base64Data, mediaType, width,
height }`; width and height always travel together because both come from the
decoder. The format is read from the bytes, not the file name, so a `.png` that
is really a JPEG is not announced wrongly. PNG, JPEG, GIF and WebP, 10 MB each;
anything else is refused with a reason in the banner.

⌘V is bound in the composer's context and re-dispatches gpui-kit's own `Paste`
when the clipboard holds no image, so an ordinary text paste is the editor's,
untouched.

The probe found that MSP admits an image part it cannot decode — the first
capture's "PNG" had a broken IDAT CRC and `turn/start` still answered
`status: accepted`. The wire checks the base64 and the media type and nothing
else, so the app decodes before it sends.

## 9. What is deliberately not here

- **`/fork`, `/name`, `/resume`** are listed and say which phase brings them.
- **Approvals and questions are still read-only.** Phase 4.
- **No structured mention part.** There is none on the wire.
- **No effort chip disabled state.** The provider accepts every tier the picker
  offers, so there is nothing to grey out.
- **The `@` index is not watched.** It is walked at boot and again on ⌘N, and
  never in between: a file created mid-session is not offered until the next
  new session.
