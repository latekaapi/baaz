# Brief — owner round 2, library package (agentic-ui)

Repository `/Users/latekaapi/Projects/agentic-ui`. Create branch `owner-round-2-2026-09-13`
off `main` and work ONLY there. Do NOT commit. Do not touch `/Users/latekaapi/Projects/harness`
or `~/Projects/cockpit`. Prefix every shell command with
`export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`.
Read `docs/00-agent-brief.md`, `docs/04-design-rules.md`. Library rules: no literal colours,
sizes or durations outside the tokens and `scale`; stateless `RenderOnce`, intents out; both
themes; a gallery entry for anything new; `python3 scripts/api-doc.py` regenerated.

Five items from the owner's screenshots of the harness.

## L1 — The project row keeps the sidebar's right margin

File: `crates/aui/src/nav/views.rs` (`ProjectGroupRow::render`).

Fault: the row is `w_full()` **and** `mx(PJ_MARGIN_X)`, so it is 16 px wider than its column
and the branch and count sit flush against the sidebar's edge, while the session rows under
it (`compact_session_row`, `margin_x` 0 in the harness) end 22 px in. Fix so the row's right
edge equals the session rows' right edge: drop `w_full` in favour of a width that respects
the margins (a wrapping `div().px(PJ_MARGIN_X)` around a `w_full` row, or `flex_1` inside the
column). The count must align with the session rows' time column. Verify in the gallery
`sidebar/views` card by eye (crop) and state the measured right edges in the report.

## L2 — Sessions nest under their project, and long groups fold

File: `crates/aui/src/nav/views.rs` (`ProjectGroup`, `SidebarView::render`, `rows`).

- Rows under a project group are indented: a new `PJ_CHILD_INDENT` (16 px) applied to the
  session rows inside a `Grouping::Project` group only (status and date views unchanged), so
  the hierarchy reads. The row's hover tray and time keep their right edge.
- `ProjectGroup` gains `.folded(hidden: usize, expanded: bool)`: when `hidden > 0` a muted
  28 px row follows the sessions reading "Show {hidden} more" (`expanded == false`) or
  "Show less" (`expanded == true`), `FS_12`, ink-3, chevron-down/up glyph before the text,
  same indent as the rows, hover ground like a row. Clicking it emits
  `GroupAction::ToggleMore` through `on_group_action`. The library does not decide how many
  rows to show: the caller passes the rows it wants visible and the count it held back.

Gallery: `sidebar/views` project column shows one group folded with "Show 12 more".

## L3 — Pinned rows say so, and the pin flips to unpin

File: `crates/aui/src/nav/session_row.rs` (both row kinds), `crates/aui-icons` if a `PinOff`
glyph is missing (add it to the sprite the way the other glyphs are added).

- A pinned session (`SessionSummary::pinned`) shows a small pin glyph (11 px, ink-3) at the
  start of its meta line, before the elapsed time, in both `session_row` and
  `compact_session_row`.
- When the row is pinned, the tray's `RowAction::Pin` button draws `PinOff` and its tooltip
  reads "Unpin"; the reported action stays `RowAction::Pin` (the caller toggles).

Gallery: the `sidebar/rows` card gets a pinned row.

## L4 — The plan is a label beside the name

File: `crates/aui/src/nav/parts.rs` (`SidebarFooter`).

Today `.plan(text, warning)` renders on its own row under the name and detail. Move it
inline: a small label (`FS_11`, ink-3; warning → `p.warning`) after the name on the name's
row, truncating the name first if room is short; the detail row stays; the meter row stays.
Keep the builder signature. Gallery: `sidebar/sidebar` footer shows "latekaapi · Power Usage".

## L5 — `folder_drop_card`

File: `crates/aui/src/nav/folder_drop.rs` (new), exported from `nav/mod.rs`.

`folder_drop_card(id) -> FolderDropCard`: a full-width card, dashed 1 px `p.line_strong`
border, `R_LG`, `p.surface_2` ground, padding `SP_5`, centred column: folder glyph (20 px,
ink-2), title "Drop a folder here" (`FS_13` semibold, ink), subtitle "or click to choose one"
(`FS_12`, ink-3), optional `.key("⌘⇧O")` keycap at the right. Builders: `.title(..)`,
`.subtitle(..)`, `.on_click(|window, cx|)`, `.on_drop(|paths: Vec<PathBuf>, window, cx|)`.
Drag-over of `gpui::ExternalPaths` lights the border in `p.accent` and the ground in
`p.accent_soft` (use gpui's `drag_over::<ExternalPaths>` and `on_drop::<ExternalPaths>`);
`on_drop` hands over every dropped path that is a directory and ignores files. Whole card
is the click target with `track_interaction`.

Gallery: new card `nav/folder-drop` in both themes, one at rest and one drawn in its
drag-over state (a builder `.dragging(true)` for the gallery only, documented as such).

## Gates

`cargo build --workspace`; the all-features build
`cargo build --workspace --features aui-webview/wry,aui-terminal/pty,aui-terminal/tui`;
`cargo test --workspace`; `cargo clippy --workspace --all-targets -- -D warnings` (run it
last, after every edit — the previous package reported it clean and it was not);
`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`; `python3 scripts/api-doc.py`.
Screenshot the changed cards in both themes to `/tmp/aui-round2/<card>-<theme>.png`. Report
per item: done / skipped-with-reason, test names, screenshot paths, gate output verbatim
(last lines); never claim a gate you did not run. When finished write the single word
`done` to `/tmp/muse-round2-lib.done`.
