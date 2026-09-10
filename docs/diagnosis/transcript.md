# Section C — Transcript diagnosis (Chat Transcript items 1–9)

Scope: `crates/harness/src/transcript.rs` + `session.rs` (render path),
`crates/muse-adapter/src/fold.rs` (cards), `crates/muse-client/src/schema.rs`
(wire), `aui` transcript components (library), gpui-pre 0.3.3 sources.
Required reading done: `CLAUDE.md`, `docs/05-handoff.md`,
`docs/09-handoff-improvements.md` §§4+7, `docs/07-architecture.md`,
agentic-ui `docs/00-agent-brief.md` (skimmed), brief
`docs/briefs/muse-diagnose-improvements.md`.

Conventions used below: `session.rs` = `crates/harness/src/session.rs`,
`transcript.rs` = `crates/harness/src/transcript.rs`, `fold.rs` =
`crates/muse-adapter/src/fold.rs`, `schema.rs` =
`crates/muse-client/src/schema.rs`. `aui/*` paths are under
`/Users/latekaapi/Projects/agentic-ui/crates/aui/src/`. `G:` =
`~/.cargo/registry/src/index.crates.io-*/gpui-pre-0.3.3/src`.

Screenshot taken for this report:
`docs/diagnosis/shots/transcript-wire-dark.png` (`--replay
fixtures/msp/transcript-wire.jsonl --theme dark`). It shows two shell tool
cards plus two "Denied by policy" approval cards stacked individually —
evidence for item 8. Zed sources are **not present** on this machine
(`find ~ -maxdepth 4 -type d -name zed` empty; no Zed checkout under
`~/Projects`), so all Zed claims below are marked not-verified/from-memory.

## C1. Scroll jank — "scroll performance is janky … should be smooth and native"

Symptom (owner): "The primary issue here is that the scroll performance is
janky. … The scroll should be smooth and native. It should work well even
for long transcripts, even when new messages are streamed."

Root cause. The transcript is a plain scrollable `div` that builds **every
cell every frame**; there is no virtualization:

- `session.rs:1856-1870` (`render_transcript`): `div().id("transcript")
  .track_scroll(&self.scroll).overflow_y_scroll().flex().flex_col()` with one
  `.child(element)` per block of every turn (`session.rs:1871-1875`). No
  `list()` / `uniform_list` / `ListState` anywhere in the harness (grep for
  `uniform_list|ListState|list(` in `session.rs` finds only the comment at
  `session.rs:1769` "scrolls the list down" — no widget).
- Scroll state is a bare `ScrollHandle::new()` (`session.rs:354`) plus a
  `follow: bool` (`session.rs:219`). Every event that changes the fold sets
  `follow = true` (`session.rs:683` in `apply`, `session.rs:738` after
  backfill, `session.rs:532,607,1114` elsewhere). The next frame does the
  tail check inline in render (`session.rs:1771-1776`: compare
  `-offset.y >= max_offset.y - TAIL_SLACK`, then `scroll_to_bottom()`).
- `SessionView::apply` ends with an **unconditional** `cx.notify()`
  (`session.rs:691`) — every wire event, including each `item/delta`
  streaming chunk, re-renders the whole transcript. Deltas map 1:1 to
  `Delta::{Text,Thinking,ToolOutput}Delta` (`fold.rs:898-912`).
- Every assistant block is additionally wrapped per-frame in a
  `stream_reveal` measuring div (`transcript.rs:297-299`), and every
  assistant/user text cell re-runs the markdown parser: `prose()` calls
  `parse()` on the full text (`aui/transcript/prose.rs:170-172,120-135`) —
  pure function, no memoization — then rebuilds all `TextRun`s
  (`prose.rs:137-156`) and a `StyledText` element per paragraph
  (`prose.rs:180`). So each streamed token re-parses and re-shapes **all**
  turns, not just the streaming one.

Measurement. The fixtures cannot show this: the largest capture is 62 lines
(`transcript-wire.jsonl`), folding to ~7 item events / a handful of blocks
(counted with python over `fixtures/msp/*.jsonl`; all captures are 10–62
lines, 1–2 turns). Jank is a live-scale phenomenon (hundreds of turns × full
re-layout per delta) and no replay capture exercises it — worth stating
because any "fix verified by replay screenshot" proves nothing about scroll.

What gpui offers (`G:elements/list.rs`, `G:elements/uniform_list.rs`):
`ListState` (`list.rs:54`), `ListAlignment::Bottom` (`list.rs:164-168`),
`ListState::splice(old_range, count)` (`list.rs:503`),
`scroll_to_reveal_item(ix)` (`list.rs:677`), `ListOffset` (`list.rs:1433`),
and `uniform_list` (`uniform_list.rs:22`, `UniformList` at `:58`,
`UniformListScrollHandle` at `:80`). Bottom-aligned lists stay pinned to the
tail by the element itself instead of the app's per-frame offset arithmetic.

Library inventory. `aui` has **no** virtualized transcript container:
`transcript/mod.rs` exports cards/rows only (`activity`, `thinking`,
`tool_card`, `turns`, … — no list/timeline module). `transcript_card`
(`aui/transcript/card.rs`) is a per-card header/body shell. Fix belongs in
**both**: harness must stop building all rows per frame; library needs a
virtualized transcript list (or documented recipe) plus memoized prose cells,
since every future consumer hits the same wall.

