#!/usr/bin/env python3
"""Summarise a `frame-trace.log` from a `HARNESS_FRAME_TRACE=1` run.

v3 format (owner round 6, part C3), one row per `Harness::render` — i.e.
once per display tick that renders anything at all, not only the ticks
that happen to rebuild the transcript column (the old v2 format's blind
spot: a sidebar-only or resize-only gesture went untraced once parts
4/C1/C2 stopped those from touching the cached centre):

    t_us,root,pane,centre,sidebar_ix,sidebar_off,sidebar_w,resize_active,
    list_px,events_since_last_tick,gesture_active,rehint,draw_us

`root` is always 1 (one row per root render, by construction). `pane` and
`centre` are how many times the sidebar pane and the cached transcript
column rebuilt since the previous row (usually 0 — that's the point of the
cache). `sidebar_ix`+`sidebar_off` is the sidebar list's `ListOffset`;
`sidebar_w` the divider's width; `resize_active` whether a drag is in
flight. `list_px` is the transcript list's pixel offset. `gesture_active`
is the OR of all three interactions' own gesture flags (transcript wheel,
sidebar wheel, resize drag) — one column for whichever of the three is
actually live on a given tick. `draw_us` is the *previous* frame's
render-to-paint micros.

For each of the three interactions this instrument exists to measure
(`--metric sidebar|transcript|resize`, or all three when omitted):

- `ticks_with_change / ticks_during_gesture` — the cadence target (owner
  round 6 brief, part C3): >= 95% for a pass.
- the longest gap between changes, in *display ticks* (one tick = the
  median gap of the densest 200 ms window of rows, i.e. the actual paint
  rate this run achieved) — the target is no gap > 1 tick.
- whether the momentum tail (the gesture-active rows after the last
  `events_since_last_tick > 0` row) kept changing every tick, or went flat
  early ("cut").
- p50/p90 frame ms (`draw_us` column) over the gesture-active rows.
- `longest_gap_ms`: the same longest silent stretch in raw wall-clock
  milliseconds, needing no tick calibration — the more trustworthy number
  on a machine with no real display link (owner round 6, part C3: this
  machine's `screencapture` returned solid black and
  `NSRunningApplication.activate()` returned `false`, both signs there is
  no attached compositor pacing `request_animation_frame` to a genuine
  60/120 Hz — `densest_tick_us` below can then read a rate faster than any
  real display, undercounting `longest_gap_ticks` or over-counting how many
  ticks "should" have changed. Two consecutive rows carrying the *same*
  value are not necessarily a dropped tick in that case — they may be two
  renders the environment's own timer produced within one real frame's
  worth of wall-clock time.  Cross-check `ticks_with_change` against
  `longest_gap_ms`: a low percentage with a small `longest_gap_ms` (under
  ~20 ms, one real frame plus slack) means the *value itself* tracked
  smoothly and the row-count metric is the artifact, not the app.

Free: reads a file.

    python3 scripts/frame-trace.py "$HARNESS_STATE_DIR"/frame-trace.log [--metric sidebar|transcript|resize]
"""

import argparse
import sys
from collections import Counter

V3_COLS = 13


def load(path):
    rows = []
    legacy = 0
    with open(path) as fh:
        for line in fh:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            parts = line.split(",")
            if len(parts) != V3_COLS:
                # v1 (4 cols) / v2 (7 cols) rows from an older binary's log:
                # skip rather than crash, but count them so the caller knows
                # why the row count looks low.
                legacy += 1
                continue
            try:
                rows.append(
                    {
                        "t_us": int(parts[0]),
                        "root": int(parts[1]),
                        "pane": int(parts[2]),
                        "centre": int(parts[3]),
                        "sidebar_ix": int(parts[4]),
                        "sidebar_off": float(parts[5]),
                        "sidebar_w": float(parts[6]),
                        "resize_active": int(parts[7]),
                        "list_px": float(parts[8]),
                        "events": int(parts[9]),
                        "gesture": int(parts[10]),
                        "rehint": int(parts[11]),
                        "draw_us": int(parts[12]),
                    }
                )
            except ValueError:
                legacy += 1
                continue
    return rows, legacy


