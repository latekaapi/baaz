# Phase 4 brief — approvals, questions, errors (Harness, Muse Code chat slice)

You are the single lead for Phase 4. You own the work end to end: library changes in
`/Users/latekaapi/Projects/agentic-ui` (branch `muse-support`, NEVER `main`) and app changes in
`/Users/latekaapi/Projects/harness` (branch `main`). Commit once per repo at the end. Do not
touch `~/Projects/cockpit`. Prefix every shell command with
`export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`.

## Spend rule (read twice)

Phase 3 spent 25 real `meta` turns against a cap of 5 because screenshot runs were started
without `HARNESS_PROVIDER=echo`. The owner counts real turns from
`~/.local/share/muse/session-index.db`, never from your report. Therefore:

- **Prefix EVERY app invocation with `HARNESS_PROVIDER=echo`**, including `cargo run -p harness`
  with no flags, the probes, and the `--ignored` tests. No exceptions.
- **Cap: 5 real `meta` turns this phase.** Before each one, write one line in your notes
  naming the turn and why echo cannot do it, then run it with `--provider meta` explicitly.
  Everything approval-shaped is free on echo through `session/userShell` (research §2.4).
  Only `userInput/*` (the model's `request_user_input` tool) needs `meta`.
- Most screenshots this phase come from `--replay <capture>` (decision A0) with no server at
  all. If a screenshot needs a server, it needs echo.

## Read first, in this order (do not skip)

1. `harness/docs/00-spec.md` — §1, §2, §3.3, §3.8, §3.9, §4, §5 item 4, §6.
2. `harness/docs/CHANGELOG.md`, `harness/docs/01-transport.md` (§2 ordering rule, §3
   reconnect, §4 discrepancies 1–3), `harness/docs/02-app.md` (§3 thread model, §7 errors),
   `harness/docs/03-composer.md` (§1 the `--steps` grammar you extend).
3. `agentic-ui/docs/00-agent-brief.md`, `agentic-ui/docs/04-design-rules.md`.
4. `agentic-ui/docs/10-muse-research.md` §1.7 (approvals — the whole section, including the
   captured multi-stage flow), §1.8 (user input), §1.11 (todo, goal), §1.12 (user shell),
   §1.14 (error table), §2.4 (what echo cannot exercise), §7.2 (mapping table). Also lines
   240–253 (`session/fork`) and the `turn/retryScheduled` row in §1.5.
5. The code you extend: `harness/crates/harness/src/{session.rs,app.rs,transcript.rs,
   overlays.rs,conn.rs,main.rs,plan.rs,skills.rs}`, `harness/crates/muse-adapter/src/{fold.rs,
   side.rs}`, `harness/crates/muse-adapter/tests/fixtures.rs`, `harness/crates/muse-client/src/
   client.rs` (every `approval_*`, `user_input_*`, `session_fork`, `session_user_shell` call
   already exists), `agentic-ui/crates/aui-protocol/src/{block.rs,intent.rs}` (every shape
   below already exists — read `Block::Approval`, `Block::Question`, `ApprovalState`,
   `ApprovalChoice`, `ApprovalStage`, `ResolvedBy`, `QuestionPreview`, `MarkerKind`),
   `agentic-ui/crates/aui/src/transcript/{approval.rs,question.rs,plan.rs,status.rs,marker.rs,
   todo.rs}`, `agentic-ui/crates/aui/src/feedback/banner.rs`, `agentic-ui/crates/aui/src/
   composer/model_menu.rs` (or wherever `model_menu` lives), `agentic-ui/crates/aui/tests/
   keyboard.rs` (how the approval card is driven by keys today), `agentic-ui/crates/aui-gallery/
   src/cards/{approval.rs,question_plan_todo.rs,summary_status.rs,markers.rs}` and `registry.rs`.
6. Schema ground truth: `harness/fixtures/msp/msp-ts/msp.d.ts`. Captures win over the doc,
   the doc wins over memory. The captures that matter here: `transcript-approve.jsonl`
   (2-stage approval, `promptUnmatched`), `transcript-wire.jsonl` (a policy denial under
   `denyUnmatched`, `view/page`), `transcript-real.jsonl` (a `userInput/request` on the real
   provider and its `userInput/settled`).

## Scope (spec §5, phase 4) — all of it

Approval card v2 (server-minted choices, multi-stage with `n/N`, feedback, badges, policy and
judge resolutions) with a real `approval/decide` round-trip; question card with header,
previews, timeout countdown, clarify and cancel via `userInput/*`; error handling (F2
humanized failures, retry, retry-scheduled countdown, wire-error banners with actions, dialogs);
every marker kind; fork; todo; goal; generic item card; findings F3, F6, F7, F8; an offline
`--replay` mode for captures.

**Gate:** `transcript-approve.jsonl`, `transcript-wire.jsonl` and `transcript-real.jsonl`
replay (through `--replay`) to the intended cards, screenshotted; one real multi-stage
approval driven from the UI end to end (`!echo hi && ls` under `promptUnmatched` on **echo**,
both stages decided by clicks/keys, `approval/resolved` observed, the shell item completing);
the F3 parity test passes on echo.

## Decisions already made (do not re-litigate; record deviations in CHANGELOG)

### Library (agentic-ui, branch `muse-support`)

L1. **`approval_card` v2** (spec §3.3). Stays a stateless `RenderOnce` with data in and
    intents out. Additions, all optional so the existing triad path keeps working:
    - `.choices(Vec<ApprovalChoice>)`: the buttons are the server's choices in server order,
      drawn with the action-row pattern (key hints left, spacer, buttons right). Primary
      styling goes to the choice whose `decision` is `Once`/`ApprovedForSession`; a
      `PolicyAmendment` choice shows its `rule_preview` in the mono face under the label at the
      existing rule opacity; `Deny`/`Abort` choices use the deny styling. Wide labels
      ("Always allow in this workspace: echo ...") wrap or the row wraps — never truncate.
      Intent: `.on_choose(Fn(choice_id: String, feedback: Option<String>, ..))`. When
      `choices` is empty the card renders the built-in triad and `on_decide` exactly as now.
    - `.stages(Vec<ApprovalStage>, current: Option<usize>)`: a stage strip under the command —
      `Stage n/N` pill, each stage's `argv` joined in mono, resolved stages checked, the
      current one highlighted, `argv_complete == false` marked "(partial parse)". Single-stage
      subjects show no strip.
    - `.badges(ApprovalBadges)`: "Protected write" and "Judge escalated" pills in the header
      (warning tint), only when set.
    - Feedback: a choice with `accepts_feedback` does not fire `on_choose` immediately. The
      card gets `.feedback_open(Option<choice_id>)` and `.feedback_slot(AnyElement)`: when open,
      the card draws the slot (the app passes a gpui-kit input it owns — same pattern as the
      composer's editor) plus "Send" and "Cancel"; Enter in the input confirms, which the app
      turns into `on_choose(choice_id, Some(text))`. The card never owns text.
    - Resolved states: `.resolved_by(Option<ResolvedBy>)`. `Policy`/`LlmJudge` with
      `AutoAllowed`/`AutoDenied` render the quiet single line "Allowed by policy · <rule>" /
      "Denied by policy · <rule>" / "Allowed by the approval judge" / "Denied by the approval
      judge" and are never actionable, with no enter animation of the buttons. `User` +
      `Denied` reads "Denied" and, if the block carries `feedback`, shows it as a quoted line.
    - Keyboard: digits 1–9 pick the n-th choice in order (extend the existing key context and
      the keystroke test in `tests/keyboard.rs`); Enter confirms an open feedback field; Esc
      closes the feedback field, then collapses the card.
    - Title: the pending title must not say "Claude Code". Make it `.title(..)` with a default
      of "Allow this command?"; the app passes "Allow Muse to run this command?".

L2. **`question_card`** additions: `.header(..)` (small label above the prompt, MSP
    `header`); per-option previews — `QuestionOption::preview` draws a "Preview" chevron on the
    row, `.previews_open(Vec<usize>)` data-in and `.on_toggle_preview(idx)` out; an open preview
    renders `content` by `format`: `markdown` through `transcript::prose`, `diff` through the
    existing diff/code block, anything else mono text. `.timeout(remaining_ms, total_ms)` draws
    a countdown pill "Auto-resolves in 12 s" in the header (the app ticks; the card is pure).
    `.on_clarify(..)` adds an "Explain instead" button; `.clarify_open(bool)` +
    `.clarify_slot(AnyElement)` is the same slot pattern as L1's feedback. `on_skip` is the
    cancel path (label it "Skip"). `answered_row` gains an outcome variant so cancelled /
    clarified / timed-out settlements read as such ("Skipped", "Clarified: …", "Timed out"),
    not as an empty answer. Multi-select honours `min`/`max` via `.limits(Option<usize>,
    Option<usize>)`: the confirm button is disabled outside the range and the hint says why.

L3. **`transcript::retry_row`**: "Attempt n/m · retrying in Ns · <reason>" on `status_row`'s
    primitives (spinner lead, shimmer off, note = reason), data-in `(attempt, max,
    remaining_ms, reason)`. Pure; the app derives `remaining_ms` from `retryDelayMs` and a
    local clock.

L4. **`transcript::generic_item_card`**: the mandated fallback — kind name, status pill,
    `fallbackText`. Replace the app's hand-built `transcript_card` for `Block::Generic`.

L5. **`transcript::goal_card`**: objective (semibold), status as a pill (verbatim string), a
    progress bar clamped to 0–100 for display with the verbatim number beside it (a value over
    100 shows the number, the bar full), current work / next work as two definition rows.
    Replace the app's hand-built goal header.

L6. **F6 — plan sections.** `aui_protocol::Block::Plan` gains
    `#[serde(default)] sections: Vec<PlanSection { label: String, first_item: usize }>`
    (additive; `sample.rs` and existing callers unchanged). `plan_card` gains
    `.sections(Vec<PlanSection>)` and draws the label as an unnumbered section row before
    `first_item`; numbering counts items only. The harness's `plan::steps` then returns
    (items, sections): headings → sections, list items → steps, and a heading with no items
    under it is dropped.

L7. **F8 — model menu labels.** The model menu sizes to its widest label (min width from a
    token, max from a token, then wrap the detail line). No label is ever ellipsised.

L8. **Banner action.** `feedback::banner` already has an action slot; use it. If a variant
    for a countdown ("Retrying in 3 s") is needed, add it there, not in the app.

L9. **Gallery**, both themes, sample data: `transcript/approval` gains pending stage 1/2 with
    three server choices, stage 2/2 with one stage resolved, feedback open, judge-escalated +
    protected-write badges, policy-allowed, policy-denied, judge-denied; `transcript/
    question-plan-todo` gains a question with header, a preview open, a timeout pill, clarify
    open, and a plan with sections; `summary_status` (or a new entry) gains `retry_row`,
    `generic_item_card`, `goal_card` (percent 40 and percent 120). Keystroke test extended for
    digit choice on a choices card.

L10. Library gates before the commit: `cargo build --workspace`, the all-features build
    (`--features aui-webview/wry,aui-terminal/pty,aui-terminal/tui`), `cargo test --workspace`,
    `cargo clippy --workspace --all-targets -- -D warnings`, `RUSTDOCFLAGS=-D warnings cargo doc
    --workspace --no-deps`, then `python3 scripts/api-doc.py` to regenerate `docs/06-api.md`.
    No literal colours, sizes or durations in components; both themes.

### App (harness)

A0. **`--replay <capture.jsonl>`** (new flag, implies `--no-connect`): open one session by
    folding the capture's `<-- ` lines through `MuseFold` exactly as `tests/fixtures.rs` does,
    render it, and let `--steps`/`--screenshot` work on it. Commands issued against a replayed
    session are refused with the banner "Replayed capture — read-only". This is how most
    Phase 4 screenshots are taken and it costs nothing. Every capture under `fixtures/msp/`
    must open without panicking.

A1. **Approval round-trip** (spec §3.3, research §1.7). A choice → `approval/decide
    { approvalId, choiceId, requirementId: <the block's current stage's requirementId>,
    feedback? }` on a background task. Keep the MSP `requirementId` per stage in `SideState`
    (or on the fold's request map) — the block's `ApprovalStage` has no room for it and that is
    fine. The card re-renders only from `approval/updated` (choices change between stages —
    never cache them) and `approval/resolved`; the ack's `terminal` flag is admission only.
    Errors: `approvalAlreadyResolved` → fold `data.resolution` into the block as resolved;
    `approvalChoiceInvalid` → banner and re-render; `approvalRequirementStale` → silent (the
    update already moved the stage); `approvalNotFound` → mark the block resolved-unknown and
    banner. `needs_you_banner` appears above the composer while any pending approval or
    question is scrolled out of view; its action scrolls to the card. On `session/resume` and
    on reconnect call `approval/listPending` and fold both lists (dedupe on ids).

A2. **User shell.** A draft starting with `!` sends `session/userShell { commandText }`
    instead of a turn (research §1.12; the capability is already requested in `conn.rs`, check
    `grantedCapabilities` and disable the path with a banner when it is missing). This is the
    free approval generator: under `promptUnmatched`, `!echo hi && ls` raises the 2-stage
    approval from `transcript-approve.jsonl`. `--steps` gains `shell:<cmd>`, `choose:<n>`
    (n-th choice of the newest pending card), `feedback:<text>`, `answer:<label>`,
    `answers:<a|b>`, `clarify:<text>`, `skip`, `fork`, `retry`, `wait:<ms>`. The gate's
    multi-stage approval is: `HARNESS_PROVIDER=echo … --steps 'mode;…' ` or simply set the
    mode with `session/setApprovalMode promptUnmatched` first, then `shell:echo hi && ls`,
    `wait:800`, `choose:1`, `wait:800`, `choose:1`. Screenshot both stages and the resolved card.

A3. **Questions** (research §1.8). `Answer` → `userInput/answer { answers: [{questionId,
    selectedLabel | selectedLabels | freeText, note?}] }` — labels, not indices. Skip →
    `userInput/cancel`; clarify → `userInput/clarify { clarification: {content, format:
    "text"} }`. The fold emits one `Block::Question` per MSP question; when a request carries
    N > 1 questions, the app holds the answers locally and sends one `userInput/answer` when the
    last is answered (the cards show "n of N"). `autoResolutionMs` drives a local countdown
    (a 1 s foreground timer while a timed question is pending; stop it on settle). `userInput/
    settled` → `answered_row` with the outcome variant. `userInputAlreadySettled` → fold
    `data.settlement`; `userInputAnswerInvalid` → banner on the card. **This is the only part
    that needs `meta`:** budget 2 real turns, named before spending — (1) a prompt that makes
    the model call `request_user_input` with two options ("Ask me which of README.md or
    notes.txt to describe, using your request_user_input tool, then describe it") and answer
    it from the UI; (2) the same prompt, answered with clarify. Capture both to
    `fixtures/msp/transcript-userinput-*.jsonl` through `muse-client`'s capture path (or the
    Python probes) so the replay test covers them afterwards. If turn (1) does not produce a
    question, do not spend more than one extra attempt; report it.

A4. **Errors.** F2: `muse-adapter::failure::humanize(kind, message, reason) -> (title,
    detail)`: title from `TurnErrorKind` ("Model error", "Configuration error", "Step limit
    reached", "Environment error", "Launch error", "Projection error", "Log error", "Workflow
    launch error", unknown → the kind itself), detail = `error.message`, and a `reason` code
    like `resume_reconcile:orphaned_by_process_loss` becomes a sentence ("The turn was orphaned
    when the session's process was lost") with the raw code kept on a second mono line. Put the
    table in one place with a unit test per known reason. `Block::Error` retry → resend the
    failed turn's input (`SideState::command_text` keyed by the failed turn id; if the text is
    gone the button is hidden). `turn/retryScheduled` → `retry_row` with the local countdown
    (same timer as A3), replaced by the turn's terminal. Wire errors: `overloaded` and
    `backpressured` → banner with a "Retry" action and an automatic retry after the backoff;
    the dialog family stays as in `docs/02-app.md` §7. Auth-shaped failures keep the existing
    "Signed out" dialog.

A5. **Markers.** Every `MarkerKind` renders with its glyph and a sentence a person would
    write: `TurnCancelled` "Turn interrupted", `TurnRetracted` "Prompt retracted", `ViewGap`
    "Some events were missed while disconnected", `ForkedFrom` "Forked from <source title or
    id group>" using the index for the title, `ContextCompacted` with the token counts the
    compaction item carries, `RetryScheduled` only as the live row (A4). Verify each in
    `--replay` or with a synthetic capture (A7).

A6. **Fork.** `/fork` and an assistant turn's "Fork from here" action → `session/fork
    { sessionId, cutPoint: { lastTurnId } }` (completed turns only; the newest completed turn
    when invoked from `/fork`). The result is a resume envelope: open the new session as the
    active one, sidebar refreshes, `ForkedFrom` marker at the top. `forkBoundaryInvalid` →
    banner. Free on echo. Remove the "Not in this build yet" toast for `/fork`.

A7. **Todo and goal.** The fold already handles `session/todoListChanged` and
    `session/goalChanged`; make sure a whole-list replace, an empty list (block removed) and a
    `null` goal (block removed) all work, with unit tests. No capture carries either event and
    echo never emits them, so add `fixtures/msp/synthetic-todo-goal.jsonl` — a hand-written
    capture in the same `<-- ` format, named `synthetic-` so nobody mistakes it for the wire —
    with a todo list that changes twice and a goal that is set, updated past 100 and cleared.
    Snapshot it like the others and screenshot it through `--replay`. Do not add `--steps`
    that invent todo/goal facts.

A8. **F3 — parity test.** `cargo test -p muse-client -- --ignored live_backfill_parity` on
    echo: start a session, send one echo turn and fold it live; then `session/resume
    { excludeItems: true }` + `view/page` forward from the start into a second fold; assert the
    two `Session`s are equal after normalising streaming flags (`streaming: false`, thinking
    state `Done`) and turn metas that backfill cannot know. Fix the fold where they differ and
    write the differences you fixed in the CHANGELOG.

A9. **F7.** A skill whose name equals a client command's name (`plan`) is hidden from the
    `/` menu. **F8** is L7. **F6** is L6 plus `plan::steps`.

A10. **Keyboard** (spec §3.9). When a pending approval or question card arrives and the
    composer draft is empty, focus moves to the card (its own focus handle in a `HarnessCard`
    key context); 1–9 choose, Enter confirms feedback/clarify, Esc collapses the card and
    returns focus to the composer; Tab moves between the card and the composer. With a
    non-empty draft, focus stays in the composer and the banner's action is the way to the card.

A11. Harness gates before the commit: `cargo build --workspace`, `cargo test --workspace`,
    `cargo clippy --workspace --all-targets -- -D warnings`, `RUSTDOCFLAGS=-D warnings cargo doc
    --workspace --no-deps`. Exactly one `gpui-pre` and one `gpui-kit` in `cargo tree -d`.
    Regenerate fold snapshots with `UPDATE_SNAPSHOTS=1` and read every diff.

## Wire facts that cost time last time

- Always run `muse serve` durable; `--no-session-log` emits no view events.
- Provider is per session; echo emits one canned message and nothing else: no tool cards, no
  reasoning, no usage, no `userInput/*`. Approvals ARE free through `session/userShell`.
- `commandId` is UUIDv7; `approval/request` and `userInput/request` are server requests
  answered only through `approval/decide` / `userInput/*` (never with a JSON-RPC result);
  dedupe against the `…/requested` notification on the id; view events may precede an ack.
- An approval's `itemId` is its own id; for a user shell its `turnId` is the shell
  `commandId`. The fold already files both in one turn.
- `approval/decide.requirementId` must equal `currentRequirementId` or the wire answers
  `approvalRequirementStale`; choices change between stages.
- `userInput/answer` keys on option **labels**; free text ≤ 500 chars; `attachments` is
  reserved and rejected.
- `session/fork.cutPoint.lastTurnId` is a turn id, not a count; in-progress turns are
  `forkBoundaryInvalid`.
- `session/setModel` on echo → `commandRejected: invalid_target`; `session/compact` on a
  fresh session → `missing_run`. Both are banners.
- `HARNESS_PROVIDER=echo cargo run -p harness -- --workspace <dir> --theme dark --screenshot
  <png> --screenshot-delay <ms> --steps '<a;b;c>'` is the screenshot shape; `--replay` joins
  it now. Scripted runs default to echo already, but prefix the variable anyway.

## Deliverables

1. Commits: agentic-ui `muse-support` (library), harness `main` (app). Messages end with
   `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`. No commits on agentic-ui `main`.
2. `harness/docs/04-approvals.md`: the approval state machine as the app implements it
   (request → stages → decide → updated → resolved, with the error re-renders), the question
   flow, the error classification table as built, retry and retry-scheduled, fork, `--replay`,
   the new `--steps`, what is deliberately not here. `harness/docs/CHANGELOG.md` gets a Phase 4
   entry with findings, F2/F3/F6/F7/F8 closed, and the real-turn spend (named turns).
   `docs/05-handoff.md` state block updated for Phase 5.
3. Screenshots in `harness/docs/images/phase4-*.png`, light and dark, 1440×900: approval
   stage 1/2 pending with choices; stage 2/2 with stage 1 resolved; feedback field open;
   resolved by user; policy-denied card (from `transcript-wire.jsonl`); question card with
   header and a preview open; question with timeout pill; clarify open; answered row;
   humanized error card with retry; retry-scheduled row (synthetic capture is acceptable —
   say so); the reconnect/overloaded banner with its action; todo + goal from the synthetic
   capture; forked session with its marker; plan card with sections; the widened model menu.
4. A final report under 400 words: what round-trips against the server and how you verified
   each, the gate evidence (which capture/screenshot proves what), **the named list of real
   turns spent**, files touched, screenshot paths, anything you could not verify.
