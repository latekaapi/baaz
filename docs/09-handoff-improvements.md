# Handoff — the Muse Code chat slice, complete; next session starts improvements and fixes

Written 2026-09-09 by the design/review session (Fable) after Phase 5 landed and
`muse-support` was merged into agentic-ui `main`. This is the long-form record:
what was built, every decision and why, what it cost, what is known to be
wrong or unverified, and how to work on it. `docs/05-handoff.md` is the short
maintenance version; read this one first when starting the improvements session.

---

## 1. State of both repositories

| repo | branch | head | note |
|---|---|---|---|
| `~/Projects/harness` | `main` | see `git log` (2026-09-13: owner round, follow-up) | the app; five phases, the improvements/audit/transcript passes, the owner round; `muse` CLI 1.1.1 |
| `~/Projects/agentic-ui` | `main` | `b1850d5` | every branch through `owner-followup-2026-09-13` merged; gates green on `main` |

(The table as first written on 2026-09-09 read `ac6a20e` / `e8538d1`; the sections below
still describe that state where they name commits. Later passes are in `docs/CHANGELOG.md`
and `docs/diagnosis/`; §12 is the next feature.)

Neither repository has a git remote. The harness CI workflow
(`.github/workflows/ci.yml`) checks out `latekaapi/agentic-ui` beside it and
has therefore never run. First push exercises it.

Harness commits, in order: `acc675f` spec → `0d6417a` Phase 1 → `fd04244`
Phase 2 → `bb964b4` + `3452b02` Phase 3 → `7aaa275` Phase 4 → `4e72b32` billing
guard → `ac6a20e` Phase 5. Library commits: `3572bb9` protocol → `857ddb1`
login/dialog/mark → `21dcccb` composer controls → `c6af578` approval/question
cards → `b73f727` + `e8538d1` sidebar operations.

## 2. What it is

A one-window macOS chat interface to Meta's Muse Code agent (`muse` CLI
1.0.3). `muse serve` speaks JSON-RPC 2.0 as NDJSON over stdio ("MSP"); the
harness spawns one child per app process and multiplexes sessions on it. Three
crates:

```
crates/muse-client    transport: child process, framing, typed schema for all 186 msp.d.ts types,
                      server-request handling, view/gap splice-fill, MUSE_CAPTURE wire recording
crates/muse-adapter   MuseFold: MSP view events -> aui_protocol::Delta + SideState; failure::humanize;
                      tool_shape; block ordering by log sequence; replay + parity tests
crates/harness        the gpui app: Harness / SessionView / Overlays entities, auth, sidebar, transcript,
                      composer, tier probe, local stores, --replay and --steps scripting
```

The UI is the `aui` library (gpui-pre 0.3.3 + gpui-kit 0.6) by path
dependency. Everything visible is a library component fed data and returning
intents; the app does the I/O.

## 3. What was built, phase by phase

**Phase 1 — transport and fold** (`docs/01-transport.md`). NDJSON framing,
frame classification on shape (server requests carry their own id space and are
never answered with a JSON-RPC result), UUIDv7 command ids, the ordering rule
(view events may precede an ack; the ack is admission, not outcome),
`view/gap` splice-fill on its own thread, reconnect by respawn + `initialize` +
`session/resume` with the last cursor. `MuseFold` maps items to blocks per
research §7.2 with `SideState` for what `Delta` cannot carry. Replay snapshots
over every capture.

**Phase 2 — shell, sessions, streaming** (`docs/02-app.md`). Boot as
`aui/examples/minimal.rs`; auth probe (`auth.json` has `providers.meta` and
`model/list` reports `providerCatalog`); login screen driving `muse login` with
`MUSE_LOGIN=1` and an SGR-stripping stderr parser; sidebar from `session/list`
filtered to the canonicalized workspace and enriched from the read-only index;
resume with `excludeItems` + `view/page` forward; streaming markdown, thinking,
tool cards, per-turn token footer; stop with retract; error banner/dialog split.

**Phase 3 — composer controls** (`docs/03-composer.md`). Model/effort/mode
pickers on the plus-menu primitives; context meter with pressure states and
compaction; queue strip with Edit/Remove/Steer, never reordered optimistically;
`@` mentions from an `ignore`-crate walk; `/` menu with client commands and
`muse skills list` skills; plan mode via the bundled `/plan` skill (probed, it
fires server-side) under `denyUnmatched`; prompt history in
`~/Library/Application Support/harness/history.json`; images decoded before
send; the `Overlays` entity; toasts; `--steps` scripting.

