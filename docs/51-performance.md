# 51 — Performance: measure, then make Baaz instant (plan, 2026-10-16)

Status: **approved 2026-10-16; P0 merged 2026-10-17; M merged 2026-10-18 (§6); P1 (§7) and its Muse-list rest (§8) and P2 (§9) on main 2026-10-21.** Evidence: static audits of Baaz + agentic-ui (2026-10-16),
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

## 6. Budgets that fail the gate (step M, 2026-10-17, branch `foundations/m-p3`)

`scripts/perf-budgets.py` measures, compares against `scripts/perf-budgets.json` and exits 1 on a miss (2 when it could
not measure — never a pass). Offline, deterministic, no model turns, a throwaway state dir; ~40 s. It runs:

- in **relay Tier V** as the adapter's entry-less `budget` verb (`uiprobe.json`; `relay_visual.py` turns each miss into a
  feel-wrong finding and each unmeasured metric into *unverified*; `--no-budget` skips it and says so);
- in **CI** as `--only store --slack 3` (the windowed metrics need a display and this machine's numbers);
- by hand: `python3 scripts/perf-budgets.py [--binary target/release/baaz] [--only <group>] [--json]`.

| Metric | How | Measured (debug, median) | Budget |
|---|---|---|---|
| `store.index_refresh_unchanged_ms` | `baaz --perf-store`: stamp-only re-read of a 2,600-row Muse-shaped index | 0.7 ms | 20 |
| `store.index_refresh_one_ms` | same, one row moved | 0.85 ms | 20 |
| `store.index_cache_load_ms` | `index-cache.json` boot read, 2,600 rows | 1.1 ms | 50 |
| `store.search_sync_unchanged_ms` | `search.db` sync, nothing changed | 0.7 ms | 25 |
| `store.search_sync_one_ms` | sync, one finished turn | 1.2 ms | 50 |
| `store.search_query_ms` | palette query | 0.3 ms | 20 |
| `store.provider_bodies_ms` | 120 Claude/Codex transcripts read for search (P3) | 23 ms | 150 |
| `boot.first_frame_ms` | `--no-connect --login signed-in`, `first-frame` mark, 1 warm-up + 5 runs | 125 ms | 300 |
| `open.replay_300_ms` | `--replay synthetic-stress-300`: open → first transcript render | 13 ms | **50** (target) |
| `switch.cached_300_ms` | `click:` away and back: cached switch → first transcript render | 3–6 ms | **16** (one frame) |
| `stream.to_render_p50_ms` / `p99` | `--bench`: each event's fold → next transcript render | 0.7 / 1.6 ms | 8 / **16** |
| `stream.fold_apply_p99_ms` | `--bench` fold per event | 0.045 ms | 2 |
| `idle.cpu_pct` | 300-turn replay open, left alone 10 s: cumulative `ps` CPU | 0.0–0.1 % | **0.5** (target) |
| `idle.renders` | same: `baaz-perf renders` lines | 0 | **0** |

Informational (no budget): `store.index_full_read_ms` 8 ms, `store.search_first_sync_ms` 440 ms (first sync of 2,600
rows of 16 KB). The real store's first sync was 19 s once (bodies 80 KB avg, 1 GB db).

What changed to make these measurable: `--perf-store` (headless, `perf_budget.rs`); `--bench` reports
`stream_to_render` (`bench-stream`); `--replay` marks `replay-open`/`replay-folded`/`first-content`; offline switches
trace like live ones; a replayed capture parks in the MRU in a scripted window (it used to come back as a blank
"Loading history…" view — the switch budget measured nothing until this changed).

**What these numbers are not.** Debug build; the window renders behind other apps, so "render" is the transcript's
element construction, not a pixel on glass, and idle cannot see display-rate loops that only run with a display link
(the Tier V `idle` entries and a live check still own that). Synthetic bodies are 16 KB, not 80 KB. Release (`--binary target/release/baaz`, same budgets): first frame 123–172 ms, open 12–13 ms, cached switch 2.0 ms,
stream-to-render p50/p99 0.4/0.6–1.2 ms, fold p99 0.024 ms, store costs within 10 % of debug (sqlite is built
optimised either way), idle 0–0.1 % and 0 renders.

Provider reopen (traced now: `resume <provider> <id>` → `provider lane open` → `first-content`), release bundle, owner
state copy, cold: Claude Code 120–150 ms, Codex 2.0–2.3 s. Not budgeted — it spawns a real CLI; P1's "stored history
painted before the child" is the fix for Codex.

## 7. P1 progress (2026-10-18/19, branch `perf/p1`)

Measured on the release bundle against a copy of the owner's state (2,612 sessions, 91 Claude/Codex records) unless
noted; every number before → after. Each cause was traced first (new marks: `reopen gate done → adapter connected →
resume acked`, `resume-ack-err <error>`), not guessed.

| What | Cause found | Fix | Before → after |
|---|---|---|---|
| Codex reopen | the ack waited on `account/rateLimits/read` (network, 2.3–6.3 s, 10 s timeout) and, for a thread at a non-default level, a cold `model/list` (3.4 s) | usage read detached; catalog cached per home and, when cold, read in the background (a turn waits for it) | 2.1 s (2–9 s) → **0.21–0.23 s** |
| Codex reopen soon after a quit | `SIGKILL` left Codex's thread writer lease held: the next resume waited ~5 s, then "already has an active writer" | `SIGTERM`, 250 ms grace, then `SIGKILL` (`provider::child_env::terminate`) | every other relaunch 5 s / failed → 0.22–0.33 s back to back |
| Claude Code reopen | `record_provider_open` re-stated the session's project → full rejoin + 54 ms search-row rebuild on the UI thread | `set_override` returns when the edit changes nothing | 110–130 ms → **28–31 ms** |
| Muse reopen (all sessions) | muse 1.4.10 refuses the terminal bridge on `session/resume` (-32603 "session MCP base configuration is unavailable") → an error dialog on every reopen | retry once without the bridge; remembered for the run | dialog → history in **0.46 s** (the rest is Muse's own resume, 0.5–1.8 s) |
| Streaming at length | `refresh_render_cache` deep-compared every turn, recounted every turn's rows and cloned 3,000 row ids per chunk | exact dirty-turn set from the fold (pinned by an every-capture test), suffix refresh, row-count memo, suffix-only list sync | 3,000 turns: render work p50 7.7 → **0.86 ms**, stream-to-render p99 19 → **4.2 ms**, frame p99 30 → 15 ms |
| Search rows after every turn | `is_side_session` scanned the session list per indexed session: O(n²) | side-marked ids collected once per build | 54 ms → **8–9 ms** (UI thread) |
| Rejoin after every pin / rename / title | ~2,070 workspace roots `realpath`ed again each pass | process-wide memo (unresolved roots re-checked after 5 min) | 18–143 ms → **6 ms** |

New budget: `stream.long_1000_element_p99_ms` (generated 1,000-turn transcript) — 0.19 ms now, budget 0.6 ms; the
pre-P1 build measures 1.17 ms there, so the old O(n) refresh fails the gate.

Looked at and left (measured, not worth it now): Muse's first history page (30 ms once the resume succeeds — the 1 s
seen earlier was the failed-resume path); a smaller first page buys nothing. Muse's own `session/resume` (0.5–1.8 s)
is server work. Muse `session/list` at boot is 27 serial pages of 200 (the server maximum), ~5 s, but the sidebar
paints from the cache and nothing waits on it — an `updatedAfter` incremental listing is the follow-up. 60 Muse chain
rows rewrite their search row once per launch because the boot label (index cache) and the list's label differ.

## 8. Speed P1, the rest: Muse's list at launch (2026-10-21, on `main`)

**Probe first (muse 1.4.11, `1.4.11-R6667.1`, read-only `muse serve` + `session/list`, no turns).**
`session/list` takes `updatedAfter` and honours it:

- it filters on **`updatedAt`, strictly later** (a cut equal to the newest stamp returns 0 rows, one microsecond earlier
  returns that row); microsecond precision; a malformed stamp is `-32602 invalidParams`;
- filtered results page like the full listing (`limit` ≤ 200, `nextCursor`);
- the listing is ordered by **`lastActivityAt` descending**, not `updatedAt` as the schema comment says, and
  `updatedAt` also moves on `session/resume`, so "newer" includes sessions that were merely opened — harmless here;
- cost on the owner's 2,661 sessions: full listing 27 pages, 2.5 s warm from a probe (4.0–4.2 s inside Baaz at boot,
  the first page cold at 1.5–2.5 s); a filtered listing is one page in 130–360 ms warm, ~1.4 s as the first call of a
  cold `muse serve` (Muse warming its index — server work, nothing Baaz can skip).

