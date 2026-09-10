# Workflow brief — Harness improvement list, seven children in parallel, then integrate

Use a workflow with seven children, one per task below, each in its own isolated git
worktree, then one integration step. You are the orchestrator; the children implement.
Repository: `/Users/latekaapi/Projects/harness` (branch `main`, Rust 2021, gpui app). The
`aui` library it depends on by RELATIVE path (`../agentic-ui/crates/*`) is at
`/Users/latekaapi/Projects/agentic-ui`, **checked out on branch `improvements-2026-09-10`**,
which already contains every library change these tasks need (markdown/links/memo,
`tool_group` + `Block::ToolGroup`, `actions_bottom`, `submit_on_enter`, uniform menu
motion, capped/occluded caret menus, chip thumbnails and file chips, `resize_handle` +
`drag_capture_overlay` + `AppShell::resizing`, `drag_region`, `SessionSummary::pinned`,
Pinned group, `RowAction::Archive`, dense-field recipe). Read that branch's
`docs/06-api.md` and `git -C ../agentic-ui log main..improvements-2026-09-10 --stat` before
you design; **do not modify the library** — if something is missing, work around it in the
harness and say so in your summary.

The owner's issue list is `docs/diagnosis/inputs/owner-issue-list.md`; the diagnosis
reports (`docs/diagnosis/{header,sidebar,transcript,composer,overall,library}.md`) cite the
exact file:line for every cause below. Read the report(s) named in your task before editing.

## Hard rules for every child and for you

- Prefix every shell command with
  `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`.
- **Worktrees must be siblings of the repo**: `git worktree add ../harness-wt-<task> -b wf/<task> main`
  → `/Users/latekaapi/Projects/harness-wt-<task>`, so `../agentic-ui` still resolves. Never
  edit `Cargo.toml` path deps; never check out anything in `../agentic-ui`.
- **Spend rule.** Every model turn is billed. No child may run the ignored live tests,
  `harness-probe`, `fixtures/msp/probe*.py`, `--send`, or `--steps` with `send:`/`steer:`.
  Run the app only as `--replay fixtures/msp/<capture>.jsonl` or `--no-connect`, always with
  `--screenshot <path> --screenshot-delay 15000` for proofs. Muse's own storage
  (`~/.config/muse`, `~/.local/share/muse`) is read-only (reading it for wire shapes is fine).
- Read `CLAUDE.md`, `docs/05-handoff.md`, `docs/09-handoff-improvements.md` §4 (D1–D21),
  `docs/07-architecture.md` and the phase doc your task names. Conventions: nothing is
  optimistic (UI moves only on the server's notification, except purely local state such
  as pin/archive/sidebar width); no literal colours/sizes/durations (tokens); comments say
  why; do not widen scope or refactor beyond the task.
- Harness state lives under `~/Library/Application Support/harness` via `store.rs`
  (`write_atomic`/`read_json`). Secrets and device codes never reach a log.
- **Gates, all must pass before a child commits** (verbatim): `cargo build --workspace`,
  `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`; `cargo tree -d` shows
  exactly one `gpui-pre` and one `gpui-kit`. If a `muse-adapter` snapshot changes, run
  `UPDATE_SNAPSHOTS=1 cargo test -p muse-adapter`, read the diff, quote it.
- **Git, explicitly requested:** each child commits on `wf/<task>` in its worktree (message
  ending with `Co-Authored-By: Muse Code <noreply@meta.com>`). Nobody commits to, rebases
  or resets `main` in either repository. The integration step merges the `wf/*` branches
  into a new branch `wf-improvements` off `main`. `main` is untouched.
- Each child: a dated (2026-09-10) entry at the top of `docs/CHANGELOG.md` under a heading
  with its task letter and title (the integrator keeps all entries); screenshots proving
  the change in both themes at `docs/images/improve-<task>-<state>-{dark,light}.png`; a
  summary with files changed, what was done, gate results verbatim, screenshot paths,
  anything not done and why. No child claims a gate it did not run.