def densest_tick_us(times):
    """One tick in micros: the median gap inside the densest 200 ms window."""
    if len(times) < 3:
        return None
    best, best_n = None, 0
    lo = 0
    for hi in range(len(times)):
        while times[hi] - times[lo] > 200_000:
            lo += 1
        if hi - lo + 1 > best_n:
            best_n = hi - lo + 1
            best = (lo, hi)
    lo, hi = best
    gaps = [b - a for a, b in zip(times[lo:hi], times[lo + 1 : hi + 1]) if b > a]
    if not gaps:
        return None
    gaps.sort()
    return gaps[len(gaps) // 2]


def sidebar_key(row):
    return (row["sidebar_ix"], round(row["sidebar_off"], 2))


METRICS = {
    "sidebar": ("sidebar scroll", sidebar_key),
    "transcript": ("transcript scroll", lambda r: round(r["list_px"], 2)),
    "resize": ("divider drag", lambda r: round(r["sidebar_w"], 2)),
}


def percentiles(values):
    if not values:
        return None, None
    s = sorted(values)
    p50 = s[len(s) // 2]
    p90 = s[min(len(s) - 1, int(len(s) * 0.9))]
    return p50, p90


def summarise_metric(name, label, key_fn, rows, tick_us):
    gesture_rows = [r for r in rows if r["gesture"]]
    if not gesture_rows:
        print(f"[{label}] no gesture-active ticks in this log")
        return
    # A tick at the list's own head/tail boundary is correct end-of-list
    # behaviour, not a dropped tick (the same "clamped" carve-out
    # `bench-scroll` makes) — a sweep or a real gesture that outruns the
    # list's scrollable range must not read as cadence jank. Approximated
    # from the data itself: the min/max key actually observed during the
    # gesture is the boundary, since a sweep's travel is driven independent
    # of the list's length.
    keys = [key_fn(r) for r in gesture_rows]
    lo, hi = min(keys), max(keys)
    changed = 0
    clamped = 0
    prev = None
    for k in keys:
        if prev is not None:
            if k != prev:
                changed += 1
            elif k in (lo, hi):
                clamped += 1
        prev = k
    total = len(gesture_rows) - 1 if len(gesture_rows) > 1 else 0
    scored = total - clamped
    pct = (100.0 * changed / scored) if scored else 100.0
    # Longest gap between changes, both in ticks (needs a reliable
    # `tick_us` — see the module docstring's note on why that can
    # over-count in an environment with no real display link) and in raw
    # wall-clock milliseconds, which needs no tick calibration at all and
    # is the more robust number where the two disagree. Both exclude a run
    # pinned at the boundary (the same "clamped" carve-out `bench-scroll`
    # makes) — a sweep or a real gesture that outruns the list's scrollable
    # range must not read as cadence jank.
    longest_gap_ticks = 0
    longest_gap_ms = 0.0
    last_t = gesture_rows[0]["t_us"]
    last_k = keys[0]
    for r, k in zip(gesture_rows[1:], keys[1:]):
        if k != last_k:
            if last_k not in (lo, hi):
                gap_us = r["t_us"] - last_t
                longest_gap_ms = max(longest_gap_ms, gap_us / 1000.0)
                if tick_us:
                    longest_gap_ticks = max(longest_gap_ticks, round(gap_us / tick_us))
            last_t = r["t_us"]
            last_k = k
    # The tail from the last change to the end of the gesture counts too (a
    # metric that stops moving before the gesture itself ends) — unless it
    # settled at the boundary, which is the expected end of a burst that
    # outran the list.
    if last_k not in (lo, hi):
        end_gap_us = gesture_rows[-1]["t_us"] - last_t
        longest_gap_ms = max(longest_gap_ms, end_gap_us / 1000.0)
        if tick_us:
            longest_gap_ticks = max(longest_gap_ticks, round(end_gap_us / tick_us))
    draws = [r["draw_us"] / 1000.0 for r in gesture_rows if r["draw_us"] > 0]
    p50, p90 = percentiles(draws)
    print(
        f"[{label}] ticks_with_change/ticks_during_gesture={changed}/{scored} ({pct:.1f}%) "
        f"clamped_ticks={clamped} longest_gap_ticks={longest_gap_ticks} longest_gap_ms={longest_gap_ms:.1f} "
        f"frame_ms_p50={p50 if p50 is None else round(p50, 2)} frame_ms_p90={p90 if p90 is None else round(p90, 2)} "
        f"gesture_ticks={len(gesture_rows)}"
    )
    if pct >= 95.0 and longest_gap_ticks <= 1:
        print(f"[{label}] PASS: change on >=95% of gesture ticks, no gap > 1 tick")
    else:
        print(f"[{label}] MISS (tick metric): target is >=95% and no gap > 1 tick")
    if longest_gap_ms <= 20.0:
        print(f"[{label}] wall-clock check: longest silent stretch {longest_gap_ms:.1f} ms — under one real ~60 Hz frame (16.7 ms) plus slack")
    else:
        print(f"[{label}] wall-clock check: longest silent stretch {longest_gap_ms:.1f} ms — a real, visible stall")
    # Momentum tail: rows after the last row carrying an applied event.
    last_event_ix = max((i for i, r in enumerate(rows) if r["events"] > 0), default=None)
    if last_event_ix is not None:
        tail = [r for r in rows[last_event_ix + 1 :] if r["gesture"]]
        if tail:
            tail_changed = sum(1 for a, b in zip(tail, tail[1:]) if key_fn(a) != key_fn(b))
            cut = tail_changed == 0 and len(tail) > 2
            print(
                f"[{label}] momentum tail: {len(tail)} gesture ticks after the last applied event, "
                f"{tail_changed} of them changed — {'CUT (flat)' if cut else 'uncut'}"
            )


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("log")
    parser.add_argument("--metric", choices=sorted(METRICS), default=None)
    args = parser.parse_args()

    rows, legacy = load(args.log)
    if not rows:
        print(f"no v3 trace rows ({legacy} legacy/unparseable rows skipped)" if legacy else "no trace rows")
        return 1
    if legacy:
        print(f"note: skipped {legacy} non-v3 rows (older binary's log format)")

    times = [r["t_us"] for r in rows]
    tick_us = densest_tick_us(times)
    print(f"rows={len(rows)} span_s={(times[-1] - times[0]) / 1e6:.2f}")
    if tick_us:
        print(f"tick_us={tick_us} (~{1e6 / tick_us:.0f} Hz from the densest 200 ms)")
        hist = Counter(round((b - a) / tick_us) for a, b in zip(times, times[1:]) if b > a)
        axis = sorted(hist)
        print("row_gap_ticks " + " ".join(f"{k}:{hist[k]}" for k in axis))
    else:
        print("tick_us=unknown (too few rows)")

    pane_ticks = sum(1 for r in rows if r["pane"] > 0)
    centre_ticks = sum(1 for r in rows if r["centre"] > 0)
    print(f"pane_rebuild_ticks={pane_ticks}/{len(rows)} centre_rebuild_ticks={centre_ticks}/{len(rows)}")

    metrics = [args.metric] if args.metric else list(METRICS)
    for m in metrics:
        label, key_fn = METRICS[m]
        summarise_metric(m, label, key_fn, rows, tick_us)
    return 0


if __name__ == "__main__":
    sys.exit(main())
