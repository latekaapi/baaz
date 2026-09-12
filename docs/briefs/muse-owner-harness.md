# Brief — owner round 2026-09-13, harness package

Repository worktree `/Users/latekaapi/Projects/harness-wt-owner`, branch
`owner-round-2026-09-13` (already checked out, off `main`). Work ONLY there. Do NOT commit.
Do not touch `/Users/latekaapi/Projects/harness` (the main checkout — another implementor
is working in it), `/Users/latekaapi/Projects/agentic-ui` or `~/Projects/cockpit`. Prefix
every shell command with
`export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`.
Spend rule (non-negotiable): never `turn/start`, `--send`, `send:`/`steer:` steps, the
ignored live tests, `muse logout`, `account/logout`. Everything you need runs on
`--replay`, `--no-connect`, and `cargo test`. Read `CLAUDE.md`, `docs/05-handoff.md`, then
`docs/diagnosis/owner-round-2026-09-13.md` (the diagnosis of every item below; it is in this
worktree too), then `docs/02-app.md` §3–§5.

Six fixes. Each names its files; stay inside them plus tests and docs. Another implementor
is changing `crates/harness/src/session/render.rs` (`transcript_list`,
`sync_virtual_list`), `crates/harness/src/session.rs` (list state) and `bench.rs` on the
main checkout; do not touch those functions, so the merge stays clean.

## H1 — The new session's row appears at once (item 3)

Files: `crates/harness/src/app/lifecycle.rs` (`new_session`, `load_sessions`),
`crates/harness/src/app.rs` (`route`), `crates/harness/src/sidebar.rs`,
`crates/harness/src/app/list.rs`.

Wire fact (`docs/09-handoff-improvements.md` §5): `session/list` lists a session only after
its log flushes on `turn/completed`; the harness re-reads the list only then. So a new
session has no row until its first turn finishes.

Fix: on the `session/start` result, before `load_sessions`, push a local `SessionEntry` for
`started.session` (`SessionEntry::join(&started.session, None, None)`; label falls to
`UNNAMED`, which reads "New session" — check `sidebar::UNNAMED` — `updated` from
`updated_at`, `turns` 0) and `invalidate_list()`. Mark it local (a new `bool local` field
on `SessionEntry`, default false). On the first `turn/started` for that session
(`route` sees `turn/started` with `sessionId`), set the row's label to the first line of the
prompt the view sent (the view knows the last submitted text: use the fold's user turn
text through the active view — `SessionView` has the turns; add a small accessor
`first_prompt_text()` if none exists), keep `local`, `invalidate_list()`. In
`load_sessions`, when the reply arrives, keep every `local` row whose id is not in the
reply (append it), and drop `local` once the id is listed (the joined row from the wire
replaces it). The empty filter already exempts the open session, so the row is visible.
The row must sort newest-first like any other (its `updated` is now).

Test: a unit test in `sidebar.rs` or `app/list.rs` over the merge rule — a local row not in
the wire list survives, a local row whose id is listed is replaced by the wire row.

## H2 — Links open the right thing (items 6 and 10)

File: `crates/harness/src/session/render.rs`, `reveal_workspace_path` only (leave the
rest of the file alone).

Fault: an absolute href inside the workspace (`/Users/latekaapi/Projects/harness/assets`)
has its leading `/` stripped and is joined onto the workspace again, so it does not
exist. Also `cx.reveal_path` only selects the item in Finder.

Fix: if the path is absolute, use it as is (after the same lexical `..` normalisation);
if relative, join to the workspace. Reject anything outside the workspace as now. Then:
a directory → `cx.open_with_system(&path)` (Finder opens that folder); a file →
`cx.open_with_system(&path)` (the default app). Keep the trailing `:line` strip and the
"No such file" toast for a path that really is missing. Unit-test the pure resolution
(factor `resolve_workspace_path(workspace, raw) -> Result<PathBuf, Escape>` out and test
absolute-inside, relative, absolute-outside, `..` escape, `:12` suffix).

## H3 — Sessions view menu anchored to its button (item 7)

File: `crates/harness/src/sidebar_view.rs` (`render_view_menu`, `render_sidebar`).

Fault: the popover is placed at a hard-coded `top(140) left(12)`.

