# Brief — sidebar noise: empty sessions (Harness, Muse Code evaluation run)

You are the single implementor for this work package in `/Users/latekaapi/Projects/harness`
(branch `main`, Rust 2021, a gpui app). You own it end to end: code, tests, screenshots,
CHANGELOG entry. Do NOT commit; leave the tree uncommitted for review.
Do not touch `/Users/latekaapi/Projects/agentic-ui` (the `aui` library this app depends on by
path) or `~/Projects/cockpit`. Prefix every shell command with
`export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`.

## Spend rule — non-negotiable

There is NO free provider on this machine; every model turn is billed. Never run the ignored
live tests, `harness-probe`, the `fixtures/msp/probe*.py` scripts, `--send`, or `--steps`
containing `send:`/`steer:`. Free and allowed: `--replay <capture>`, `--no-connect`,
`--workspace <dir>` with NO prompt (it starts a session and lists sessions, no model call),
`cargo build/test/clippy/doc`.

## The problem

The sidebar lists every Muse session in the workspace. Screenshot and test runs leave dozens
of sessions that never had a turn, so the list is mostly noise. Rows already know their turn
count: `SessionEntry.turns` in `crates/harness/src/sidebar.rs` comes from `session/list`'s
`turnCount`. Sessions can already be hidden per row (`SessionMeta.hidden` in
`crates/harness/src/sessions.rs`, persisted to
`~/Library/Application Support/harness/sessions.json`) and the sidebar footer has a
"Show hidden (n)" toggle (`crates/harness/src/app.rs`: `show_hidden`, `visible_sessions`,
the footer near `toggle_hidden`, and the `/hidden` arm in the `/` command match).

## What to build

1. **Empty sessions are hidden by default.** A session is *empty* when `turns == 0`, it is
   not the active session in this window, it is not running, and it has no user-given name
   (`SessionMeta.name`). Put this rule in ONE pure function in `sidebar.rs`
   (e.g. `SessionEntry::is_empty(&self, active: Option<&str>) -> bool`) and use it from
   `visible_sessions`. The active session is never filtered: a fresh session the user just
   created must stay visible.
2. **"Show empty (n)" toggle** in the sidebar footer, next to the existing "Show hidden (n)"
   button, same style (`button(..).ghost().xs()`), shown only when n > 0. Field
   `show_empty: bool` on the app entity, default false. Also an `/empty` arm in the `/`
   command match, next to `/hidden`, that toggles it. Register the command wherever
   `/hidden` is registered so it appears in the `/` menu with a one-line description.
3. **"Clear empty" action**: a button in the same footer row, shown only when the toggle is
   on and n > 0, that sets `hidden = true` on every currently-empty session through the
   existing `set_override` path (so it persists) and shows a toast with an Undo action,
   following the existing hide/undo pattern (`hidden_undo`; extend it so one undo restores
   the whole batch). This is local state, so applying it immediately is correct.
4. **Empty state text.** When every visible session was filtered out only because it is
   empty, the sidebar's empty state (see `hidden_only` near the end of `app.rs`) must say so
   ("Only empty sessions here" / "Turn on “Show empty” below to see them.") rather than the
   generic text.
5. **Tests.** Unit tests in `sidebar.rs` for the rule: zero turns hidden; active exempt;
   running exempt; named exempt; a session with turns never empty. Tests in `app.rs` are not
   expected (the entity needs a window); keep the logic in the pure function so it is
   testable.
6. **Docs.** Add a dated entry (2026-09-09) at the top of `docs/CHANGELOG.md` under a heading
   "Improvements — sidebar noise" describing the rule, the toggle, the action and the
   command. Add the `/empty` command to the command table in `docs/03-composer.md` §5 and the
   footer buttons to `docs/02-app.md` where the sidebar footer is described (grep for
   "Show hidden").
7. **Screenshots.** Two PNGs to `docs/images/improve-sidebar-empty-{dark,light}.png` from
   `cargo run -p harness -- --workspace /Users/latekaapi/Projects/harness --theme dark --screenshot <path>`
   (no `--send`, no prompt: this is free). The screenshot should show the footer with the
   new toggle. If the app needs a screenshot delay use `--screenshot-delay`.

## Conventions

- Read `CLAUDE.md`, `docs/05-handoff.md` and the sidebar section of `docs/02-app.md` before
  editing.
- No literal colours, sizes or durations in the app; everything visible is an `aui`
  component fed data. Reuse `button`, `toast_with_action` and the footer builder as the
  existing code does.
- Muse's own storage (`~/.config/muse`, `~/.local/share/muse`) is read-only.
- Keep names plain and comments in the style of the surrounding code (they explain *why*).
- Do not widen scope: no library changes, no new crates, no refactors beyond what the
  feature needs.

## Gates (all must pass before you stop)

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
```

If `cargo test -p muse-adapter` snapshot tests fail because of your change (they should
not), do NOT regenerate them; report it.

## Report

Finish with a short report: files changed, the rule as implemented, gate results verbatim
(pass/fail per gate), screenshot paths, anything you could not do and why. Do not claim a
gate passed that you did not run.
