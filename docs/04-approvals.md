# Approvals, questions and errors

Phase 4 of the spec (`docs/00-spec.md` §5). Everything the agent has to stop and
ask about, and everything that goes wrong: the approval card and its
`approval/decide` round-trip, the question card and its `userInput/*` round-trip,
the error classification, retry and retry-scheduled, `session/fork`, and the
`--replay` mode that makes almost all of it reproducible for nothing.

The gate for this phase is one sentence: **the card is never ahead of the
server.** A press sends a command and then waits; the card changes when — and
only when — a notification says it changed. An acknowledgement is admission, not
an outcome.

---

## 0. What costs a turn, and what does not

Read this before running anything.

**`echo` is not a free provider.** Phase 1 recorded that it was, and research
§2.4 says so too; both are wrong on a machine that is signed in. The session log
is the evidence: `~/.local/share/muse/sessions/<y>/<m>/<d>/<id>/session.jsonl`
opens with a `command_intake` record carrying `provider_id: echo` — the route
asked for — and then carries a **metadata** record naming
`provider_id: meta, model_id: muse-spark-1.3-contributor`. `session-index.db`
follows the metadata record, not the intake, so the index reports `meta` for a
session started as `echo`. Turns routed through `echo` bill reasoning tokens
(`fixtures/msp/transcript-echo.jsonl` carries a `session/tokenUsage` with
`reasoningTokens: 94`), carry provider response ids, and answer with varied real
text rather than one canned line.

`--provider` picks a **route**, not a bill. The cap of five real turns per phase
covers every provider.

| costs nothing | why |
|---|---|
| `--replay <capture.jsonl>` | folds a checked-in capture; no server, no session, no turn |
| `--no-connect` | draws the chrome without spawning `muse serve` |
| `session/start` | opens a session; no model call |
| `session/userShell` (the `!` path) | the **server** runs the command, not the model |
| `approval/*`, `userInput/*`, `session/fork`, `session/list`, `view/page` | no model call |

Anything that reaches `turn/start` spends a turn: `--send`, and a `--steps` list
containing `send:` or `steer:`.

This is why the whole approval flow is exercisable for free — a user shell
command raises a real, multi-stage, server-minted approval and never touches a
provider — while the question flow is not, because `userInput/request` only
happens when the model calls its `request_user_input` tool.

---

## 1. The approval state machine

### 1.1 The shape on the wire

An approval is a **server request**: `approval/request`, a real JSON-RPC request
with an id, which is answered only by sending `approval/decide` — never by
returning a JSON-RPC result. It is twinned with an `approval/requested`
notification carrying identical params; the fold dedupes them on the id so one
request draws one card.

A subject may have **stages**. A shell pipeline is decided one stage at a time:
`echo hi && ls` under `promptUnmatched` is two stages, and the server asks about
`echo hi` first. Each stage has its own `requirementId`, and the choices change
between stages — `allow_local_prefix` is "Always allow in this workspace:
`echo ...`" at stage 1 and "`ls ...`" at stage 2.

```
approval/request ─┬─→ card drawn (stage 1/2, server's choices)
approval/requested┘
        │
        │  a press → approval/decide { approvalId, choiceId, requirementId, feedback? }
        │            (background task; the ack is admission only)
        ↓
approval/updated ──→ re-render: new stage, new currentRequirementId, NEW CHOICES
        │
        │  a press → approval/decide with the *new* requirementId
        ↓
approval/resolved ─→ the card goes quiet and single-line
```

### 1.2 What the app keeps, and what it must not

The MSP `requirementId` is kept per stage in `SideState`, on the fold's request
map — `aui_protocol::ApprovalStage` has no room for it and does not need one.

**Choices are never cached.** They belong to the stage, and the stage moves. The
card re-renders from `approval/updated` and `approval/resolved` and from nothing
else; the acknowledgement's `terminal` flag is admission, not truth.

`approval/decide.requirementId` must equal the server's `currentRequirementId`
or the wire answers `approvalRequirementStale`.

### 1.3 The card

`aui::transcript::approval_card`, stateless as always: data in, intents out.

| data in | what it draws |
|---|---|
| `.choices(Vec<ApprovalChoice>)` | the server's buttons in the server's order, action-row pattern (key hints left, spacer, buttons right). Primary styling for `Once` / `ApprovedForSession`, deny styling for `Deny` / `Abort`. A `PolicyAmendment` shows its `rule_preview` in the mono face. The row **wraps** — a half-shown rule is worse than a taller row. |
| `.stages(Vec<ApprovalStage>, current)` | a `Stage n/N` pill and each stage's `argv` in mono, resolved stages checked, the current one at full ink, `argv_complete == false` marked "(partial parse)". A single-stage subject draws no strip. |
| `.badges(ApprovalBadges)` | "Protected write" and "Judge escalated" pills in the header, warning-tinted, only when set. |
| `.resolved_by(Option<ResolvedBy>)` | see §1.5 |
| `.feedback_open(..)` + `.feedback_slot(..)` | see §1.4 |
| `.title(..)` | the pending question. The library's default is "Allow this command?"; the app passes "Allow Muse to run this command?" — the card never names a provider it does not know. |

