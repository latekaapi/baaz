# Claude Code as provider #2

Written 2026-09-23 from a **live probe of the CLI on this machine**, not from
memory. Every frame quoted below was captured by running `claude` and is
checked in under `fixtures/claude-code/`. Where this doc says "evidenced", it
means a named fixture line; where it says "unverified", nobody has run it.

    $ which claude     → /Users/latekaapi/.local/bin/claude
    $ claude --version → 2.1.276 (Claude Code)

## 0. Why this doc exists before any brief

Stage 3 is the first time the provider vocabulary (`crates/provider`) meets a
backend that is not muse. If the vocabulary only fits muse, that is a design
fault worth finding now, and it can only be found against real frames. Every
stage here that went well began with a design doc and a live probe; every
stage that went badly began with a brief written from memory.

## 1. The process shape — one long-lived child, not one per turn

Evidenced by `fixtures/claude-code/bidi.jsonl`. Two user messages written to
one child's stdin produced **two `result` frames under one `session_id`**
(`c19345b4-8d95-4a3c-9d34-87e895b58407`), from a single process that exited 0
when stdin closed.

    claude --print \
           --input-format stream-json \
           --output-format stream-json \
           --verbose \
           [--include-partial-messages] \
           [--model <alias|id>] \
           [--mcp-config <path>] [--strict-mcp-config] \
           [--resume <uuid> | --session-id <uuid>] [--fork-session] \
           [--permission-prompts host --permission-prompt-tool <tool>]

So the adapter owns **one child per session**, writes user turns to its stdin
as NDJSON, and reads frames from its stdout. This is the shape that makes
`SubmitTurn`, `SteerTurn` and `TurnControl` reachable at all: a
`--print`-per-turn design would re-pay session startup every turn and could
never interrupt one.

Two traps found by the probe, both cheap to hit and confusing to debug:

- **Variadic flags eat the prompt.** `--allowedTools A "the prompt"` consumes
  the prompt as a second tool name and the CLI dies with *"Input must be
  provided either through stdin or as a prompt argument"*. Put the prompt
  first, or terminate the variadic. The adapter should never pass a prompt
  positionally — it uses `--input-format stream-json` — but any probe script
  must.
- **Closed stdin is not the same as no stdin.** Without `< /dev/null` the CLI
  waits 3s and warns. The adapter always holds stdin open; a probe must
  redirect it.

### Input frame

    {"type":"user","message":{"role":"user",
      "content":[{"type":"text","text":"Say A1"}]}}

One NDJSON object per line. `--replay-user-messages` re-emits these on stdout
for acknowledgement; the adapter should enable it, because it is the only way
to know the child actually accepted a turn rather than buffering it.

## 2. The output frames, as captured

Seventeen distinct shapes appeared across the five fixtures. Counted from
`partial.jsonl` (45 lines, one tool-using turn with
`--include-partial-messages`):

| frame | count | carries |
|---|---|---|
| `system/init` | 1 | `session_id`, `cwd`, `tools[]`, `mcp_servers[]`, `model`, `permissionMode`, `slash_commands[]` |
| `system/hook_started` · `hook_response` | 1 · 1 | the user's own `SessionStart` hooks — **noise the adapter must tolerate, not parse** |
| `system/status` | 2 | `"requesting"` etc. — a spinner signal |
| `system/thinking_tokens` | 2 | `estimated_tokens`, `estimated_tokens_delta` |
| `system/task_summary` | 2 | a one-line human label for what the turn is doing |
| `system/post_turn_summary` | 1 | `status_category`, `status_detail`, `needs_action`, `summarizes_uuid` |
| `stream_event/message_start` | 2 | opening `usage` (already carries cache counts) |
| `stream_event/content_block_start` · `_delta` · `_stop` | 4 · 14 · 4 | `index`, `content_block.type` ∈ {`thinking`,`text`,`tool_use`}, `delta.type` ∈ {`thinking_delta`,`text_delta`,`input_json_delta`,`signature_delta`} |
| `stream_event/message_delta` · `message_stop` | 2 · 2 | terminal `stop_reason`, final `usage` |
| `assistant` | 4 | the **whole assembled block**, emitted again after its deltas |
| `user` | 1 | a `tool_result` — plus a sibling `tool_use_result` with `stdout`/`stderr`/`interrupted` |
| `rate_limit_event` | 1 | see §5 |
| `result/success` | 1 | `duration_api_ms`, `total_cost_usd`, `usage`, `modelUsage`, `permission_denials[]`, `terminal_reason` |

