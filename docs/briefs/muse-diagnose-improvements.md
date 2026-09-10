# Workflow brief — diagnose the owner's improvement list (research only, six children)

Use a workflow with six children, one per section below, run in parallel, then one short
integration step that only checks every report file exists and lists gaps. Children are
**researchers**: they read code, run free commands, take screenshots, and write one report
each. Nobody edits source, nobody commits, nobody creates a worktree or branch.

Repository: `/Users/latekaapi/Projects/harness` (branch `main`, gpui app, Rust 2021). The
`aui` library it uses by relative path is `/Users/latekaapi/Projects/agentic-ui` (branch
`main`, clean; crates `aui`, `aui-gallery`, `aui-tokens`, `aui-motion`, `aui-protocol`,
`aui-icons`, `aui-terminal`, `aui-webview`). gpui is `gpui-pre 0.3.3` + `gpui-kit 0.6`;
its sources are in `~/.cargo/registry/src/*/gpui-pre-0.3.3/` (find with `cargo metadata` or
`find ~/.cargo/registry/src -maxdepth 2 -name 'gpui-pre-*'`).

The owner's full issue list is `docs/diagnosis/inputs/owner-issue-list.md`; its five
screenshots are `docs/diagnosis/inputs/image1..5.png` (image1: markdown rendered as raw text;
image2: the "reminderChild" card; image3: a "Searched web … 5 results" collapsed group card
with two rows and "+3 more" — the layout the owner wants for consecutive tool calls; image4:
the harness slash-command menu spanning the full composer width; image5: Claude Code's
narrower dropdown for comparison). Read the whole list first, then your section.

## Hard rules for every child

- Prefix every shell command with
  `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`.
- **Read-only.** Do not modify, create or delete any file except your own report
  `docs/diagnosis/<section>.md`. No `git` writes of any kind in either repository.
- **Spend rule.** Every model turn is billed. Never run the ignored live tests,
  `harness-probe`, `fixtures/msp/probe*.py`, `--send`, `--steps` with `send:`/`steer:`, or
  the app connected to a login. Run the app only as
  `cargo run -p harness -- --replay fixtures/msp/<capture>.jsonl --theme dark --screenshot <path> --screenshot-delay 15000`
  or `--no-connect`. Screenshots go under `docs/diagnosis/shots/<section>-*.png`.
- Muse's own storage (`~/.config/muse`, `~/.local/share/muse`) is read-only.
- Read `CLAUDE.md`, `docs/05-handoff.md`, `docs/09-handoff-improvements.md` §4 (decisions
  D1–D21) and §7, and `docs/07-architecture.md` before your section. Library rules are in
  `/Users/latekaapi/Projects/agentic-ui/docs/00-agent-brief.md` and `docs/04-design-rules.md`;
  the API overview is `docs/06-api.md`; the gallery (`crates/aui-gallery`) is the reference
  for how every component is meant to look and be wired.
- **Evidence, not opinion.** Every claim cites a file and line (or a gpui source path, or a
  screenshot you took). If you could not determine something, say "not determined" and why.
  Do not propose a fix for a cause you did not locate.

## Report shape (each child, `docs/diagnosis/<section>.md`)

For every numbered item in your section:

1. **Symptom** in one line (quote the owner).
2. **Root cause** with file:line anchors. What the code does today and why it produces the
   symptom. Where relevant, what the aui gallery / demo app does differently (file:line).
3. **Library inventory**: which existing `aui` component, token, motion or helper already
   covers the need (file:line), and what is missing. Say explicitly whether the fix belongs
   in the harness, in the library, or both.
4. **Proposed fix**: concrete, at the level of "in `fn x` (file:line) do y"; the gpui API to
   use (cite its path in the gpui-pre source); estimated size (S/M/L); risks; what to test
   and which screenshot proves it.
5. **Open questions** for the owner, only if the item cannot be decided from code.

End with a short "dependencies between items" list and "what I ran" (commands, verbatim).

## Section A — Header (`docs/diagnosis/header.md`)

Items Header 1–5. Specifically determine:
- Where the traffic lights are drawn twice: gpui `WindowOptions.titlebar` /
  `TitlebarOptions { appears_transparent, traffic_light_position, .. }` in
  `crates/harness/src/main.rs` versus any aui window-chrome/titlebar component that paints
  its own lights (grep `traffic`, `titlebar`, `window_controls` in both repos). Which one is
  native and which one the owner calls "rasterised".
- Why click-drag and double-click do nothing: find how `aui-gallery` (or any demo app in
  agentic-ui) makes its header draggable — `cx.start_window_move()` / `start_window_move` /
  `zoom_window` / `on_mouse_down` on the titlebar, `WindowDecorations` — and what the
  harness header (`crates/harness/src/app.rs`, `session.rs`) does instead. Quote both.
- The right-pane collapse button: where it is, what it toggles, whether anything depends on
  it. The overflow "…" button: what it opens today. What menu/dropdown component aui offers
  (`popover_layer`, menu, dropdown) and how the composer's model picker uses it.