An empty `choices` list falls back to the built-in allow / always / deny triad
and `on_decide`, which is what the pre-phase-4 path still uses.

**Keyboard.** Digits `1`–`9` pick the n-th choice in server order
(`aui::keys::ChooseNth`, bound in `APPROVAL_CONTEXT`); Enter confirms an open
feedback field; Esc closes the field, then collapses the card. A card with fewer
choices than the digit pressed does nothing —
`aui/tests/keyboard.rs::a_digit_past_the_last_choice_chooses_nothing`.

### 1.4 Feedback

A choice whose `accepts_feedback` is set does **not** fire on its first press.
The app opens a field instead; the confirming press carries the text.

The card never owns text. `.feedback_slot(AnyElement)` takes a gpui-kit input the
**app** owns — exactly the division the composer's editor already uses — and
`.feedback_text(..)` is data in so that "Send" can hand it straight back through
`on_choose(choice_id, Some(text))`. "Cancel" closes the field via
`on_feedback_toggle(None)`.

### 1.5 Resolutions nobody was asked for

A policy rule or the approval judge can settle a request before a person sees it.
Those never render as an actionable card: `resolved_by` collapses them to one
quiet line and no buttons enter.

| `resolved_by` | state | reads |
|---|---|---|
| `Policy` | allowed | "Allowed by policy · Rule `<rule>`" |
| `Policy` | denied | "Denied by policy · Rule `<rule>`" |
| `LlmJudge` | allowed | "Allowed by the approval judge" |
| `LlmJudge` | denied | "Denied by the approval judge" |
| `User` | denied | "Denied", plus the feedback quoted if any went out |

`transcript-wire.jsonl` is the captured policy denial under `denyUnmatched`.

### 1.6 When the decide loses

| error | what the app does |
|---|---|
| `approvalAlreadyResolved` | fold `data.resolution` into the block as resolved. Not an error the person needs to see. |
| `approvalChoiceInvalid` | banner, and re-render from the server |
| `approvalRequirementStale` | **silent** — the update that moved the stage is already on its way |
| `approvalNotFound` | mark the block resolved-unknown, and banner |

### 1.7 Finding it again

`needs_you_banner` sits above the composer while any pending approval or question
is scrolled out of view; its action scrolls to the card. On `session/resume` and
on every reconnect the app calls `approval/listPending` and folds the result,
deduped on ids, so a card that arrived while the socket was down is still there.

---

## 2. Questions (`userInput/*`)

### 2.1 The round-trip

`userInput/request` is the model calling its `request_user_input` tool. Like an
approval it is a server request twinned with a `userInput/requested`
notification, and it is answered only through the `userInput/*` commands.

| the person does | the app sends |
|---|---|
| picks and confirms | `userInput/answer { answers: [{ questionId, selectedLabel \| selectedLabels \| freeText, note? }] }` |
| "Skip" | `userInput/cancel` |
| "Explain instead" | `userInput/clarify { clarification: { content, format: "text" } }` |

**Answers key on option labels, not indices.** Free text is capped at 500
characters and `attachments` is reserved and rejected.

One request may carry several questions. The fold emits one `Block::Question`
each; the app holds the answers locally and sends a single `userInput/answer`
when the last one is answered, and the cards read "n of N" meanwhile.

`autoResolutionMs` drives a local 1 s foreground countdown while a timed question
is pending, stopped on settle. The card is pure — `.timeout(remaining, total)`
draws the pill, the app owns the clock.

### 2.2 Settlement

`userInput/settled` carries an `outcome`, and only one of its values is an
answer. `answered_row` says which, so a cancelled prompt does not read as
"Answered:" with nothing after it:

| outcome | the row |
|---|---|
| `answered` | "Answered: `<chips>`" |
| `cancelled` | "Skipped" |
| `clarified` | "Clarified: `<the text>`" |
| timed out | "Timed out" |

| error | what the app does |
|---|---|
| `userInputAlreadySettled` | nothing — `userInput/settled` is on its way with the real outcome |
| `userInputAnswerInvalid` | banner on the card; the local selection is dropped |

### 2.3 What was captured, and what was not

`fixtures/msp/transcript-userinput-answer.jsonl` is a **real** capture of the
whole answer path on the real provider: the prompt, the model's
`request_user_input` with two options, `userInput/answer` with
`selectedLabel: "README.md"`, the `userInput/settled` with
`outcome: "answered"`, and the reply that followed. It cost one real turn.

