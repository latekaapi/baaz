# Consolidated findings — all five audit areas

Read-only integration; no source file changed, nothing committed.
Source files (all under `docs/audit/`, relative to harness root):

- `client-adapter.md` — Area 1: 18 findings (high 2, medium 10, low 6)
- `app-core.md` — Area 2: 18 findings (high 0, medium 14, low 4)
- `support.md` — Area 3: 22 findings (high 0, medium 11, low 11)
- `performance.md` — Area 4: 18 findings (high 0, medium 11, low 7)
- `library-hotpaths.md` — Area 5 (agentic-ui `login-methods`): 15 findings (high 0, medium 10, low 5)

Totals: high 2, medium 56, low 33 — 91 raw findings, 85 unique after de-dup (§0).

## 0. De-duplicated pairs (canonical kept first)

- D1: `performance-11` == `library-hotpaths-1` (prose re-parse) → E-LIB-1
- D2: `performance-12` ⊂ `library-hotpaths-6` (markdown memo hash + evict-all) → E-LIB-4
- D3: `performance-10` == `app-core-18` (env lookup per frame) → A-MECH-10
- D4: `performance-5` ⊃ `support-2` (sidebar clone+sort per frame) → D-PERF-3
- D5: `performance-6` ⊃ `support-3` (clock reads per row) → D-PERF-3
- D6: `performance-17` ⊂ `client-adapter-8` (per-event Value clones) → D-PERF-6

## Required order

Execution order: **E → A → B → C → D**.
Library (E) before harness; within harness, mechanical (A) before
dead-code removal (B) before structure (C) before performance (D).
Bench infra (`performance-18`, in D-PERF-8) is the exception: build it first
if any D claim needs re-measuring. Sections below stay in A–E label order.

## E. Library (agentic-ui) — size M — order 1 (first)

All paths `crates/aui/...` in agentic-ui unless marked `harness:`.

- E-LIB-1 (M) — `library-hotpaths-1` + `performance-11`dup — medium —
  `prose()` re-parses per render uncached; route through `parsed_markdown`.
- E-LIB-2 (S) — `library-hotpaths-2` — medium — `CodeBlock` re-tokenizes per
  line per frame; memoize runs per `(language, line)`, bounded.
- E-LIB-3 (S) — `library-hotpaths-3` — medium — shell bodies re-parse ANSI
  per line per frame; cache `(text, runs)` per output revision.
- E-LIB-4 (M) — `library-hotpaths-6` + `performance-12`dup — medium —
  memo hits still hash full source + global lock, streaming never hits,
  evict-all past 128; key per turn, per-entry eviction.
- E-LIB-5 (S) — `library-hotpaths-5` — medium — `span_runs` rebuilds
  text/runs/links + `font()` lookups per block per frame; hoist fonts to
  `LazyLock`, cache runs per `(block, style)` hash if profiling justifies.
- E-LIB-6 (S) — `library-hotpaths-4` — medium — streaming caret re-shapes
  closing block per frame; cache trailing width on `(source hash, width)`.
- E-LIB-7 (S) — `library-hotpaths-7` — medium — `format!` element ids per
  block/line/row per frame; key by integer child ids (`aui_motion::child_id`).
- E-LIB-8 (M) — `library-hotpaths-8` — medium — by-value `tool_group` /
  `question_card` / `plan_card` force harness clones per frame
  (`harness:transcript.rs:470`); take `&`/`&[]` with cheap `SharedString`.
- E-LIB-9 (S) — `library-hotpaths-9` — medium — `SelectableText` clones full
  text+runs per frame; build from shared buffer or cache per revision.
- E-LIB-10 (S) — `library-hotpaths-10` — medium — browser body infinite pulse
  loop while visible even settled; gate ring on pending/recent-capture.
- E-LIB-11 (S) — `library-hotpaths-11` — low — turn selection by value vs
  `Markdown` borrow; change to `Option<&TextSelection>`.
- E-LIB-12 (S) — `library-hotpaths-12` — low — small bounded per-frame
  formats (footers, meters, login headline, queue header); hoist to owners.
- E-LIB-13 (S) — `library-hotpaths-14` — low — `markdown_selected_text`
  bypasses parse cache on copy path; call `parsed_markdown`.
- E-LIB-14 (S) — `library-hotpaths-15` — low — settled spring/tween/presence
  silence unverified (gpui-kit out of scope); assert zero idle frames.

## A. Mechanical cleanup — size S — order 2

One-line-to-one-function safe fixes, no architecture change.

- A-MECH-1 (S) — `client-adapter-1` — HIGH — `reindex` never remaps cached
  `Slot.turn` after `TurnRemoved`; remap slots via `positions`, drop gone.
- A-MECH-2 (S) — `client-adapter-2` — HIGH — `dispatch` parks only
  Notifications during `view/gap`; park `ServerRequest` with sessionId too.
- A-MECH-3 (S) — `client-adapter-5` — medium — post-timeout late responses
  dropped silently; log/emit unmatched responses.