Proposed fix (M/L, risky part is variable row heights):

- In `SessionView::render_transcript` (`session.rs:1766`) replace the
  `overflow_y_scroll` div with `uniform_list` keyed by `block_key(turn,idx)`
  (`transcript.rs:185-187`) if rows can commit to fixed heights, else gpui
  `list()` with a persistent `ListState` (`ListAlignment::Bottom`,
  `list.rs:164-168`) stored on the view next to `scroll` (`session.rs:216`);
  on `changed` deltas call `splice` (`list.rs:503`) for the affected range and
  `scroll_to_reveal_item` (`list.rs:677`) only when the reader was at the
  tail (keep the `TAIL_SLACK` test, `session.rs:94`). Keep `ScrollHandle`
  only if `uniform_list`'s `UniformListScrollHandle` (`uniform_list.rs:80`)
  cannot carry the tail test.
- Throttle re-parse: memoize `prose()` output per (text, style) in aui (new
  helper beside `last_paragraph_runs`, `prose.rs:162`), and in `apply`
  (`session.rs:631`) only set `follow`/notify the streaming row instead of
  rebuilding settled turns (settled turns never change except via
  backfill/approval updates — D11).
- Test: scripted run appending N synthetic turns is not possible by replay
  alone (captures are tiny), so verify with a stress capture or a live
  long session; screenshot proves layout only, not smoothness — record
  frame time from gpui's own perf counters instead. Screenshot
  `shots/transcript-<scroll|tail>-*.png` for layout parity.
- Risks: variable-height rows + `uniform_list` need measured heights;
  `list()` requires stable indices across `splice` — the D6 sequence-order
  rule (`docs/07-architecture.md` §4) means indices shift on backfill, which
  must go through `splice`, not rebuild.

