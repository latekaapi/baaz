# Brief — P0: deterministic captures and a real `--bench` mode (Harness)

Repository `/Users/latekaapi/Projects/harness`, branch `main`, clean tree. Do NOT commit.
Do not touch `/Users/latekaapi/Projects/agentic-ui` or `~/Projects/cockpit`. Prefix every
shell command with `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`.
Spend rule: never `turn/start`, `--send`, `send:`/`steer:`, live tests, `harness-probe`,
`fixtures/msp/probe*.py`, `muse logout`, `account/logout`. Free: `--replay`, `--no-connect`,
cargo gates. Read `docs/audit/01-plan.md` first, then the findings named below in
`docs/audit/performance.md` and `docs/audit/app-core.md`.

## 1. Deterministic captures — `HARNESS_DETERMINISTIC=1`

Two identical `--replay … --screenshot` runs differ today in up to 10 % of pixels
(measured on 43 of 47 reference captures): relative time labels ("now", "2m"), the
composer caret blink, spinners, shimmers and entrance animations captured mid-flight.
With `HARNESS_DETERMINISTIC=1` in the environment a capture must be byte-identical run to
run. Do it at the sources, not by sleeping:

- one clock: every `Local::now()` / `Instant::now()` used for labels or elapsed text goes
  through a `clock()` helper in one module; under the flag it returns a fixed instant
  (pick the newest `updated` in the data so labels read "now") — find every call site
  (`sidebar.rs` grouping/elapsed, session tickers, `history.rs`, anything else) with grep;
- animations at rest: the library's motion helpers already offer `.at_rest()` on the login
  screen and `EnterExit`/presence timing; under the flag pass zero durations / at-rest
  everywhere the harness constructs them (login card, dialogs, toasts, menus, tool cards
  that shimmer, the composer caret, spinners) — if a library component has no way to be
  drawn at rest, list it in the report as an E-package item instead of hacking around it;
- the "scripted run" stderr line and everything else stays.

Prove it: take the full reference set twice with the flag and compare byte-for-byte:
`for f in fixtures/msp/transcript-*.jsonl fixtures/msp/synthetic-*.jsonl` × `--theme dark|light`
at `--screenshot-delay 2500`, plus `--no-connect --login <choose|device|apikey|apikey-error|error>`
(dark). Report the count that differ (target 0) and name any that still differ with why.
Document the flag in `docs/02-app.md` next to `--screenshot`.

## 2. `--bench` mode (finding performance-18; also performance-1)

`harness --bench <capture.jsonl> [--bench-cadence-ms N (default 4)] [--bench-scroll top|mid|tail|sweep (default sweep)] [--bench-frames N (default 600)] [--bench-out <file.json>]`,
free (no child, no server): stream the capture's `<--` lines through the fold on a timer at
the cadence (so streaming cost is real), while driving the transcript `ListState`
programmatically (`sweep` = top→tail→top over the run), and measure:

- element time: the existing `render_transcript` construction timer;
- frame time: whole-frame interval from gpui frame callbacks (time between consecutive
  paints while frames are being requested), and dropped frames (> 16.7 ms);
- fold-apply time per event;
- peak RSS at the end (`libc::getrusage` or `/usr/bin/time`-equivalent via `mach`).

Print one line per metric with `n p50 p90 p99 max` and `frames fps dropped`, and with
`--bench-out` write one JSON object with the same numbers plus the command, capture, build
profile, and git short hash. Also add an **idle assertion**: after the stream ends and
scrolling stops, count frames over the next 2 s; print it, since a settled transcript must
request none (finding performance-13 will be fixed later; report the number you see now).
Replace the old `bench:<n>` step's role with this (keep the step working). Read
`HARNESS_FRAME_STATS` once (`OnceLock`, A-MECH-14) and make `--bench` imply it.

Run the baseline and put the exact commands and numbers in the report:
`--bench fixtures/msp/synthetic-stress-300.jsonl` and `--bench fixtures/msp/transcript-echo.jsonl`,
debug build, plus once with `--release` (`cargo build --release -p harness` first; note the
build time).

Document `--bench` in `docs/02-app.md` (flags table and a short "measuring" section) and
mention both in `docs/05-handoff.md`'s gate list.

## Gates

`cargo build --workspace`; `cargo test --workspace`;
`cargo clippy --workspace --all-targets -- -D warnings`;
`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`;
`UPDATE_SNAPSHOTS=1 cargo test -p muse-adapter` (no diff expected). Report files changed,
the determinism count, the bench tables, gate output verbatim. Never claim a gate you did
not run.