- A-MECH-4 (M) — `client-adapter-12` — medium — add `view/subscribe`,
  `item/readOutput`, `session/modelRouteUnserved` dispatch arms + tests.
- A-MECH-5 (S) — `client-adapter-3` — medium — silent decode failures; count
  per session in `SideState` / diagnostic delta.
- A-MECH-6 (S) — `client-adapter-9` — medium — unify approval by/allowed map,
  cursor/session extraction, document todo-mapping split.
- A-MECH-7 (S) — `client-adapter-17` — low — `call_no_params` helper for
  `account_read`/`account_login_cancel`/`account_logout`.
- A-MECH-8 (S) — `client-adapter-13` — low — add `authRequired` to failure
  title test. A-MECH-9 (S) — `client-adapter-14` — low — fix `events()`
  fan-out docstring. A-MECH-10 (S) — `client-adapter-15` — low — `Unknown`
  drops raw string; retain or pin lossy test.
- A-MECH-11 (S) — `client-adapter-18` — low — cap/normalize cancel-reason
  marker text via mapping like `failure.rs`.
- A-MECH-12 (S) — `app-core-16` — low — one `harness_log` helper + derive
  login state names. A-MECH-13 (S) — `app-core-17` — low — resolve docs dir
  once, cache `Option<PathBuf>`.
- A-MECH-14 (S) — `app-core-18` + `performance-10`dup — low — `OnceLock<bool>`
  for `HARNESS_FRAME_STATS`; `performance-9` title/status caching joins here.
- A-MECH-15 (S) — `support-5` — medium — log index schema drift (table vs
  column vs locked). A-MECH-16 (S) — `support-6` — low — history dedup vs
  last 3 entries. A-MECH-17 (S) — `support-8` — low — `truncated` flag on `@`
  walk. A-MECH-18 (S) — `support-9` — low — one-decimal MB format.
- A-MECH-19 (S) — `support-13` — low — name bridge thread, assert exit on
  drop. A-MECH-20 (S) — `support-14` — low — `/logout`→`account/logout`
  comment. A-MECH-21 (S) — `support-17` — low — read preamble env once in
  `Args`. A-MECH-22 (S) — `support-18` — low — plan fallback prefers section
  labels. A-MECH-23 (S) — `support-19` — low — widen attachment text
  allowlist, UTF-8-sniff small unknowns.

## B. Dead and legacy code removal — size S — order 3

- B-DEAD-1 (S) — `support-1` — medium — delete dead `available`/`coming_in`
  pair + call-site branch. B-DEAD-2 (S) — `client-adapter-11` — medium —
  delete discarded `turn_retry_scheduled` format (or land retry-row feature).
- B-DEAD-3 (S) — `app-core-15` — low — delete `right_open` + no-op
  `ToggleRightPane` + "in this phase" comments, or track pane as upcoming.
- B-DEAD-4 (S) — `library-hotpaths-13` — low — delete `_unused` stub and
  caller-less `last_paragraph_runs` alias (or document it).
- B-DEAD-5 (S) — `support-15` — medium — `reconnect_after_login` behind
  `allow(dead_code)` (D25); delete after one billed live turn confirms.
- B-DEAD-6 (S) — `client-adapter-10` — medium — fix "echo is free" docstring
  (D19 wording) + `HARNESS_PROBE_LIVE=1` guard before `turn_start`.
- B-DEAD-7 (S) — `support-20` — medium — mark scripting-only verbs with
  "(scripting only)" + cost warnings in one help place.
- B-DEAD-8 (S) — `support-21` — medium — header-comment every
  `fixtures/msp/probe*.py|run*.py|drive.py|tui*.py` for turn-sending; point
  new work at `--replay` captures, keep `make-stress-300.py`.
- B-DEAD-9 (S) — `support-22` — low — keep `scripts/bundle.sh`; exclude from
  probe-script cleanup (record only, no action).

## C. Structure / refactor — size L — order 4

Do after A+B; each item lands behind the `wire_call` helper where async.

- C-STR-1 (M each) — `app-core-1..6` — medium — extract `login.rs`,
  `sidebar_view.rs`, `dialogs.rs` (or into `overlays.rs`), `steps.rs`
  (with `app-core-4`), tier/billing, `ResizeDrag` from `app.rs`.
- C-STR-2 (L) — `app-core-7` + `app-core-8` — medium — split `SessionView`
  (152 methods) and `Harness` (109) along existing section seams; C-STR-1 first.
- C-STR-3 (M) — `app-core-9` + `app-core-10` — medium — hoist title sync,
  resize settle, session-swap kick into an `on_frame` pre-pass; pure render.
- C-STR-4 (M) — `app-core-11` + `app-core-12` — medium — single
  `refresh_render_cache` sync point; per-card `*_card` fns, `block` as dispatch.
- C-STR-5 (M) — `app-core-4` + `app-core-13` — medium — one verb-to-handler
  table in `steps.rs` for shell/session/login steps; enumerate verbs in test.
