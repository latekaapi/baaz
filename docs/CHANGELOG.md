# Harness changelog

## 2026-09-13 — Owner round 2, surface

Eight items from the owner's screenshots, on the `owner-round-2-2026-09-13`
branch. The wire package below landed first on the same branch.

- **P1 — "Choose folder…" does nothing.** Two faults: palette row clicks
  never reached their row (the scrim dismissed on mouse-down, so the release
  found no row — every palette row's click only dismissed), and the folder
  panel could open behind everything (`cx.activate(true)` first). The scrim
  now dismisses on click; three `harness:` log lines (select, entry,
  resolution) stay for the next regression.
- **P2 — The folder card.** The Add section's head is the library's
  `folder_drop_card` (click → the panel, drop → every dropped directory is
  adopted, the first becoming current); the "Choose folder…" row is gone, so
  the keyboard walks past the card, and ↩ on an empty Projects palette opens
  the panel. The hero keeps its two buttons and takes a drop anywhere.
- **P3 — Nesting and five recent per project.** Pinned rows, then the five
  most recent others; the rest fold behind "Show N more" / "Show less"
  (`expanded_groups`, persisted like `closed_groups`); the open session
  always survives the cut. Nine → five plus "4 hidden" is unit-tested.
- **P4 — Any open menu closes on a click outside.** The project menu (and its
  colour submenu), the overflow menu, the view menu and the account menu all
  gained the catchers the chip pickers already had. (The brief's
  `.on_dismiss` does not exist on `popover_layer`; none of the four had any
  outside handling.)
- **P5 — Pinned rows show it.** `summary` already set `.pinned()`; the
  meta-line glyph and the `PinOff` tray button are the library's, the rail is
  unchanged, and a test pins the flag to the row.
- **P6 — The plan label sits beside the name.** Already so: the footer uses
  the library's inline `.plan(..)` and renders two rows, never three.
- **P7 — Room under the last block.** One fixed-height `SP_7` row below the
  final block, counted and hinted like any row. Wheel bench
  `frame.series_us`: before p50=520 p90=732 max=7982, after p50=517 p90=616
  max=6152 — no regression.
- **P8 — Captures and docs.** `projects-sidebar-{dark,light}.png` and
  `projects-palette-dark.png` retaken (fixture: seven `acme-web` sessions, so
  "Show 2 more" shows; `s-web-1` went idle — a running row's pulse ring
  animates on wall-clock and can never be byte-identical), plus
  `round2-pinned-dark.png` (`…;pin`) and `round2-session-in-use-dark.png`.
- **Fix-up — The drop card lives inside the palette.** The `folder_drop_card`
  is the Add section's `.lead(..)` now — first under "ADD", inside the card —
  and the floating box is gone. `projects-palette-dark.png` retaken.
- **Fix-up — A held session is a banner, not a dialog.** Every session-scoped
  rejection from a direct `resume`/`open` raises the reconnect path's lease
  notice on that session's view (read-only, no dialog). Proven live again:
  `round2-session-in-use-dark.png` retaken.

## 2026-09-13 — Owner round 2, wire

The wire package (schema 1.2.1, two errors, the tier probe), also on
`owner-round-2-2026-09-13`.

- **S1 — Schema 1.2.1.** Fixtures re-exported from the CLI and mirrored
  (pending requests now carry full approval and user-input payloads;
  `Session` gains `name`, `title`, `firstUserPrompt`, `branch`, which the
  sidebar cascade reads before the index); a capture of the new shapes with
  its snapshot; round-trips green.
- **S2 — Stale-sidecar retry.** The backfill page waits for the resume
  lease; a `-32603` naming a stale sidecar retries once, `derive_titles`
  treats it as no title.
- **S3 — Session-in-use never downs the wire.** A reconnect is `Ready` on
  the handshake; a session-scoped resume rejection (`-32021`) becomes a
  banner on that view ("This session is open in another window…") with the
  view read-only, and the sidebar, palette and ⌘N keep working. Proven
  live: `docs/images/round2-session-in-use-dark.png`.
- **S4 — The probe reads the 1.2.1 card.** The plan sentence draws before
  the percentages, so a plan without them is "not yet", not an answer
  (verbatim redacted card in the tier tests); one probe at a time across
  windows (`probe.lock`, bounded wait) with a fresh-under-an-hour cache
  reused; "Check again" reads "Checking…" while probing and toasts the
  result; the banner leaves once the plan is known.

## 2026-09-13 — Projects, package 2

The surface over package 1's model (`docs/12-projects.md` §5, §8 for what
bent): project groups with their trays, the header crumb and its menu, the
Projects palette and the folder panel, removal, tinted rail tiles, the hero,
scoped search, scripted verbs with a sidebar fixture, and eight captures —
each run twice and `cmp`'d byte-identical.

- **U1 — Group rows act.** `+` starts a session in that project (making it
  current); on Other it opens the Projects palette. `…` opens the project
  menu; on Other the menu carries only "Add as project…".
- **U2 — The crumb and the menu.** The header reads `mark project ▾ ·
  session` (provider mark before the session label); the crumb opens
  `MenuKind::Project` under it ("Add a project…" with no current project).
  The menu lists the projects checked for its own, New session here, Rename
  (the crumb's dense field; empty reverts to the folder name), a Colour
  submenu of eight named swatches, Pin/Unpin, Reveal in Finder, Remove.
- **U3 — The Projects palette (⌘⇧O).** Section Projects (mark, `~`-root,
  visible session count; pick starts a session there), section Add ("Choose
  folder…" with the ⌘⇧O hint, then recent Muse workspaces minus adopted
  roots, existing dirs only, newest first, at most 12; adopting never starts
  a session). Entry points: the key, File › Add Project…, the nav row, the
  rail cell, `/project`.
- **U4 — Remove from sidebar.** Archive-shaped danger dialog, no Undo;
  sessions keep their rows under Other, current passes to the most recently
  opened remaining adoption.
- **U5 — Rail tint.** Tiles wear their project's label colour; Other keeps
  the default ink.
- **U6 — Empty states.** No project: the "Add a project" hero with its two
  ways in. Otherwise the empty transcript reads "Muse runs in {project
  name}."
- **U7 — Search.** Hits badged with the project name (the folder name for
  Other); narrowed palettes read "Search {project}…".
- **U8 — Verbs, fixture, captures.** `projects`, `project:<path>`,
  `project-menu[:<name>]`, `project-colour:<n>`, `group-by:<date|project>`,
  `remove-project:<name>`, `remove-confirm`; `--sidebar-fixture` merges nine
  scripted rows through `join` (`fixtures/sidebar/projects.json`);
  `--login signed-in` and `--no-project` exist for the hero capture alone.
- **Audit (Fable).** The Projects palette's recent workspaces skip the harness's own
  state directory (the tier probe's throwaway workspace lived there and was offered as a
  project); the palette capture retaken. The library's collapse was found to paint no rows
  on its first frame in a quiet window and fixed there (agentic-ui `07d684e`), which is why
  the grouped sidebar captures hold.
- **U9 — Docs.** `02-app.md` §5 is "Projects and sessions", §9 gains the
  File item; `08-keymap.md` gains ⌘⇧O; `12-projects.md` §8 records the build
  deviations; `05-handoff.md` names worktrees next.

`docs/images/projects-sidebar-dark.png`, `projects-sidebar-light.png`,
`projects-header-menu-dark.png`, `projects-group-menu-dark.png`,
`projects-palette-dark.png`, `projects-rail-dark.png`,
`projects-remove-dark.png`, `projects-hero-dark.png`,
`projects-search-dark.png`.

## 2026-09-13 — Projects, package 1

The model under the Projects design (`docs/12-projects.md` §4): adopted roots with
their own identity, every session carrying its workspace, the sidebar grouping by
project. With one adopted project the window draws exactly as before (the date view
stays the default); with more, sessions group under their project and the rest under
"Other workspaces". The header crumb, project menu, Projects palette, removal and the
steps verbs arrive in package 2. Upgrading drops and rebuilds `search.db`'s FTS
tables (`sessions_fts` is rebuilt from the list; `files_fts` starts empty and fills
again as turns complete).

- **H1 — `projects.rs`.** `projects.json` (version 1, camelCase, atomic write,
  best-effort read): id, canonical root, name, colour 1–8 (least-used wins),
  `pinned`, timestamps, per-project model/effort/approval-mode defaults; resolve
  is stored id, then canonical root equality, never a prefix.
- **H2 — the index reads `workspace_root`.** Selected only when the column
  exists, so an older Muse index still yields titles; `workspaces()` aggregates
  root, session count, newest activity for package 2's palette.
- **H3 — every session, each with its workspace.** `session/list` is unfiltered
  and paged (200/page, cursor to `None`); rows carry the canonicalized workspace
  and the resolved project id, re-resolved after adoptions change; replays read
  theirs from the capture.
