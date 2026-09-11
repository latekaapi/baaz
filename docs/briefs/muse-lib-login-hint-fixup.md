# Brief — Device state: the hint must not share the action row (tiny library fix-up)

Repository: `/Users/latekaapi/Projects/agentic-ui`, branch `login-methods` (checked out;
commit on it when the gates pass, message ending `Co-Authored-By: Muse Code <noreply@meta.com>`).
Do not touch `/Users/latekaapi/Projects/harness` or `~/Projects/cockpit`. Prefix every shell
command with `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`.

In `crates/aui/src/screens/login.rs`, the `Device` state now has three buttons (Cancel, Copy
code, Open in browser), so the action row's hint truncates ("expires in 1…" in the gallery
capture), and the fallback sentence "Approve the request in your browser, then come back
here." can never fit. Fix: in `Device`, draw the hint (the `expires` text, or that sentence)
as its own full-width muted line (the `STATUS_TEXT` size, `ink_3`, left-aligned, wrapping
allowed) directly above the action row, and pass `None` as the action row's hint. Nothing
else changes; every other state keeps its layout. Update the gallery legend line for Device
if it mentions the hint's position. Re-take the gallery `login` capture in both themes
(`/tmp/login-dark.png`, `/tmp/login-light.png`) and inspect them.

Gates, all must pass: `cargo build --workspace`;
`cargo build --workspace --features aui-webview/wry,aui-terminal/pty,aui-terminal/tui`;
`cargo test --workspace`; `cargo clippy --workspace --all-targets -- -D warnings`;
`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`; `python3 scripts/api-doc.py`.
Report the diff summary and gate results verbatim. Never claim a gate you did not run.
