# Round 5 diagnosis — sidebar scroll jank + sidebar alignment/colour

2026-09-14, harness `main` 3be7d5b, agentic-ui `main` 61b7695, gpui-pre 0.3.3.
Written **before any code edit** (branch `owner-round-5-2026-09-14` had just
been created; the instrument/numbers in §A1.6 were appended after the
read-only analysis, still before the fix — see the notes there).

Status key: **[code]** = proven by reading the cited source;
**[measured]** = observed from a run below. Nothing here is a guess.

---

## A1. Sidebar scroll jank

### A1.1 What a wheel event over the sidebar does [code]

The sidebar list is a plain div — `crates/harness/src/sidebar_view.rs:333-344`
(`overflow_y_scroll()` + `track_scroll(&self.sessions_scroll)`). Its wheel
path is gpui's `Div::paint_scroll_listener`,
`~/.cargo/registry/src/*/gpui-pre-0.3.3/src/elements/div.rs:3257-3319`:

- one handler per scrollable div, Bubble phase, gated on
  `hitbox.should_handle_scroll(window)` (= the div's hitbox is under the
  pointer and no occluder blocks scroll — `window.rs:811-816,873-876`);
- `let mut delta = event.delta.pixel_delta(line_height)` — **no `coalesce`**,
  so the `ScrollDelta::coalesce` sign-flip/zero bug from
  `docs/diagnosis/scroll-research-2026-09-13.md` §4 H-2 **cannot fire here**;
  each event's own delta is applied: `scroll_offset.y += delta_y`
  (`div.rs:3311-3313`), clamped later at paint (`div.rs:2390-2437`);
- on a changed offset it calls `cx.notify(current_view)` (`div.rs:3314-3316`)
  where `current_view` was captured at paint time (`div.rs:3270`). The scroll
  div paints inside `SidebarPane`'s subtree, so this notifies **the
  `SidebarPane` entity** — never `window.refresh()` (`window.rs:2179-2184`,
  which would dirty the whole window). The transcript (`SessionView`) and the
  `Harness` root are **not** rebuilt by a sidebar wheel. That half of the
  round-4 design holds.

Consequences: N wheel events between two paints = N offset writes **and N
pane notifies** = N full column rebuilds (see A1.2). There is no accumulator,
no one-per-frame drain, no gesture horizon, no axis lock beyond gpui's own
`OngoingScroll::filter` (which only runs when
`restrict_scroll_to_axis` is set — the sidebar div does not set it, so a
diagonal gesture feeds raw dy). The transcript has had all four since round 4
(`session.rs:1556-1586` `push_wheel`/`drain_pending_wheel`/`GESTURE_HORIZON`,
`session/render.rs:95-105,1550-1585` `wheel_capture`); the sidebar has none.

### A1.2 The `.cached` pane re-renders on every wheel event [code]

`.cached(size_full)` reuses the subtree "until the entity is notified (or
the cached bounds / text style change)" (`view.rs:226-236`). The scroll
listener notifies the pane entity on **every** event that moves the offset
(A1.1), so the cache is busted per event by construction: a 6-event burst
between two paints rebuilds the whole column 6 times and paints once. The
round-4 comment (`sidebar_view.rs:155-158`) says "wheel notifies leave the
pane clean" — that is true only of notifies addressed elsewhere (e.g. the
transcript's `SessionView` notify); the sidebar's **own** scroll listener
addresses the pane itself.

`SidebarKey`/`on_frame` (`sidebar_view.rs:101-165`, `app.rs:1614`) does **not**
re-arm during a scroll [code]: the key holds row/grouping pointers,
selection, rename, reveal, auth, tier and current project — no scroll offset,
no hover. `sync_sidebar_pane` is a no-op while those are steady, so it
contributes nothing per event and nothing per frame. It is innocent.

