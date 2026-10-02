# r1002 — Right pane (workbench) + transcript links: diagnosis

Date 2026-10-02 · baaz main @ 8bbbc41 · agentic-ui v0.3.14 (= ~/Projects/agentic-ui b072936)
Read-only pass: no source edits. One offline measurement run (`--replay`, no turn sent), described in §2.

Path shorthands: `app.rs` = `crates/baaz/src/app.rs`; `aui/…` = `~/Projects/agentic-ui/crates/aui/src/…`;
`aui-webview/…` = `~/Projects/agentic-ui/crates/aui-webview/src/…`.

---

## 1. No UI way to reach Files / Diff / Changes

### Current behaviour (confidence: high)

Four kinds exist — `RightKind::{Browser, Diff, Git, Files}` (`crates/baaz/src/layout.rs:34-84`, labels
"Browser", "Diff review", "Changes", "Files"). How you can open each today:

| Route | Where | What it does |
|---|---|---|
| Header button `PanelRight` | `app.rs:2805-2833` (`hd-centre-toggle-right`) | `toggle_right` — reopens the **last** kind (default Files, `layout.rs:289-291`). It does not choose a kind |
| Header button `Terminal` | `app.rs:2798` | terminal dock, not the right pane |
| Right header cell | `app.rs:3740-3748` | **a plain text label** (`hd-right-title`, `kind.label()`). Nothing to click |
| ⌘⌥B / ⌘\ | `keymap.rs:114`, `app.rs:3103`, `app.rs:3972` | `toggle_right` again, so it also can't choose a kind |
| ⌘K palette rows | `dialogs.rs:563-578` → `show_right(kind)` | the only per-kind route |
| Slash commands `/browser /diff /changes /files` | `overlays.rs:456-459` | the same `Command::Right*` |
| Agent `browser_open` | `right.rs:582-608` `show_browser_for_agent` | Browser only |
| `--steps right:<kind>` | `steps.rs:341-400` | capture aid |

So switching kinds without ⌘K or a slash command can't be done. Once the pane is on Browser, the toggle only
ever reopens Browser. The owner reads the two header glyphs as "browser" and "terminal" because the PanelRight
toggle is the only right-pane control.

`show_right` (`app.rs:2031-2049`) **toggles** when it is asked for the kind that is already showing. That is
right for ⌘K but wrong for a tab click, so any tab UI needs a non-toggling selector.

The library already ships the intended control. `aui::shell::header::right_header` (`aui/shell/header.rs:347-389`)
is documented as "the pane's tab strip, `+`, spacer, close". It wraps `aui::shell::tab_strip::tab_strip`
(`aui/shell/tab_strip.rs:101-140`), which has a sliding ink indicator, `TabItem::closable(false)` and `on_select`.
Baaz never adopted it and draws a bare label in that slot. These sprite icons exist and fit the kinds: `globe`,
`edit`/`split`, `git`, `folder`, `panel-right`.

### Options

**A — keep one toggle, put a tab strip at the top of the pane (recommended).**
- `app.rs:3740-3748`: replace the `hd-right-title` label with
  `right_header("hd-right").tabs(tab_strip("hd-right-tabs", tabs, active).on_select(...)).on_close(...)`.
  The four tabs are `TabItem::new(slug, label, icon).closable(false)` with these icons: Browser→Globe,
  Diff→Edit (or Split), Changes→Git, Files→Folder. Drop the `+` (no `on_add`), or have aui make `+` optional,
  because `RightHeader::render` always draws it (`header.rs:367-388`), so that is a small library change.
- New `Harness::select_right(kind)` in `app.rs` beside `show_right`. It sets the kind and opens the pane, never
  toggles, then does the same persist/refresh/notify as `show_right`. Wire it to `on_select`; the close X calls
  `toggle_right`.
- `app.rs:2805-2833`: the PanelRight toggle stays the only header control, so the header stays uncluttered.
- Keyboard: add four `KeymapEntry`s (for example ⌘⌥1…4 → `RightBrowser`/`RightDiff`/`RightGit`/`RightFiles`
  actions) in `keymap.rs:~114`. They then appear in Settings → Shortcuts automatically.
