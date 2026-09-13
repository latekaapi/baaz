# Brief — owner round 2, surface fix-up (harness)

Repository `/Users/latekaapi/Projects/harness`, branch `owner-round-2-2026-09-13` (checked
out; the surface package is committed on it). Work ONLY there. Do NOT commit. Do not touch
`/Users/latekaapi/Projects/agentic-ui` (its `main` now has `PaletteSection::lead`) or
`~/Projects/cockpit`. Prefix every shell command with
`export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`.
Spend rule (non-negotiable): never `turn/start`, `--send`, `send:`/`steer:` steps, the
ignored live tests, `muse logout`, `account/logout`.

## F1 — The drop card lives inside the palette

File: `crates/harness/src/dialogs.rs` (Projects palette).

The `folder_drop_card` currently floats above the palette as a detached box. Put it in the
Add section's new `.lead(..)` slot (library `PaletteSection::lead`) so it is the first thing
under "ADD", above the recent workspaces, inside the palette card. Remove the floating box.
The hero keeps its own card. Retake `docs/images/projects-palette-dark.png` with
`scripts/captures.sh`'s palette line (byte-identical twice).

## F2 — A held session is a banner, not a dialog

Files: `crates/harness/src/app/lifecycle.rs` (`resume`, `open`), the `report` path.

Clicking a session another host holds shows the generic error dialog ("Session already in
use", Dismiss / Reconnect). Reconnect is the wrong verb: the wire is fine. Route every
session-scoped rejection (`conn::is_session_scoped`) from a direct `resume`/`open` through
the same `lease_notice` banner the reconnect path uses, on that session's view, read-only,
no dialog; the sidebar row stays selectable. Retake `docs/images/round2-session-in-use-dark.png`
the way the surface package did (two processes, no turn).

## F3 — Docs

CHANGELOG "Owner round 2, surface" entry gains two lines; `docs/12-projects.md` §8 notes the
palette lead.

## Gates

`cargo build --workspace`; `cargo test --workspace`;
`cargo clippy --workspace --all-targets -- -D warnings` (last);
`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`; one `gpui-pre` and one
`gpui-kit` in `cargo tree -d`; `UPDATE_SNAPSHOTS=1 cargo test -p muse-adapter` with the diff
read. Report: done / skipped-with-reason, screenshot paths, gate output verbatim (last
lines). When finished write the single word `done` to `/tmp/muse-round2-surface-fixup.done`.
