# Brief — H2: transcript scroll jank

Repository `/Users/latekaapi/Projects/harness`, **your checkout is a git worktree** (see the
launch command's `--workspace`); work there, on its branch. **Do NOT commit.** Do not touch
`/Users/latekaapi/Projects/agentic-ui` or `~/Projects/cockpit`. Prefix every shell command with
`export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`. Spend rule: never
`turn/start`, `--send`, `send:`/`steer:`, live tests. Everything here is `--replay`/`--bench`.
Read `docs/02-app.md` "Measuring", `crates/harness/src/bench.rs`,
`crates/harness/src/session/render.rs` (`sync_virtual_list`, `transcript_list`) and gpui's
list element at `~/.cargo/registry/src/index.crates.io-*/gpui-pre-0.3.3/src/elements/list.rs`
(`ListItem::summary`, `ListState::scroll`, `layout_items`, `reset_with_uniform_height`).

## Diagnosis (done; verify it with the new instrument before fixing)

The bench's `sweep`/`top`/`mid`/`tail` modes drive the list with `scroll_to(ListOffset)` and
never exercise wheel scrolling, so they measured frame time (6–7 ms p50, fine) and missed the
fault. Wheel scrolling goes through `ListState::scroll`, which converts a pixel delta into an
item offset using the sum tree's heights — and an `Unmeasured` item with no `size_hint`
counts as **0 px**. The harness creates the list with `ListState::new(0, Top, px(TAIL_SLACK
* 2.0))` (an overdraw of 96 px) and sizes it with `reset(count)` / `splice(..)`, which give no
hints. Consequences, both visible to a person:

- From the tail, everything above the leading 96 px is 0 px tall, so one upward wheel event
  larger than ~96 px clamps `new_scroll_top` at 0 and **teleports to turn 1**.
- Scrolling down, `scroll_max` is the measured height only, so each event can advance at most
  ~96 px past what is measured; a flick rubber-bands in per-frame steps while `layout_items`
  measures the trailing overdraw.

Separately, `bench-idle frames_2s` is 8 without `HARNESS_DETERMINISTIC=1` and 0 with it: a
settled transcript still asks for four frames a second from some motion primitive
(`docs/02-app.md` says it must be none). Find which (candidates: the newest row's
`stream_reveal` with `settled=false`, the user turn's action-row opacity, the composer caret)
and gate it; the deterministic flag only masks it.

## What to build

1. **Instrument first.** A `--bench-scroll wheel` mode in `bench.rs` that dispatches real
   `ScrollWheelEvent`s through `window.dispatch_event(PlatformInput::ScrollWheel(..), cx)` at
   the transcript's centre, after the stream has landed and the list is at the tail: (a) a
   flick up — 12 events of +120 px on consecutive frames; (b) 90 events of −40 px (down);
   (c) 300 events of +20 px (up, trackpad-slow); (d) the same down. Sample the list's
   `logical_scroll_top()` every frame and print one `bench-scroll` line: events, frames,
   `jumps` (frames where `item_ix` moved by more than 3), `stalls` (frames where an event was
   dispatched and the position did not change), and the `item_ix` after each phase. With the
   current code phase (a) must end at `item_ix=0`; put that "before" line in the report.
   Extend `docs/02-app.md` "Measuring" with the mode.
2. **Fix the cause.** Overdraw becomes a viewport's worth (`WINDOW_H`, named const with the
   reason: measured rows outside the viewport are not re-laid out per frame, so the cost is
   one measurement each); `sync_virtual_list` uses `reset_with_uniform_height(count,
   TURN_HEIGHT_HINT)` for the first fill (hint = a typical settled turn, ~120 px, named const)
   and keeps `splice` for appends. After this, phase (a) must land a viewport or so above the
   tail, `jumps` must be 0 in every phase, and `stalls` must be 0 in (b)–(d).
3. **Then the per-frame cost, only where numbers move.** `bench-frame` p50 is 6–7 ms in both
   debug and release with element construction at 7 µs, so the frame is gpui layout of the
   visible rows. Try, one at a time, keeping only what moves `bench-frame` p50/p90 on
   `synthetic-stress-300` and `transcript-real` beyond noise: skip the `relative().top().opacity()`
   reveal wrapper when a block is settled; anything else you can show with numbers. Do not
   change what the transcript looks like: the deterministic set must stay byte-identical.
4. **Idle frames.** `bench-idle frames_2s` must be 0 on `synthetic-stress-300`,
   `transcript-echo` and `transcript-real` **without** the deterministic flag, and 2 with
   `--bench-open-turn` (the 1 Hz row). Name the primitive that was asking and the gate you added.

## Regression proof

`scripts/captures.sh <dir>` after the change, `cmp` against
`/private/tmp/claude-501/-Users-latekaapi-Projects-harness/f7a85ce4-a78a-46db-b3ec-e131d3878ca7/scratchpad/set-before`:
all 53 byte-identical (nothing here is a visual change). Adapter snapshots unchanged.
Bench table before/after (debug; release too for the final numbers) for `sweep` and `wheel` on
`synthetic-stress-300` and `transcript-real`: `bench-element`, `bench-frame`, `bench-frames`
(dropped), `bench-idle`, `bench-scroll`.

## Gates (run all; report verbatim; never claim a gate you did not run)

`cargo build --workspace`; `cargo test --workspace`; `cargo clippy --workspace --all-targets
-- -D warnings`; `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`. Report per item,
the bench table, the capture comparison count, gate output.
