# Brief — the tier probe leaks its `muse` TUI process (Harness)

You are the single implementor for this work package in `/Users/latekaapi/Projects/harness`
(branch `main`). Do NOT commit; leave the tree uncommitted for review. Do not touch
`/Users/latekaapi/Projects/agentic-ui` or `~/Projects/cockpit`. Prefix every shell command
with `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`.

## Spend rule — non-negotiable

Every model turn is billed. Never run the ignored live tests, `harness-probe`, the
`fixtures/msp/probe*.py` scripts, `--send`, or `--steps` containing `send:`/`steer:`. The tier
probe itself is free: it opens the `muse` TUI with no prompt and types `/upgrade`, which
makes no model call. `cargo run -p harness -- --print-tier` is therefore allowed and is your
main reproduction tool.

## The bug

`crates/harness/src/tier.rs` drives the `muse` TUI in a pty (`Pty::open`, `probe`) to read
the billing plan (`docs/06-billing.md`). Evidence gathered 2026-09-09:

- Two processes `muse-bin-1.0.3-R2198.1 --workspace ~/Library/Application Support/harness/tier-probe`
  were found alive for 6 hours: ppid 1, each its own session leader (`Ss+`), the binary file
  already deleted by the launcher's self-update. They ignored SIGTERM and died on SIGKILL.
- `~/.local/bin/muse` is a bash launcher that `exec`s the versioned binary (line ~1135), so
  the pid `Command::spawn` returns IS the TUI process; `Pty`'s `Drop` (`child.kill()` +
  `wait()`) would kill it if it ran.
- So `Drop` did not run for those two: find out why. Candidates to check, in order: the
  probe runs on a background thread and the process exits (`--print-tier` returns, the app
  quits, a `--screenshot` run exits) before the thread finishes and the `Pty` is dropped;
  `pump` or a read blocks past the 20 s ceiling; a panic path that `mem::forget`s or leaks.
  Read `probe`, `Pty::open`, `pump`, `Drop`, and every caller in `main.rs` and `app.rs`
  (`tier_probing`, the boot path, `--print-tier`).

## What to build

1. **Make the leak impossible by construction, on both exits.** Whatever the cause, the
   outcome must be: no `muse` process started by the probe outlives the harness process by
   more than a second, and none outlives the probe's own 20 s ceiling while the harness is
   still running. Acceptable means: join the probe thread with a bounded wait before the
   process exits on the `--print-tier` and screenshot paths; kill with SIGKILL (not SIGTERM,
   the TUI ignores it) on the ceiling; make sure every early-return and error path drops the
   `Pty`. Keep the change inside `tier.rs` and its callers.
2. **Sweep stale probes at the next probe.** Write the child's pid to
   `~/Library/Application Support/harness/tier-probe/probe.pid` when it starts, remove it on
   clean exit, and at the start of the next probe read it and, if a process with that pid is
   still a `muse` probe (check its command line contains `tier-probe`, never kill on pid
   alone), SIGKILL it. This catches a harness that was force-quit.
3. **Test what can be tested offline.** A unit test for the pid-file read/parse/stale logic
   with a fake process check (a closure), and a test that the `Pty` kill path sends SIGKILL
   (e.g. spawn `/bin/sleep 60` through the same code path and assert it is gone after drop).
   No test may open the real `muse`.
4. **Reproduce and verify.** Before the change: run `cargo run -p harness -- --print-tier`
   and immediately `ps -ax | grep "[t]ier-probe"` to see whether a process is left; also try
   the boot path `cargo run -p harness -- --workspace /Users/latekaapi/Projects/harness --screenshot /tmp/t.png --screenshot-delay 3000`
   (free: no prompt) and check `ps` after it exits. After the change, repeat both and show
   `ps` output empty. Quote the before/after `ps` output in the report.
5. **Docs.** Add a `## 2026-09-09 — tier probe leak` entry at the top of `docs/CHANGELOG.md`
   (above the entries already there; the file may have a muse 1.1.1 entry by now) saying
   what leaked, why, and the fix. Add two sentences to `docs/06-billing.md` where the probe's
   lifecycle is described.

## Conventions

Comments explain why, in the style of the surrounding code. No new crates; `libc` is already a
dependency. Muse's own storage is read-only; `~/Library/Application Support/harness` is ours.

## Gates (all must pass before you stop)

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
```

## Report

Root cause with the line(s) that prove it; files changed; the before/after `ps` output; gate
results verbatim; anything you could not do and why.