- Accessibility: TabStrip tabs carry their labels; give the strip a `Role::TabList` label "Right pane" (CLAUDE.md
  rule: role + human label in the same change).
- Trade-off: while the pane is closed, the toggle still reopens the last kind. That is acceptable because the
  tabs are one click away once it opens.

**B — split button in the centre header.** `PanelRight` plus a chevron that opens a 4-row menu with a check on
the current kind and the shortcut on the right. This needs a new `MenuKind::RightPane` in the overlay menu stack
(`overlays.rs`) and a trigger-bounds report like the overflow button's (`app.rs:2840-2852`). The webview already
hides under any non-account menu (`browser.rs:519-523`). It is denser, but it adds a second header control and
hides the kinds behind a click.

**C — `segmented` control at the top of the pane body.** `aui/workbench/diff_review.rs:271-330` already has one,
with keyboard ←/→ and a radiogroup label. Each `right::render` arm (`right.rs:416-528`) would grow a common top
band. It costs vertical space inside the pane and duplicates the header cell's job. Not recommended.

### Verification
- Unit (gpui test): clicking each tab calls `select_right`. Assert `(right_open, right_kind)` after each, and that
  clicking the active tab does **not** close the pane. Model this on
  `show_right_opens_each_kind_and_closes_the_current_one` (`app.rs:4601`).
- Tier V: add an entry `right-tabs` (steps `right:files`) and re-baseline the five `right-*` entries **on main**,
  in their own commit, because the header cell changes:
  `cargo build -p baaz && python3 ~/.claude/skills/relay/scripts/relay_visual.py --repo . --entry right-browser --entry right-diff --entry right-git --entry right-files --entry right-closed`
  then look at the images (CLAUDE.md: "a capture that exits 0 is not evidence the script ran").

---

## 2. Opening and closing a right-pane item is slow

### Measurement

Command (offline, free, no turn; the scratch state dir is throwaway):

```bash
S=$(mktemp -d); BAAZ_STATE_DIR=$S BAAZ_FRAME_TRACE=1 BAAZ_TRACE=1 ./target/debug/baaz \
  --replay fixtures/msp/transcript-real.jsonl \
  --steps "wait:3000;right:browser;wait:2500;right:off;wait:2500;right:browser;wait:2500;right:off;wait:2500;right:files;wait:2500;right:off;wait:2500;right:files;wait:2500;right:off;wait:2000" \
  2> $S/out.log
# frame rows: $S/frame-trace.log ; timings: grep search- $S/out.log
```

This build is a debug/dev profile (opt + debuginfo). There was no real display pacing: frames arrived at
irregular 3–35 ms intervals.

| Observation | Number |
|---|---|
| Frames of motion per open/close (spring) | ~0.55–0.60 s of continuous frames for every toggle (for example t=5.887→6.439 s, 8.428→9.026 s) |
| Layout spring 360/32/1 (simulated, 480 px pane) | 50 % at 81 ms, 90 % at 167 ms, 98 % at 219 ms; settles to ε=0.1 px at ~500 ms |
| First Browser open (WKWebView creation) | **one 183 ms frame** (t=3.323 s) |
| Browser close | one **70 ms** frame (t=11.104 s) |
| Normal frames during the spring | 1.5–2.6 ms draw, so gpui-side layout is **not** the bottleneck |
| UI-thread search-row rebuild **per toggle** | 21–42 ms (`search-rows-built rows=1239 in=…ms`) |
| Background FTS rebuild **per toggle** | 2.4–3.0 s each (`search-rebuild-done … in=2425…2996ms`) |
| `search.db` in a fresh state dir after boot + 8 toggles | **183 MB**. The owner's real one is 220 MB |
| Idle after the browser was ever created, pane **closed** | root re-renders every 55–58 ms forever (draw 4–5 ms) |

### Causes, in order of how visible they are

