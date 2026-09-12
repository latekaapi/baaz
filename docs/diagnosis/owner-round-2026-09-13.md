# Owner round 2026-09-13 — fifteen faults: diagnosis and tracker

The owner sent three written faults and nine annotated screenshots after the 2026-09-12
pass. Every item was diagnosed by reading the code and, where it mattered, by reproducing
it on free wire calls (`session/list`, `session/resume`, `view/page`, `--replay`,
`--bench`). No model turn was spent. Owner of each fix: **Fable** (this session) or
**Muse** (`muse exec` from a brief in `docs/briefs/`).

| # | Fault (owner's words) | Diagnosis | Fix | Owner | Status |
|---|---|---|---|---|---|
| 1 | Pin/unpin "causes a spinning loader — it's that slow" | Not the store and not the search index (both are small and the index rebuild is already off the UI thread). The library row's hover tray buttons do not stop propagation, and gpui fires every `on_click` up the tree in the bubble phase, so a click on the pin icon also fires the row's `on_select` → `resume(id)` → a full re-open of the session: the "Loading history…" spinner and a `view/page` round trip. Same for Rename and Archive. | Library `session_row::action_tray`: `cx.stop_propagation()` in the tray buttons' click handlers. | Fable | done |
| 2 | Chat transcript scroll is janky on the trackpad | Measured with `--bench --bench-scroll wheel` on a real 808-event capture (release): frame p50 4 ms, p90 8 ms, max 23 ms, while the harness's own element construction is 3–24 µs. One list item is one **turn**, and real turns are enormous (hundreds of blocks), so gpui lays the whole visible turn out every frame; at 120 Hz the 8.3 ms budget is missed on a large share of frames. Two secondary faults: history pages after the first are appended to the list with **no height hint** (the 2026-09-12 hint only covered the first fill), so the head-jump returns on multi-page sessions; and gpui drops every hint when the list width changes. The trackpad itself is fine: macOS supplies the momentum, gpui passes the precise deltas through, the platform layer is Zed's. | Block-granular virtual list (one item per block), hints kept across page appends and width changes, and the bench's capture reader unpacks `view/page` results so real sessions can be benched. | Fable | done |
| 3 | New session + first message not shown in the sidebar | `session/list` only lists a session once its log flushes on `turn/completed` (wire fact, `docs/09` §5), and the harness only re-reads the list on `turn/completed`; nothing places a row locally in between. | Insert a local row at `session/start` (label "New session"), title it from the first prompt on send, keep it through `load_sessions` until the wire lists it. | Muse | done |
| 4 | Error popup has two Dismiss buttons | `render_dialog` always adds a secondary "Dismiss"; for `DialogAction::Dismiss` the primary is also "Dismiss". | No secondary when the primary already dismisses. | Fable | done |
| 5 | "muse error -32011: unknown cursor anchor" popup | A cached view is topped up with `session/resume { cursor }`; when the server no longer knows that cursor it answers `-32011 notFound`, which `report` turns into a dialog. | On `notFound` in `top_up`, drop the cached view and reopen fresh, silently. | Fable | done |
| 6 | Link toast "No such file: /Users/…/harness/assets" | `reveal_workspace_path` strips the leading `/` and joins the absolute path onto the workspace again (`…/harness/Users/latekaapi/…`). | Accept absolute paths inside the workspace; folders open in Finder, files open in their default app (`open_with_system`). | Muse | done |
| 7 | Sessions view menu opens far from the toggle | Hard-coded `.top(140).left(12)`. | Anchor under the sliders button, right-aligned to the sidebar. | Muse | done |
| 8 | Rename breaks the row layout, too much padding | The rename field wrapper has no height bound; the editor's own line box plus the row's text lines add up. | Fixed-height field matching the row title line; no vertical padding. | Muse | done |
| 9 | Hovered toasts stack on top of each other | The library fans toasts by an **estimated** height (one body line); a wrapped body is taller, so the next toast lands on it. | Fan by measured heights (`on_children_prepainted`), one gap between cards. | Muse | done |
| 10 | Link click on a folder errors; should open Finder / default app | Same cause as 6, plus `reveal_path` selects rather than opens. | Same fix as 6. | Muse | done |
| 11 | Empty state not aligned | The suggestion-chip row is `w_full` left-aligned under a centred title. | Centre the chips in the measure. | Muse | done |
| 12 | Still "Working…" after the reply | Truthful to the wire: after the `agentMessage`, Muse runs `reminderChild` items (memory reminders) for 30–70 s before `turn/completed`. The row just does not say so. | "Finishing up…" once the running turn's reply is complete. | Muse | done |
| 13 | Chat message design broken (user bubble) | Reproduced on a free capture of the brief session: the bubble's column is `items_end` with a percentage `max_w`, so the bubble sizes to its longest unwrapped line and overflows to the left. | Bubble bounded to the column's width so the markdown wraps. | Muse | done |
| 14 | Cannot scroll inside the slash-command popup | The library menu stops the wheel unconditionally; the harness's scroll wrapper is its parent and never sees the event. | The menu is its own scroll container and stops propagation on the same element. | Muse | done |
| 15 | "Is the transcript the same as the design system?" | Yes for the cards; the only composition faults found are 13 and the block gaps landed on 2026-09-12. | Covered by 13. | — | done |

## Outcome

- Scroll, measured (release, wheel phase of the real 808-event capture, frame intervals):
  p50 4.7 → 1.2 ms, p90 5.4 → 1.9 ms, p99 8.4 → 6.8 ms, max 30.6 → 8.4 ms; frames past a
  120 Hz budget 14 → 1. The 300-turn synthetic capture (small turns) is unchanged within
  noise. The 53 deterministic captures are visually identical; 18 differ by sub-pixel
  anti-aliasing on an icon row.
- Rename (item 8): Muse's 22 px wrapper kept the row's height but the library's 20 px
  editor recipe clipped the glyphs' descenders; the editor keeps its natural line box
  inside the fixed wrapper now (Fable, after the audit).
- Library commit `d64cf8d` on agentic-ui `owner-round-2026-09-13`, merged to `main`;
  harness merge `4199b11` plus the rename follow-up.
- Spend: zero model turns from the harness. The two `muse exec` runs were the implementors'
  own sessions on the subscription (1 and 22 accepted intents). The "New session" evidence
  screenshot came from a free `session/start`.

## Working notes

- Pin, dialog and cursor fixes were small enough that Fable did them directly.
- The reproduction capture for 13 was taken with `MUSE_CAPTURE` on `session/resume` +
  `view/page` (free) and replayed with `--replay`; it is not checked in (it holds a whole
  session's tool output).
- The scroll fault was measured, not felt: the bench dispatches real `ScrollWheelEvent`s but
  not a trackpad's momentum curve. The owner's on-screen check is still the last word.
