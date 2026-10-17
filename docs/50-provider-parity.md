# 50 — Provider parity: Muse becomes one provider among three (plan, 2026-10-16)

Status: **plan; owner decisions taken 2026-10-16 (§6).** No code written. Evidence: a read-only audit of the working tree on 2026-10-16
(it includes the other session's uncommitted permission-mode diff) and a fresh read of the competitors (t3code
`cfafb4bb5`, synara `f2614e58`, bb `01affc19`, paseo `c1558d2`, orca `e52298621`, emdash `79f37d920`, zed `5504e87`,
all pulled 2026-10-16 under `~/Sandbox/baaz/orchestration-research/repos/`). Every file:line below was read, not run.

## 1. What is wrong today

Claude Code and Codex go through the `provider::Provider` seam: one child per session, `ProviderEvent::Deltas`, a lane
task per view. **Muse does not.** It runs on the original path: the raw `MuseClient`, a global event pump
(`Harness::pump/route`, `app.rs:2091-2335`), `MuseFold::apply(MuseEvent)`, and about 30 MSP methods called directly from
`crates/baaz`. A `MuseAdapter` exists and is wrapped in a `Provider` (`conn.rs:109`), but nothing sends through it, and
every Muse event is folded a second time into deltas nobody reads.

What the owner would see, worst first:

| # | Asymmetry | Evidence | Effect |
|---|---|---|---|
| 1 | **Muse's sign-in gates the whole app** | `Harness.auth` is Muse's account (`login.rs`); sidebar `loading` waits on a Muse `session/list` (`sidebar_view.rs:493,893`, `lifecycle.rs:475`); external URLs parked until Muse signs in (`external_urls.rs:195,224,239`); Esc routes to the Muse login (`app.rs:7127`); footer identity and plan are Muse's (`sidebar_view.rs:918-927`); Muse sign-out clears every provider's rows (`login.rs:250,447,473`) | Without Muse: skeleton sidebar forever, links never open, "Not signed in" while Claude is connected |
| 2 | **Muse always spawns; Muse is the default** | `connect` at boot unconditionally (`app.rs:1835`); a missing Muse = modal + a permanent "Muse is not running" banner on every session (`app.rs:2063,4662,5416`); `--provider` defaults to `"meta"`, and `ProviderId::parse` maps anything unknown to Muse (`main.rs:304`, `providers.rs:71-76`); onboarding's Continue never picks a default (`connect.rs:594`) | A Claude-only person sees Muse errors at every launch, and their first ⌘N opens a dead Muse draft |
| 3 | **Every provider's titles, bylines and handoff summaries run on Muse** | `app/titles.rs:105,145,450,724`, `lifecycle.rs:4065` | No Muse = no titles; with Muse, Claude/Codex prompts are sent to (and billed on) Muse |
| 4 | **Two implementations of every command** | `if self.is_provider_lane()` in submit, steer, interrupt, unqueue, model, mode, compact, models, plan (`session/commands.rs:84…1019`); Muse-only data pulled from raw MSP in `route`; `ProviderEvent` has 5 variants | Every feature is built twice; fixes land on one lane only |
| 5 | **Search and the session list are Muse-index-centric** | palette indexes only Muse's `session-index.db` (`app/find.rs:107`) | Claude/Codex sessions cannot be found by search |
| 6 | **Reconnect only for Muse** | Muse respawns + resumes (`app.rs:2348-2470`); lanes just show a banner (`session/lane.rs:287`) | A crashed `claude` child is a dead session |
| 7 | Detection is asymmetric; Muse's caps are pinned, not read; MCP isolation is Claude/Codex-only; Muse strings in approvals, About, Skills, Settings | `provider_status.rs:890`, `providers.rs:139`, `transcript.rs:42,2013`, `dialogs.rs:1379`, `skills_page.rs:483,1287` | Polish, but it reads as "a Muse app with plug-ins" |

What already exists and is good: the first-run **Connect your providers** screen treats all three equally
(`connect.rs`, docs/23 §3-4); `provider_status.rs` probes in parallel; capability tables per adapter
(`provider-*/src/caps.rs`, native/emulated/unavailable/unverified); the mcp-bridge is neutral; the usage card is mostly
symmetric; Claude/Codex homes are isolated.

## 2. What the best peers do (the bar)

Ranked by what Baaz should take. ★ = who does it best.

