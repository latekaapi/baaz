# Performance audit — whole harness, measured not guessed

Branch `main` at `9a86482`, clean tree except this file. Library hotpaths read on
agentic-ui branch `login-methods` at `2e56405`. No source file changed in either repo.

## Measurements (debug build, this machine)

Frame stats measure harness element construction in `render_transcript` only, not
gpui layout/paint or library row cost (`crates/harness/src/session.rs:4032`).

| command | result (stderr) |
|---|---|
| `HARNESS_FRAME_STATS=1 ./target/debug/harness --replay fixtures/msp/synthetic-stress-300.jsonl --steps 'bench:240' --screenshot /tmp/stress-perf.png --screenshot-delay 500` | `n=120 p50=8us p90=13us p99=19us max=119us`, `n=120 p50=7us p90=16us p99=35us max=36us` |
| `HARNESS_FRAME_STATS=1 ./target/debug/harness --replay fixtures/msp/transcript-echo.jsonl --steps 'bench:240' --screenshot /tmp/echo-perf.png --screenshot-delay 500` | `n=120 p50=12us p90=36us p99=68us max=79us` |
| `HARNESS_FRAME_STATS=1 ./target/debug/harness --replay fixtures/msp/synthetic-stress-300.jsonl --steps 'mid;bench:120' --screenshot /tmp/stress-mid.png --screenshot-delay 500` | `n=120 p50=8us p90=13us p99=21us max=107us` |

Per-frame cost is flat across turn count (300-turn stress p50 7–8us vs few-turn
echo p50 12us), consistent with the virtualised `list()` over `Rc<Vec<Turn>>`.
Every command above is free: `--replay` folds captures with no child and no server.

Verified good (not findings): streaming `apply` notifies only on fold/view change
(`crates/harness/src/session.rs:780`); turn ticker is 1 Hz and notifies only when
the displayed second changes (`crates/harness/src/session.rs:1239`); `@` mention
rank runs on `background_spawn` with epoch guard (`crates/harness/src/session.rs:2977`);
`skills::list` + `files::walk` run on `background_spawn` (`crates/harness/src/app.rs:1932`);
sqlite `index::read` runs on `background_spawn` with 250 ms busy timeout and
read-only `NO_MUTEX` flags (`crates/harness/src/app.rs:1429`,
`crates/harness/src/index.rs:23`); `text_selections` holds at most one entry
(`crates/harness/src/session.rs:2529`).

## Findings