**Phase 4 — approvals, questions, errors** (`docs/04-approvals.md`). Approval
card v2: server-minted choices, stage strip `n/N` with argv, protected-write and
judge-escalated badges, app-owned feedback slot, policy/judge resolutions that
are never actionable, digits 1–9; `approval/decide` with the current
`requirementId`, re-rendered only from `approval/updated`/`resolved`. Question
card: header, per-option previews, timeout countdown, Skip → `userInput/cancel`,
Explain instead → `userInput/clarify`, answers keyed on labels. F2 humanized
failures, retry, retry-scheduled row; every marker kind; fork; todo and goal
cards; generic item card; `--replay <capture>`; `!cmd` → `session/userShell`.

**Phase 5 — billing guard, session operations, polish, docs, CI**
(`docs/06-billing.md`, `07-architecture.md`, `08-keymap.md`, `README.md`).
`tier.rs` drives the `muse` TUI in a pty and reads `/upgrade`; the plan sits in
the sidebar footer; pay-as-you-go blocks sending behind a banner. `sessions.json`
holds rename/hide/derived titles; ⌘⇧F search; `/resume` and ⌘K on the palette;
empty states with suggested prompts; focus and motion pass; CI workflow.

## 4. Decisions taken, and why

Numbered so the next session can cite them. Spec sections in parentheses.

- **D1. Separate repo, path dependencies** (§2). The harness is not a crate in
  agentic-ui; library gaps go into agentic-ui as their own commits. Keeps the
  library reusable and the gates independent.
- **D2. Three crates, `muse-adapter` has no gpui** (§2). The fold is pure and
  testable by replay; that is what made the offline parity test possible.
- **D3. Fold on the UI thread, in wire order; every command on a background
  task** (02-app §3). A frame always renders a consistent transcript; the UI
  thread never waits on the pipe.
- **D4. Nothing is optimistic.** Chips, queue rows, approval cards and mode
  markers move only on the server's notification. The one client-authored block
  is the plan card (`append_client_block`).
- **D5. User turn keyed on the `userMessage` item id, assistant turn on the MSP
  turn id** (01-transport §4.10). `turnId == commandId` for a fresh turn, so
  keying both on the turn id collided. Assistant turns are created lazily by
  their first block so the reply never lands above the prompt.
- **D6. Blocks are placed by `sourceRange.first.sequence`, not arrival**
  (Phase 4, F3). Live streams see `item/started` early; backfill sees only
  `item/completed`. Without sequence ordering an approval landed above the shell
  item it gated on backfill. Expressed as append plus rotating `BlockUpdated`s
  because `Delta` has no insert.
- **D7. Plan mode is `/plan <text>` under `denyUnmatched`** (§3.1). One real
  turn proved the bundled skill fires server-side (`transcript-plan-probe.jsonl`).
  The spec's preamble survives behind `HARNESS_PLAN_PREAMBLE=1`.
- **D8. Reasoning effort is the one client-side chip** (03-composer). The wire
  never reflects it; the chip says so.
- **D9. Approval mode is the four MSP modes; plan is a client flag** (§3.6).
  `PermissionMode` in aui-protocol was replaced accordingly.
- **D10. Approvals live in the transcript with a needs-you banner**, not docked
  over the composer (§3.3). The library's design was kept over the reference
  apps'.
- **D11. Choices are never cached.** They change between stages; the card
  renders from the latest `approval/updated`. `requirementId` must equal
  `currentRequirementId` or the wire answers `approvalRequirementStale`.
- **D12. Stateless cards, app-owned editors.** Feedback and clarify text fields
  are slots the app fills with a gpui-kit input it owns, the same pattern as the
  composer. Cards never own text.
- **D13. `Block::Plan.sections` is additive** (F6). Headings become section
  labels; only list items are numbered.
- **D14. Muse's storage is read-only; the harness's own state lives under
  `~/Library/Application Support/harness`** (§3.7): `history.json`,
  `sessions.json` (name, hidden, derived_title), `tier.json`. Written atomically.
- **D15. Titles for shell-only sessions are derived** (F10). Muse's index writes
  the literal string "New session" as a title; it is rejected and the first shell
  command is used.
- **D16. Errors branch on `error.data.kind`, never on the message** (§3.8,
  research §1.14). Auth failures are the one message-shaped classification,
  deliberately narrow.
- **D17. `--replay` is the screenshot and test tool.** It opens a capture with
  no server. Synthetic captures are named `synthetic-*` and say which lines are
  real.
