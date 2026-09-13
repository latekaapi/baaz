# Scroll research — 2026-09-13 (R1)

Why the transcript feels stepped on a trackpad while the sidebar does not, what in gpui
can cause it, and how to prove which mechanism is live before changing any code.

Sources read in full:

- `gpui-pre-macos-0.3.3/src/events.rs` (NSScrollWheel → `ScrollWheelEvent`), `src/window.rs`
  (`handle_view_event`, the `CVDisplayLink` `step`), `src/display_link.rs`
- `gpui-pre-0.3.3/src/interactive.rs` (`ScrollDelta`, `pixel_delta`, `coalesce`),
  `src/elements/list.rs` (`ListState`, `ListOffset`, `fn scroll`, `scroll_by`, the paint-time
  wheel closure), `src/elements/div.rs` (`Interactivity::paint`, `paint_scroll_listener`,
  `ScrollHandleState`), `src/gestures.rs` (`OngoingScroll::filter`), `src/window.rs`
  (`InputRateTracker`, the frame-rate throttles)
- `gpui-kit-0.6.0`, `gpui-base-0.6.0/src/virtual_list.rs`, `gpui-component-0.6.0/src/scroll/`
- `harness/crates/harness/src/session/render.rs`, `session.rs`, `bench.rs`;
  `docs/diagnosis/owner-round-2026-09-13.md`, `docs/diagnosis/transcript-pass-2026-09-12.md`
- Zed issues/PRs and the 120 fps blog post (links in §6)

## 1. What is *not* the fault

- **The platform layer.** `events.rs:258-286`: `hasPreciseScrollingDeltas == YES` — always
  true for a trackpad — yields `ScrollDelta::Pixels(scrollingDeltaX, scrollingDeltaY)`
  verbatim. `handle_view_event` does no coalescing, dropping or rate limiting. macOS's own
  momentum curve reaches gpui one event per NSEvent in exact pixels. No quantisation here.
- **Lines→pixels.** A trackpad never produces `ScrollDelta::Lines`, so `list.rs`'s
  hardcoded `px(20.)` (§4, H-B) cannot be the trackpad fault.
- **Frame time.** Round 2 took the wheel phase from p50 4.7 → 1.2 ms, p90 5.4 → 1.9 ms,
  frames past a 120 Hz budget 14 → 1, by going block-granular. At 1–2 ms a frame the app is
  not missing vsync.
- **gpui-pre 0.3.4.** Its `src/` is byte-identical to 0.3.3 except `action.rs` and
  `platform/test/platform.rs`. No scroll changes; upgrading buys nothing.
- **The macOS 26 autofill hitch** (Zed PR #38179, issue #33182 — `NSAutoFillHeuristicController`
  hooking every scroll pass). Already fixed in this snapshot:
  `gpui-pre-macos-0.3.3/src/platform.rs:1276` sets `NSAutoFillHeuristicControllerEnabled=false`.

## 2. Two different scroll algorithms in one app

The app has two kinds of scrollable surface and they move by different maths. Everything
except the transcript is a `div`; the transcript is a `list()`.

**`div` + `overflow_y_scroll`** (sidebar, menus, palettes, terminal) — `div.rs:3272-3318`:

```rust
let mut delta = event.delta.pixel_delta(window.line_height());   // not a constant
if restrict_scroll_to_axis && event.delta.precise() { ongoing_scroll.filter(&mut delta, phase) }
scroll_offset.y += delta_y;                                       // INCREMENTAL, per event
```

Absolute pixel offset, moved by each event's own delta. Axis-locked by
`OngoingScroll::filter` (`gestures.rs:59-107`). No accumulator, no frame-fixed base.

**`list()`** (the transcript) — `list.rs:1597-1614`, registered in the list's `paint`:

```rust
let scroll_top = prepaint.layout.scroll_top;                  // captured ONCE per painted frame
let mut accumulated_scroll_delta = ScrollDelta::default();    // == Lines(0,0)
window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
    if phase == Bubble && hitbox_id.should_handle_scroll(window) {
        accumulated_scroll_delta = accumulated_scroll_delta.coalesce(event.delta);
        let pixel_delta = accumulated_scroll_delta.pixel_delta(px(20.));
        list_state.borrow_mut().scroll(&scroll_top, height, pixel_delta, ...)
    }
});
```

