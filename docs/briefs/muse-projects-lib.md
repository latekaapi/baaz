# Brief — Projects, library package (agentic-ui)

Repository `/Users/latekaapi/Projects/agentic-ui`. Create branch `projects-2026-09-13` off
`main` and work ONLY there. Do NOT commit. Do not touch `/Users/latekaapi/Projects/harness`
or `~/Projects/cockpit`. Prefix every shell command with
`export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`.
Read `docs/00-agent-brief.md`, `docs/04-design-rules.md`, `docs/06-api.md`, then
`/Users/latekaapi/Projects/harness/docs/12-projects.md` §3–§5 (the design this serves; the
harness consumes every item below in its next two packages).

Library rules: no literal colours, sizes or durations outside `design/tokens/*.json` and the
`scale` module; stateless `RenderOnce` components with intents out; both themes; a gallery
entry for anything new; `python3 scripts/api-doc.py` regenerated.

Seven items. Each names its files; stay inside them plus tests, gallery and docs.

## L1 — The label ramp

Files: `design/tokens/tokens.json`, `crates/aui-tokens/build.rs` (only if the generator
needs it), `crates/aui-tokens/src/lib.rs`, `design/tokens/tokens.css` if it is generated
from the JSON (check how; if hand-kept, add the same keys).

Add eight colours `label-1` … `label-8` to both `light` and `dark`: red, orange, yellow,
green, teal, blue, violet, pink, in that order. They tint a project mark (L2) whose initial
is drawn in `bg`, so pick mid-luminance values that keep the initial legible (≥ 3:1 against
`bg`) in both themes, in the calm register of the existing `accent` and semantic hues —
nothing neon. Verify the generated `Palette` has `label_1..label_8` and add
`Palette::label(index: u8) -> Hsla` that maps `index % 8` onto the ramp (0 → `label_1`).
`Palette::COLOR_NAMES` must include the new keys; a test asserts eight `label-*` names in
each theme.

Gallery: extend the `foundations/colour` card with a "Labels" row showing the ramp.

## L2 — `project_mark`

File: `crates/aui/src/nav/project_mark.rs` (new), exported from `nav/mod.rs`.

`project_mark(initial: impl Into<SharedString>, colour: Hsla) -> ProjectMark`: a rounded
square (default 18 px, radius `scale::R_SM`, fill `colour`, the initial `FS_11` semibold in
`p.bg`, single glyph, centred). Builders: `.size(px)` (14 for menu rows and the palette,
18 for the header and group rows, 22 for the rail). Stateless; no click handling. The
`Sidebar` header switcher in `nav/sidebar.rs` (`Sidebar::header`) currently draws an
18×18 accent square; make it use `project_mark` with `p.accent` so the shape has one home
(no behaviour change).

Gallery: new card `nav/project-mark` — the eight colours at the three sizes, both themes.

## L3 — Project group rows

Files: `crates/aui/src/nav/views.rs` (`ProjectGroup`, `project_group_row`, `SidebarView`).

`ProjectGroup` gains:
- `.mark(initial, colour)` — drawn in place of the folder glyph (muted groups keep the
  folder glyph: "Other workspaces" has no mark);
- `.trailing(text)` — the branch, mono `FS_11` ink-3, truncating, sitting before the count;
- `.state(AgentState)` — a `status_dot` after the name, `.pulse()` when `Running`;
- hover actions at the row's right end, the same tray pattern `session_row` uses: `Plus`
  (`GroupAction::New`) and `Dots` (`GroupAction::Menu`). New enum `GroupAction { New, Menu }`.
  Clicking a tray button must NOT bubble to the row's toggle (the session row had exactly
  this bug on 2026-09-13; look at how `session_row` stops propagation and do the same).

`SidebarView` gains `.on_group_action(|group_id, GroupAction, window, cx|)`. The row's
height stays 30 px; the tray overlays the count the way the session row's tray overlays its
meta. `.on_toggle` keeps firing for the row body.

Sessions inside a project group already render through `session_row`; check that
`SessionSummary::repo(..)` is drawn there as a meta item (the "Other workspaces" rows carry
their folder name in it). If it is not, render it as the first meta item.

Gallery: the `sidebar/views` card gets a Project-grouping example with marks, a trailing
branch, one running group with its dot, and a closed muted "Other workspaces" group.

## L4 — Swatch rows in the view menu

File: `crates/aui/src/nav/view_menu.rs`.

`MenuRow::Toggle` gains an optional leading swatch: add `MenuRow::Swatch { label, colour:
Hsla, checked }` (a 10 px circle before the label, the check as `Toggle` draws it). Render it
in both `view_menu` and `view_submenu` — if `view_submenu` takes plain `SharedString`
items, give it a second constructor `view_submenu_rows(id, rows: Vec<MenuRow>)` rather than
changing the existing signature.

Gallery: the existing menu card (find where `view_menu` is demonstrated, likely
`composer/menus` or `sidebar/views`) gains a "Colour" submenu of eight swatches.

## L5 — Rail tile tint

File: `crates/aui/src/nav/rail.rs`.

`RailItem::session(..).tint(colour: Hsla)`: the tile's initial is drawn in `colour` instead
of the default ink; the state dot and everything else unchanged. Ignored by other kinds.

Gallery: the rail in the `sidebar/sidebar` card (collapsed state) shows two tinted tiles.

## L6 — Palette mark

File: `crates/aui/src/overlay/command_palette.rs`.

`PaletteIcon::Mark { initial: SharedString, colour: Hsla }` rendered through
`project_mark(..).size(14.0)` in the leading slot. `PaletteIcon` is `Copy` today; if
`SharedString` breaks that, drop `Copy` and fix the call sites (there are few).

Gallery: the `shell/command-palette` card gets a "Projects" section with two marked rows.

## L7 — Docs and parity

`docs/06-api.md` regenerated by `python3 scripts/api-doc.py`; a row per new component in
`docs/03-parity-process.md` §checklist; a short "Labels" paragraph in `docs/04-design-rules.md`
saying what the ramp is for (identity of a project, never status).

## Gates

`cargo build --workspace`; the all-features build
`cargo build --workspace --features aui-webview/wry,aui-terminal/pty,aui-terminal/tui`;
`cargo test --workspace`; `cargo clippy --workspace --all-targets -- -D warnings`;
`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`; `python3 scripts/api-doc.py`.
Run the gallery once (`cargo run -p aui-gallery`) and screenshot the new and changed cards in
both themes with the gallery's own screenshot hook (see `docs/03-parity-process.md`) to
`/tmp/aui-projects/<card>-<theme>.png`. Report per item: done / skipped-with-reason, test
names, screenshot paths, gate output verbatim (last lines); never claim a gate you did not
run. When finished write the single word `done` to `/tmp/muse-projects-lib.done`.
