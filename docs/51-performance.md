# 51 — Performance: measure, then make Baaz instant (plan, 2026-10-16)

Status: **approved 2026-10-16; M + P0 in progress on `perf/measure-p0`.** No code written. Evidence: static audits of Baaz + agentic-ui (2026-10-16),
sqlite measurements on **copies** of the real databases (`/private/tmp/claude-501/perf-audit/`), Zed's source
(`zed@5504e87`) and the peers' code and issue trackers. Live numbers (first frame, switch, send) have **not** been
taken yet — step M below does that first.

## 1. The bar

| Metric | Peers (measured / reported) | Baaz target |
|---|---|---|
| Cold session open | Synara 280–330 ms (own report); Zed agent threads stall 30–85 s on long threads (#64883, #43772, #57574) | **< 50 ms any length** (tail first) |
| Switch back to a session | Synara 64–113 ms | **one frame (≤ 16 ms)** |
| Stream → pixel | peers add 30–300 ms (coalescers, Zed's 200 ms reveal buffer) | **≤ 1 frame after stdout** |
| Git refresh | peers poll every 10–60 s | **≤ 100 ms after a file event** |
| Idle | Orca idle renderer 90 % CPU (#6655) | **< 0.5 % CPU, 0 frames at rest** |
| Memory per open thread | Codex desktop 1–8 GB, Zed 0.6–2.3 GB | **flat; < 300 MB app without CEF** |

## 2. What is in the way (ranked)

Measured = timed on a db copy. Everything else is read from code (file:line) and must be confirmed by step M.

| # | Problem | Evidence | Hurts | Fix | Size |
|---|---|---|---|---|---|
| 1 | **The full-text index is rebuilt from scratch about twice per finished turn**, and on every rename/pin/title/byline | `rebuild_search_index` from `load_index` (`lifecycle.rs:386`) and `settle_overrides` (`app/list.rs:78-84`); `DELETE FROM sessions_fts` + re-insert all (`search.rs` `rebuild_sessions`). **Measured: 34 s wall, 13.6 s CPU, ~1 GB journal per rebuild**; `search.db` has grown to 1.03 GB. UI thread clones 210 MB of `search_text` first (`app/find.rs:137`) | idle CPU, disk, search, jank | Incremental upsert of changed sessions only; label edits = one-row UPDATE; WAL + `synchronous=NORMAL`; one long-lived connection on a writer thread; VACUUM once | M |
| 2 | **Boot and every turn read 210 MB of `search_text`; the sidebar waits for it** | `index::read` (`index.rs:115-135`) over Muse's 703 MB index (+1.38 GB WAL); `lifecycle.rs:362-366`. **Measured: 5.8 s cold vs 0.47 s without the column** | startup, RSS (+210 MB) | Select label columns only; `WHERE updated_at_us > ?` on refresh; paint the sidebar from a cached snapshot on frame 1 | S |
| 3 | **CEF (Chromium) initialises on every launch, then pumps at 30 Hz forever; webviews poll at 20 Hz each, hidden or not** | `main.rs:825` `warm_up_soon` → `CefInitialize`, ~182 ms main-thread block (spike S1) + helper processes; repeating 1/30 s `CFRunLoopTimer` (`aui-cef/src/shim.m:57,69-75`); `aui-webview/src/view.rs:248-251` | startup, RSS, idle wakeups (defeats App Nap) | Warm only on Browse/browser intent or after idle; one-shot timer at CEF's requested delay; no polling while hidden | S–M |
| 4 | **Any looping animation re-renders the whole window at display rate** — gpui dirties every ancestor (`gpui-pre window.rs:2148`), so the terminal cursor blink, the sidebar running dot, spinners and the braille/shimmer progress (which keeps running while a session sits on "Waiting for approval…" for hours) each re-run `Harness::render` 60–120×/s — worst case: **while any agent terminal block is running (a dev server, `cargo watch`) the transcript requests a frame every frame, forever**, each pulling that block's whole output out of the grid (`session/render.rs:236-241,538-578`) — and with it `sync_right_cache` (→ git every 2 s, #8), `visible_sessions`, the right-pane clones (#9) and a LaunchServices IPC in `on_frame` | `aui-terminal/src/grid.rs:2308` (inside `app.rs:3609`); `sidebar.rs:850`; `session/render.rs:1447,2598`; `glyph.rs:112`; `status.rs:216`; `tool_card.rs:545`; `default_browser.rs:192` | idle CPU; multiplies every per-frame cost | Animated elements in their own `Entity` views; ambient loops sampled from one 10–15 Hz timer (`pulse_phase` exists, unused); 530 ms caret timer; static glyph while waiting on the person | M |
| 5 | **Polling loops forever**: 15 ms terminal-service drain, 8 ms sleep per terminal tab, 50 ms accept loop, new socket per tool call | `app.rs:1725`, `grid.rs:432-439`, `terminal/service.rs:101,661`, `mcp-bridge terminal.rs:159` | idle CPU, App Nap, +8 ms per keystroke echo, +0–65 ms per agent terminal tool call | Event-driven wakeups; blocking accept; one persistent bridge connection | M |
| 6 | **Streaming cost grows with the session**: whole render cache diffed per delta; root re-rendered per Muse event; markdown re-parsed whole per chunk; O(N) `turn_mut`; fold-plan and syntax caches `clear()` when full | `session/render.rs:262-279,881-967`; `app.rs:2335`; `aui/transcript/markdown.rs:983-1010`; `aui-protocol/src/session.rs:59`; `transcript.rs:945-975`; `syntax.rs:233-260` | stream frame time, latency on long sessions | Dirty turn ids from the fold; `Rc<Turn>`; drain events once per frame; settled-prefix markdown; LRU caches; `.cached()` settled rows; `remeasure_items(i..i+1)` only | ~3 days |
| 7 | **Opening an uncached session**: Muse pages *forward from the oldest event*, 1,000/page, folded on the UI thread; Claude/Codex run `--version` on every open, read the jsonl twice, paint nothing until the child spawns; MRU holds 8 | `session/events.rs:292-418`; `provider-claude-code/src/lib.rs:1016,1047,1114`; `provider-codex/src/lib.rs:776`; `lifecycle.rs:11` | session switch | **Tail first** (`view/page` Backward exists, `muse-client/src/schema/view.rs:449`), fold off-thread, older pages on scroll; stored history painted before the child; memoised versions; bigger MRU for child-less views | 2–3 days |
| 8 | **Git: six serial forks every 2 s, polled from render**, timed-out children leaked, no `--no-optional-locks`, uncapped diff that vanishes past 2 s | `right.rs:1207,1417-1440,503`; `app.rs:3229,3247-3269,6217` | git view latency, idle CPU, lock contention with the agent | One `status --porcelain=v2 -z --no-optional-locks` + `diff --numstat` in parallel; lazy capped per-file diff; FSEvents watcher (100–200 ms debounce); keyed, deduplicated git jobs (Zed `git_store.rs:884,10261`); compare before notify | M |
| 9 | **Right pane deep-clones its cache every render** (up to 1 MB preview twice, re-parsed markdown, 3 full-text scans) | `right.rs:276-310,~2496,~2519,1119,~3015`; `aui/transcript/code.rs:587,729,748` | git/files view jank | `Arc`/`SharedString`; precompute at load | S |
| 10 | **UI-thread disk work**: uncached `realpath` per row in `rejoin`; ~57 synchronous JSON store writes (110 KB pretty `sessions.json`) incl. the send path | `lifecycle.rs:1131-1153` (cached variant `projects.rs:558` unused); `sessions.rs:192`, `lifecycle.rs:4046,4139` | switch, send jitter | Use the cache; one debounced background writer per file (pattern exists: `schedule_overrides_write`) | S |
| 11 | **Perceived latency**: rows open on mouse-up; Muse send waits for the echo before showing your bubble | `aui interact/fluid_list.rs:865`; `session/commands.rs:123-150` | switch, send feel | Activate on mouse-down (Browse rows already do); optimistic bubble like the provider lanes | S |
| 12 | Startup probes: `claude`/`codex --version` + auth + a codex app-server + the Muse TUI tier probe (up to 20 s, `thread::sleep` on gpui background threads); pipe read only after exit (deadlock past 64 KB) | `provider_status.rs:430-490,651`; `tier.rs:1190,1276`; `billing.rs:80`; `skills.rs:497-514` | startup, executor starvation | Cached statuses (doc 50 P5); dedicated threads for sleeps; stream stdout while waiting | S–M |
| 13 | Housekeeping: `tasks` Vec never pruned; **3,419 stale terminal sockets**; `layout.json` parsed 5× at boot; full-res image decode for 64 px tiles; no `[profile.release]` tuning; gpui `test-support` in the shipped binary; Files walk ignores `.gitignore`; `files_fts` has **0 rows** (file search is broken); `baaz_log!` has no level gate and **logs every keystroke routed to a browser page** (privacy, `browser/mod.rs:3986`); Claude/Codex lanes open a scripted lane when `self.client.is_none()` — a click before Muse connects (or with no Muse) gets a lane that never sends (`lifecycle.rs:2420,3000,3263`; verify live) | `wire.rs:80`; `run/`; `app.rs:1427…`; `turns.rs:445`; `Cargo.toml`; `right.rs:54`; `session/events.rs:190` | memory, startup, correctness | Each is ≤ 1 hour | S |

Outside Baaz: Muse's own `session-index.db` WAL is 1.38 GB and never checkpointed — report upstream.

Already good (keep): IO goes through `wire_call` on the background executor; the transcript and sidebar are virtualised
gpui lists with range splices; `.cached()` session view and sidebar; 8-view MRU; delta-that-changes-nothing doesn't
notify; previews capped and `uniform_list`ed; `BAAZ_TRACE` boot + switch marks; `--bench` measures element/draw
p50/p99, fold time, RSS and idle frames.

## 3. Plan

**M · Measure first (2 days, gate for everything after).** Add marks under `BAAZ_TRACE` and a JSON sink shared with
`bench.rs`: first-frame, interactive, switch (mouse-down → painted, cached and cold), send (Enter → bytes; first stdout
line → painted), frame time while streaming (50/300/3,000-turn fixtures + a real 13 MB session), idle frames and CPU in
four focus states, per-turn background CPU and bytes written, git refresh latency, tool-call round trip, RSS. A
main-thread stall watchdog (log any span > 8 ms, like Zed's hang detector). New uiprobe verbs (`boot`, `switch`,
`turn`, `idle`) that **fail the gate past budget**, offline only. Baseline against a *copy* of the real state dir
(synthetic fixtures hide #1 and #2). Zed has no CI perf gate; this one would put Baaz ahead.

**P0 · Quick wins (≈ 1 week):** #1, #2, #3, #4, #10, #11, #13. Expected: sidebar on frame 1, ~27 s CPU and GBs of
writes removed per turn, ~200 MB less RSS, idle at rest.

**P1 · Structural (≈ 1.5–2 weeks):** #6 streaming path, #7 tail-first open for every provider, #8 git on a watcher,
#5 event-driven terminal/bridge, #9, #12; sqlite done Zed's way (WAL, readers + one writer thread, prepared statements).

**P2 · Hold the line:** budgets in relay Tier V; criterion benches for fold, render cache, markdown, `visible_sessions`;
a frames-requested-by counter in `aui-motion` (would have caught #4).

Each step re-runs M and reports before/after numbers. Library changes ship as an agentic-ui tag like every other round.

## 4. Order against doc 50 and the running build session

P0 touches `search.rs`, `index.rs`, `app/find.rs`, `app/list.rs`, `main.rs`, `aui-terminal` — nearly disjoint from the
other session's provider work, so **M and P0 can start now in a worktree**. #7 and the registry half of #1 overlap doc
50 P3/P4; do them together after the other session commits.

## 5. Progress (2026-10-16, branch `perf/measure-p0` + agentic-ui `perf/p0`)

Measured on copies of the real stores unless noted. "Not seen live" means no person or capture has watched it in
the running app yet.

| # | Fix | Before → after |
|---|---|---|
| 1 | Search index syncs only changed sessions (fingerprints, rowid deletes, WAL, no schema write per open) | full rebuild 34 s / 13.6 s CPU / ~1 GB → 5 ms unchanged, 39 ms one changed (19 s once, first run) |
| 2 | Index read: no `search_text`; stamps from the covering index; labels cached in `index-cache.json`; title/prompt cut to 1,000 chars | 5.4 s → 4 ms with a current cache (cache 5.5 MB, loads in 7 ms); full read runs once in the background after boot |
| 4 | `aui_motion::looping` and the text shimmer wake their view on a 30 Hz timer (stepped loops at their step) through a next-frame callback, so a hidden window wakes nothing; the terminal card mirror at 10 Hz | per-frame (60–120 Hz root renders) → ≤ 30 Hz; approval wait 23 % → 15 % CPU (window behind other apps); the ported shimmer is **not seen live** |
| 3 | CEF warms up a second after the window unless a page is restored; its safety pump idles at 1 Hz with no page open | not measurable in a debug build (CEF ships in release bundles only) — checked in the BaazCheck bundle |
| 5 | Terminal service pump sleeps until a job is queued; listener blocks in `poll(2)`; pty reader thread and UI notifier wait on signals, not 8 ms timers | ~66 + ~250/tab wakeups per second at rest → 0 at rest; accept no longer waits up to 50 ms |
| 10 | `rejoin` and search rows canonicalize once per root per pass; `sessions.json` writes off the UI thread, ordered by generation | (UI-thread stalls, not timed) |
| 11 | Session rows open on mouse-down (a shown action tray stops the press) | 60–120 ms earlier per switch (perceived) |
| 13 | Finished wire tasks pruned; dead terminal sockets swept at start | unbounded → ≤ 64 parked; 3,419 → 0 |
| M | `BAAZ_TRACE`: `first-frame`, `app-running`, `aui-init-done`; `renders=N/2s`; `baaz-send` submit → ack → first-reply-painted | warm boot first frame 170–245 ms (target 150): ~80 ms before `app.run`, ~60 ms window creation |

Deferred to doc 50 P4 (it moves Muse onto the provider lane): the Muse optimistic bubble.
Found while measuring: offline, the sidebar never leaves its loading skeleton (Muse-gated, doc 50 P1), so the
login screen animates indefinitely — now at 30 Hz instead of display rate.
Measurement traps: replay runs read the real Muse index unless `XDG_DATA_HOME` is isolated; `ps %cpu` is a decaying
average (use cumulative `ps -o time`); test windows behind the owner's apps get no display link.