Render-count proof [measured — counters, see §A1.6]: before the fix a
6-event burst drew 2–3 frames (pane and root alike — the root recomposes
every draw, uncached); the per-event div notify is real but sync events
coalesce. `root` counts draws, not wheel-caused rebuilds, in every table
here.

### A1.3 The reveal cannot fight an in-flight gesture — except for one stale arm [code]

Armed only in `resume` (`app/lifecycle.rs:729`) and `activate` (`:906`), both
setting `reveal = Some(id)` + `reveal_stable = false`. Consumed only by
`settle_reveal` (`sidebar_view.rs:359-384`): `Landed` clears at once, `Inside`
clears on the second consecutive inside reading, `NotReady` keeps the flag
**and pokes the pane directly** (`sidebar_pane.update(cx, |_, cx|
cx.notify())`) so the prepaint re-runs. No path re-arms on `session/list`,
regroup, hover or frame [code — grep of `reveal = Some` hits only the two
arm sites].

Two stains:

1. `NotReady` poke loop: while layout is unsettled the pane is notified from
   every prepaint — bounded (ends when layout settles), but it is per-frame
   column rebuilds during activation, adjacent to a scroll.
2. **Stale arm**: `install_reveal`'s else branch (`sidebar_view.rs:450`)
   returns the view *without* an intent when neither the session's project
   nor `current_project` names a group — the flag then stays `Some` until
   the next activation. If that group later renders (project adopted,
   group opened), the stale reveal fires `set_offset` and moves a list the
   user may have scrolled meanwhile. Narrow but real; the fix clears a
   reveal once a user scroll lands, and never installs one while a gesture
   is active.

### A1.4 Hover re-renders every row crossing during a gesture [code]

Every sidebar row tracks hover through `track_interaction`
(`agentic-ui/crates/aui/src/util.rs:33-62`): `on_hover` runs
`hover.update(cx, |s, cx| { …; cx.notify(); })` on the row's `Entity<Interaction>`.
That entity was created by `window.use_keyed_state`, which subscribes
`cx.observe(&new_state, move |_, cx| cx.notify(current_view))` with
`current_view` captured at creation — the `SidebarPane` entity, since rows
first render inside its subtree
(`gpui-pre-0.3.3/src/window.rs:3937-3950`). So **each hover flip notifies the
pane and busts `.cached`**: the whole column rebuilds when the pointer
enters *and* leaves a row. During a scroll gesture with a still pointer,
rows stream under the pointer — roughly one leave+enter pair per row height
(~30 px) of travel — each pair also restarting two 120 ms tweens
(`tint_fade`/`tween`, `aui-motion/src/tween.rs:63-84` via
`gpui_kit::base::transition`, which keeps requesting frames while settling).
The compact session row additionally cross-fades its tray/time
(`session_row.rs:605-611` `acts_opacity`), and the project group row fades
its tray vs branch+count (`views.rs:1071-1084` `tray_opacity`/`under_tray`).
Per-frame cost during a gesture is therefore not one list append but a full
column rebuild **plus** tween frames, at exactly the cadence the owner calls
janky. Hover is not the trigger (A1.2 is), but it multiplies every event.

### A1.5 Frame cadence [measured — see §A1.6]

`--steps wheel:` is hard-wired to the window centre
(`app/lifecycle.rs:316-322`) and `bench.rs:270,281,404` dispatches at
`(0.5, 0.5)` — both hit the transcript, never the sidebar. The sidebar
instrument added for this round is `sidebar-wheel:<dy>[,n]` (§A1.6): N
synthetic events at a sidebar point (x = 100, y = 40 % height) inside one
step, i.e. between two frames — the burst shape a trackpad really delivers.
Cadence numbers (before fix) are in §A1.6.

### A1.6 Instruments and before-numbers [measured]

Added for diagnosis (kept: they are the sidebar analogue of the existing
wheel instruments):