- **No first-party privilege.** Every provider goes through the same public API; a doc test keeps the API doc in sync
  with the types. ★bb (`docs/provider-plugin-api.md`, `plugins/provider-*`). Zed is close (native agent is "just another
  `AgentConnection`") but in practice its native agent is richer. **T3 Code and Paseo now both ship a Muse provider.**
- **One semantic event grammar assembled by the host**; unknown payloads are kept visibly (`provider/unhandled`), never
  dropped. ★bb (`thread/delta` v3), Synara (`event.unmapped`, bounded ingress).
- **Capabilities negotiated, only narrowed by the handshake; no ungated call ever sent; each missing capability has a
  fallback** (steer → queue → interrupt-and-restart; model switch in-session/restart/unsupported; fork → portable
  handoff). ★bb, T3, Synara.
- **Side-effect-free status probes through the protocol** (Codex `account/read`; Claude SDK init that never yields a
  prompt), single-flight, a deadline, the last good status kept on failure, cached on disk per instance. ★T3. If a CLI
  probe is used, lock it against OAuth refresh rotation. ★Synara.
- **Binary resolution:** login-shell PATH, `launchctl getenv PATH` fallback, known dirs (~/.local/bin, bun, volta, asdf,
  mise, nvm, fnm, pnpm), a per-provider path override in Settings. ★Synara, T3.
- **Version floors as data** (broken / unsupported / ok bands, per-model `minVersion`). ★T3 `model-manifest.json`.
- **Typed status enum** (not_installed / unsupported_version / unauthenticated / expired / ready / unknown) and **typed
  recovery hints** (authRequired, rateLimited) — never match error text. ★bb.
- **Onboarding:** providers are step one, re-detected on open, one line and one action each (install / sign in /
  update), sign-in in a host-owned terminal whose exit re-probes, an honest "no agent ready — Skip". ★T3, Synara, bb.
  T3's mistake: onboarding lists only Codex + Claude.
- **Import existing native sessions** (`~/.claude/projects`, `~/.codex/sessions`) as resumable threads. ★T3
  (`AgentSessionScanner.ts`), Synara, Paseo.
- **A diagnostic sheet**: PATH used, each resolution step, raw probe output. ★Paseo.
- **Conformance tests + recorded parity runs per adapter.** ★bb (`bridge.conformance.test.ts`, `provider-parity/`).
- **Crash = "Failed to launch" view with stderr + Retry; pending approvals settled on exit.** ★Zed, Synara.
- Anti-patterns to avoid: 7–9k-line adapter files (T3, Synara, Paseo), full-access default (Synara), "install" that
  only writes config (Paseo), availability that ignores auth (Paseo), uncached health (bb).

## 3. Target architecture

```
                 ┌──────────────── crates/provider (the seam, no provider privileged) ───────────────┐
 ProviderRegistry│ ProviderDriver: detect() → ProviderStatus (typed), models(), capabilities()       │
  (one per app)  │ Provider: send(Command) / events() → ProviderEvent (Deltas + Account/Usage/        │
                 │   SessionStatus/TurnSettled/ModelChanged/Unhandled) ; one-shot complete() (utility) │
                 └──────────────┬──────────────────────┬──────────────────────┬────────────────────────┘
                     provider-muse (shared child,   provider-claude-code   provider-codex
                     demux by session_id)           (child per session)    (app-server; shared where possible)
                                │                      │                      │
                 session-store (neutral, out of MuseFold): Delta → aui_protocol::Session, approvals, client blocks
                                │
 crates/baaz: SessionView has ONE command path; Harness has NO Muse fields; login.rs = the Muse sign-in sheet only;
              tier.rs/billing.rs behind Muse's own status; session registry + search cover all three.
```

Rules: nothing in `crates/baaz` names a provider except the registry and per-provider Settings content; a capability
the handshake didn't grant is never offered; a fourth provider (an ACP lane later) is one new crate and no app edits.

## 4. Work, in order (each step ships alone and is gated)

| Step | What | Size |
|---|---|---|
| **P1 · Ungate** | Per-provider status replaces `Harness.auth` for footer, Esc, URLs, sidebar `loading` (settles on local records + connected providers); Muse sign-out clears only Muse rows; Muse spawned lazily when enabled and needed; no modal/banner for a provider nobody uses; default provider chosen from Connected at onboarding and boot (`"meta"` default removed); fallbacks never "Muse". | 3–4 days |
| **P2 · Utility model** | Titles, bylines, handoff summaries through a neutral one-shot `complete()` per provider — default: the session's own provider's cheapest model (owner decision, §6). | 2 days |
| **P3 · One registry + search** | One Baaz-owned session registry for all three (extend `provider-sessions.json` or move to sqlite with perf doc 51 §S); Muse `session/list` becomes one source; search indexes every provider's text, incrementally. | 3 days (shared with doc 51) |
| **P4 · Muse onto the seam** | Extend `ProviderEvent`; demux Muse's shared child per session into lanes; delete `conn::Legacy`, `Harness.client`, `pump/route`, every `is_provider_lane()` fork; move MuseFold's store half into a neutral `session-store`; MSP-only features (queue/unqueue, steer, wizard questions, `view/page` paging, `item/readOutput`) become Commands/capabilities. Regenerate probe baselines on main. | 1.5–2 weeks |
| **P5 · Detection to best-practice** | Protocol probes where possible, single-flight, disk cache with stale-while-revalidate, version floors as data, known install dirs + path override, typed status + recovery hints, install/sign-in as a host-run plan that re-probes, a diagnostic sheet, Muse sign-in probed like the others. | 3–4 days |
| **P6 · Robustness for all** | Reconnect-and-resume for every lane; "Failed to launch" view with stderr + Retry; approvals settled on exit; a conformance suite every adapter must pass (same scripted scenarios, recorded fixtures per provider). | 3–4 days |
| **P7 · Import (owner decision)** | Optional import of existing `~/.claude/projects` / `~/.codex/sessions` as resumable Baaz sessions. | 2–3 days |

Total ≈ 4–5 weeks of build. P1 is the highest value per day and can start first.

## 5. Collision with the running build session

The other session is editing the same files right now (uncommitted: permission modes across all three adapters,
`tier.rs` probe timing, `session/lane.rs`, `session/commands.rs`, `lifecycle.rs`). **P4 must start after that work is
committed**, on a branch cut from it. P1/P2 touch `login.rs`, `sidebar_view.rs`, `external_urls.rs`, `app/titles.rs`,
`connect.rs`, `main.rs` — mostly disjoint, but they should still wait for that commit, then go in a worktree.

## 6. Owner decisions (2026-10-16)

1. **Titles, bylines, summaries:** the session's own provider, on its cheapest model. Never Muse implicitly.
2. **Import:** yes — existing Claude Code (`~/.claude/projects`) and Codex (`~/.codex/sessions`) sessions become
   resumable Baaz sessions (P7). **Muse also gets a Baaz-private home**, like the other two.
3. **Provider screens** (first run, Settings › Providers: typed states, diagnostics, install/sign-in): **redesigned in
   the D5 Fable session**; the build follows that spec.
4. **Start now:** performance M + P0 in a worktree (`perf/measure-p0`); provider work after the running session commits.

## 7. Progress

**P1 + P2** merged 2026-10-17 (main 5d2c755).

**P3 · one registry + search (2026-10-17, branch `foundations/m-p3`).** What was already one path, checked before
building: every Claude Code / Codex session Baaz opened is in `provider-sessions.json` (W5), listed in the sidebar beside
Muse's `session/list` rows, and every row — sidebar click, `⌘K` Resume, search hit — opens through `Harness::resume`,
which routes a provider record to `reopen_provider` and a Muse id to `session/resume`. What was not: **search**. The
palette's `search.db` was fed only from Muse's index, so a Claude/Codex session was findable only as a member of a
handoff chain with a Muse session, and only by its title.

Now:
- Each adapter says what its stored transcript's searchable words are — `provider_claude_code::history::search_text`
  (user prompts + assistant text; no tool calls/results, thinking, `isMeta`, `<command-…>` blocks) and
  `provider_codex::rollout::search_text` (user/assistant `response_item` messages; no developer instructions, tool
  output or injected `<environment_context>`), both capped at 256 KB — and how to find them
  (`history::stored_transcripts`, `rollout::stored_rollouts`: one walk per home).
- `crate::provider_search` reads them for the sync, Baaz's own homes first, the owner's `~/.claude` / `~/.codex` only
  for ids not found there (legacy unmigrated sessions).
- `search_sync_rows` gives every provider record its own row (label ladder: rename → generated title → handoff title →
  ack title → first prompt), chains still collapse under their head whatever provider each member is, and the
  fingerprint covers a record's turn count + stamp — so a settled exchange rewrites that one row (the settle now
  triggers the sync) and every other row is skipped. The P0 incremental sync is unchanged.
- Measured: 120 transcripts' words read in 23 ms (debug), budgeted in `scripts/perf-budgets.py`.
- Tests: adapter extraction (both), home walks (both), Baaz dispatch + owner-home fallback, and a Harness test that
  syncs Muse + Claude + Codex rows into one `search.db` and finds each by its own transcript words.

Not done in P3 (still P4): Muse's `session/list` is still the Muse half of the list, and Muse rows are not in Baaz's own
registry file; the registry is two stores read as one, not one store.
