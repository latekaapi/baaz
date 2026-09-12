# Brief — D2: performance, rendering (Harness)

Repository `/Users/latekaapi/Projects/harness`, branch `main`, clean tree. Do NOT commit.
Do not touch `/Users/latekaapi/Projects/agentic-ui` or `~/Projects/cockpit`. Prefix every
shell command with `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`.
Spend rule: never `turn/start`, `--send`, `send:`/`steer:`, live tests, `muse logout`,
`account/logout`. Read `docs/audit/01-plan.md`, `docs/02-app.md` "Measuring", then the
findings named below in `docs/audit/performance.md` and `docs/audit/support.md`.

Measure first with `--bench` on `synthetic-stress-300.jsonl` and `transcript-echo.jsonl`
(debug), keep a before/after table per item; `bench-element` and `bench-frame` are the
metrics that should move here.

## Scope, in this order

- **D-PERF-1** (`performance-2`): cache the pending approval id and choices in `apply`;
  no transcript-wide scan in render.
- **D-PERF-3** (`performance-5`, `support-2`, `performance-6`, `support-3`, `performance-7`):
  cache the sorted visible session list and its grouping, invalidated only when sessions,
  index, or the show/hide/empty flags change; one clock read per frame (already through
  `clock.rs`), elapsed labels quantised to the minute; palette rows computed once per
  frame and shared between render and handlers, selection carrying the row id.
- **D-PERF-4** (`performance-8`): composer emptiness flag maintained on change events.
- **D-PERF-9** (`performance-14`): image read + decode + thumbnail on `background_spawn`
  with a placeholder chip until it resolves; the `image:<path>` step waits for it.
- **D-PERF-10** (`performance-13`): every caret/shimmer/tween clock gated on its visible
  active state; `--bench`'s idle count must stay 0 and a new bench scenario with a
  streaming turn left open must report the frames it needs and no more.
- **D-PERF-11** (`performance-15`): screenshot resize/encode/write off the UI thread.
- **D-PERF-14** (`support-10`): 10 s timeout on `skills::list`.
- **D-PERF-15** (`support-11`): pin the tier card sentences in a test; distinct
  stale-probe signal.
- **D-PERF-16** (`support-12`): cache `auth.json` name/email by mtime.
- `performance-9`: window title and search status cached, invalidated on change.

Constraints: reference captures byte-identical (the placeholder chip must not appear in
any existing capture — none attach an image); adapter snapshots unchanged.

## Gates

`cargo build --workspace`; `cargo test --workspace`;
`cargo clippy --workspace --all-targets -- -D warnings`;
`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`;
`UPDATE_SNAPSHOTS=1 cargo test -p muse-adapter` (no diff). Report per finding id, the bench
table, capture comparison count, gate output verbatim.