**The single most important decode rule:** `stream_event` frames and
`assistant` frames describe *the same content twice*. The deltas stream it;
the `assistant` frame repeats the completed block. An adapter that folds both
into the transcript **double-renders every message**. Pick one lane:

- render from `stream_event` deltas, and treat `assistant` as a checkpoint
  used only to correct drift (this is what a live UI wants);
- or drop `--include-partial-messages` and render from `assistant` alone.

This is exactly the shape of the Stage-2 ledger defect, one layer up: two
paths describing one thing, and nothing asserting they agree. The gate for
S3.1 must fold `partial.jsonl` **and** `basic.jsonl` and assert the resulting
transcript is identical modulo streaming granularity.

Every frame carries `session_id` and `uuid`; `stream_event` and `assistant`
also carry `parent_tool_use_id`, which is `null` on the main thread and set
inside a sub-agent. That field, not a heuristic, is how `SubagentTurns` is
decided.

## 3. Sessions, resume and stored history

Evidenced by `resume.jsonl`.

- The session id is a UUID, first seen on `system/init` and repeated on every
  frame including `result`.
- `--resume <uuid>` **keeps the same id** (`f3815266-…` in and out) and
  hydrates prior context: the resumed turn answered a question about the
  previous turn's tool call correctly.
- `--fork-session` with `--resume` mints a **new** id instead — that is
  `Command::ForkSession`, natively.
- `--session-id <uuid>` lets the caller *choose* the id up front. The adapter
  should use this rather than scraping `init`, so a Baaz session id and a
  Claude Code session id are the same string and `ReadSession` needs no map.

**Resume does not replay the transcript on stdout.** A resumed child emits a
fresh `init` and then only the new turn. So `Command::ReadSession` and
`PageTranscript` cannot be served from the stream; they must read the stored
transcript:

    ~/.claude/projects/<cwd-slug>/<session-id>.jsonl

where `<cwd-slug>` is the absolute cwd with `/` → `-`
(`/private/tmp/ccprobe/work` → `-private-tmp-ccprobe-work`). Measured: three
sessions, 218–242 KB each, same NDJSON frames as the stream.

The handoff named stored-history hydration as the least confident spot in the
whole seam. It is: the slug transform is undocumented, derived here by
observation from one directory, and **`/tmp` resolving to `/private/tmp` is
already a counter-example** — the slug follows the *resolved* path. S3.1 must
resolve the cwd before slugging and must degrade to `Unverified`, never to a
wrong file, if the directory is absent.

## 4. Client tools — the reason Stage 3 comes before Stage 4

Evidenced end-to-end by `mcp.jsonl`. A stdio MCP server declared in a
`--mcp-config` file:

    {"mcpServers":{"baazprobe":{"type":"stdio",
      "command":"python3","args":["/tmp/ccprobe/mcpsrv.py"]}}}

produced, in one turn and with no further wiring:

    init.mcp_servers  [{"name":"baazprobe","status":"connected","source":"dynamic"}, …]
    init.tools        contains "mcp__baazprobe__baaz_ping"
    tool_use          {"name":"mcp__baazprobe__baaz_ping","input":{}}
    tool_result       [{"type":"text","text":"BAAZ_PONG"}]

So `ClientTools` is **native**, and the naming convention is
`mcp__<server>__<tool>`. Two notes the probe forced:

- `init.mcp_servers` also listed five of the user's own claude.ai connectors,
  two `connected` and three `needs-auth`. Baaz must pass
  **`--strict-mcp-config`** so a Baaz session sees Baaz's tools and nothing
  else; otherwise a turn's tool surface depends on the operator's unrelated
  configuration, which is neither reproducible nor safe.