Open questions: none on cause; the performance target ("native-smooth at N
turns") needs an owner number to test against.

## C2. Flicker on session switch — "there is a flicker … where the empty session … is shown"

Symptom (owner): "When we change from one session to another — there is a
flicker in between where the empty session (new session screen — with three
options) is shown."

Root cause. `Harness::open` (`app.rs:956-979`) constructs a **fresh,
empty** `SessionView` (`SessionView::new`, `fold` empty), installs it as
`self.active` immediately (`app.rs:976`), and only then starts the async
`backfill` (`session.rs:724-741`, `view/page` over background_spawn). The
very next frame `render_transcript` finds `fold.session(&id)` is `None`
(`session.rs:1793-1795`) or `turns.is_empty()` (`session.rs:1797-1799`) and
renders `empty_state` (`transcript.rs:633-660`: "New session" + workspace
line + the three `SUGGESTIONS` chips, `transcript.rs:674-678`) — i.e. the
new-session screen — until the first page lands, flips `loading_history`
(`session.rs:736-738`) and notifies. Every switch therefore paints ≥1 frame
of the empty state even when the target session has history. Note
`loading_history` exists (`session.rs:224`) but the render path never
consults it; it only gates nothing visible.

Library inventory: nothing needed from aui (placeholder/spinner rows already
exist: `status_row`, `thinking` shimmer). Fix belongs in the **harness**.

Proposed fix (S):

- In `Harness::open` (`app.rs:956`) / `resume` (`app.rs:922`): do not swap
  `self.active` synchronously. Keep rendering the old view and install the
  new `SessionView` when its first backfill batch applies (the
  `this.fold.apply(event)` loop at `session.rs:734-735` — swap on first
  non-empty fold, or on `loading_history: false` at `session.rs:736`).
- Until then, if the old view must go (e.g. hidden session), render a neutral
  placeholder (spinner `status_row`, not `empty_state`): branch
  `render_transcript` on `loading_history` (`session.rs:224`) before the
  `turns.is_empty()` check (`session.rs:1797`) so "no turns *yet*" and "no
  turns *at all*" are different states.
- Test: switch between two backfilled replays via `--steps`, screenshot
  mid-switch; prove no frame contains "New session". Screenshot
  `shots/transcript-switch-*.png`.
- Risks: low; the deferred swap must still handle backfill failure (keep old
  view + error banner — `resume` already clears `active` on error at
  `app.rs:941-947`, preserve that).

## C3. Text selection — "Text inside the transcript cannot be selected … rendered like an image"

Symptom (owner): "Text inside the transcript cannot be selected
(highlighted), it is rendered like an image."

Root cause. gpui-pre 0.3.3 offers **no selectable-text element**. Its text
elements are `StyledText` (static runs, `G:elements/text.rs:391-399`) and
`InteractiveText` (`text.rs:981-985`: click/hover/tooltip over char
**ranges** via `on_click(Vec<Range<usize>>)` at `text.rs:1017-1035` — press
and release must land in the same range; there is drag-select, no selection
model, no clipboard). Grep for `selectable|text_selection|Selectable` in
`G:elements/text.rs` returns nothing. Every aui transcript cell builds on
`StyledText::with_runs` (`prose.rs:144,180,191`, `syntax`, `ansi_runs`,
`tool_card.rs:305-313` search hits, web results) — none wires
`InteractiveText` at all. Zed comparison: not determined (no Zed sources on
this machine); from memory Zed renders messages through its editor/buffer
machinery which carries selection for free — **not verified**, do not rely
on it.

Library inventory: selection primitive is **absent** from aui. This is the
largest library gap in the section. Fix belongs in the **library** (new
selectable-prose element, likely wrapping `InteractiveText` + a
selection/drag model + copy intent), harness only wires the copy intent to
the clipboard (`cx.read_from_clipboard`/`write_to_clipboard` pattern already
used at `session.rs:1486`).

Proposed fix (L):

- New aui component (e.g. `transcript/selectable.rs`): `InteractiveText`-backed
  (`text.rs:1006-1017`) paragraph renderer emitting the same runs as
  `prose()` (`prose.rs:137-156`), tracking press→drag→release into a
  `Range<usize>` with platform selection painting, plus a Copy intent out
  (component stays stateless per library rules — selection state lives in
  the app view, cf. D12 `docs/09-handoff-improvements.md` §4).
- Migrate `prose()` call sites (`turns.rs` assistant/user, tool bodies) to
  it one by one; keep `StyledText` for chrome (headers, footers).
- Test: drag-select across a paragraph + Copy in a replay run; screenshot
  `shots/transcript-select-*.png` showing the highlight. Risks: gpui hitbox
  granularity for per-glyph ranges; multi-cell (cross-paragraph) selection
  needs a view-level range, not per-cell state.

## C4. Markdown — "Markdown is not rendered properly … should have rich support"

Symptom (owner): "Markdown is not rendered properly — it should have rich
support for rendering tables, images, headings, bullets, syntax-highlighted
code blocks etc." (image1 in `docs/diagnosis/inputs/` shows raw `##`/`-`.)

Root cause. Nothing parses real markdown. `aui/transcript/prose.rs:70-135`
is a hand subset: `Block` is only `Paragraph | List` (`prose.rs:72-75`),
inline is only `` `code` `` + `**bold**` (`prose.rs:77-118`), lists are only
chunks where **every** line starts with `- ` (`prose.rs:128`), soft wraps
join with a space (`prose.rs:131`). No headings, no `*`/`1.` lists, no
fenced code blocks, no tables, no quotes, no images, no links. And the turn
components can only call that: `AssistantTurn::render` embeds
`prose((id, "text"), &self.markdown, style)` directly (`aui/transcript/
turns.rs`, caret/prose block) — a fenced block or `##` has nowhere else to
go, so it paints literally (image1). Dependency check: no
`pulldown`/`comrak` in either repo or workspace `Cargo.toml`; `Cargo.lock`
contains a `markdown` 1.0.0 commonmark package but nothing in the workspace
depends on it (only referenced by registry kits). `syntect` is absent;
tree-sitter exists only behind aui's optional `tree-sitter` feature
(`aui/Cargo.toml:19-28` → gpui-kit grammars rust/typescript/tsx/python/bash)
which the harness does **not** enable (workspace `Cargo.toml:34` is a bare
path dep, no `features`). The small built-in lexer (`syntax.rs:120-153`,
`tokenize_line`) is what plain `code_block` (`code.rs:96`) gets.

Library inventory. Exists: `prose` (subset), `code_block`/`diff_block`
(`code.rs:96` +), `syntax_runs`/`tokenize_line` (small lexer), `ansi_runs`
(shell output). Missing: block-level markdown (headings, tables, fences,
lists beyond `-`, quotes, images, links). Fix belongs in **both**: library
gains a real markdown block renderer (block parser → existing `prose` for
inline + `code_block` with `syntax_runs` for fences + new table/heading
rows); harness enables `aui/tree-sitter` (or wires `syntax_runs_in`) so
fences highlight.

Proposed fix (M/L):

- In aui, add `transcript/markdown.rs`: line/block pass producing
  `Paragraph | Heading(u8) | List | CodeBlock{lang, text} | Table | Quote`
  from the same source `AssistantTurn`/`UserTurn` hold, reusing `prose`
  runs, `code_block` (`code.rs:96`), `syntax_runs` (`syntax.rs`), with a
  gallery entry (library gates require one).
- In harness `Cargo.toml:34` / workspace `Cargo.toml`, enable the aui
  `tree-sitter` feature; confirm `cargo tree -d` still shows one gpui-pre /
  one gpui-kit (CLAUDE.md gate).
- Test: replay a capture containing `##`, fences, a table (none of the
  current captures do — author one `synthetic-markdown.jsonl` or reuse a
  live transcript); screenshot `shots/transcript-markdown-*.png` vs image1.
  Risks: streaming re-parse of fences mid-chunk (unclosed fence must render
  as code-in-progress, not raw); table layout in a flex column.

## C5. Links — "Links cannot be clicked … website … Finder … docs/x/y.md"

Symptom (owner): "Links cannot be clicked. Any link to a website etc should
open in the native browser … a link to any file should open Finder … any
text that has locations like docs/xysdf/abc.md should actually be linked."

Root cause. `prose`'s `Span` (`prose.rs:63-68`) has no link variant and
`parse_inline` (`prose.rs:77-118`) recognises no `[t](u)` / bare-URL /
path shapes; cells render `StyledText`, never `InteractiveText`, so no
click ranges exist. Turn actions are only
Copy/Retry/Fork/Pin + Edit/Copy/Resend (`aui/transcript/turns.rs:52-69`) and
the harness does not even wire those (zero `on_action` calls in
`crates/harness/src/`). gpui side is ready: `App::open_url(&str)`
(`G:app.rs:1500`, i.e. `cx.open_url(..)` — default browser) and
`App::reveal_path(&Path)` (`G:app.rs:1604`, Finder reveal) plus
`open_with_system` (`G:app.rs:1608`); both live on `impl App`
(`G:app.rs:778`). Nothing detects repo-relative paths today.

Library inventory: link spans + click intent are **absent** from aui
(`InteractiveText::on_click(ranges)` at `G:elements/text.rs:1017` is the
mechanism, unused by transcript cells). Fix in **both**: library parses
link spans and emits intents; harness resolves + opens.

Proposed fix (M):

- aui `prose`: add `Span::Link{label, target: LinkTarget::Url|Path}`,
  detect `[t](u)`, bare `https?://…`, and backtick-or-bare `\S+\.md`
  (+ a few more extensions) path shapes; render underline + accent run and
  expose `Vec<Range<usize>>` for `InteractiveText::on_click`
  (`text.rs:1017-1035`); link click becomes a new turn intent out (data in /
  intents out per library rules).
- Harness: on intent, `cx.open_url(url)` for URLs; for paths resolve
  against the session workspace (`session.rs` owns `workspace`,
  `session.rs:208`) and `cx.reveal_path(&abs)` (Finder) — check existence
  first, fall back to `open_with_system` for files vs reveal for dirs.
- Test: message containing a URL + `docs/x/y.md` in replay; click each;
  screenshot `shots/transcript-links-*.png`. Risks: false-positive
  path-linking inside code spans (skip spans already typed `Code`);
  workspace-escape (`../`) must be rejected or confirmed.

## C6. Message actions — "Chat bubble actions are misplaced … bottom of each chat message"

Symptom (owner): "Chat bubble actions are misplaced. It should be shown at
the bottom of each chat message — for both agent and user messages."

Root cause. Two compounding facts: (1) the harness never wires actions —
`assistant_turn`/`user_turn` are built in `transcript.rs:281-307` with no
`.on_action`, so the toolbar buttons (which only attach `on_click` when a
handler exists, `aui/transcript/turns.rs:300-302`) are dead; (2) the aui
design itself puts them on hover **above** the turn: `AssistantTurn::render`
builds an `absolute().top(TOOLBAR_TOP + rise)` toolbar with
copy/retry/fork/pin (`turns.rs:285-307`, opacity-tweened on hover), and the
gallery reference states it explicitly —
`aui-gallery/src/cards/turns.rs:44-46` note: "The toolbar appears on hover
above the turn and never shifts layout." The owner wants a persistent bottom
row instead, for both roles — a design change, not a wiring bug. (`session.rs`
also never reads `AssistantTurnAction`/`UserTurnAction`.)

Library inventory: `UserTurnAction{Edit,Copy,Resend}` / `AssistantTurnAction
{Copy,Retry,Fork,Pin}` (`turns.rs:52-69`) exist as intents; a bottom action
row component does **not**. Fix in **both**: library adds the row variant
(design-token spacing, both themes, gallery entry); harness wires handlers.

Proposed fix (M):

- aui `turns.rs`: add e.g. `.actions_bottom()` (or a gallery-agreed variant)
  rendering the four/three glyph buttons in-flow under the prose + footer
  (visible affordance, no hover needed on touch); keep hover toolbar until
  the owner signs off on removal.
- Harness `transcript.rs:block` Text arms (`transcript.rs:333-344`) and user
  arm (`:281-286`): `.on_action` → copy writes text via clipboard, retry
  re-sends turn input where `retryable_turns` allows, fork/pin map to
  existing `ForkPicker`/sidebar intents.
- Test: hover + no-hover screenshots of both roles in replay
  (`shots/transcript-actions-*.png`). Risks: layout shift of the footer row
  (`silent_footer_row`, `transcript.rs:248`); in-flow row changes turn
  heights (matters for C1 virtualization — same height source).

Open questions: should the row be always-visible or hover-reveal (owner says
"shown at the bottom" — read as visible)? Should retry/fork/pin apply to
user turns too, or copy-only?

## C7. reminderChild — "There is one card called reminderChild that keeps showing up"

Symptom (owner): "There is one card called reminderChild that keeps showing
up again and again. Can we get rid of it." (image2.)

Root cause. `reminderChild` is a real MSP item kind
(`ItemKind::ReminderChild = "reminderChild"`, `schema.rs:1331`): a
Muse-internal child-session record carrying `childSessionId`
(`schema.rs:1110`), `childSessionLogPath` (`schema.rs:1114`),
`generationId` (`:1160`), `reminderAgentId` (`:1193`), `taskId` (`:1235`).
The fold has no card for it: `block_for` falls through to the mandated
generic rendering (`fold.rs:877-885`: `Block::Generic{kind, status,
fallbackText}`), which `generic_item_card` paints with the literal kind
name — hence a card literally titled "reminderChild". It repeats because the
server re-emits one per reminder generation. **No checked-in capture
contains one**: `grep -l reminderChild fixtures/msp/*.jsonl` matches nothing
(exit 1), so the owner only sees it live; the fixture gate
(`muse-adapter/tests/fixtures.rs:99-113`) explicitly allows `workflow` and
`reminderChild` as the two generic-fallback kinds, and `fold.rs`'s module
doc (`muse-adapter/src/lib.rs:32`) confirms only those two reach it in
1.0.3. Sibling internal items get the same treatment: `workflow` → identical
`Block::Generic` path; `subagent` at least gets a `ToolCall/SubAgent` card
(`fold.rs:861-873`), `compaction` a marker (`:874-876`).

Library inventory: `generic_item_card` (`aui/transcript/item.rs`, via
`transcript/mod.rs`) exists and is doing its mandated job (01-transport:
unknown kinds MUST render generically). Nothing missing in aui. Fix belongs
in the **harness fold** (filter/collapse policy), not the library.

Proposed fix (S):

- In `fold.rs:block_for` (`fold.rs:828`) or a fold option: skip
  `ItemKind::ReminderChild` (emit no block/delta) — or collapse all of a
  turn's reminder children into one muted `MarkerKind`-style row if the
  owner wants traceability. Same decision needed for `workflow` (owner did
  not complain; leave it generic).
- Test: author `synthetic-reminderchild.jsonl` (none exists — that is why
  fixtures stay green while live shows the card) and assert it folds to
  zero/​one muted blocks; screenshot `shots/transcript-reminder-*.png`.
- Risks: hiding server-emitted items breaks the "session read twice says
  the same" parity expectation if any other client counts them — acceptable,
  it is presentational. Do not invent drill-in (childSessionId → session/read
  exists per `schema.rs:1110` but is out of scope).

Open questions: drop entirely, or keep one collapsed "N reminders" row? (If
the reminder text is ever user-relevant, hiding loses it — check one live
`fallbackText` first.)

## C8. Grouping consecutive tool calls — "join all of them together … like below"

Symptom (owner): "When multiple tool calls are happening one after the other
— is there a way to join all of them together and show them visually like
below. The below screenshot shows a todo. Can we have a similar layout but
adapted for tool calls?" (image3: "Searched web … 5 results" collapsed card,
two rows + "+3 more".)

