# 19 — Codex as provider #3

Written 2026-09-24, **from a live probe on this box**, before any brief.
Every claim below is either a quoted frame from `fixtures/codex/` or a line
from a schema in `schemas/codex/`. Where something is not evidenced, it says so.

    codex --version   ->  codex-cli 0.144.6
    which codex       ->  /opt/homebrew/bin/codex
    account           ->  chatgpt · latekaapi@gmail.com · planType "prolite"

## 0. The headline, and it inverts the plan

The board planned stage 4 around *Codex being the weaker provider* — "Codex works
without native client tools … the capability row earns its keep here by saying so
plainly". **That is wrong, and the probe says so.**

Codex's `app-server` is the **richest** of the three backends. It is a
JSON-RPC 2.0 protocol with **87 client requests, 10 server requests and 68 server
notifications**, and it natively supports several things Claude Code either
emulates or cannot do at all — including steering, interrupting, compaction,
a model catalog, an account/rate-limit surface, and a client-tool call direction.

Stage 4 is therefore **not** "prove the vocabulary degrades gracefully". It is
"prove the vocabulary does not *cap* a backend that exceeds it". That is a
different risk and the briefs must be written for it.

### The board's "267 vendored schemas" was right after all

The stage-3 handoff corrected the board to "there are no vendored schemas —
the binary generates 39". Both halves of that correction are partly wrong:

    codex app-server generate-json-schema --out DIR
      ->  39 entries at the top level (37 .json + v1/ + v2/)
      ->  267 .json files in total

267 is the real number. The handoff counted only the top level.

## 1. Transport

    codex app-server            # stdio, newline-delimited JSON-RPC 2.0
      daemon | proxy | generate-ts | generate-json-schema

`app-server` is flagged **`[experimental]`** in `codex --help`. Record that in the
capability map: the floor is a version we pin, and the protocol may move under us.

Handshake, exactly as captured in `fixtures/codex/basic.jsonl`:

    ->  {"id":1,"method":"initialize","params":{
          "clientInfo":{"name":"baaz","title":"Baaz","version":"..."},
          "capabilities":{"experimentalApi":true}}}
    <-  {"id":1,"result":{"userAgent":"baaz/0.144.6 (Mac OS 26.6.2; arm64) ...",
          "codexHome":"/Users/…/.codex","platformFamily":"unix","platformOs":"macos"}}
    ->  {"method":"initialized","params":{}}

`InitializeCapabilities` is client-declared and includes `experimentalApi`,
`optOutNotificationMethods` (suppress named notifications for this connection —
useful, 68 notification kinds is a lot), `mcpServerOpenaiFormElicitation` and
`requestAttestation`.

Then a thread and a turn:

    ->  {"id":2,"method":"thread/start","params":{"cwd":"…","model":"gpt-5.6-sol"}}
    <-  {"id":2,"result":{"thread":{"id":"01a0d344-…","sessionId":"…",
          "forkedFromId":null,"parentThreadId":null,"historyMode":"legacy",
          "modelProvider":"openai","createdAt":…}}}
    ->  {"id":3,"method":"turn/start","params":{"threadId":"…","model":"…",
          "input":[{"type":"text","text":"…"}]}}
    <-  {"id":3,"result":{"turn":{"id":"01a0d344-…","status":"inProgress"}}}

**`threadId` and `sessionId` are the same string** on a fresh thread. Baaz can
choose neither — unlike Claude Code's `--session-id`, the server mints it. That
is a real difference from stage 3 and the session mapping must store it.

## 2. What is proven, and the fixture that proves it

| Fixture | Frames | What it pins |
|---|---|---|
| `fixtures/codex/basic.jsonl` | 34 | initialize, model/list, account/read, rateLimits, permissionProfile/list, a turn that completes |
| `fixtures/codex/approval.jsonl` | 55 | a sandbox-escaping command, `item/commandExecution/requestApproval`, `accept`, the command then running |
| `fixtures/codex/interrupt.jsonl` | — | `turn/steer` mid-turn and `turn/interrupt`, ending `status:"interrupted"` |

Each line is `{"_dir":"client->server"|"server->client","frame":{…}}`, so the
fixtures record **both directions**. Stage 3's fixtures only recorded the child's
output; a bidirectional protocol needs both and the replay harness must expect it.

## 3. Approvals — the part that matters most, and it is better than ours

Codex asks the client for approval as a **server→client JSON-RPC request**.
Five distinct kinds exist in `ServerRequest.json`:

    item/commandExecution/requestApproval
    item/fileChange/requestApproval
    item/permissions/requestApproval
    item/tool/requestUserInput
    mcpServer/elicitation/request