- **D18. Every scripted run defaults to `echo`** (Phase 3 review) — retained,
  but see D19: echo is a route, not a discount.
- **D19. There is no free provider on this machine; leads spend zero turns.**
  See §6.
- **D20. The billing tier is probed through the TUI** because nothing on the
  wire exposes it (§6). Cached against `auth.json`'s mtime; a failed probe never
  blocks boot; pay-as-you-go blocks sending until "Send anyway".
- **D21. One Opus lead per phase from a self-contained brief; Fable designs and
  reviews only.** Briefs are kept in `docs/briefs/`. The review counts spend from
  the session logs, reruns one gate, reads the screenshots, and never trusts the
  lead's report for spend.

## 5. Wire facts that were expensive to learn

All in `docs/01-transport.md` §4 and the CHANGELOG; the ones that bite:

- `muse serve --no-session-log` accepts turns and emits no view events. Always run durable.
- An approval's `itemId` is its own id; a user-shell approval's `turnId` is the shell's `commandId`.
- `onRequest` raises no approval for a model-issued `ls`; only `promptUnmatched`/`denyUnmatched` do.
- Five schema-optional fields are always on the wire, sometimes `null` (modelled required-nullable).
- `ReasoningEffort` has `ultra` and no `max`; `max` is `invalidParams`.
- `model/list` ignores `providerId`; `cost` is `null` on every catalog row.
- `session/list` matches `workspaceRoot` by exact string; canonicalize `/tmp` → `/private/tmp`.
- A new session appears in `session/list` only after its log flushes (`turn/completed`).
- `session/setModel` on an echo session → `commandRejected: invalid_target`; `session/compact` on a fresh session → `missing_run`.
- An image part is admitted undecoded; the app decodes before sending.
- `session/start`'s `approvalMode` raises no `approvalModeChanged`; the chip is seeded from the start result.
- `session/read` on a session no host has loaded serves no history.
- `session/setApprovalMode` cannot reach `promptUnmatched` on this server; `session/start` can.

## 6. The two incidents

**Spend.** The spec capped real turns at five per phase. Actual, counted from
`grep -c runtime.user_intent.accepted` over every `session.jsonl`: Phase 1 ~6,
Phase 2 ~4, Phase 3 25, Phase 4 6, Phase 5 0; 46 in total. Phase 3's overrun
came from screenshot runs that omitted `HARNESS_PROVIDER=echo`; Phase 4's from
leads running the ignored live tests. The research doc's §2.4 claim that echo is
free was wrong: on a signed-in machine `providerId: "echo"` is recorded at start
and then a metadata record switches the run to `muse-spark-1.3-contributor`, with
response ids and reasoning tokens. Even the Phase 1 echo capture bills tokens.

**Billing tier.** Muse has two credential tiers, Pay-as-you-go and Subscription,
decided from the login token. The owner's 2026-09-08 login token was on
pay-as-you-go, so everything through Phase 4 was billed as API usage although the
app only ever used the account login (launcher logs: `credential.status
source="login"` for all 112 serve runs). A logout and re-login at 14:46 on
2026-09-09 moved the token to the "Muse Code High Usage" plan. The Phase 5 guard
exists so this is visible next time.

## 7. Known limitations and unverified paths

- **Live approvals cannot be minted on this Mac.** Muse's managed shell sandbox
  reports itself unavailable, so a `userShell` under `promptUnmatched` fails
  before an approval is raised. Stage-1 approval screenshots come from a
  truncated real capture. Under `denyUnmatched` policy refuses first, so that
  path still works live.
- **Clarify was verified against a synthetic capture only**
  (`synthetic-userinput-clarify.jsonl`); the answer path is real.
- **Retry-scheduled** and **todo/goal** come from synthetic captures; no live
  capture carries those events.
- **`session/read` derivation of titles** is untested against real data (see §5).
- **CI never ran.**
- **`cargo tree -d`** lists `gpui-pre-collections` twice at the same version;
  pre-existing, harmless.

## 8. Candidate improvements for the next session

Ordered by what the owner is likely to notice first.

1. **Sidebar noise.** Screenshot runs leave many empty sessions; consider hiding
   zero-turn sessions by default, or a "Clear empty" action over `sessions.json`.
2. **Approval card width.** "Always allow in this workspace: …" wraps well now;
   check very long argv stages and `argv_complete == false` rendering.
3. **Reasoning display.** A turn can bill reasoning tokens with no reasoning
   item; the footer shows the count but nothing says "thought silently".