- `sidebar-wheel:<dy>[,n]` window step (`app/lifecycle.rs::step_sidebar_wheel`):
  dispatches n `ScrollWheelEvent`s at the sidebar point synchronously (one
  step = between two frames), then logs
  `sbwheel dy events offset_before->offset_after pane_renders/root_renders`.
- `Harness::sidebar_px()` (`app.rs`): `f32::from(self.sessions_scroll.offset().y)`
  (negative downward), the sidebar analogue of `SessionView::bench_list_px`.
- Render counters: `SIDEBAR_PANE_RENDERS` in `SidebarPane::render`,
  `HARNESS_ROOT_RENDERS` at the top of `Harness::render`
  (`sidebar_view.rs`, `app.rs`) — relaxed-atomics, drained per `sbwheel`
  log like `take_wheel_scroll_bys` (`session.rs:1513-1516`).

Fixture with many sidebar rows: `fixtures/sidebar/stress.json` (3 adopted
projects × 12 sessions + 4 strays = 40 rows; the checked-in `projects.json`
is 3 projects × ~7 rows and never overflows the list viewport).
Recipe (fresh state dir every run, deterministic clock):

```sh
export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"
HARNESS_STATE_DIR=$(mktemp -d) HARNESS_DETERMINISTIC=1 \
  ./target/debug/harness --workspace fixtures/ws/acme-web \
  --replay fixtures/msp/synthetic-stress-hetero.jsonl \
  --sidebar-fixture fixtures/sidebar/stress.json \
  --theme dark \
  --steps "project:fixtures/ws/acme-web;project:fixtures/ws/acme-internal;project:fixtures/ws/notes;sidebar-wheel:-40,6" \
  --screenshot /tmp/r5-sb-burst.png
```

Before-fix numbers (debug build, same-machine, settled 2.5 s, §A1.6 runs;
`root` counts `Harness::render`, which recomposes **every draw** — the root
is uncached, round-4 "composition only" — so root renders == draws):

| burst | events | Δoffset (px) | pane renders | root renders (draws) |
|---|---|---|---|---|
| sidebar 6 × −40 | 6 | 0 → −240 exact, synchronously | 2–3 | 2–3 |
| sidebar 1 × −40 | 1 | −40 exact | ~2 | ~2 |
| sidebar 1 × 0 | 1 | unchanged | 0 | 2 |
| over-scroll −320 | 1 | −320, then −310.5 at paint | — | — |

`max_offset.y` = 310.5 in this fixture: the div applies per event unclamped
and gpui clamps at paint (`div.rs:2425-2429`) — the −320 → −310.5 drift is
that clamp, not a reveal. The `sb-take` trace (added mid-diagnosis, since
removed) confirmed the div handler runs once per dispatched event.
`HARNESS_FRAME_TRACE=1` paints/s is transcript-only (`list_px`); sidebar
cadence comes from the `sbwheel` log.

After-fix numbers (same protocol):

| burst | events | Δoffset | drains | pane renders | root renders (draws) |
|---|---|---|---|---|---|
| sidebar 6 × −40 | 6 | 0 → 0 at dispatch; −240 after 800 ms | **1** | ~20 | ~20 |
| sidebar 1 × −40 | 1 | −40 after the wait | 1 | 2–7 (run variance) | same |

Reading: travel is conserved exactly and 6 events collapse into one clamped
write. The ~20 draws are the 150 ms horizon at ~120 Hz plus ambient ticks —
the transcript's own tradeoff (round 4: the tail must paint to stay smooth),
and per-draw work is unchanged (same column rebuild, same root recompose
every transcript frame already pays). Before drew 2–3 irregular frames with
no tail; after draws a regular tail. The owner's hand is the verdict, as in
round 4.

Empirical footnotes:

- `SidebarKey` is innocent, measured: a temporary key-change trace printed
  6 boot transients and **zero** changes across every burst.
- `window.refresh()` is ruled out for the zero-delta case: a refresh busts
  the pane cache (`view.rs:388-398`, `!window.refreshing`), but pane renders
  were 0 there. The 2 root renders per event (even zero-delta) come from an
  event-level Harness dirtiness that was not isolated — it predates the fix
  and is orthogonal to it (no scroll handler runs for a zero event).