**2a. The WKWebView is resized, not moved, on every animation frame (high confidence).**
`aui-webview/view.rs:403-436` `laid_out()` pushes `visible = bounds ∩ clip` to `backend.set_bounds`. The right
column clips with `overflow_hidden` while it springs (`aui/shell/app_shell.rs:282-287, 372-380`), so `visible`
grows from 0 to 480 px over ~30 frames. Every frame gives the native view a new frame **width**, so WebKit
re-lays-out and re-paints the page at each intermediate width in its WebContent process, asynchronously and
behind gpui's Metal frame. That is the "page arrives visibly late" in the recording. gpui itself avoids this
reflow by design: it lays the pane out at its rest width (`app_shell.rs:289-291`, "slides under the divider
instead of reflowing"). The native page gets no such treatment.
*Fix (aui-webview):* push the unclipped `bounds` (rest size) when the clip cuts only the side the window's
content view clips anyway. The right pane hangs off the window's right edge, so AppKit clips it for free and
only the origin moves. Alternatively, add a `WebviewState::defer_native_during(motion)` that keeps the native
view hidden, paints the existing snapshot stand-in (`set_obscured` path, `view.rs:340-359`), and shows the
native view once the spring has settled (one `set_bounds`). Host side: pass `resizing`/spring-in-flight into the
pane.

**2b. Every toggle runs the whole session-list settle: rejoin, sessions.json write, full FTS reindex (high confidence).**
`toggle_right`/`show_right` (`app.rs:2011-2049`) → `save_right_for_active` (`app.rs:2065-2098`) → `set_override`
(`app/list.rs:46-49`) → `settle_overrides` (`app/list.rs:75-80`). That runs `rejoin()` over every row
(`app/lifecycle.rs:1090+`), a synchronous `sessions::write` (44 KB JSON on the UI thread), and
`rebuild_search_index` (`app/find.rs:234-259`). The rebuild builds rows **on the UI thread** (21–42 ms measured)
before the first animation frame, then spawns a `DELETE FROM sessions_fts` + full re-insert
(`search.rs:191-211`, 2.4–3 s of background CPU and disk). The pane's open/kind state is not searchable, so all
of this is wasted, and the delete/re-insert churn is why `search.db` keeps growing (183 MB in a fresh dir after
~10 rebuilds). Overlapping rebuilds from quick toggles contend for the SQLite write lock. The same path runs on
every expand/collapse, preview, and select in Files (`right.rs:536-571`), and on every real browser navigation
(`browser.rs:470-487`).
*Fix:* add `Harness::set_right_state(session_id, RightState)` that edits `overrides` in memory and schedules a
**debounced, off-thread** `sessions::write`, with no `rejoin` and no `rebuild_search_index`. Use it from
`save_right_for_active`, `show_browser_for_agent` and the URL sync. Separately, make `rebuild_search_index`
incremental (upsert changed rows) and `VACUUM`/`optimize` once. That is out of scope for the pane but it is the
same bug.

**2c. Closing the Browser shows the wrong content while it slides out (high confidence).**
`app.rs:3719-3720` only builds the webview element while `right_open`, so on the close frame `right::render`
gets `browser = None` and draws the **"Opening the page / The webview is being attached."** placeholder
(`right.rs:426-433`). Meanwhile `sync_browser` hides the native view at once because `pane_open` is false
(`browser.rs:563-579` → `view.rs:348-359`). The page vanishes, a loading card slides out for ~0.5 s, and a 70 ms
hitch was measured on that frame (`release_keyboard` + `set_visible(false)`).
*Fix:* keep passing the existing webview entity while the column is still animating closed (render
`browser_pane` obscured, which paints the last snapshot), or render the snapshot stand-in rather than
`loading_state`. Only build a new webview when the pane is open.

**2d. First Browser open creates the WKWebView inside `render` (high confidence; 183 ms).**
`ensure_browser_person` → `browser_for` → `WryBackend::new_at` (`browser.rs:285-340`) runs synchronously in the
frame that starts the animation, so the spring's first ~11 frames' worth of time is gone.
*Fix:* pre-warm one hidden webview after boot on idle (for example 2 s after the first frame), or on hover of the
toggle/tab. Keep the "never create as a render side effect" rule by creating it from a spawned task with the
window.

**2e. Once any webview exists, the whole root re-renders at ~18 Hz forever, even with the pane closed (high confidence).**
`aui-webview/view.rs:569-583` `drain()` does `if changed || self.native { cx.notify() }` on a 50 ms poll
(`view.rs:30`). Hidden webviews included. The trace's 55–58 ms cadence continues after every pane is closed. It
costs about 4–5 ms of render per tick, and battery.
*Fix:* `if changed || (self.native && self.shown)`. A hidden view needs no `laid_out` refresh, and
`sync_visibility` already runs on the tick.

**2f. Data kinds open on a loading card when their cache is cold (medium confidence for "slow").**
Files, Diff and Changes draw `loading_state` until the background read lands (`right.rs:435-528`,
`refresh_right_now` `app.rs:2180-2241`; git subprocesses plus a walk capped at 300 entries). A warm cache from an
earlier open within the same root draws at once. The first open per project does not, so the pane arrives and
then fills.
*Fix:* warm the cache for the current project on project activation (off-thread, same `refresh_right_now` body,
without the `right_open` guard) so the first open draws real rows.

**2g. The animation itself is ~0.5 s (design choice, low priority).** It is the `Layout` spring
(`aui-motion/spring.rs`, 360/32/1). If the open still feels heavy after 2a–2e, the pane could use the `Swap`
spring or snap via `resizing(true)` (the existing `right_snap` bypass, `app.rs:3726`).

### Verification
- Same command as above, before and after each fix. Expected results:
  - `grep -c search-rows-built out.log` does **not** grow per toggle (2b).
  - `search.db` size stays flat across toggles (2b).
  - No row with `draw_us` > 16 000 at open or close after warm-up (2c, 2d).
  - Rows stop (no 55 ms cadence) once the pane is closed and idle (2e): `python3 scripts/frame-trace.py $S/frame-trace.log`.
- Unit: a gpui test that `toggle_right` with a session active does not call `rebuild_search_index`. Add a counter
  the way `take_draw_samples` (`session.rs:2271`) does.
- For 2a: an aui-webview test against `FakeWebBackend` asserting that `set_bounds` sizes stay constant (only the
  origin changes) across a sequence of narrowing clips at the window edge.
- Visually: a screen recording on the owner's Mac of ⌘⌥B on Browser. The page should slide in already
  rendered, with no re-wrap.

---

## 3. Transcript URLs open in Chrome, not the in-app browser

### Current behaviour (confidence: high)
- aui classifies links in `aui/transcript/markdown.rs:238-251`: `http(s)`/`mailto` become `LinkTarget::Url`, and
  anything else becomes `LinkTarget::Path`.
- Baaz wires them through `session/render.rs:305-315` → `SessionView::handle_link`
  (`session/render.rs:896-903`): `LinkTarget::Url(url) => cx.open_url(&url)`. That is the system default
  browser.
- The same thing happens for terminal OSC 8 links: `app.rs:2383-2390` (`TerminalGridIntent::OpenUrl` →
  `cx.open_url`).

### Fix
1. `session.rs:250` add `SessionEvent::OpenUrl { url: String, external: bool }`. In `handle_link`, emit it in
   place of `cx.open_url`. Read `window.modifiers().platform` in the link closure (`render.rs:313`) so ⌘-click
   (or a `mailto:`/non-http scheme) stays `external: true` → `cx.open_url`.
2. `app/lifecycle.rs:3524` `on_session_event`: on `OpenUrl{external:false}`, set the kind to Browser and open
   the pane with the non-toggling `select_right` from §1, then store `self.browser_pending_url = Some(url)`.
   `on_session_event` has no `Window`, so `ensure_browser_person` (`browser.rs:348-358`, called from
   `app.rs:3719` where the window is at hand) consumes the pending URL and does
   `state.update(|s, _| s.navigate(&url))`, the same as `step_browse` (`browser.rs:360-372`). Don't arm URL-field
   focus for this route.
3. Optional: route `app.rs:2388` (terminal OSC 8) the same way.
4. Settings toggle "Open links in Baaz's browser" (default on) if the owner wants an escape hatch.

### Verification
- gpui test: build a `SessionView`, call `handle_link(LinkTarget::Url("https://example.com"))`, and assert the
  emitted event. On the Harness, assert `layout.right_open && right_kind == Browser` and the fake backend's
  `url()` equals the URL after one render (fake backend under `cfg(test)`, `browser.rs:308`).
- Steps verb for Tier V: `link:<turn>:<n>` that clicks the n-th link of a turn through the same handler. Capture
  `right-browser-from-link` offline using a replay fixture containing a link, then look at the image.

---

## 4. File and folder links should reveal in Files and preview with highlighting

### Current behaviour (confidence: high)
- **What becomes a link:** explicit `[label](dest)` with a non-http dest becomes `LinkTarget::Path`
  (`markdown.rs:238-251`). A bare token becomes a link only if it contains `/` **and** ends in a known extension
  (`is_path_token`, `markdown.rs:270-284`). So bare `main.rs`, bare `crates/baaz/src` and inline-code paths are
  **not** links. `[crates/baaz/src](crates/baaz/src)` and `[lane.rs](crates/baaz/src/session/lane.rs)` are.
- **What a click does:** `handle_link` → `reveal_workspace_path` (`session/render.rs:911-920`). The path is
  resolved by `resolve_link_path` (`render.rs:2171-2189`): absolute stays as is, relative joins
  **`SessionView.workspace`** (the session cwd), and `..`/`.` are normalised. Then a synchronous
  `std::fs::metadata` on the UI thread, then `cx.open_with_system(path)`. A folder opens **Finder** and a file
  opens its **default app** (Xcode/TextEdit). A missing path toasts "No such file".
- **Defect:** `resolve_link_path` strips only a `:line` suffix. Claude Code's usual `path/file.rs#L42` (and
  `#L10-L20`) and `file:///…` URIs resolve to non-existent paths and toast "No such file". (High confidence from
  the code; not reproduced live.)
