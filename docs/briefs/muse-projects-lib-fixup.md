# Brief — Projects, library fix-up: the reveal's first frame, the name over the branch

Repository `/Users/latekaapi/Projects/agentic-ui`, branch `projects-2026-09-13` (exists; check
it out — `main` is at the same commit). Work ONLY there. Do NOT commit. Do not touch
`/Users/latekaapi/Projects/harness` or `~/Projects/cockpit`. Prefix every shell command with
`export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`.
Read `docs/00-agent-brief.md` and `docs/04-design-rules.md`. Two items, both found by the
owner's audit of the harness package that consumes this library.

## F1 — A collapse is right on its first frame

File: `crates/aui-motion/src/collapse.rs` (and `crates/aui-motion/src/lib.rs` exports).

Diagnosis (measured on the harness, `--screenshot` captures at 15 s and 30 s and one with a
late repaint): `collapse` wraps the child in `gpui_kit::base::MotionReveal`
(source: `~/.cargo/registry/src/*/gpui-base-0.6.0/src/motion/reveal.rs`). On its first sight
that element has no measured height, so `request_layout` registers **no child** and the
node's auto height is 0; `prepaint` then measures the child as a root, stores the height and
calls `window.request_animation_frame()`. That second frame is what draws the rows — and in
a window nothing else repaints, it does not arrive: a sidebar whose project groups open on
the frame the session list lands shows headers and counts and no rows until something
unrelated repaints (the tier probe, a click). The spring is not involved: `spring()` starts
at its target on first sight.

Fix: aui-motion carries its own reveal element and `collapse` returns it. Copy
`MotionReveal` into `collapse.rs` as `Reveal` (same id, progress, child, element state with
`height: Option<Pixels>`) with one change: when the state has **no** height, `request_layout`
lays the child out as a real layout child (`self.child.request_layout(window, cx)`, passed to
`window.request_layout(style, [child_id], cx)`) with the style's height left auto, so the
node's first frame is the child's natural height times nothing — full height when open
(`progress == 1.0`), and when opening from unmeasured with `progress < 1.0` the node is
still full height for that one frame, which is the same one-frame overshoot gpui-base has
today on close, accepted. In `prepaint`, in that unmeasured case, read the child's height
with `window.layout_bounds(child_id).size.height`, store it, and prepaint the child normally
(`self.child.prepaint(window, cx)`, not `_at`); once a height is stored, behave exactly as
gpui-base does (measure as root, `height * progress`, clip, `request_animation_frame` on a
change). Keep the `RequestLayoutState` carrying the child layout id for the unmeasured
path. `paint` unchanged apart from the clip. The public signature of `collapse` may change
its first tuple element to the new type; the four callers (`transcript/card.rs:125`,
`nav/roles.rs:490`, `nav/views.rs:370` and `:399`) bind it as `(reveal, _)` and need no edit.
Remove the `gpui_kit::base::MotionReveal` import.

Proof: a test in `crates/aui` that draws a `sidebar_view` with one open `ProjectGroup`
holding three sessions for **exactly one frame** and asserts the session rows painted (the
gpui `test-support` feature exposes `window.painted_quads()`; `crates/aui/tests/keyboard.rs`
shows how the library drives a test window — follow it). If a single-frame draw cannot be
asserted that way, say exactly why and instead add a gallery card state to `sidebar/views`
that captures on the first frame (`AUI_GALLERY_VIEWS_STEPS`, see `docs/03-parity-process.md`).

## F2 — The project name wins over the branch

File: `crates/aui/src/nav/views.rs` (`ProjectGroupRow::render`).

Fault (seen with a project on branch `projects-2026-09-13`): the trailing branch is
`flex_none` with `max_w(120)` and the name is `min_w(0).truncate()`, so a long branch squeezes
the name to two letters and an ellipsis. Rule: the name never drops below a readable minimum
(a new `NAME_MIN` of 96 px) while the branch shrinks first — the branch gets `flex_shrink`,
`min_w(0)`, `truncate`, and is dropped entirely below 40 px of room; only once the branch is
gone does the name truncate. Keep the tray and count where they are.

Gallery: the `sidebar/views` project column gets one group whose branch is
`feature/projects-2026-09-13-long` at a width that forces the branch to give way first; the
`sidebar/sidebar` card is unchanged.

## Gates

`cargo build --workspace`; the all-features build
`cargo build --workspace --features aui-webview/wry,aui-terminal/pty,aui-terminal/tui`;
`cargo test --workspace`; `cargo clippy --workspace --all-targets -- -D warnings`;
`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`; `python3 scripts/api-doc.py`.
Screenshot the `sidebar/views` card in both themes to `/tmp/aui-projects/views-fixup-<theme>.png`.
Report per item: done / skipped-with-reason, test names, screenshot paths, gate output
verbatim (last lines); never claim a gate you did not run. When finished write the single
word `done` to `/tmp/muse-projects-lib-fixup.done`.
