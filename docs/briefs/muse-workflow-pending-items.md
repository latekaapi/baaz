# Workflow brief — five pending Harness items, in parallel, then integrate

Use a workflow with five children, one per task below, each in its own isolated git
worktree, then one integration step. You are the orchestrator; the children implement.
Repository: `/Users/latekaapi/Projects/harness` (branch `main`, Rust 2021, gpui app). The
`aui` library it depends on by RELATIVE path (`../agentic-ui/crates/*`) is at
`/Users/latekaapi/Projects/agentic-ui` (branch `main`, clean).

## Hard rules for every child and for you

- Prefix every shell command with
  `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`.
- **Worktrees must be siblings of the repo**: create them as
  `/Users/latekaapi/Projects/harness-wt-<task>` (e.g. `git worktree add ../harness-wt-b -b wf/b main`)
  so `../agentic-ui` still resolves. If `cargo build` cannot find the aui crates, the
  worktree is in the wrong place; move it, never edit `Cargo.toml` paths.
- **Spend rule.** Every model turn is billed. No child may run the ignored live tests,
  `harness-probe`, `fixtures/msp/probe*.py`, `--send`, or `--steps` with `send:`/`steer:`.
  Screenshots come from `--replay fixtures/msp/<capture>.jsonl` or `--no-connect` only.
- **Git, explicitly requested here:** each child commits its finished work on its own branch
  `wf/<task>` in its worktree (message ending with the line
  `Co-Authored-By: Muse Code <noreply@meta.com>`). Nobody commits to, rebases, or resets
  `main` in either repository. The integration step merges the `wf/*` branches into a new
  branch `wf-pending` off `main` and commits the merges there. `main` is untouched.
- Muse's own storage (`~/.config/muse`, `~/.local/share/muse`) is read-only. The harness's
  state lives under `~/Library/Application Support/harness`.
