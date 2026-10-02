# r1002 — transcript noise: factual base for a redesign

Owner, 2026-10-02: *"The transcripts are too noisy. Entire skills are printed as is. There are too many progress
cards, tool cards etc. I should see the progress and understand what's happening without being overloaded."*

Read-only diagnosis on baaz `main` 8bbbc41, agentic-ui v0.3.14 (checkout `b072936`). No source changed and no model turn sent.

Paths:
- `AUI` = `~/.cargo/git/checkouts/agentic-ui-ceaee0e1248ba6d4/b072936/crates/aui/src`
- `PROTO` = `…/crates/aui-protocol/src`
- Baaz paths are relative to `crates/`.

---

## 0. Answers in one screen

| Complaint | Cause (evidence) |
|---|---|
| **Skill / quickstart printed in full** | **(a)** Claude Code's `Artifact` tool (and `ToolSearch`, `Grep`, `Glob`, `WebFetch`, `WebSearch`, …) has no arm in `provider-claude-code/src/fold.rs::tool_card` (2111–2276). It falls to `Block::Generic` (2255–2275). On the result, `finish_tool_block` turns it into `Block::Generic { text: result.text.clone() }` (`fold.rs:347-351`). That is the **whole `tool_result` content**. `aui::generic_item_card` draws it open, with no chevron and no cap (`AUI/transcript/item.rs:56-69`). Baaz passes it through untouched (`baaz/src/transcript.rs:992-994`). Measured: the quickstart result is **121 lines / 26,543 chars**. **(b)** The `Skill` tool itself folds to the quiet "Loaded skill `x`" row (`fold.rs:2169-2186`, `transcript.rs:1040`). But Claude Code then writes the SKILL.md body as a separate `user` line with `isMeta:true, sourceToolUseID` (measured: 309 lines / 16.7 k chars). `frame.rs::decode_user` (595–660) does not look at `isMeta`. Any user line with text and no `tool_result` becomes `Frame::UserText`, and `apply_user_text` (`fold.rs:1244-1300`) makes it a **user bubble**. It also **closes the running assistant turn** (`fold.rs:1275-1279`). Stored-history replay goes through this same decode (`lib.rs:739-758` finds the jsonl), so a reopened session shows the skill as a 309-line "user message". Whether the live stream-json also emits these lines is unverified: no fixture contains `isMeta`. |
| **"Thought for 0.0 s"** | Every lane hard-codes `elapsed_ms: 0`: Muse `muse-adapter/src/fold.rs:1554`, Claude `provider-claude-code/src/fold.rs:1110` and `:1513` (sub-agent), Codex `provider-codex/src/fold.rs:1658`. No delta ever updates it: `PROTO/delta.rs:59` `ThinkingDelta` carries text only. `aui::format_duration(0)` = `"0.0 s"` (`AUI/transcript/tool_card.rs:187`). The header prints `"Thought for {elapsed}"` (`AUI/transcript/thinking.rs:78`). While thinking, it shows a frozen `0.0 s` on the right (`:85-86`). Empty traces are already suppressed (`transcript.rs:1231`, Claude `fold.rs:1107`, Codex `fold.rs:1650`), so this only shows when there is text. In practice that means Muse/Codex reasoning summaries. |
| **Every command is a full card** | Lone `Block::ToolCall` → `tool_call_card` (`transcript.rs:1518`). It is open by default for every body except big edits (`default_open`, `transcript.rs:360-365`). A shell body draws **6 lines** (`SHELL_FOLD`, `tool_card.rs:25`), then "N more lines" + "open in terminal" (`:437-439`). Every shell card also gets a "Run in terminal" header button (`transcript.rs:1566-1573`, `shell_run_command:327`). |
| **Codex / Claude never group** | Grouping exists **only in the Muse fold** (`muse-adapter/src/fold.rs` `try_join` 1276–1320). `provider-claude-code` and `provider-codex` emit one `BlockAdded{ToolCall}` per call and never build `Block::ToolGroup`. Nothing in Baaz groups at render time either (`transcript.rs:962-969` dispatches block by block). |
| **"Read 2 files · 2 calls", "11 tool calls · 11 calls"** | Muse `group_summary` (`fold.rs:2382-2403`) gives "Ran N commands" / "Read N files" / "N tool calls". The library always appends `count_label` "N calls" (`AUI/transcript/tool_group.rs:163`). The count is said twice, and a mixed group says nothing about what happened. |
| **"Approving… `<cmd>` Running"** | `Block::decide_approval` moves a card to `Approving` on the click (`PROTO/block.rs:369-391`). **Muse** only settles `Approving` when the whole **turn** completes `completed` (`muse-adapter/src/fold.rs:948-952`). So the card reads "Approving… Running" for the rest of the turn, long after the command finished, and it stays that way forever if the turn fails or is cancelled. **Claude** settles only on a non-error result (`provider-claude-code/src/fold.rs:1328-1338`). An approved command that exits non-zero leaves the card "Approving…" permanently. When it does settle, every lane writes `duration_ms: 0` (Muse `:951`, Claude `:1336`, Codex `:1718`, `:1784`), so it reads "exit 0 · 0.0 s". The tool card for the same command is drawn separately, so each approved command is **two cards**. |