Root cause. Consecutive `toolCall` items fold to **sibling** `Block::ToolCall`
cards (`fold.rs:841-849`, one per item) and `transcript.rs:364-407` renders
each as its own `tool_card` — no grouping pass exists in fold or app. Taken
for this report (`shots/transcript-wire-dark.png`): two `$` shell cards and
two "Denied by policy" cards stacked separately — the ungrouped today-state.
The library *does* own the visual language the owner points at: per-card
result collapsing already exists — `ToolBody::Web` header shows "N results"
(`tool_card.rs:183-186`), `web_body` lists rows then "+{hidden} more"
(`tool_card.rs:330`); shell shows "N more lines" (`tool_card.rs:269`,
`SHELL_FOLD` at `:23`); diffs "+{n} more hunks" (`:295`) — and image3 is
almost certainly that Web-body collapsed style, not a multi-call group. For
true cross-call grouping aui has `activity_group` (Card 33,
`activity.rs:51`): summary header + step-glyph strip + elapsed + collapsible
timeline (`activity.rs:94+`, header built at `:99-125`). **But the fold never
emits `Block::Activity`**: zero producers in `fold.rs` (only
`aui-protocol/sample.rs:503` constructs one), so the `Block::Activity` arm in
`transcript.rs:358-363` is dead code on live data and consecutive tool calls
can never group today. There is no `ToolGroup` component — `activity_group`
is the nearest.