- **H4 — grouping by project.** `layout.json` gains `groupBy` (auto: Project
  with ≥2 adoptions or a stray session, else Date), `closedGroups` ("Other
  workspaces" starts closed) and `searchAllProjects`; the view menu toggles the
  grouping and the search scope; group rows carry mark, branch and running dot.
- **H5 — the current project.** Boot adopts per D39; opening a session adopts
  its project; ⌘N starts in the current project with its root, model and mode;
  the title bar names the project.
- **H6 — per-project defaults.** Picking model, effort or approval mode in a
  session stores it on that session's project; the next session there starts
  with it.
- **H7 — the `@` index and skills per root.** File and skill caches are keyed
  by root (8 roots, LRU); the mention picker never ranks another root's files;
  the truncation toast names the project.
- **H8 — `history.json` honours `HARNESS_STATE_DIR`.**
- **H9 — `search.db` learns the workspace.** Both FTS tables gain `workspace
  UNINDEXED` behind a `meta` schema version; queries return it, scoped search
  filters by the current project's root, reveals join the hit's own workspace.

## 2026-09-13 — Owner follow-up: settled cards, one-line menu rows, a palette that holds its field, a rail worth collapsing to

Four faults from the owner's second look, diagnosed on the wire and fixed the same evening
(library `b1850d5` on agentic-ui `owner-followup-2026-09-13`, merged to `main`).

- **Spinners under a finished reply.** The wire ended the turn with the bash call still
  `inProgress` — Muse keeps a live shell session open, and only the next turn's
  `bash_input {terminate}` closed it, thirty minutes later — so the card spun and its
  approval read "Running" under a completed reply. The fold now settles a turn's open
  cards at its terminal (`settle_open_blocks`): running calls become done on a completed
  turn and cancelled on a cancelled or failed one, an approval still "approving" reads as
  allowed. Not optimistic: the terminal is the server's word that no more work runs in
  that turn. The `synthetic-toolgroup` snapshot changed for exactly that approval.
- **Slash menu rows overlapping, janky scroll.** Rows were fixed-height while long skill
  names folded and descriptions wrapped inside them; and the menu scrolled the *hovered*
  row into view every frame, fighting the wheel. Names never wrap, descriptions truncate to
  one line, and only the keyboard selection scrolls the list.
- **Collapsed sidebar empty.** The rail showed running sessions only, as bare dots. It shows
  the open session and the eight most recent visible ones (pinned first) as tiles bearing
  their initial, the state dot in the corner, the title as a tooltip; running ones pulse.
- **Search palette.** The query field floated above the card in its own box, the list ran to
  the window's edge, and file snippets ran past the right edge. The library palette takes
  the harness's editor in its query row (`query_slot`), bounds the list to 320 px and
  scrolls it, and truncates the context column.


## 2026-09-13 — Owner round: fifteen faults from three notes and nine screenshots

Diagnosed first (`docs/diagnosis/owner-round-2026-09-13.md`, every item with its cause and
owner), then fixed in three lanes: Fable directly for the pin, the dialog, the stale cursor
and the scroll; Muse from `docs/briefs/muse-owner-lib.md` (agentic-ui
`owner-round-2026-09-13`) and `docs/briefs/muse-owner-harness.md` (worktree
`../harness-wt-owner`, same branch name), merged and audited here. No model turn from the
harness; the reproductions ran on `session/list`, `session/resume`, `view/page`,
`--replay` and `--bench`.

- **Pin was slow (item 1).** Not the store, not the index: the library row's tray buttons
  let the click bubble to the row's `on_select`, so a pin also re-opened the session
  (loading row, `view/page`). The tray stops propagation now (library `session_row.rs`).
- **Trackpad scroll (item 2).** Measured on a real 808-event capture, wheel phase, release:
  frame p50 4.7 → 1.2 ms, p90 5.4 → 1.9 ms, p99 8.4 → 6.8 ms, max 30.6 → 8.4 ms, frames
  past a 120 Hz budget 14 → 1. Cause: one list item per turn, and gpui lays a visible item
  out whole every frame. The list is one item per **row** now (`transcript::turn_rows` /
  `turn_row`: a block, a bubble, or the silent footer), every row carries a hint until
  measured — first fill, after every history page (the 2026-09-12 hint covered the first
  fill only), and after a width change (gpui drops every hint then) — and a row-count
  change in one turn splices from that turn only. `--bench` unpacks `view/page` results, so
  a `MUSE_CAPTURE` of a real open path benches; `--bench-out` carries the frame series;
  the jump threshold is per one-line row. The 53 deterministic captures are visually
  unchanged (18 differ by sub-pixel anti-aliasing on one icon row; read side by side).
  The trackpad's physics were never the fault: macOS supplies the momentum and gpui
  passes precise deltas through. Still for the owner: the feel on the real trackpad.
- **Two Dismiss buttons (item 4).** A dismiss-only dialog has one button.
- **`-32011 unknown cursor anchor` (item 5).** A cached view topped up from a cursor the
  server forgot (`notFound`) is dropped and the session reopened afresh; no dialog.

The harness package Muse implemented from the brief, merged from `owner-round-2026-09-13`:

- **H1 — the new session's row appears at once.** `session/start` places a local
  row ("New session") before `load_sessions`; the first `turn/started` titles it
  from the prompt the view sent. The merge keeps locals the wire does not list
  yet and drops them once listed.
- **H2 — links open the right thing.** Absolute workspace paths resolve as is;
  folders open in Finder and files in their default app (`open_with_system`).
- **H3 — the Sessions view menu anchors under its button.** The sessions scroll
  handle reports the viewport and offset, so the popover sits under the sliders
  icon, right-aligned, following resizes and scrolls.
- **H4 — the rename field keeps the row's height.** The wrapper is the library's
  fixed 22 px box (`dense_field` un-overridden), so siblings never move.
- **H5 — the empty state is centred.** The suggestion chips sit in the measure,
  centred under the title; `synthetic-empty.jsonl` replays to the empty state.
- **H6 — "Finishing up…" after the reply.** While the running turn's reply is
  complete but the turn is still open (the `reminderChild` tail), the status row
  says so with a "memory reminders" note; `synthetic-finishing.jsonl` pins it.

## 2026-09-12 — The five owner-visible faults (transcript design, scroll, open path, lights/header, reopen)

Diagnosed first (`docs/diagnosis/transcript-pass-2026-09-12.md`), then fixed library-first:
agentic-ui `b0b40bd` on `transcript-2026-09-12` (stacked on `audit-2026-09-12`), harness
`2e71c09` (H1) plus the `wt/scroll` and `wt/open` merges. Briefs in `docs/briefs/muse-*.md`;
Muse implemented, the reviewer audited, reran every gate and committed.

- **Transcript design.** The library cards matched the design and the gallery; the harness
  composition did not. Blocks inside a turn now sit 8 px apart (`.grp2{gap:8px}`), the
  column is bounded to an 880 px measure and centred (composer content included, the docked
  band and its hairline still spanning the pane), and colour/type were verified unchanged.
- **Traffic lights and header.** The window places macOS's own lights with
  `aui::shell::traffic_light_position` and the sidebar header reserves their 72 pt footprint
  (`native_lights`); the header row no longer collapses with the sidebar
  (`header_follows_sidebar(false)`): only the pane below "New session" goes to the rail.
- **Scroll.** gpui's `list` counts an unmeasured row with no hint as 0 px; the harness used a
  96 px overdraw and `reset()` with no hint, so one upward flick from the tail of a backfilled
  session landed on turn 1 and the list never got back. The first fill now hints every row
  at a typical turn height and the overdraw is a viewport. `--bench-scroll wheel` streams
  head-pinned (the tail unmeasured, as after a real open) and dispatches real wheel events:
  before `a_ix=0 … d_ix=64` (stuck at the head), after `a_ix=551 b_ix=590 c_ix=524 d_ix=589`,
  `jumps=0 stalls=0`. Frame time was never the fault (6–7 ms p50 before and after, 0 dropped).
- **Open path.** The click's target highlights the row and titles the header on its own
  frame; the centre swaps at once to the new view (its loading row until page 1 lands);
  `view/page` pages fold one per update with `HistoryReady` after the first; the last eight
  views are parked in an MRU and reopen instantly, topped up by `session/resume { cursor }`
  (a forward `view/page` from the cached head answers `missingAnchor`). `HARNESS_TRACE=1`
  prints the timeline; `--steps open:<id>` scripts a switch. Live, free calls only: a 790-event
  session went from ~990 ms click-to-content to 1–4 ms to its loading row and ~1 s to
  complete; a cached reopen draws the full transcript in ~2 ms.
- **Close and reopen.** ⌘W and the red dot hide the app (`cx.hide()`) after the probe
  cleanup, so the window and the `muse serve` child survive and the Dock icon or ⌘-Tab
  bring the same session back; `on_reopen` rebuilds the window through `open_shell_window`
  if it is ever gone.
- **D25 closed.** `reconnect_after_login` and its `allow(dead_code)` deleted.

Proof: `scripts/captures.sh` (53 deterministic captures) — every session capture changed
once, for H1's named visual changes, and was byte-identical through the scroll and open-path
merges; the five login captures never changed; adapter snapshots unchanged. Bench, debug,
`synthetic-stress-300`: sweep element 7/77/264 → 4/59/89 µs, frame 6/7/10 → 6/7/8 ms,
dropped 8 → 0, idle 8 → 0 (the 8 did not reproduce for any implementor and was not
gated). Still for the owner on screen: the lights' alignment, a real trackpad, ⌘W then
the Dock. Spend: zero model turns from the harness; the runs were four `muse exec`
sessions on the subscription.

## 2026-09-12 — Code review and performance pass (audit packages P0, E, A, B, C1, C2, D1, D2)

A read-only audit first (`docs/audit/00-findings.md`: 85 unique findings, two HIGH), then
eight packages in the order `docs/audit/01-plan.md` fixed: measurement before change,
library before harness, mechanical before structural before performance. Every package was
proved against **byte-identical captures** (47 replay and login captures under
`HARNESS_DETERMINISTIC=1`, unchanged from start to finish) and the new `--bench`.

- **P0** (`ddb9391`): `HARNESS_DETERMINISTIC=1` (one clock, animations at rest, reduce
  motion) makes a capture reproducible to the byte; `--bench` streams a capture through the
  fold at a cadence while sweeping the list and reports element, fold-apply and frame
  percentiles, dropped frames, peak RSS and idle frames.
- **E** (agentic-ui `audit-2026-09-12`): one memo for every per-frame parse — prose
  markdown (−95 % on a hit), syntax runs (−67 %), ANSI (−44 %), selected text (−92 %); ids,
  faces and caret measures no longer rebuilt per frame; a finished browser card asks for no
  frames (120 → 0 per 2 s).
- **A** (`60a029b`): the two HIGHs — `reindex` remaps cached slots after a turn removal
  (updates no longer land on the wrong turn) and `dispatch` parks server requests during a
  `view/gap` backfill — plus 21 mechanical fixes: visible late responses, counted decode
  failures, typed `modelRouteUnserved`, unknown enum strings retained, one
  `approval_state`, one `harness_log!`, named index failures, wider attachment sniffing.
- **B** (`952f814`): the phase-era wire probes and TUI drivers and the billed
  `harness-probe` binary removed; dead right-pane state, dead command availability and a
  discarded retry-row format deleted; scripting-only verbs marked in one place.
- **C1** (`8cfea89`): one `wire_call` helper at 37 sites; `steps.rs` with a test that the
  verb table and its docs agree; `login.rs`. `app.rs` 4009 → 3267 lines.
- **C2** (`f281024`): both entities split along their section seams (`app.rs` → 1402,
  `session.rs` → 1178); a frame decides in `on_frame` before it draws; the transcript block
  is a dispatch over per-card functions; `schema.rs` split by surface with one inventory
  test; batch hides settle once; capture flags are a token, not process globals; library
  card payloads borrowed and footer strings caller-owned.
- **D1** (`dafb40d`): per-turn slot index (a shifted or removed turn touches one bucket);
  events deserialised from a borrowed `Value`; render cache shares turns by `Rc` and
  re-clones only the changed turn — element construction p90 174 → 79 µs, p99 242 → 99 µs
  on the 300-turn stream; per-session maps pruned or capped; folds keep an MRU of eight;
  gap backfill bounded and an aborted fill says so; `client/protocolError` is a card.
- **D2** (`a0c140a`): pending approval recorded at cache sync; sidebar list, grouping,
  title and search status cached on an epoch (500 rows: ~240 → ~60 µs per frame); palette
  rows run by id; composer emptiness flag; image decode off the UI thread behind a
  placeholder; screenshots encode off the UI thread; `--bench-open-turn`.

- **E2** (agentic-ui `820e258`): `sidebar_view` borrows its grouping (`Rc<Grouping>`), so
  the last per-frame sidebar cost at 500 rows goes from ~40 µs to nanoseconds; the library's
  idle-frame suite now holds every streaming/working/settled clock gate.

Numbers, `--bench`, debug build, before the pass → after (release in brackets):

| capture | element p50/p90/p99 µs | fold-apply p50 µs | frame p50 ms | peak RSS MB | idle frames / 2 s |
|---|---|---|---|---|---|
| `synthetic-stress-300` | 5/154/239 → 6/85/123 (6/82/246) | 11 → 10 | 7 → 7 | 107 → 106 (102) | 0 → 0 |
| `transcript-echo` | 9/36/65 → 12/19/32 (11/17/27) | 4 → 5 | 6 → 6 | 98 → 98 (93) | 0 → 0 |

Frame time is gpui layout and paint and did not move; what moved is everything the
harness and the library do before handing gpui the tree. Still open, recorded in
`docs/audit/00-findings.md` and the package reports: the library's terminal
and TUI cursors blink without a focus gate; the 1 Hz caret still samples at frame rate.

Spend: zero model turns for the harness packages; the audit and P0 ran on Muse (the
owner's API key), the rest on Claude subagents while the owner was away.

## 2026-09-11 — Sign-in over the wire: Meta account and API key

What was wrong (one paragraph, see `docs/diagnosis/login.md`): the harness
parsed the wrong stream of the wrong program. `auth.rs` spawned `muse login`
with stdout to `/dev/null` and parsed **stderr** for the launcher's
device-code wording, but the real binary prints the device flow on **stdout**
with different wording — so no event ever reached the screen, the card sat on
"Starting sign-in…" until the code expired, and two smaller faults hid behind
it: `model/list` answers from the provider catalog while logged out (so the
"live" half of the boot probe never decided anything), and there was no
API-key path at all.

What changed: sign-in moved onto the wire. `conn.rs` opts in to
`experimentalApi` at `initialize`; `muse-client` mirrors the eight `Account*`
types and adds `account_read`, `account_login_start`, `account_login_cancel`
and `account_logout` (each documented with its `-32601` /
`experimentalRequired` gating). `auth.rs` lost the stderr parser, the `muse
login` / `muse logout` children and the probe halves: the wire owns sign-in
and `auth.json` is read-only, only for the two display strings when the
wire's `label` is absent. `app.rs` probes with `account/read` (`loggedOut` →
login screen, anything else → signed in), runs the two-method screen from the
library (device code with auto-open browser, masked API-key field that is
cleared when the submit call returns), folds `account/loginCompleted` and
`account/changed` before the session view sees them, enters the app with no
reconnect, and signs out with `account/logout` (an `envKey` lane survives it
and says so). New: `--login <state>` for `--no-connect` captures,
`--login-steps <a;b;c>` for scripted sign-in, `fixtures/msp/
transcript-account.jsonl` (device flow started and cancelled, secrets
sanitized), the experimental schema bundles beside the stable ones, and
`docs/images/login-*.png` in all six states × both themes.

Spend: zero. No live test, no probe script, no `--send`, no `send:` /
`steer:` step and no `turn/start` ran anywhere in this task — only `muse
schema`, `muse serve` driven through `initialize` / `account/*`, `--replay`,
`--no-connect`, and the offline gates. The log count reads 139 accepted
turns in total (`grep -c runtime.user_intent.accepted
~/.local/share/muse/sessions/*/*/*/*/session.jsonl`), all predating or
outside this task's turn-free wire traffic.

Fix-ups after the owner-side audit (same day): the Device state's hint moved
out of the three-button action row in the library (`login-methods` `2e56405`);
`--login-steps` now sets the capture's `await_steps` and raises the
steps-running flag so a headless sign-in is captured after it finishes;
`--tier` wins over the lane-derived tier (it is how a scripted capture gets
past the pay-as-you-go guard); the footer avatar for the key lanes is "A", not
the wire label's initial; `account/loginCompleted` is logged on stderr as
outcome and display message (the server's typed vocabulary, never a secret).

Verified live on 2026-09-11/12 (`docs/diagnosis/login.md` §7): the API-key
lane end to end from a signed-out machine, and a real turn on it. The
Meta-account lane reached the device screen and opened the browser three
times; each code expired unapproved (the server's lifetime is 611 s), so the
approval itself is still the owner's to do. Spend for the tests: one billed
turn on the API key ("OK", 24.2k tokens, `muse-spark-1.3-contributor`); the
implementation ran on the owner's API key through `muse exec`.

## 2026-09-10 — Fix-up 2 on `wf-improvements` (owner re-read of the retakes)

Four faults, all `--replay` proofs, no live turn spent.

- Sidebar description (`sidebar.rs`, `index.rs`, `app.rs`): the row still
  read the same words twice ("Run the shell command…" over "Run the shell
  command `ls` in the"). The old `label_from_prompt` only caught the index
  fallthrough, but this label is Muse's index `title` — Muse writes whole
  first prompts into it (seen on a host session whose title and first prompt
  are the same 150-char prompt). Rule now, on the text: `last_summary` wins;
  otherwise the first prompt shows only when the label is a user-given name
  (`/name` or the index `session_name`) or a Muse title that is not a
  prefix/elision of that prompt (lowercased, whitespace-collapsed, first 40
  chars, trailing elision trimmed); otherwise no second line. `join` and
  `rejoin` share it; the superseded `IndexEntry::label_from_prompt` is gone.
  Covered by the rewritten `describe` test plus a direct `echoes_prompt`
  predicate test (equal, elided, case/space-folded, foreign, empty).
- Rename field (`app.rs`): the fixed 22 px wrapper with full
  `overflow_hidden` cropped the glyphs at the top in the row and the header.
  The wrapper is a flex row with `items_center` now, its height whatever the
  editor's own line-height makes it (`h_auto`), clipping horizontal only —
  and the rename state runs with soft wrap off (`set_soft_wrap(false)` at
  creation), because `whitespace_nowrap` never reaches the editor's layout
  and the narrow row wrapped the 80-char label onto two lines. Same element
  serves both places; rows keep their height.
- Search palette (`app.rs`): the committed frame sat at ~50 % opacity with
  the input floating off the panel. Not reproduced on this tree — three
  consecutive retakes render the card at full opacity (38,41,49 vs the dim
  frame's 14,15,18), the palette structure is byte-identical before/after
  except the matched-range highlights (highlight-only, cannot dim), and the
  library palette file is untouched since 2026-09-09 — so the dim frame was
  most likely caught mid-enter-animation by a capture that raced the steps.
  Hardened anyway: scripted screenshots draw the card `.at_rest()` (the
  library's documented static-composition switch; live opens keep the rise),
  and the palette column is `items_center` so the search field — the sidebar
  list's box with its 8 px side margins — lands exactly on the card (both
  span 440–1000 px on the proof). Opaque, one surface, pixel-verified.
- Turn actions: the Pin opt-out landed in the library (`33b8d54`,
  `AssistantTurn::actions(..)`), so assistant turns keep Copy/Retry/Fork and
  Pin is hidden in both the hover toolbar and the bottom row. The Pin match
  arm stays for exhaustiveness (unreachable; still toasts). `docs/02-app.md`
  updated; `improve-integrated-markdown-dark.png` retaken.
- Screenshots (all `--replay`, 15 s delay):
  `docs/images/improve-shell-open-{dark,light}.png` (no second line),
  `improve-integrated-{dark,light}.png`,
  `improve-shell-rename-{dark,light}.png` (single full-height line, row and
  header), `improve-integrated-search-{dark,light}.png` (opaque, one
  surface), `improve-integrated-markdown-dark.png` (no Pin). The
  `--steps resume` comparison frame was taken to `/tmp` (opaque, one
  surface) and not committed.

## 2026-09-10 — Fix-up pass on `wf-improvements` (owner-side audit)

Seven faults from the owner's screenshots, all `--replay`/`--no-connect`
proofs, no live turn spent.

- Transcript gutter and alignment (`session.rs`): the virtualised list's
  rows were full-bleed against the sidebar divider and the right edge, and
  `ListAlignment::Bottom` left a void above short transcripts. The list
  wrapper carries the pre-virtualised container's own gutters again
  (`TRANSCRIPT_PAD_TOP`/`TRANSCRIPT_PAD_X` px, `SP_4` below; per-row `SP_5`
  is the inter-turn gap), and the list runs `ListAlignment::Top` with the
  existing follow-the-tail logic untouched (`follow` +
  `is_scrolled_to_end`/`scroll_to_end`). No content max-width was restored
  because none existed: neither the old container nor the composer column
  constrains width in code, so the turns line up with the status/banner rows
  through the shared `TRANSCRIPT_PAD_X` step.
- Header title (`app.rs`): the title flexes inside the header cell
  (`flex_1`, `min_w(0)`, `overflow_hidden`, `truncate`) with a flex-none
  provider mark, so a whole first prompt as the derived label elides to one
  line and can never push the overflow button out. No fixed max width: no
  width token exists in `aui-tokens`, so the flex leftover is the
  constraint, which also holds on narrow windows.
- Sidebar description (`sidebar.rs`): the second line repeated the first
  prompt the label already showed. Now `last_summary` wins when present;
  otherwise the first prompt shows only when the row's label is not derived
  from it (user name or Muse title, via `IndexEntry::label_from_prompt`);
  otherwise the row carries the turns meta alone. The derived title is no
  longer a description fallback anywhere (`join` and `rejoin` share the
  rule). Covered by the rewritten `describe` unit test.
- Rename field (`app.rs`): the dense field wrapped long names onto a
  clipped second line; it is single-line now (`whitespace_nowrap`,
  `overflow_x_hidden` on the field, `overflow_hidden` on the 22 px
  wrapper) and fills its slot to the trailing meta. Renaming the open
  session also swaps the header title for the same field (Task A's design:
  "Rename swaps the title for a dense inline field"), through the same
  `ConfirmRename`/Escape path.
- Search palette (`search.rs`, `app.rs`): rows showed the raw index
  envelope (session-id fragments, `^_` separators, `valid`, workspace
  paths). The snippet is cut from the new `clean_search_text` — uuid/short
  id, `valid`/`meta`, bare absolute paths and model ids stripped, whitespace
  collapsed, ~90 chars around the first match — while FTS still matches the
  raw body. Primary text stays the sidebar label; files keep path plus the
  owning session label. The query's first hit in each label is emphasised
  through the row's own `matched` ranges. Caveat: the snippet itself stays
  the library's muted mono context — the palette row offers no
  proportional-font or snippet-highlight shape, and the library is untouched.
  The dead sidebar quick-filter is gone entirely (field, state, matching
  helpers, empty-state branch): ⌘⇧F and the search icon open only the
  palette, so the two can never be open together.
- Turn actions: Pin stays. The library draws it unconditionally in both the
  hover toolbar and the bottom row (`turns.rs`, no opt-out builder), and the
  library branch is frozen — hiding it needs a library-side change. The
  toast ("Pin lives on sidebar sessions") still answers the press.
- Text selection (Task C item 8b): the library now forwards
  `selection(..)`/`on_selection_change(..)` through both turns and
  `turn_selected_text(..)`, and the harness half is fully wired: every turn
  gets its own held cell, intents carry the turn's markdown source back, ⌘C
  in the transcript context copies `turn_selected_text` of the held turn
  (never from the composer or a card field), Escape and plain clicks
  elsewhere clear it. Per turn, not one shared cell: the library scopes cell
  keys (`p0`, …) to the markdown view, so a shared selection would light up
  every turn at once. New `--steps select-text:<turn>:<from>-<to>` verb
  holds a scripted selection for screenshots. Residual library limit: an
  assistant turn with several text blocks shares one turn id, so a selection
  tints each block's same-key cell — ⌘C still copies exactly what was
  dragged, because the source travels with the intent.
- Covered by three new offline unit tests in `search.rs` (envelope
  stripping, survivor shapes, the ~90-char window) plus the rewritten
  `describe` rule test in `sidebar.rs`; no test opens a real session.
- Screenshots (all `--replay`, 15 s delay):
  `docs/images/improve-integrated-{dark,light}.png` (transcript-real: gutter,
  top start, elided header), `improve-integrated-markdown-dark.png`,
  `improve-transcript-stress-tail-dark.png` (300-turn tail still sticks),
  `improve-shell-{open,rename}-{dark,light}.png` (description rule,
  single-line rename in row and header),
  `improve-integrated-search-{dark,light}.png` (clean rows),
  `improve-transcript-selection-{dark,light}.png` (scripted hold over the
  user bubble).

## 2026-09-10 — Improvements (A) — shell: header and sidebar

The header shows the active session's label ("Harness" with nothing open) with
the provider mark and an overflow "…" menu (Rename, Fork, Archive); the shell
paints no traffic lights of its own, so only the window's native set remains.
There is no right-pane toggle and no right-header close button — the right
slot stays empty with `right_open = false` (the library always paints those
two icons, so the shell uses a plain header cell with the same title
construction instead of `centre_header`/`right_header`). The shell's drag
region wraps the header row: press-drag moves the window, double-click zooms,
and the buttons keep their clicks. Collapsed, the column is the library rail
(`flat(true)`: New and Search cells, a separator, one dot per running session,
the account avatar) at 48 px, and the centre title stands 14 px off so it
clears the native lights.

Above the Sessions caption sit New session (Plus, ⌘N) and Automations (Zap,
muted "Soon" tag — a placeholder that answers with a toast). The caption's
sliders icon opens the view menu: Show/Hide empty, Show/Hide hidden (legacy
`/hide` rows), Clear empty, Show/Hide archived. The footer is the library's
account row again — avatar, name, email, plan row, the tier probe's weekly
fraction as the usage meter (omitted while unknown, chevron always shown) —
and opens the account menu whose only row is Sign out.

`SessionMeta` gains `pinned`, `archived` and `last_summary` (all camelCase,
old files still read); row actions are Pin, Rename and Archive, with a Pinned
group first and a muted Archived tag on shown archives. Archive asks first
through a danger dialog and offers an eight-second Undo on a toast, sharing
one undo stack with hide/Clear-empty; archiving the open session opens the
newest remaining visible session (or the empty state), and archives stay out
of Clear-empty. Every row shows one muted second line — `last_summary`, else
the first prompt, else the derived title — written free from the in-memory
fold on `turn/completed` (first line, ≤ 120 chars). Rename uses the library's
`dense_field` in its 22 px wrapper, so the editing row keeps 30 px. Row
gutters measure 8 px left / 8 px right (±1 for radius) on the replay capture,
so no harness container change was needed beyond the library's row-idiom fix.
New `--steps` verbs: `sidebar`, `overflow`, `view-menu`, `account`, `pin`,
`archive`, `archive-confirm`, `show-archived`. No key bindings changed, so
`docs/08-keymap.md` is untouched. `docs/02-app.md` §4–§5 describe the header,
rail, nav rows, view menu, pin/archive and footer.

- Screenshots (all `--replay fixtures/msp/transcript-real.jsonl`, 15 s
  delay): `docs/images/improve-shell-{open,collapsed,overflow,viewmenu,
  account,rename,dialog,archived,pinned}-{dark,light}.png`.
- Covered by five new offline unit tests (`describe` precedence and cap,
  pin/archive/summary store retention and camelCase round-trip, weekly
  fraction); the description line itself is screenshot-proven only in
  structure — replay rows carry no summary, and writing one needs a live
  completed turn.
- Not done: rail dots cover running sessions only (the harness tracks no
  waiting state); header menus are click + Escape driven with no arrow-key
  selection; the view menu floats at a fixed offset under the caption rather
  than anchored to the sliders icon.

## 2026-09-10 — Improvements (G) — platform: menu bar, ⌘W/⌘Q, app bundle and icon

The app finally has a native menu bar (`crate::app::set_menus`, called from
`main.rs` after `bind_keys` because macOS reads each item's shortcut from the
keymap): Harness (About with the version, Services, Quit ⌘Q), File (New
Session ⌘N, Close Window ⌘W), Edit (Undo/Redo/Cut/Copy/Paste/Select All, each
carrying its `OsAction` for OS recognition and deliberately *without* global
bindings — the focused field owns those keys), View (sidebar, palette ⌘K,
search ⌘⇧F, theme), Window (Minimize ⌘M, Zoom), Help (Harness Documentation,
which reveals `docs/` in Finder). Close and Quit are global listeners (not
window handlers, which validate dimmed on the login screen) that run the same
`tier::cleanup_probes` as the window should-close and app-quit hooks, so every
exit path is one function; the deferred `remove_window` works around menu
dispatch holding the window out of `App.windows` mid-dispatch. Verified live
on `--no-connect`: the app log shows `CloseWindow` then `QuitApp (global)`,
the window closed while the process stayed up after ⌘W, and the process was
gone with no `muse` child left after ⌘Q.

`scripts/bundle.sh` assembles `target/bundle/Harness.app` from the release
binary, a generated `Info.plist` (`dev.harness.app`, `LSMinimumSystemVersion`
14.0) and `Harness.icns` converted from the checked-in placeholder
`assets/icon-1024.png` — a flat dark rounded tile with a white H, drawn by the
checked-in stdlib-only `assets/make-icon.py` — ad-hoc signed and launched once
with `open`. Signing/notarisation are out of scope.

- Screenshots: `docs/images/improve-platform-menubar.png` (live menu bar),
  `docs/images/improve-platform-app-dark.png` /
  `docs/images/improve-platform-app-light.png` (replay in both themes),
  `docs/images/improve-platform-dock.png` (the H tile running in the Dock).
- Covered by one offline unit test (`find_docs_dir`: cwd wins, then the
  executable's ancestors, then `None`); no test opens a real session.

## 2026-09-10 — Improvements (F) — resizable sidebar

The sidebar divider is now a drag target. A 6 px transparent strip
(`aui::shell::resize_handle`) sits over the sidebar/centre divider with the
horizontal-resize cursor; the press arms the drag (`sidebar_width`,
`resizing`, `grab_x`, `start_w` on `Harness`), every move sets
`clamp(start_w + dx, 180, 420)` through the library tokens, and a
full-window `drag_capture_overlay` owns moves and the release while the drag
is in flight, so outrunning the strip never stalls it. `AppShell::resizing`
skips the layout spring mid-drag so the divider tracks the pointer, and the
spring re-arms on release for the settle. The drag clears on mouse-up
anywhere and when the window loses focus mid-drag.

The settled width persists globally (not per workspace) in
`~/Library/Application Support/harness/layout.json` via the same
atomic-write/best-effort-read store pattern as the sessions file
(`crates/harness/src/layout.rs`), restored at boot and clamped on the way
in. A double-click on the handle resets to the 252 px default: the handle
reports positions only, never the click count, so two taps with no travel
inside 500 ms are the reset signal. Scripted as `--steps sidebar-width:<px>`
(clamped, settled, persisted like a released drag).

- Screenshots: `docs/images/improve-resize-{min,default,max}-{dark,light}.png`
  are `--replay fixtures/msp/transcript-real.jsonl` at 180/252/420 px, taken
  with `--screenshot --screenshot-delay 15000` so they cost nothing.
- Covered by four offline unit tests on the drag math and the store default
  (`layout::drag_width`, `layout::sidebar_width`); no test opens a real
  session. Documented in `docs/02-app.md` §5.

## 2026-09-10 — Improvements (E) — full-text search palette and local store

The sidebar's search icon was inert and Cmd+Shift+F opened a label filter;
full-text search of sessions and created files did not exist (header item 4,
overall item 2). New `crates/harness/src/search.rs` owns a harness-side
`search.db` (sqlite via the existing `rusqlite` bundled build — FTS5 verified
with a `USING fts5` smoke query at open, no new dependency): `sessions_fts`
over label/title/first-prompt plus Muse's `search_text` transcript column,
rebuilt off the UI thread at boot and after each index refresh, and
`files_fts` over created files, recorded off the UI thread when a turn
completes. "Files we created" means write/edit tool targets, relativized
against the session workspace with escapes rejected; reads, searches,
shells and unknown tools are never recorded.

`PaletteKind::Search` lists both halves — Sessions (label plus a one-line
`snippet()` around the match) and Files (path plus the owning session's
label). Enter on a session resumes it; on a file reveals it in Finder (a
toast when it is gone). Opened from the sidebar search icon (`.on_search`),
Cmd+Shift+F (retargeted; the empty query lists recent sessions plus recent
files, which is what the old filter did) and `/search`. Queries run
off-thread with a query epoch, latest wins; punctuation can never error the
query. Scripted as `--steps search:<query>`.

Findings P3/P4 in the same pass: `history::write_all` goes through
`store::write_atomic`, and `history::read`/`append` run off the UI thread;
`files::walk` lowercases once at walk time and the `@` menu ranks on a
background task with epoch/latest-wins instead of scanning 5 000 paths on
the UI thread per keystroke. Covered by unit tests over ranking, snippets
and the recorder's verb detection; full detail in `docs/12-search.md`.

- Screenshots: `docs/images/improve-search-{dark,light}.png` replay
  `fixtures/msp/transcript-real.jsonl` with `--steps search:harness`, a
  query hitting both sections (Files seeded from a scratch workspace).

## 2026-09-10 — Task D — composer files, thumbnails, plus menu

The composer attaches more than images now. The `+` menu holds three rows —
**Attach file or photo** (⌘U, also bound in the composer's context),
**@ Mention file** and **/ Slash commands** — where it used to hold one
"Attach image" row. Mention and commands type their sigil into the draft
through `set_draft`, so the caret and its popover open exactly as if the
person had typed them; attach opens the same system picker as before, which
already accepted any file.

Which chip a picked or dropped path becomes is decided by its extension.
Image extensions still become image chips and `TurnInputPart::Image` parts,
and each one now draws a 64 px thumbnail (`images::THUMB_LONG_EDGE`),
decoded and downscaled once at attach time while the full-resolution bytes
still go on the wire. Everything else goes through the new
`crates/harness/src/attachments.rs`: text-like types are read as text, PDF
arrives through its text layer (`pdf-extract`), xlsx/xls sheet by sheet as
CSV-ish text under `## <sheet>` headers (`calamine`), docx from
`word/document.xml` with its tags stripped (`zip`) — all pure Rust, so
`cargo tree -d` still shows one `gpui-pre` and one `gpui-kit`. Anything else
is refused with the banner reason, because MSP's `TurnInputPart` is a closed
enum and a file's bytes have nowhere else to go. Each file is capped at
64 KB of text with a `[file truncated to 64 KB]` note, eight files per turn;
`parts()` emits one `--- file: <name> ---` text part per file ahead of the
prompt text, and each file chip wears a muted `KIND · size` detail.
`--steps file:<path>` attaches from a command line, beside `image:<path>`.

Enter sends through the library's `submit_on_enter` (`composer_state_rows`
sets it; verified present, no harness binding change needed): plain Enter
submits with no newline while Shift+Enter still inserts one. Documented in
`docs/03-composer.md` §8 and `docs/08-keymap.md` (the ⌘U row).

- Screenshots: `docs/images/improve-composer-files-dark.png` shows an image
  thumbnail chip beside md, pdf and xlsx file chips;
  `docs/images/improve-composer-plus-dark.png` shows the three-row `+` menu.
  Both are `--replay` of `fixtures/msp/transcript-echo.jsonl`, so they cost
  nothing.
- Covered by eight offline unit tests in `attachments.rs` (md text, the
  64 KB truncation note, refused extensions, extensionless sniffing,
  spreadsheet/pdf/docx garbage, an in-memory docx and xlsx, empty files);
  no test opens a real session.

## 2026-09-10 — Improvements (C) — transcript rendering

The transcript list is virtualized: `render_transcript` renders a gpui `list()`
with a persistent bottom-aligned `ListState`, one item per turn, instead of
building every cell every frame. Fold changes splice the affected range only
(pure appends splice just the new tail), and only visible rows are laid out per
frame, so per-frame cost stays bounded as the transcript grows: with
`HARNESS_FRAME_STATS=1` driving 240 frames, `synthetic-stress-300.jsonl`
(~300 turns) reports p50 11µs / p90 16µs and the few-turn `transcript-echo`
reports p50 13–14µs — flat across turn count. (No before-numbers exist: the old
code had no stats hook. The hook measures harness element construction; row
layout/paint inside gpui and the library is not instrumented.)

`apply` notifies only when the fold changed or view state changed (unchanged
streaming deltas earn no frame), the turn ticker runs at 1 Hz and notifies only
when the displayed second changes, and unchanged turns are never re-parsed (the
library memoises markdown via `parsed_markdown`).

A session switch never flashes the empty state: `Harness::open` keeps the old
view rendered until the new session's first backfill batch applies (marked by
`SessionEvent::HistoryReady`), then swaps; with no old view a neutral loading
row stands in, and a failed switch keeps the old view with the error banner. A
mid-switch frame needs a live session, so the swap itself is verified by code,
not a screenshot — the switch screenshots show the Resume palette over a live
transcript. `synthetic-stress-300.jsonl` (~300 turns, built by the checked-in
`fixtures/msp/make-stress-300.py`) and the hand-written `synthetic-markdown`
(headings, table, fence, links) and `synthetic-toolgroup` captures are replayed
by the existing fold snapshot test, which covers every capture in the
directory.

Turns carry an in-flow action row under the prose (`actions_bottom`) for both
roles. Assistant: Copy writes the turn's text to the clipboard, Retry resends
the user input behind the turn, Fork opens the turn picker, and Pin — which the
row always draws but a turn cannot honour — says it lives on sidebar sessions.
User: Copy, Edit (text into the composer draft), Resend. Wire actions are
live-only: in a replayed capture they answer with a toast. Markdown links click
through: URLs open in the browser, workspace paths reveal in Finder (escapes
above the workspace are rejected with a toast, missing paths toast).
`Block::ToolGroup` renders through the library `tool_group` with open state in
`Folds` keyed stably — but today's fold never emits a group (grouping is Task
B), so the group card path is wired, not yet live; the toolgroup screenshots
show the three sibling cards the fold currently produces.

Text selection is half-landed (8b): `SessionView` holds one `TextSelection`,
⌘C in the transcript context copies it and Escape clears it — but the
library's `UserTurn`/`AssistantTurn` expose no `.selection()` /
`.on_selection_change()` (only the lower-level `markdown()` does), so turns
cannot display or report a selection and no dragged-selection screenshot
exists. The harness half waits on the library forwarding selection through the
turn components. Top inset raised to the horizontal-gutter step so the first
turn clears the header. `--steps top`, `end`, `mid`, `bench` and
`expand-groups` drive screenshots.

- Screenshots: `docs/images/improve-transcript-stress-mid-{dark,light}.png`
  (mid-scroll), `improve-transcript-stress-tail-{dark,light}.png` (tail),
  `improve-transcript-markdown-{dark,light}.png`,
  `improve-transcript-toolgroup-{dark,light}.png`,
  `improve-transcript-switch-{dark,light}.png` (Resume palette over a live
  transcript; the no-flash swap itself is code-verified).
- Covered by one offline unit test (`tail_slack_stays_put` keeps the 48 px
  tail-follow slack named); no test opens a real session.
## 2026-09-10 — Improvements (B) — adapter fold: structured tool results, tool groups, reminderChild, reasoning

Shell tool cards showed the raw JSON envelope (`{"chunk_id": …, "command": …,
"description": …, "exit_code": …, …}`) and the todo tool showed
`{"todos":[…]}` / `{"ok":true,…}` as code. The fold now presents both, and
groups consecutive tool calls the way the owner asked (transcript report
§C7–§C9). Shapes were learned read-only from the newest session logs under
`~/.local/share/muse/sessions/2026/09/` and rebuilt as sanitised synthetic
fixtures; the library branch already carries `Block::ToolGroup` and is not
touched.

- A shell result serialised as a JSON envelope folds into the shell body:
  the command is the title (one line, elided past 120 characters), the
  output text is the body, the status comes from the exit code, and the
  "N more lines" fold still applies. The envelope's `description` has no
  home in the library card and is dropped (documented workaround in
  `docs/01-transport.md`); an empty command falls back to it as the title.
- A todo tool call (`args` with a `todos` array) folds into the session's
  todo card; its `{"ok":…}` result is never shown. File reads keep the
  line-count body. Any other JSON object/array result folds pretty-printed
  with its args as parameter pairs. D6 log-sequence ordering is unchanged.
- Consecutive `ToolCall`s in one assistant turn fold into one
  `Block::ToolGroup` ("Ran 3 commands", "Read 4 files", else "N tool
  calls"), incrementally while streaming, with stable group/turn keys.
  Approvals, questions, errors, plans, todos, thinking and approval-gated
  calls each break the run.
- `reminderChild` renders as nothing (`workflow` stays generic); `reasoning`
  falls back to its raw `text` when `summary` is empty. Incidental fix: a
  tool call's own `exitCode` now reaches the shell body (it used to fold to
  `null`).
- Covered by four new synthetic fixtures and seven new fold tests; snapshots
  regenerated and read (`transcript-phase3` groups its three consecutive
  calls, `synthetic-readoutput` gains the exit code).
- Screenshots: `docs/images/improve-fold-toolgroup-dark.png` and
  `docs/images/improve-fold-toolgroup-light.png` replay the tool-group
  capture in both themes, taken with `--replay` so they cost nothing.

## 2026-09-09 — Improvements (E) — fork picker

`/fork` used to fork the newest completed turn with no say in the matter; the
TUI picks from a list, and now so does the harness. Typed bare, or picked from
the `/` menu, `/fork` opens the turn picker over the command palette's
`PaletteKind::Fork`: the session's completed assistant turns, newest first,
each row the first line of the user prompt that started the turn plus the
turn's wall-clock time in footer words. Picking a row forks that turn through
the same `session/fork` call as before — the new session still opens only when
the server's resume envelope arrives, so nothing is optimistic. Typed with a
number (`/fork 2`), it skips the picker and forks the nth newest completed
turn directly (`1` is the newest); a number with no turn behind it is a banner,
never a fork of whatever the server thinks is newest. The picker rows come
from one `SessionView::fork_turns` helper that the direct path reuses, so the
picker and `/fork <n>` can never disagree about what "newest" means. Scripted
as `--steps fork-picker`, beside the window's other palette verbs.

- Screenshot: `docs/images/improve-fork-picker-dark.png` shows the open picker
  over a replayed real capture (`fixtures/msp/transcript-real.jsonl`), taken
  with `--replay` so it cost nothing.
- Covered by three offline unit tests (the `/fork [n]` parse, the row label,
  the row time); no test opens a real session.

## 2026-09-09 — Improvements (D) — silent reasoning footer

Improvement candidate 3 in `docs/09-handoff-improvements.md` §8: a turn can
bill reasoning tokens and emit no `reasoning` item, and the footer showed the
count with nothing saying the thinking happened off-screen. A finished
assistant turn with `reasoning_tokens > 0` and no `Block::Thinking` now gets
the same footer line with the reasoning cell reading
`419 reasoning, thought silently` — one line, in the footer's own style
(mono, `FS_11`, `ink_4`). A first cut drew it as a second row under the
footer, repeating the count and doubling the height; the fix folds the note
into the footer's own cell. Turns with a visible thinking card keep the
plain `419 reasoning` cell.

The decision is the pure `transcript::silent_reasoning` in
`crates/harness/src/transcript.rs` (assistant turn, count above zero, no
thinking card), covered by six unit tests; the fold already accumulated the
count (`TurnUsage::reasoning` → `TurnMeta::reasoning_tokens`), so no snapshot
changed. `fixtures/msp/transcript-real.jsonl` is the real capture that
exercises it: 419 billed reasoning tokens, no `reasoning` item. Screenshots
`docs/images/improve-silent-reasoning-{dark,light}.png` are `--replay` of that
capture. Footer documented in `docs/02-app.md` §6.

## 2026-09-09 — Task C — "Show full output" for truncated tool output

muse 1.1.1 keeps the full bytes of a truncated tool/user-shell output under an
`outputRef`, fetchable with `item/readOutput` (`docs/10-msp-1.1.1-diff.md`).
The fold now records that handle: a `toolCall`/`userShell` item completing
with `truncated: true` and an `outputRef` is kept in a per-session
`itemId → OutputRef` map on the fold (`MuseFold::stored_output`), keyed by the
id its `ToolCall` block renders as. The map is not part of the replay
snapshot, so no existing snapshot changed.

The affordance lives in the existing card's own action slot — no library
change. The tool card's fold row already emits `ToolCardIntent::Unfold`, so on
a card whose item truncated with an `outputRef`, that intent is "Show full
output": the app pages `item/readOutput` on a background task (02-app §3),
concatenates `utf8` pages (decoding `base64` binary pages first) up to a 2 MiB
cap, and replaces the card's body on the server's result (D4 — nothing is
optimistic). Past the cap the body ends with "…truncated at 2 MiB". A second
press mid-fetch is ignored; a failed fetch reports its banner and keeps the
truncated body; a replayed capture refuses the fetch read-only like every
other command. The row's label stays the library's ("N more lines") — the
library owns the card's text.

No capture carried an `outputRef`, so `fixtures/msp/synthetic-readoutput.jsonl`
is new: every `<-- ` line is hand-written to `msp.d.ts` (a completed `bash`
tool call with ten visible lines, `truncated: true` and an available
`outputRef`); only the item envelope mirrors the real bash item in
`transcript-real.jsonl`. It folds to a checked-in replay snapshot, and
`docs/images/improve-full-output-dark.png` replays it. Covered by a fold
integration test (`stored_output` keeps `out-syn-1`) and unit tests for page
concat, base64 decode, and the empty case in `full_output.rs`.

Compile note: the shared `agentic-ui` checkout already carries sibling Task A's
`effort-max` branch (`ReasoningEffort::Max`), so two exhaustive matches gained
one-line `Max` arms (`effort_wire`, `effort_detail`) to keep this branch
building. The picker tiers themselves are Task A's change, not this one's.

## 2026-09-09 — window-close probe kill (task B)

The tier-probe-leak fix below closed the `--screenshot` and `--print-tier`
exits but left the interactive ones: closing the window or quitting mid-probe
ran no hook, so the `muse` TUI child — its own session leader — was orphaned
(the leak entry names this as its residual). `main.rs` now registers both
hooks after opening the window: `on_window_should_close` for the red dot
(macOS does not quit when the last window closes) and `cx.on_app_quit` for
Cmd+Q, Dock quit and `cx.quit()`, each calling the same `kill_live_probes`
plus bounded 3 s `wait_for_probes_gone` pair as the screenshot path. Both are
idempotent and the quit proceeds when the wait expires. Verified by reading
the hook path against the gpui-pre 0.3.3 sources; no live probe was run, per
the spend rule.

## 2026-09-09 — effort picker offers max (task A)

Muse 1.1.1 added `max` to the closed MSP `ReasoningEffort` vocabulary between
`xhigh` and `ultra`, and the wire client already accepted it — but the effort
picker still omitted it because its tiers are the library's
`aui_protocol::ReasoningEffort`. The library (agentic-ui branch `effort-max`)
gained `ReasoningEffort::Max` with the `Max` picker label, the gallery sample
rows list it, and the protocol test pins the `"max"` wire string. The harness
maps it end to end: `EFFORTS` lists `Max` between `Xhigh` and `Ultra`
(8 → 9 slots with the leading Default), `effort_detail` describes it, and the
wire map sends `Wire::Max`. This retires the picker-omits-max tail of
`docs/01-transport.md` §4 item 6 and the out-of-scope note in
`docs/10-msp-1.1.1-diff.md`; `docs/03-composer.md` now lists `max` among the
effort rows. No live turn was spent: replay/unit gates only.

## 2026-09-09 — tier probe leak

The billing probe orphaned its `muse` TUI. The probe runs on a background
thread (`probe_tier` → `background_spawn`), while the process can leave
without it: a `--screenshot` run quits ~3 s in, long before the ~11–20 s
probe finishes. The quitting process kills the thread without running
`Pty`'s `Drop`, and the child — its own session leader since `pre_exec`'s
`setsid` — is reparented to pid 1 and lives on. Two such orphans were found
alive after 6 hours; they ignored SIGTERM and died on SIGKILL. Reproduced on
the screenshot boot path (orphan at ppid 1, confirmed); the `--print-tier`
path was always clean (synchronous probe, `Drop` runs before `exit`).

The fix, in `crates/harness/src/tier.rs` plus the two exit paths: the `Drop`
SIGKILLs explicitly (the TUI ignores SIGTERM) and reaps; every probe writes
its child's pid to `tier-probe/probe.pid` and removes it on drop, and each
new probe SIGKILLs a previous pid whose command line still names the probe
workspace — never on the pid alone. `--print-tier` joins a probe thread with
a bounded wait (`PROBE_CEILING` + 5 s) before exiting; the screenshot's quit
SIGKILLs live probes and waits, bounded, for their drops, so the pid file is
gone too. The pump stops on a dead child (`try_wait`), so a killed probe
finishes within a tick. Covered by three offline unit tests (pid-file parse,
sweep-only-a-probe, drop-SIGKILLs-`/bin/sleep`); no test opens the real
`muse`. Residual: closing the interactive window mid-probe still orphans —
the next probe sweeps it.

## 2026-09-09 — muse 1.1.1 schema

The `muse` CLI self-updated 1.0.3 → 1.1.1, so the MSP schema exports were
regenerated in place (`fixtures/msp/msp-ts/msp.d.ts`,
`fixtures/msp/msp/{manifest.json,msp.schema.json}`) and `schema.rs` absorbs the
whole diff. Fingerprint `sha256:0331…758b7` → `sha256:c669…03e6a4f` (manifest,
corroborated by a free `initialize`). The diff is purely additive: 2 new
methods (`item/readOutput`, `view/subscribe`), 1 new notification
(`session/modelRouteUnserved`), 6 new types, 3 new enum variants, 4 new fields —
including `ReasoningEffort.max`, which retires the §4 item 6 discrepancy in
`docs/01-transport.md`. Both new methods have typed params/results and
`MuseClient` wrappers; the new notification is indexed but deliberately has no
fold arm (the fold ignores unknown methods). No behaviour fix was needed:
framing, ids, `session/list`, approval/userInput shapes and error kinds are
unchanged. The 1.0.3 captures replay untouched, with no snapshot regeneration.
Full write-up: `docs/10-msp-1.1.1-diff.md`.

## 2026-09-09 — Improvements — sidebar noise

Screenshot and test runs leave dozens of sessions that never had a turn, so
the sidebar was mostly noise. Sessions with no turns are now hidden by
default. A session counts as *empty* when it has no turns, is not the open
session, is not running, and has no user-given name (`SessionMeta.name`) —
the rule lives in one pure function, `SessionEntry::is_empty`, and the open
session is never filtered, so a session just created stays visible.

- **"Show empty (n)"** in the sidebar footer, next to "Show hidden (n)",
  same style, shown only when n > 0. `/empty` (in the `/` menu, and typed in
  full) toggles the same filter.
- **"Clear empty"** on its own row below the toggle, shown only while the
  empty rows are on screen: it hides every currently-empty session through
  the existing `hidden = true` override, so it persists, with a toast whose
  one Undo restores the whole batch. Rows already hidden stay out of the
  batch. The footer buttons stack in short right-aligned rows (hidden row,
  empty-toggle row, clear row) because the plan label keeps a fixed width
  and truncates first: one shared row squeezed it into an ellipsis.
- While the toggle is on it reads **"Hide empty"** with no count — the rows
  are on screen, so the count is redundant, and the width is needed for the
  plan label. Off it stays "Show empty (n)".
- The sidebar's empty state names its own toggle when only empty sessions
  were filtered out: "Only empty sessions here" / "Turn on “Show empty”
  below to see them."
- Screenshots: `docs/images/improve-sidebar-empty-{dark,light}.png` show the
  footer with the toggle off; `docs/images/improve-sidebar-empty-on-{dark,light}.png`
  show it on, with "Hide empty" and "Clear empty" on their own rows next to
  the fully readable plan label. The on-state shots are taken in the
  `/private/tmp/harness-ws` scratch workspace: it holds stable empty
  sessions, while `muse serve` 1.1.1 prunes freshly started turn-less
  sessions before any later run can photograph them (see the footer note in
  `docs/02-app.md`).

## 2026-09-08
- Spec frozen: docs/00-spec.md. Wire captures and schema exports from muse 1.0.3 added under fixtures/msp.

## 2026-09-08 — Phase 1: transport and fold

Two crates, no UI. `crates/muse-client` (the `muse serve` child, NDJSON JSON-RPC,
a typed Rust surface for all 186 types in `msp.d.ts`) and `crates/muse-adapter`
(`MuseFold`: MSP view events → `aui_protocol::Delta` + `SideState`), plus the
`aui-protocol` extensions on agentic-ui's `muse-support` branch. Every capture in
`fixtures/msp/*.jsonl` replays through the fold to a checked-in snapshot with no
`Generic` fallbacks; `live_echo` drives a real `muse serve` on the free echo
provider. Full detail in `docs/01-transport.md`.

Discrepancies recorded per the spec's precedence rule (capture wins):

- **`muse serve --no-session-log` emits no view events.** The spec names it as
  the flag tests should use. Under an ephemeral host a `turn/start` is accepted
  and `session/started` is the only notification that ever arrives — no
  `turn/started`, no items, no `turn/completed`. Verified through `muse-client`
  and independently through `fixtures/msp/probe.py`. The probe and `live_echo`
  therefore run **durable**; `HARNESS_PROBE_EPHEMERAL=1` reproduces the silence.
- **An approval's `itemId` is its own id, not the gated item's.** The research
  doc §7.2 implies otherwise. The `userShell` item it gates has a different id
  and no back-pointer; only `toolCall.approvalId` joins the two, and only for
  model tool calls.
- **An approval's `turnId`, for a user shell, is the shell item's `commandId`**
  even though the item's own `turnId` is `null`. Nothing in `msp.d.ts` says this;
  the fold relies on it to file the shell card and its approval in one turn.
- **Five schema-optional fields are always present on the wire**, sometimes as
  `null`: `Item.turnId`, `SessionTokenUsageParams.modelId`,
  `SessionBranchChangedParams.branch`, `SessionModelChangedParams.providerId`,
  `SessionGoalChangedParams.goal`. Modelled as required-nullable.
- `UnframedViewNotificationParams` needs a `serde(flatten)` catch-all or every
  `view/page` event loses its payload.
- Confirmations of the research doc, not contradictions: `onRequest` raises no
  approval for a model-issued `ls`; a turn can bill reasoning tokens and emit no
  `reasoning` item; `session/started`, `approval/request` and `userInput/request`
  are all on the wire and all absent from the published method index.

Spend: zero real-provider turns. Everything ran on `echo`.

## 2026-09-09 — Phase 2: shell, sessions, streaming

`crates/harness`, the gpui app. It boots as `aui/examples/minimal.rs` does, opens
one 1440×900 window titled "Harness", and drives a real `muse serve` through
`muse-client` and `MuseFold`. Auth probe and device-code login screen, the
sessions sidebar filtered to the workspace and enriched from the local index,
resume with full `view/page` backfill, a real streaming turn with tool cards and
the per-turn token footer, stop with retract, the reconnect procedure, and the
error banner/dialog split of §3.8. Full detail in `docs/02-app.md`.

Spend: **two real `meta` turns** (of the five the spec allows), both in
`docs/images/phase2-turn-*.png`. Everything else ran on `echo`
(`HARNESS_PROVIDER=echo`).

Findings and decisions worth keeping:

- **`session/list` filters `workspaceRoot` on exact string equality.** A session
  started in `/tmp/x` and one started in `/private/tmp/x` are two different
  workspaces to the wire, even though the index records the same
  `workspace_key`. The app therefore canonicalizes `--workspace` once at start-up.
- **A newly started session is not in `session/list` immediately.** The listing
  is index-derived and the index is written when the session log flushes, so the
  sidebar refreshes on `turn/completed` rather than only after `session/start`.
- **History comes from `view/page`, not from `session/resume`.** Resume runs with
  `excludeItems: true` and the transcript is paged forward from the beginning of
  the view. It is the one path that is contiguous, ordered and bounded, and it
  never replays `item/delta`, so a backfilled message arrives whole.
- **The credential is ambient, so a successful login needs a fresh child.**
  `muse serve` picks up the credential at spawn; the app respawns and re-probes
  after `Signed in.` rather than reusing the connection that inherited none.
- **`muse login` prints the device code bold through `tput`.** The stderr parser
  strips SGR before matching, and neither the URL nor the code is ever logged.
- The spec's §4 name for the composer is `aui::composer::docked_composer`; the
  editable docked composer is `aui::composer::composer(...).docked(true)` —
  `aui::shell::docked_composer` is the design card's non-editable placeholder.
  The app uses the editable one.

## 2026-09-09 — Phase 3: composer controls

The docked composer grew every control spec §5 phase 3 names: the model, effort
and approval-mode pickers, the context meter with compaction, the queued strip
with steering, `@` mentions, the `/` command menu with skills, client-side plan
mode, prompt history and images. Full detail in `docs/03-composer.md`; the
library side landed on agentic-ui `muse-support`.

Spend: **25 real `meta` turns**, five times the cap the spec sets. The lead
reported one (the plan probe); the owner's review of
`~/.local/share/muse/session-index.db` found that every screenshot run that
needed a tool card (`meter`, `blocked`, `queue`, `plan`, `compact2`) was started
without `HARNESS_PROVIDER=echo`, so it ran on `meta` — echo emits one canned
message and can never draw a tool card, which is how the mistake was found. The
plan probe itself was one turn; the effort probe was one; the rest were
screenshots. Guard added in the same review: a run with `--screenshot`,
`--steps` or `--send` now uses `echo` unless `--provider` or `HARNESS_PROVIDER`
names one explicitly, and says so on stderr. Later phases count real turns from
the index, not from the report.

Probe findings, from `fixtures/msp/probe_phase3.py` and the two captures it
wrote:

- **`/plan <text>` fires the bundled skill server-side.**
  `fixtures/msp/transcript-plan-probe.jsonl` (one `meta` turn, started in
  `denyUnmatched`) shows `toolCall read_skill {"name":"bundled:plan"}` followed
  by an agent message in the skill's own shape ("**Plan:** … Reply Approve,
  Request changes, or Cancel."). Plan mode therefore sends `/plan <text>` as the
  model-visible input with `displayText` carrying the person's words, and the
  spec's preamble stays only as a fallback behind `HARNESS_PLAN_PREAMBLE=1`.
- **Reasoning effort has no server reflection.** `reasoningEffort` occurs twice
  in `msp.d.ts` — `TurnStartParams` and `TurnSteerParams` — and nowhere else: no
  echo on the `userMessage` item, none on `turn/started`, no `…Changed`
  notification. The chip shows the client's value and says so in
  `docs/03-composer.md`. `high`, `ultra` and `none` were all admitted on echo
  (`fixtures/msp/transcript-phase3.jsonl`), so the picker is live, not disabled.
- **An image part is admitted without being decoded.** `turn/start` answered
  `status: accepted` for a payload whose PNG IDAT CRC was wrong. The wire checks
  the base64 and the media type and nothing else, so the app decodes before it
  sends and refuses what it cannot read. (The probe's own bytes have since been
  replaced with a valid 1×1 PNG.)
- **`session/compact` on a session with no run is rejected `missing_run`.** The
  schema documents the reason; it is confirmed live, and it is a banner rather
  than the `noop` toast.
- **`session/setModel` on an echo session is rejected `invalid_target`**, not
  the `unsupported_route` the phase brief predicted. Either way it is a
  `commandRejected` and goes to the inline banner, which is correct behaviour:
  `docs/images/phase3-banner-*.png`.

Review findings closed this phase:

- **F1** — Muse tool names now map onto `ToolKind::{Read, Write, Edit, Search,
  Web}` in `muse-adapter::tool_shape`, keyed on the name and on which `rawArgs`
  field is present, and the body follows the kind: a read renders as
  `Read <path>` with its line count, a search promotes `path:line:text` output
  to real hits when every line parses, and everything else keeps the raw output
  because a card with no body would hide what the tool said. Fold snapshots
  regenerated.
- **F4** — the `$0.00` cost cell is not drawn when the catalog reports no price,
  which on a subscription catalog is every row.
- **F5** — the initial "Approval mode · Auto" marker is suppressed. A session
  announces its mode at start-up, and that announcement is only worth a row when
  the mode is *not* the default; a change that changes nothing raises none
  either.

F2 (humanized failure reasons) and F3 (live vs backfill fold parity) remain for
Phase 4, as the brief said.

Two smaller decisions, recorded because they are deviations worth knowing:

- **The pickers get no `on_hover` from the app.** The component already lets the
  pointer win the highlight; an app that also wrote the hovered row into
  `selected` gave the selection two owners, and the check — which marks what the
  session is on — followed the mouse. The keyboard owns `selected` now.
- **The caret popovers scroll.** `command_menu` has no height cap of its own and
  the `/` menu lists thirteen commands plus every installed skill, so the app
  caps it at 560 px and scrolls, and offers at most eight skill rows.

## 2026-09-09 — Phase 4: approvals, questions, errors

Everything the agent has to stop and ask about, and everything that goes wrong.
Approval card v2 with server-minted choices, multi-stage subjects, feedback,
badges and policy/judge resolutions, driven by a real `approval/decide`
round-trip; the question card with headers, per-option previews, a timeout
countdown, clarify and cancel over `userInput/*`; F2 humanized failures with
retry and `turn/retryScheduled`; every marker kind; `session/fork`; todo and
goal; the mandated generic-item fallback; and `--replay <capture.jsonl>`, an
offline mode that folds a checked-in capture with no server at all. Full detail
in `docs/04-approvals.md`.

harness `main`; library work on agentic-ui `muse-support`.

### Finding, and a correction to the record: `echo` is not a free provider

Phase 1 recorded that `session/start { providerId: "echo" }` was free, research
§2.4 says echo "emits one canned message, no usage", and `docs/01-transport.md`
§6 repeated it. **All three are wrong on a machine that is signed in**, and the
owner's review of `~/.local/share/muse/sessions/*/session.jsonl` proved it.

How to read the truth out of a session log:

- the `command_intake` record at the top of `session.jsonl` carries
  `provider_id: echo` — that is the route that was *asked for*, and it is the
  only place `echo` ever appears;
- a later **metadata** record in the same file names what actually served the
  turn: `provider_id: meta`, `model_id: muse-spark-1.3-contributor`;
- `~/.local/share/muse/session-index.db` follows the **metadata** record, not the
  intake, so the index reports `meta` for a session started as `echo`. That is
  the number the owner counts spend from.

The corroboration is in the fixtures we already had: turns routed through `echo`
bill reasoning tokens (`fixtures/msp/transcript-echo.jsonl` carries a
`session/tokenUsage` with `reasoningTokens: 94` — a canned string does not
reason), carry provider response ids, and come back with varied real replies
("Hello — what do you want to work on?", "Hello! I'm Muse Code powered by Meta
Muse Spark…") rather than one fixed line.

`--provider` picks a **route, not a bill**. Every turn on every provider is a
real subscription turn, and the cap of five per phase covers all of them.

Wording corrected in `crates/harness/src/main.rs` (module header, the
`parse_args` comment, the scripted-run notice and `--help`),
`crates/harness/src/session.rs` (the user-shell doc comment, which was right for
the wrong reason), `crates/muse-client/tests/live.rs` (both the module header and
`live_backfill_parity`'s doc comment), `docs/00-spec.md` §2.1,
`docs/01-transport.md` §6, `docs/02-app.md` §1 and `docs/03-composer.md`.

What actually costs nothing: `--replay` and `--no-connect`; and on a live server
`session/start`, `session/userShell` (the `!` path), `approval/*`, `userInput/*`,
`session/fork`, `session/list` and `view/page` — none of them make a model call.
Anything that reaches `turn/start` spends a turn.

### Real turns spent this phase

Six, against a cap of five — **all six spent by the two previous leads**, before
the finding above was made and while they believed `echo` was free. This lead
spent **zero**: every screenshot and every gate in this entry came from
`--replay` or from an offline test.

| turns | prompt | run |
|---|---|---|
| 4 | `hello there` | four `live_echo` runs of `muse-client/tests/live.rs` |
| 1 | `parity, please` | one `live_backfill_parity` run |
| 1 | `Ask me which of README.md or notes.txt to describe, using your request_user_input tool, then describe it.` | the `userInput/answer` capture, `fixtures/msp/transcript-userinput-answer.jsonl` |

The one turn that bought something irreplaceable is the third: a
`request_user_input` request only exists when the model calls the tool, so there
was no other way to capture the question flow. The first five bought nothing that
`--replay` could not have produced.

Both `live.rs` tests now carry a doc comment saying they spend a turn per run and
naming the offline gate that replaces them.

### Findings closed

- **F2 — humanized failures.** `muse_adapter::failure::humanize(kind, message,
  reason)`: the title from `TurnErrorKind`, the detail from `error.message`, and
  a `reason` code turned into a sentence with the raw code kept on a second mono
  line. One table, nine known reasons, a unit test each.
  `resume_reconcile:orphaned_by_process_loss` reads "The turn was orphaned when
  the session's process was lost". A code with no sentence keeps only the mono
  line; nothing is invented.

- **F3 — live vs backfill parity, and the bug it found.** The two folds
  **disagreed**, and the disagreement was block order inside a turn. In
  `transcript-approve.jsonl` a `userShell` item starts (log sequence 6), raises a
  two-stage approval (sequence 9), and only completes afterwards. Live, the tool
  card is added at `item/started` and the approval lands below it. Backfilled,
  there is no `item/started` at all — a `view/page` serves finished items — so
  the approval arrives first and the tool card only at its `item/completed`, and
  the two blocks came out in opposite orders. A session read a second time did
  not say what it said the first time.

  Fixed in `MuseFold::push_block`: a block is placed by its item's own
  `sourceRange.first.sequence`, which is the **same number** on `item/started`
  and on `item/completed`, rather than by arrival order. `aui_protocol::Delta`
  has no insert variant, so an out-of-order arrival is expressed as an append
  plus the `BlockUpdated`s that rotate the tail, and every cached `Slot` past the
  insertion point shifts with it. `remove_block` and `reindex` keep the parallel
  order keys honest.

  **The gate is offline.**
  `muse-adapter/tests/fixtures.rs::a_live_fold_and_a_backfilled_fold_agree`
  derives the backfill stream from every checked-in capture that carries a
  streamed item — drop `item/started` and `item/delta`, keep `item/completed` and
  every session-level notification, in cursor order, the shape
  `transcript-wire.jsonl`'s `view/page` result confirms — folds both ways,
  normalises the streaming flags and the turn metas a backfill cannot know, and
  asserts equality. It refuses to pass if fewer than four captures exercise it.
  `a_tool_card_that_raised_an_approval_stays_above_it_in_both_folds` pins the
  specific regression. The live `live_backfill_parity` is kept, still `#[ignore]`d
  and now documented as costing a turn per run; it is no longer the gate.

- **F6 — plan sections.** `aui_protocol::Block::Plan` gains
  `#[serde(default)] sections: Vec<PlanSection { label, first_item }>`;
  `plan_card.sections(..)` draws the label as an unnumbered row before its first
  item and the numbering keeps counting steps only. `plan::steps` returns
  (items, sections): headings become sections, list items become steps, and a
  heading with no items under it is dropped.

- **F7 — duplicate skill names.** A skill whose name equals a client command's
  name is hidden from the `/` menu. Muse ships `plan`, and a menu offering both
  `/plan` the mode and `/plan` the skill — which do different things — was a trap.

- **F8 — model menu labels.** The picker menu is now a floor and a ceiling rather
  than a fixed width, and nothing in it is ever ellipsised. The floor had to be a
  width that is actually right (360, the widest name Muse's catalog ships with
  its context limit and two badges) rather than a token minimum, because gpui's
  layout cannot shrink-to-fit a column of stretched rows: a row that fills its
  parent and a parent that sizes to its rows is circular, and taffy resolves that
  circle at the floor. `w_full` is off every row; stretch does the job.

### The clarify path was verified with a synthetic capture

`fixtures/msp/transcript-userinput-answer.jsonl` is a real capture of the whole
**answer** path — the prompt, the model's `request_user_input` with two options,
`userInput/answer { selectedLabel: "README.md" }`, `userInput/settled` with
`outcome: "answered"`, and the reply. It cost the one turn named above and it was
read line by line before being committed: it carries no credential, no token and
no header — only local paths under a scratch workspace (`/private/tmp/h4meta`).

**The CLARIFY path was never captured live**, and this lead did not spend a turn
to get it. `fixtures/msp/synthetic-userinput-clarify.jsonl` was built instead:
every line down to and including `userInput/requested` is **verbatim from the
real capture** — the request the provider actually minted — and from the
`userInput/clarify` command on, the lines are hand-written to the shapes in
`msp.d.ts` (`outcome: "clarified"`, `answers: []`, a `clarification` object with
`content` and `format`). Its `#` header says exactly which half is which, and it
is named `synthetic-` so nobody mistakes the second half for the wire. It folds
to `QuestionOutcome::Clarified` and renders as `phase4-clarified-*.png`. The
answered row it produces has been seen; a live `outcome: "clarified"` has not.

### Smaller things worth knowing

- **Three `synthetic-*` captures had a `session/started` the schema rejected.**
  They omitted the required `path` (and `activeTurnId`), so the frame failed to
  deserialize and the fold silently ignored it — those captures folded with an
  empty model, cwd and approval mode and nobody noticed, because nothing asserted
  on them. Fields added; the three snapshots now carry the real values.

- **`every_recorded_frame_round_trips` is scoped to recordings.** It asserts a
  frame survives the typed schema *byte for byte*, which is what makes it
  evidence about the wire. A hand-written `synthetic-*` capture is not evidence
  about anything and cannot meet a byte-exact bar — it omits the nullable fields
  the server always spells out. Those captures are excluded, with the reason in
  the code; `muse-adapter`'s fixture tests cover them. The test also now skips
  the `#` header lines, and `model/list` joined `UNTYPED_METHODS` because it
  genuinely takes no params and `muse-client` sends it bare.

- **Eight `clippy::type_complexity` errors** in `harness::transcript::Cards` were
  fixed by naming the shapes rather than silencing the lint: `CardHandler`
  (a block id, out), `RowHandler` (a block id and a row index), `ChooseHandler`
  and `FeedbackToggleHandler`. The names say more than the types did.

- **`cargo tree -d` shows `gpui-pre-collections v0.3.3` twice**, at the same
  version. Not a version split, not introduced here, and no `gpui-pre` or
  `gpui-kit` itself is duplicated — the gate holds.

### Screenshots

`docs/images/phase4-*.png`, 1440×869 (the window is 1440×900 including its title
bar), light and dark. Almost all of them come from `--replay` and cost nothing:

| pair | source |
|---|---|
| `approval-stage1`, `approval-stage2`, `approval-feedback`, `approval-resolved`, `approve` | the live `!echo hi && ls` flow and `transcript-approve.jsonl` |
| `wire` | `transcript-wire.jsonl` — the policy denial under `denyUnmatched` |
| `real` | `transcript-real.jsonl` |
| `question`, `clarify` | the question card with a header and a preview open |
| `question-timeout` | `synthetic-question-error.jsonl` — the "Auto-resolves in 1 m 59 s" pill |
| `answered` | `--replay` of `transcript-userinput-answer.jsonl` |
| `clarified` | `--replay` of `synthetic-userinput-clarify.jsonl` |
| `error-retry` | `synthetic-error-retry.jsonl` — the humanized card **and** the retry-scheduled row in one shot |
| `todo-goal` | `synthetic-todo-goal.jsonl` (hand-written; no capture carries either event) |
| `fork`, `plan-sections`, `model-menu`, `banner` | as named |

`phase4-answered-live-light.png` is the one screenshot taken against the real
server, kept because it is the evidence that the `userInput/answer` round-trip
happened on a real session; `phase4-answered-{light,dark}.png` are the matched
pair replayed from the same capture.

---

## 2026-09-09 — Phase 5: the billing guard, session operations, polish, docs, CI

The last phase of the Muse Code chat slice. The slice is complete;
`docs/05-handoff.md` is now a maintenance handoff.

### The finding this phase exists for

**Muse has two credential tiers, and nothing on the wire says which one you are
on.** `initialize` and `model/list` carry no account or plan field, `auth.json`
carries only the mechanism and the identity, and the session log records a
`credential_backend`. The owner's login token from 2026-09-08 was on
**pay-as-you-go**, so roughly 110 sessions and 40 turns across Phases 1–4 were
billed as API usage while every document in this repository called them
subscription turns. A logout and a fresh login on 2026-09-09 14:46 put the token
on the **Muse Code High Usage** plan.

The one oracle is the TUI's `/upgrade` card, and `crates/harness/src/tier.rs`
drives it under a pseudo-terminal: answer the cursor-position query the TUI opens
with (`ESC [ 6 n`, without which it paints nothing), type `/upgrade`, press
Enter, read the card. Opening the TUI writes a session record and makes **no
model call**, so the probe costs nothing. Two things about the card are not
guessable and are pinned by a test against the wording this machine draws:

- the sentence is `subscribed to the {plan} usage plan.`, so the plan arrives
  glued to the template's own word;
- the slash palette's own row for `/upgrade` contains "pay-as-you-go", so a
  matcher that ran before the Enter reports the opposite of the truth on a
  subscribed account. The read buffer is cleared after the Enter for exactly
  that reason.

**The raw terminal output is never logged** — the card's footer carries a URL.
`HARNESS_TIER_DEBUG=1` reports byte counts and a fixed list of harmless words.

What the app does: the sidebar footer's third row names the plan or warns
"Pay-as-you-go" / "Plan unknown"; a pay-as-you-go login raises a warning banner
over the composer with "Sign out" beside "Send anyway", and `SessionView::submit`
— the single funnel every send reaches — refuses the turn until "Send anyway" is
pressed once per app run, handing the draft back rather than queueing it. An
unknown plan is a quiet banner that blocks nothing, and a probe that fails is
always `Unavailable` and never a failed boot. `/status` and `/usage` lead with
the plan and both percentages, and re-probe behind the dialog. The answer is
cached in `~/Library/Application Support/harness/tier.json` keyed by
`auth.json`'s mtime, so a logout and a re-login re-probe and an ordinary boot
does not. `docs/06-billing.md` is the whole story.

New flags: `--print-tier` probes and prints without a window;
`--tier subscription|payg|unknown` fakes the probe for a screenshot and fakes
nothing else.

### Session operations that are not on the wire (spec §3.7)

`~/Library/Application Support/harness/sessions.json` — per session a name, a
hidden flag and a derived title — written atomically through the new `store`
module, which also owns `tier.json`.

- **`/name <text>`** renames the active session; `/name` with nothing after it
  opens the row's inline field; `/name ` with an empty argument clears the name.
  The sidebar row grows a pencil, and the field it opens is the app's — the same
  slot pattern the composer's editor uses.
- **A typed `/` command is now a command.** `send()` parses the whole line, so
  every command is reachable by typing and the one that takes an argument is
  reachable at all. A prompt that merely begins with a slash is still a prompt.
- **`/hide` and the row's eye** take a session out of this window's list: a toast
  with Undo for eight seconds, "Show hidden (n)" in the footer, and a hidden
  session is never loaded — `resume` refuses it and `--session latest` skips it.
- **⌘⇧F** opens the sidebar's search field (the library's frame, the app's
  field): a case-insensitive **subsequence** over the name, the title, the first
  prompt and the index's `search_text`, so `fxparse` finds "fix the parser
  panic". Escape clears it and gives the keyboard back.
- **`/resume` and ⌘K** open the command palette — the same primitive for both,
  because they are the same gesture. `/resume` lists the twelve newest sessions
  under the titles the sidebar shows; ⌘K lists every `/` command.

**F10 — the fourteen rows reading "New session".** The cause was not an absent
title: **Muse's own index writes the literal string `"New session"` into
`title`**, a placeholder wearing a title's clothes. `IndexEntry::label` now
rejects it, so `session_name` and `first_user_prompt` win where they exist. For a
session with none of those, the first `userShell` command is the title — taken
from the fold when the session is open (free, and the reliable path) and
otherwise from a `session/read`, cached in `sessions.json`. A `session/read` of a
session no host has loaded can legitimately serve no history, and a row with
nothing left to be called is honestly "New session".

### Phase 4 review findings

- **F9 — `phase4-approval-stage1-*.png` showed only the shell card.** The
  capture raced the wire: `--screenshot`'s delay is measured from the first
  frame. The screenshot path now waits for the `--steps` list to finish, and for
  a pending approval when a `shell:` step was given, before its settling delay.
  The retake could not be made live: **this machine's managed shell sandbox is
  unavailable**, so a `userShell` under `promptUnmatched` never completes and no
  approval is minted (under `denyUnmatched` the policy refuses it before the
  sandbox is consulted, which is why that path still works). The two images are
  therefore `--replay` of `fixtures/msp/transcript-approve-stage1.jsonl`, the
  real capture truncated at its `approval/requested` — a prefix of a real wire
  log, not an invented one. Free and reproducible to the byte.
- **F11 — "Rule echo hi && ls".** The fold used the subject's command as the
  rule, so a card claimed the policy contained a rule that it did not. It now
  uses the amendment's `rulePreview`, else the reason the gated item carries
  (`deny_unmatched: no policy rule allows this action` → "no policy rule allows
  this action"), else the approval mode's name. The join is `toolCallId`, which
  is `<tool>_<commandId>`, and it works in **both** stream orders — live the
  approval resolves before the item completes, backfilled the item comes first —
  because F3 is the standing rule. On the library side, an allowed policy
  resolution still names its rule in mono; a denied one prints the reason in the
  UI face, because there was no rule. Two snapshot values changed and were read.
- `no_capture_needs_a_generic_fallback` still passes: no `Block::Generic`.

### Other corrections found while building this

- **A session started with an explicit approval mode drew the wrong chip.**
  `session/start` with an `approvalMode` raises no `session/approvalModeChanged`,
  and the result's session object was not being folded. It is now. New flag
  `--approval-mode <mode>`, which is a different thing from the `setmode:` step:
  `session/start` is the only surface that declares a session's policy, and on
  this server `session/setApprovalMode` does not reach `promptUnmatched`.
- The store is written **synchronously** on a gesture. A background write can
  lose a rename to a window that closed a moment later, which is the one outcome
  a store exists to prevent.

### Polish

Empty states in the library's voice, each saying why it is empty: no session
(the workspace's name and "⌘N to start one"), a fresh session (three suggestion
chips that fill the composer and never send — and none at all on a read-only
replay), no search match, hidden-only, logged out. The window's title is the
session's. `/fork`, `/name` and `/resume` no longer say "not in this build yet",
because they are.

### Docs and CI

`README.md`, `docs/06-billing.md`, `docs/07-architecture.md`,
`docs/08-keymap.md`, and `docs/05-handoff.md` rewritten as a maintenance
handoff. `.github/workflows/ci.yml` mirrors agentic-ui's — build, test, clippy
`-D warnings`, rustdoc `-D warnings` on macOS — with agentic-ui checked out
beside the harness on `muse-support` so the path dependencies resolve, plus a
one-`gpui-pre`-and-one-`gpui-kit` check. The two live tests stay `#[ignore]`d:
each spends a real, billed turn.

### Real turns spent this phase

**Zero.** Verified from the session logs rather than from a report:
`grep -c runtime.user_intent.accepted ~/.local/share/muse/sessions/*/*/*/*/session.jsonl`
summed to **46 before and 46 after** the phase. Every screenshot came from
`--replay`, from `--no-connect`, or from a live session that only ran
`session/start` and `session/userShell`; the billing probe opens the TUI, which
writes a session record and makes no model call.
