# Handoff — continue the Muse Code chat slice

Paste the block below into a new Claude Code session opened in
`/Users/latekaapi/Projects/harness`.

```
Continue building the Harness: a feature-complete gpui chat interface to Meta's Muse Code
agent (`muse` CLI 1.0.3, `muse serve` = "MSP", JSON-RPC 2.0 as NDJSON over stdio), built on
the `aui` library at /Users/latekaapi/Projects/agentic-ui (path dependencies; gpui-pre 0.3.3
+ gpui-kit 0.6; library changes go on agentic-ui branch `muse-support`, never main).

Read first, in order: docs/00-spec.md (frozen spec: crates, decisions, keymap, five phases
with gates), docs/CHANGELOG.md, docs/01-transport.md (Phase 1 findings), docs/02-app.md if it
exists (Phase 2), agentic-ui/docs/10-muse-research.md §1, §3, §4, §7 (wire contract, auth,
what is not on the wire, adapter mapping), agentic-ui/docs/00-agent-brief.md, and your memory
file project-harness-muse-slice-2026-09-08.

State on 2026-09-09:
- Phase 1 DONE and reviewed: harness commit 0d6417a (crates/muse-client transport + typed
  schema, crates/muse-adapter `MuseFold` + `SideState`, replay tests over fixtures/msp/*.jsonl
  with snapshots in crates/muse-adapter/tests/snapshots, `live_echo` ignored test,
  `harness-probe` binary); agentic-ui `muse-support` 3572bb9 (aui-protocol extensions:
  Provider::Muse, four MSP approval modes + Session::plan, new Delta/Block/Marker/Intent
  variants). Gates green in both repos.
- Phase 2 (gpui app: shell, auth probe + device-code login screen, sessions sidebar with
  view/page backfill, live streaming transcript, stop/retract, reconnect, error dialog;
  library gains `aui::screens::login` and `aui::overlay::dialog` with gallery entries) was
  running as one Opus lead when this handoff was written. Check `git log --oneline | head`
  in both repos: if the top harness commit is still 0d6417a and `crates/harness/` is
  untracked, the lead was interrupted — review what exists, finish Phase 2 against the spec's
  phase-2 gate (screenshots light/dark in docs/images/phase2-*.png, docs/02-app.md, commits in
  both repos), otherwise review its commit and screenshots, then move on.
- Phases 3 (composer controls: model/effort/mode menus, context meter + compaction, queue
  strip + steer, mentions + command menu with skills, client-side plan mode with the /plan
  skill probe, prompt history, images), 4 (approval card v2 multi-stage + feedback + policy
  and judge resolutions, question previews/timeout/clarify, error banners/dialogs, retry and
  retry-scheduled, markers, fork, todo, goal) and 5 (polish, motion, focus, docs, CI) are
  not started. Each is one Opus lead session briefed with the spec sections named in
  docs/00-spec.md §5; the owner reviews at each gate on screenshots and the diff.

Facts that cost time to learn (details in docs/01-transport.md and the research doc):
- `muse serve --no-session-log` accepts turns but emits NO view events; always run durable.
- Provider is chosen per session (`session/start { providerId: "echo" | "meta" }`); echo is
  free — use it for everything except at most 5 real turns per phase.
- `commandId` must be UUIDv7; both directions send requests (approval/request and
  userInput/request arrive with the server's ids — never answer them with a JSON-RPC result,
  use approval/decide and userInput/*); view events can precede a command's ack.
- ReasoningEffort on the wire is none|minimal|low|medium|high|xhigh|ultra; `max` is rejected.
- No plan mode, no auth, no rename/delete/search on the wire; `/plan` is a skill; login is
  `muse login` with `MUSE_LOGIN=1` parsed from stderr; logged-out shows up only as a failed
  turn. Session names/titles come read-only from ~/.local/share/muse/session-index.db.
- An approval's itemId is its own id; the fold joins approvals to their turn instead.

Working rules (owner's instructions): the main session is Fable and spends its tokens on
design decisions and reviews only; ONE Opus (or Sonnet) lead per phase does the work,
briefed with a self-contained prompt, and Fable spot-checks the diff, reruns one gate and
reads the screenshots. Prefix shell commands with
`export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`. Library rules:
no literal colours/sizes/durations, stateless RenderOnce components, intents out,
popover_layer for overflow, both themes, gallery entry for every new component, gates
(`cargo build/test/clippy --all-features -D warnings`, rustdoc -D warnings, regenerate
docs/06-api.md). Commit per phase in both repos; commit messages end with
"Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>". Do not touch ~/Projects/cockpit.
```

## Pointers

- Research (wire contract, live captures, auth, TUI, reference-app inventories, aui gaps):
  `/Users/latekaapi/Projects/agentic-ui/docs/10-muse-research.md`
- Exact schema for this muse build: `fixtures/msp/msp-ts/msp.d.ts`, `fixtures/msp/msp/`
- Wire captures (ground truth): `fixtures/msp/transcript-*.jsonl`; `probe*.py` are working
  Python clients for re-probing.
- Persistent memory for this project lives at
  `~/.claude/projects/-Users-latekaapi-Projects-agentic-ui/memory/`.
