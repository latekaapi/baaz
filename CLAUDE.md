# Harness — project instructions

A macOS gpui chat interface to Meta's Muse Code agent (`muse` CLI 1.1.1; `muse serve` =
"MSP", JSON-RPC 2.0 as NDJSON over stdio), built on the `aui` component library at
`/Users/latekaapi/Projects/agentic-ui` by **path dependency** (gpui-pre 0.3.3 + gpui-kit 0.6).
The first slice — a feature-complete chat app — is finished (five phases, 2026-09-08/09).
This project now does improvements, fixes and the next slices.

## Read first, in this order

1. `docs/09-handoff-improvements.md` — the long-form handoff: state, decisions D1–D21, wire
   facts, incidents, limitations, candidate improvements, how to work.
2. `docs/05-handoff.md` — the short maintenance rules.
3. `docs/06-billing.md` and `docs/04-approvals.md` §0 — what costs a turn.
4. `docs/00-spec.md` (frozen spec), `docs/CHANGELOG.md`, then whichever of
   `docs/01-transport.md`, `02-app.md`, `03-composer.md`, `04-approvals.md`,
   `07-architecture.md`, `08-keymap.md` your change touches.
5. Library rules: `/Users/latekaapi/Projects/agentic-ui/docs/00-agent-brief.md` and
   `docs/04-design-rules.md`; API overview `docs/06-api.md`; Muse research
   `docs/10-muse-research.md` (its §2.4 claim that echo is free is wrong).

## The spend rule (non-negotiable)

There is **no free provider** on this machine. `--provider echo` picks a route, not a bill:
a signed-in login routes echo to the real model. Anything that reaches `turn/start` is
billed on the login's tier. Free: `--replay <capture>`, `--no-connect`, `session/start`,
`session/userShell`, `approval/*`, `userInput/*`, `session/fork`, `session/list`,
`view/page`, `model/list`, and the `muse` TUI opened without a prompt. Never run the
ignored live tests, `--send`, or `--steps` containing `send:`/`steer:` unless the owner has
named the turn (`harness-probe` and the `fixtures/msp/probe*.py`/`run*.py` scripts that used
to carry this warning were removed 2026-09-12; git history has them). Count spend from
Muse's own logs, never from a report:

```sh
grep -c runtime.user_intent.accepted ~/.local/share/muse/sessions/*/*/*/*/session.jsonl | awk -F: '{s+=$2} END {print s}'
```

Check which plan the login is on before any real session: `cargo run -p harness -- --print-tier`.

## Shell and build

Prefix every shell command with
`export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`.

```sh
cargo run -p harness -- --replay fixtures/msp/transcript-approve.jsonl --theme dark
cargo run -p harness -- --no-connect --screenshot /tmp/shot.png
cargo test --workspace                          # replay, parity and unit tests; spawns nothing
UPDATE_SNAPSHOTS=1 cargo test -p muse-adapter   # regenerate fold snapshots, then READ the diff
```

Gates before every commit: `cargo build --workspace`, `cargo test --workspace`,
`cargo clippy --workspace --all-targets -- -D warnings`,
`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`, exactly one `gpui-pre` and one
`gpui-kit` in `cargo tree -d`, snapshots regenerated and read.

## Library changes

Go in `/Users/latekaapi/Projects/agentic-ui` on a **new branch off `main`** (the whole stack through `transcript-2026-09-12`
is merged), as their own commits, under the library's gates: build, the all-features build
(`--features aui-webview/wry,aui-terminal/pty,aui-terminal/tui`), test, clippy
`-D warnings`, rustdoc `-D warnings`, `python3 scripts/api-doc.py`, and a gallery entry for
anything new. Library rules: no literal colours/sizes/durations, stateless `RenderOnce`
components with intents out, `popover_layer` for overflow, both themes.

## Conventions

- Nothing is optimistic: chips, rows and cards move only on the server's notification.
- Muse's storage (`~/.config/muse`, `~/.local/share/muse`) is read-only; the harness's own
  state lives under `~/Library/Application Support/harness`.
- Secrets, device-code URLs and codes never reach a log.
- Rust 2021, `rust-version` matching agentic-ui. Commit messages end with
  `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`.
- Do not touch `~/Projects/cockpit`.

## Working model

Fable (Claude) designs, writes briefs (`docs/briefs/` has the shape), audits diffs, reruns
one gate, reads the screenshots, counts spend and commits. **Muse Code implements**:
`muse exec --json --workspace "$PWD" --trust-workspace --approval-mode never
--disable-sandbox --user-input-auto-resolve --max-model-steps N --prompt-file
docs/briefs/<brief>.md` for one package; for parallel packages, prompt a workflow ("use a
workflow with N children, one per task, each in its own sibling worktree
`../harness-wt-<task>`, then one integration step") with `--parallel-tool-calls`. Launch it
with `nohup` from a small script (a Claude Bash call times out at 10 min; `setsid` does not
exist on macOS) and watch a sentinel file, the worktrees' git state, or the session's
`session.jsonl`. Briefs say "do NOT commit" unless a branch is named; library changes run
first as their own package. Operator guide: `docs/11-muse-workflows.md`.