- The search icon: what it does today; the command palette that exists (`overlays.rs`); what
  `crates/harness/src/index.rs`, `store.rs`, `history.rs`, `files.rs` index and how
  (format, location under `~/Library/Application Support/harness`, whether transcripts are
  full-text searchable, whether created files are recorded).
- Traffic lights cut when the sidebar is collapsed: measure the collapsed sidebar width and
  the `traffic_light_position` / padding; screenshot both states
  (`--steps` verb for collapsing the sidebar, if one exists — see `docs/02-app.md`).

## Section B — Sidebar (`docs/diagnosis/sidebar.md`)

Items Sidebar 1–8. Specifically determine:
- What the collapsed sidebar renders today (`crates/harness/src/sidebar.rs`, `app.rs`) and
  what the aui sidebar component renders when collapsed (icon rail?). Screenshot both.
- The aui sidebar reference design: the gallery's sidebar entry with New task / Tasks /
  Automations / Inbox / Workspaces rows, its footer (avatar, name, plan, sign out), row
  hover actions, pinned section, secondary muted description line ("acme-web
  feature/checkout" style). List the exact component names, props and intents
  (file:line). Then diff against what the harness builds: margins (why content is flush
  right and how the gallery sets equal gutters), footer (why "Show empty"/"Clear empty"
  live there — see `docs/02-app.md` and the sidebar-noise CHANGELOG entry — and where the
  library's footer would put them), hover actions (what the harness shows: hide/rename?;
  what is needed: pin, rename, archive-with-confirmation; whether aui has a confirm
  pattern — inline or modal).
- Session description line: what per-session data the harness has for free (`sessions.rs`,
  `history.rs`, `session/list` fields, first/last message text) without spending a turn.
- **Resizable sidebar (item 6) — do this thoroughly.** Determine whether aui or gpui-kit has
  any resizable/split-pane component (grep `resiz`, `split`, `drag` in agentic-ui and in
  `~/.cargo/registry/src/*/gpui-kit-*`). Read how Zed does it if its source is on this
  machine (`find ~ -maxdepth 4 -type d -name zed 2>/dev/null`; else summarise from memory
  and mark it "not verified"): the handle element, `on_mouse_down` → `on_mouse_move` on the
  window while dragging, `cx.stop_propagation`, cursor style, min/max clamps, persisting
  width, and why a per-frame re-layout of the whole sidebar is or is not cheap. Recommend one
  design with the gpui-pre 0.3.3 APIs that actually exist (cite paths).
- Rename input too big (item 8): which aui input component renders in the row, its
  default padding/height tokens, and what a dense variant would need.

## Section C — Transcript (`docs/diagnosis/transcript.md`)

Items Chat Transcript 1–9. Specifically determine:
- **Scroll jank (item 1) — do this thoroughly.** How the transcript is laid out today
  (`crates/harness/src/transcript.rs`, `session.rs`): a plain `div().overflow_y_scroll()`
  with every cell rendered every frame, or gpui `list()` / `uniform_list` with a
  `ListState`? Is there a `ScrollHandle`, and how is "stick to bottom on stream" done? Does
  the app call `cx.notify()` on every stream delta and re-render every cell? Are markdown
  cells or `StyledText` re-parsed every frame? Measure: add nothing, but read
  `docs/07-architecture.md` and count cells in the largest replay capture. Then read how
  gpui-pre's `list` element works (`gpui-pre-0.3.3/src/elements/list.rs`: `ListState`,
  `ListAlignment::Bottom`, `ListOffset`, `scroll_to_reveal_item`, `splice`) and how Zed's
  message list uses it if Zed sources are present. Recommend one design; state what the aui
  transcript component (`aui` crate transcript/timeline) would need to change.
- Flicker on session switch (item 2): trace the switch path (`app.rs` open session →
  `session/start`/resume → state) and find the frame where the empty-state view renders:
  which field is `None`/empty at that moment and why. Propose a state machine that keeps the
  old view or shows a neutral placeholder until the new session's items arrive.
- Text selection (item 3): what gpui-pre 0.3.3 offers for selectable text (grep
  `selectable`, `InteractiveText`, `text_selection` in gpui source), what aui's transcript
  cells use, what Zed's markdown does.
- Markdown (item 4): what parses markdown today (grep `pulldown`, `markdown`, `comrak` in
  both repos and in Cargo.lock), what aui's markdown/rich-text component supports (tables,
  headings, lists, code blocks with syntax highlighting — is `syntect`/`tree-sitter` present?),
  and why the harness shows raw `##`/`-` (image1).
- Links (item 5): whether aui's markdown emits link spans with a click intent, how gpui
  opens URLs (`cx.open_url`) and Finder (`open -R <path>` / `cx.reveal_path`); how to detect
  repo-relative paths like `docs/x/y.md` in prose.
