# Brief — Projects, harness package 1: the model

Repository `/Users/latekaapi/Projects/harness`. Branch `projects-2026-09-13` (already
checked out, off `main`). Work ONLY there. Do NOT commit. Do not touch
`/Users/latekaapi/Projects/agentic-ui` (the library at its `main` checkout already carries
the label ramp, `project_mark`, the group-row additions, swatch rows, rail tint and palette
mark this package uses; read `crates/aui/src/nav/views.rs`, `project_mark.rs`,
`crates/aui-tokens/src/lib.rs` `Palette::label`) or `~/Projects/cockpit`. Prefix every
shell command with
`export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`.
Spend rule (non-negotiable): never `turn/start`, `--send`, `send:`/`steer:` steps, the
ignored live tests, `muse logout`, `account/logout`. `session/list`, `session/start`,
`session/read`, `git` reads and `muse skills list` are free. Read `CLAUDE.md`,
`docs/05-handoff.md`, then `docs/12-projects.md` in full (the design; §4 is the data model
you are building), then `docs/02-app.md` §2 and §5.

This package is data and plumbing. At its end the window must draw exactly as today when
one project exists, and group sessions by project when more than one does. The header
crumb, the project menu, the Projects palette, removal and the steps verbs are package 2.

## H1 — `projects.rs`: the store

Files: `crates/harness/src/projects.rs` (new), `crates/harness/src/store.rs` (reuse
`write_atomic` / `read_json`), `crates/harness/src/main.rs` (module).

`Project { id: String (uuid v4), root: PathBuf (canonical), name: String, colour: u8 (1–8),
pinned: bool, added_at: String, last_opened_at: String, defaults: ProjectDefaults }`,
`ProjectDefaults { model_id: Option<String>, effort: Option<String>, approval_mode:
Option<ApprovalMode> }` (the wire enum from `muse_client::schema`). `Projects { version:
u32 = 1, current: Option<String>, projects: Vec<Project> }`, camelCase, every optional field
`skip_serializing_if`. File `projects.json` under `store::support_dir()`.

API: `read()`, `write(&Projects)`, `Projects::add(root) -> &Project` (canonicalize; if a
project with that root exists return it; else name = last path component, colour = the
least-used index among existing projects (ties → lowest), timestamps now),
`find(id)`, `find_by_root(&Path)`, `remove(id)`, `touch(id)` (bumps `last_opened_at`),
`resolve(&self, workspace_root: Option<&str>, meta_project: Option<&str>) -> Option<&Project>`
implementing §4's order exactly (meta id if it exists, else canonical root equality, else
None — never a prefix match), and `sorted(&self) -> Vec<&Project>` (pinned first, then by
newest session activity supplied by the caller as a map id → time; ties by name).

`branch_of(root) -> Option<String>`: `git -C <root> symbolic-ref --short -q HEAD`, run on the
background executor, `None` on any failure, never logs.

Tests (with `HARNESS_STATE_DIR` pointing at a temp dir): round-trip; `add` twice is one
project; `/tmp/x` and `/private/tmp/x` are the same project; colour assignment is
least-used; `resolve` by id beats root, root beats nothing, prefix never matches; a
corrupt file reads as empty.

## H2 — The index reads `workspace_root`

File: `crates/harness/src/index.rs`.

`IndexEntry` gains `workspace_root: Option<String>`. Select it defensively: run
`PRAGMA table_info(sessions)` first and include the column only if present, so an older
Muse index still yields titles rather than an empty map (the schema-drift path at the
second `prepare` returns an empty map today — keep that behaviour for a truly foreign
schema). Add `Index::workspaces(&self) -> Vec<(String, usize, Option<i64>)>` — distinct
`workspace_root`, session count, newest `updated_at_us` — for the Projects palette in
package 2. Test with a temp SQLite file built two ways (with and without the column).

## H3 — Every session, each with its workspace

Files: `crates/harness/src/app/lifecycle.rs` (`load_sessions`), `crates/harness/src/sidebar.rs`
(`SessionEntry`, `join`), `crates/harness/src/sessions.rs` (`SessionMeta`).

