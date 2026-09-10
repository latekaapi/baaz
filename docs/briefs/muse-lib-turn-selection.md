# Brief — thread text selection through `UserTurn`/`AssistantTurn` (single small package)

Repository: `/Users/latekaapi/Projects/agentic-ui`, branch `improvements-2026-09-10` (work
directly on it; commit on it when the gates pass; do not create worktrees; do not touch
`/Users/latekaapi/Projects/harness`). Prefix every shell command with
`export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`.

Gap found by the consumer: `markdown(...)` has `.selection(..)`, `.on_selection_change(..)`
and `selected_text(..)` (`crates/aui/src/transcript/markdown.rs`, `selectable.rs`), but
`UserTurn` and `AssistantTurn` (`crates/aui/src/transcript/turns.rs`) do not expose them,
so an app rendering turns cannot show or report a selection.

1. Add `.selection(Option<TextSelection>)` and `.on_selection_change(handler)` builders to
   both turn components, passed straight through to their inner `markdown(...)`, in the
   same style as the existing `.on_link(...)` passthrough. Add a free helper
   `turn_selected_text(markdown_source, &TextSelection) -> Option<String>` (or document
   that `Markdown::selected_text` on a freshly built `markdown(source)` is the way) so the
   app can copy without re-rendering.
2. Gallery: the `transcript/turns` card holds an `Option<TextSelection>` and wires it, so a
   drag inside a turn highlights (legend line added).
3. Gates: `cargo build --workspace`; `cargo build --workspace --features aui-webview/wry,aui-terminal/pty,aui-terminal/tui`;
   `cargo test --workspace`; `cargo clippy --workspace --all-targets -- -D warnings`;
   `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`; `python3 scripts/api-doc.py`.
   Commit (message ending `Co-Authored-By: Muse Code <noreply@meta.com>`).

Report: files changed, API added, gate results verbatim. Never claim a gate you did not run.
