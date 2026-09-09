# Phase 3 brief — composer controls (Harness, Muse Code chat slice)

You are the single lead for Phase 3. You own the work end to end: library changes in
`/Users/latekaapi/Projects/agentic-ui` (branch `muse-support`, NEVER `main`) and app changes in
`/Users/latekaapi/Projects/harness` (branch `main`). Commit once per repo at the end. Do not
touch `~/Projects/cockpit`. Prefix every shell command with
`export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`.

## Read first, in this order (do not skip)

1. `harness/docs/00-spec.md` — frozen spec. §1, §2, §3.1, §3.4, §3.5, §3.6, §3.9, §3.10, §4, §5 item 3, §6.
2. `harness/docs/CHANGELOG.md`, `harness/docs/01-transport.md`, `harness/docs/02-app.md`.
3. `agentic-ui/docs/00-agent-brief.md` (library rules), `agentic-ui/docs/04-design-rules.md`.
4. `agentic-ui/docs/10-muse-research.md` §1.5 (turns, steer, unqueue), §1.9 (models,
   setModel), §1.10 (context, compaction), §4.1 (no plan mode), §4.2 (slash commands, TUI
   keymap), §4.3 (skills), §7.2 (mapping table).
5. The code you extend: `harness/crates/harness/src/{session.rs,app.rs,transcript.rs,main.rs}`,
   `harness/crates/muse-adapter/src/{fold.rs,side.rs}`, `agentic-ui/crates/aui/src/composer/*.rs`,
   `agentic-ui/crates/aui/src/data/meter.rs`, `agentic-ui/crates/aui/src/overlay/`,
   `agentic-ui/crates/aui-protocol/src/{intent.rs,session.rs}`,
   `agentic-ui/crates/aui-gallery/src/cards/{composer.rs,menus.rs}` and `registry.rs`,
   `agentic-ui/crates/aui/examples/minimal.rs`.
6. Schema ground truth: `harness/fixtures/msp/msp-ts/msp.d.ts`. Captures win over the doc,
   the doc wins over memory.

## Scope (spec §5, phase 3) — all of it

Model, effort and mode menus; context meter and compaction; queue strip with steer; `@`
mentions and `/` command menu with skills; plan mode with the `/plan` probe; prompt history;
images (paste / drop / attach). Plus three Phase 2 review findings (F1, F4, F5 below).

**Gate:** every control changes real session state and the UI reflects the server's
notification, not an optimistic local write. Screenshots, light and dark, 1440×900, of every
new state (list under Deliverables).

## Decisions already made (do not re-litigate; record deviations in CHANGELOG)

### Library (agentic-ui, branch `muse-support`)

