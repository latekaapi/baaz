# Projects — one window over several workspaces

Design record for the Projects feature, 2026-09-13. Research first (four reference apps, the
wire, the codebase), then the decisions taken with the owner, then the data model, the UI,
and the build order. The briefs Muse implements from are `docs/briefs/muse-projects-lib.md`,
`muse-projects-harness-1.md` and `muse-projects-harness-2.md`, in that order.

Supersedes the brief in `docs/09-handoff-improvements.md` §12 where the two differ (§12
recommended one project at a time behind a switcher; the owner chose all projects at once).

## 1. What the four references do

Researched 2026-09-13 from vendor docs, changelogs, issue trackers and (for T3 Code) the
source at `pingdotgg/t3code@2db675a`. Claude for Mac facts include the app's own session
and sidebar tool schemas as a primary source.

| Axis | Claude (Mac) | Codex (Mac, now a ChatGPT.app view) | T3 Code | Synara |
|---|---|---|---|---|
| Project object | None: folder is a session attribute, "project" a grouping key | Registered folder; multi-folder with a primary since Jul 2026 | Thin record: dir on an environment, icon, scripts, defaults | Folder; grouped into user-named Spaces |
| Sidebar | All sessions; Group by Date / Project / State / Custom; Pinned on top; unread, needs-input, failed dots | Flat project → chat tree; By-project or Chronological; Activity inbox; dock badge | One list by attention state (pinned / active / snoozed / settled) with a project scope filter above | Spaces → projects → thread tree; Kanban |
| Add / remove | Native picker + recents from `~/.claude.json`; no remove (open complaint) | ⌘O picker, "start from scratch", three-dot Remove keeps chats | Picker, paste path, clone URL, GitHub/GitLab | ⌘⇧O picker |
| New session | Folder pill, branch, worktree checkbox (on by default), model, effort, mode | Local / Worktree / Cloud target per prompt; Hand off between them | Checkout vs worktree, base branch, per-project model default | Checkout vs worktree; provider, model, effort |
| Worktrees | App-managed under `.claude/worktrees`; archive removes | App-managed, detached HEAD, keeps 15 | `~/.t3/worktrees`; PR merge auto-settles | Managed; cancellable setup |
| Per-project state | Permission mode remembered per folder | AGENTS.md, `.codex/`, skills from the primary folder | Default model, env mode, scripts (`t3.json`) | Favourite file, scripts |
| Cross-project | Group by project; no home screen | Activity view, Scheduled inbox | Status pill rolls up to the project row | Spinners per row; ctrl-tab recents |
| Multi-window | Split + pop-out | Present, buried | No | Split chats |

Common ground: every project is visible at once; sessions group under projects; a project is
added through the native folder picker and removed without deleting its sessions; a
worktree per session is a first-class choice at creation. Failure modes worth avoiding:
Claude's path-keyed grouping hides worktree forks (the session's folder is not its
project's); Codex's `thread/list` cap of 50 hid whole projects; Claude has no way to forget
a folder.

## 2. What the wire and the codebase give us

- `muse serve --trust-workspace` reads "Load each session workspace's skills and rules": one
  host trusts every workspace it opens. No per-project trust gate.
- `session/list` without `workspaceRoot` returns every session, paged (`limit` max 200,
  `nextCursor`), each row carrying its own `workspace_root`. Free.
- `session/start { workspaceRoot }` opens a session anywhere; one child multiplexes.
- Worktrees are CLI-only (`muse -w`); `session/start` has no worktree parameter. A harness
  worktree means `git worktree add` by the harness and the worktree path as `workspaceRoot`.
- Muse's index (`session-index.db`, read-only) has `workspace_root` and `git_branch` per
  session; the harness's reader selects neither yet.
- The harness's `SessionEntry::join` drops `Session.workspace_root`; the library's
  `sidebar_view` already renders `Grouping::Project`; `prompt_for_paths` is in gpui-pre and
  unit-testable; ⌘O and ⌘⇧O are unbound; `history.json` is already keyed by workspace;
  `search.db` has no workspace column; the library has no label-colour ramp.

## 3. Decisions (D30–D41)

Taken with the owner on 2026-09-13.

- **D30 Sidebar shape.** All projects at once: collapsible project groups, pinned rows first
  inside each group, a Date/Project toggle in the Sessions view menu. "Current project" is
  the open session's project, else the last used.