`fn scroll` (`list.rs:898-960`) sets `new_scroll_top = scroll_top(frame base) − accumulated.y`,
clamped to `[0, scroll_max]`, then re-derives `ListOffset { item_ix, offset_in_item }` from
the **current** item heights. Logical anchoring is the right design — it is what Zed's own
agent panel uses — but the pixel↔logical mapping it re-derives through is not stable.

## 2a. The answer, measured

**H-2 below is the cause.** The burst instrument added for this round
(`--bench-scroll wheel`, the `bench-burst` lines) dispatches several events
between two frames, the way a trackpad actually delivers them, with one
`scrollingDeltaY == 0.0` sample woven into the middle. On
`fixtures/msp/synthetic-stress-300.jsonl`, release:

| burst | dispatched | travelled, before | travelled, after |
|---|---|---|---|
| up-clean | 864 px | 864 px | 864 px |
| **up-zeroed** | 864 px | **432 px** | **864 px** |
| down-clean | 864 px | 781 px | 781 px |
| down-zeroed | 864 px | 864 px | 864 px |

A single zero sample in a six-event upward burst threw away **half the
travel**, and the same burst downward lost nothing. That is the predicted
asymmetry, to the pixel: three events accumulate −36 px, the zero sample
overrides the accumulator to 0, the last three accumulate −36, and the frame
applies −36 of the −72 asked for. The list then snaps back to the frame's base
offset for the events that were discarded — which is what "visibly stepped"
looks like, and why it is worse scrolling up.

H-1 is real but is *not* what the owner is seeing: the single-event phases
(a–d) report `jumps=0 stalls=0 clamped=0` before and after, and frame rate is
unchanged (256 → 256 fps). The 72 px hint is still worth improving; it is
filed as a P3 follow-up rather than fixed here.

The fix and where it lives are in §7.

## 3. H-1: a 72 px hint re-based into a real height under the gesture

`session.rs:180` sets `ROW_HEIGHT_HINT = 72.0`, and `render.rs:312`
`reset_with_uniform_height(count, px(ROW_HEIGHT_HINT))` gives every unmeasured row that
hint. Real block rows are nothing like 72 px uniformly: a one-line markdown paragraph is
~24 px, a tool card or a diff is several hundred.

`list.rs:245` distinguishes `ListItem::Unmeasured { size_hint }` from
`ListItem::Measured { size }`, and an item is measured only once it is visible. So every
frame of a gesture that pulls new rows into the viewport replaces a 72 px hint with a real
height, which changes the SumTree sum that `fn scroll` uses for **both**
`self.scroll_top(scroll_top)` and `scroll_max`. The delta is applied in pixels against a
ruler whose graduations are being redrawn as you scroll.

Consequence: content advances at a rate that changes discontinuously each time a row is
measured — a 3× mapping error where a 72 px hint becomes a 24 px paragraph, and a much
larger one where it becomes a 400 px tool card. That is precisely "visibly stepped, not
smooth", it needs only one wheel event per frame, and it gets worse the more heterogeneous
the transcript is — which matches the owner seeing it on a real session and not on the
uniform 300-turn synthetic capture.

This is the mechanism the independent web research also ranks first, and it is the one the
existing bench cannot separate from correct motion, because `drive_wheel` records only
`item_ix`-level jump/stall/clamp counts, never the pixel offset series.

## 4. The mechanisms, and which one it was

- **H-2 — the cause (§2a). `coalesce` discards a frame's accumulated motion on a zero or sign-flipped sample.**
  `interactive.rs:626-660`: `let y = if a.y.signum() == b.y.signum() { a.y + b.y } else { b.y }`
  — an override, not a sum. In Rust `0.0f32.signum() == 1.0`, and AppKit emits exactly-`0.0`
  `scrollingDeltaY` samples routinely (the `MayBegin`/`Began` pair, finger-down pauses,
  horizontal-only samples, the momentum tail). So when ≥2 events land between two paints:
  scrolling **down** (dy positive) the zero sample has the same signum and the accumulator
  survives; scrolling **up** (dy negative) the signums differ, `y` is overwritten with `0.0`,
  the whole frame's motion is discarded and the list snaps back to the frame's base offset.
  **Falsifiable prediction: stepping is worse scrolling up than down.** Needs ≥2 events per
  painted frame, so at today's 1–2 ms frames it fires only in bursts — rank it below H-1,
  It is invisible to a one-event-per-frame bench, which is why round 2
  measured the scroll clean while the owner still saw it stepping.