---

## 1. Inventory: every transcript block and how it renders

The block enum is `PROTO/block.rs:13-260`. The dispatch is `baaz/src/transcript.rs:942-999` (`fn block`). One block is one virtual-list row (`turn_rows`, `transcript.rs:844`). Rows are 8 px apart, with 16 px after a turn.

| Block | Library component (file:line) | Baaz wrapper | Default | Fed by |
|---|---|---|---|---|
| `Text` | `assistant_turn` (`AUI/transcript/turns.rs:619`) | `text_card` (`transcript.rs:1084`) | full markdown; the last block carries the footer | Muse `AgentMessage` (`fold.rs:1537`); Claude `ContentBlock::Text` (`fold.rs:1115`); Codex `agentMessage` (`fold.rs:1608`). **Pre-tool "commentary" lines are drawn exactly like the answer.** Codex `phase:"commentary"\|"final_answer"` and Muse `phase` are on the wire but unused. |
| `Thinking` | `thinking_block` (`AUI/transcript/thinking.rs:42`) | `thinking_card` (`transcript.rs:1221`): `None` when text is empty; expanded while thinking, collapsed to one row when done | live: header + 4-line viewport; done: "Thought for 0.0 s · summary" | Muse `Reasoning` (`fold.rs:1541-1557`, summary = first summary part); Claude thinking (`fold.rs:1108`, summary `None`, always `Done`); Codex `reasoning` (`fold.rs:1650-1663`, summary `None`) |
| `Activity` | `activity_group` (`AUI/transcript/activity.rs:51`): step-glyph strip + timeline | `activity_card` (`transcript.rs:1244`), closed by default | one row | **No lane produces it.** It is only used in tests/sample (`baaz/src/session.rs:2617`). This is the library's existing compact "progress" idiom, and it is unused. |
| `ToolCall` | `tool_card` (`AUI/transcript/tool_card.rs:134`) | `tool_call_card` (`transcript.rs:1518-1641`); a skill load becomes `skill_load_row` (`:1040`, one quiet line) | **open** (`default_open`, `:360`); Edit/Write closed when more than 12 changed lines | all lanes; see the body table below |
| `ToolGroup` | `tool_group` (`AUI/transcript/tool_group.rs:86`) | `tool_group_card` (`transcript.rs:1287-1305`) | **closed** = header + **2 preview rows** + "+k more" (`tool_group.rs:198-230`); open = every call as a full open `tool_card` (`:167-197`, calls default open `transcript.rs:1293`) | **Muse only** |
| `Approval` | `approval_card` (`AUI/transcript/approval.rs`) | `approval_block_card` (`transcript.rs:1307`) | full card; settled states keep a header + mono line (`approval.rs:511-570`) | Muse approvals; Claude `apply_approval` (`fold.rs:1620`); Codex `apply_*_approval` (`fold.rs:1080-1420`) |
| `Question` | `question_card` / `answered_row` | `question_block_card` (`transcript.rs:1364`) | full until answered, then one row | all lanes |
| `Plan` / `Todo` | `plan_card` / `todo_list` | `transcript.rs:972-977` | todo open by default (`folds.open(key,true)`) | Muse todo tool (`fold.rs:1214-1219`); Claude TodoWrite/TaskCreate (`fold.rs:1131`); Codex `turn/plan/updated` (`fold.rs:1518`) |
| `Summary` | `summary_card` | `transcript.rs:978` | full | rare |
| `Error` | `error_card` | `error_block_card` (`:1475`) | full | all lanes |
| `Goal` | `goal_card` (`AUI/transcript/item.rs:89`) | `:987` | full | Muse `goal_changed` |
| `Generic` | `generic_item_card` (`AUI/transcript/item.rs:48-71`): **always open, no chevron, no cap** | `:992` | header (kind + status pill) + **entire text** | Muse unknown item kinds (`fold.rs:1610`, `fallbackText`); **Claude every unmodelled tool, with the full result** (`fold.rs:347`, `:2255`); Codex unknown items (`fold.rs:1827`) and non-terminal MCP calls (`:1869`, error text only) |
| `Marker` / `Handoff` | `marker_row` / handoff card | `:995-996` | one row / card | compaction, retraction, handoff |
| *(silent footer)* | — | `silent_footer_row` (`transcript.rs:739`) | one row | a turn that billed reasoning tokens but has no Thinking block |