`load_sessions` drops the `workspace_root` filter and pages: `limit: Some(200)`, follow
`next_cursor` until `None`, concatenating (all on the same background task; one
`wire_call_in`). `SessionEntry` gains `workspace: Option<String>` (the row's
`workspace_root`, canonicalized when the path exists, verbatim otherwise) and
`project: Option<String>` (the resolved project id). `SessionMeta` gains `project:
Option<String>` (camelCase `project`, `skip_serializing_if` none; `is_empty` updated; the
round-trip test extended). `join` takes the `Projects` (or a resolver closure) and fills
both fields. `rejoin` re-resolves after a project is added or removed. `SessionEntry::replayed`
sets `workspace` to the capture's `workspace_root` if the first `session/…` line carries
one, else `None`.

## H4 — Grouping by project

Files: `crates/harness/src/sidebar.rs`, `crates/harness/src/sidebar_view.rs`
(`render_sidebar`, `render_view_menu`, `ViewAction`), `crates/harness/src/layout.rs`,
`crates/harness/src/app/list.rs` (`ListCache`).

`layout.json` gains `group_by: GroupBy { Date, Project }` (default: `Project` when the
projects file has ≥ 2 projects or any session resolves to none, else `Date` — compute at
read time when the field is absent, and persist once the person toggles it),
`closed_groups: Vec<String>` and `search_all_projects: bool` (default true; used in
package 2). Keep the file's "global, not per workspace" comment true: these are window
preferences.

`sidebar::grouping_by_project(entries, projects, branches, closed, now) -> Grouping::Project`:
one `ProjectGroup` per project in `Projects::sorted` order — `.mark(initial, colour)` with
`Palette::label(colour - 1)` (the view passes the palette in; the pure function takes the
colour index and the view maps it), `.trailing(branch)` when known, `.state(Running)` when any
session in it runs, count = visible sessions, `.open(...)` unless the id is in
`closed_groups`; sessions newest-first with pinned first, each `summary(now)` as today —
then, if any entry resolves to no project, a last group `id "other"`, name
"Other workspaces", `.muted()`, closed by default, its rows carrying `.repo(<folder name of
the workspace>)`. An empty project still gets its group row (count "0"). The empty/hidden/
archived filters apply per row exactly as in the date view.

`render_sidebar` picks the grouping from `layout.group_by`; the view menu gains a
`Toggle` "Group by project" at the top (new `ViewAction::GroupByProject`) and, at the
bottom, `Toggle` "Search all projects" (`ViewAction::SearchAllProjects`, stored only —
package 2 reads it). `sidebar_view(..).on_toggle` flips a group's id in `closed_groups`
and writes layout. `on_group_action` is wired in package 2; for now log nothing and ignore.
`ListCache` keys on `group_by` and the closed set as well. Branches: `branch_of` for every
project on each `load_sessions`, cached on `Harness` as `HashMap<String, String>`.

Tests in `sidebar.rs`: three projects and two stray workspaces group into four groups in
the right order with the right counts and marks; pinned project first; a running session
marks its group; the empty filter hides rows but not groups; the perf test gains a
project-grouping twin.

## H5 — The current project and the `workspace()` audit

Files: `crates/harness/src/app.rs` (`workspace`, `workspace_name`, `window_title`, boot),
`crates/harness/src/app/lifecycle.rs` (`new_session`, `open`), `crates/harness/src/session.rs`
(`SessionHost`), `crates/harness/src/dialogs.rs` (`load_menu_sources`),
`crates/harness/src/main.rs` (boot rules).