- **H-3, wheel events dropped for a frame after a reset.** `fn scroll` opens with
  `if self.reset { return; }` ("Drop scroll events after a reset, since we can't calculate
  the new logical scroll top without the item heights"). `rehint_rows` already documents and
  confines this to a page landing or a width change — so it is not a per-frame cost, but
  every history page still eats one frame of wheel events mid-gesture.
- **H-4, no axis lock on the list.** `div` runs `OngoingScroll::filter`; `list` does not. A
  diagonal trackpad gesture feeds the transcript raw `dy` with no dominant-axis lock.
- **H-5, splice during streaming.** `sync_virtual_list` splices `from..old` per changed turn.
  `splice` does not set `reset`, so events survive, but heights below the splice are dropped
  and re-measured, moving `scroll_max` under a scroll in flight. Only while a turn streams.
- **H-B, hardcoded `px(20.)` lines factor** (`list.rs:1604`). Reached only by a real mouse
  wheel. `div` uses `window.line_height()` for the same conversion, so a wheel steps the
  transcript and the sidebar by different amounts. An inconsistency to fix, not the
  trackpad fault.

## 5. Frame pacing: context, not cause, but worth knowing

gpui draws from a `CVDisplayLink`, one frame per vsync (`display_link.rs`, `window.rs:2999`).
Three behaviours in `window.rs:1700-1820` bear on smoothness:

- `InputRateTracker` (`:1242`): ≥60 inputs/sec sets `sustain_until = now + 1s`, and while
  `is_high_rate()` the window presents **even when not dirty** — Zed's fix for ProMotion
  downclocking during input (zed.dev/blog/120fps). Present in this snapshot.
- **~30 fps cap when the window is not focused** (`:1731`, unless `is_high_rate()`).
- **~60 fps cap under `ThermalState::Serious | Critical`** (`:1733`). A warm laptop halves
  the frame rate during scroll — an environmental cause of stepping that no bench will
  reproduce and that would make any fix look inconsistent from session to session. Worth
  checking the machine's thermal state when reproducing by hand.

Zed's own addendum to the 120 fps post notes `CVDisplayLink` "introduced frame time
oscillation between 8-16ms" and that this was never fully solved. Zed has **no** scroll
easing or smoothing anywhere (issue #4355, discussion #31518 — a `ScrollAnimationManager`
proposal, closed unmerged), only `scroll_sensitivity` multipliers. So "add inertia" is not
an option the ecosystem supports; the fix has to be a correct offset mapping.

`gpui-kit 0.6` / `gpui-component 0.6` add nothing here: `scrollable.rs` is a scrollbar
overlay only, the container is still gpui's `Stateful<Div>` + `ScrollHandle`, and
`gpui-base`'s `VirtualListScrollHandle` derefs to a plain absolute-pixel `ScrollHandle`.
No smoothing, no accumulation, nothing to borrow.

## 6. The instrument (added this round)

The existing instrument must be extended; its one-event-per-frame shape is exactly the case
that cannot reproduce H-2, and its `item_ix`-granular counters cannot see H-1 at all.

1. **Offset trace.** Sample `list_state.logical_scroll_top()` converted to absolute pixels
   after every dispatched event and after every frame; emit the series in `--bench-out`
   beside `frame.series_us`. Smooth motion is a monotone series with near-constant first
   differences. H-1 shows as first differences that change abruptly as rows are measured;
   H-2 shows as a sawtooth (advance, snap back to the frame base, advance).
2. **Measured-vs-hinted trace.** Count, per frame, how many rows changed from `Unmeasured`
   to `Measured`, and correlate with the first-difference discontinuities. This is what
   separates H-1 from everything else.
3. **A momentum phase** in `WHEEL_PHASES`: several events per frame, decaying magnitude,
   interleaved exact-`0.0` samples and one sign flip; run once up and once down. H-2
   predicts the up run loses distance and shows the sawtooth while the down run does not.
4. Baselines on `fixtures/msp/transcript-real.jsonl` (heterogeneous — where H-1 bites) and
   the 300-turn stress capture from `fixtures/msp/make-stress-300.py` (uniform — where it
   should not), release build.
5. **The real trackpad last:** `--replay` the stress capture, scroll by hand with the trace
   on, read the series, and note the machine's thermal state (§5).

## 7. The fix, and the fixes not taken

**Not taken, for H-1 — make the ruler stable.** The numbers in §2a say H-1 is
not what the owner is seeing, so this is a P3 follow-up, not this round's fix.
Two routes when it is picked up:

- *Measure-ahead*: widen the list's overdraw so rows are measured well before they reach the
  viewport, so the mapping is already settled where the gesture is working. Cheapest, but it
  only moves the problem to fast flicks, and it costs layout on rows nobody sees.
- *Better hints*: replace the single 72 px constant with a per-block-kind hint from the
  block's own content (a markdown paragraph's line count, a tool card's row count), so a
  hint is close enough that its replacement does not visibly re-base. This attacks the cause
  and needs no gpui change. The block kinds are already known at `turn_rows` time.

**Taken, for H-2 — drive the list incrementally, like a `div`.**
`crates/harness/src/session/render.rs`, `wheel_capture`. `gpui-pre` is a registry crate, so
`coalesce` cannot be patched, and the list's own handler cannot be preempted from its parent:
`Interactivity::paint` (`div.rs:2451-2540`) registers a container's scroll listener **before**
painting children, and the bubble phase runs in reverse registration order, so the child list
always runs first. A parent `on_scroll_wheel` + `stop_propagation` is useless here.

What works is a **capture-phase** listener — capture runs in registration order, so the
outermost registrant beats every bubble handler. Register it from a `canvas` painted as the
transcript wrapper's first child (the wrapper already runs one to report its width to
`note_list_width`), and drive the list with the incremental API gpui already exposes:

```rust
// capture phase, pointer inside the list's bounds, vertical component only
ListState::scroll_by(-dy)   // list.rs:565 — reads logical_scroll_top() fresh each call,
                            // clamps at 0, stops tail-following on an upward scroll
cx.stop_propagation();      // vertical component only
```

`scroll_by` is exactly the `div` semantics: previous position plus this delta, per event, no
accumulator and no frame-fixed base. Leaving the horizontal component to bubble keeps the one
inner scroller in the transcript working — a markdown table,
`aui/src/transcript/markdown.rs:1463`, `overflow_x_scroll`, horizontal only. A sweep of
`crates/aui/src/transcript/` confirms there is **no** inner *vertical* scroller in any block,
so stopping the vertical component is safe. If this lands, consider adding gpui's own axis
lock (`OngoingScroll::filter`, `pub` in `gestures.rs`) on the same path to match `div` (H-4),
and `window.line_height()` instead of `px(20.)` for the wheel case (H-B).

Note that H-1 and H-2 are independent: `scroll_by` also re-derives through the SumTree, so
the incremental handler alone does **not** fix a moving ruler. If measurement says H-1 is
primary, the hint work is the fix and the capture handler is a secondary improvement.

**Where the fix lives.** The transcript's `ListState` is constructed in the **harness**
(`session.rs:610`); `aui` contains no `ListState` anywhere. So this is a harness change, not
a library change, unless the capture-phase wheel helper is worth generalising into `aui`.

## References

- Zed, *Optimizing the Metal pipeline to maintain 120 FPS in GPUI* — https://zed.dev/blog/120fps
- PR #38179 / issue #33182, macOS 26 autofill scroll hitch (already in this snapshot) —
  https://github.com/zed-industries/zed/pull/38179
- PR #19894, `anchor_scroll` / `ScrollAnchor` — https://github.com/zed-industries/zed/pull/19894
- PR #64057 (open), "scroll only the innermost container under the pointer" — not in 0.3.3 —
  https://github.com/zed-industries/zed/pull/64057
- Issue #45857, "Hitching on scroll", macOS trackpad, open, no root cause —
  https://github.com/zed-industries/zed/issues/45857
- Issue #42847 (Low Power Mode), #38326 (lag after prolonged use), #34778 (stutter in editor
  and AI panel), #7558 ("Scrolling is not 120fps")
- Issue #4355 and discussion #31518 — Zed has no scroll easing, proposal closed unmerged
- `hasPreciseScrollingDeltas` —
  https://developer.apple.com/documentation/appkit/nsevent/hasprecisescrollingdeltas