Library inventory: `activity_group` + per-body "+k more" collapsing exist
(partial); a tool-call group card (header title + count + chevron, N tool
rows, "+k more") is **absent**. Fix in **both**: fold/app groups, library
renders.

Proposed fix (M):

- Grouping (harness, in `fold.rs` beside the D6 ordering or in
  `transcript.rs:turn` before render): fold runs of adjacent `ToolCall`
  blocks in one assistant turn into one `Block::Activity{steps, summary,
  elapsed, state}` (summary e.g. "Ran 3 commands"/verb-derived, steps from
  each call's verb+target+status). Keep approvals/questions/errors/plan/todo
  ungrouped (they are decision points, D10).
- Card (library): either reuse `activity_group` (timeline rows gain per-tool
  tap → expand that call's body) or add a `ToolGroup` sibling in
  `aui/transcript/` with the image3 header idiom
  (`transcript_card(id, open)` + chevron, cf. `card.rs`) and a gallery entry;
  bodies reuse `tool_card` body painters (`shell_body`, `web_body`).
- Test: replay with ≥3 consecutive tool calls (author synthetic — current
  captures interleave approvals); screenshot collapsed + expanded
  (`shots/transcript-toolgroup-*.png`) against image3. Risks: streaming
  group membership churn (a group that keeps gaining rows while open);
  toggle-key stability (`block_key`, `transcript.rs:185`) across regroups;
  approval-gated calls must break the group (D11 liveness).

## C9. Reasoning — "Is there a way to show the reasoning/thinking?"

Symptom (owner): "Is there a way to show the reasoning/thinking?"

Root cause — the full path already exists, with one lossy joint:

- Wire: `ItemKind::Reasoning` (`schema.rs:1331`); fields `summary:
  Vec<String>` — "one entry per summary part; part *n* streams via
  `item/delta` field `summary.n`" (`schema.rs:1253-1257`) — plus raw
  `text` ("raw committed reasoning text where the provider exposes it",
  `schema.rs:1241`) and `providerItemId` (`:1203`). Token counts ride
  usage (`fold.rs:492-493` → `TurnMeta.reasoning_tokens`, `fold.rs:635`).
- Fold: `Reasoning` → `Block::Thinking{text: summary.join("\n\n"),
  summary: first part, state: Thinking/Done by terminal}` (`fold.rs:834-840`);
  streaming via `ThinkingDelta` (`fold.rs:903-905`). **The raw `text` field
  is dropped** — only `summary` is rendered. A turn can also bill reasoning
  tokens with zero reasoning items; that case is covered since 2026-09-09 by
  the silent-reasoning footer (`transcript.rs:197-267`,
  `silent_reasoning_text`, `docs/CHANGELOG.md:26-45`, improvement candidate 3
  in `docs/09-handoff-improvements.md` §8).
- Library: `thinking_block` (Card 32, `aui/transcript/thinking.rs:37-43`):
  shimmer label + elapsed while running, capped 96 px viewport with top fade
  (`VIEWPORT_MAX`, `FADE_H` at `thinking.rs:22-28`), collapses to the
  one-line `summary` when done, click re-expands (`expanded`/`on_toggle`).
  `transcript.rs:346-357` wires exactly that (live trace open, finished
  collapsed unless hand-toggled via `Folds::open`, `transcript.rs:179-182`).
  Gallery reference: `aui-gallery` thinking entry (Card 32).

So "a way to show" exists and is wired; if the owner sees no thinking, it is
because the provider exposed no `summary` parts on those turns (then only
the "thought silently" footer shows), or the finished trace collapsed to one
line and was missed.

Library inventory: thinking/reasoning cell exists, complete. Remaining gap
is the dropped raw `text` (fold, harness side). Fix in **harness**
(adapter) + optionally library (long-trace viewport already handles length).

Proposed fix (S):

- In `fold.rs:834-840`, prefer `text` when `summary` is empty (or append it):
  `text: item.summary…join(…).or(item.text)`-style fallback so exposed raw
  reasoning is never silently dropped; keep `summary: first part` for the
  collapsed line.
- No render change needed; verify against a capture with a `reasoning` item
  carrying `text` but no `summary` (none checked in — author synthetic).
  Screenshot collapsed + expanded (`shots/transcript-thinking-*.png`).
- Risks: raw text can be long/verbatim chain-of-thought the provider only
  exposed incidentally — confirm with the owner before surfacing by default;
  the collapsed-by-default posture (`transcript.rs:351`) already mitigates.

Open questions: show raw reasoning expanded or collapsed by default when a
summary line also exists?

## Dependencies between items

- C1 (virtualization) needs C4's block inventory (variable heights come from
  markdown/table/code rows) and C6's action-row heights — do C4+C6 cell
  design before committing to `uniform_list` vs `list()`.
- C4 (markdown) is the prerequisite for C5 (links): link spans ride the new
  inline parser; do not build link detection on the current subset parser.
- C3 (selection) should be built on C4's new cells (selectable prose) and
  composes with C5's clickable ranges (selection drag vs link click
  disambiguation) — plan the three as one library work package with three
  gallery entries.