L1. **Pickers.** `aui::composer::{model_menu, effort_menu, mode_menu}` — three stateless
    `RenderOnce` list menus built on `composer::menu` (plus_menu) primitives: same morph spring,
    same overlay ground, painted through `overlay::popover_layer`, anchored to the composer
    chip that opened them. Rows: label, one-line detail, a selected check; the model menu also
    shows context limit and `default`/`active` badges (rows may have zero or several `active`
    — never assume one). Data in (`Vec<Row>`, `selected: usize`, `open: bool`), intents out
    (`on_pick(id)`, `on_hover(idx)`, `on_close`). Keyboard: ↑/↓ move, Enter picks, Esc
    closes; ⌘⇧M / ⌘⇧E / ⌘⇧P open them (bind in the app's key context; see spec §3.9).
    `Composer` gets one slot per picker mirroring `.plus_menu(open, menu)` (or one
    `.chip_menu(ComposerChipAnchor, open, menu)` — your call; the anchor must be the chip).

L2. **Context meter.** `aui::data::context_meter`: ring + percent; states `Normal | Warning |
    Blocked` drive the ring colour from tokens; a no-denominator mode (`window_tokens: None`)
    renders "N tokens" with an empty ring; hover reveals a breakdown popover (used, window,
    cumulative prompt/output/total) with a **Compact** button; `Blocked` shows Compact inline.
    `Composer::context_percent(u8)` is replaced by `.context(ContextMeterState)`; update the
    gallery and `examples/minimal.rs`.

L3. **Queue strip.** `aui::composer::queue_strip`: a column of `queue_row`s above the docked
    composer with a small "Queued · n" header. `QueueIntent` gains `Steer`. Intents carry the
    row id. No optimistic reorder: the strip renders exactly what it is given.

L4. **Composer additions.** A "Plan" pill in the footer (`.plan(bool)`, click → a new
    `ComposerIntent::ExitPlan`); `ComposerIntent::Steer` (the app binds ⌘Enter to it) and
    `ComposerIntent::Attach`; `can_send(false)` is what `Blocked` pressure uses. Image chips
    reuse `ComposerChipKind::Image`; `composer::attachments::drop_overlay` shows while a drag
    hovers the window.

L5. **aui-protocol.** Verify `Intent::{Steer, Unqueue, EditQueued, Compact, SetModel,
    SetEffort, SetMode}` exist from Phase 1; add whatever the app needs that is missing (no
    renames of existing variants). `PermissionMode` stays the four MSP modes; plan is a
    client flag on the app's session, not a protocol mode.

L6. **Gallery.** Every new component gets a gallery entry with sample data in both themes:
    the three pickers (open), the meter in all four states (normal / warning / blocked /
    no-denominator, plus the hover breakdown), the queue strip with three rows (one in edit
    state), the composer with the Plan pill and an image chip.

L7. Library gates before the commit: `cargo build --workspace`, the all-features build
    (`--features aui-webview/wry,aui-terminal/pty,aui-terminal/tui`), `cargo test --workspace`,
    `cargo clippy --workspace --all-targets -- -D warnings`, `RUSTDOCFLAGS=-D warnings cargo doc
    --workspace --no-deps`, then `python3 scripts/api-doc.py` to regenerate `docs/06-api.md`.
    No literal colours, sizes or durations in components; both themes.

### App (harness)

A1. **`Overlays` entity** (spec §2.3, docs/02-app.md §2): move the dialog there and add the
    open-menu state (which picker/menu, selected index, filter text) and a toast slot. One
    entity, rendered on `popover_layer`.

A2. **Model.** On open: `model/list { sessionId }` (background task; snapshot, no subscription)
    → rows with `isActive` selected. Pick → `session/setModel { modelId, providerId, profileId }`.
    The chip changes only on `session/modelChanged`. On echo the server answers
    `commandRejected: unsupported_route`; that goes to the existing inline banner — screenshot
    it, it is correct behaviour.

A3. **Effort.** Client-side per-session `Option<ReasoningEffort>` from the MSP enum
    (`none…xhigh, ultra`, never `max`); `None` = "Default" (omit the field). Sent on every
    `turn/start` and `turn/steer`. Check `msp.d.ts` for any echo of `reasoningEffort` on the
    `userMessage` item or `turn/started`; if there is none, effort is the one control with no
    server reflection — the chip shows the client value and the CHANGELOG says so. Probe once
    on echo whether a non-default effort is accepted (`/effort` is provider-gated in the TUI);
    if echo rejects it, disable the chip on echo with a tooltip.

A4. **Mode.** `session/setApprovalMode` with the four modes (labels per spec §3.6); the chip
    updates on `session/approvalModeChanged`, which also draws the marker the fold already
    emits. Apply **F5**: do not synthesize the initial "Approval mode · Auto" marker unless the
    mode differs from the default.

A5. **Context meter and compaction.** Meter state from `SideState.context` (+ `cumulative`
    for the breakdown). Compact → `session/compact {}`; an ack of `noop` is a success: toast
    "Nothing to compact · <reason>". The `compaction` item's marker already renders. `blocked`
    → composer `can_send(false)` with the meter offering Compact.

A6. **Queue and steer** (spec §3.4). Enter while a turn runs → `turn/start` (wire default
    `ifBusy: queue`), the ack's `disposition: queued` tells you which. ⌘Enter → `turn/steer
    { expectedTurnId: <running turn> }`. Strip rows come from `SideState.queued` only (server
    order). Edit → `turn/unqueue`, and the text is restored to the composer when
    `turn/unqueued` arrives (via the fold's `command_text`); Remove → `turn/unqueue`; Steer
    now → `turn/unqueue`, then on `turn/unqueued` send `turn/steer` with the same text. Never
    reorder or remove optimistically.

A7. **`/` command menu.** `aui::composer::command_menu` opens on `/` at line start, filters as
    you type, ↑/↓/Enter/Esc. Sections: **Commands** (spec §3.10 list) and **Skills** from
    `muse skills list --json` run once at boot on a background thread — shape is
    `{"skills":[{"id":"bundled:browser-app-delivery","name":…,"display_name":…,"description":…}]}`;
    tag each with the scope prefix of `id` (`bundled`/`user`/`project`/`plugin`) via
    `CommandItem::source_tag`. A skill inserts `/name ` as text. Client commands implemented
    now: `/model` `/effort` `/mode` (open the picker), `/plan` (toggle), `/compact`, `/clear`
    (new session), `/logout`, `/help` (opens the menu unfiltered), `/status` and `/usage`
    (a dialog built from `SideState`: model, mode, effort, context, cumulative tokens, session
    id, workspace, branch). `/fork` is Phase 4 and `/name` `/resume` are Phase 5: they stay in
    the list and show a toast "Not in this build yet".

A8. **`@` mentions.** `aui::composer::mention_picker` opens on `@`; candidates are workspace
    files walked once in the background with the `ignore` crate (respects `.gitignore`, skips
    `.git`, capped at 5 000 entries, re-walked on ⌘N), filtered by subsequence match, ↑/↓/Enter.
    Picking inserts `@relative/path ` into the text. On the wire mentions are plain text inside
    the text part (research §1.5); no structured part.

A9. **Plan mode** (spec §3.1). **Probe first, one real turn:** start a `meta` session and send
    the literal text `/plan reply with a one-line plan for printing hello` as the text part.
    Look at the items: a `toolCall` that reads a skill, or a reply shaped by the skill's
    prompt, means the skill fires server-side; a plain reply means it does not. Record the
    verdict and the evidence in CHANGELOG. If it fires: plan mode sends `/plan <text>` (with
    `displayText` = the user's text) under `denyUnmatched`. If not: the preamble from
    `crates/harness/src/plan.rs`. Either way: Shift+Tab / `/plan` toggle, Plan pill, remember
    and restore the previous approval mode (reflected via `approvalModeChanged`, not assumed),
    and when the reply completes append `Block::Plan` (headings → steps) with Accept / Refine /
    Reject exactly as §3.1 says. One more real turn to run the full flow and screenshot it.

A10. **Prompt history.** Every sent prompt is appended to
    `~/Library/Application Support/harness/history.json` keyed by canonical workspace, last
    200, deduplicated on consecutive repeats. ↑ on the first line / ↓ on the last line walks
    it (the current draft is kept as the newest slot). Find the caret line from gpui-kit's
    `TextareaState`; if the API does not expose it, fall back to "↑ when the caret is at
    offset 0 or the draft is empty, ↓ when at the end", and say so in the phase doc.

A11. **Images.** Paste (`cx.read_from_clipboard()` → `ClipboardEntry::Image`), drop
    (`on_drop::<ExternalPaths>` with `drop_overlay`), attach (plus menu "Attach image" →
    `cx.prompt_for_paths`). Each becomes a removable image chip and a `TurnInputPart::Image
    { base64Data, mediaType, width, height }` (width and height together or neither). Probe on
    echo whether an image part is accepted; if echo rejects it, spend one real turn to prove
    the path end to end. Cap 10 MB per image; PNG/JPEG/GIF/WebP.

A12. **Review findings.** F1: map Muse tool names (`read`/`read_file`/`view`, `write`/`edit`/
    `create`, `grep`/`glob`/`search`, `web_*`, `fetch`) onto `ToolKind::{Read, Edit, Search,
    Web}` in `muse-adapter::tool_shape` using `rawArgs` fields (`path`, `file_path`, `pattern`,
    `query`, `url`) so the verb and body match; regenerate the fold snapshots and read the
    diff. F4: hide the `$0.00` cost cell in the turn footer when the catalog reports no price.
    F5: see A4. Findings F2 (humanized failure reasons, `modelError` message) and F3 (live vs
    backfill fold parity) are Phase 4 — leave them.

A13. Harness gates before the commit: `cargo build --workspace`, `cargo test --workspace`,
    `cargo clippy --workspace --all-targets -- -D warnings`, `RUSTDOCFLAGS=-D warnings cargo doc
    --workspace --no-deps`. Exactly one `gpui-pre` and one `gpui-kit` in `cargo tree -d`.

## Wire facts that cost time last time

- Always run `muse serve` durable; `--no-session-log` emits no view events.
- Provider per session: `session/start { providerId: "echo" }` is free; `meta` is real.
  **Budget: at most 5 real turns this phase** (plan probe, plan flow, image if needed; keep
  two in reserve). Say in the report how many you spent.
- `commandId` is UUIDv7; view events may precede a command's ack; the ack is admission, not
  outcome. `turnId` from `turn/start`'s ack is authoritative.
- Effort enum on the wire: `none|minimal|low|medium|high|xhigh|ultra`; `max` is
  `invalidParams`.
- `model/list` ignores `providerId` (echo sessions see the meta catalog); `cost` is `null`.
- `session/setModel` on echo → `commandRejected: unsupported_route`.
- `session/compact` may ack `noop` — a success.
- A new session appears in `session/list` only after `turn/completed`.
- `HARNESS_PROVIDER=echo cargo run -p harness`; `--screenshot <png>`, `--screenshot-delay`,
  `--session <id>|latest`, `--send <text>`, `--no-connect`, `--theme light|dark` already exist.
  Extend the scripting flags as you need (for example `--steps` like the gallery's
  `AUI_GALLERY_STEPS`) so every screenshot is reproducible from a command line.

## Deliverables

1. Commits: agentic-ui `muse-support` (library), harness `main` (app). Messages end with
   `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`. No commits on agentic-ui `main`.
2. `harness/docs/03-composer.md`: how each control round-trips (command → notification →
   chip), the plan-mode verdict, history and image details, the scripting flags, what is
   deliberately not here. `harness/docs/CHANGELOG.md` gets a Phase 3 entry with the probe
   findings and real-turn spend. `docs/05-handoff.md` state block updated.
3. Screenshots in `harness/docs/images/phase3-*.png`, light and dark, 1440×900: model menu
   open, effort menu open, mode menu open, context meter breakdown hovered, meter in blocked
   state (fake it by folding a synthetic `session/contextUsage` under a test flag if echo
   cannot reach it — say so), queue strip with two queued rows, command menu with skills,
   mention picker, plan mode pill with a completed `Block::Plan`, an image chip in the
   composer, the `unsupported_route` banner on echo.
4. A final report under 400 words: what round-trips against the server and how you verified
   each, the plan probe verdict, real turns spent, files touched, screenshot paths, anything
   you could not verify.
