# Muse Code 1.1.1 workflows and subagents: operator's guide

Evidence-based notes for driving `muse exec` headless and fanning one run
out to subagents or a workflow. Binary: `Muse Code 1.1.1 (1.1.1-R2514.1)`
(`muse --version`). No live turns were started for this doc (reading only);
all claims cite a surface the author actually read. No spend was incurred.

## 1. What a "workflow" is

A workflow is a durable multi-child run owned by the model, not a CLI
object. Evidence:

- Feature gates live in `~/.local/share/muse/feature-config/` (a directory;
  the file inside is `7075626c6963.json`):
  `"workflow_tool": true, "workflow_api_v2_rollout": false`
  (alongside `"local_session_messaging": true`, `"plugins": false`).
- `muse schema --help` says the exported MSP schema covers `session/*`,
  `turn/*`, `model/list`, `view/*`, `approval/*`, `userInput/*`, "with one
  exception: `workflow/*` (spec 14410) carries no row yet. tdd.md
  SS3.19/SS3.20 ratify its params and ack, so what is outstanding is the
  protocol-crate types and bundle rows, not the shapes."
- Confirmed: `MspMethod` in `fixtures/msp/msp-ts/msp.d.ts` (line ~1962) and
  in a fresh `muse schema generate-ts --out /tmp/schemadump --experimental`
  export list `subagent/*` but **no** `workflow/*` method. There is no
  client-callable launch RPC in the schema this binary exports.

What the wire fold records about a workflow run (`Item` in `msp.d.ts`):

- `children?: WorkflowChild[]` — "folded per-child state, keyed by
  `(childId, attempt)`, re-emitted whole on every change — the item
  `revision` is the ordering guard".
