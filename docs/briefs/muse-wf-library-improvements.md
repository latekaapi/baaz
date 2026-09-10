# Workflow brief — agentic-ui library changes for the Harness improvement list (four children + integration)

Use a workflow with four children, one per task below, each in its own isolated git
worktree, then one integration step. You are the orchestrator; the children implement.

Repository for ALL work in this brief: the `aui` component library at
`/Users/latekaapi/Projects/agentic-ui` (branch `main`, clean; Rust 2021; gpui-pre 0.3.3 +
gpui-kit 0.6). The consumer app (read-only for this brief, useful for context) is
`/Users/latekaapi/Projects/harness`; its diagnosis reports in
`/Users/latekaapi/Projects/harness/docs/diagnosis/*.md` cite every file:line these tasks
touch — read the report named in your task before editing.

## Hard rules for every child and for you

- Prefix every shell command with
  `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`.
- **Worktrees must be siblings of the repo**: `git -C /Users/latekaapi/Projects/agentic-ui worktree add ../agentic-ui-wt-<task> -b lib/<task> main`
  → `/Users/latekaapi/Projects/agentic-ui-wt-<task>`. Work only there. Never check out a
  branch in `/Users/latekaapi/Projects/agentic-ui` itself (other children build against it).
- **Do not touch `/Users/latekaapi/Projects/harness`** (no edits, no builds there).
- Library rules (`docs/00-agent-brief.md`, `docs/04-design-rules.md`, `docs/06-api.md`):
  no literal colours/sizes/durations — use `cx.aui().colors`, `aui_tokens::scale::*`,
  `cx.aui().metrics`, `aui_motion` presets/tokens; stateless `RenderOnce` components, data
  in / intents out; `popover_layer` for anything overflowing its box; both themes correct;
  **a gallery entry (`crates/aui-gallery`) for every new component or new visible variant**;
  comments say why, in the surrounding style. Do not widen scope.