4. **Fork from an arbitrary completed turn** works from the turn action; from
   `/fork` it takes the newest. A picker would match the TUI.
5. **Question groups.** N questions in one request are answered together; the
   "n of N" affordance is minimal.
6. **Tier probe robustness.** It parses the TUI's card text; a wording change
   breaks it silently into `Unavailable`. Pin the two sentences in tests (done)
   and consider surfacing "probe stale" after 24 h.
7. **Right pane.** Spec §1 names diffs, terminal and browser as the next slice;
   the shell keeps the slot and `ToggleRightPane` is wired to nothing.
8. **Subagent and workflow cards** are generic; `childSessionId` allows drill-in.
9. **Library `main`** now carries Muse-specific components; a pass to make
   names provider-neutral where they are (`ApprovalChoice` etc. already are).

## 9. How to work on it

```sh
export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"
cd ~/Projects/harness
cargo run -p harness -- --print-tier                          # which plan the login is on
cargo run -p harness -- --replay fixtures/msp/transcript-approve.jsonl --theme dark
cargo run -p harness -- --no-connect --screenshot /tmp/login.png
cargo run -p harness -- --workspace ~/code/thing              # a real session (billed per turn)
cargo test --workspace                                        # replay + parity + unit, no child
UPDATE_SNAPSHOTS=1 cargo test -p muse-adapter                 # regenerate, then READ the diff
```

Flags: `--workspace --provider --theme --session <id>|latest --send --steps
<a;b;c> --screenshot --screenshot-delay --no-connect --replay --tier
subscription|payg|unknown --print-tier --approval-mode <mode>`. Environment:
`HARNESS_PROVIDER`, `HARNESS_MUSE`, `HARNESS_PLAN_PREAMBLE`, `MUSE_CAPTURE=<path>`
(records the wire in fixture format).

Spend rule: anything that reaches `turn/start` is billed on the login's tier.
Free: `--replay`, `--no-connect`, `session/start`, `session/userShell`,
`approval/*`, `userInput/*`, `session/fork`, `session/list`, `view/page`,
`model/list`, the TUI opened without a prompt. Count from the logs:

```sh
grep -c runtime.user_intent.accepted ~/.local/share/muse/sessions/*/*/*/*/session.jsonl | awk -F: '{s+=$2} END {print s}'
```

Gates before every commit — harness: build, test, clippy `--all-targets -D
warnings`, rustdoc `-D warnings`, one `gpui-pre` and one `gpui-kit` in `cargo
tree -d`, snapshots regenerated and read. Library: the same plus the
all-features build (`aui-webview/wry,aui-terminal/pty,aui-terminal/tui`),
`python3 scripts/api-doc.py`, and a gallery entry for anything new. Library rules
(`agentic-ui/docs/00-agent-brief.md`): no literal colours/sizes/durations,
stateless `RenderOnce` components with intents out, `popover_layer` for
overflow, both themes. Commit messages end with
`Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`. Do not touch
`~/Projects/cockpit`.

Working model that held up: one Opus lead per phase from a self-contained
brief (`docs/briefs/phase{3,4,5}-brief.md` are the shape); the review session
counts spend itself, reruns one gate, reads the screenshots. Leads stall on the
harness's 600 s watchdog roughly once per long phase; a fresh lead told to
inventory the uncommitted trees first recovers cleanly, because leads leave
buildable trees.

## 10. Pointers

- Spec: `docs/00-spec.md` (frozen 2026-09-08). Per-phase docs `01`–`04`, `06`–`08`. Changelog with every finding: `docs/CHANGELOG.md`.
- Research: `~/Projects/agentic-ui/docs/10-muse-research.md` — §2.4 is wrong about echo; everything else held.
- Schema: `fixtures/msp/msp-ts/msp.d.ts`, `fixtures/msp/msp/`. Captures: `fixtures/msp/*.jsonl`; the `probe*.py`/`run*.py`/`harness-probe` clients that sent turns were removed 2026-09-12 (git history has them) — `drive.py` and `make-stress-300.py` remain, header-commented with what each sends and costs.
- Library API overview: `~/Projects/agentic-ui/docs/06-api.md`; agent brief `docs/00-agent-brief.md`.
- Memory for this project: `~/.claude/projects/-Users-latekaapi-Projects-agentic-ui/memory/` (`project-harness-muse-slice-2026-09-08`, `muse-echo-not-free`).

## 11. Prompt for the improvements session

