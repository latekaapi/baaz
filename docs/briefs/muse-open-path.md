# Brief — H3: opening a session from the sidebar

Repository `/Users/latekaapi/Projects/harness`, **your checkout is a git worktree** (see the
launch command's `--workspace`); work there, on its branch. **Do NOT commit.** Do not touch
`/Users/latekaapi/Projects/agentic-ui` or `~/Projects/cockpit`. Prefix every shell command with
`export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`.
Spend rule: never `turn/start`, `--send`, `send:`/`steer:`, live tests, `muse logout`,
`account/logout`. Opening a session live — `session/list`, `session/resume`, `view/page` —
starts no turn and is free; that is the only live thing this brief does. Read `CLAUDE.md`,
`docs/05-handoff.md`, `docs/07-architecture.md` §2–§3, `docs/01-transport.md` §3 (cursors,
`view/gap`, the reconnect procedure with `session/resume { cursor }`), `docs/02-app.md`
(the "A session switch never flashes" paragraph), then `crates/harness/src/app/lifecycle.rs`
(`resume`, `open`, `swap_pending_in`, `on_session_event`), `session/events.rs::backfill`,
`session.rs::page_all`, `sidebar_view.rs::render_sidebar` and `app.rs::render_centre_header`.

## Diagnosis (done; measure it before fixing)

A click on a sidebar row calls `resume` → `open(.., backfill=true)`, which builds a fresh
`SessionView` with an empty `MuseFold`, and `backfill` runs `page_all` on the background
executor: it pages the **whole** transcript (`view/page`, 1000 events a page, serially) and
returns every event at once; only then are they all folded in one UI update and
`HistoryReady` emitted; only on the next frame does `swap_pending_in` make the view active.
Until that frame nothing on screen acknowledges the click: the sidebar's `selected` is derived
from `self.active`, and the centre header's title too. Nothing is cached: switching back to a
session just left pages it all again. The sessions in `~/Projects/harness` run to 13 MB of
log, so the pause is seconds long, with the UI thread blocked for the fold at the end.

## What to build, in this order

1. **Trace it.** `HARNESS_TRACE=1` prints, to stderr, microseconds since the click for:
   `resume` entered, `session/resume` acknowledged, each `view/page` page (event count),
   each page folded, `HistoryReady`, the swap, and the first `render_transcript` after the
   swap. Measure with `./target/debug/harness --workspace /Users/latekaapi/Projects/harness
   --session <id>` for `01a08706-b851-7260-a907-503095a5bcf5` (3.6 MB) and
   `01a08c85-52aa-7c11-8721-6f09436e05b8` (13 MB) — `--session` runs the same `resume` a click
   does — and put the "before" numbers in the report (click → highlight, click → first
   content frame, click → history complete).
2. **Feedback on the click.** Record the target (`pending_id`) the moment `resume` runs. The
   sidebar row highlights from it (selected = pending target, else active); the centre header
   shows the target's label from the list entry at once; the centre swaps at once to the new
   view — the cached one when there is one (item 4), otherwise the view showing its loading row
   (`loading_row`, never the empty state). The old view is not kept on screen any more; the
   `pending_active`/`pending_ready` deferral goes, with `HistoryReady` staying as the cue for
   `follow`. A failed `session/resume` keeps the new view and reports, as `resume` does today.
3. **Stream the pages.** `page_all` becomes a task that sends each page to the UI thread as it
   arrives (a `futures` channel drained by a foreground task, or one `wire_call` per page
   chained): page 1 is folded and drawn before page 2 is requested; each page folds in its own
   update so frames interleave; `HistoryReady` after page 1; `follow` keeps the tail pinned as
   later pages land. Consider a smaller first page (200 events) if the numbers say page 1 of a
   13 MB session is still slow; keep 1000 after that. Report first-content time on both sessions.
4. **Keep what was opened.** `Harness` holds an MRU of the last eight `SessionView`s by id
   (folds already keep an MRU of eight). Switching away parks the view (its subscription
   dropped, its fold, scroll position and draft kept); reopening shows it immediately and tops
   it up: `session/resume { cursor: <SideState::last_cursor> }` (the reconnect procedure in
   `docs/01-transport.md` §3, which serves what happened since), then `view/page` forward from
   that cursor if the resume result says history was not served. Events for a non-active
   session are still dropped by `route`; the top-up is what covers them. Eviction drops the
   view; a hidden/archived session is never cached.
5. **Docs.** `docs/02-app.md` (replace the "never flashes" paragraph with the new contract:
   immediate swap, loading row, streamed pages, MRU), `docs/07-architecture.md` (the
   `app/lifecycle.rs` row), `docs/05-handoff.md` "five things" #2 if it needs a word.

## Regression proof

`scripts/captures.sh <dir>` after the change, `cmp` against
`/private/tmp/claude-501/-Users-latekaapi-Projects-harness/f7a85ce4-a78a-46db-b3ec-e131d3878ca7/scratchpad/set-before`:
all 53 byte-identical (`--replay` never pages, so none of this shows in a capture). Adapter
snapshots unchanged. `--bench` on `synthetic-stress-300` unchanged beyond noise. The trace
numbers before/after for both sessions in a table, plus a live check that switching A → B → A
shows A instantly and its tail matches a fresh open of A (`--steps end` screenshot of each,
compared by eye and described).

## Gates (run all; report verbatim; never claim a gate you did not run)

`cargo build --workspace`; `cargo test --workspace`; `cargo clippy --workspace --all-targets
-- -D warnings`; `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`. Report per item,
the trace table, the capture comparison count, gate output.
