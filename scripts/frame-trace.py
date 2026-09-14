#!/usr/bin/env python3
"""Summarise a `frame-trace.log` from a `HARNESS_FRAME_TRACE=1` run.

Each row is `t_us,list_px,events_since_last_paint,gesture_active` (v1) or
that plus `centre_w,rehint,draw_us` (v2, one per paint; see `docs/02-app.md`
§6). Prints:

- paints/s over the gesture (first to last `gesture_active=1` row),
- the gap histogram in display ticks, where one tick is 1/refresh and the
  refresh comes from the median gap of the densest 200 ms window,
- events applied vs paints,
- whether the last applied event was the last delivered one (proxy: after
  the final row carrying events, the offset must settle and stay settled —
  a backlog still draining would keep moving it while `gesture_active` is 0).

Free: reads a file. Usage:

    python3 scripts/frame-trace.py "$HARNESS_STATE_DIR"/frame-trace.log
"""

import sys
from collections import Counter


def load(path):
    rows = []
    with open(path) as fh:
        for line in fh:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            parts = line.split(",")
            if len(parts) not in (4, 7):
                continue
            try:
                if len(parts) == 7:
                    rows.append(
                        (
                            int(parts[0]),
                            float(parts[1]),
                            int(parts[2]),
                            int(parts[3]),
                            float(parts[4]),
                            int(parts[5]),
                            int(parts[6]),
                        )
                    )
                else:
                    rows.append((int(parts[0]), float(parts[1]), int(parts[2]), int(parts[3])))
            except ValueError:
                continue
    return rows


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


def main():
    if len(sys.argv) != 2:
        print(f"usage: {sys.argv[0]} <frame-trace.log>", file=sys.stderr)
        return 2
    rows = load(sys.argv[1])
    if not rows:
        print("no trace rows")
        return 1
    times = [r[0] for r in rows]
    tick = densest_tick_us(times)
    gesture = [r for r in rows if r[3] == 1]
    if gesture:
        span_s = (gesture[-1][0] - gesture[0][0]) / 1e6
        rate = len(gesture) / span_s if span_s > 0 else float(len(gesture))
    else:
        span_s, rate = 0.0, 0.0
    print(f"paints={len(rows)} gesture_paints={len(gesture)} gesture_span_s={span_s:.2f} paints_per_s={rate:.1f}")
    if tick:
        print(f"tick_us={tick} (~{1e6 / tick:.0f} Hz from the densest 200 ms)")
        hist = Counter(round((b - a) / tick) for a, b in zip(times, times[1:]) if b > a)
        total = sum(hist.values())
        axis = sorted(hist)
        print("gap_ticks " + " ".join(f"{k}:{hist[k]}" for k in axis))
        over = sum(n for k, n in hist.items() if k > 2)
        print(f"gaps_over_2_ticks={over}/{total}")
    else:
        print("tick_us=unknown (too few paints)")
    events = sum(r[2] for r in rows)
    with_events = sum(1 for r in rows if r[2] > 0)
    print(f"events_applied={events} paints_with_events={with_events} paints={len(rows)}")
    if rows and len(rows[0]) == 7:
        widths = [r[4] for r in rows]
        width_ticks = sum(1 for a, b in zip(widths, widths[1:]) if abs(b - a) > 0.5)
        rehints = sum(r[5] for r in rows)
        draws = sorted(r[6] for r in rows if r[6] > 0)
        if draws:
            p50 = draws[len(draws) // 2]
            p90 = draws[min(len(draws) - 1, int(len(draws) * 0.9))]
            print(f"width_changing_ticks={width_ticks}/{len(rows) - 1} rehint_paints={rehints} draw_us_p50={p50} draw_us_p90={p90} draw_us_max={draws[-1]}")
        else:
            print(f"width_changing_ticks={width_ticks}/{len(rows) - 1} rehint_paints={rehints} draw_us=none")
    last_event_ix = max((i for i, r in enumerate(rows) if r[2] > 0), default=None)
    if last_event_ix is None:
        print("tail=no wheel events in this log")
    else:
        tail = rows[last_event_ix + 1 :]
        quiet = len(tail)
        drift = max((abs(r[1] - rows[last_event_ix][1]) for r in tail), default=0.0)
        tail_gesture = sum(r[3] for r in tail)
        settled = drift < 0.5 and tail_gesture == 0
        print(f"last_event_row={last_event_ix} trailing_quiet_paints={quiet} drift_px={drift:.1f} settled={settled}")
        print("last_applied_is_last_delivered=" + ("yes" if settled else "UNCERTAIN (offset still moves, or the gesture flag never decayed — see drift_px)"))
    return 0


if __name__ == "__main__":
    sys.exit(main())