- Read `CLAUDE.md`, `docs/05-handoff.md`, `docs/09-handoff-improvements.md` §4 (decisions
  D1–D21) and the phase doc your task names before editing. Conventions: nothing is optimistic
  (UI moves only on the server's notification); no literal colours/sizes/durations; comments
  say why, in the surrounding style; do not widen scope or refactor beyond the task.
- Each child's gates, all must pass before it commits:
  `cargo build --workspace`, `cargo test --workspace`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`.
  If a `muse-adapter` snapshot changes, run `UPDATE_SNAPSHOTS=1 cargo test -p muse-adapter`,
  read the diff, and quote it in the child's summary.
- Each child ends with a summary: files changed, what was done, gate results verbatim,
  anything not done and why. No child claims a gate it did not run.
- Each child adds its own dated entry (2026-09-09) to the top of `docs/CHANGELOG.md` under a
  heading naming its task letter and title; the integrator resolves the resulting merge
  conflicts in that file by keeping all entries.

## Task A — reasoning effort `max` (library + picker)

The wire enum gained `max` in muse 1.1.1 (`docs/10-msp-1.1.1-diff.md`,
`docs/01-transport.md` §4 item 6). The harness picker cannot offer it because the tiers come
from `aui_protocol::ReasoningEffort` (`/Users/latekaapi/Projects/agentic-ui/crates/aui-protocol/src/session.rs` ~line 160),
which has no `Max`.

- In agentic-ui: create branch `effort-max` off `main` (work directly in that repo; it is not
  worktreed), add `Max` between `Xhigh` and `Ultra` with its label, update every exhaustive
  match, the gallery if it lists tiers, and run the library's gates: `cargo build --workspace`,
  the all-features build `cargo build --workspace --features aui-webview/wry,aui-terminal/pty,aui-terminal/tui`,
  `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`, `python3 scripts/api-doc.py`.
  Commit on `effort-max`. Then **check the branch out** so the harness sees it
  (`git checkout effort-max`); say so in the summary so the reviewer knows the library is on
  a branch.
- In the harness worktree: map `Max` to the wire (`crates/harness/src/session.rs` ~line 3045,
  `Wire::Max`), add it to the picker tiers (`crates/harness/src/overlays.rs` ~line 360),
  update `docs/03-composer.md` and retire the "picker omits max" sentences in
  `docs/01-transport.md` §4 item 6 and `docs/10-msp-1.1.1-diff.md`.

## Task B — window close kills live tier probes

`crates/harness/src/tier.rs` now has `kill_live_probes()` and `wait_for_probes_gone()`; the
`--screenshot` quit path calls them (`crates/harness/src/shot.rs`), but closing the window in
an interactive run still orphans the `muse` TUI until the next probe's sweep (see the
"tier probe leak" CHANGELOG entry and `docs/06-billing.md`). Find the app's window-close or
app-quit path in `crates/harness/src/main.rs` / `app.rs` (gpui offers `cx.on_app_quit` and
window `on_should_close`-style hooks; use whichever the app already has or the least
invasive one) and call the same two functions with the same 3 s bound. Verify by reading the
code path; offline tests are not expected. Two sentences in `docs/06-billing.md`.

## Task C — "Show full output" for truncated tool output

muse 1.1.1 added `item/readOutput` (`docs/10-msp-1.1.1-diff.md`; client wrapper
`MuseClient::item_read_output` in `crates/muse-client/src/client.rs`; types in
`schema.rs`). Items whose output was truncated carry an `outputRef` (search `output_ref` /
`outputRef` in `schema.rs` and `crates/muse-adapter/src/fold.rs`). Build: when a tool or
user-shell block's output is truncated and an `outputRef` exists, the card offers a "Show
full output" action; the app fetches pages with `item/readOutput` on a background task
(`docs/02-app.md` §3: commands never on the UI thread), concatenates `utf8` pages up to a
sane cap (e.g. 2 MiB, then "truncated at 2 MiB"), and replaces the block's body on the
server's result (D4: nothing optimistic). Keep the affordance in the existing card via
whatever action slot the library's tool card already has; if the card has no action slot,
put the action in the block's existing menu/affordance and say so; do NOT change the
library. If no fixture capture carries an `outputRef`, add a `synthetic-readoutput.jsonl`
capture (name it `synthetic-*`, say which lines are real) so `--replay` shows the affordance,
and a replay snapshot. Screenshot to `docs/images/improve-full-output-dark.png`.

## Task D — "thought silently" on reasoning-only turns

Handoff item: a turn can bill reasoning tokens with no reasoning item; the per-turn footer
shows the count but nothing says the model thought without showing it. In
`crates/muse-adapter/src/fold.rs` (`reasoning` usage ~line 109, `reasoning_tokens` ~line
615) and the transcript footer in `crates/harness/src/transcript.rs` / `session.rs`: when a
completed turn has `reasoning_tokens > 0` and no reasoning block, the footer says
"Thought silently" (with the count, in the footer's existing style). Unit test on the pure
decision; replay screenshot from `fixtures/msp/transcript-real.jsonl` or whichever capture
has such a turn (check the captures' `session/tokenUsage`); if none has one, a
`synthetic-silent-reasoning.jsonl` capture. Update `docs/02-app.md` where the footer is
described.

## Task E — fork picker

`/fork` takes the newest completed turn (`crates/harness/src/session.rs` `fn fork` ~line
2671, dispatched at ~1360 and ~1633); the per-turn action forks from that turn. Add a
picker: `/fork` with no argument opens a palette (the app has a palette in `app.rs`,
`PaletteKind`, used by `/resume`) listing completed assistant turns newest first, each row
the user prompt's first line and the turn's time; choosing one calls `fork(Some(turn_id))`.
`/fork <n>` forks from the n-th newest without the palette. Nothing optimistic: the new
session appears when `session/fork`'s result and the list refresh arrive, as today.
Update `docs/03-composer.md` §5. Replay screenshot of the picker to
`docs/images/improve-fork-picker-dark.png` (use `--steps` to open it if a verb exists;
otherwise add a `fork-picker` verb next to the existing `resume`/`hidden`/`empty` verbs).

## Integration step (after all five children report)

1. Create `wf-pending` off `main` in the main harness checkout, merge `wf/a`, `wf/b`,
   `wf/c`, `wf/d`, `wf/e` in that order, resolving conflicts (CHANGELOG: keep every entry,
   newest task on top; code: keep both sides' intent, rebuild).
2. With agentic-ui on `effort-max`, run the four harness gates on `wf-pending` and
   `cargo tree -d | grep -E "^gpui-(pre|kit) "` (must print nothing).
3. Remove the five worktrees (`git worktree remove`), keep the `wf/*` branches.
4. Final report: per child — summary, gates, commit hash; the merge — conflicts and how they
   were resolved; integrated gate results verbatim; the state of both repos (branches,
   heads); anything left undone. Do not touch `main` in either repo.