What a filtered listing cannot see is a session that left Muse. Hence: **`list_cache`** keeps the last listing in
`session-list-cache.json` (state dir), asks for rows newer than its newest `updatedAt` less a 5 s overlap, merges by id
and re-sorts like the server; a full listing runs when the copy is missing, unreadable, from another Muse data dir,
older than 24 h, or when the filtered call fails. Rows are compacted as they arrive — title, first prompt and names cut
to 1,000 characters like the index read — because Muse lists whole first prompts: the first saved copy was **233 MB**
(128 MB of prompts + 128 MB of titles); compacted it is 6.6 MB. The same listing runs after every finished Muse turn
and every fork, which used to re-read all 27 pages each time.

| Measured (release bundle, BaazCheck, fresh state dir, real Muse index) | Before | After |
|---|---|---|
| Launch listing (`session/list-work-done`) | 3.97–4.21 s, 27 pages | **1.39–1.62 s, 1 page** (cold Muse's first call) |
| Listing after a turn / fork (warm Muse, probe) | ~2.5 s, 27 pages | **130–360 ms, 1 page** |
| Data over the pipe per listing | ~260 MB of prompts and titles | the newer rows only |

Offline budgets (`--perf-store`, scripted 2,600-row server — the wire itself is Muse's): `store.list_cache_load_ms`
(read the saved copy), `store.list_newer_ms` (the newer-rows listing and merge) and `store.list_newer_pages` (budget
**1**: a listing that falls back to every page reads 13 there and fails).

Found on the way and fixed (each its own commit, 2026-10-21):
- **Labels flipped between three ladders** (boot rows: index only; list join: wire first, cut to 80; `rejoin`: index,
  uncut), so 63–64 rows rewrote their search row on every launch. One `sidebar::label_ladder` now; boot rows join the
  saved list. Warm boots write **0** search rows (were 63).
- **A second boot install dropped every boot row** when the index reconcile landed before the list reply.
- **The sidebar showed skeleton rows until the list reply** (1.4 s typically, 20 s once) although the boot rows were
  built at ~0.5 s: the D3 screen's `loading` waited for `session/list` again, undoing P0 #2. Rows paint at
  `first-rows-painted` +593 ms now (list reply +2,119 ms in the same run); index-only boot rows keep their empty-filter
  exemption (`turns_unknown`), rows joined from the saved list are filtered like the reply's.

Still open here: the list reply joins all 2,661 rows on the UI thread (`list-applied` 64–144 ms) after every finished
Muse turn, although the incremental listing knows which rows changed.

## 9. P2 — hold the line (2026-10-21)

**Benches (criterion 0.5.1, no default features).** Release, this machine:

| Bench | Where | Result |
|---|---|---|
| `fold/replay/synthetic-stress-300` (1,291 events) | `cargo bench -p muse-adapter` | 2.55 ms (~2.0 µs an event) |
| `fold/replay/synthetic-stress-hetero` | 〃 | 3.32 ms |
| `fold/tail/stress-300_last_200` (at length) | 〃 | 383 µs (~1.9 µs an event: no growth) |
| `parse_markdown/2048` · `/20480` · `/102400` | agentic-ui `cargo bench -p aui --bench markdown` | 47 µs · 374 µs · **1.90 ms** |
| `parse_markdown_settled/json_400_rows` | 〃 | 32 µs |
| `render_cache/stream_turn_at_300` (one turn, ~6 events, apply + refresh) | baaz, ignored test `bench_render_cache` | 192 µs |
| `render_cache/stream_turn_at_3000` | 〃 | **1.07 ms** |
| `render_cache/full_300` | 〃 | 110 µs |
| `visible_sessions/2600_after_a_change` | baaz, ignored test `bench_visible_sessions` | 2.3 ms |
| `visible_sessions/2600_cached` | 〃 | 22 ns |

Baaz is a binary crate, so its two benches run criterion from ignored tests:
`cargo test -p baaz --release --bin baaz bench_ -- --ignored --nocapture --test-threads=1`. Every bench asserts its
input folded (the first render-cache run timed an empty view — wrong session id — at 52 µs and looked plausible).

What they show, for later work, not fixed here: **a new turn costs O(transcript) in the render cache** (5.6× from 300
to 3,000 turns; P1 made chunks *inside* a turn cheap, `refresh_render_suffix`, not a turn's arrival); a **100 KB
answer re-parses in 1.9 ms per chunk** (#6 settled-prefix markdown still open); `visible_sessions` rebuilds in 2.3 ms
on the UI thread after every list change.

**Frames-requested-by counter (agentic-ui `aui_motion::frames`).** `aui_motion::request_frame(window)`
(`#[track_caller]`) replaces `window.request_animation_frame()` at every call site in aui-motion, aui and Baaz, and
`looping`/`synced` count each redraw their wake causes against their caller, so every frame asked for is attributed
to a `file:line`. Baaz prints `baaz-perf frames=N by file:line=n …` beside its `renders=N/2s` trace line
(`BAAZ_TRACE=1`), and `perf-budgets.py` gates **`idle.frame_requests` at 0**, naming the call sites on a miss —
the always-rendering bug of #4 would have failed with its culprit's line.

**Idle CPU with the window visible.** The full release run (all budgets pass but one) measured `idle.cpu_pct`
**0.6–0.8 %** against 0.5 on four runs, with `idle.renders` 0 and `idle.frame_requests` 0. `sample(1)`: gpui's
`CVDisplayLink` thread fires every vsync and wakes the main thread (`gpui_macos::window::step`) although nothing
renders. gpui stops the link only when the window is occluded, which is why earlier runs — the test window behind the
owner's apps — read 0.0–0.1 % and docs/61 saw one 0.6. A visible idle Baaz costs ~0.7 % CPU at 120 Hz. Fixing it is a
gpui-pre change (stop the link when no frame is requested); the budget is unchanged and the number is real.
