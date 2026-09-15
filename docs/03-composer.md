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
omitted. A probe script (`fixtures/msp/probe_phase3.py`, removed 2026-09-12;
git history has it), against the capture `fixtures/msp/transcript-phase3.jsonl`,
confirmed `high`, `ultra` and `none` are all admitted on the echo provider, so
the picker is live rather than disabled.

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
| `file:<path>` | attach a file (extracted to text, unless it is an image) |
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
  field, including the `max` tier muse 1.1.1 added between `xhigh` and
  `ultra`.
- **Mode** rows are the four MSP modes with the labels from spec §3.6.

None of the three is given an `on_hover` handler. The component already lets the
pointer win the highlight while it is over a row; an app that also wrote the
pointer's row into `selected` would give the selection two owners, and the check
— which marks what the session is actually on — would wander with the mouse.
The keyboard owns `selected`; the pointer owns its highlight and reports a click.

On the **echo** provider `session/setModel` is refused, and that refusal is
correct behaviour rather than a bug: `docs/images/phase3-banner-*.png` is the
inline banner showing it. The rejection reason on this build is
`invalid_target`, not `unsupported_route`.

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
`/fork` landed in Phase 4 and grew a picker afterwards; `/name`, `/hide`
and `/resume` landed in Phase 5 — nothing in the list says "not in this build
yet" any more. `/empty` toggles the sidebar's empty-session filter: sessions
with no turns are hidden by default, and the command shows them again (or
hides them once shown).

**`/fork` names a turn two ways.** Typed bare, or picked from the `/` menu, it
opens the turn picker: the session's completed assistant turns, newest first,
each row the first line of the user prompt that started the turn plus the
turn's time. Picking a row forks that turn. Typed with a number (`/fork 2`) it
skips the picker and forks the nth newest completed turn directly — `1` is the
newest. A number with no turn behind it is a banner, never a fork of whatever
the server thinks is newest. Choosing a row, like every fork, only sends
`session/fork`; the new session opens when the server's resume envelope
arrives, never before. The picker is scripted as `--steps fork-picker` (see
`docs/images/improve-fork-picker-dark.png`).

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
so the naive fallback — "↑ at offset 0" — was not needed. The keymap
does this with a key context rather than a guess: the composer's holder wears
`histup` / `histdown` when the caret is on the first / last line and `menu` when
a popover is open, and the bindings are predicated on them, so ↑, ↓ and ↩ mean
the menu, the history or the editor without any of the three being taken away.

## 8. Images and files

Paste (⌘V with an image on the clipboard), drop (`ExternalPaths` onto the centre
pane, with the library's drop overlay) and attach (the `+` menu, or ⌘U). The
`+` menu holds three rows: **Attach file or photo** (⌘U), **@ Mention file**
(types `@` into the draft so the mention picker opens) and **/ Slash
commands** (types `/` so the command menu opens).

Which chip a dropped or picked path becomes is decided by its extension. Image
extensions (PNG, JPEG, GIF, WebP) become a removable image chip and a
`TurnInputPart::Image { base64Data, mediaType, width, height }`; width and
height always travel together because both come from the decoder. The format is
read from the bytes, not the file name, so a `.png` that is really a JPEG is
not announced wrongly. Ten megabytes each; anything else is refused with a
reason in the banner. Each image chip draws a 64 px thumbnail, decoded and
downscaled once at attach time (`images::THUMB_LONG_EDGE`); the full-resolution
bytes still go on the wire.

Everything else goes through `attachments::from_path` and becomes a removable
`ComposerChipKind::File` chip (name plus a muted `KIND · size` detail), because
MSP's `TurnInputPart` is a closed enum — `text` or `image`, anything else is
`invalidParams` — and a file's bytes have nowhere else to go. Text-like types
(md, txt, csv, source code and friends, extensionless files sniffed as UTF-8)
are read as text; PDF arrives through its text layer (`pdf-extract`), xlsx/xls
sheet by sheet as CSV-ish text under `## <sheet>` headers (`calamine`), docx
from `word/document.xml` with its tags stripped (`zip`). Anything else is
refused with the banner reason. Each file is capped at 64 KB of text
(`attachments::MAX_FILE_BYTES`, truncated with a `[file truncated to 64 KB]`
note) and a turn carries at most 8 files (`attachments::MAX_FILES`); `parts()`
emits one text part per file — `--- file: <name> ---\n<content>` — ahead of
the prompt text. All three crates are pure Rust, so `cargo tree -d` still shows
one `gpui-pre` and one `gpui-kit`.

Enter sends because the composer's editor runs with `submit_on_enter`: plain
Enter emits the submit without a newline and the harness binding sends the
turn, while Shift+Enter still inserts a newline. The flag lives in the library
(`composer_state_rows`), so the harness binds nothing of its own for it.

⌘V is bound in the composer's context and re-dispatches gpui-kit's own `Paste`
when the clipboard holds no image, so an ordinary text paste is the editor's,
untouched.

## 9. New-session drafts

An unsent session has **no sidebar row** and costs at most one server session
per project. `Harness.drafts` names one draft session per project id; ⌘N
(`new_session_in`) reopens that view while it is still unsent (zero turns,
no name) instead of calling `session/start`, so repeated ⌘N never accumulates
"New session" rows. Views named in `drafts` are never evicted from the
parked-view MRU.

The row appears when the first message is accepted: `turn/started` inserts a
`local` row built from the seeded session envelope, titled from the prompt
and dated now (newest-first puts it at the top of its project), and drops
the id from `drafts`; `turn/completed` reloads and the wire row replaces it
as before. Switching away parks the unsent view silently — there is no row
to filter.

The draft is per project: text, images and files. Picking another project
from the header crumb's menu moves the active draft's content into the picked
project's draft session (`take_draft` / `put_draft`; created there if needed,
inheriting that project's effort default), and the old id leaves `drafts`
(the server prunes zero-turn sessions on relaunch). A composer that already
holds something keeps it — moved content never clobbers. Drafts live in
memory for the app's lifetime; nothing is persisted to disk.

Scripting: `--steps new` is ⌘N, `new:<project name>` the group row's `+`.
Every `session/start` the app sends logs one `harness: session/start
project=<id> reason=<no-draft|retarget>` line, so a scripted run can count
them. Under `--no-connect` / `--replay` there is no child to start on, so
`new` opens the draft as a local view (`local-draft-<project>`) and `open:`
opens the row the same way — captures drive the same lifecycle for free.

The probe found that MSP admits an image part it cannot decode — the first
capture's "PNG" had a broken IDAT CRC and `turn/start` still answered
`status: accepted`. The wire checks the base64 and the media type and nothing
else, so the app decodes before it sends.

## 10. What is deliberately not here

- **`/fork`, `/name`, `/resume`** are listed and say which phase brings them.
- **Approvals and questions are still read-only.** Phase 4.
- **No structured mention part.** There is none on the wire.
- **No effort chip disabled state.** The provider accepts every tier the picker
  offers, so there is nothing to grey out.
- **The `@` index is not watched.** It is walked at boot and again on ⌘N, and
  never in between: a file created mid-session is not offered until the next
  new session.