- Hover churn stays code-level evidence: synthetic positions do not move the
  OS pointer, so no hover flips fire in these runs. Suppression was
  considered and declined — under the horizon the frames already run, so
  suppression saves no frames, only tween restarts, at the price of library
  API plus stuck-hover risk. The crossed-row fade under a still pointer is
  noted for hand verification.

### A1.7 Ranked causes (one line each)

1. **Per-event pane notify**: the div scroll listener notifies `SidebarPane`
   per wheel event (`div.rs:3314-3316`), busting `.cached` → a full column
   rebuild per event instead of one offset write per frame.
2. **No gesture horizon**: nothing requests frames through the momentum tail,
   so a burst's tail paints only on unrelated dirtiness — uneven cadence.
3. **Hover churn**: each row crossing under a still pointer notifies the pane
   twice more (`util.rs:33-62` + `window.rs:3937-3950`) and restarts 120 ms
   tweens — a multiplier on (1).
4. **Stale reveal arm** (`sidebar_view.rs:450`): a reveal with no group waits
   forever and can `set_offset` a user-scrolled list later.
5. **`SidebarKey`/`on_frame`**: innocent — key carries no scroll/hover state
   and never re-arms mid-gesture.
6. **`coalesce`**: inapplicable — the div path never coalesces (A1.1).

---

## A2. Sidebar alignment and the bright first project

### A2.1 The target (brief, unchanged from round 4)

One vertical line at the sidebar's left: nav icons centred on it; each
project group label's **first letter starting exactly on it** (plain, muted,
no tile, no chevron by default); session status dots centred on it; session
titles and nav labels starting at one shared x. All project labels the same
muted colour; the accent bar (Settings flag, default off) may mark the
current project but never its label colour.

### A2.2 The constants [code]

Library `agentic-ui/crates/aui/src/nav/parts.rs:32-44`:
`NAV_GUTTER = 8` (column edge → leading box), `LEADING_BOX = 20`,
`NAV_LABEL_X = 32` (= 8 + 20 + 4), `NAV_LABEL_GAP = 4`. The doc comment
there pins the design: "the leading centre lands at 8 + 10 = 18 px from the
column edge and the labels at 8 + 20 + 4 = 32 px (the owner's Claude frame:
glyph centre ≈ 36 at 2×, labels ≈ 64 at 2×)". Project row:
`PJ_MARGIN_X = 8`, `PJ_LEAD_GAP = 4` (`views.rs:29-35`); session row:
`SR_MARGIN_X = 8`, `SR_LEAD_GAP = 4` (`session_row.rs:45-47,89`).
Neither the `SidebarView` column root (`views.rs:542-556`,
`v_flex().w_full()`, no padding) nor the harness `sessions-scroll` div
(`sidebar_view.rs:332-346`, no left padding) nor `render_nav_block`
(`sidebar_view.rs:167-172`, "No side inset of its own") adds any x — the
numbers below are from the column edge.

### A2.3 Measured-by-reading x positions, per row state [code]

| element | x from column edge | source |
|---|---|---|
| nav icon centre | 8 + 10 = **18** | `NavItem`: `pl(8)` + `w(20)` box, icon centred (`parts.rs:120-133`) |
| nav label | 8 + 20 + 4 = **32** | same row, `gap(4)` |
| plain group-label first glyph | 8 + 0 = **8** | `ProjectGroupRow`, no chevron/mark: `ml(8)`, `pl(0)`, no leading child (`views.rs:1003-1045`) |
| chevron-group chevron centre | 8 + 10 = **18** | `chevron` flag: `w(20)` box + `gap(4)` → label at **32** (`views.rs:1027-1034`) |
| session dot centre | 8 + 10 = **18** | sidebar rows are non-nested `CompactSessionRow`: `ml(8)` + `w(20)` box, dot centred (`session_row.rs:648-676`) |
| session title | 8 + 20 + 4 = **32** | same row, `gap(SR_LEAD_GAP)` when not nested |
| Sessions caption text | **32** | `GroupRow`: `pl(NAV_LABEL_X)` (`parts.rs:189-201`) |
| fold ("Show N more") text | **32** | "carries the session rows' margins and gutter" (`views.rs:493-496`) |