- **The Files pane today** (`right.rs`):
  - Tree root = **current project root** (`right_project` → `current_project`, `app.rs:2167-2169`), *not* the
    session workspace. They usually match but can differ (a session in a subfolder or an unadopted cwd).
  - Directory expansion: `toggle_expanded` (`right.rs:1278`) only *toggles*. There is no "expand all ancestors".
  - Selection marker: `mark_selected` (`right.rs:1207`) highlights one row. Selecting a **directory** through
    `begin_file_preview` toggles it rather than selecting it (`right.rs:1319-1323`).
  - **No reveal/scroll-to-row.** The baaz wrapper tracks a `ScrollHandle` (`right.rs:1720-1729`), but
    `aui::workbench::files::file_tree` scrolls its own rows in an inner `overflow_y_scroll`
    (`aui/workbench/files.rs:264`; rows are fixed `ROW_H = 24`, `files.rs:34`). There is no `scroll_to(id)`.
  - **File preview exists:** `begin_file_preview_for` (`right.rs:545-571`) does an off-thread read and renders
    `file_preview_pane` (`right.rs:1739-1840`). Markdown goes to `doc_pane`, text to `aui::transcript::code_block`
    with a language from `preview_language` (`right.rs:1223`), and binary or >1 MB files go to a `file_card`.
    There is no line targeting: `code_block.start_line` exists (`aui/transcript/code.rs:183`), but there is no
    "highlight/scroll to line N".