- C-STR-6 (M) — `app-core-14` — medium — one `wire_call<T>` helper for all
  ~46 background-spawn-then-update sites; prerequisite for C-STR-1 churn.
- C-STR-7 (M) — `client-adapter-16` — low — split `schema.rs` by surface,
  generate dispatch checklist from one inventory.
- C-STR-8 (S) — `support-7` — medium — `set_overrides` batch single
  rejoin/write/reindex for batch hide.
- C-STR-9 (S) — `support-16` — medium — move `PENDING_APPROVAL` /
  `STEPS_RUNNING` off process globals onto `Harness`/capture token.
- C-STR-10 (M) — `client-adapter-4` — medium — fold `client/protocolError`
  into banner/marker delta, handle in `SessionView::apply`.

## D. Performance — size L — order 5 (last)

Measure with D-PERF-8 before/after each item; E-LIB first (row cost dominates).

- D-PERF-1 (M) — `performance-2` — medium — cache pending approval id+choices
  in `apply`; stop transcript-wide approval scan per frame.
- D-PERF-2 (M) — `performance-3` + `performance-4` — medium — `Rc` snapshots
  for folds struct; fold hands out `Rc`-shared turns / splice deltas in.
- D-PERF-3 (S) — `performance-5`+`support-2`dup, `performance-6`+`support-3`dup,
  `performance-7` — medium/low — cache sorted visible list + grouping (one
  `now` per frame, minute-quantised labels); compute palette rows once, carry ids.
- D-PERF-4 (S) — `performance-8` — low — cache composer emptiness flag on
  change events instead of copying draft per frame.
- D-PERF-5 (M) — `client-adapter-6` — medium — prune per-session maps on turn
  remove/complete; store queued text once.
- D-PERF-6 (M) — `client-adapter-8` + `performance-17`dup — medium — per-turn
  slot index (`shift_slots`/`relocate_call`/`reindex` touch one turn);
  deserialize from `&Value`, drop per-chunk clones.
- D-PERF-7 (M) — `client-adapter-7` — medium — bound gapfill page loop +
  buffer, track/join thread, emit visible abort event.
- D-PERF-8 (M, build first within D) — `performance-18` — medium — `--bench`
  mode: cadenced streaming replay + programmatic scroll + element/frame/
  fold-apply timing + RSS + `--bench-out json`. (`performance-1` whole-frame
  timing folds into this.)
- D-PERF-9 (S) — `performance-14` — medium — image read+decode+thumbnail on
  `background_spawn` with placeholder chip.
- D-PERF-10 (S) — `performance-13` — medium — gate caret/shimmer/tween clocks
  on visible-active state; idle-silence assertion in bench mode.
- D-PERF-11 (S) — `performance-15` — low — resize/encode/write screenshots
  off UI thread. D-PERF-12 (S) — `performance-16` — low — evict folded state
  on view close (active + bounded MRU).
- D-PERF-13 (S) — `support-4` — medium — `INSERT OR IGNORE` + uniqueness
  index for `record_files`. D-PERF-14 (S) — `support-10` — medium — 10 s
  timeout on `skills::list`. D-PERF-15 (S) — `support-11` — medium — pin tier
  card sentences in test, distinct stale-probe signal. D-PERF-16 (S) —
  `support-12` — medium — cache `auth.json` name/email by mtime.

## Ten highest-value items (single list, in priority order)

1. A-MECH-1 (`client-adapter-1`, HIGH) — post-retraction updates land on the
   wrong turn; silent data corruption.
2. A-MECH-2 (`client-adapter-2`, HIGH) — approvals fold out of order during
   backfill; wrong-card / missing-context approvals.
3. D-PERF-2 (`performance-4`) + D-PERF-6 (`client-adapter-8`) — per-change
   O(transcript) clones and transcript-wide slot scans; the streaming-cost core.
4. E-LIB-1 (`library-hotpaths-1`) — uncached `prose()` re-parse per render;
   cheapest big frame-cost win, library-first.
5. D-PERF-1 (`performance-2`) — per-frame full-transcript approval scan;
   O(turns x blocks) every frame, trivially cacheable.
6. C-STR-6 (`app-core-14`) — one `wire_call` helper for ~46 spawn/update
   sites; de-risks all later structural churn.
7. D-PERF-8 (`performance-18`) — `--bench` mode; makes every other D claim
   falsifiable (today only element-construction p50/p90 exists).
8. D-PERF-5 (`client-adapter-6`) + D-PERF-12 (`performance-16`) — unbounded
   per-session maps + never-evicted sessions; the two memory leaks.
9. A-MECH-5 (`client-adapter-3`) + C-STR-10 (`client-adapter-4`) — silent
   decode failures + swallowed protocol errors; wire faults invisible.
10. D-PERF-9 (`performance-14`) — image decode on UI thread; worst single
    frame hitch (10 MB photo stalls landing frame).