## Task A — shell: header and sidebar (`wf/shell`)

Reports: `header.md` items 1–3, 5; `sidebar.md` items 1–5, 7, 8. Docs: `docs/02-app.md`.
Files: `crates/harness/src/app.rs` (render, render_sidebar, footer, act, step),
`sidebar.rs`, `sessions.rs`, `overlays.rs`.

1. Traffic lights: pass `.traffic_lights(false)` to `app_shell` and `sidebar_header`
   (`app.rs` ~1977-1985); only the native lights remain. Collapsed state: add a `--steps`
   verb `sidebar` (toggle) and screenshot it; if the native lights (x≈9–61) now overlap the
   first centre-header control, widen the collapsed column or pad the centre header —
   choose from the screenshot, not from guesswork.
2. Header: the centre header title shows the **active session's label** (derived title /
   name; "Harness" when none). The header is draggable and double-click zooms through
   the library's `drag_region` (already applied inside `app_shell`); make sure none of the
   header's buttons or the rename field is swallowed by it. Remove the right-pane
   toggle (`on_toggle_right`) and the right header's close button; leave `right_open=false`.
3. Overflow "…" menu: `.on_overflow` opens a `view_menu` in `popover_layer` anchored under
   the button with **Rename**, **Fork**, **Archive**. Rename swaps the title for a
   dense inline field (library recipe) committing through the existing rename path
   (`/name`, `ConfirmRename`) and cancelling on Escape. Fork opens the existing fork
   picker (`PaletteKind::Fork`). Archive runs the archive flow of item 6. Escape ordering
   goes through `overlays.rs`. Add `--steps overflow`.
4. Sidebar collapsed rail: `.rail(...)` on `app_shell` built from `aui::nav::rail` with
   `flat(true)`: nav cells "new" (Plus → new session) and "search" (→ the search palette;
   call `focus_search` until Task E lands its palette — the integrator rewires it), a
   separator, one `RailItem::session` per running/waiting session (selected/pulse mirror
   the rows), avatar at the bottom.
5. Sidebar body: above the "Sessions" caption, a nav block with two rows built with the
   library's `nav_item`: **New session** (Plus, ⌘N) and **Automations** (a clock/zap
   glyph) which is a placeholder — muted "Soon" trailing tag, click shows a toast
   "Automations are not wired up yet". The "Sessions" caption gets `on_view_options`
   (sliders icon) opening a `view_menu` with: Show empty (n) / Hide empty, Clear empty,
   Show archived (n) / Hide archived. Remove those toggles from the footer.