Captured live (`fixtures/codex/approval.jsonl`), under `permissionProfile: ":read-only"`,
for a write that escapes the sandbox:

    <- {"id":…,"method":"item/commandExecution/requestApproval","params":{
         "threadId":"…","turnId":"…","itemId":"exec-62dc5340-…",
         "environmentId":"local","startedAtMs":1790250968177,
         "reason":"Allow creating /tmp/baaz_probe_write.txt containing HELLO as requested?",
         "command":"/bin/zsh -lc \"printf 'HELLO' > /tmp/baaz_probe_write.txt\"",
         "cwd":"/Users/latekaapi/Projects/harness",
         "commandActions":[{"type":"unknown","command":"printf 'HELLO' > …"}]}}
    -> {"id":…,"result":{"decision":"accept"}}

and the command then ran: `status:"completed"`, `exitCode:0`, and
`/tmp/baaz_probe_write.txt` contained `HELLO` on disk.

**`reason` is a human sentence the model writes.** Baaz's approvals surface gets a
plain-language justification for free — no other provider gives us that.

### The decision vocabulary maps onto Baaz's approvals surface almost exactly

`CommandExecutionApprovalDecision` (from the schema, not from memory):

| Decision | Meaning | Baaz control |
|---|---|---|
| `accept` | run it, once | **Approve** |
| `acceptForSession` | run it and stop asking, session-scoped cache | **Approve for this session** |
| `acceptWithExecpolicyAmendment` | approve + persist an execpolicy rule | *(defer — needs a rule editor)* |
| `applyNetworkPolicyAmendment` | persist allow/deny for a host | *(defer)* |
| `decline` | refuse; **the turn continues** | **Deny** |
| `cancel` | refuse; **the turn is interrupted** | **Deny and stop** |

`FileChangeApprovalDecision` is the same minus the two amendment variants.
`decline` vs `cancel` is a genuine distinction Baaz's current surface does not
draw, and it should: "no, do something else" and "no, stop" are different answers.

**The first wrong guess, recorded so nobody repeats it:** I replied
`{"decision":"approved"}` and it was silently treated as a refusal —
`status:"declined"`, `Rejected("rejected by user")` in the log, no schema error.
The token is `accept`. An invalid decision **fails closed and looks like a user
denial**, which is the safe direction but is indistinguishable from a real one.
Any adapter code must construct this from a typed enum, never a string literal.

### Approval only fires outside the sandbox profile

`permissionProfile/list` returns `:read-only`, `:workspace`, `:danger-full-access`.
Under `:read-only`, `echo BAAZ_PROBE` **ran with no approval request at all**
(`source:"unifiedExecStartup"`). The write to `/tmp` did prompt. So:

> Codex auto-approves inside its profile and only asks when an action escapes it.

That is the opposite of Claude Code, where the host is asked about everything it
has not pre-allowed. **Baaz cannot present one approvals model over both backends
without choosing which behaviour is the truth.** Named here as a design decision
for S4.2, not silently averaged.

## 4. Client tools — `Unverified`, not `Unavailable`, and the reason is named

The stage plan expected `ClientTools: Unavailable`. The protocol says otherwise:

    ServerRequest:  item/tool/call        ->  DynamicToolCallParams
    DynamicToolCallParams { threadId, turnId, callId, tool, namespace?, arguments }

That is unmistakably "server asks the client to execute a tool", i.e. the same
direction Baaz's `mcp-bridge` serves for Claude Code. The call direction exists.

**What is not evidenced is registration.** `InitializeParams` has no tool-declaration
field, and nothing in the 87 client requests obviously advertises a client tool
surface (`app/list`, `plugin/list` and `skills/list` are adjacent and may be the
real mechanism). So the honest capability value is:

> `ClientTools: Unverified` — "the protocol has `item/tool/call`, but no probe has
> made Codex call a Baaz-provided tool, and the registration path is unidentified."

Not `Unavailable`. `Unavailable` asserts a negative we have not established, and
the handoff was right to demand it be proved rather than assumed — the proof came
back the other way.

Codex also speaks MCP itself (`mcpServer/tool/call`, `mcpServerStatus/list`,
`config/mcpServer/reload`, `mcpServer/oauth/login`). Three MCP servers started
during every probe: `cua_repl`, `node_repl`, `codex_apps`. **As with stage 3's
`--strict-mcp-config` lesson, Baaz must establish whether it can scope these
down — a Baaz session that silently inherits the owner's Codex MCP servers is the
same defect stage 3 found, in a new place.** Not yet probed. S4.2 owns it.

## 5. Capability map — proposed, with the evidence in the cell