- The model reached the tool through `ToolSearch` first
  (`{"query":"select:mcp__baazprobe__baaz_ping"}`). Deferred tools are normal;
  an adapter must not treat a `ToolSearch` call as a failure to find the tool.

"SDK-type MCP server" on the board means an **in-process** server, which the
SDK offers and the CLI does not. Over the CLI the equivalent is this stdio
server, spawned by Baaz, speaking to the child over a pipe. That is what S3.2
builds, and this fixture is its acceptance evidence.

### It has since been done with Baaz's own bridge — `mcp-rust.jsonl`

The probe above used a throwaway Python server. `crates/mcp-bridge`'s real
binary has now been put in its place and a live `claude` child drove it:

    init.mcp_servers  [{"name":"baaz","status":"connected","source":"dynamic"}]
    init.tools        contains "mcp__baaz__ping"
    tool_use          {"name":"mcp__baaz__ping","input":{}}
    tool_result       [{"type":"text","text":"PONG"}]

That is `ClientTools: Native` earned rather than asserted, and it is the first
time anything in this repo has been called by a second provider.

**`--strict-mcp-config` is confirmed in the same run**: `init.mcp_servers`
listed the baaz server and *nothing else*, where the unstrict run listed five
of the operator's own claude.ai connectors beside it.

### The permission gate, found the hard way

The first attempt failed, and the failure is a design constraint, not a
mishap. With the server connected and the tool listed, the call came back:

    "Claude requested permissions to use mcp__baaz__ping, but you haven't
     granted it yet." · is_error: true

A connected MCP server is **not** a callable one. The child refuses every
client tool until it is allowed, so a Baaz-spawned child must either pass
`--allowedTools mcp__<server>__<tool>` for each tool it means to expose, or
answer the prompt through `--permission-prompts host --permission-prompt-tool`.
Baaz already owns an approvals surface, so routing these to it is the
better of the two and keeps a person in the loop — but **whichever is
chosen, "the server connected" must never be read as "the tool works".**
An adapter that declares `ClientTools: Native` on the strength of the
handshake alone would ship a tool surface that refuses every call.

## 5. Account, and the rate-limit frame

`rate_limit_event`, captured verbatim:

    {"type":"rate_limit_event","rate_limit_info":{
      "status":"allowed_warning","resetsAt":1790384400,
      "rateLimitType":"seven_day","utilization":0.79,
      "isUsingOverage":false,"surpassedThreshold":0.75,
      "unifiedWindows":{
        "five_hour":{"utilization":0.19,"resetsAt":1790187000},
        "seven_day":{"utilization":0.79,"resetsAt":1790384400}}}}

This is the same information the muse tier probe produces — two windows, a
percentage each, a reset time each — and it maps onto the footer meter with no
new UI. `isUsingOverage` is the Claude Code analogue of the pay-as-you-go
banner: **true means turns are billing beyond the plan**, and Stage 2 already
established that a person is entitled to know that before they send.

`result.total_cost_usd` is present and non-zero (0.0187698 on a Haiku turn),
which is worth noting against open defect D3, where muse reports 0.0.