**The clarify path was never captured live.** A second live turn would have
bought a settlement whose only difference from the answered one is three fields,
so `fixtures/msp/synthetic-userinput-clarify.jsonl` was built instead: every line
down to and including `userInput/requested` is verbatim from the real capture —
the request the provider actually minted — and from the `userInput/clarify`
command on, the lines are hand-written to the shapes in `msp.d.ts`
(`outcome: "clarified"`, `answers: []`, a `clarification` object). It is named
`synthetic-` so nobody mistakes the second half for the wire. It folds to
`QuestionOutcome::Clarified`.

---

## 3. Errors

### 3.1 F2 — humanized failures

`muse_adapter::failure::humanize(kind, message, reason) -> (title, detail)`, one
table in one place with a unit test per known reason.

The **title** comes from `TurnErrorKind`:

| kind | title |
|---|---|
| `ModelError` | Model error |
| `ConfigError` | Configuration error |
| `StepLimit` | Step limit reached |
| `EnvironmentError` | Environment error |
| `LaunchError` | Launch error |
| `ProjectionError` | Projection error |
| `LogError` | Log error |
| `WorkflowLaunchError` | Workflow launch error |
| anything else | the kind's own wire string, verbatim |

The **detail** is `error.message`, and a `reason` code becomes a sentence with
the raw code kept below it in mono — so
`resume_reconcile:orphaned_by_process_loss` reads "The turn was orphaned when the
session's process was lost" over `resume_reconcile:orphaned_by_process_loss`. A
code with no sentence keeps only the mono line; nothing is ever invented. The
table covers `interrupt:user`, `interrupt:shutdown`, `cancel:user`,
`cancel:superseded`, `step_limit:exceeded`, `provider:overloaded`,
`provider:context_exhausted`, `workflow:launch_failed` and
`resume_reconcile:orphaned_by_process_loss`, with a unit test each.

### 3.2 Retry

`Block::Error`'s retry button resends the failed turn's input.
`SideState::command_text` keeps the prompt keyed by the failed turn id; **if the
text is gone the button is hidden**, because a retry that cannot resend anything
is a lie.

`turn/retryScheduled` draws `transcript::retry_row` — "Attempt n/m · retrying in
Ns · `<reason>`" on `status_row`'s primitives — with the same local clock the
question countdown uses, replaced by the turn's terminal when it arrives.

### 3.3 Wire errors

`overloaded` and `backpressured` raise a banner with a "Retry" action and an
automatic retry after the backoff. Auth-shaped failures keep the "Signed out"
dialog; the rest of the dialog family is unchanged from `docs/02-app.md` §7.

`session/setModel` on echo answers `commandRejected: invalid_target`, and
`session/compact` on a fresh session answers `missing_run`. Both are banners, not
dialogs — nothing is broken, the command simply did not apply.

---

## 4. Markers

Every `MarkerKind` renders with its glyph and a sentence a person would write.

| kind | sentence |
|---|---|
| `TurnCancelled` | Turn interrupted |
| `TurnRetracted` | Prompt retracted |
| `ViewGap` | Some events were missed while disconnected |
| `ForkedFrom` | Forked from `<source title or id group>`, titled from the local index |
| `ContextCompacted` | the token counts the compaction item carries |
| `RetryScheduled` | not a marker — it is the live row in §3.2 |

---

## 5. Fork

`/fork`, and an assistant turn's "Fork from here" action, send
`session/fork { sessionId, cutPoint: { lastTurnId } }`. `lastTurnId` is a **turn
id, not a count**, and only completed turns are valid cut points — an in-progress
one answers `forkBoundaryInvalid`, which is a banner. `/fork` with no argument
cuts at the newest completed turn.

The result is a resume envelope: the app opens the new session as the active one,
the sidebar refreshes, and a `ForkedFrom` marker sits at the top of the new
transcript. No model call, so it costs nothing.

---

## 6. `--replay`

`--replay <capture.jsonl>` (implies `--no-connect`) opens one session by folding
the capture's `<-- ` lines through `MuseFold` exactly as
`crates/muse-adapter/tests/fixtures.rs` does, renders it, and lets `--steps` and
`--screenshot` work on it. There is no child process and no server.

The sidebar shows one row labelled by the **file**, because a replayed session is
not one this host ever ran and the index has nothing to say about it. Commands
issued against a replayed session are refused with the banner "Replayed capture —
read-only".

Every capture under `fixtures/msp/` must open without panicking —
`fixtures.rs::every_capture_opens_without_panicking` is that gate — and this is
how almost every screenshot of this behaviour was taken.

---