- `scriptId` ("launched script identity"), `entryId` ("launched entry
  identity"), `resumeFromRunId` ("set on resumed launches"),
  `triggerSource` ("camelCased `WorkflowLaunchTriggerSource`, verbatim
  (durable runtime vocabulary, e.g. `\"modelProposal\"`)"), `message` ("the
  reconciled terminal message, set on completion"), `workflowRunId` ("the
  owning durable workflow run id (opaque string — not a UUID family)").
- `WorkflowChild`: `childId`, `attempt` (retries are new attempts under the
  same child id), `status` (verbatim durable vocabulary), `label`,
  `phase`, `resultRef` (opaque), `terminal: TurnTerminal`, `usage`,
  `durationMs`.
- Failure surface: `TurnErrorKind` includes `"workflowLaunchError"`, mapped
  in `crates/muse-adapter/src/failure.rs` ("Workflow launch error").

Reading: a workflow is defined by a **script + entry** the model proposes
(`triggerSource: "modelProposal"` is the documented example); children run
as subagent turns under one `workflowRunId`; retries are attempts; a later
launch can resume via `resumeFromRunId`. There is **no** `muse exec`
`--workflow` flag (`muse exec --help` has none) — the operator triggers one
by prompting ("use a workflow with N children…") and the model proposes the
launch through the workflow tool (gated on, per `workflow_tool: true`).
Where script definitions live on disk (workspace file? `~/.config/muse`?
plugin?) was **not** determinable from any surface read (see §6).

## 2. Subagents

Spawn primitive: the model opens a `subagent` item; the owner drives it
with `subagent/*` methods. `muse exec --help` offers no spawn fan-out
flag — "spawn N in parallel" is a prompt instruction, optionally with
`--parallel-tool-calls` ("Enable Meta API parallel tool calls").

- Agent definitions: `muse --help` documents only
  `--agents <JSON>` — "Supply one ephemeral agent-definition overlay".
  The JSON shape is not described in help, and `muse init --dry-run`
  scaffolds only project rules (`AGENTS.md`: name, commands, layout) —
  no agent-definition scaffold. Workspace agent-config pathsourced from
  skills (`.agents/skills/<id>/`, `$CONFIG_DIR/skills/<id>`) cover skills,
  not agent definitions. Roles: the fold records `role?: string` ("role as
  spawned") and `objective?: string` ("objective as spawned") — free-form
  at the wire level.
- Depth: `depth?: number` ("nesting depth"; `u32` in
  `crates/muse-client/src/schema.rs`). No max depth found anywhere (see §6).
- Worktree isolation: `--subagent-worktree-isolation` —
  "Compatibility flag; capability defaults on. Only an affirmative
  per-child request asks for isolation; omission stays shared. Requests may
  reject when capability, provider, or Git prerequisites are unavailable."
  Session-level `-w, --worktree [off|create|existing]` (bare `-w` = create,
  `--worktree-base` default `HEAD`) is the parent session's own worktree,
  a separate axis from per-child isolation.
- Results: `SubagentResult` — `summary: string` ("Bounded result summary
  (<=512 chars, runtime-enforced)"), `text?: string` ("Result text
  (<=32 KiB)"), `artifactRefs: string[]`, `evidenceRefs: string[]`,
  `structuredData?`, `errorKind?`. Lifecycle: `controlStatus`
  (`accepted|starting|running|resultReady|closing|closed|recoveryPending|
  manualReconciliation`; generic `status` stays terminal authority).
  Drill-down: `childSessionId` ("readable via `session/read`/`view/page`");
  on disk, `read-session` skill: `subagent/<child-session-id>/session.jsonl`.
- Owner RPCs (schema): `subagent/sendMessage`, `followupTask` (body:
  "trimmed… rejected when empty"), `interrupt`, `stop`, `resume`,
  `reopen`, `close`, `readResult` — all keyed by
  `(commandId: UUIDv7, sessionId, subagentId)`.
- No bundled workflow/subagent skill exists: `muse skills list --source
  built-in --json` enumerates 15 ids (browser-app-delivery, create-skill,
  doctor, durable-test-collateral, git, greenfield-project-scaffolding,
  grill, import, manage-settings, plan, python-env, read-session,
  requirements-clarification, table-fit, taste). `read_skill` on doctor,
  read-session, create-skill confirmed none describes spawning.

## 3. Cross-session messaging

`muse session-message --help`: only
`list [--json]` and
`send --target <session-uuid-or-name> [--in-reply-to <reply-token>]
[--display-context <json>] [--json] < body`.
Observed: `muse session-message list` prints
`external agent ingress is unavailable`. Gate `local_session_messaging`
is `true`, so the pipe exists but ingress is currently refused here.
Verdict: not an orchestration primitive — children are driven by
`subagent/*` owner methods, not messages; session-message is
operator-to-session notes, and it does not currently accept input in this
environment.

## 4. Limits, usage accounting, billing

- No numeric caps surfaced: neither help text, nor the stable nor the
  experimental schema, nor `docs/` states max subagents, max depth, or a
  workflow step cap. `--max-model-steps <N>` ("Cap the number of model
  steps") is the only step budget, and whether one budget covers parent
  plus children or each child gets its own was not determinable (see §6).
- Usage: `Item.usage` on a subagent is "**transitive** observed usage —
  the child and its own descendants… never folded into
  `session/tokenUsage.cumulative`" (`msp.d.ts`; same doc on
  `session/tokenUsage`: "never folded in — it rides the owning items").
  Monitor per-child cost on the owning `subagent`/`workflow` item, not the
  session totals.
- Billing: per `docs/06-billing.md` and project rules, cost is decided by
  the login token, and anything reaching `turn/start` is billed. Each
  child runs its own turns, so expect each child's model steps to bill
  like parent steps. Current login (read-only `cargo run -p harness --
  --print-tier`): `Muse Code High Usage` plan.

## 5. Worked recommendation: 4 isolated tasks + integrate

Invocation (run from a clean checkout; nothing below was live-tested —
`exec` starts a billed turn, so this is a recipe, not a transcript):

```sh
muse exec --json --session-id <uuid> --yolo \
  --subagent-worktree-isolation \
  --max-model-steps 60 \
  --prompt-file /tmp/wf-prompt.md
```

Prompt shape (`/tmp/wf-prompt.md`): name the workflow, enumerate the four
independent tasks with acceptance criteria and non-overlapping file scopes,
require one isolated-worktree child per task (affirmative isolation
request each), require the model to report each child's `resultRef`/
summary, and gate a final integration step (merge to parent worktree,
build + tests) on all four succeeding; forbid the parent from editing task
files while children run.

```text
Use a workflow with 4 children, one per task, each in its own isolated
worktree. Task A: … (files: …; done when: …). Task B: … Task C: … Task D:
… Do not start the integration step until all four report success; then
merge, run <build+test>, and report per-child summaries plus the test
result.
```

Monitor: `--json` streams JSONL; the durable-signal event names for
workflow/subagent progress in that stream were **not** observed in this
read-only run (see §6). Reliable fallback: tail the session log and fold
`item/started|updated|completed` for `kind: "workflow"` (watch
`children[]` statuses per `(childId, attempt)`) and `kind: "subagent"`
(watch `controlStatus` → `resultReady`, then `result.summary`, `usage`,
`workflowRunId` linkage); drill into `subagent/<id>/session.jsonl`.

## 6. What could NOT be determined

- Workflow script format and where definitions live (script language?
  workspace file? `~/.config/muse`? plugin?); the `workflow_tool` tool
  schema itself (params beyond spec 14410's ratified shapes).
- `--agents <JSON>` overlay schema; workspace agent-definition paths; the
  role vocabulary; what tools/context a subagent inherits.
- Numeric limits: max subagents, max depth, workflow/child step caps, and
  whether `--max-model-steps` covers the whole tree or one turn/run.
- `muse exec --json` event envelope and type names for workflow/subagent
  progress (running it bills a turn; deliberately not done here).
- Whether per-child worktree isolation needs a git-clean tree or a base
  ref like `-w` takes (plausibly yes; unconfirmed).
- Exact billing unit of child turns beyond "a turn is a turn".