Fix: anchor it under the sliders icon, right edge aligned to the sidebar's content edge.
The library's `sidebar_view(...).on_view_options(...)` hands you `window` and `cx`, not
bounds, so measure: wrap the sidebar column's caption area or the whole `sidebar_view` in
a `div().on_children_prepainted(...)` that records the bounds you need (the caption row is
the first child of the view; the icon sits at its right end), store them on `Harness`
(a `Cell<Option<Bounds<Pixels>>>` or plain field set from the listener through
`cx.entity()`), and position the popover at `top = caption.bottom + 4`, `right = sidebar
width − caption.right`. With the sidebar resized, it must follow. Read
`aui::nav::sidebar_view` to see how the caption row is built; if the library exposes the
caption row's bounds a simpler way, use that. `--steps view-menu` with
`--replay fixtures/msp/transcript-real.jsonl --screenshot` at two sidebar widths
(`sidebar-width:240;view-menu` and `sidebar-width:360;view-menu`) proves it; the retakes
go to `docs/images/owner-view-menu-{240,360}-dark.png`.

## H4 — The rename field keeps the row's height (item 8)

File: `crates/harness/src/sidebar_view.rs` (`rename_field`).

Fault: the field wrapper is `h_auto` and the editor's own line box plus its internal
padding is taller than the row's title line, so the row grows and the list below jumps.

Fix: bound the wrapper to the title line's height (`scale` token for the row title line
height; the row title is `FS_13` at `LH_TIGHT` — read `aui::nav::session_row` to take the
same constant, e.g. a `pub const` the library exports, or `scale::H_XS`), `py(0)`, keep
`px(6)` and the 1 px border; the editor centred inside. Verify with `--replay
fixtures/msp/transcript-real.jsonl --steps "rename:x" --screenshot
docs/images/owner-rename-dark.png`: the renamed row is the same height as its neighbours
(compare pixel rows against a capture without the step).

## H5 — Empty state centred (item 11)

File: `crates/harness/src/transcript.rs` (`empty_state`).

Fault: `suggestion_chips` is `w_full` and left-aligned under a centred title.

Fix: wrap the chips in `div().flex().justify_center().max_w(px(...))` inside the column
so the three chips sit centred under the title (the harness measure is
`TRANSCRIPT_MEASURE` in `session.rs`; re-export or duplicate as a named constant in
`transcript.rs` rather than reaching across). Capture: `--no-connect` is the login screen,
so use `--replay fixtures/msp/transcript-real.jsonl` is not empty either — add a `--steps
"clear"`-free route: the empty state is what `SessionView` draws with no turns, so a
synthetic capture with only `session/branchChanged` (copy the first two `<-- ` lines of
`fixtures/msp/transcript-real.jsonl` into `fixtures/msp/synthetic-empty.jsonl`, header
comment like the other synthetic files) replays to it. Screenshot both themes to
`docs/images/owner-empty-{dark,light}.png`.

## H6 — "Finishing up…" after the reply (item 12)

Files: `crates/harness/src/session/render.rs` (`render_status` only),
`crates/harness/src/session.rs` (a small accessor).

Wire fact (measured on a real capture): after the turn's `agentMessage` item completes
and `session/tokenUsage` arrives, Muse runs `reminderChild` items for 30–70 s before
`turn/completed`. The harness shows "Working…" the whole time; the owner reads it as stuck.

Fix: while `running` is set and the running turn's last text block is complete (not
streaming) and the fold has seen the turn's `session/tokenUsage` (or, simpler and enough:
the turn's assistant text block is complete and no tool call in the turn is in progress),
the status row reads "Finishing up…" with the same elapsed clock and the `esc` hint, and
a note "memory reminders" so the reader knows what Muse is doing. Add
`SessionView::reply_complete_for_running_turn() -> bool` reading the cached turns. Test it
with the existing `synthetic-reminderchild.jsonl` fixture: a `--replay` of a capture cut
just before its final `turn/completed` (make `fixtures/msp/synthetic-finishing.jsonl` from
`synthetic-reminderchild.jsonl` minus the last `turn/completed` line, header-commented)
shows "Finishing up…"; a unit test on the accessor with a two-block turn (text complete,
no tool in progress) returns true, and with a streaming text block returns false.

## Gates

`cargo build --workspace`; `cargo test --workspace`;
`cargo clippy --workspace --all-targets -- -D warnings`;
`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`; exactly one `gpui-pre` and one
`gpui-kit` in `cargo tree -d`; `UPDATE_SNAPSHOTS=1 cargo test -p muse-adapter` then read
the diff (only the new synthetic fixtures' snapshots may appear). Add a CHANGELOG entry
under a new heading "2026-09-13 — Owner round" listing H1–H6 in one line each, and update
`docs/02-app.md` where it describes the sidebar list (H1) and the status row (H6). Report
per item: done / skipped-with-reason, test names, screenshot paths, gate output verbatim
(last lines). When finished write the single word `done` to `/tmp/muse-owner-harness.done`.
