# Brief — owner round 2026-09-13, library package (agentic-ui)

Repository `/Users/latekaapi/Projects/agentic-ui`, branch `owner-round-2026-09-13` (already
checked out, off `main`). Do NOT commit. Do not touch `/Users/latekaapi/Projects/harness`,
`/Users/latekaapi/Projects/harness-wt-owner` or `~/Projects/cockpit`. Prefix every shell
command with `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`.
Spend rule: never `turn/start`, `--send`, `send:`/`steer:`, live tests, `muse logout`,
`account/logout`. Read `docs/00-agent-brief.md` and `docs/04-design-rules.md` first. Library
rules: no literal colours/sizes/durations outside the file's own named constants, stateless
`RenderOnce` components with intents out, `popover_layer` for overflow, both themes, a
gallery entry for anything new (nothing here is new; the gallery pages that show these
components must still render).

The diagnosis for every item is in
`/Users/latekaapi/Projects/harness/docs/diagnosis/owner-round-2026-09-13.md` (read-only for
you). Three fixes, each isolated to one file.

## L1 — Toast stack fans by measured heights (`crates/aui/src/feedback/toast.rs`), item 9

Fault: `ToastData::height()` estimates one body line; a wrapped body (the harness shows
"No such file: /Users/latekaapi/Projects/harness/assets", two lines at the stack's width) is
taller than the estimate, so on hover the next toast's fanned `top` lands on the previous
toast. The owner's screenshot shows three "Link" toasts overlapping.

Fix: measure. Keep the estimate only as the first-frame fallback. Store per-toast measured
heights in element state on the stack (`window.use_keyed_state((id, "heights"), cx, |_, _|
Rc<RefCell<HashMap<ElementId or toast id, f32>>>)` or the `with_element_state` pattern the
file already uses through `interaction_flags`), record each card's height from a
`div().on_children_prepainted(move |bounds, _, _| …)` wrapper around the card (bounds are
in window pixels; `bounds[0].size.height` is the card), and build `fanned[]` from the
measured height when one exists, else `height(text_scale)`. Because the fan runs on a
spring over several frames, a one-frame estimate error self-corrects. Keep `FAN_GAP` as the
single gap between fanned cards. Do not change the tucked (unhovered) geometry.

Prove it: a unit test in the file's `tests` module is not possible for a measurement, so
add a gallery check instead — the existing toast gallery page must show a stack where one
toast body wraps to two lines; hover fans without overlap. Take one screenshot of the fanned
state through the gallery's own screenshot route and name it in the report.

## L2 — User bubble bounded to the column (`crates/aui/src/transcript/turns.rs`), item 13

Fault: `UserTurn::render` builds `v_flex().max_w(relative(USER_MAX)).items_end()` and puts
the bubble `div()` inside with no width bound. In a column with `items_end` the bubble is
content-sized; a long markdown prompt sizes it to its longest unwrapped line, the percentage
`max_w` on the column does not reach the bubble, and the harness's `justify_end` row pushes
the overflow off the left edge. The owner's screenshot shows text cut off at the left and
large empty vertical gaps. Reproduced with a real prompt of ~120 lines of markdown
(headings, lists, code spans, a fenced block).

Fix: the bubble must never exceed the column: give the bubble `.max_w_full()` and
`.min_w(px(0.))` (or `.w_full()` when the column is the measure), and make the column's
width definite so percentages resolve — `w_full()` on the column plus `max_w(relative(
USER_MAX))`, keeping `items_end` so a short bubble still hugs the right. Long words and code
spans inside must wrap or truncate within the bubble; fenced code inside a user bubble
scrolls horizontally inside its own frame (the markdown renderer's existing rule), never
widens the bubble. Actions (`actions_bottom(true)` row and the hover tray) keep their
current placement relative to the bubble's right edge.

Prove it: add a gallery example to the user-turn page with such a long prompt (take the
text from `/Users/latekaapi/Projects/harness/docs/briefs/muse-transcript-design.md`, the
first ~80 lines) and screenshot it in both themes; a short prompt must still produce a
short bubble hugging the right (screenshot that too). Both must fit inside the column.

## L3 — Command menu scrolls (`crates/aui/src/composer/menus.rs`), item 14

Fault: the popover container (the `.pop` div, ~line 385) has
`.on_scroll_wheel(|_, _, cx| cx.stop_propagation())` and no overflow of its own. gpui runs
bubble-phase listeners newest-registered first, so this child listener runs before any
ancestor's scroll handler — including the harness's wrapper (`overflow_y_scroll` +
`max_h`) that was meant to scroll the menu. Result: the `/` menu cannot scroll; long menus
(18 commands + skills) are cut off.

Fix: make the menu its own scroll container and keep the transcript still. On the same
`.pop` element: `.overflow_y_scroll()` with a `max_h` — a named constant `MENU_MAX_H`
(560 px, the harness's `POPOVER_MAX_H`, now owned here) — and keep the
`on_scroll_wheel(... stop_propagation)` on that same element: an element's own scroll
handler is registered after its user listeners, so it runs first in the bubble phase and
the menu scrolls before the event is stopped. The mention picker (`mention_picker`) gets
the same treatment if it shares the container. Keyboard: when the selected row moves with
the arrow keys past the visible band, the container must scroll it into view — use a
`ScrollHandle` kept in element state (`window.use_keyed_state`) with `track_scroll` and
`scroll_to_item`, or the row ids and `scroll_handle.scroll_to_item(index)`; whichever the
gpui-pre 0.3.3 API offers (check `~/.cargo/registry/src/*/gpui-pre-0.3.3/src/elements/div.rs`
for `ScrollHandle`).

Prove it: gallery page for the command menu with 30 rows: wheel over it scrolls it and not
the page behind; arrow-down 25 times keeps the selection visible. Screenshot the scrolled
state.

## Gates

`cargo build --workspace`; `cargo build --workspace --features
aui-webview/wry,aui-terminal/pty,aui-terminal/tui`; `cargo test --workspace`;
`cargo clippy --workspace --all-targets -- -D warnings`;
`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`; `python3 scripts/api-doc.py`.
Report per item: done / skipped-with-reason, the screenshots' paths, gate output verbatim
(the last lines of each). When finished write the single word `done` to
`/tmp/muse-owner-lib.done`.
