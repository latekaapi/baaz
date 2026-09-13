# Brief — owner round 2, library fix-up: the footer keeps its name

Repository `/Users/latekaapi/Projects/agentic-ui`, branch `owner-round-2-2026-09-13` (checked
out; the round-2 library package is on it, uncommitted — keep every change in the tree and add
yours). Work ONLY there. Do NOT commit. Do not touch `/Users/latekaapi/Projects/harness` or
`~/Projects/cockpit`. Prefix every shell command with
`export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`.

## F1 — The name never disappears

File: `crates/aui/src/nav/parts.rs` (`SidebarFooter`), gallery `sidebar/sidebar`.

Fault (gallery `sidebar/sidebar`, footer "Bharani · Max" with plan "Power Usage" and a
meter, no detail): the footer renders `L  · Power Usage [meter] 78%` — the name has
truncated to nothing because the plan label holds its room and the meter shares the same
row.

Rule: the name is the one thing the footer always shows. Layout for the name row: the name
truncates but never below `NAME_MIN` (72 px); the plan label sits after it and is the first
to give way (truncate, then drop entirely below 40 px of room); the meter never sits on the
name row — it stays on the second row (with the detail when there is one; alone when there
is not), exactly where it was before the round-2 change. With detail present: row one is
`name · plan`, row two is `detail … meter`. Without detail: row one `name · plan`, row two
the meter. The chevron stays at the right of row one.

Proof: the gallery `sidebar/sidebar` card footer reads "Bharani · Max · Power Usage" with
the meter below; add a second footer state to the `sidebar/rows` or `sidebar/sidebar` card
at 200 px width showing the name intact and the plan dropped. Screenshot both themes to
`/tmp/aui-round2/footer-fixup-<theme>.png`.

## Gates

`cargo build --workspace`; the all-features build
`cargo build --workspace --features aui-webview/wry,aui-terminal/pty,aui-terminal/tui`;
`cargo test --workspace`; `cargo clippy --workspace --all-targets -- -D warnings` (last);
`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`; `python3 scripts/api-doc.py`.
Report: done / skipped-with-reason, screenshot paths, gate output verbatim (last lines).
When finished write the single word `done` to `/tmp/muse-round2-lib-fixup.done`.