6. Per-session state and actions. `SessionMeta` gains `pinned: bool`, `archived: bool`,
   `last_summary: Option<String>`. Row actions become `[Pin, Rename, Archive]`. Pin toggles
   and the list shows a Pinned group first (library `SessionSummary::pinned`). Archive
   opens a danger `dialog` ("Archive "<label>"?" — "Archived sessions stay on disk and can
   be shown from the Sessions menu.") with Archive/Cancel; on confirm the session leaves
   the list (and, if active, the next visible session or the empty state opens) and a
   toast offers Undo (reuse the hide/undo pattern). Archived sessions are excluded from
   the list and from Clear-empty; "Show archived" lists them with a muted Archived tag and
   an Unarchive row action (reuse `Hide`-style handling). Add `--steps pin`, `archive`,
   `show-archived`.
7. Description line: every row shows one muted second line: `last_summary` when present,
   else `first_user_prompt` (index) or the derived title, through the existing `one_line`
   cap; "N turns" stays in the meta. `last_summary` is written (free, no model call) when a
   turn completes in this app: the first line of the last assistant text block, ≤ 120
   chars. Persist through `sessions.json`.
8. Margins: fix the flush-right rows so left/right gutters are equal (container-side fix
   per `sidebar.md` §4; the library row idiom was also fixed — verify with a pixel scan of
   the screenshot and quote the numbers).
9. Footer: restore the library shape — `sidebar_footer(initial, name).detail(email).plan(tier)`
   with the usage meter (`Provider::Muse`, the weekly fraction the tier probe already
   reports, e.g. "12% weekly" → 0.12; omit the meter when unknown) and the chevron
   opening an account `view_menu` with **Sign out**. No list-management buttons in the
   footer.
10. Rename field: apply the library dense-field recipe so the editing row stays at row
    height; prove with the `--steps rename` screenshot (no sibling shift).
11. Docs: `docs/02-app.md` (header, rail, nav rows, view menu, archive/pin, footer),
    `docs/08-keymap.md` if bindings change, CHANGELOG.

## Task B — adapter fold: structured tool results, tool groups, reminderChild, reasoning (`wf/fold`)

Reports: `transcript.md` §C7, §C8, §C9 and the owner's annotated screenshot note: shell
tool cards currently show a raw JSON envelope (`{"chunk_id": "exec-1-1", "command": …,
"description": …, "exit_code": 0, "terminal_status": "completed", …}`) and the todo tool
shows `{"todos":[…]}` / `{"ok":true,"revision":1,"items":6}` as code. Files:
`crates/muse-adapter/src/fold.rs`, its tests/snapshots, `fixtures/msp/`.

1. Learn the real shapes read-only from the newest session logs under
   `~/.local/share/muse/sessions/2026/09/*/*/session.jsonl` (grep `chunk_id`, `todos`,
   `reminderChild`, `"reasoning"`); never modify them. Build **synthetic fixtures** from
   what you find (`fixtures/msp/synthetic-toolshapes.jsonl`, `synthetic-reminderchild.jsonl`,
   `synthetic-reasoning-text.jsonl`, `synthetic-toolgroup.jsonl`) with paths and any
   personal data replaced.
2. Structured shell results: when a shell tool's output/result is a JSON object carrying
   `command`/`description`/`exit_code`/`terminal_status` and an output field, fold it into
   the existing shell tool body: title = command (one line, elided), subtitle =
   description, body = stdout/stderr text, status from exit code / terminal status, plus
   the existing "N more lines" fold. Todo tool calls (args `{"todos":[…]}`) fold into the
   existing todo/plan block instead of a shell card (their `{"ok":…}` result is not shown).
   File reads render as the existing file/read body with the line count. Any other JSON
   result stays a code body but pretty-printed and folded. Keep the D6 ordering rules.
3. Tool groups: consecutive `Block::ToolCall`s inside one assistant turn fold into one
   `Block::ToolGroup` (library type) with a verb-derived summary ("Ran 3 commands",
   "Read 4 files", otherwise "N tool calls") and the group's aggregate state. A group is
   incremental — a new call joins the open group during streaming; group/turn keys stay
   stable. Approvals, questions, errors, plans, todos, thinking and any call awaiting
   approval break the group (D10/D11 liveness).
4. `reminderChild` items produce no block (and `workflow` stays generic). Reasoning items
   fall back to the raw `text` when `summary` is empty so exposed reasoning is never
   dropped; the collapsed line stays the first summary part.
5. Tests: fold tests over the new fixtures; regenerate and READ the snapshots; quote the
   diff. `docs/01-transport.md` gets a short "presentation policy" note (what is dropped
   or regrouped and why) and CHANGELOG.

## Task C — transcript rendering: smooth scroll, no flicker, links, actions, groups (`wf/transcript`)

Reports: `transcript.md` §C1–C6, §C8 (render side), `overall.md` Findings P1, P2. Files:
`crates/harness/src/session.rs` (render_transcript, apply, tickers, open/resume path with
`app.rs`), `transcript.rs`. Docs: `docs/02-app.md`, `docs/07-architecture.md`.

1. Virtualise: replace the `overflow_y_scroll` div in `render_transcript` with gpui
   `list()` + a persistent `ListState` (`ListAlignment::Bottom`), one item per turn
   (turn bodies are built from blocks as today). On fold changes call `splice` for the
   affected range only; follow the tail only when the reader was at the tail (keep the
   `TAIL_SLACK` test semantics); `scroll_to_bottom`/steps keep working. Read
   `~/.cargo/registry/src/*/gpui-pre-0.3.3/src/elements/list.rs` first.
2. Notify less: `apply` notifies only when the fold changed or view state changed; the
   turn ticker drops to 1 Hz (the elapsed row shows seconds) and only while a turn runs.
   Use the library's memoised markdown so unchanged turns are not re-parsed.
3. Measure: add a debug-only frame timer behind `HARNESS_FRAME_STATS=1` (prints
   render-time percentiles to stderr) and a generated stress capture
   (`fixtures/msp/synthetic-stress-300.jsonl`, built by a small checked-in script from an
   existing capture, ~300 turns). Report before/after numbers for that capture in the
   summary; the after must show bounded per-frame cost independent of turn count.
4. Flicker on switch: do not swap `Harness::active` to an empty view synchronously.
   Keep the old view rendered until the new session's first backfill batch applies, then
   swap; when no old view exists render a neutral loading row (never `empty_state`) while
   `loading_history` is true. Backfill failure keeps the old view and shows the existing
   error banner.
5. Links: wire the markdown `on_link` intent — URLs → `cx.open_url`; paths resolve against
   the session workspace (reject escapes above it), existing files → `cx.reveal_path`
   (Finder), directories → reveal, missing → toast. Apply the same to tool-card paths
   where the library exposes a click intent.
6. Actions: turn on `actions_bottom(true)` for assistant and user turns and wire the
   intents: Copy → clipboard; Retry → existing retry path when `retryable_turns` allows,
   else disabled; Fork → fork picker at that turn; user Edit → puts the text into the
   composer draft; Resend → sends the same input (respect the spend rule: it is a real
   turn — it only runs when the owner clicks it in a live session; in replay it is a
   no-op with a toast). Hide Pin if it has no meaning here.
7. Groups: render `Block::ToolGroup` through the library `tool_group` with open state in
   `Folds` keyed stably; `reminderChild` no longer appears (Task B) — nothing to render.
8. Top inset: the first turn's first line must not be cut under the header (owner's
   screenshot); check the transcript's top padding and the sticky/absolute header.
