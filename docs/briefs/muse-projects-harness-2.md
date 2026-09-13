# Brief — Projects, harness package 2: the surface

Repository `/Users/latekaapi/Projects/harness`. Branch `projects-2026-09-13` (already
checked out; package 1 — the store, grouping, current project, defaults, per-root index,
search column — is on it and committed). Work ONLY there. Do NOT commit. Do not touch
`/Users/latekaapi/Projects/agentic-ui` or `~/Projects/cockpit`. Prefix every shell command
with `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`.
Spend rule (non-negotiable): never `turn/start`, `--send`, `send:`/`steer:` steps, the
ignored live tests, `muse logout`, `account/logout`. Read `CLAUDE.md`, `docs/05-handoff.md`,
`docs/12-projects.md` (§5 is what you are drawing), the package-1 changelog entry, then
`docs/02-app.md` §5–§6 and `docs/08-keymap.md`. Library API you will use (read the source):
`aui::nav::{project_mark, ProjectGroup, GroupAction, view_menu, view_submenu_rows,
MenuRow::Swatch, RailItem::tint}`, `aui::overlay::PaletteIcon::Mark`, `Palette::label`.

Nine items.

## U1 — Group rows: actions

Files: `crates/harness/src/sidebar_view.rs`, `crates/harness/src/app/lifecycle.rs`.

Wire `sidebar_view(..).on_group_action`: `GroupAction::New` on a project starts a session
in that project (a `new_session_in(project_id)` that `new_session` now calls with the
current project), and makes it current; on "other" it opens the Projects palette (U3).
`GroupAction::Menu` opens the project menu (U2) anchored under the row's `…` button —
measure the row the way the view menu is anchored (`on_children_prepainted`, see
`render_view_menu`), right edge aligned to the sidebar content edge; for "other" the menu
has one row, "Add as project…", which opens the Projects palette.

## U2 — The header crumb and the project menu

Files: `crates/harness/src/app.rs` (`render_centre_header`, `window_title`),
`crates/harness/src/sidebar_view.rs` or a new `crates/harness/src/project_menu.rs`,
`crates/harness/src/overlays.rs` (`MenuKind`), `crates/harness/src/dialogs.rs`.

The centre header's title becomes: `project_mark(initial, Palette::label(colour-1))` (18),
the project name (semibold, ink), a `ChevronDown` (11 px, ink-3), a `·` separator (ink-4),
then the session label exactly as today (the Muse provider mark moves before the session
label). The mark + name + chevron is one clickable id `hd-project` with
`track_interaction`; click opens `MenuKind::Project { id, anchor }` under it. With no
current project the crumb reads "Add a project…" and opens the Projects palette.

The project menu (`view_menu` shape, 250 wide): one `Toggle` row per project in sidebar
order, checked for the menu's project (pick: if the active session has zero turns and no
name, start a session in the picked project and hide the abandoned row locally — it never
reaches the wire without a turn; otherwise start a sibling session there); `Separator`;
"New session here"; "Rename project"; `Submenu` "Colour" → `view_submenu_rows` of eight
`MenuRow::Swatch` (label = colour name, checked = current; pick writes the project and
`invalidate_list`); "Pin project" / "Unpin project"; "Reveal in Finder" (`cx.reveal_path`);
`Separator`; "Remove from sidebar" (U4). Escape and click-outside close it, like the other
menus.

Rename: `renaming_project: Option<String>` on `Harness`; the crumb's name swaps for the
dense field (`rename_field` pattern; a `ConfirmRenameProject`-free approach is fine: reuse
`ConfirmRename` and branch on which rename is active); Enter writes `name`, Escape cancels,
empty reverts to the folder name. Also reachable from the group row menu.

## U3 — The Projects palette and the folder panel

Files: `crates/harness/src/overlays.rs` (`PaletteKind::Projects`, `Command::Project`),
`crates/harness/src/dialogs.rs` (`palette_rows`, `run_palette_row`), `crates/harness/src/app.rs`
(`bind_keys`, `set_menus`, a `AddProject` action), `crates/harness/src/sidebar_view.rs`
(`render_nav_block`), `crates/harness/src/index.rs` (`workspaces`, from package 1).

`PaletteKind::Projects`: section "Projects" — every project, `PaletteIcon::Mark`, context
= the root with `~` for home, badge = visible session count; pick = new session there (and
current). Section "Add" — first row "Choose folder…" (`PaletteIcon::Glyph(IconName::Folder)`,
key hint `⌘⇧O`), then "Recent Muse workspaces": `Index::workspaces()` minus adopted roots,
only paths that exist as directories, newest first, at most 12, context = path, badge =
session count; pick = `Projects::add` + current + `rejoin` + `invalidate_list` + a new
session there is NOT started (adopting is not opening). The query filters both sections by
name and path.

"Choose folder…" → `cx.prompt_for_paths(PathPromptOptions { files: false, directories: true,
multiple: false, prompt: Some("Add".into()) })` on a spawned task, the pattern
`SessionView::prompt_for_image` uses; a chosen path is adopted the same way. Cancel does
nothing.