**Not evidenced:** there is no login/logout command surface here.
`claude auth` is a separate subcommand this probe did not run, and `BeginLogin`
/ `CancelLogin` / `LogOut` are therefore **unavailable** for now, with the
honest reason "Claude Code authenticates outside the session; use `claude
auth`". They must not be declared `Unverified`, because `Unverified` is
attempted and attempting them would do nothing.

## 6. The capability declaration

This is the S3 deliverable that must be **evidenced, not asserted**. The
`state` column names its evidence; anything without evidence is `Unverified`,
which is honest ignorance and is still attempted.

| capability | state | evidence / reason |
|---|---|---|
| `SessionLifecycle` | Native | `--session-id`, `--resume`; `init` + `result` in every fixture |
| `ForkSession` | Native | **executed**: `fork.jsonl` — resuming `f3815266-…` with `--fork-session` minted `c4b6fee5-…`, a different id |
| `CompactSession` | Emulated | `--autocompact <auto\|tokens>` sets a window; there is no "compact now" command over `--print`. Differs from native: it happens when the window fills, not when asked |
| `SessionConfig` | Native | `--model`, `--permission-mode`, `--add-dir`, `--append-system-prompt` |
| `SessionShell` | Native | the `Bash` tool is in `init.tools`; `tool_use_result` carries `stdout`/`stderr`/`interrupted` (`partial.jsonl`) |
| `SubmitTurn` | Native | `bidi.jsonl`: two turns, one child |
| `SteerTurn` | Unverified | a second stdin frame mid-turn was **not** probed. Do not claim it |
| `TurnControl` | Unverified | interrupt/cancel over stdin not probed |
| `ModelCatalog` | Emulated | `--model` takes aliases and ids, but no fixture enumerates them; Baaz supplies the list |
| `Approvals` | Native | `--permission-prompts host --permission-prompt-tool <tool>`; `result.permission_denials[]` exists and was empty |
| `Questions` | Unavailable | Claude Code has no question channel distinct from the transcript. Reason: "Claude Code asks in prose; there is no question id to answer" |
| `Transcript` | Native | `stream_event` + `assistant`, plus the stored `.jsonl` (§3) |
| `Account` | Native | `rate_limit_event` (§5) |
| `ClientTools` | Native | `mcp.jsonl` **and** `mcp-rust.jsonl` — the latter drives Baaz's own bridge binary end to end (§4). Native only once the tool is allowed; see the permission gate in §4 |
| `ReasoningTraces` | Native | `thinking` blocks with `thinking_delta`, plus `system/thinking_tokens` |
| `SubagentTurns` | Unverified | `parent_tool_use_id` is on every streamed frame and `--forward-subagent-text` exists, but no probe ever spawned a sub-agent. Read off `--help` is not evidence; `Unverified` is attempted, never refused |

Three `Unverified` and one `Unavailable` is the honest reading today. Raising
any of them requires a new fixture, not an argument.

## 7. Version handling — a floor, reusing S2.2

`crates/provider-muse/src/caps.rs` already has the mechanism:
`MUSE_VERSION_FLOOR`, `muse_version_supported`, numeric `major.minor.patch`
comparison, unparseable fails closed. **Reuse it; do not invent a second one.**

`claude --version` prints `2.1.276 (Claude Code)` — a trailing parenthetical,
where muse's is a `-R3401.1` trailer, so the existing parser must tolerate
both. The floor is **2.1.0**: `--include-partial-messages`,
`--strict-mcp-config`, `--permission-prompts` and `--session-id` are all
present at 2.1.276 and none was probed below it. An exact pin is forbidden
here for the reason already written into `docs/17-providers.md` — pinning a
CLI version by string equality once made half the features fail closed on this
machine after a routine upgrade.

## 8. What this doc does **not** establish

Named so nobody reads a table cell as a promise:

- No turn was ever steered or interrupted. `SteerTurn` and `TurnControl` are
  `Unverified` and must ship that way.
- Sub-agent forwarding was read off `--help`, not run, so `SubagentTurns` is
  `Unverified`. (`--fork-session` **was** run after the first draft of this doc:
  see `fork.jsonl` and §6.)
- The `<cwd-slug>` transform is one directory's worth of observation (§3).
- Every probe ran on Haiku with a trivial prompt. Nothing here says anything
  about long transcripts, compaction, or a turn that exceeds the window.
- No Baaz UI has ever rendered one of these frames.

---

# Addendum 2026-09-24 — how client tools get permission. SETTLED, by probe.

The stage-3 handoff left this open with two options and told the next session to
ask the owner. The owner asked for research instead. **The research found a third
option that is strictly better than both, and it is proven live on this box**
(`claude 2.1.276`, fixture `fixtures/claude-code/permission.jsonl`).

## The answer

    --permission-prompts host  --permission-prompt-tool stdio

`stdio` is a **sentinel, not a tool name**. It routes every permission decision
over the *same* `--input-format stream-json` control channel the adapter already
reads and writes. No second MCP server, no blanket allowlist, no extra process.

Captured exchange, both directions:

    <- {"type":"control_request","request_id":"a2200299-…","request":{
         "subtype":"can_use_tool",
         "tool_name":"mcp__baaz__ping",
         "display_name":"Ping",
         "mcp_server":{"name":"baaz","source":"dynamic"},
         "input":{},
         "tool_use_id":"toolu_01HEpjaH…",
         "permission_suggestions":[{"type":"addRules","behavior":"allow",
             "rules":[{"toolName":"mcp__baaz__ping"}],"destination":"localSettings"}]}}
    -> {"type":"control_response","response":{"subtype":"success",
         "request_id":"a2200299-…",
         "response":{"behavior":"allow","updatedInput":{}}}}
    <- tool_result: [{"type":"text","text":"PONG"}]

## Why this beats both options on the board

- **vs. blanket `--allowedTools`:** nothing is silently pre-approved. An
  auto-approved tool *never reaches the callback at all* — the SDK docs are
  explicit that a bare allow entry bypasses every host-side check for that tool.
  A harness whose whole point is showing the owner what the agent is doing should
  not start by making its own tool calls invisible.
- **vs. `--permission-prompt-tool mcp__baaz__<something>`:** that needs a second
  MCP server whose tool handler must somehow reach the UI thread and block on a
  human. The `stdio` route delivers the request on a channel Baaz's adapter
  already owns, in the same reader loop, with the turn already suspended.
- **It is not limited to Baaz's own tools.** The same channel carries `Bash`,
  `Edit`, `Write` — *every* tool the child wants. So Baaz's approvals surface
  becomes the approvals surface for the whole Claude Code session. That is the
  actual prize, and neither board option reached it.
- `permission_suggestions` hands us the "always allow" affordance already shaped:
  `addRules` / `behavior:"allow"` / `destination:"localSettings"`. It is the
  "Don't ask again" checkbox, provider-authored and persistable.
- `display_name` ("Ping") is a human label. Free accessibility.

## What had to be learned the hard way — three dead ends, all probed

1. **`--permission-prompts host` alone does not open the channel.** It
   auto-denies. The child emits `system/permission_denied` and a
   `post_turn_summary` with `status_category:"blocked"`, and the tool_result is
   `is_error:true`. This is exactly the failure the handoff recorded — it was
   never a missing grant, it was a missing *answerer*.
2. **Completing the `initialize` control handshake does not open it either.**
   Sending `{"type":"control_request","request":{"subtype":"initialize","hooks":{}}}`
   gets a correct `control_response` (it returns the slash-command list), and the
   very next tool call still auto-denies. The control channel being *live* is not
   the same as permissions being *routed* to it.
3. Only adding `--permission-prompt-tool stdio` produced a `can_use_tool`
   request. **Both flags are required together.**

`--permission-prompt-tool` does not appear in `claude --help`; only
`--permission-prompts` does, and its help text mentions it in passing
("the SDK host or `--permission-prompt-tool`"). It is accepted and it works.

## Consequences for the wiring (§3b of the stage-3 handoff)

- The adapter's frame reader must handle a **third frame direction**: not just
  child→host output frames, but `control_request` needing a `control_response`.
  A reader that only folds `assistant`/`stream_event` will hang the turn forever
  on the first permission request — the child waits silently, with no timeout
  observed.
- `result` frames carry `permission_denials: [{tool_name, tool_use_id, tool_input}]`,
  a structured after-the-fact list. Useful for the transcript; not a substitute
  for answering.
- Unlike muse, Claude Code's `result` frame carries a **real** `total_cost_usd`
  and a per-model `modelUsage` with `costUSD`. D3 was withdrawn for muse because
  the field was a hardcoded `0.0`; **on this provider the number is real and must
  not inherit muse's `0.0` literal.**