- **[performance-1] Frame stats miss layout and paint** — `crates/harness/src/session.rs:4032` — medium — `record_frame_stats` times element construction in `render_transcript` only, so the 7–8us p50 says nothing about gpui row layout/paint or library card cost on a 300-turn stream — extend the hook to whole-frame time (see performance-16) and keep element time as a sub-metric
- **[performance-2] Approval scan walks the whole transcript every frame** — `crates/harness/src/session.rs:1964` — medium — `newest_pending_approval` reverse-scans every turn and block on each `render_centre` (via line 2024) and clones the choices `Vec` on a hit, O(turns×blocks) per frame on the 300-turn stream — cache the pending approval id and choices in `apply` when approval events fold and read the cache in render
- **[performance-3] Folds struct cloned per frame** — `crates/harness/src/session.rs:2096` — medium — `render_transcript` clones `toggled`, `titles`, `cached_full_output` and collects `text_selections` into a fresh map every frame even when nothing changed — hold these behind `Rc` snapshots refreshed only in `refresh_render_cache`
- **[performance-4] Transcript cache deep-clones all turns on every fold change** — `crates/harness/src/session.rs:2289` — medium — `refresh_render_cache` runs `session.turns.clone()`, a deep clone of every block string on each streaming delta that changes the fold, so a 300-turn stream pays O(transcript) per chunk — have the fold hand out an `Rc`-shared turn list or splice deltas into the cached `Rc` instead of re-cloning
- **[performance-5] Sidebar clones and sorts twice per frame** — `crates/harness/src/app.rs:2204` — medium — `visible_sessions` clones every entry and sorts newest-first on each call, and `sidebar::grouping` sorts the same rows again (`crates/harness/src/sidebar.rs:181`), while `render_sidebar` and the palette each call it per frame — cache the sorted visible list and invalidate it only when sessions, index, or show/hide/empty flags change
- **[performance-6] Clock reads per row per frame** — `crates/harness/src/sidebar.rs:186` — low — `grouping` calls `Local::now()` per entry for the date bucket and `elapsed` calls it per row (line 210), so a hundreds-row sidebar does hundreds of clock reads per frame — hoist one `now` per frame and quantise elapsed labels to the minute so rows stop repainting every second
- **[performance-7] Palette rebuilds rows per frame and re-resolves clicks by scan** — `crates/harness/src/app.rs:2439` — medium — `palette_rows` rebuilds all rows (including another `visible_sessions` clone+sort for Resume and `format!` ids in `search_rows`) on every frame, and `confirm_palette`/`select` recompute the same rows to map index back to id — compute rows once per frame, share them between render and handlers, and carry the row id in the selection instead of re-scanning by position
- **[performance-8] Composer copies the whole draft every frame** — `crates/harness/src/session.rs:3015` — low — `render_composer` runs `self.composer.read(cx).value().to_string()` plus a chips `collect()` per frame only to derive `can_send` — cache the trimmed-emptiness/images/files-empty flag on composer change events instead of copying the draft per frame
- **[performance-9] Window title and search status allocate per frame** — `crates/harness/src/app.rs:613` — low — `window_title` clones the session id, scans `sessions` for the label, and formats every frame, and `search_status` (line 3253) allocates its status string per frame while the palette is open — cache the title and invalidate on session/label/workspace change; build the status string only when counts change
- **[performance-10] Env lookup on every transcript frame** — `crates/harness/src/session.rs:4035` — low — `record_frame_stats` calls `std::env::var("HARNESS_FRAME_STATS")` on every `render_transcript` even when disabled — read it once into a `OnceLock<bool>` and early-return on the cached flag
- **[performance-11] Library prose() re-parses markdown uncached per render** — `/Users/latekaapi/Projects/agentic-ui/crates/aui/src/transcript/prose.rs:177` — medium — `prose` runs its own `parse` (line 117) on every render with no memo, called for each question prompt (question.rs:278) and plan item (plan.rs:150), while turns use memoised `parsed_markdown` — route `prose` through the shared parse cache or memoise per (source, style)
- **[performance-12] Markdown memo hashes the full source per render and evicts all at cap** — `/Users/latekaapi/Projects/agentic-ui/crates/aui/src/transcript/markdown.rs:954` — low — `parsed_markdown` re-hashes the whole turn text with `DefaultHasher` on every hit and `clear()`s all 128 entries at `PARSED_CACHE_CAP` (line 963), causing a re-parse storm for every visible turn at once — keep the hash alongside the source or key on `Arc<str>` identity, and replace clear-all with per-entry eviction
- **[performance-13] Idle frames from caret, shimmer, and springs** — `/Users/latekaapi/Projects/agentic-ui/crates/aui/src/transcript/prose.rs:36` — medium — `caret_visible` runs a 1 s `looping` clock per streaming turn, `activity.rs:103` and `status.rs` shimmer while working, and question/code rows run `tween`/`spring_phase` per frame, with no verified idle-frame count — gate each clock on its visible active state and add an idle assertion (zero frames for N seconds on a settled replay) to the bench mode
- **[performance-14] Image attach decodes synchronously on the UI thread** — `crates/harness/src/session.rs:1718` — medium — `attach_paths` calls `images::from_path` (fs read plus full `image::load_from_memory` decode, `crates/harness/src/images.rs:98`) inline in the drop/paste/step handler, so a 10 MB photo stalls the frame it lands on — move read+decode+thumbnail onto `background_spawn` and insert a placeholder chip until it resolves
- **[performance-15] Screenshot path resizes and saves on the UI update** — `crates/harness/src/shot.rs:121` — low — `capture_and_quit` runs Lanczos3 `resize`, `create_dir_all`, and `image.save` inside `cx.update`, blocking the UI thread once per capture — do the resize/encode/write on the background executor and keep only `render_to_image` on the UI thread
- **[performance-16] Fold retains every opened session forever** — `crates/muse-adapter/src/fold.rs:164` — low — `apply` inserts per-session `Folded` state into the sessions map and nothing ever removes a session entry (only per-turn internals are retained at line 1642), so memory grows with every session opened in the process — evict a session's folded state when its view closes, keeping only the active session plus bounded MRU
- **[performance-17] Fold clones the session JSON per session/started** — `crates/muse-adapter/src/fold.rs:429` — low — `session_started` clones the whole `session` sub-`Value` just to `from_value` it into `msp::Session` — deserialize from `&Value` (`from_value` accepts the reference via `serde_json::from_value::<T>(v.clone())` is unnecessary; use `serde_json::from_value` on the borrowed value or `Deserialize` from `&Value`) to avoid the per-open clone
- **[performance-18] --bench mode: streaming replay plus scroll plus whole-frame timing** — `crates/harness/src/session.rs:1924` — medium — today's `bench` step only re-notifies N frames on a static replay, so it cannot measure streaming fold cost, scroll cost, or layout/paint, and `record_frame_stats` covers element construction alone — build `--bench <capture> [--cadence-ms N] [--scroll top|mid|tail] [--frames N]` that replays `<--` lines through the fold at a fixed cadence on a timer, drives the list to top/mid/tail per frame window, times element-construction plus gpui frame (via frame-callback timestamps around `render_to_image`-free normal frames), and prints `frames, fps, dropped, p50/p90/p99/max` for element time, frame time, fold-apply time per event, plus peak RSS, with a `--bench-out json` row for tracking across runs

High: 0. Medium: 11. Low: 7.