- **D31 Worktrees.** A follow-on slice. This slice stores an explicit project id on every
  session the harness starts, so a worktree session (whose folder differs from its
  project's root) slots in later without regrouping.
- **D32 Unadopted workspaces.** Sessions whose workspace is not an added project show under
  one muted, collapsed group "Other workspaces", each row tagged with its folder name. The
  Projects palette offers those workspaces for adoption in one click.
- **D33 Identity.** An initial mark plus a per-project colour from a new eight-colour label
  ramp in the library tokens (both themes), assigned round-robin at add time, changeable
  from the project menu. Group rows, the header crumb, the palette and the rail tiles use it.
- **D34 New session.** ⌘N starts in the current project. The centre header shows
  `mark project › session`; the project part is a menu: pick another project and the empty
  session is replaced by one there (a session with turns instead gets a sibling there). Each
  group row also carries its own `+`.
- **D35 Per-project defaults.** The last-used model, effort and approval mode are stored on
  the project record and applied to that project's new sessions. `--approval-mode` still
  wins for scripted runs.
- **D36 Search.** All projects, results badged with the project name; a "Search all
  projects" toggle in the Sessions view menu narrows to the current project. `search.db`
  gains a workspace column on both tables.
- **D37 Ordering (rewritten 2026-09-13, owner round 4, O1).** Project groups sort
  pinned first, then by name case-insensitively (ties by `addedAt`, then id) —
  never by recency; no drag reorder. A project never moves because a session in
  it was created, opened, or got a turn. "Other workspaces" is always last.
- **D38 Removal.** Remove from sidebar keeps every session; they move to "Other
  workspaces". Nothing on Muse's side changes.
- **D39 Boot.** `--workspace` wins and is adopted if new; else the stored current project;
  else the most recently opened; on a first run the launch directory unless it is `/` or
  `$HOME`, in which case the window opens with no project and an "Add a project" hero.
- **D40 One window, one process.** No split, no pop-out; the parked-view MRU stays keyed
  by session id and is valid across projects.
- **D41 Identity of a project.** A UUID; the canonical root is unique among projects. The
  name defaults to the folder name and is renameable; a rename never touches the folder.

## 4. Data model

`~/Library/Application Support/harness/projects.json` (atomic write, `HARNESS_STATE_DIR`
honoured), camelCase:

```json
{
  "version": 1,
  "current": "6d0e…",
  "projects": [
    {
      "id": "6d0e…",
      "root": "/Users/latekaapi/Projects/harness",
      "name": "harness",
      "colour": 3,
      "pinned": false,
      "addedAt": "2026-09-13T10:00:00Z",
      "lastOpenedAt": "2026-09-13T11:20:00Z",
      "defaults": { "modelId": "muse-spark-1.3", "effort": "high", "approvalMode": "onRequest" }
    }
  ]
}
```

`sessions.json` `SessionMeta` gains `project: Option<String>` (a project id), written at
`session/start` by this app. `layout.json` gains `groupBy` (`"date"` | `"project"`),
`closedGroups: [id]`, `searchAllProjects: bool`. `search.db` tables gain
`workspace UNINDEXED`.

A session resolves to a project in this order: `meta.project` when that id still exists;
else the project whose canonical root equals the row's `workspace_root`; else "Other
workspaces". Never by prefix, never by the current project.

## 5. UI

- **Sidebar.** Nav block: New session, Add project, Automations (Soon). Then the list:
  project groups (mark, name, state dot when a session runs, branch in mono at the right,
  count; hover `+` and `…`), sessions inside newest-first with pinned first, then the muted
  "Other workspaces" group. With one project and nothing in Other the list stays the date
  view. The view menu: Group by project (toggle), the existing empty/hidden/archived rows,
  Search all projects (toggle).
- **Header crumb.** `mark project ▾ · session label`. The project part opens the project
  menu: the projects (current checked), separator, New session here, Rename project,
  Colour ▸ (eight swatches), Pin/Unpin project, Reveal in Finder, Remove from sidebar. The
  group row's `…` opens the same menu for that project. Renaming swaps the crumb for the
  dense rename field, as session rename does.
- **Projects palette** (⌘⇧O, File › Add Project…, the nav row, `/project`): section
  "Projects" (adopted; pick = new session there), section "Add": "Choose folder…" (the
  native panel, directories only) then "Recent Muse workspaces" from the index, newest
  first, not yet adopted, existing on disk, badged with their session count.
- **Rail.** Tiles keep the session initial, tinted with the project colour.
- **Empty states.** No project: "Add a project" hero with the two ways in. A project with
  no sessions: the existing "No sessions yet · ⌘N starts one."
- **Window title.** `session — project`, or `project`.

## 6. Build order and packages

1. `muse-projects-lib.md` — agentic-ui branch `projects-2026-09-13` off `main`: the label
   ramp, project mark, group-row additions, swatch menu rows, rail tint, palette mark,
   gallery entries.
2. `muse-projects-harness-1.md` — harness branch `projects-2026-09-13`: store, index
   column, unfiltered paged list, project resolution, grouping, current project and the
   `workspace()` audit, per-project defaults, per-root `@` index, search column. Data only;
   the window still draws.
3. `muse-projects-harness-2.md` — same branch: the sidebar groups, header crumb and menu,
   Projects palette and folder panel, removal, rail tint, search badges and scope, steps
   verbs, a sidebar fixture for reproducible captures, docs.

Free throughout: `session/list`, `session/start`, `session/read`, `muse skills list`, `git`
reads. Nothing in this feature sends a turn.

## 7. Deferred

Worktree per session (D31), custom groups or Spaces, an attention inbox across projects,
multi-folder projects, clone-from-URL, per-project scripts, split and pop-out windows,
notifications. Each is a later slice; none is blocked by the model above.

## 8. As built (package 2, 2026-09-13)

Everything in §5 landed. The deviations below are where the brief met the
codebase and bent.

- **Group-row menu anchor.** The brief wanted the menu measured under the
  row's `…` button. The library reports no per-row geometry for a group row,
  and the harness does not fork the library for it — so the row menu anchors
  right-aligned to the sidebar's content edge under the header, by the same
  scroll-bounds math as the view menu, clamped into the window. Deterministic
  in captures; one step removed from the row in life.
- **`MenuKind::Project` carries neither id nor anchor.** The kind stays
  `Copy` (every menu match relies on it): the target id lives on
  `Menu::project` (`None` is "Other workspaces") and the header-vs-row anchor
  on `Menu::project_header`.
- **Colour is click-toggled, not hover-opened.** The submenu hangs off the
  menu's right edge while `project_colour_open` is set; picking a swatch
  writes the project and regroups, leaving the menu open like the view menu's
  toggles.
- **Renaming from the menu switches to that project.** The field lives in the
  header crumb, which shows the current project — so the menu's project
  becomes current (touched, written, no session started) before the field is
  seeded.
- **Two capture-only additions.** No `--login` state showed the shell, so
  `--login signed-in` boots the signed-in chrome on sample identity with no
  child behind it; and every scripted run adopts its launch directory, so
  `--no-project` boots with nothing adopted and nothing current. Together
  they are the "Add a project" hero. Neither touches a live path.
- **Fixture rows read as replayed.** No index or store source speaks for a
  scripted id, so a later `rejoin` would blank the fixture's label back to
  the fallback; marking the row `replayed` keeps it, exactly as a capture's
  own row is kept. They never flip the grouping default on their own, and
  with no client behind them `resume` stays a no-op.
- **The search capture reads the real index.** `search.db` rebuilds from it,
  so `search:acme` shows whatever this machine holds — byte-identical run to
  run, machine-specific across machines. The badge and scope rules it
  exercises are index-independent.

### Owner round 2, surface (2026-09-13)

- **The "Choose folder…" row is a card, not a row.** The Add section's lead
  is the library's `folder_drop_card` (`PaletteSection::lead`), which is not
  a `PaletteItem`, so it draws inside the card under the section title —
  first under "ADD", above the recent workspaces — rather than as a floating
  box above it. The keyboard walks past it; ↩ on an empty Projects palette
  opens the panel. The hero keeps its own card.
- **The panel needed two fixes, found by log.** Clicks never reached any
  palette row — the scrim dismissed on mouse-down, so the release found no
  row — and the panel could open behind an inactive app (`cx.activate(true)`
  first). The scrim now dismisses on click. The three `harness:` lines
  (select, entry, resolution) stay.
- **Folding holds back past five, not past the open session.** Pinned rows
  never count toward the five; the open session appends past the cut rather
  than displacing a newer row; "Other workspaces" never folds.
- **No menu had click-outside handling.** The brief's `.on_dismiss` does not
  exist on `popover_layer` — the view, account, project (+ colour) and
  overflow menus all gained catcher siblings in the same deferred draw, the
  shape the composer's chip pickers already used.
- **The fixture folds.** `fixtures/sidebar/projects.json` holds seven
  `acme-web` sessions so the captures show "Show 2 more", and `s-web-1`
  went idle: a running row's pulse ring animates on wall-clock time and no
  capture containing one can be byte-identical run to run.

## 8a. Owner round 3 (2026-09-13)

- **A project's rows end where the project row ends, at any sidebar width.**
  The indent wrapper between `rows` and the reveal had no width, and
  `collapse` lays its child out as a root, where `width: auto` is fit-content
  rather than stretch — so the whole indented block sized to its content: the
  rows stopped short of the project row at a wide sidebar and spilled past its
  right margin at a narrow one. The library's `sidebar/views` gallery card now
  carries the grouping at 240 px and 520 px so the edge is checkable without
  the app. Note that the shell clamps the sidebar to `SIDEBAR_MAX_WIDTH` =
  420, so 520 is reachable only in the gallery.
- **The current project wears a 2 px accent bar** at the row's left edge, and
  keeps `ink` at semibold even when the group is muted
  (`ProjectGroup::current`). It marks the project of the open session; with no
  session open, the store's `current` — the project the header crumb names and
  the one ⌘N would land in.
- **The hover tray hides what it covers.** The branch and the count fade out
  as the tray fades in, on the one tween, opacity only.
- **Ordering counts adoption (superseded 2026-09-13, owner round 4, O1).**
  `Projects::sorted` used to key on session activity, then on the newest of
  session activity, `last_opened_at` and `added_at`. It now keys on nothing
  but pin and name (D37 as rewritten): adopting several folders in one go
  orders them by name at once, and no activation, creation or turn ever moves
  a group. `last_opened_at` survives only for the `most_recent` boot fallback.
  Scroll-into-view on activation replaces "a new project lands on top" as the
  way a fresh adoption is found.