So today: icons and dots share x = 18, titles/labels share x = 32 — but the
plain project label sits at **8** (left of the icon line) and the
chevron-flag label at **32** (right of it). **Neither equals 18.** The target
wants the plain label's first letter at 18: `ml(8)` + `pl(10)`
(= `LEADING_BOX / 2`), a library change.

States that move nothing horizontally [code]: `current_bar` (absolute bar
at the row's left edge, `views.rs:1046-1058`), branch/trailing (trailing
flex children, `views.rs:1085-1108`), count (trailing tag), running
`state` dot (after the name), pinned (pin on the meta line,
`session_row.rs:631-634`), hover tray (absolute right, opacity-only,
`views.rs:849-891`), the reveal/current wrappers (plain `w_full` columns,
`views.rs:623-640`, `rows()` `:468-488`). The **only** state that pushes a
project label right is `group_chevron` (8 → 32).

### A2.4 The bright first project [code]

`ProjectGroupRow::render` (`views.rs:1003-1045`):
`.text_color(if self.current { p.ink } else { p.ink_3 })` and
`.font_weight(if self.current { SEMIBOLD } else { MEDIUM })`.
The harness sets it unconditionally for one project: `sidebar.rs:424-432`
(current = open session's project, else the store's) and `:500-502`
(`if current_project == Some(id) { group = group.current(true); }`) —
**not gated on any layout flag**. So in any project window one label is
always `ink`/semibold ("bright white") and the rest `ink_3`/medium. That is
the owner's report exactly. Fix per target: `current` alone changes neither
colour nor weight (stays `ink_3`/medium like every other label); only
`current_bar` draws the bar. Library change; the harness keeps passing
`current` (the bar intent and the reveal wrapper still need it).

### A2.5 What differs between the round-4 capture and the real app

Round-4 `sb1-grouped-dark.png` (flags off): labels at 8, LEFT of the nav
icons; current project (`notes`) bold/bright. Owner's real app: labels too
far RIGHT, first project (`harness`) bright. By §A2.3 the only label-right
state in the code is the chevron flag — the owner's window has the Settings
switches on (all three ship default-off in `layout.json`; the Settings
dialog owns them since round 4). With `group_chevron` on, every label sits
at 32 with the chevron at 18; with `group_bar` on, the bar additionally
marks the current project; `current` still brightens it in both cases.
No real-state-only element moves the label: branch/count/tray/pin are
trailing or absolute (§A2.3), the caption padding (fixup 3be7d5b) touches
only the caption row, and no `mark` is ever passed (harness passes none;
`views.rs:1035-1045` draws the box only for an explicit mark). A screenshot
of the owner's live state is impossible (its projects never leave the
machine), so the fix implements the target for every flag combination and
the captures below prove each one.

### A2.6 Capture matrix (to take after the fix; recipe §A1.6 + `open:`)

`project:fixtures/ws/acme-web;…;open:<id>` (one project current), both
themes, flags off vs all on, plus a scrolled sidebar with a group menu open
and the palette `wheel:` proof. For each: read nav-icon centre, group-label
first glyph, dot centre and title x for the current AND a non-current
project. Target numbers: 18 / 18 / 18 / 32 in design px (×2 at retina).

---

## As built