- Message actions (item 6): where the copy/etc. actions render today vs the aui gallery
  message component (bottom action row).
- `reminderChild` (item 7): find it in a replay capture (`grep -l reminderChild fixtures/msp/*.jsonl`),
  identify its wire shape (item kind, source, whether it is a subagent/child-session item or
  a Muse-internal system reminder), where `crates/muse-adapter/src/fold.rs` turns it into a
  card, and whether other Muse-internal items get the same treatment.
- Grouping consecutive tool calls (item 8): how the aui gallery renders a collapsed group
  card (image3 style: header row with title + count + chevron, N rows, "+k more"); whether
  aui has a `ToolGroup`/`ActivityGroup` component; where the fold or the app would group
  adjacent tool-call cells.
- Reasoning (item 9): what the wire carries (`reasoning`, `thought`, `summary` items in
  `crates/muse-client/src/schema.rs`, `docs/10-msp-1.1.1-diff.md`, the "thought-silently"
  CHANGELOG entry), what the fold drops, and what aui's thinking/reasoning cell looks like.

## Section D — Composer (`docs/diagnosis/composer.md`)

Items Chat composer 1–6. Specifically determine:
- Enter/Shift+Enter: the current key bindings (`docs/08-keymap.md`, `crates/harness/src/main.rs`
  keymap, aui composer's key handling) and why Enter inserts a newline today.
- Image thumbnails and non-image files: what `crates/harness/src/images.rs` and `files.rs`
  do, what the wire accepts for attachments (schema: image parts, file parts, mime types),
  what aui's attachment chip/thumbnail component offers.
- The plus-menu transition: which aui popover/menu and which `aui-motion` spring or duration
  it uses versus the model picker; why they differ.
- Slash/@ menu width: where the menu's width is set (full width of the composer anchor?)
  and what aui's popover offers for `max_w`/anchoring.
- **Scroll bleed (item 6):** when the slash or @ menu is open, wheel events scroll both the
  menu and the transcript. Find why: does the menu's scroll container stop propagation
  (`cx.stop_propagation()` in `on_scroll_wheel`), is the popover rendered in
  `popover_layer` outside the transcript's hitbox, does gpui deliver scroll-wheel to every
  hitbox under the cursor? Cite gpui's scroll-wheel dispatch (`gpui-pre-0.3.3/src/window.rs`
  or `interactive.rs`) and propose the minimal fix, saying whether it belongs in the library.
- Plus-menu contents: "Attach file or photo", "@ Mention file", "/ Slash commands".

## Section E — Overall (`docs/diagnosis/overall.md`)

Items Overall 1–5. Specifically determine:
- Performance audit: profile-free inspection — where `cx.notify()` is called per stream
  delta, whether the whole `App` view re-renders on every notification, any per-frame
  allocation or parsing in `render` (markdown, regex, JSON), any blocking I/O on the main
  thread (`store.rs`, `history.rs`, `index.rs`, session.jsonl reads), timers. Rank the top
  five by expected impact with evidence.
- Local storage: what lives under `~/Library/Application Support/harness` (list it), the
  formats (JSON files? sqlite? — check `Cargo.lock` for `rusqlite`/`sled`/`redb`), how the
  session index and full-text search are built and whether they scale; recommend whether a
  sqlite (with FTS5) store is warranted and what it would replace.
- Menu bar: what gpui offers (`cx.set_menus`, `Menu`, `MenuItem`, `OsAction` in
  gpui-pre source — cite), how the gallery sets its menus if it does, and the harness's
  current `set_menus` call if any.
- App icon: how the binary is run (plain `cargo run`, no `.app` bundle?), what a placeholder
  bundle needs (`Info.plist`, `.icns`, `cargo-bundle` or a script), and how the gallery does it.
- Cmd+W / Cmd+Q: current bindings, gpui `cx.quit()`, `window.remove_window()`, and what the
  tier-probe cleanup on quit (`tier.rs::kill_live_probes`, `shot.rs`) requires so quitting
  never orphans a `muse` TUI.

## Section F — Library inventory and merge-back map (`docs/diagnosis/library.md`)

Not tied to one section. Produce a table of every aui component the harness uses (grep
`aui::` in `crates/harness/src`) with the gallery entry that shows its reference design, and
a second table of library gaps implied by the owner's list (resizable panes, markdown
richness, selectable text, grouped tool cards, reasoning cell, dropdown menu, pinned
sessions, inline confirm, sidebar collapsed rail, attachment thumbnails, dense input),
each marked: exists / partial (what is missing) / absent. For each gap say whether the
right home is the library (with a gallery entry) or the harness. Also record the library's
gates verbatim from its `CLAUDE.md`/docs and the branch rule (new branch off `main`).

## Integration step

Confirm the six report files exist and each covers every numbered item of its section;
list any item a report skipped. Write `docs/diagnosis/README.md` with one line per report
and the skipped items. Do not edit the reports. Do not commit.