- **Syntax highlighting:** `aui/transcript/syntax.rs` is a small lexer with **JS/TS keywords only**
  (`syntax.rs:17-25`). Rust previews get strings, numbers and comments, plus coincidental keywords (`if/let/return`)
  but not `fn/pub/struct/impl/use/match`. aui has an optional `tree-sitter` feature with Rust, TS, TSX, JSON,
  Python and Bash grammars (`aui/Cargo.toml:23-32`, `syntax.rs:146-175`), but baaz doesn't enable it: no
  tree-sitter in `Cargo.lock`, `Cargo.toml:49` has no features.
- **Perf caveat:** `code_block` renders every line on every frame (`code.rs:390`, not virtualised). The Files
  pane isn't `.cached`. With 2e's 20 Hz re-render, a large (say 500 KB) preview costs on every tick. (Medium
  confidence; not measured.)

### Fix
1. **Route:** emit `SessionEvent::RevealPath { abs: PathBuf, line: Option<u32> }` from `handle_link` in place
   of `open_with_system`. Move the `metadata` check off the UI thread into the Harness handler's background task.
   Extend `resolve_link_path` to strip `#L\d+(-L?\d+)?` and to accept `file://`. Keep ⌘-click as "open in
   default app".
2. **Harness handler** (`app/lifecycle.rs:3524`): map `abs` to the Files root with
   `abs.strip_prefix(project_root)`. If the path is outside the project, preview it anyway with the file's parent
   as a transient root for the preview only, or fall back to `open_with_system` with a toast. Then:
   - folder: new `right::expand_to(cache, root, rel)` (inserts every ancestor *and* the folder into
     `expanded`), new `select_dir` (sets `selected`, closes any preview), `select_right(Files)`,
     `refresh_right_now`, then scroll the row into view.
   - file: `expand_to(parent)`, `begin_file_preview_for(root, rel)` (already off-thread), and carry `line` on
     `FilePreview` so `file_preview_pane` passes it to the code block.