- C8 (grouping) interacts with C1 (group ↔ row identity for `splice`) and
  C6 (per-tool actions inside a group).
- C2 is independent (harness-only, S) — do first.
- C7 is independent (fold filter, S) — do any time; needs the owner's
  drop-vs-collapse answer.
- C9 is independent (one-line fold fallback, S).

## What I ran (verbatim)

- `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"; pwd; ls`
- `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"; ls crates/harness/src/; ls fixtures/msp/ | head -50; wc -l fixtures/msp/*.jsonl`
- `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"; ls /Users/latekaapi/Projects/agentic-ui/crates/; ls /Users/latekaapi/Projects/agentic-ui/crates/aui/src/ 2>/dev/null | head -60; ls /Users/latekaapi/Projects/agentic-ui/crates/aui-gallery/src/ 2>/dev/null | head -40`
- `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"; grep -n "scroll\|follow\|ScrollHandle\|overflow\|empty\|Empty\|uniform_list\|list(" crates/harness/src/session.rs | head -60`
- `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"; sed -n '1750,1900p' crates/harness/src/session.rs`
- `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"; grep -n "cx.notify\|apply\|backfill\|loading_history\|open_session\|active_session\|SessionView::new\|switch\|resume\|excludeItems\|view/page" crates/harness/src/session.rs | head -50; echo ---APP---; grep -n "open_session\|active_session\|SessionView\|empty\|session_id\|switch\|resume\|backfill\|loading" crates/harness/src/app.rs | head -80`
- `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"; sed -n '921,1000p' crates/harness/src/app.rs; echo ===; sed -n '620,750p' crates/harness/src/session.rs`
- `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"; grep -rn "reminderChild\|reminder_child\|systemReminder\|system_reminder" crates/ fixtures/msp/*.jsonl docs/ 2>/dev/null | head -30; echo ===; grep -n "pulldown\|comrak\|markdown\|Markdown\|md_text\|rich_text\|StyledText\|InteractiveText\|selectable\|text_selection\|open_url\|reveal_path\|cx.open" crates/harness/src/*.rs | head -40`
- `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"; sed -n '1090,1360p' crates/muse-client/src/schema.rs`
- `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"; sed -n '850,960p' crates/muse-adapter/src/fold.rs; echo ===FIXTURES===; sed -n '85,130p' crates/muse-adapter/tests/fixtures.rs; echo ===LIBDOC===; sed -n '1,60p' crates/muse-adapter/src/lib.rs`
- `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"; grep -l "reminderChild" fixtures/msp/*.jsonl; echo "exit=$?"; echo ===; grep -n "Reasoning\|reasoning\|Thinking\|thought" crates/muse-adapter/src/fold.rs | head -30; echo ===; grep -rn "thought-silently\|thought silently\|silent" docs/CHANGELOG.md docs/10-msp-1.1.1-diff.md 2>/dev/null | head; ls docs/`
- `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"; sed -n '820,850p' crates/muse-adapter/src/fold.rs; echo ===; grep -n "reasoning\|Reasoning\|thought\|summary" docs/10-msp-1.1.1-diff.md | head -30; echo ===; grep -n "pulldown\|comrak\|markdown\|Markdown" Cargo.lock crates/*/Cargo.toml /Users/latekaapi/Projects/agentic-ui/crates/*/Cargo.toml 2>/dev/null | head -20`
- `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"; grep -n -A3 -B3 'name = "markdown"' Cargo.lock; echo ===; grep -rn "markdown" crates/harness/src/ crates/muse-adapter/src/ crates/muse-client/src/ | head -20; echo ===AUI===; grep -rln "markdown\|Markdown\|pulldown\|comrak" /Users/latekaapi/Projects/agentic-ui/crates/aui/src/ | head -20`
- `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"; grep -rn "markdown" /Users/latekaapi/Projects/agentic-ui/crates/aui/Cargo.toml; echo ===; ls /Users/latekaapi/Projects/agentic-ui/crates/aui/src/transcript/; echo ===; wc -l /Users/latekaapi/Projects/agentic-ui/crates/aui/src/transcript/prose.rs /Users/latekaapi/Projects/agentic-ui/crates/aui/src/transcript/turns.rs`
- `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"; cat /Users/latekaapi/Projects/agentic-ui/crates/aui/Cargo.toml | head -60; echo ===; grep -rn "syntect\|tree-sitter\|tree_sitter\|pulldown\|comrak" /Users/latekaapi/Projects/agentic-ui/crates/aui/Cargo.toml /Users/latekaapi/Projects/agentic-ui/Cargo.toml /Users/latekaapi/Projects/agentic-ui/crates/aui/src/transcript/syntax.rs /Users/latekaapi/Projects/agentic-ui/crates/aui/src/transcript/code.rs 2>/dev/null | head -20`
- `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"; grep -n "assistant_turn\|copy\|Copy\|action\|Action\|link\|Link\|url\|Url\|open_url\|reveal" /Users/latekaapi/Projects/agentic-ui/crates/aui/src/transcript/turns.rs | head -40; echo ===MOD===; cat /Users/latekaapi/Projects/agentic-ui/crates/aui/src/transcript/mod.rs`
- `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"; sed -n '270,381p' /Users/latekaapi/Projects/agentic-ui/crates/aui/src/transcript/turns.rs`
- `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"; grep -n "on_action\|AssistantTurnAction\|UserTurnAction\|needs_you\|copy\|Copy\|clipboard" crates/harness/src/transcript.rs crates/harness/src/session.rs | head -20; echo ===; grep -n "activity_group\|ActivityGroup\|ToolGroup\|tool_group\|collapsed\|chevron\|more" /Users/latekaapi/Projects/agentic-ui/crates/aui/src/transcript/activity.rs | head -30; echo ===; ls /Users/latekaapi/Projects/agentic-ui/crates/aui-gallery/src/cards/ 2>/dev/null; ls /Users/latekaapi/Projects/agentic-ui/crates/aui-gallery/src/`
- `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"; sed -n '1,140p' /Users/latekaapi/Projects/agentic-ui/crates/aui/src/transcript/activity.rs; echo ===GALLERY-ASSISTANT===; grep -n "action\|bottom\|toolbar\|hover\|copy" /Users/latekaapi/Projects/agentic-ui/crates/aui-gallery/src/cards/assistant.rs | head -30`
- `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"; grep -rn "assistant_turn\|AssistantTurn\|on_action" /Users/latekaapi/Projects/agentic-ui/crates/aui-gallery/src/cards/assistant.rs /Users/latekaapi/Projects/agentic-ui/crates/aui-gallery/src/turns.rs 2>/dev/null | head; echo ===; grep -rn "assistant_turn" /Users/latekaapi/Projects/agentic-ui/crates/aui-gallery/src/ | head; echo ===TOOLGROUP===; grep -rn "ToolGroup\|tool_group\|group" /Users/latekaapi/Projects/agentic-ui/crates/aui/src/transcript/*.rs | head -20`
- `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"; sed -n '1,80p' /Users/latekaapi/Projects/agentic-ui/crates/aui-gallery/src/cards/turns.rs; echo ===THINKING===; sed -n '1,60p' /Users/latekaapi/Projects/agentic-ui/crates/aui/src/transcript/thinking.rs | head -70`
- `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"; ls ~/.cargo/registry/src/*/ 2>/dev/null | grep -i "gpui-pre\|gpui-kit" | head; echo ===; find ~/.cargo/registry/src -maxdepth 2 -name 'gpui-pre-*' 2>/dev/null | head`
- `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"; G=/Users/latekaapi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/gpui-pre-0.3.3; ls $G/src/elements/ | head -30; echo ===; grep -n "pub struct ListState\|pub enum ListAlignment\|pub struct ListOffset\|pub fn scroll_to_reveal_item\|pub fn splice\|Bottom" $G/src/elements/list.rs | head -20`
- `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"; G=/Users/latekaapi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/gpui-pre-0.3.3; grep -rn "selectable\|InteractiveText\|text_selection\|Selectable" $G/src/elements/text.rs $G/src/elements/*.rs 2>/dev/null | head -20; echo ===OPEN_URL===; grep -rn "pub fn open_url\|pub fn reveal_path\|open_url" $G/src/app.rs $G/src/*.rs 2>/dev/null | head -10; echo ===UNIFORM===; grep -n "pub fn uniform_list\|pub struct UniformList" $G/src/elements/uniform_list.rs | head`
- `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"; G=/Users/latekaapi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/gpui-pre-0.3.3; sed -n '975,1060p' $G/src/elements/text.rs; echo ===; grep -rn "StyledText" $G/src/elements/text.rs | head -5; echo ===; grep -rn "selectable\|selection" $G/src/elements/text.rs | head -10`
- `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"; echo ===CAPTURE-CELLS===; python3 -c "<cell-count script over fixtures/msp/*.jsonl>"; echo ===CODEBLOCK===; grep -n "code_block\|CodeBlock\|diff_block\|prose(" crates/harness/src/transcript.rs | head; echo ===TREE-SITTER-FEATURE===; grep -n "aui" crates/harness/Cargo.toml`
- `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"; grep -n "aui" Cargo.toml | head; echo ===; grep -rn "aui.*features\|tree-sitter" Cargo.toml crates/harness/Cargo.toml /Users/latekaapi/Projects/agentic-ui/Cargo.toml 2>/dev/null | head; echo ===KITDIRS===; ls /Users/latekaapi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/ | grep gpui-kit; echo ===LINKS===; grep -rn "link\|Link\|href\|Href" /Users/latekaapi/Projects/agentic-ui/crates/aui/src/transcript/prose.rs /Users/latekaapi/Projects/agentic-ui/crates/aui/src/transcript/turns.rs /Users/latekaapi/Projects/agentic-ui/crates/aui/src/transcript/code.rs | head`
- `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"; grep -n "Activity\|activity" crates/muse-adapter/src/fold.rs | head -20; echo ===; grep -n "empty_state\|pub fn suggestion" crates/harness/src/transcript.rs; echo ===; sed -n '500,620p' crates/harness/src/transcript.rs`
- `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"; grep -rn "Block::Activity" crates/ | head; echo ===; grep -rn "Activity" /Users/latekaapi/Projects/agentic-ui/crates/aui-protocol/src/*.rs | head -15; echo ===; sed -n '620,700p' crates/harness/src/transcript.rs`
- `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"; ls docs/diagnosis/inputs/ docs/diagnosis/ 2>/dev/null; echo ===; grep -n "more lines\|more\b" /Users/latekaapi/Projects/agentic-ui/crates/aui/src/transcript/tool_card.rs | head; echo ===; grep -n "tool_card\|ToolCard" /Users/latekaapi/Projects/agentic-ui/crates/aui-gallery/src/cards/tool_cards.rs | head -20`
- `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"; sed -n '100,200p' /Users/latekaapi/Projects/agentic-ui/crates/aui/src/transcript/tool_card.rs`
- `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"; head -50 /Users/latekaapi/Projects/agentic-ui/docs/00-agent-brief.md; echo ===; grep -n "confirm\|/stateless\|RenderOnce\|intent" /Users/latekaapi/Projects/agentic-ui/docs/00-agent-brief.md | head -10; echo ===; find ~/Projects -maxdepth 3 -type d -iname "*zed*" 2>/dev/null | head -3; find / -maxdepth 4 -type d -name "zed" 2>/dev/null | head -3`
- `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"; mkdir -p docs/diagnosis/shots && ls docs/diagnosis/shots/ && grep -c "" fixtures/msp/transcript-wire.jsonl`
- `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"; cargo run -p harness -- --replay fixtures/msp/transcript-wire.jsonl --theme dark --screenshot docs/diagnosis/shots/transcript-wire-dark.png --screenshot-delay 15000`
- `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"; python3 -c "<agentMessage text scan of transcript-real.jsonl>"`
- `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"; grep -n -B8 '"markdown"' Cargo.lock | grep "name =" | head; echo ===; G=/Users/latekaapi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/gpui-pre-0.3.3; sed -n '1495,1510p' $G/src/app.rs; sed -n '1600,1612p' $G/src/app.rs; echo ===; grep -n "pub fn code_block" /Users/latekaapi/Projects/agentic-ui/crates/aui/src/transcript/code.rs`
- `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"; G=/Users/latekaapi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/gpui-pre-0.3.3; grep -n "^impl " $G/src/app.rs | awk -F: '$1<1500' | tail -3; grep -n "^impl " $G/src/app.rs | awk -F: '$1<1604' | tail -3`

Not run / not determined: Zed message-list and markdown sources (no Zed
checkout on this machine); try-synara/t3.codes (external, not inspected);
frame-time profiling (no stress capture exists); live `reminderChild`
`fallbackText` content (no capture contains one); owner screenshots
image1–5 were read via the brief's descriptions, not pixel-inspected.
Touched files: only `docs/diagnosis/transcript.md` and
`docs/diagnosis/shots/transcript-wire-dark.png`. No source edits, no git
writes, no live turns (all runs `--replay`; spend-rule compliant).
