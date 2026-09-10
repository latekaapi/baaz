# Brief — adapt the Harness to the library's `improvements-2026-09-10` branch (compile fixes only)

Repository: `/Users/latekaapi/Projects/harness` (branch `main`). The `aui` library at
`/Users/latekaapi/Projects/agentic-ui` is checked out on `improvements-2026-09-10`, which
adds and changes public API (see `git -C ../agentic-ui log main..improvements-2026-09-10 --stat`
and `../agentic-ui/docs/06-api.md`). Do NOT modify the library. Do NOT commit — the owner
commits after review. Do not implement features; this is the smallest change that makes the
harness build, test and pass clippy/docs against the new library.

Prefix every shell command with
`export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`. Spend rule:
never run live tests, `harness-probe`, `probe*.py`, `--send` or `--steps` with `send:`/`steer:`.

1. `cargo build --workspace`; fix every error minimally: new `Block` variants get a
   rendering arm that reuses the closest existing card (e.g. `Block::ToolGroup` renders
   its calls as individual `tool_card`s for now); renamed/re-signatured builders get the
   equivalent call; new required fields get `Default`/false values (e.g.
   `SessionSummary::pinned: false`).
2. Gates: `cargo build --workspace`, `cargo test --workspace`,
   `cargo clippy --workspace --all-targets -- -D warnings`,
   `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`; `cargo tree -d` shows one
   `gpui-pre` and one `gpui-kit`. If a `muse-adapter` snapshot changes (it should not —
   the fold is untouched), stop and say so instead of regenerating.
3. One screenshot: `cargo run -p harness -- --replay fixtures/msp/transcript-real.jsonl --theme dark --screenshot docs/images/adapt-library-dark.png --screenshot-delay 15000`;
   note anything that now looks different (the new menu motion, the markdown rendering).

Report: files changed with one line each, gate results verbatim, screenshot path,
anything that could not be adapted minimally and why.