8b. Text selection: the library's `transcript/selectable.rs` (see the branch's
   `docs/06-api.md`) gives `markdown(...)` a `.selection(..)`/`.on_selection_change(..)`
   pair and a `selected_text` helper. Hold the current `TextSelection` on `SessionView`,
   pass it to every turn's markdown, clear it on click elsewhere/Escape, and bind ⌘C in
   the transcript context to copy the selected text (when the composer is not focused or
   has no selection of its own). Screenshot a dragged selection.
9. Screenshots: `synthetic-stress-300` mid-scroll and at tail, markdown-rich capture
   (author `fixtures/msp/synthetic-markdown.jsonl` with headings, table, fence, links),
   tool-group collapsed/open, switch mid-way (`--steps` two resumes) proving no
   "New session" frame. Docs and CHANGELOG.

## Task D — composer: attachments, files, plus menu (`wf/composer`)

Reports: `composer.md` §D2, §D3, §D4 (contents). Files: `session.rs` (composer region
~2140-2280, attach_paths/prompt_for_image ~1498-1530, parts ~825), `images.rs`, new
`attachments.rs`, `crates/harness/Cargo.toml`. Docs: `docs/03-composer.md`.

1. Enter/Shift+Enter now come from the library (`submit_on_enter`); verify with
   `--steps 'draft:hello'` + Enter that one send starts and no stray newline remains, and
   that Shift+Enter inserts a newline. Fix the harness binding only if the library flag
   alone is not enough; document what you found.