```
You are starting the improvements-and-fixes pass on the Harness, a macOS gpui chat interface
to Meta's Muse Code agent at /Users/latekaapi/Projects/harness (branch main), built on the aui
library at /Users/latekaapi/Projects/agentic-ui (branch main; muse-support is merged). Read
docs/09-handoff-improvements.md in full first, then docs/05-handoff.md, then whichever phase
doc your change touches. Spend rule: there is no free provider; count turns from the session
logs; use --replay for screenshots. Prefix shell commands with
export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH". Library changes go on
a new agentic-ui branch off main. Gates before every commit as §9 lists. One lead per work
package from a self-contained brief; the main session designs and reviews.
```

## 12. Next: Projects — one window over several workspaces (brief for the next session)

Today the harness is **one window, one workspace**: `--workspace` (or the launch directory)
is canonicalized once, `session/list` is filtered to it, `session/start` names it, the `@`
index walks it, and the sidebar is that workspace's sessions. The owner wants **Projects**:
the app remembers the workspaces it has been opened on, shows them as first-class objects,
and lets a person move between them without relaunching.

**What the wire gives, already known.** `session/list { workspaceRoot }` matches the string
exactly (canonicalize `/tmp` → `/private/tmp`); `session/start { workspaceRoot }` opens a
session anywhere; one `muse serve` child multiplexes sessions across workspaces (the
client is per process, not per workspace); `session/resume` re-attaches any listed
session; the index (`~/.local/share/muse/session-index.db`, read-only) carries
`workspace_root` per session. Skills and rules are per workspace (`muse skills list` runs
in a directory). Nothing on the wire is a "project": it is the harness's own object.

**What the harness already has.** The rail's `RailItem::nav("workspace", …)` slot and the
library's `Sidebar` workspace switcher row (`aui::nav::sidebar` handles `"workspace"` in
`on_action`) — both wired to nothing. `Harness::workspace()` is one string read everywhere
a call needs it (`load_sessions`, `new_session`, `load_menu_sources`, the search index's
`files_seen`, `files::walk`). The stores under `~/Library/Application Support/harness`
(`sessions.json`, `history.json`, `layout.json`, `search.db`) are keyed by session id or
global, not by workspace; `history.json` and `layout.json` would stay global,
`sessions.json` already keys by session id and needs nothing.

**Decisions to take (recommendations in brackets).**

1. *What a project is.* [A canonical workspace path plus a display name (folder name by
   default, renameable) and a colour/initial; stored in a new `projects.json` under the
   harness's Application Support dir, most-recently-opened first; nothing on Muse's side.]
2. *Switching.* [The sidebar header's workspace switcher (the library row already exists)
   lists projects newest-first with "Open folder…" (an `NSOpenPanel` through gpui's
   `prompt_for_paths`) at the bottom; ⌘⇧O opens it. Switching swaps `workspace()`, re-reads
   `session/list`, re-walks the `@` index, reloads skills, and keeps the `muse serve` child.
   The MRU of parked session views stays valid across projects because views are keyed by
   session id.]
3. *Sidebar shape.* [One project at a time, not all projects at once: the list stays the
   current project's sessions, grouped as now; the project's name sits in the header. A
   "recent across projects" section is a later step. The collapsed rail keeps its titled
   session tiles for the current project.]
4. *New session.* [Always in the current project; `session/start { workspaceRoot }` as now.]
5. *Search.* [`search.db` grows a `workspace` column on `sessions_fts` (rebuild is already
   a full rewrite); the palette searches the current project by default with a "all
   projects" toggle later.]
6. *Boot.* [`--workspace` still wins; without it the last-opened project; with none the
   launch directory, which becomes the first project.]
7. *Library.* [The switcher popover is the existing `view_menu` shape; a `ProjectRow` for
   the picker (name, path, session count, last activity) is the one new component and needs
   a gallery entry. No new colour or size.]

**Build order.** (a) `projects.rs` store + canonicalize + tests; (b) `Harness::workspace()`
becomes the current project and every call site is audited (grep `workspace()`); (c) the
switcher UI on the existing rows; (d) `--steps project:<path>` and `projects` verbs so a
screenshot is reproducible, plus captures in both themes; (e) search scoping; (f) docs
(`02-app.md` §sidebar, `08-keymap.md`, changelog). Free throughout: `session/list` and
`session/start` cost nothing; never send a turn to prove a switch.

**Watch for.** `session/list` matches the string exactly — canonicalize on the way in and
never compare uncanonicalized paths; a session opened in project A and listed while B is
current must not show (filter on the session's own `workspace_root`, not the current
one); the `@` walk is capped at 5 000 entries per project; the tier probe and login are
global, not per project.