`Harness` gains `projects: Projects` and `current_project: Option<String>`.
`Harness::workspace()` returns the current project's root, else `args.workspace`.
`current_project_id()`, `current_project() -> Option<&Project>`. Boot (D39): read
`projects.json`; if `args.workspace` was given explicitly (`--workspace`, or the launch
directory when `--replay`/`--no-connect`/`--steps`/`--screenshot` is set — scripted runs
keep today's semantics), `add` it and make it current; else the stored `current` if it
exists; else the most recently opened; else, if the launch directory is not `/` and not
`$HOME`, add it and make it current; else no current project. Write the file when it
changed. Whenever a session view opens (`open`, `resume`, `new_session`), the current
project becomes that session's project when it has one, `touch` it, and write.

`new_session`: `workspace_root` = current project root (if none, do nothing and return —
package 2 shows the hero); `model_id` = the project's `defaults.model_id` (else as today);
`approval_mode` = `args.approval_mode.or(defaults.approval_mode)`; set
`SessionMeta.project` for the started id through `set_override`; `SessionHost.workspace`
= that root. `window_title`: `"{session} — {project name}"` or the project name; with no
project, "Harness".

Audit every remaining `self.args.workspace` / `workspace()` read
(`grep -n "args.workspace\|workspace()" crates/harness/src -r`) and make each one take the
right root: the session's own (`SessionHost`, `SessionView.workspace`) for anything about a
session; the current project's for anything about "where the next session goes"; leave
the tier probe and bench alone. List every site you changed in the report.

## H6 — Per-project defaults

Files: `crates/harness/src/session/commands.rs` (`set_model`, `set_mode`, the effort
setter), `crates/harness/src/session.rs`, `crates/harness/src/app.rs`.

When a person changes model, effort or approval mode in a session view, the view reports
it to the application (an `Event`/callback the view already uses for other things — find
the one `route` or `SessionView` emits for the sidebar and follow it) and the application
writes it into that session's project `defaults` and persists. A new session in that project
starts with them: `set_model`'s wire call at `session/start` (H5), effort applied to the
view's initial `effort` before the first turn, mode as the start param. Test the pure part:
a `Projects` with defaults, `new_session`'s params built from them (factor the param
construction into a pure function and test it).

## H7 — The `@` index and skills per root

Files: `crates/harness/src/dialogs.rs` (`load_menu_sources`), `crates/harness/src/overlays.rs`,
`crates/harness/src/session/render.rs` (mention picker, `overlays.files` reads),
`crates/harness/src/files.rs`.

`Overlays.files` / `files_truncated` / `skills` become per root: `files_by_root:
HashMap<String, WalkResult>`, `skills_by_root: HashMap<String, Vec<Skill>>`, with
`files_for(root)`, `skills_for(root)`. `load_menu_sources(root)` walks one root (cap 5 000
as now) and lists skills with that root as the working directory; it runs when a session
view for a root opens and the cache lacks that root, and again at `new_session` for the
current root as today. Keep at most 8 roots (drop the least recently used, matching
`SESSION_CACHE_LIMIT`). The mention picker reads `files_for(self.workspace)`; its cache
keys must include the root. The truncation toast names the project.

## H8 — `history.json` honours `HARNESS_STATE_DIR`

File: `crates/harness/src/history.rs`. `history::path()` uses `store::support_dir()` like
every other store. One test that sets the variable and finds the file there.

## H9 — `search.db` learns the workspace

Files: `crates/harness/src/search.rs`, `crates/harness/src/app/find.rs`,
`crates/harness/src/session/events.rs` (`record_created_files`), `crates/harness/src/app.rs`
(`reveal_created`).

Both FTS tables gain `workspace UNINDEXED`. FTS5 cannot add a column: bump a schema
version stored in a `meta` table (create it if absent), and on mismatch drop and recreate
both tables (`sessions_fts` is rebuilt from the list anyway; `files_fts` starts empty —
say so in the changelog). `rebuild_sessions` writes each row's workspace; `record_files`
takes the session's workspace; `search::query` returns it on every hit. `reveal_created`
joins the hit's own workspace, not `args.workspace`. `refresh_search` filters by the current
project's root when `layout.search_all_projects` is false. Tests: a file created in
project A is revealed under A while B is current; a query scoped to B does not return A's
session.

## Gates

`cargo build --workspace`; `cargo test --workspace`;
`cargo clippy --workspace --all-targets -- -D warnings`;
`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`; exactly one `gpui-pre` and one
`gpui-kit` in `cargo tree -d`; `UPDATE_SNAPSHOTS=1 cargo test -p muse-adapter` then read the
diff (nothing should change — say so). Captures: `HARNESS_DETERMINISTIC=1` runs of
`--replay fixtures/msp/transcript-real.jsonl --theme dark --screenshot` before and after
must be byte-identical with one project (the date view is the default then); then, with
`HARNESS_STATE_DIR` at a temp dir holding a `projects.json` with two projects, a screenshot
`/tmp/projects-h1-grouped-dark.png` showing the grouped sidebar (15 s `--screenshot-delay`).
Add a CHANGELOG entry "2026-09-13 — Projects, package 1" listing H1–H9 in one line each.
Report per item: done / skipped-with-reason, test names, every `workspace()` call site you
changed (H5), screenshot paths, gate output verbatim (last lines); never claim a gate you
did not run. When finished write the single word `done` to `/tmp/muse-projects-harness-1.done`.