2. Plus menu rows: **Attach file or photo** (⌘U), **@ Mention file**, **/ Slash commands**.
   Mention/commands focus the composer and insert `@` / `/` through the existing draft
   path so the caret menus open; attach opens the file prompt accepting any file.
3. Files: `attach_paths` accepts non-images. Text-like types (md, txt, csv, tsv, json,
   toml, yaml/yml, xml, html, source code extensions, no-extension text sniffed as UTF-8)
   are read as text; PDF via the `pdf-extract` crate; xlsx/xls via `calamine` (sheet by
   sheet as CSV-ish text); docx via `zip` + stripping the XML tags of `word/document.xml`;
   anything else is refused with the banner reason. Cap 64 KB per file (truncate, append a
   "[truncated]" note) and 8 files per turn. `parts()` emits one text part per file:
   `--- file: <name> ---\n<content>` before the prompt text. Chips: `ComposerChipKind::File`
   with name and size detail; drag-and-drop of files goes through the same path.
4. Image thumbnails: decode once at attach (`image` crate, already a dependency), downscale
   to ≤ 64 px, hand the library chip its thumbnail through the new `ComposerChip` field.
   Full-resolution bytes still go on the wire as today.
5. `--steps file:<path>` verb; screenshots of a chip row with an image thumbnail plus md,
   pdf and xlsx chips, and of the plus menu. Docs and CHANGELOG. Keep `cargo tree -d`
   clean (one gpui-pre, one gpui-kit); prefer pure-Rust crates.

## Task E — full-text search palette and local store (`wf/search`)

Reports: `header.md` item 4, `overall.md` items 2 and Findings P3, P4. Files: new
`crates/harness/src/search.rs`, `index.rs`, `history.rs`, `files.rs`, `overlays.rs`, the
palette region of `app.rs` (~1366-1428, 1822-1894), `sidebar_header` `.on_search`.

1. `search.db` (sqlite via the existing `rusqlite` bundled build; verify FTS5 with a
   `USING fts5` smoke query at open) under the support dir: `sessions_fts(session_id
   UNINDEXED, label, title, first_prompt, body)` fed from `index::read()` (Muse's
   `search_text` column — verified to carry transcript text) plus `sessions.json`
   overrides, and `files_fts(path, session_id UNINDEXED, kind)` fed by a recorder: when a
   turn completes, scan the fold's tool calls for write/edit/create verbs and record the
   workspace-relative targets. Both rebuilt/updated off the UI thread (`background_spawn`)
   at boot and after each index refresh; queries run off-thread with a query epoch,
   latest wins.