### `ToolBody` (card 34) — what each body draws (`AUI/transcript/tool_card.rs:290-372`)

| Body | Header right | Body when open | Cap |
|---|---|---|---|
| `Shell` | pill `live` / `exit 0` / `exit N` (no pill when `exit_code` is None, `:291-301`), duration, then "Run in terminal" (`transcript.rs:1569`) or "Open terminal" | **first** 6 lines (`take(SHELL_FOLD)`, `:423`); while streaming that means the head, not the tail, is shown; then "N more lines · open in terminal" | 6 lines; unfold fetches full output (Muse `outputRef`, `render.rs:867-878`) |
| `Read` | "N lines" | none (chevron only) | — already compact |
| `Edit` | +N −N tags | first hunk, cut to 40 rows (`DIFF_DISPLAY_CAP`, `transcript.rs:348`), then "N more hunks · open in Diff" | starts closed when more than 12 changed lines |
| `Search` | "N hits" | **every hit** (`:469-484`) | none |
| `Web` | "N results" | every result | none |
| `Browser` | action | 70 px placeholder + caption | — |
| `SubAgent` | — | "N turns in the delegated transcript" | — |
| `Mcp` | — | every param pair + **entire `result_json`** (`:563-569`) | **none**: Claude MCP results (`fold.rs:314-321`, e.g. `browser_read` page text) and Muse unknown-tool JSON (`muse-adapter/src/fold.rs` `tool_presentation`) print in full |
| `None` | — | chevron only | — |

### Tool → card mapping per provider

- **Muse** (`muse-adapter/src/fold.rs::tool_shape` 2527–2620; `tool_presentation` 2684–2757):
  - `bash`/`shell`/`exec_command`/`bash_input` → Shell "Ran"
  - `read*` → Read
  - `read_skill` → quiet "Loaded skill" row
  - `write*`/`edit*`/`apply_patch` → Write/Edit
  - `grep`/`glob`/`search` → Search (hits) or a Shell fallback
  - `fetch`/`web_*` → Web
  - terminal tools → "Running/Ran in terminal" shell card with the live mirror
  - anything else → `Mcp{server:"muse"}` (pretty JSON in full) or a Shell body
  - `UserShell` → Shell "$"
  - todo → Todo card
  - `ReminderChild` is dropped
  - Live: item revisions; `ToolOutputDelta` streams into shell output (group members re-fold through `append_group_output`, `:1520`).
- **Claude Code** (`provider-claude-code/src/fold.rs::tool_card` 2111–2276):
  - `Bash` → Shell "Run"→"Ran". The exit code is parsed only from an "Exit code N" first line (`:255-260`), so success has no pill.
  - `Read` → Read
  - `Write`/`Edit` → Edit body from `structuredPatch`
  - `Skill` → quiet row
  - `Agent` → SubAgent
  - `mcp__baaz__terminal_*` → terminal shell card
  - other `mcp__*` → Mcp (full result)
  - TodoWrite/Task* → Todo
  - **everything else** (`Grep`, `Glob`, `ToolSearch`, `Artifact`, `WebFetch`, `WebSearch`, `NotebookEdit`, `BashOutput`, …) → `Generic` "*Name* called (k=v, …)" while running, then the full result text
  - Thinking is always `Done` with elapsed 0.
  - `duration_ms: None` on every card.
- **Codex** (`provider-codex/src/fold.rs::apply_item` 1602–1835):
  - **Only `item/completed` draws.** `item/started` is remembered for fileChange approvals only (`remember_item`, `:1592`), so a Codex command has **no running card**; it appears finished.
  - `commandExecution` → Shell "Ran" with `aggregatedOutput`, `exitCode` and `duration_ms: None` (`:1689-1700`)
  - `fileChange` → Edit with diff_stat
  - `reasoning` → Thinking (summary + content joined, summary `None`)
  - `subAgentActivity` → SubAgent
  - `mcpToolCall` → terminal shell card or `Generic` (error text only)
  - other → `Generic`

---

## 2. Grouping: when it applies and when it does not

The logic is all in `muse-adapter/src/fold.rs`:

- **Who may join.** `is_groupable` (2376): a `ToolCall` whose status is not `Error`. Item-level: there must be no `approval_id` (`:1224`), and it must not be a todo call (`:1214`).
- **What counts as consecutive.** `try_join` (1276) joins only when:
  - the turn's open run is the **last block** (`cursor.block + 1 == blocks.len()`, `:1290`);
  - the run's generation still matches `group_gen`;
  - the call arrives in log order (`seq_allows_join`, `:1323`).
- **What breaks a run.**
  - Any `Text`/`Thinking` block between two calls ends it, because it becomes the last block. Pre-tool commentary like "Let me check X" therefore splits runs.
  - `break_groups()` also ends it, and fires on: a non-groupable item (an error or an approval-gated call), approval requested/updated/resolved (1676, 1693, 1729), question asked/settled (1820, 1845), todo/goal changes (777, 795), and client blocks (381, 392).
  - A gated call is pulled out of its group (`relocate_call`, 1415).
- **Summary text.** `group_summary` (2382): all Shell → "Ran N commands", all Read → "Read N files", otherwise "N tool calls". The library adds "N calls" (`tool_group.rs:141-144, 163`).
- **Claude Code and Codex do not group.** Neither fold has the concept: there is no `ToolGroup` construction outside `muse-adapter` (grep confirms). Baaz's renderer has no view-side grouping pass. So every Claude/Codex call is a lone, open card. That matches the owner's "for Codex every command is a full output card".
- **Even when grouped, it is noisy.**
  - A collapsed group still draws a body (2 preview rows + "+k more").
  - Opening it shows every call as a full open card (calls default open, `transcript.rs:1293`).
  - Groups are per uninterrupted run, so one Muse turn with commentary between calls produces several groups.

---

## 3. Measured noise on real sessions

Three real "Explain how this project is laid out" sessions, one per provider. These are the owner's own sessions, read from disk.

**Where the transcripts live.** `baaz.db` holds no transcripts: only `meta` and `usage_turns`. Titles are in `sessions.json` and the provider is in `provider-sessions.json` under `~/Library/Application Support/baaz/`.

| Session | Provider / file |
|---|---|
| office-samples layout + "create a simple html explainer" | Claude Code: `~/.claude/projects/-Users-latekaapi-Sandbox-office-samples/01a0fbc2-3f23-7bd1-8723-82e6a81942fe.jsonl` |
| "Project Layout Structure Overview" (harness) | **Codex**: `~/.codex/sessions/2026/09/29/rollout-2026-09-29T19-22-07-01a0ed6f-e20a-79c1-b941-ab28117173a2.jsonl` |
| office-samples layout | Muse: `~/.local/share/muse/sessions/2026/10/02/01a0fbbe-9e76-7281-8006-1e24751794f7/session.jsonl` |

How each turn renders today. The rendered estimate uses the current rules: a shell card is header + 6 + fold row, a Generic card is header + all its lines, and a group is header + 2 preview rows.

| Turn | Tool calls | Largest results | Cards drawn today | Rendered rows (est.) | Final answer | Ratio |
|---|---|---|---|---|---|---|
| Claude T1 "explain layout" | Bash ×2 | 30 / 28 lines | 1 commentary text + 2 open shell cards; the empty thinking is dropped | ≈ 17 | 26 lines | 0.7× |
| Claude T2 "html explainer" | Artifact ×2, Bash, Write | **Artifact quickstart 121 lines / 26,543 chars** | Generic "Artifact" (all 121 lines, which wrap well past that), commentary text, shell card, Write card (closed), Generic "Artifact" publish (5 lines) | ≈ 138 source lines, more once wrapped | 10 lines | **≈ 14×** |
| Codex "Project Layout…" | 4 `exec` calls fanning out to 5 commands | 673 / 352 / 252 / 241 / 37 lines | 5 lone open shell cards (each with an exit 0 pill and "Run in terminal") + commentary; no grouping, no durations | ≈ 42 | 54 lines | 0.8× (29× if unfolded) |
| Muse "explain layout" | bash ×2 | 58 / 33 lines | 1 group "Ran 2 commands · 2 calls" + 2 preview rows (or 2 cards if commentary split them); 3 empty thinking rows dropped | ≈ 3–20 | 11 lines | 0.3–1.8× |