## 7. The new `--steps`

`--steps` gains the phase-4 verbs (`docs/03-composer.md` §1 has the phase-3
grammar). Steps are `;`-separated because a payload may contain a comma.

| step | what it does |
|---|---|
| `shell:<cmd>` | `session/userShell` — raises an approval without a model call |
| `setmode:<mode>` | `session/setApprovalMode`, without opening the picker |
| `choose:<n>` | the n-th choice of the newest pending approval |
| `feedback:<text>` | type into an open feedback or clarify field |
| `answer:<label>` / `answers:<a\|b>` | pick options on the newest question |
| `confirm-answer` | send the answer |
| `preview:<n>` | open the n-th option's preview (0-based) |
| `select:<label>` | pick an option without sending |
| `clarify:<text>` | "Explain instead"; with no text, only opens the field |
| `skip` | decline the newest question |
| `fork` | `session/fork` at the newest completed turn |
| `retry` | retry the newest failed turn |
| `wait:<ms>` | let the wire catch up before the next step |

Phase 5 adds five more, four of which belong to the window rather than to a
session: `name:<text>` renames, `hide` hides, `resume` opens the session picker,
`palette` opens the command palette, `search:<text>` opens and fills the
sidebar's search field, and `rename[:<text>]` opens the row's inline field.

A `--screenshot` run now **waits for its steps to finish** before capturing, and
for a pending approval as well when a `shell:` step was given. The delay is
measured from the first frame, so a step list with a `wait:` in it used to
outlive the capture — which used to leave a capture showing the shell card
alone with no approval yet raised (finding F9).

Every one of these is free. None of them invents a fact: there is deliberately no
step that fabricates a todo list or a goal — those come from
`fixtures/msp/synthetic-todo-goal.jsonl` instead, which is labelled as
hand-written.

---

## 8. Keyboard (spec §3.9)

When a pending approval or question arrives and the composer draft is **empty**,
focus moves to the card — it has its own focus handle in a `HarnessCard` key
context. `1`–`9` choose, Enter confirms an open feedback or clarify field, Esc
collapses the card and returns focus to the composer, Tab moves between the card
and the composer.

With a **non-empty** draft focus stays where the person was typing, and the
needs-you banner's action is the way to the card. Nobody's half-written prompt
gets stolen by a permission request.

---

## 9. F3 — live and backfill must agree

A transcript arrives two ways: live, as `item/started` → `item/delta` →
`item/completed`; or backfilled, as `view/page` events that never replay a delta
and hand each item over whole. If the two disagree, reopening a session changes
what it says.

**They did disagree.** In `transcript-approve.jsonl` a `userShell` item starts
(log sequence 6), raises a two-stage approval (sequence 9), and only completes
afterwards. Live, the tool card is added at `item/started` and the approval lands
below it. Backfilled, there is no `item/started` at all: the approval arrives
first and the tool card only at its `item/completed` — so the two blocks came out
in opposite orders.

The fix is in `MuseFold::push_block`: a block is placed by the item's own
`sourceRange.first.sequence`, which is the **same number** on `item/started` and
on `item/completed`, rather than by arrival. `aui_protocol::Delta` has no insert
variant, so an out-of-order arrival is expressed as an append plus the
`BlockUpdated`s that rotate the tail, and every cached slot past the insertion
point shifts with it.

Two families of difference are **normalised** rather than fixed, because a
backfill genuinely cannot know them:

- **streaming flags** — a live text block is `streaming: true` while its deltas
  arrive and a live thinking block is mid-flight; both settle the same way on the
  turn's terminal, so both are settled before comparing;
- **turn metas** — the model id and the duration come off the live
  `turn/completed`, and a fold that never saw the turn start has no clock.

The gate is
`muse-adapter/tests/fixtures.rs::a_live_fold_and_a_backfilled_fold_agree`, which
derives the backfill stream from every checked-in capture that carries a streamed
item and costs nothing. `muse-client/tests/live.rs::live_backfill_parity` does
the same against a running server and **spends a real turn per run**, so it stays
`#[ignore]`d and is not the gate.

---

## 10. Deliberately not here

- **No optimistic card state.** Nothing moves on a press; everything moves on a
  notification. The one exception is opening a feedback or clarify field, which
  is local by definition.
- **No client-side approval policy.** The app never decides an approval itself,
  never pre-filters the server's choices, and never reorders them.
- **No "Other" row on a question.** MSP has no such thing: free text is
  `freeText` on an answer, and "let me explain instead" is `userInput/clarify`.
- **No invented todo, goal or timeout facts in `--steps`.** A screenshot of a
  state the server never produced would be a screenshot of nothing.
- **No retry of an approval.** An approval that failed to decide is re-decided
  from the card the server re-renders, not resent blind.