2. Palette: `PaletteKind::Search` — sections **Sessions** (label, a `snippet()` line with
   the match) and **Files** (path, owning session label). Enter/click on a session resumes
   it; on a file reveals it in Finder. Opened by the sidebar-header search icon
   (`.on_search`), ⌘⇧F (retarget the binding; keep the sidebar quick-filter available via
   the palette's empty query showing recent sessions), and the palette `/search` command.
   Add `--steps search:<query>` and screenshot results for a query hitting both sections.
3. `history::write_all` goes through `store::write_atomic`; `history::read`/`append` run
   off the UI thread. `files::walk` pre-lowercases at walk time and `filter` ranks on a
   background task with epoch/latest-wins (the `@` menu must not rank 5 000 paths on the
   UI thread per keystroke).
4. Docs: new `docs/12-search.md` (schema, refresh policy, what "files we created"
   means: paths written by tool calls, workspace-relative), `docs/02-app.md`,
   CHANGELOG. Unit tests for ranking/snippets and the recorder's verb detection.

## Task F — resizable sidebar (`wf/resize`)

Report: `sidebar.md` §6. Files: `app.rs` render region, new `layout.rs` or `store.rs`
entry, `docs/02-app.md`.

1. Render the library `resize_handle` over the sidebar/centre divider and, while a drag is
   active, the `drag_capture_overlay`. State on `Harness`: `sidebar_width`, `resizing`,
   `grab_x`, `start_w`; each drag sets `sidebar_width = clamp(start_w + dx,
   SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH)` (library tokens) and notifies; `AppShell::resizing(true)`
   during the drag so the width tracks the pointer without the spring, spring resumes on
   release. Clear the drag on mouse-up and on window blur/focus loss.
2. Persist the width in `layout.json` (global, not per workspace) through `store.rs`;
   restore at boot; a double-click on the handle resets to the default width.
3. Unit test the clamp; `--steps sidebar-width:<px>` verb; screenshots at min, default and
   max widths. Docs and CHANGELOG. Coordinate nothing else in `app.rs` — the integrator
   merges with Task A.

## Task G — platform: menu bar, ⌘W/⌘Q, app bundle and icon (`wf/platform`)

Report: `overall.md` items 3, 4, 5. Files: `main.rs`, `app.rs` (actions beside
`bind_keys`), new `scripts/bundle.sh`, `assets/`, `docs/08-keymap.md`, `docs/02-app.md`.

1. `cx.set_menus([...])` after `bind_keys`: **Harness** (About Harness → toast/dialog with
   version, Quit ⌘Q), **File** (New Session ⌘N, Close Window ⌘W), **Edit** (Undo, Redo,
   Cut, Copy, Paste, Select All via `OsAction`), **View** (Toggle Sidebar, Command Palette
   ⌘K, Search ⌘⇧F, Theme submenu if the app has a theme toggle), **Window** (named exactly
   `Window`: Minimize, Zoom), **Help** (Harness Documentation → opens `docs/` in Finder).
   New actions `CloseWindow` → `window.remove_window()` and `Quit` → `cx.quit()`, both
   landing on the existing `on_window_should_close`/`on_app_quit` probe-kill hooks; the
   key equivalents come from the menu items, not raw bindings.
2. Verify on a live `--no-connect` run (a window opens on this Mac): capture the menu bar
   with `screencapture -x docs/images/improve-platform-menubar.png` while the app is
   frontmost, then confirm ⌘W closes and ⌘Q quits with no `muse` child left
   (`pgrep -fl muse` before/after) and record the process-table evidence in the summary.
3. Bundle: `scripts/bundle.sh` builds the release binary, generates `Harness.icns` from a
   checked-in placeholder `assets/icon-1024.png` (generate a simple flat rounded tile with
   an "H" via a small Python/Swift script checked in beside it; no external downloads),
   writes `Info.plist` (`CFBundleIdentifier` `dev.harness.app`, name, executable, icon,
   `LSMinimumSystemVersion`), and assembles `target/bundle/Harness.app`. `open` it once
   and screenshot the Dock icon region with `screencapture -x -R`; document usage in
   `docs/02-app.md`. Signing/notarisation out of scope.
4. Docs and CHANGELOG; `docs/08-keymap.md` retires the "No ⌘W / ⌘Q overrides" sentence in
   favour of "delivered by the menu bar".

## Integration step

Create `wf-improvements` off `main`; merge `wf/fold`, `wf/transcript`, `wf/composer`,
`wf/search`, `wf/resize`, `wf/platform`, `wf/shell` in that order, resolving conflicts
by keeping every change (CHANGELOG keeps all entries; `app.rs`/`session.rs` regions are
disjoint by design — where they are not, integrate both behaviours). Rewire the rail's
"search" cell to the search palette (Task A note). Rerun all gates and regenerate/read
snapshots; take one dark and one light screenshot of `--replay fixtures/msp/transcript-real.jsonl`
on the merged tree (`docs/images/improve-integrated-{dark,light}.png`). Remove the seven
worktrees. Report: merge order, every conflict (file + resolution), gate results verbatim,
the screenshot paths, and a consolidated list of anything any child left undone.