Three readings:
- **The answer competes with the work.** On ordinary turns the cards take about as many rows as the answer (0.7–1.8×). The owner's "overloaded" is the cumulative effect: card chrome (pill, button, fold row) on every call, plus commentary lines drawn at answer weight.
- **One Generic card can dominate a turn.** On a turn with one unmodelled Claude tool (Artifact quickstart), the work is **14×** the answer, all from a single card.
- **Reasoning content is almost always empty.**
  - Claude: 2,322 of 2,797 thinking blocks in the last 40 sessions have empty text, and 100% of Baaz-spawned sessions do.
  - Codex: `summary:[]`, encrypted only.
  - Muse: `reasoning_committed.text:""`.
  - So a reasoning row can at most say "Thought for N s", and today N is always 0.

---

## 4. Data available for a compact activity summary

| Datum | Muse (MSP) | Claude Code (stream-json / jsonl) | Codex (app-server) |
|---|---|---|---|
| Tool kind | `item.tool` name → `tool_shape` | `tool_use.name` (+ `input`) | item `type`; **`commandActions[]` (read / listFiles / search / unknown, with `path`, `name`, `query`)**, e.g. `fixtures/codex/read-search.jsonl` (**unused**; frame.rs:537 does not parse it) |
| Target | args `path` / `command` / `pattern` | `input.file_path` / `command` / `pattern` / `skill`; structured `tool_use_result` (Read `file.numLines`, Grep/Glob filenames, ToolSearch `matches`, Artifact `quickstart.types`) | `command` (`display_command` unwraps the shell), `commandActions[].path`, `changes[].path` |
| Exit status | `item.exit_code` / envelope `exit_code`; often absent (bash returned `execution_state:"background_running"`) | `is_error`; code only in an "Exit code N" prefix on failure | `exitCode`, `status` (completed / failed / declined) |
| Duration | `item.duration_ms` (used); `recorded_at` per item | **none on the wire**: derive from frame arrival (live) or jsonl `timestamp` deltas (replay) | **`durationMs` on commandExecution (unused**: the fold sets `duration_ms: None`, `fold.rs:1695`); `startedAtMs` / `completedAtMs` on item notifications; turn `durationMs` |
| Live / running | item revisions with status; `ToolOutputDelta` streaming | tool_use → Running card until tool_result | **not drawn**: `item/started` arrives with `status:"inProgress"` but is ignored for commands |
| Thinking duration | `recorded_at` of the reasoning item vs the next item (live: wall clock between first sight and terminal revision) | jsonl timestamp gaps (≈5 s, 32 s, 28 s in Claude T2); live: arrival clock | `startedAtMs` == `completedAtMs` for reasoning in the measured rollout, so arrival clock or the gap to the next item |
| Commentary vs answer | `phase` field on messages | none (position only: text before a tool_use is commentary) | **`agentMessage.phase: "commentary" \| "final_answer"`** (unused) |
| Skill body | `read_skill` row (D63) | `Skill` result is 1 line; the body is a separate `isMeta:true` user line keyed by `sourceToolUseID` (must be suppressed or attached to the skill row) | n/a |
| Full output on demand | `outputRef` + `item/readOutput` (wired, `full_output.rs`) | the result text is already whole in the block | `aggregatedOutput` whole in the block |

Existing pieces a redesign can reuse:
- `Block::Activity` / `activity_group`: step strip, timeline, collapsed by default. It exists but is unused by every lane.
- The status row: "Running shell…" / "Thinking…" / elapsed, built in `render.rs:1360-1480`.
- `skill_load_row`: the quiet-row precedent.
- Muse's `try_join`: the grouping policy, but it lives inside one provider's fold.

A provider-neutral grouping pass over `Turn.blocks` on the Baaz side would cover all three lanes at once.

---

## 5. Smaller defects found on the way

1. **Thinking elapsed is always 0.** See §0; the fix needs a timestamp source per lane (§4).
2. **The settled approval duration is always 0** (`AllowedOnce{duration_ms:0}`) on all lanes. Muse holds `Approving` until the turn ends. Claude leaves it `Approving` forever when the approved command fails.
3. **Codex commands have no running state.** They appear only on `item/completed`.
4. **Claude `isMeta` user lines become user bubbles** and split the assistant turn in two (`fold.rs:1275`). Certain on stored-history replay; unverified live.
5. **A live shell body shows the first 6 lines, not the last.** `take(SHELL_FOLD)` at `tool_card.rs:423`. Terminal cards mirror the last 6 lines instead (`TERMINAL_MIRROR_LINES`, `transcript.rs:253`).
6. **Group header double-counts:** "Read 2 files" plus "2 calls".
7. **Empty Thinking blocks still occupy list rows.** `thinking_card` returns an empty `div` (`transcript.rs:955-957`), which keeps the 8 px row gap. Muse keeps the block in the fold.
8. **`Mcp` and `Search` bodies have no cap at all** (`tool_card.rs:469`, `:563`).