| Capability | Codex | Evidence |
|---|---|---|
| `SteerTurn` | **Native** | `turn/steer` → `{"turnId":…}`, mid-turn, `fixtures/codex/interrupt.jsonl`. Needs `expectedTurnId` |
| `TurnControl` | **Native** | `turn/interrupt` → `turn/completed` with `status:"interrupted"` after 14.3s of a 49s turn |
| `CompactSession` | **Native** | `thread/compact/start` + `thread/compacted` notification *(method present; not executed)* |
| `ModelCatalog` | **Native** | `model/list` → 4 models with `displayName`, `description`, `supportedReasoningEfforts`, `hidden` |
| `Account` | **Native** | `account/read` → `{type:"chatgpt",email,planType:"prolite"}`; `account/rateLimits/read` → `usedPercent`, `windowDurationMins`, `resetsAt`, `credits` |
| `Questions` | **Native** | `item/tool/requestUserInput` + `mcpServer/elicitation/request` are server requests *(shape read; not executed)* |
| `Approvals` | **Native** | five request kinds, round trip proven end to end |
| `ClientTools` | **Unverified** | `item/tool/call` exists; registration path unidentified (§4) |
| `Cost` | **Unavailable** | no cost field anywhere in the protocol. `thread/tokenUsage/updated` gives tokens, never money |
| `Fork` | **Native** | `thread/fork`; `thread.forkedFromId` is in the thread object |
| `Resume` | **Native** | `thread/resume`, `thread/list`, `thread/read`, `thread/loaded/list` |

Three cells say *(method present; not executed)*. Those are **`Unverified`
until a fixture exists** under the stage-2 rule — reading a method name off a
generated schema is exactly the "reading a flag off `--help` is not evidence"
mistake the handoff warns about. They are listed as Native *proposals*; S4.1 must
either capture the fixture or downgrade the cell. **Do not ship the table as-is.**

## 6. The ledger gets better input than it has ever had

    thread/tokenUsage/updated {
      threadId, turnId,
      tokenUsage: {
        total: {totalTokens, inputTokens, cachedInputTokens, outputTokens, reasoningOutputTokens},
        last:  {…same…},
        modelContextWindow: 258400 }}

Per-turn, carrying its own `turnId`, split into `total` and `last`, with cache
and reasoning tokens broken out. This lands directly on the stage-3 `(session_id,
turn_id)` key with **no cursor and no backfill** — which is to say, the entire
class of defect D1 came from does not exist on this provider.

**And it is the right moment to pay the debt the stage-3 handoff named.**
`migrate_v1_to_v2` breaks ties by lexicographic `view_cursor`; it went the right
way by luck (92,278 tokens survived over 0). The next ledger change must replace
it with a principled tie-break — prefer non-zero tokens, then later `finished_at_ms` —
pinned by a test built from that exact pair. **Stage 4 touches the ledger. That
makes it stage 4's debt.**

`account/rateLimits/updated` is also **pushed unprompted** after every turn, so
the money guard can be fed without polling.

## 7. Live findings that are not about Baaz, but the owner should know

1. **Codex on this box cannot complete a turn in its default config.**
   `~/.codex/config.toml` sets `model = "gpt-6-luna"`, and the account rejects it:
   `"The 'gpt-6-luna' model is not supported when using Codex with a ChatGPT account."`
   `model/list` offers `gpt-5.6-sol`, `gpt-5.6-terra`, `gpt-5.6-luna`, `gpt-5.5`.
   Every probe here passed an explicit `model`. **A one-word config fix.**
2. `codex_models_manager` logs `failed to load models cache: missing field
   'base_instructions'` on every start. Harmless so far; noted so it is not
   rediscovered as a Baaz bug.
3. Codex rate limit: **19% of a 7-day window used.** (Claude Code's, read the same
   day from a `rate_limit_event`: **90%**, `allowed_warning`.)

## 8. Version floor

Reuse `provider::version_at_least` from `crates/provider/src/version.rs`. Do not
invent a second mechanism — stage 3 hoisted it precisely so a third provider
would not.

The justified floor is **0.144.6**, the version every frame here was captured
from, because `app-server` is `[experimental]` and we have no evidence about any
earlier one. `codex --version` prints `codex-cli 0.144.6`; the stage-3 parser fix
(which choked on `2.1.276 (Claude Code)`) handles the `codex-cli ` prefix only if
the adapter strips it — **test that, it is the same bug in a new costume.**

## 9. The drift gate

    schemas/codex/               37 top-level + v1/ + the two aggregate schema files
    schemas/codex/MANIFEST.sha256   sha256 of all 267 generated files
    schemas/codex/VERSION           0.144.6

The gate regenerates from the installed binary into a temp dir, hashes every file
and diffs against `MANIFEST.sha256`. The manifest covers all 267 so nothing drifts
unseen; the checked-in aggregates (`codex_app_server_protocol{,.v2}.schemas.json`,
516 definitions) make the diff *readable* when it fires. `v2/`'s 228 per-type
expansions are redundant with the aggregate and are deliberately not committed.

A drift is not a failure — it means the CLI moved. The gate's job is to make that
a decision rather than a surprise.