3. **aui changes:** `FileTree::scroll_to(id)` or accept an external `ScrollHandle` (index × `ROW_H`, since the
   tree knows the flattened order). `CodeBlock::highlight_lines(range)` + scroll-to-line. Virtualise
   `code_block` above N lines (`uniform_list`), or give the preview its own virtual list.
4. **Highlighting:** enable `aui = { …, features = ["tree-sitter"] }` in `Cargo.toml:49`. That gives real
   grammars for rust/ts/tsx/json/python/bash, and other languages keep the lexer. Measure the build-time and
   binary-size cost before committing. Longer term, add Rust keywords to the lexer as a cheap fallback.
5. Optional: linkify bare `main.rs`-style tokens only when they resolve under the workspace. That needs an
   app-side resolver hook, because aui must stay I/O-free.

### Verification
- Unit: `resolve_link_path(ws, "src/a.rs#L42")` → `ws/src/a.rs`, plus line 42 from a new `parse_link_line`.
- Unit: `expand_to` on `crates/baaz/src` yields `{crates, crates/baaz, crates/baaz/src}` in `expanded`.
- gpui test: `RevealPath` for a directory sets `right_open`, `kind == Files`, `selected == rel` and no preview.
  For a file, the preview path is set and `PreviewContent::Text` lands with `language == "rust"`.
- Tier V: new entries `right-files-reveal` (steps `right:files;files-reveal:crates/baaz/src`) and
  `right-file-preview-rs` (`files-select:crates/baaz/src/main.rs`). Look at the images to confirm the highlighted
  row and coloured Rust keywords. Baseline on main only.

---

## Confidence summary

| Item | Root cause confidence | Evidence type |
|---|---|---|
| 1 | high | code read, plus the unused library control |
| 2a | high (mechanism), medium (share of the perceived lag) | code read; the native-view reflow is not directly measured |
| 2b | high | measured (UI 21–42 ms, background 2.4–3.0 s, DB growth) |
| 2c, 2d, 2e | high | measured frames (70 ms, 183 ms, 55 ms idle cadence) + code |
| 2f, 2g | medium / design | code read + spring simulation |
| 3 | high | code read |
| 4 | high (current behaviour), medium (preview perf) | code read |
