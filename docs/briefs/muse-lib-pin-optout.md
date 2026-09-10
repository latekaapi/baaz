# Brief — opt out of the Pin action on transcript turns (tiny library package)

Repository: `/Users/latekaapi/Projects/agentic-ui`, branch `improvements-2026-09-10`
(work on it directly, commit on it when the gates pass, message ending
`Co-Authored-By: Muse Code <noreply@meta.com>`). Do not touch
`/Users/latekaapi/Projects/harness`. Prefix every shell command with
`export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`.

`AssistantTurn` (`crates/aui/src/transcript/turns.rs`) draws the Pin action unconditionally
in both the hover toolbar and the `actions_bottom` row. Add a builder
`.actions(&[AssistantTurnAction])` (explicit action set, default = today's four) — or, if
the enum is not `Copy`, a `.without_pin()` toggle — so a consumer can hide actions that
have no meaning for it; same for `UserTurn` (`.actions(&[UserTurnAction])`). Both rows
honour it. Gallery `transcript/turns`: one state showing a reduced action set (legend line).
Regenerate `docs/06-api.md`.

Gates, all must pass: `cargo build --workspace`;
`cargo build --workspace --features aui-webview/wry,aui-terminal/pty,aui-terminal/tui`;
`cargo test --workspace`; `cargo clippy --workspace --all-targets -- -D warnings`;
`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`; `python3 scripts/api-doc.py`.
Report files changed, the API added, gate results verbatim. Never claim a gate you did not run.
