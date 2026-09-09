# Handoff — continue the Muse Code chat slice

Paste the block below into a new Claude Code session opened in
`/Users/latekaapi/Projects/harness`.

```
Continue building the Harness: a feature-complete gpui chat interface to Meta's Muse Code
agent (`muse` CLI 1.0.3, `muse serve` = "MSP", JSON-RPC 2.0 as NDJSON over stdio), built on
the `aui` library at /Users/latekaapi/Projects/agentic-ui (path dependencies; gpui-pre 0.3.3
+ gpui-kit 0.6; library changes go on agentic-ui branch `muse-support`, never main).

Read first, in order: docs/00-spec.md (frozen spec: crates, decisions, keymap, five phases
with gates), docs/CHANGELOG.md, docs/01-transport.md (Phase 1 findings), docs/02-app.md
(Phase 2), docs/03-composer.md (Phase 3), agentic-ui/docs/10-muse-research.md §1, §3, §4, §7 (wire contract, auth,
what is not on the wire, adapter mapping), agentic-ui/docs/00-agent-brief.md, and your memory
file project-harness-muse-slice-2026-09-08.

State on 2026-09-09:
- Phase 1 DONE and reviewed: harness commit 0d6417a (crates/muse-client transport + typed
  schema, crates/muse-adapter `MuseFold` + `SideState`, replay tests over fixtures/msp/*.jsonl
  with snapshots in crates/muse-adapter/tests/snapshots, `live_echo` ignored test,
  `harness-probe` binary); agentic-ui `muse-support` 3572bb9 (aui-protocol extensions:
  Provider::Muse, four MSP approval modes + Session::plan, new Delta/Block/Marker/Intent
  variants). Gates green in both repos.
- Phase 2 DONE and reviewed on 2026-09-09: harness fd04244 (crates/harness boots on the aui
  shell, auth probe + device-code login, sessions sidebar with view/page backfill, live
  streaming of real turns, stop/retract, reconnect, dialog; scripting flags `--session
  <id>|latest`, `--send <text>`, `--no-connect`, env `HARNESS_PROVIDER=echo`); agentic-ui
  `muse-support` 857ddb1 (`aui::screens::login`, `aui::overlay::dialog`, `Provider::Muse`
  mark, `SidebarFooter::detail`, assistant footer reasoning tokens). Gates green; screenshots
  in docs/images/phase2-*.png; docs/02-app.md describes the app. Two more wire facts:
  `session/list` matches `workspaceRoot` by exact string (canonicalize `/tmp` → `/private/tmp`),
  and a new session appears in `session/list` only after its log flushes (sidebar refreshes on
  `turn/completed`).
- Phase 3 DONE and reviewed on 2026-09-09: harness bb964b4 plus the review commit
  3452b02 (composer controls: model /
  effort / mode pickers, context meter + compaction, queue strip with steer,
  `@` mentions and the `/` menu with skills, client-side plan mode, prompt
  history, images, the `Overlays` entity, toasts, and scripting flag `--steps`);
  agentic-ui `muse-support` 21dcccb (`composer::{model_menu, effort_menu,
  mode_menu}`, `data::context_meter`, `composer::queue_strip`, the composer's
  `.context()` / `.context_open()` / `.plan()` / `.chip_menu()` slots, new
  `ComposerIntent::{Steer, ExitPlan, Attach, Compact}` and `QueueIntent::Steer`,
  gallery entry `composer/pickers`). Gates green in both repos; screenshots in
  `docs/images/phase3-*.png`, light and dark; `docs/03-composer.md` describes it
  all. Spend: **25 real `meta` turns** — the screenshot runs were started
  without `HARNESS_PROVIDER=echo`; the lead reported one. Scripted runs
  (`--screenshot`/`--steps`/`--send`) now default to `echo`; verify spend from
  `~/.local/share/muse/session-index.db` (`provider_id`, `workspace_root`), never
  from a lead's report.
- Phase 3 findings: `/plan <text>` **does** fire the bundled plan skill
  server-side (evidence in `fixtures/msp/transcript-plan-probe.jsonl`);
  `reasoningEffort` is on `turn/start` and `turn/steer` only and is never
  reflected back, so the effort chip is the one client-side value; an `image`
  part is admitted without being decoded; `session/setModel` on echo is rejected
  `invalid_target`. Review findings F1, F4 and F5 are closed; F2 and F3 remain
  for Phase 4.
- Review findings from the Phase 3 screenshots, to fix in Phase 4 (not blocking):
  6. The plan card numbers markdown headings as steps alongside list items
     (`plan::steps`); headings should become section labels, list items the steps.
  7. `/plan` appears twice in the `/` menu (client command + bundled skill); hide a
     skill when a client command shares its name.
  8. The model menu truncates labels (`muse-spark-1...`); widen it or wrap.
- The Phase 3 brief that worked is kept at docs/briefs/phase3-brief.md; write the Phase 4
  brief in the same shape (read list, scope, numbered decisions, wire facts, deliverables)
  and save it as docs/briefs/phase4-brief.md.
- Phases 4 (approval card v2 multi-stage + feedback + policy and judge
  resolutions, question previews/timeout/clarify, error banners/dialogs, retry
  and retry-scheduled, markers, fork, todo, goal — plus findings F2 and F3) and
  5 (polish, motion, focus, docs, CI, and the `/name` `/resume` commands that
  currently only toast) are not started. Each is one Opus lead session briefed
  with the spec sections named in docs/00-spec.md §5; the owner reviews at each
  gate on screenshots and the diff.

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
reads the screenshots. SendMessage is not available to the main session: if a lead dies
mid-phase (rate limit), launch a fresh lead told to inventory the uncommitted trees first.
Spend rule after Phase 3: the brief must tell the lead to prefix EVERY app invocation with
`HARNESS_PROVIDER=echo` and to name each real turn before spending it; at the gate, count
real turns yourself with
`sqlite3 ~/.local/share/muse/session-index.db "select workspace_root, first_user_prompt from sessions where session_dir like '%/<date>/%' and provider_id='meta'"`
and `grep -c runtime.user_intent.accepted <session_dir>/session.jsonl` per session. Prefix shell commands with
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