- **Spend rule.** Never run anything that talks to Muse or a model. Library only.
- **Gates, all must pass before a child commits** (verbatim):
  `cargo build --workspace`,
  `cargo build --workspace --features aui-webview/wry,aui-terminal/pty,aui-terminal/tui`,
  `cargo test --workspace`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`,
  `python3 scripts/api-doc.py` (regenerates `docs/06-api.md`; commit the result).
  If a parity/snapshot test changes, read the diff and quote it in the summary.
- **Git, explicitly requested:** each child commits on its own branch `lib/<task>` in its
  worktree (commit message ending with the line `Co-Authored-By: Muse Code <noreply@meta.com>`).
  Nobody commits to, rebases or resets `main`. The integration step creates branch
  `improvements-2026-09-10` off `main`, merges every `lib/*` branch into it (resolving
  conflicts, rerunning all six gates), commits, and then **checks that branch out in
  `/Users/latekaapi/Projects/agentic-ui`** so the consumer sees it — say so in the report.
- Each child ends with a summary: files changed, new public API (names), gallery entries
  added, gate results verbatim, anything not done and why. No child claims a gate it did
  not run.

## Task A — markdown, links, and memoised prose (`lib/markdown`)

Read `harness/docs/diagnosis/transcript.md` §C4, §C5 and `overall.md` Finding P1 first.
Today `crates/aui/src/transcript/prose.rs` parses a hand subset (paragraph, `- ` list,
`` `code` ``, `**bold**`) and `AssistantTurn`/`UserTurn` (`transcript/turns.rs`) call
`prose()` directly, so `##`, fences, tables and links paint literally, and every render
re-parses.

1. Add `crates/aui/src/transcript/markdown.rs`: a block-level renderer
   `markdown(id, source, style)` producing `Paragraph | Heading(1..=6) | BulletList |
   OrderedList | CodeBlock{lang, text} | Table{header, rows, align} | Quote | Rule |
   Image{alt, url}` (images render as a bordered placeholder tile with the alt text —
   no network). Use the `pulldown-cmark` crate (add it to `crates/aui/Cargo.toml`, no
   default HTML features) for parsing rather than extending the hand parser; keep inline
   rendering on the existing `prose` run builder (extend `Span` with `Italic`,
   `Strikethrough`, and `Link{label, target}`). Fenced code goes through the existing
   `code_block` with `syntax_runs`; an **unclosed fence while streaming must render as an
   in-progress code block**, never raw. Tables: header row bold, cell padding and rule
   colours from tokens, horizontal scroll inside their own container when wider than the
   column. Headings: token type scale, not literal sizes.
2. Links: `LinkTarget::Url(String)` for `[t](http…)` and bare `https?://…`;
   `LinkTarget::Path(String)` for `[t](relative/path)` and for bare workspace-looking
   paths in prose (`\S+/\S+\.(rs|md|toml|json|jsonl|txt|py|ts|tsx|js|css|html|yaml|yml|sh)`
   and `:line` suffixes) — never inside code spans/blocks. Render underlined in the accent
   colour; emit clicks through an intent: `markdown(...).on_link(|target, window, cx| …)`,
   implemented with gpui `InteractiveText::on_click(ranges)`
   (`~/.cargo/registry/src/*/gpui-pre-0.3.3/src/elements/text.rs`). Cursor pointer on hover.
3. Memoise parsing: parse cost must not be paid per frame. Add a small cache keyed by
   (source hash, style/theme key) — e.g. a `parsed` cache in a `Global` or an
   `Arc<Parsed>` the caller may hold — such that re-rendering unchanged text does no
   parsing. Document the mechanism in the module doc.
4. Switch `AssistantTurn` and `UserTurn` to `markdown(...)` and thread `on_link` through
   them (`assistant_turn(...).on_link(...)`). Keep `prose()` public (other cells use it).
5. Gallery: a `transcript/markdown` entry showing headings, both list kinds, a table, a
   fenced block, a quote, a rule, an image placeholder, URL and path links, in both themes.
6. Tests: unit tests for block parsing (heading, table, unclosed fence, link detection
   incl. a path inside a code span that must NOT link).

## Task B — tool-call groups, bottom action row (`lib/transcript-cards`)

Read `harness/docs/diagnosis/transcript.md` §C6, §C8 and inputs `image3.png`.

1. Protocol: add `Block::ToolGroup { calls: Vec<ToolCall-shaped items>, summary: String,
   state }` to `crates/aui-protocol/src/block.rs` (reuse whatever struct `Block::ToolCall`
   already carries per call, so a group is a `Vec` of those plus a summary). Update
   `sample.rs`/fixtures so tests stay green.
2. Component `tool_group(id, group, open)` in `crates/aui/src/transcript/tool_group.rs`
   with the image3 idiom: one header row (status glyph, summary such as "Ran 3 commands"
   or "Read 4 files", a muted count, a chevron), then when collapsed the first two calls
   as single-line rows (verb + target, mono) and a "+k more" line; when open, every call
   as a full `tool_card` (reuse the existing body painters). Intents out: `on_toggle`
   for the group and per-call toggles. Design tokens only.
3. Bottom action row for turns: add `.actions_bottom(true)` to `AssistantTurn` and
   `UserTurn` (`transcript/turns.rs`) rendering the existing action intents
   (assistant: Copy/Retry/Fork/Pin; user: Edit/Copy/Resend) as an in-flow row of `Xs`
   ghost glyph buttons under the prose, always visible at muted opacity, full opacity on
   hover of the turn; the hover-above toolbar is not rendered when this is on. Keep the
   default (`false`) unchanged so existing gallery cards do not move.
4. Gallery: `transcript/tool-group` entry (collapsed + open, both themes); extend the
   `transcript/turns` entry with an `actions_bottom` state.
5. Tests as the transcript module already does (render/parity where present).

## Task C — composer: Enter sends, uniform snappy menus, capped menu width, no scroll bleed, thumbnails and file chips (`lib/composer`)

Read `harness/docs/diagnosis/composer.md` §D1, §D2, §D3, §D4, §D5, §D6.

1. `composer_state_rows` (`crates/aui/src/composer/composer.rs:~153`): chain
   `.submit_on_enter(true)` on the `TextareaState` so Enter submits (emits `PressEnter`
   and propagates) and Shift+Enter inserts a newline. Single-line fields are unaffected.
2. Uniform, snappier menu motion: the plus menu (`composer/menu.rs`) and the chip pickers
   (`composer/pickers.rs`) currently spring-morph (`SpringKind::Gentle`, scale from .85)
   while the caret menus (`composer/menus.rs`) tween. Make all four use one presence
   tween: add `EnterExit::QUICK` (or equivalent) in `aui-motion` built from existing
   duration tokens (enter ≤ 150 ms, exit ≤ 120 ms — add `durations::QUICK*` tokens if
   none fit; no literals at call sites), fade + 4–6 px rise, scale from .98. Keep the
   `at_rest()`/`present(false)` paths working. Update any parity/snapshot expectations.
3. Cap caret menus: `popover_frame` in `composer/menus.rs` gets a max width from a token
   (add `MENU_W_MAX`-style constant next to the pickers' existing `MENU_W_MIN/MAX`, use the
   same 520 ceiling), left-anchored; long rows wrap, not clip.
4. Scroll bleed: add `.occlude()` to `popover_frame` (as `pickers.rs` already does) so
   wheel/hover/click do not reach elements behind an open caret menu, and add an
   `on_scroll_wheel` handler that calls `cx.stop_propagation()` on the menu's own scroll
   container. Gallery card for the caret menus must still scroll internally.
5. Attachments: give `ComposerChip` an optional thumbnail (`thumbnail: Option<Arc<gpui::RenderImage>>`
   or `ImageSource`) rendered as a rounded 28–32 px tile in place of the glyph for
   `ComposerChipKind::Image`; render `ComposerChipKind::File` with a file glyph, the file
   name, and a muted extension/size detail. Keep `attachment_row` consistent (Image kind
   accepts the same thumbnail).
6. Plus menu: nothing structural — confirm `PlusMenuItem` supports an icon + label +
   optional keycap for three rows; add one if missing.
7. Gallery: update `composer/composer`, `composer/menus`, `composer/pickers`,
   `composer/attachments` entries to show the new motion, the capped width, a thumbnail
   chip and a file chip.

## Task D — shell and sidebar: resizable sidebar, pinned group, archive action (`lib/shell-sidebar`)

Read `harness/docs/diagnosis/sidebar.md` §6, §7, §4 and `header.md` §2.

1. Resizable sidebar. In `crates/aui/src/shell/app_shell.rs`: the sidebar column width
   is `sidebar_width` passed through a layout spring (`spring_px`). Add
   `.resizing(bool)` — when true the width is applied directly (no spring) so it tracks
   the pointer; when it flips back to false the spring settles from the current value
   (no jump). Add a new component `crates/aui/src/shell/resize_handle.rs`:
   `resize_handle(id)` — a 6 px transparent strip meant to be absolutely positioned over
   the sidebar/centre divider, full height, `CursorStyle::ResizeLeftRight`, with intents
   `on_drag_start(|x, window, cx|)`, `on_drag(|x, window, cx|)` (pointer x in window
   pixels, fired via `on_mouse_move` while the left button is pressed) and `on_drag_end`.
   Because hover-gated `on_mouse_move` stops once the pointer leaves the strip, also
   export `drag_capture_overlay(id)` — a full-window transparent element the app renders
   while a drag is active that forwards `on_mouse_move`/`on_mouse_up` to the same intents.
   Document the recipe (state in the app: `resizing`, `grab_x`, `start_w`, clamp) in the
   module doc, and make `AppShell` accept `.sidebar_min/max` bounds or document the
   clamp as the app's job. Provide `SIDEBAR_MIN_WIDTH`/`SIDEBAR_MAX_WIDTH` tokens.
2. Header drag region: add `crates/aui/src/shell/drag_region.rs` — a helper
   `drag_region(id)` that wraps a header row so press-drag calls
   `window.start_window_move()` and double-click calls `window.titlebar_double_click()`
   (macOS) / `zoom_window()` elsewhere, copying the gpui-component `TitleBar` element's
   three handlers (`~/.cargo/registry/src/*/gpui-component-0.6.0/src/title_bar.rs`
   ~318-360); interactive children keep their clicks (they stop propagation).
   Apply it to `app_shell`'s header row so consumers get it for free, with `.draggable(bool)`
   defaulting to true.
3. Pinned sessions: add `pinned: bool` to `SessionSummary` (`nav/types.rs`) and make
   `Grouping::Date` (`nav/views.rs`) emit a "Pinned" group first containing pinned
   sessions (excluded from their date buckets), styled like the gallery's `Pinned 3` group.
   Add `RowAction::Archive` (archive-box icon; add the icon to `aui-icons` if absent) to
   `nav/session_row.rs` and its name/glyph maps.
4. Symmetric gutters: fix the `CompactSessionRow` width idiom (`w_full` + `mx`) so rows
   inside a clipped column keep equal left/right margins (`flex_1`/`min_w(0)` or padding
   on the container, whichever keeps card parity within the documented ≤2 % rule).
5. Dense inline editor: add `dense_field(state)`-style helper (or document the exact
   builder chain) in `nav/session_row.rs`'s module doc so a rename editor fits a 30 px row:
   XSmall size, no appearance/border, text size matching the row title, explicit focus
   border via tokens. A gallery `sidebar/rows` state must show a row mid-rename at row
   height.
6. Gallery: `shell/app-shell` shows the resize handle and a live drag (state in the
   gallery card); `sidebar/views` shows the Pinned group; `sidebar/rows` shows Archive in
   the tray and the dense rename state.

## Integration step

Create `improvements-2026-09-10` off `main`, merge `lib/markdown`, `lib/transcript-cards`,
`lib/composer`, `lib/shell-sidebar` (in that order), resolve conflicts keeping every
change, rerun all six gates, commit, check the branch out in
`/Users/latekaapi/Projects/agentic-ui`, remove the four worktrees
(`git worktree remove`), and report: merged branches, conflicts resolved (file + what),
gate results verbatim, the full list of new/changed public API names, and anything a
child left undone.
