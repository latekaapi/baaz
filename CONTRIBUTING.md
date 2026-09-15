# Contributing

## Setup

```sh
export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"
```

You'll need `agentic-ui` checked out **beside** this repository (the `aui`
crates are path dependencies — see `Cargo.toml`), the `muse` CLI on your
`PATH`, and Rust 1.85+.

## Building and testing

```sh
cargo build --workspace
cargo test --workspace                          # replay, parity and unit tests; spawns nothing live
cargo clippy --workspace --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
cargo tree -d | grep -E '^gpui-(pre|kit) '       # should print nothing — one copy of each
UPDATE_SNAPSHOTS=1 cargo test -p muse-adapter    # regenerate fold snapshots, then read the diff
```

All four of the first checks are the gate for a change to land. `cargo test
--workspace` never spawns a real `muse serve` process or spends a turn: it
folds every checked-in capture under `fixtures/msp/` and compares it to a
snapshot. Two tests that do spawn a real `muse serve` (`live_echo`,
`live_backfill_parity`) are `#[ignore]`d for exactly that reason — each one
spends a real, billed turn — and stay that way; don't run them without
knowing what login they'll bill.

## Read before touching cost

**There is no free provider.** `--provider echo` picks a route, not a bill —
on a signed-in machine it still reaches a real model, and the login's plan
decides what that costs. Free: `--replay <capture>`, `--no-connect`,
`--print-tier`, and most `--steps` verbs (below). Anything that reaches
`turn/start` spends a turn. Check which plan a login is on before any real
session:

```sh
cargo run -p harness -- --print-tier
```

See `docs/06-billing.md` for the full picture.

## Debug and scripting env vars

| var | effect |
|---|---|
| `HARNESS_STATE_DIR` | Overrides where the harness's own state lives (sessions metadata, search index, settings, tier cache — everything under `~/Library/Application Support/harness` normally). **Always set this to a fresh `$(mktemp -d)` for any scripted or automated run** — otherwise it reads and writes your real local state. |
| `HARNESS_MUSE` | The `muse` binary to spawn (defaults to `muse` on `PATH`). |
| `HARNESS_TRACE=1` | Verbose switch/state tracing to stderr. |
| `HARNESS_FRAME_STATS=1` | Records per-frame render timing (see `--bench` and `docs/02-app.md`). |
| `HARNESS_FRAME_TRACE=1` | Traces the normal window's paint cadence to `$HARNESS_STATE_DIR/frame-trace.log`, so a scripted gesture on `--replay` becomes a measurement (`scripts/frame-trace.py`). |
| `HARNESS_DETERMINISTIC=1` | Freezes anything that would otherwise vary run to run (relative timestamps, etc.), so a `--screenshot` capture is byte-identical across runs. Used throughout `scripts/captures.sh`. |

## The scripting surface: `--replay`, `--steps`, `--screenshot`

The same flags back the checked-in fixtures, the docs' screenshots, and any
new capture you add.

```sh
HARNESS_STATE_DIR="$(mktemp -d)" cargo run -p harness -- \
  --replay fixtures/msp/transcript-approve.jsonl \
  --theme dark \
  --steps 'choose:1' \
  --screenshot-delay 4000 \
  --screenshot /tmp/shot.png
```

- `--replay <capture.jsonl>` folds a checked-in wire capture with no server
  at all. Free, deterministic, the basis of most of this project's tests
  and screenshots.
- `--no-connect` draws the chrome (including the login screen, with
  `--login <state>`) without a server. Also free.
- `--steps 'a;b;c'` applies `;`-separated steps to the session as soon as it
  opens. Most step verbs are free and read-only from the wire's point of
  view — `wait:`, `choose:`, `answer:`, `fork`, `settings`, `search:`,
  `sidebar-wheel:`, and so on. **Two verbs are not**: `send:<text>` and
  `steer:<text>` submit a real prompt, which on a live connection reaches
  `turn/start` and spends a turn on whatever plan the signed-in login is on.
  Never put `send:` or `steer:` in a `--steps` list against a live
  connection without knowing — and intending — what it will bill; they are
  harmless against `--replay` (there is no live server to send to).
- `--screenshot <out.png>` / `--screenshot-delay <ms>` render a frame to a
  PNG only once every step — including every `wait:` in the list, not just
  the last one — has actually finished running, and then the delay on top
  of that: the delay never races the steps, and it is applied exactly once,
  after them, never before. `--theme dark|light` picks the theme.
- On a live connection, a `--screenshot` run also waits, bounded (up to two
  minutes, logged to stderr while it waits), for no turn to still be
  running in any session the window has open before it quits. Quitting
  mid-turn kills the `muse` child that turn is running on and orphans it —
  reopening the session later shows "Turn interrupted: the turn was
  orphaned when the session's process was lost." A `send:` as the last step
  with no `wait:` after it long enough for the reply is exactly what this
  guards against, but the wait it does is a backstop, not a budget: size
  your own `wait:` for the reply you are capturing rather than relying on
  it.

See `docs/03-composer.md` and `docs/04-approvals.md` for the full step-verb
tables, and `scripts/captures.sh` for the capture set this project's own
docs and tests are built from.
