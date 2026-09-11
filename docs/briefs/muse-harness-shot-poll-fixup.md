# Brief — `--screenshot` starves foreground tasks unless `--steps` is set (tiny harness fix-up)

Repository `/Users/latekaapi/Projects/harness`, branch `main`, tree already carries the
uncommitted sign-in-over-the-wire work: keep it, do NOT commit, do not touch agentic-ui or
cockpit. Prefix every shell command with
`export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`. Spend rule as in
`docs/briefs/muse-harness-login-wire.md`: never `turn/start`, never `account/logout`,
never `muse logout`, never a real key.

## The finding (reproduced three ways)

`./target/debug/harness --theme dark --screenshot-delay 8 --screenshot /tmp/x.png` prints
`connected` and nothing else: the `probe_account` continuation (`cx.spawn` in
`crates/harness/src/app.rs`) is never polled, so `account/read`'s answer (which the wire
capture shows arriving) is never applied and `--login-steps` never runs. The same command
with `--steps 'wait:100'` added prints `account → loggedOut`, `login → choose`, and windowed
runs are fine. Cause: `crates/harness/src/shot.rs::capture_and_quit` sleeps the whole delay
on one `timer(delay)`; with `await_steps` it instead polls `STEPS_RUNNING` every 100 ms,
and that periodic wake-up is what keeps the foreground executor draining background
completions in the off-screen window. `--login-steps` neither sets `await_steps` nor
raises `STEPS_RUNNING`.

## What to change

1. `shot.rs`: replace the single settling `timer(delay)` waits (both the initial one and
   the post-wait one) with a loop of `POLL` timers until the deadline, so a headless capture
   always keeps the foreground executor ticking. Document why in the rustdoc, citing this
   finding.
2. `main.rs`/`app.rs`: `--login-steps` sets `await_steps` exactly as `--steps` does, and
   `run_login_steps` raises/clears `shot::set_steps_running` around its loop the way the
   `--steps` loop does, so the capture waits for the login script to finish before the
   settling delay.
3. Verify (the machine is signed in on the API-key lane; that is fine — the login screen still shows if you pass `--login-steps`? No: with a stored key the app signs straight in, so verify instead that `account → apiKey` prints and the PNG shows the signed-in shell with the "Pay-as-you-go · API key" footer; then the run without steps must print the same line): `cargo run -q -p harness --
   --theme dark --login-steps 'apikey;wait:500' --screenshot-delay 8 --screenshot /tmp/x.png`
   must print `account → loggedOut`, `login → choose`, `login → apikey` and the PNG must show
   the API-key form (read it). Then the same without any steps must print the first two.

Gates: `cargo build --workspace`; `cargo test --workspace`;
`cargo clippy --workspace --all-targets -- -D warnings`;
`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`. Report the diff and the stderr
of the two verification runs verbatim. Never claim a gate you did not run.