Entry points: `AddProject` action bound to `⌘⇧O` in `ROOT_CONTEXT`; File › "Add Project…"
in `set_menus` above "New Session"; a `nav_item("nav-projects", IconName::Folder, "Add
project")` under "New session" in `render_nav_block`; `Command::Project` (`/project`, "Add or
switch project") in `Command::ALL` after `Clear`; the rail gets `RailItem::nav("projects",
IconName::Folder)` after "search" with a `"projects"` arm. All open the palette.

## U4 — Remove from sidebar

Files: `crates/harness/src/app/list.rs` or `projects.rs`, the dialog code that
`open_archive_dialog` uses.

A dialog in the archive dialog's shape: title "Remove {name} from the sidebar?", body
"Its {n} sessions stay on disk and move to Other workspaces. Nothing in the folder
changes.", buttons Cancel / Remove. Remove: `Projects::remove`, clear `meta.project` on its
sessions (so a later re-add resolves by root), if it was current pick the most recently
opened remaining (or none), `rejoin`, `invalidate_list`, write. Undo is not offered (re-adding
is one click in the palette).

## U5 — Rail tint

File: `crates/harness/src/sidebar_view.rs` (`render_rail`).

Each session tile `.tint(Palette::label(colour-1))` of its project; sessions in Other keep
the default ink.

## U6 — Empty states

Files: `crates/harness/src/app.rs` (`render_no_session`), `crates/harness/src/transcript.rs`
(`empty_state`).

No project at all: a centred hero in the transcript column — title "Add a project", line
"Muse works inside a folder. Add one to start.", two real buttons: "Choose folder…" and
"Recent workspaces" (both open the Projects palette, the second with the Add section
focused). `empty_state`'s line becomes "Muse runs in {project name}." using the project's
display name, not the folder's.

## U7 — Search across projects

Files: `crates/harness/src/dialogs.rs` (`palette_rows` for `PaletteKind::Search`),
`crates/harness/src/app/find.rs`.

Search hits carry their workspace (package 1). Rows get `badge = project name` (or the
folder name for Other); with `layout.search_all_projects` false the query is scoped to the
current project (already in package 1) and the palette's placeholder reads
"Search {project}…" instead of "Search sessions and files…". The Sessions view menu's
"Search all projects" toggle (package 1) is what flips it.

## U8 — Steps verbs and reproducible captures

Files: `crates/harness/src/steps.rs` (tables and the rustdoc table — the doc-vs-code test
fails otherwise), `crates/harness/src/app/lifecycle.rs` (step handlers),
`crates/harness/src/main.rs` (`--sidebar-fixture`), `fixtures/sidebar/projects.json` (new),
`fixtures/ws/{acme-web,acme-internal,notes}/.keep` (new; the roots the fixture uses, so
they canonicalize), `scripts/captures.sh`.

Window verbs: `projects` (open the Projects palette), `project:<path>` (adopt `path` and
make it current, no panel), `project-menu` (open the header project menu),
`project-menu:<name>` (open the group row menu for the project named), `project-colour:<n>`
(set the current project's colour), `group-by:<date|project>`, `remove-project:<name>`
(open the removal dialog), `remove-confirm`.

`--sidebar-fixture <json>`: a list of `{ "sessionId", "workspaceRoot", "label",
"updatedAt", "turnCount", "status" }` merged into the session list as if the wire had
listed them (build `SessionEntry`s through `join` with a synthetic `Session`); allowed only
with `--replay` or `--no-connect`. `fixtures/sidebar/projects.json`: 9 sessions across
`fixtures/ws/acme-web` (4, one `running`), `fixtures/ws/acme-internal` (2),
`fixtures/ws/notes` (1) and two stray roots `/private/tmp/h4ws` (1) and
`/private/tmp/harness-demo` (1); timestamps spread over today/yesterday/last week. The
fixture roots are relative to the repo and resolved at load.

Captures, all `HARNESS_DETERMINISTIC=1`, `HARNESS_STATE_DIR` at a fresh temp dir,
`--replay fixtures/msp/transcript-real.jsonl --sidebar-fixture fixtures/sidebar/projects.json
--screenshot-delay 15000`, into `docs/images/`:
- `projects-sidebar-{dark,light}.png` — steps `project:fixtures/ws/acme-web;project:fixtures/ws/acme-internal;project:fixtures/ws/notes;project-colour:2`.
- `projects-header-menu-dark.png` — same plus `project-menu`.
- `projects-group-menu-dark.png` — same plus `project-menu:acme-internal`.
- `projects-palette-dark.png` — same plus `projects`.
- `projects-rail-dark.png` — same plus `sidebar` (collapsed).
- `projects-remove-dark.png` — same plus `remove-project:notes`.
- `projects-hero-dark.png` — `--no-connect --login signed-in`-equivalent with no projects
  (use whatever `--login` state shows the shell; if none does without a project, add the
  minimal path) — the "Add a project" hero.
- `projects-search-dark.png` — same as the sidebar capture plus `search:acme`.
Run each twice and `cmp` the pair. Extend `scripts/captures.sh` with the eight.

## U9 — Docs

`docs/02-app.md` §5 becomes "Projects and sessions" (the model, the resolution order, the
menu, the palette, removal), §9 gains the File item; `docs/08-keymap.md` gains `⌘⇧O`;
`docs/12-projects.md` gets an "As built" section listing every deviation from §5;
`docs/05-handoff.md` "Where things are" points at Projects as landed and names worktrees
as the next piece; CHANGELOG entry "2026-09-13 — Projects, package 2" with U1–U9 one line
each and the eight image paths.

## Gates

`cargo build --workspace`; `cargo test --workspace`;
`cargo clippy --workspace --all-targets -- -D warnings`;
`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`; exactly one `gpui-pre` and one
`gpui-kit` in `cargo tree -d`; `UPDATE_SNAPSHOTS=1 cargo test -p muse-adapter` then read the
diff (nothing should change — say so); the eight captures byte-identical run to run. Report
per item: done / skipped-with-reason, test names, screenshot paths, gate output verbatim
(last lines); never claim a gate you did not run. When finished write the single word
`done` to `/tmp/muse-projects-harness-2.done`.
