# Diagnosis — the five owner-visible faults, 2026-09-12

Written by the design/review session (Fable) before any change; the fixes ride the briefs
named per section. Evidence is the deterministic capture set (`scripts/captures.sh`), the
gallery's own card screenshots, the design references under
`agentic-ui/design/reference/`, one real-window capture (`screencapture`, before the screen
locked), gpui's `list` source, and `--bench`.

## 1. Transcript design (`docs/briefs/muse-transcript-design.md`)

Compared `--replay` of `transcript-approve`, `transcript-real`, `synthetic-markdown`,
`synthetic-toolgroup` and the tail of `synthetic-stress-300` (both themes) against
`cards/31-turns`, `33-activity-group`, `34-tool-cards`, `35-approval`, `screens/harness-Main`
and the gallery's `transcript/{turns,tool-cards,tool-group,approval,markdown}` renders.

- Library cards render as the gallery does and the gallery matches the design: header 34 px,
  12.5 px header text, mono targets, right-aligned results, bubble radius 12/4, footer mono
  11 px. Colour comes from the same `tokens.css`; type is 13.5 px × the 1.1 product scale
  (decision 2026-09-05). Neither is at fault.
- **Blocks in one assistant turn touch** (`Ran ls` card against the `Answered` row; the tool
  group's three cards stacked with ~1 px between). The design puts 8 px between stacked cards
  and between prose and a card (`10-app-shell.html` `.grp2{gap:8px}`). Cause:
  `session/render.rs::transcript_list` builds a `v_flex` per turn with no gap.
- **Turn rhythm**: the only inter-turn space is the row's bottom padding; the design column is
  `.tr{gap:16px}`.
- **No measure**: the column is the pane's width (1112 px at 1440); every reference bounds the
  transcript and composer to ~760–800 px, centred.
- **Native traffic lights sit over the back button** (real-window capture: lights at
  x≈13–70 pt, ~3 pt above the header's centre; the back arrow at x≈20 pt under the yellow
  light). `app.rs` passes `.traffic_lights(false)` to shell and header, which reserves
  nothing, and `main.rs` never sets `traffic_light_position`.
- **Collapsing the sidebar collapses the header cell** (`sidebar_header(..).collapsed(..)`,
  `docs/images/improve-shell-collapsed-dark.png`): a 48 px empty cell, the lights over it.
- Kept as decided on 2026-09-10 (owner list, C6): the in-flow action row under each turn at
  idle opacity, although the design's toolbar is hover-only.

## 2. Scroll jank (`docs/briefs/muse-scroll-jank.md`)

`--bench` (debug, `sweep`): element 7/77/264 µs, fold-apply 12 µs, frame 6/7/10 ms p50/p90/p99,
166 fps, 8 dropped of 1530; release the same frame times. Frame time is not the fault. The
bench drives `scroll_to(ListOffset)` and never dispatched a wheel event, so it never saw what a
person sees. gpui's `ListItem::Unmeasured { size_hint: None }` sums to **0 px**; the harness
creates the list with overdraw `TAIL_SLACK * 2` = 96 px and fills it with `reset(count)`, no
height hint. From the tail, one upward wheel event above ~96 px clamps the pixel top at 0 and
lands on turn 1; downward, `scroll_max` is the measured height, so a flick advances one
overdraw per frame. `bench-idle frames_2s` is 8 without `HARNESS_DETERMINISTIC=1` and 0 with
it: an ungated motion primitive on a settled transcript. The real-window trackpad test could
not run (screen locked); the brief adds a wheel mode to the bench instead.

## 3. Sidebar open (`docs/briefs/muse-open-path.md`)

`resume` → `open` → new `SessionView` + `backfill` → `page_all` pages the whole view serially
on the background executor and returns everything at once; one UI update folds it all; the
next frame swaps. Sidebar `selected` and the centre title derive from `active`, so the click
gets no acknowledgement until then; nothing is cached across switches. Harness-workspace
sessions run to 13 MB of log. Numbers come from the `HARNESS_TRACE=1` instrument the brief adds.

## 4. Traffic lights and header collapse (`muse-lib-shell-header.md` + H1 item 3)

See §1. Library: `SidebarHeader::native_lights`, `traffic_light_position(cx)`,
`AppShell::header_follows_sidebar(false)`.

## 5. Close and reopen (H1 item 4)

`main.rs` `on_window_should_close` returns `true` (window removed) and no `on_reopen` is
registered; gpui only calls the reopen callback when no window is open, and nothing rebuilds
one; ⌘-Tab activates an app with no window. Fix: hide the app on close (`cx.hide()`, return
`false`; the session and the `muse serve` child survive) and an `on_reopen` that rebuilds the
window if none exists.

## Method

Before set: `scripts/captures.sh` (53 PNGs, byte-identical run to run) into the session
scratchpad; bench baselines above. Library first (`transcript-2026-09-12`, stacked on
`audit-2026-09-12` because the harness builds against that checkout), then H1 on `main` and
H2/H3 in sibling worktrees (`wt/scroll`, `wt/open`), merged and audited by the reviewer.