Scroll: `sidebar_wheel_capture` (capture canvas, first child of a new
`relative` wrapper around `sessions-scroll`) takes vertical wheel events
ahead of the div's bubble listener, accumulates into the shared
`SidebarWheelState` cell (`Harness::sidebar_wheel` — shared because a wheel
can arrive inside a `Harness` update, where an entity update panics; the
first version updated the entity and panicked under `sidebar-wheel:`, the
second deferred through `spawn` and broke coalescing), and
`render_sidebar` drains one clamped write per frame
(`sidebar::clamp_sidebar_offset`, unit-tested), notifying only the pane,
with the 150 ms horizon (`SIDEBAR_GESTURE_HORIZON`) requesting frames while
open. The div's own handler stays for yielded events. A wheel disarms the
reveal (`scrolled` applied in the drain); installs are gated on
`!sidebar_user_scrolled && !gesture_active`; a groupless reveal clears. The
`SidebarKey` verdict held: zero re-arms across every burst (key-change
trace, since removed). Hover suppression declined (§A1.6): under the horizon
the frames already run, so it saves no frames. `root` counts draws (the root
is uncached and recomposes every draw — round-4 "composition only"), `pane`
counts column rebuilds.

Alignment/colour (library `987318a`, committed first): `PJ_PLAIN_PAD`
(`NAV_GUTTER + LEADING_BOX / 2 − PJ_MARGIN_X` = 10) puts the plain label's
first glyph at 18; `current` renders `ink_3`/medium like every other label;
only `current_bar` draws the bar. Harness passes `current` unchanged (the
bar intent and reveal wrapper still need it). Gallery card 23's comment
updated (it already demos a current group).

Measured x (1x captures, threshold ±1 px; theory 18 / 32 / 18 / 32):

| capture | nav icon centre | nav label | plain label 1st glyph | chevron centre | chevron label | dot centre | title |
|---|---|---|---|---|---|---|---|
| rest dark, non-current | 17.5 | 33 | 18 | — | — | 14–21* | 33 |
| rest dark, current | — | — | 18–19 | — | — | 14–17 | 33 |
| rest light, both | 17.5 | 33 | 18 | — | — | — | 33 |
| flags dark, non-current | — | — | — | 17.5 | 32 | — | — |
| flags dark, current | — | — | — | 17.5 | 32 | — | — |
| flags light | — | — | — | 17.5 | 32 | — | — |

\* dot span 14–21 at the threshold (the glyph is small and dim); centre ≈ 18
by construction (`ml(8)` + centred 20 px box, unchanged code).

Label colour (dark, mean/p50/max glyph luminance): acme-internal
103.9/108/133, acme-web current 107.5/109/133 on text (154 peak beside the
text is the blue running-state dot), notes 102.8/106/134 — uniform muted;
session titles 181/199/255 (`ink`, unchanged hierarchy). Flags-on keeps the
same numbers with the bar at x8–9 on the current project only.

Bench (transcript, must not regress): hetero wheel debug shell draw
p50 1825 (round-4 after: 1857), frame 2/7/9, scroll 696 events / 793 frames
/ 0 jumps / 0 stalls / 0 clamps with sample-identical phase indices
(239 230 239 224 239), drains 744; bare p50 1619 (was 1559). `bench-idle`
reads 19 in shell AND bare on this machine (round 4: 0) — shared cause
outside the diff (bare renders no sidebar code), reported as drift.

Captures (`/tmp/round5/`, all read): `sb-rest-dark/light.png`,
`sb-flags-dark/light.png` (flags on), `sb-scrolled-menu-dark.png`
(offset −240 held 15 s, acme-internal menu open and seated),
`sb-palette-dark.png` + log proof `wheel dy=-40 list_px=-0->-0`.
Screenshots are 1x (1440×869), not 2x — `shot.rs` captures at one device
pixel per point on this machine; design px map 1:1.

Not established: the event-level Harness dirtiness behind the pre-fix 2
root renders per zero-delta event (no scroll handler runs there); the exact
per-tick split of horizon frames past the drain (ambient vs requested).
Neither blocks the fix — the drain, travel and install-gating numbers are
exact.
