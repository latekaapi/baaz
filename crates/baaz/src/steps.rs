//! The scripting surface: `--steps` and `--login-steps`, in one place.
//!
//! Every screenshot in `docs/images/` is taken from a command line rather than
//! from a pointer, and these two flags are how. Until now the verbs lived in
//! three separate matches — window verbs in `app.rs`, session verbs in
//! `session.rs`, login verbs in `app.rs` again — with no shared dispatch and
//! nothing that could say what the whole surface is (finding `app-core-4` /
//! `app-core-13`). This module is that one place: one parser
//! ([`split`]), three verb tables ([`WINDOW_VERBS`], [`SESSION_VERBS`],
//! [`LOGIN_VERBS`]), the two runners, and the tables below that document them.
//!
//! A handler is a plain function pointer into the entity that owns the state
//! the verb touches, so the state stays private where it was; what moved here
//! is the vocabulary and the dispatch.
//!
//! Every handler returns a [`StepResult`]: `Ok` when it did what the step
//! says, `Err(reason)` when it did not. The first failed or unknown step ends
//! the script with a `steps: … failed: <reason>` log line, and a capture then
//! exits 1 — as it does when the list never became ready to run at all. A
//! step that silently does nothing is a bug in its handler (docs/82 1.4).
//!
//! # Cost
//!
//! **The whole surface is scripting only**: no verb here has any other entry
//! point, so this module is the one place that documents them. Only `send:`
//! and `steer:` reach `turn/start` and bill a real turn (like `--send`);
//! every other step — including `shell:`, which the **server** runs, and which
//! is how an approval is raised for nothing — is free. `--login-steps`
//! reaches `turn/start` nowhere at all, so none of it costs anything.
//!
//! # `--steps <a;b;c>`
//!
//! One step per item, `;`-separated because a step's payload may contain a
//! comma. A step is `verb` or `verb:<payload>`. A verb the window owns is
//! tried first; anything else goes to the open session.
//!
//! | step | what it does |
//! |---|---|
//! | `draft:<text>` | put text in the composer |
//! | `send:<text>` | send a turn — **costs a turn** |
//! | `steer:<text>` | steer the running turn — **costs a turn** |
//! | `send-anyway` | the pay-as-you-go banner's "Send anyway": sends what the guard held back — **costs a turn** |
//! | `model` | open the model picker |
//! | `effort` | open the reasoning-effort picker |
//! | `provider` | open the provider picker |
//! | `mode` | open the approval-mode picker |
//! | `folder` | open the new-session row's folder picker (docs/82 6.6); fails when the session shows no row (it has turns), the draft is moving, or the picker is already open |
//! | `confirm` | activate the open menu's selected row |
//! | `name:<name>` | `/name`, the session rename command |
//! | `hide` | `/hide` the open session |
//! | `setmodel:<id>` | `session/setModel`, without waiting for the catalog |
//! | `handoff:<provider>` | "Hand off to X" as a verb (`muse`, `claude-code`, `codex`), headless: no confirm dialog — the command line already asked. Needs turns, refuses on a pending question or approval, and never fires for the session's own provider; an unknown id fails, free |
//! | `setprovider:<id>` | the provider menu's row as a verb (`muse`, `claude-code`, `codex`): a fresh session swaps lanes, a session with turns starts a new session on the pick instead and the step fails saying so; an unknown id fails, free |
//! | `fixture-models` | fill the model menu with a fixed three-row Muse-shaped catalog whose rows open effort submenus (no catalog request), free |
//! | `submenu` | open the open model menu's highlighted row's submenu (as →); fails when the model menu is closed, free |
//! | `seteffort:<level>` | the effort menu's row as a verb (`default` clears); an unknown spelling fails, free |
//! | `compact` | `session/compact` |
//! | `meter` | pin the context meter's breakdown open |
//! | `context:<used>/<window>/<level>` | a synthetic `session/contextUsage` |
//! | `plan` | turn plan mode on |
//! | `image:<path>` | attach an image |
//! | `file:<path>` | attach a file |
//! | `plus` | open the composer's `+` menu |
//! | `drop` | raise the drop overlay |
//! | `command:<filter>` | open the `/` caret popover |
//! | `mention:<filter>` | open the `@` caret popover |
//! | `shell:<cmd>` | `session/userShell`, the approval generator that makes no model call |
//! | `setmode:<mode>` | `session/setApprovalMode`, without opening the picker |
//! | `choose:<n>` | the n-th choice of the newest pending approval |
//! | `feedback:<text>` | type into the open session's approval feedback or question clarify field, without sending, free |
//! | `answer:<label>` | pick one option on the newest question and send |
//! | `answers:<a\|b>` | pick several options on the newest question and send |
//! | `clarify:<text>` | "Explain instead"; with no text, only opens the field |
//! | `preview:<n>` | open the n-th option's preview (0-based) |
//! | `select:<label>` | pick an option without sending |
//! | `skip` | decline the newest question |
//! | `select-text:<turn>:<from>-<to>` | hold a text selection over a turn |
//! | `select-span:<turn>` | hold the whole turn as one cross-block span |
//! | `quote` | quote the held selection into the composer (the selection bar's Quote, D6 §0) |
//! | `copy:<turn>` | capture aid: press the turn's copy button (0-based, like `select-span`), so the success check holds for the screenshot, free |
//! | `top` | jump to the head of the transcript |
//! | `transcript-find:<query>` | ⌘F's bar on the open transcript with `<query>` typed: every match tinted, the first from the reader's place current and scrolled to (docs/82 7.5); fails on an empty query or no match |
//! | `transcript-find-next` | ⌘G / ⏎ in the bar: the next match, wrapping; fails with no bar or no match |
//! | `transcript-find-prev` | ⇧⌘G / ⇧⏎: the previous match, wrapping; fails with no bar or no match |
//! | `transcript-find-close` | Esc / Done: close the bar and clear every mark; fails with no bar |
//! | `mid` | jump to the middle of the transcript |
//! | `end` | jump to the tail of the transcript |
//! | `expand-groups` | open every folded tool group |
//! | `bench:<n>` | drive N back-to-back frames for `BAAZ_FRAME_STATS` |
//! | `fork` | `session/fork` at the newest completed turn |
//! | `retry` | retry the newest failed turn |
//! | `search:<query>` | open the search palette, optionally on a query |
//! | `open:<session_id>` | open a session as an outside activation does (palette, boot; scripting only) — arms the one-shot reveal |
//! | `click:<session_id>` | open a session as a sidebar click does (scripting only) — never arms the reveal, never moves the list |
//! | `palette` / `palette:<query>` | toggle the command palette / open it and type `<query>` in its field |
//! | `resume` | open the resume picker |
//! | `fork-picker` | open the fork picker |
//! | `rename:<name>` | open the sidebar row's inline rename field |
//! | `hidden` | list hidden sessions anyway |
//! | `empty` | list sessions with no turns anyway |
//! | `show-archived` | list archived sessions anyway |
//! | `sidebar` | collapse or expand the sidebar |
//! | `sidebar-peek` | hold the sidebar peek open (or release it) while the sidebar is closed — a capture aid, no hover |
//! | `sidebar-width:<px>` | settle the sidebar divider at a width |
//! | `right:<browser\|changes\|files\|terminal\|tasks\|off>` | open the right pane on that kind, idempotently (`off` closes it, empty toggles; `terminal` moves the terminal into the pane); unknown fails the step, free |
//! | `right-lower:<changes\|files\|terminal\|tasks\|off>` | stack that view below the pane's top one (docs/82 8.1), opening the pane, idempotently (`off` closes the lower pane); the Page, the kind on top, Browse, the Settings page or an unknown kind fails the step, free |
//! | `right-split:<share>` | settle the lower pane's divider at that share of the pane's height (0.25–0.75, held), as a released drag does; no lower pane showing or a bad number fails the step, free |
//! | `terminal-place:<bottom\|pane>` | move the terminal there, as its header's place control does (docs/82 8.1), idempotently: the dock opens, or the pane shows it (below an open Page, else on top); the same tabs and ptys either way. Unknown, or the Settings page open, fails the step, free |
//! | `browse:<url>` | navigate the active session's browser (the home one with no session open), idempotently; empty fails the step, free |
//! | `tab-new:<url>` | open a tab on `url` (a person tab in Browse mode, otherwise for the shown owner: a session tab in the active session, a person tab with no session open) and select it; empty fails the step, free |
//! | `open-url:<url>` | the scripted form of a real open-URL event: the same classify/queue/open path `App::on_open_urls` drains into (a person tab in the current project, Browse mode, window to the front; queued before sign-in); empty fails the step, free |
//! | `tab-select:<n>` | select the n-th tab of the shown strip (1-based); unknown or out-of-range fails the step, free |
//! | `tab-close:<n>` | close the n-th tab of the shown strip (1-based), activating its neighbour; unknown or out-of-range fails the step, free |
//! | `tab-pin:<n>` | pin (or unpin, when already pinned) the n-th person tab of the current project (1-based, on-screen order); unknown or out-of-range fails the step, free |
//! | `tab-menu:<n\|shown>` | open the n-th person tab's right-click menu (1-based, on-screen order), or the shown tab's (a session's tab offers Keep this tab); it draws in the overlay window, which a main-window shot does not show; unknown or out-of-range fails the step, free |
//! | `tab-discard-now` | drop every hidden tab's live page at once (audible and agent-active pages are spared); the live-check hook, free |
//! | `history:<url>\|<title>\|<days ago>\|<words>` | seed one page into the (in-memory, capture-only) history, visited `days ago`, with the page's `words` kept for search when given; free |
//! | `bookmark:<url>\|<title>\|<folder name>` | seed one bookmark (capture-only store) into the named folder, made at the top level when new (default: Bookmarks bar); free |
//! | `peek:<url>` | ⇧-click: Peek `url` over the shown Browse tab (R4); free |
//! | `split:<url>` | ⌥-click: open `url` in Split beside the shown Browse tab (R4); free |
//! | `split-ratio:<0..1>` | set the Split's left share, as dragging its divider does (held to 0.2–0.8); free |
//! | `chrome-import` | open the Import from Chrome sheet (captures read only `BAAZ_CHROME_ROOT`, never the person's Chrome); free |
//! | `bookmarks` | open the Field scoped to Bookmarks (board R4Daily 1c); free |
//! | `omnibox:<text>` | focus the Browse address field and type `text` (the dropdown stays open on the typed text; entering Browse with its tab when needed), free |
//! | `find:<query>` | open the Browse find strip on the shown tab with `query` typed (a scripted "3 of 12" result lands through the poll path); empty fails the step, free |
//! | `permission:<kinds>` | script a permission ask (`camera,microphone`) on the shown tab, so the strip shows; unknown kinds fail the step, free |
//! | `download:<name>` | script an active download (`done:<name>` lands it) on the shown tab and open the popover; empty fails the step, free |
//! | `crashed:<reason>` | script a renderer crash on the shown tab, so the Baaz error page shows (empty is "the page crashed (-1)"), free |
//! | `page-actions` | open the page controls' ⋯ on the shown tab (the library's page menu with its offers); it draws in the overlay window, which a main-window shot does not show; no page fails the step, free |
//! | `extensions` | open the Browse extensions popover on the shown tab, free |
//! | `browse-ask:<text>` | Browse: the Field's Ask with `text` — the ⌘J line opens on the shown page and asks there (docs/82 8.2); scripted lanes only (the browser chat's lane sends nothing anywhere), so it reaches no model; empty text, no page, live lanes or no line fail the step |
//! | `keep-tab:<id\|shown>` | Keep this tab on tab `id` or the one shown (Browse's, else the Page's): the session's tab becomes the person's, same page (docs/82 8.3); a tab that is no session's fails the step, free |
//! | `page-expand` | the Page's "Open in Browse": the session's tab full width in Browse, the same page; no session tab on the Page fails the step, free |
//! | `page-collapse` | "Back to the session's Page" from Browse showing a session's tab: Sessions, that session, its Page on the tab; no session tab shown fails the step, free |
//! | `send-page:<what>` | Send page to session… (`what` empty) or Send selection (`selection`) for the page shown, through the composer router (the composer on screen, else the picker); drafts only; no page or no selection fails the step, free |
//! | `shot-pick` | a page screenshot landing where the window stands (a 1-px fixture picture; the scripted page takes none): with no composer on screen (Browse, Settings) "Add to which session?" opens; no picker (a composer on screen took it) fails the step, free: nothing attaches until a pick |
//! | `fixture-session:<name>` | capture aid: park a local session row plus one session tab carrying `https://example.com`, and carry it into Browse (the offline `browse-from-session` shot), free |
//! | `page-menu` | open the Page row's Recent pages menu (`page-menu:off` closes it), free |
//! | `right-width:<px>` | settle the right-pane divider at a width, clamped into the library range so captures never depend on `layout.json`, free |
//! | `mode:<sessions\|browse>` | set the window's mode idempotently (`browse` shows the person's tabs, `sessions` the sessions; never toggles); unknown fails the step, free |
//! | `files-select:<path>` | preview the project-relative file in the Files pane through the click's own path (a directory toggles instead); empty or project-less fails, free |
//! | `changes-open:<path>` | open that changed file's diff in the Changes pane (idempotent; the path is project-relative, as git reports it); unknown fails the step, free |
//! | `select-lines:<path>:<a>-<b>` | hold a line selection (docs/82 7.1) over the shown view of `path`: on Files its open preview (file lines `a..=b`), on Changes its open diff (the rows numbered `a..=b` on the new side; `<path>:old:<a>-<b>` for removed rows on the old side); fails, holding nothing, when that view is not open or loaded or no line matches, free |
//! | `quote-lines` | the pane's selection bar's Quote: the held lines go to the open session's composer as a quote with their path and range; fails when no lines are held, their view closed, or no session is open, free |
//! | `undo-turn:last` | the confirm for the open session's newest undoable turn (docs/82 6.4), through the session menu's path; a capture (which takes no checkpoints and runs in the owner's repository) puts up the confirm from fixture data — the newest agent turn that edited files — and its button cannot apply; no session, another payload or nothing to undo fails the step, free |
//! | `revert-file:<path>` | the confirm for that changed file in All changes, back to `HEAD` (docs/82 6.4), through the row's path; a capture puts it up from fixture data and its button cannot apply; no session or project, or no such changed row, fails the step, free |
//! | `tick:<path>` | tick that changed file for the commit in All changes (docs/82 7.2; a setter, never a toggle); no tree read or no such changed row fails the step, free |
//! | `untick:<path>` | untick it (the same setter, off); the same failures, free |
//! | `changes-branch:<name>` | capture aid: the capture's fixture tree on that branch, tracking `origin/<name>` and level with it (so `main` is the default branch); outside a capture, or empty, fails the step, free |
//! | `pr-fixture:<name>` | capture aid (docs/82 7.3): a recorded `gh` answer (`passing`, `failing`, `merged`, `closed`, `draft`; fixtures/gh) as the PR of the pane's root — the fixture tree clean on the PR's branch, the card at the Changes foot, and the rows' book on that root and branch. Never runs `gh`; outside a capture, with no root, or an unknown name fails the step, free |
//! | `pr-details:<name>` | capture aid: `pr-fixture:<name>` with Details open over Changes (the checks and review comments), free |
//! | `new-branch` | open New branch…'s name field on the Changes foot (docs/82 7.2; idempotent); not on the default branch, or a foot without the branch line, fails the step, free |
//! | `amend:<on\|off>` | set "Amend last commit" in the commit box (docs/82 7.2; a setter); no changes, or Amend not offered (HEAD on a remote), fails the step, free |
//! | `commit:<message>` | commit the ticked files with `<message>` through the Commit button's own path — for real, so **never in a capture** (fails the step); no message, no ticked file or a git operation already running fails the step; the commit lands off the UI thread, so a script `wait:`s and checks git (Gate 7's scripted live run) |
//! | `branch:<name>` | New branch… with `<name>` typed and ⏎ (`git switch -c`, the working tree carried) — for real, **never in a capture** (fails the step); off the default branch, an empty name or a git operation already running fails the step; git's refusal is the toast, so a script checks git after |
//! | `changes-scope:<all\|session\|last>` | the Changes pane on that scope (docs/82 6.3): a capture loads the fixture checkpoint index (two finished turns over the fixture tree) — no refs are read or written; no open session or an unknown word fails the step, free |
//! | `run` | press Run on the open session (docs/82 7.4): the folder's `.baaz/project.toml` `run` starts in a terminal tab of the session's own, listed in Tasks; a Run that still runs is shown, not started again; no session or no `run` declared fails the step — **starts a real process** (captures: a static server in a fixture folder only) |
//! | `run-stop` | Stop run on the open session: its process group ends; nothing running fails the step |
//! | `run-wait-preview` (or `run-wait-preview:<s>`) | the runner waits up to that many seconds (30) while the Run's preview port has not answered, then the step checks the session's Page opened on it; gave up, run ended, moved on or no Run fails the step |
//! | `tasks-fixture` (or `tasks-fixture:<session_id>`) | the Tasks view's capture fixture (docs/82 6.5): four tasks on that session, else the open one — running on :5173, exited 0, exited 1, stopped — with fixed durations and no process behind any; no session, or one that already has tasks, fails the step, free |
//! | `pages-fixture` (or `pages-fixture:<session_id>[,<n>]`, `pages-fixture:,<n>`) | capture aid (docs/82 8.4): `n` pages (2 by default, 1–9) owned by that sidebar row (the open session with no id), as tabs of its own on fixed `example.com` paths, no page loaded; no such row or no session open, a bad count, or a session that already owns pages fails the step, free |
//! | `pages-row:<session_id>` | the row's "N pages" click: one page opens the session on its Page, several open Browse on its task folder; a session that owns no pages fails the step, free |
//! | `browse-footer:<needs-you\|last>` | the Browse footer's row click (docs/82 8.4): "Needs you" (Sessions, its filter on, the first waiting session open) or the last session (Sessions on it); outside Browse, or a row the footer does not show, fails the step, free |
//! | `tasks-row:<session_id>` | the sidebar row's "N running" click: that session opens with the right pane on Tasks; a session with nothing running fails the step, free |
//! | `files-reveal:<path>` | reveal the project-relative file or folder in the Files pane through the link's own path (a folder selects with every ancestor expanded and no preview, a file previews banded on the payload's `#L…`/`:line` suffix); empty or project-less fails, free |
//! | `row-detail:<session_id>` | capture aid: pin the hover card open for one row, seated at the selected row's bounds (pair with `click:` on the same id; empty clears), free |
//! | `hover:<session_id>` | capture aid: deliver the selected row's own hover report (what its hover event sends), arming the card past the delay seated from the row's bounds at the sidebar's right edge (pair with `click:` on the same id and a `wait:` past the delay; empty means the selected row), free |
//! | `resize-begin:<x>` | start a scripted resize drag through the real divider handler, logging `baaz: rsdrag` with the width, the sidebar offset, the reveal arm, the scrolled flag and the drag |
//! | `resize-move:<x>` | move a scripted resize drag through the real divider handler (same log) |
//! | `resize-end` | end a scripted resize drag through the real divider handler (same log) |
//! | `resize-sweep:<to_w,step_px>` | march the divider toward `to_w` one `step_px` per rendered frame, logging `baaz: rssweep w=<width> pane=<pane> root=<root> rehint=<0/1>` per tick |
//! | `sidebar-scroll-sweep:<dy,finger_ticks,tail_ticks>` | a frame-paced sidebar wheel gesture: `dy` for `finger_ticks` rendered frames, then decaying to 5% of `dy` over `tail_ticks` more, one push per tick (the in-process fallback for a real `CGEvent` gesture where the environment cannot confirm a landing window; pair with `BAAZ_FRAME_TRACE=1`) |
//! | `page-eval:<js>` | run `js` in the Browse centre page (no `;` — it separates steps), e.g. `window.scrollTo(0,900)`; free |
//! | `site-info` | open the site-information panel (the address row's shield) for the centre page; no page fails the step |
//! | `pointer:<x>,<y>` | dispatch one mouse move to that window point: the pointer resting on a row, so a following `sidebar-scroll-sweep:` shows hover under a scroll |
//! | `transcript-scroll-sweep:<dy,finger_ticks,tail_ticks>` | the transcript's twin of `sidebar-scroll-sweep:`, pushing the active session's own wheel accumulator |
//! | `overflow` | open the header's overflow menu |
//! | `view-menu` | open the Sessions caption's view menu |
//! | `account` | open the account menu |
//! | `settings:<section>` | open the Settings page, optionally at that section (`settings` alone reopens the last-visited section, General on first open; `settings:providers/codex` opens a provider sub-page). Idempotent: open, never toggle |
//! | `stall:<ms>[,busy]` | block the UI thread for `ms` (at most 10 s; `,busy` spins on CPU instead of sleeping): the stall watchdog's live check — it should write a `stall:` line, a `logs/stall-*.txt` sample past 2 s and `stall ended:` to `<state dir>/logs/baaz.log`; a bad number fails the step, free |
//! | `settings-set:<key>=<value>` | the scripted twin of the Settings → Browser rows (`search-engine=<google\|duckduckgo\|bing\|kagi>`) and of Settings → Appearance (`summary-line=<on\|off>`, `theme=<system\|light\|dark>`, `density=<comfortable\|compact>`, `text-size=<default\|large>`); unknown keys or values fail the step, free |
//! | `sidebar-hover:<session_id>` | capture aid: stand the sidebar's pointer state on that session's row (its trailing slot shows the age and the `⋯`), as the row's own hover report would; an id with no row fails the step, free |
//! | `row-menu:<session_id>` | open that session row's `⋯` menu (idempotent: open, never toggle); an id with no row fails the step, free |
//! | `pin` | pin or unpin the open session |
//! | `archive` | raise the archive confirmation |
//! | `archive-confirm` | confirm it |
//! | `undo-confirm` | press the open Undo / Revert confirm's primary (docs/82 6.4; danger dialogs take no Enter); no such dialog open fails the step |
//! | `error-dialog:<offline\|blocked\|sorry>` | capture aid: raise a critical error dialog of that family through the real dialog path (empty means `sorry`; an unknown family fails the step), free |
//! | `toast:<title>\|<body>` | capture aid: raise a toast through the production toast path (empty title fails the step), free |
//! | `projects` | open the Projects palette |
//! | `project:<path>` | adopt `path` as a project and make it current (no panel, no session) |
//! | `project-menu` | open the header's project menu (`project-menu:<name>` opens the group row menu for the project named) |
//! | `project-colour:<n>` | set the current project's colour slot (1–8); with no payload, open the Colour submenu |
//! | `group-by:<date\|project>` | persist the sidebar grouping and regroup |
//! | `group-bar` | flip the current-project accent bar flag |
//! | `needs-you-filter:<on\|off>` | set the sidebar's "Needs you" filter (a setter; anything else fails the step) |
//! | `row-needs-you:<label>` | the sidebar row with that label starts waiting for an approval and the app takes its attention look (a live check's offline way to post a real notification; no such row, or one already waiting, fails the step) |
//! | `project-close:<name>` | close the project group named (a setter: a closed group stays closed); its header then shows the strongest state inside |
//! | `group-branch` | flip the trailing-branch flag |
//! | `auto-title` | flip the automatic session-naming switch |
//! | `auto-summary` | flip the sidebar-summaries switch |
//! | `transcript:fold` | flip the persisted "Fold finished turns" switch and apply it live to every open view |
//! | `title-pending` | capture aid: the open session reads as if its title generation were in flight (`Naming this session…`), free |
//! | `title-timeout` | capture aid: stand that generation down through the watchdog's own path, so the row falls back to the first prompt, free |
//! | `title-land:<text>` | capture aid: land `<text>` as the open session's generated title, so the row and the crumb update — after `title-timeout`, like a late answer would — free |
//! | `remove-project:<name>` | raise the project removal dialog |
//! | `remove-confirm` | confirm it |
//! | `new` | the same as ⌘N |
//! | `new:<project>` | the group row's `+` for the project named; the session opens on the `session/start` round-trip, so following session verbs (`name:`, `draft:`, `send:`) wait for the switch (bounded, 10 s) instead of acting on the session that is still open |
//! | `new-in:<path>` | `session/start` in `path` without adopting it — the folder panel's headless twin for "New session" with no current project; same round-trip wait as `new:<project>` — **costs a turn** |
//! | `worktree:on\|off` | move the open unsent draft into a new worktree of its project, or back to the project root, through the replace a provider switch uses (docs/82 6.1); fails when it already runs there, when it is no unsent draft or still opening, and always in a capture (the person's repository is off limits) |
//! | `worktree-default:on\|off` | the current project's worktree default for new sessions, stored with its other defaults; fails when it already says so |
//! | `unarchive:<id>` | put an archived session back (the Archive tray action); a worktree session's worktree is restored first (docs/82 6.2). Fails when the id names no archived row |
//! | `move-back` | "Move back to project folder" on the open session: its worktree's changes land in the project root, uncommitted, and it reopens there (docs/82 6.2). Fails where the overflow menu would not offer it |
//! | `wheel:<dy>` | dispatch one synthetic wheel event at the window centre and log `baaz: wheel dy=<dy> list_px=<before>-><after>` (the palette-scroll instrument) |
//! | `sidebar-wheel:<dy>[,n]` | dispatch n synthetic wheel events at a sidebar point and log `baaz: sbwheel dy=<dy> n=<n> sidebar_ix=<before_ix>+<before_off>-><after_ix>+<after_off> rows=<entries> pane=<pane> root=<root> centre=<centre> drains=<drains>` (the sidebar-scroll instrument: the virtual list's `ListOffset`, item index plus the pixel offset into that row, in place of the old div's pixel offset; pair with `wait:<ms>` and a trailing `sidebar-wheel:0,0` to read the burst's renders; `centre` is the cached transcript column's rebuilds) |
//! | `centre` | log `baaz: centre hero=<hero> loading=<loading>`: hero vs loading-row paints since the last call (the open-flicker instrument) |
//! | `key:<keystroke>` | post a real keypress for that `gpui::Keystroke::parse` keystroke through CoreGraphics into Baaz's own pid, exactly like a person's keypress — modifiers plus letters, digits, punctuation, enter, escape, tab, arrows and f1–f12; unknown names fail the step, free |
//! | `page-focus` | give the active browser page the keyboard, free |
//! | `agent-line` | Browse: open or close the ⌘J agent line on the page on screen (R3), free |
//! | `page-theme-changed` | Browse: send Chromium's theme-changed notification to every page window (as a macOS appearance or display change does), free |
//! | `agent-card` | Browse: open or close the ⌘E agent card (R3), free |
//! | `agent-column` | Browse: dock the card as a column, or undock it (⌘⇧E, R3), free |
//! | `browser-chat-fixture:<answer\|steps\|draft>` | Browse: a read-only browser chat with a canned transcript made the line's chat — `answer` lands the inline answer, `steps` opens the card, `draft` the landed draft (R3), free: no turn |
//! | `handoff-picker` | open the agent card's handoff picker (R3 board 3a), free |
//! | `setup:<provider>:<install\|signin\|update>` | open the D5 setup sheet as its button would (`claude-code`, `codex`); deterministic runs show a scripted terminal with the typed command, never a shell; a kind the provider has no command for fails the step, free |
//! | `connect-menu` | open the "New sessions start in" menu: the connect screen's footer, else Settings › Providers' (fails when neither is up), free |
//! | `codex-login:<waiting\|device\|failed>` | put Codex's sign-in in that state with sample values (example.invalid link, a sample code) — no app-server, no browser; captures only, free |
//! | `muse-sheet` | raise Muse's sign-in sheet over the screen, free |
//! | `launch-failed:<provider>[:<crashed\|not-installed\|signed-out\|too-old>]` | land the typed "Couldn't start" view (docs/71 §3) for that provider with sample words and a sample stderr well, as a failed open would; no child, no turn; unknown parts fail the step, free |
//! | `child-lost` | the open lane hears its child die (`ConnectionLost`), as its adapter would report it: the quiet banner with Retry now; twice in a row grows it into the typed failure (sample stderr well) under the transcript. No child, no turn; no session fails the step, free |
//! | `signed-out` | raise the signed-out banner above the composer for the open session's provider, as a typed auth failure would; no session fails the step, free |
//! | `provider-action:<provider>:<recheck\|signin\|signout>` | press that Settings card button through its click's intent handler (`signout` raises the confirm only) — **signs a CLI in or out for real**: live checks run it on throwaway `CLAUDE_CONFIG_DIR` / `CODEX_HOME` homes only; unknown parts fail the step |
//! | `signout-confirm` | press the open sign-out confirm's Sign out (no such dialog fails the step) — same warning |
//! | `crumb:<n>` | press the n-th Settings breadcrumb (0-based); the current page's crumb, or a crumb that is no link, fails the step, free |
//! | `import-open` | open the P7 import sheet (docs/72) on a plan scanned now, synchronously, from the owner home (`BAAZ_IMPORT_OWNER_HOME` in captures); a capture with no fixture home scans nothing and fails the step, free |
//! | `import-toggle:<folder>` | flip one folder's box on the open import sheet, by its name or its full path; `scripted` flips "Skip scripted runs"; an unknown folder fails the step, free |
//! | `import-run:[<n>]` | press "Import N sessions"; with `n`, stop after `n` sessions so the progress state holds for a capture; writes records (never files), free |
//! | `import-undo` | the import toast's Undo for the newest run: removes exactly the records it added, never a file; no run fails the step, free |
//! | `details:<provider>` | open that provider's Details on its Settings card (`claude-code`, `codex`, `muse`), free |
//! | `diagnostics:<provider>` | print what "Copy diagnostics" would copy to stderr instead of the clipboard (it runs the CLI's `--version` and auth status), free |
//! | `skills:<fixture>` (`page`, `empty`, `add-menu`, `import-preview`, `new`) | open the Skills page on a fixture catalog — no CLI, deterministic, offline; the K3 fixtures layer the Add menu, the import preview and the New dialog over the page fixture, free |
//! | `terminal-dock:<title>` | capture aid: show the terminal where it sits (the dock, or the pane per `terminal-place:`) over a FakePty-backed tab titled `<title>` (empty is "terminal") and drain its script at once, so two runs render the same bytes, free |
//! | `folder-new:<name>` | a new folder named `<name>` in the current browser space, holding the active person tab (the new-folder flow, committed), free |
//! | `space-new:<name>` | add a browser space named `<name>` and make it current, free |
//! | `wait:<ms>` | let the wire catch up before the next step |
//!
//! # `--login-steps <a;b;c>`
//!
//! Honoured only when the app is really connected (never with `--no-connect`
//! or `--replay`), run once the login screen is up, with the same parsing.
//! A step that fails ends the run with a stderr line.
//!
//! | step | what it does |
//! |---|---|
//! | `account` | start the device flow (the browser opens) |
//! | `apikey` | open the API-key form |
//! | `key-from-env:<VAR>` | put the value of environment variable `VAR` into the API-key field |
//! | `submit` | submit the API-key form |
//! | `wait:<ms>` | let the wire catch up before the next step |
//!
//! The key travels from the environment into the field and then into the wire
//! call: it never appears in argv, a log or a screenshot argument.

use aui::screens::LoginIntent;
use gpui::{Context, Window};

use crate::app::Harness;
use crate::overlays::{MenuKind, PaletteKind};
use crate::session::{SessionEvent, SessionView};
use crate::wire::WireCall;

use std::sync::atomic::{AtomicU64, Ordering};
use crate::layout::LayoutGlobal as _;

/// Scripted-step accounting: a flow script that names a renamed verb used to
/// pass silently (a stderr line, exit 0), so the script rotted into a no-op
/// unnoticed. A failed step — a verb's `Err`, an unknown verb, a session
/// verb with no open session — counts one failure and ends the script, as
/// does every step in a list readiness never released; the scripted-run
/// exit fails the process when the count is non-zero.
static STEP_FAILURES: AtomicU64 = AtomicU64::new(0);
/// Every scripted item actually executed: counted by the runners below as
/// each step runs. A skipped step is not a run step — a session verb that
/// finds no session, and any list the capture finds still undrained because
/// the run never became able to execute it (see `record_unrun_steps`),
/// count failures only, never runs.
static STEPS_RAN: AtomicU64 = AtomicU64::new(0);
/// The step names behind `STEP_FAILURES`, in order, for the
/// one-line-per-failure report before exit.
static FAILED_STEP_NAMES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Failures so far; the scripted-run exit fails when this is non-zero.
pub(crate) fn step_failures() -> u64 {
    STEP_FAILURES.load(Ordering::Relaxed)
}

/// Scripted items seen so far, for the `steps: ran=<n> failed=<n>` line.
pub(crate) fn steps_ran() -> u64 {
    STEPS_RAN.load(Ordering::Relaxed)
}

/// Failed step names behind `step_failures`, in order.
pub(crate) fn step_failure_names() -> Vec<String> {
    FAILED_STEP_NAMES.lock().map(|names| names.clone()).unwrap_or_default()
}

/// Classify a scripted list the capture finds still undrained because the
/// run never became able to execute it (see `steps_ready_for`): none of
/// these ran, so `ran` stays untouched — a skipped step is not a run step —
/// and every one of them failed, because the script did not do what it
/// said. The only exception is `wait:`, which touches nothing: an unrun
/// sleep is not a failure. Free: table lookups only, nothing reaches the
/// wire.
pub(crate) fn record_unrun_steps(steps: &[String]) {
    for step in steps {
        if step.strip_prefix("wait:").is_some() {
            continue;
        }
        record_step_failure(step);
    }
}

/// Whether a `--steps` item names a known verb, window or session. `wait:`
/// never reaches a table; it is the runners' own. Test-only: the runners
/// classify by what a step needs (see `is_session_step`), not by whether
/// its name exists, so an unrun or unrunnable step fails whatever it names.
#[cfg(test)]
fn is_known_step(step: &str) -> bool {
    if step.strip_prefix("wait:").is_some() {
        return true;
    }
    let (head, _) = split(step);
    WINDOW_VERBS.iter().any(|v| v.verb == head) || SESSION_VERBS.iter().any(|v| v.verb == head)
}

/// What every `--steps` verb reports: `Ok` when it did what the step says,
/// `Err(reason)` when it did not — an unparseable payload, a target that
/// is not there, a refusal. A verb that does nothing must say so: a script
/// that silently does nothing used to report success (docs/82 1.4).
pub(crate) type StepResult = Result<(), String>;

/// Count one failed step and name it for the summary. The runner calls
/// this for a verb's `Err`, an unknown step and a step with nowhere to
/// run; [`record_unrun_steps`] for a list that never ran.
pub(crate) fn record_step_failure(step: &str) {
    STEP_FAILURES.fetch_add(1, Ordering::Relaxed);
    if let Ok(mut names) = FAILED_STEP_NAMES.lock() {
        names.push(step.to_owned());
    }
}

/// The one parser: a step is `verb` or `verb:<payload>`.
///
/// The payload keeps every character after the first colon, which is what lets
/// `select-text:2:0-40` and `context:120000/200000/warn` be one step each.
pub(crate) fn split(step: &str) -> (&str, &str) {
    step.split_once(':').unwrap_or((step, ""))
}

/// A `--steps` verb the window owns.
pub(crate) struct WindowVerb {
    /// What the step says.
    pub verb: &'static str,
    /// What it does to the window, and whether it did it.
    pub run: fn(&mut Harness, &str, &mut Window, &mut Context<Harness>) -> StepResult,
}

/// A `--steps` verb the open session owns.
pub(crate) struct SessionVerb {
    /// What the step says.
    pub verb: &'static str,
    /// What it does to the session, and whether it did it.
    pub run: fn(&mut SessionView, &str, &mut Window, &mut Context<SessionView>) -> StepResult,
}

/// A `--login-steps` verb. The handler returns whether the run continues.
pub(crate) struct LoginVerb {
    /// What the step says.
    pub verb: &'static str,
    /// What it does to the login screen.
    pub run: fn(&mut Harness, &str, &mut Window, &mut Context<Harness>) -> bool,
}

/// The window's own `--steps` verbs, tried before the session's.
///
/// `resume` is here and in [`SESSION_VERBS`]; the window's picker wins,
/// exactly as it did when the two matches were tried in this order.
pub(crate) const WINDOW_VERBS: &[WindowVerb] = &[
    WindowVerb { verb: "search", run: |this, rest, window, cx| this.step_search(rest, window, cx) },
    WindowVerb { verb: "open", run: |this, rest, window, cx| this.step_open(rest, false, window, cx) },
    WindowVerb { verb: "click", run: |this, rest, window, cx| this.step_open(rest, true, window, cx) },
    WindowVerb { verb: "palette", run: |this, rest, window, cx| this.step_palette(rest, window, cx) },
    WindowVerb { verb: "resume", run: |this, _, _, cx| { this.open_palette(PaletteKind::Resume, cx); Ok(()) } },
    WindowVerb { verb: "fork-picker", run: |this, _, _, cx| { this.open_palette(PaletteKind::Fork, cx); Ok(()) } },
    WindowVerb { verb: "rename", run: |this, rest, window, cx| this.step_rename(rest, window, cx) },
    WindowVerb { verb: "hidden", run: |this, _, _, cx| this.step_toggle_hidden(cx) },
    WindowVerb { verb: "empty", run: |this, _, _, cx| this.step_toggle_empty(cx) },
    WindowVerb { verb: "sidebar-width", run: |this, rest, _, cx| this.step_sidebar_width(rest, cx) },
    WindowVerb { verb: "right", run: |this, rest, _, cx| this.step_right(rest, cx) },
    WindowVerb { verb: "right-width", run: |this, rest, _, cx| this.step_right_width(rest, cx) },
    WindowVerb { verb: "right-lower", run: |this, rest, window, cx| this.step_right_lower(rest, window, cx) },
    WindowVerb { verb: "right-split", run: |this, rest, _, cx| this.step_right_split(rest, cx) },
    WindowVerb { verb: "terminal-place", run: |this, rest, window, cx| this.step_terminal_place(rest, window, cx) },
    WindowVerb {
        verb: "page-menu",
        run: |this, rest, _, cx| match rest.trim() {
            "" | "on" => { this.right_pane.update(cx, |pane, cx| pane.set_page_menu(true, cx)); Ok(()) }
            "off" => { this.right_pane.update(cx, |pane, cx| pane.set_page_menu(false, cx)); Ok(()) }
            other => Err(format!("`{other}` is not on or off")),
        },
    },
    WindowVerb { verb: "mode", run: |this, rest, window, cx| this.step_mode(rest, window, cx) },
    WindowVerb { verb: "browse", run: |this, rest, window, cx| this.step_browse(rest, window, cx) },
    WindowVerb { verb: "tab-new", run: |this, rest, window, cx| this.step_tab_new(rest, window, cx) },
    WindowVerb { verb: "open-url", run: |this, rest, window, cx| this.step_open_url(rest, window, cx) },
    WindowVerb { verb: "tab-select", run: |this, rest, window, cx| this.step_tab_select(rest, window, cx) },
    WindowVerb { verb: "tab-close", run: |this, rest, window, cx| this.step_tab_close(rest, window, cx) },
    WindowVerb { verb: "tab-pin", run: |this, rest, _, cx| this.step_tab_pin(rest, cx) },
    WindowVerb { verb: "tab-menu", run: |this, rest, _, cx| this.step_tab_menu(rest, cx) },
    WindowVerb { verb: "folder-new", run: |this, rest, _, cx| this.step_folder_new(rest, cx) },
    WindowVerb { verb: "space-new", run: |this, rest, _, cx| this.step_space_new(rest, cx) },
    WindowVerb {
        verb: "tab-discard-now",
        run: |this, _, window, cx| this.step_tab_discard_now(window, cx),
    },
    WindowVerb { verb: "omnibox", run: |this, rest, window, cx| this.step_omnibox(rest, window, cx) },
    WindowVerb { verb: "history", run: |this, rest, _, cx| this.step_history(rest, cx) },
    WindowVerb { verb: "bookmark", run: |this, rest, _, cx| this.step_bookmark(rest, cx) },
    WindowVerb { verb: "peek", run: |this, rest, window, cx| this.step_link_gesture(rest, aui_webview::LinkGesture::Peek, window, cx) },
    WindowVerb { verb: "split", run: |this, rest, window, cx| this.step_link_gesture(rest, aui_webview::LinkGesture::Split, window, cx) },
    WindowVerb {
        verb: "split-ratio",
        run: |this, rest, _, cx| {
            let ratio = rest.trim().parse::<f32>().map_err(|_| format!("`{rest}` is not a number"))?;
            let ratio = crate::browser::peek_split::clamp_split(ratio);
            this.browser.update(cx, |bv, cx| bv.split_drag(crate::browser::peek_split::SplitDrag::Move(ratio), cx));
            Ok(())
        },
    },
    WindowVerb { verb: "chrome-import", run: |this, _, _, cx| { this.open_chrome_import(cx); Ok(()) } },
    WindowVerb { verb: "bookmarks", run: |this, _, window, cx| this.step_bookmarks(window, cx) },
    WindowVerb { verb: "find", run: |this, rest, window, cx| this.step_find(rest, window, cx) },
    WindowVerb { verb: "permission", run: |this, rest, window, cx| this.step_permission(rest, window, cx) },
    WindowVerb { verb: "download", run: |this, rest, window, cx| this.step_download(rest, window, cx) },
    WindowVerb { verb: "crashed", run: |this, rest, window, cx| this.step_crashed(rest, window, cx) },
    WindowVerb { verb: "page-actions", run: |this, rest, window, cx| this.step_page_actions(rest, window, cx) },
    WindowVerb { verb: "browse-ask", run: |this, rest, window, cx| this.step_browse_ask(rest, window, cx) },
    WindowVerb { verb: "shot-pick", run: |this, rest, window, cx| this.step_shot_pick(rest, window, cx) },
    WindowVerb { verb: "keep-tab", run: |this, rest, _, cx| this.step_keep_tab(rest, cx) },
    WindowVerb { verb: "page-expand", run: |this, _, window, cx| this.step_page_expand(window, cx) },
    WindowVerb { verb: "page-collapse", run: |this, _, window, cx| this.step_page_collapse(window, cx) },
    WindowVerb { verb: "send-page", run: |this, rest, window, cx| this.step_send_page(rest, window, cx) },
    WindowVerb { verb: "extensions", run: |this, rest, window, cx| this.step_extensions(rest, window, cx) },
    WindowVerb {
        verb: "fixture-session",
        run: |this, rest, window, cx| this.step_fixture_session(rest, window, cx),
    },
    WindowVerb { verb: "files-select", run: |this, rest, _, cx| this.step_files_select(rest, cx) },
    WindowVerb { verb: "changes-open", run: |this, rest, _, cx| this.step_changes_open(rest, cx) },
    WindowVerb { verb: "select-lines", run: |this, rest, _, cx| this.with_right_pane(cx, |pane, cx| pane.step_select_lines(rest, cx)) },
    WindowVerb { verb: "quote-lines", run: |this, _, _, cx| this.with_right_pane(cx, |pane, cx| pane.quote_held_lines(cx)) },
    WindowVerb { verb: "changes-scope", run: |this, rest, _, cx| this.step_changes_scope(rest, cx) },
    WindowVerb { verb: "tick", run: |this, rest, _, cx| this.step_changes_tick(rest, true, cx) },
    WindowVerb { verb: "untick", run: |this, rest, _, cx| this.step_changes_tick(rest, false, cx) },
    WindowVerb { verb: "changes-branch", run: |this, rest, _, cx| this.step_changes_branch(rest, cx) },
    WindowVerb { verb: "pr-fixture", run: |this, rest, _, cx| this.step_pr_fixture(rest, false, cx) },
    WindowVerb { verb: "pr-details", run: |this, rest, _, cx| this.step_pr_fixture(rest, true, cx) },
    WindowVerb { verb: "new-branch", run: |this, _, window, cx| this.step_new_branch(window, cx) },
    WindowVerb { verb: "amend", run: |this, rest, _, cx| this.step_changes_amend(rest, cx) },
    WindowVerb { verb: "commit", run: |this, rest, _, cx| this.step_changes_commit(rest, cx) },
    WindowVerb { verb: "branch", run: |this, rest, window, cx| this.step_changes_create_branch(rest, window, cx) },
    WindowVerb { verb: "tasks-fixture", run: |this, rest, _, cx| this.step_tasks_fixture(rest, cx) },
    WindowVerb { verb: "tasks-row", run: |this, rest, window, cx| this.step_tasks_row(rest, window, cx) },
    WindowVerb { verb: "pages-fixture", run: |this, rest, _, cx| this.step_pages_fixture(rest, cx) },
    WindowVerb { verb: "pages-row", run: |this, rest, window, cx| this.step_pages_row(rest, window, cx) },
    WindowVerb { verb: "browse-footer", run: |this, rest, window, cx| this.step_browse_footer(rest, window, cx) },
    WindowVerb { verb: "run", run: |this, _, _, cx| this.step_run(cx) },
    WindowVerb { verb: "run-stop", run: |this, _, _, cx| this.step_run_stop(cx) },
    WindowVerb { verb: "run-wait-preview", run: |this, _, _, cx| this.step_run_wait_preview(cx) },
    WindowVerb { verb: "undo-turn", run: |this, rest, _, cx| this.step_undo_turn(rest, cx) },
    WindowVerb { verb: "revert-file", run: |this, rest, _, cx| this.step_revert_file(rest, cx) },
    WindowVerb { verb: "files-reveal", run: |this, rest, _, cx| this.step_files_reveal(rest, cx) },
    WindowVerb { verb: "row-detail", run: |this, rest, _, cx| this.step_row_detail(rest, cx) },
    WindowVerb { verb: "hover", run: |this, rest, _, cx| this.step_hover(rest, cx) },
    WindowVerb { verb: "sidebar-hover", run: |this, rest, _, cx| this.step_sidebar_hover(rest, cx) },
    WindowVerb { verb: "row-menu", run: |this, rest, _, cx| this.step_row_menu(rest, cx) },
    WindowVerb { verb: "resize-begin", run: |this, rest, _, cx| this.step_resize_drag("begin", rest, cx) },
    WindowVerb { verb: "resize-move", run: |this, rest, _, cx| this.step_resize_drag("move", rest, cx) },
    WindowVerb { verb: "resize-end", run: |this, rest, _, cx| this.step_resize_drag("end", rest, cx) },
    WindowVerb { verb: "resize-sweep", run: |this, rest, window, cx| this.step_resize_sweep(rest, window, cx) },
    WindowVerb { verb: "sidebar-scroll-sweep", run: |this, rest, _, cx| this.step_sidebar_scroll_sweep(rest, cx) },
    WindowVerb {
        verb: "transcript-scroll-sweep",
        run: |this, rest, window, cx| this.step_transcript_scroll_sweep(rest, window, cx),
    },
    WindowVerb { verb: "sidebar", run: |this, _, _, cx| { this.toggle_sidebar(cx); Ok(()) } },
    // Was `peek`, which the Browse link verb above shadowed, so it never ran.
    WindowVerb { verb: "sidebar-peek", run: |this, _, _, cx| { this.capture.toggle_peek_pinned(); cx.notify(); Ok(()) } },
    WindowVerb { verb: "overflow", run: |this, _, _, cx| { this.open_menu(MenuKind::Overflow, cx); Ok(()) } },
    WindowVerb { verb: "view-menu", run: |this, _, _, cx| { this.open_menu(MenuKind::ViewOptions, cx); Ok(()) } },
    WindowVerb { verb: "account", run: |this, _, _, cx| { this.open_menu(MenuKind::Account, cx); Ok(()) } },
    WindowVerb { verb: "settings", run: |this, rest, _, cx| this.step_settings(rest, cx) },
    WindowVerb { verb: "settings-set", run: |this, rest, _, cx| this.step_settings_set(rest, cx) },
    WindowVerb { verb: "stall", run: |_, rest, _, _| step_stall(rest) },
    WindowVerb { verb: "pin", run: |this, rest, _, cx| this.step_pin(rest, cx) },
    WindowVerb { verb: "archive", run: |this, _, _, cx| this.step_archive(cx) },
    WindowVerb { verb: "error-dialog", run: |this, rest, _, cx| this.step_error_dialog(rest, cx) },
    WindowVerb { verb: "toast", run: |this, rest, _, cx| this.step_toast(rest, cx) },
    WindowVerb { verb: "archive-confirm", run: |this, _, window, cx| { this.confirm_archive_dialog(window, cx); Ok(()) } },
    WindowVerb { verb: "undo-confirm", run: |this, _, _, cx| this.step_undo_confirm(cx) },
    WindowVerb { verb: "show-archived", run: |this, _, _, cx| this.step_toggle_archived(cx) },
    WindowVerb { verb: "projects", run: |this, _, window, cx| this.step_projects(window, cx) },
    WindowVerb { verb: "project", run: |this, rest, _, cx| this.step_adopt(rest, cx) },
    WindowVerb { verb: "project-menu", run: |this, rest, _, cx| this.step_project_menu(rest, cx) },
    WindowVerb { verb: "project-colour", run: |this, rest, _, cx| this.step_project_colour(rest, cx) },
    WindowVerb { verb: "group-by", run: |this, rest, _, cx| this.step_group_by(rest, cx) },
    WindowVerb { verb: "title-pending", run: |this, _, _, cx| this.step_title_pending(cx) },
    WindowVerb { verb: "title-timeout", run: |this, _, _, cx| this.step_title_timeout(cx) },
    WindowVerb { verb: "title-land", run: |this, rest, _, cx| this.step_title_land(rest, cx) },
    WindowVerb { verb: "auto-title", run: |this, _, _, cx| this.step_auto_title(cx) },
    WindowVerb { verb: "auto-summary", run: |this, _, _, cx| this.step_auto_summary(cx) },
    WindowVerb { verb: "group-bar", run: |this, _, _, cx| this.step_group_bar(cx) },
    WindowVerb { verb: "project-close", run: |this, rest, _, cx| this.step_project_close(rest, cx) },
    WindowVerb { verb: "needs-you-filter", run: |this, rest, _, cx| this.step_needs_you_filter(rest, cx) },
    WindowVerb { verb: "row-needs-you", run: |this, rest, _, cx| this.step_row_needs_you(rest, cx) },
    WindowVerb { verb: "group-branch", run: |this, _, _, cx| this.step_group_branch(cx) },
    WindowVerb { verb: "remove-project", run: |this, rest, _, cx| this.step_remove_project(rest, cx) },
    WindowVerb { verb: "remove-confirm", run: |this, _, _, cx| this.step_remove_confirm(cx) },
    WindowVerb { verb: "new", run: |this, rest, window, cx| this.step_new(rest, window, cx) },
    WindowVerb { verb: "new-in", run: |this, rest, window, cx| this.step_new_in(rest, window, cx) },
    WindowVerb { verb: "worktree", run: |this, rest, _, cx| this.step_worktree(rest, cx) },
    WindowVerb { verb: "worktree-default", run: |this, rest, _, cx| this.step_worktree_default(rest, cx) },
    WindowVerb { verb: "unarchive", run: |this, rest, _, cx| this.step_unarchive(rest, cx) },
    WindowVerb { verb: "move-back", run: |this, _, _, cx| this.step_move_back(cx) },
    WindowVerb { verb: "terminal-dock", run: |this, rest, window, cx| this.step_terminal_dock(rest, window, cx) },
    WindowVerb { verb: "wheel", run: |this, rest, window, cx| this.step_wheel(rest, window, cx) },
    WindowVerb { verb: "sidebar-wheel", run: |this, rest, window, cx| this.step_sidebar_wheel(rest, window, cx) },
    WindowVerb { verb: "pointer", run: |this, rest, window, cx| this.step_pointer(rest, window, cx) },
    WindowVerb { verb: "page-eval", run: |this, rest, _, cx| {
        let state = this.browse_centre_state(cx).ok_or("no page is open")?;
        state.update(cx, |state, _| state.eval_with_result(u64::MAX, rest));
        Ok(())
    } },
    WindowVerb { verb: "site-info", run: |this, _, _, cx| {
        let open = this.browse_centre_state(cx).is_some();
        this.overlays.update(cx, |o, cx| o.set(crate::overlay_stack::Layer::SiteInfo, open, cx));
        if this.overlays.read(cx).stack.is_open(crate::overlay_stack::LayerKind::SiteInfo) { Ok(()) } else { Err("no page is open".into()) }
    } },
    WindowVerb { verb: "centre", run: |this, _, _, _| this.step_centre() },
    WindowVerb { verb: "skills", run: |this, rest, window, cx| this.step_skills(rest, window, cx) },
    WindowVerb {
        verb: "transcript",
        run: |this, rest, window, cx| this.step_transcript_fold(rest, window, cx),
    },
    WindowVerb { verb: "key", run: |this, rest, _, _| this.step_key(rest) },
    WindowVerb { verb: "page-focus", run: |this, _, window, cx| this.step_page_focus(window, cx) },
    // R3, free (no turn, no wire): the agent window over a Browse page.
    // `agent-line` / `agent-card` / `agent-column` toggle as ⌘J / ⌘E / ⌘⇧E
    // do; `browser-chat-fixture:<answer|steps|draft>` makes a read-only
    // browser chat with a canned transcript and points the line at it;
    // `handoff-picker` opens the card's picker.
    WindowVerb { verb: "agent-line", run: |this, _, window, cx| { this.toggle_agent_line(window, cx); Ok(()) } },
    WindowVerb { verb: "agent-card", run: |this, _, _, cx| { this.toggle_agent_card(cx); Ok(()) } },
    WindowVerb { verb: "agent-column", run: |this, _, _, cx| { this.toggle_agent_column(cx); Ok(()) } },
    WindowVerb { verb: "browser-chat-fixture", run: |this, rest, window, cx| this.step_browser_chat_fixture(rest, window, cx) },
    WindowVerb { verb: "handoff-picker", run: |this, _, _, cx| { this.overlays.update(cx, |o, cx| o.push(crate::overlay_stack::Entry::new(crate::overlay_stack::Layer::HandoffPicker), cx)); Ok(()) } },
    // Chromium's theme-change notification on every page window, as macOS
    // sends on an appearance or display change: the page ground must stay
    // white through it (owner 9 Nov: dark margins on Hacker News). Free.
    WindowVerb { verb: "page-theme-changed", run: |_, _, _, cx| {
        #[cfg(feature = "cef")]
        aui_cef::notify_theme_changed();
        cx.notify();
        Ok(())
    } },
    // D5 providers: the setup sheet (`setup:<provider>:<install|signin|update>`)
    // and the connect screen's footer menu. Both run on the connect screen too.
    WindowVerb { verb: "setup", run: |this, rest, window, cx| this.step_setup(rest, window, cx) },
    WindowVerb { verb: "connect-menu", run: |this, _, _, cx| this.step_connect_menu(cx) },
    WindowVerb { verb: "codex-login", run: |this, rest, _, cx| this.provider_model.update(cx, |model, cx| model.step_codex_login(rest, cx)) },
    WindowVerb { verb: "muse-sheet", run: |this, _, _, cx| { this.open_muse_sheet(cx); Ok(()) } },
    WindowVerb { verb: "launch-failed", run: |this, rest, _, cx| this.step_launch_failed(rest, cx) },
    // P7 import (docs/72): the sheet, a folder's box, the run, the Undo.
    WindowVerb { verb: "import-open", run: |this, _, _, cx| this.step_import_open(cx) },
    WindowVerb { verb: "import-toggle", run: |this, rest, _, cx| this.step_import_toggle(rest, cx) },
    WindowVerb { verb: "import-run", run: |this, rest, _, cx| this.step_import_run(rest, cx) },
    WindowVerb { verb: "import-undo", run: |this, _, _, cx| this.step_import_undo(cx) },
    WindowVerb {
        verb: "child-lost",
        run: |this, _, _, cx| {
            // The open lane hears its child die, as its adapter would say
            // it (docs/71 §2): no child, no turn. No session records a
            // step failure.
            let view = this.active.clone().ok_or("no open session")?;
            view.update(cx, |view, cx| view.runtime().clone().update(cx, |rt, cx| rt.step_child_lost(cx)))
        },
    },
    WindowVerb {
        verb: "signed-out",
        run: |this, _, _, cx| {
            // The banner a typed auth failure raises, on the open session's
            // provider; no session records a step failure.
            let id = this.active.as_ref().map(|view| view.read(cx).rt(cx).provider_kind()).ok_or("no open session")?;
            this.provider_model.update(cx, |model, cx| model.raise_typed_signed_out(id, cx));
            Ok(())
        },
    },
    WindowVerb {
        verb: "diagnostics",
        run: |_, rest, _, cx| {
            // Captures and live checks only: the text "Copy diagnostics"
            // would put on the clipboard, printed to stderr instead.
            let id = crate::providers::ProviderId::parse_known(rest).ok_or_else(|| format!("no provider `{rest}`"))?;
            let status = crate::provider_status::live_statuses()
                .into_iter()
                .find(|status| status.provider == id)
                .unwrap_or_else(|| crate::provider_status::ProviderStatus::checking(id));
            gpui::AppContext::background_spawn(cx, async move { eprintln!("baaz: diagnostics ↓\n{}", crate::details::collect(id, &status)) })
                .detach();
            Ok(())
        },
    },
    WindowVerb {
        verb: "details",
        run: |this, rest, _, cx| {
            let id = crate::providers::ProviderId::parse_known(rest).ok_or_else(|| format!("no provider `{rest}`"))?;
            this.settings_page.update(cx, |page, cx| page.open_details(id, cx));
            Ok(())
        },
    },
    // A card button as its click delivers it, for live checks of the
    // sign-in/out round trip on throwaway logins. `signout` only raises
    // the confirm; `signout-confirm` presses its Sign out.
    WindowVerb { verb: "provider-action", run: |this, rest, window, cx| this.step_provider_action(rest, window, cx) },
    WindowVerb {
        verb: "signout-confirm",
        run: |this, _, window, cx| {
            let target = this
                .overlays
                .read(cx)
                .dialog()
                .filter(|dialog| dialog.action == crate::overlays::DialogAction::ProviderSignOut)
                .and_then(|dialog| dialog.archive_target.clone());
            let wire = target.ok_or("no sign-out confirm is open")?;
            let id = crate::providers::ProviderId::parse_known(&wire).ok_or_else(|| format!("no provider `{wire}`"))?;
            this.confirm_provider_signout(id, window, cx);
            Ok(())
        },
    },
    WindowVerb {
        verb: "crumb",
        run: |this, rest, _, cx| {
            let target = rest
                .parse::<usize>()
                .ok()
                .and_then(|index| crate::settings::settings_crumb_target(&this.settings_section(cx), index));
            let target = target.ok_or_else(|| format!("no crumb `{rest}` on this Settings page"))?;
            this.open_settings_page(target, cx);
            Ok(())
        },
    },
];

impl Harness {
    /// `provider-action:<provider>:<recheck|signin|signout>`: the Settings
    /// card's button, through the same intent handler its click reaches.
    /// An unknown provider or action records a step failure.
    /// `launch-failed:<provider>[:<kind>]`: the typed failure view with
    /// sample words — what a capture of docs/71 §3 needs without a child.
    pub(crate) fn step_launch_failed(&mut self, rest: &str, cx: &mut Context<Self>) -> StepResult {
        use crate::provider_status::FailureKind;
        let (wire, kind) = rest.split_once(':').unwrap_or((rest, "crashed"));
        let provider = crate::providers::ProviderId::parse_known(wire).ok_or_else(|| format!("no provider `{wire}`"))?;
        let kind = match kind {
            "crashed" => FailureKind::Crashed,
            "not-installed" => FailureKind::NotInstalled,
            "signed-out" => FailureKind::SignedOut,
            "too-old" => FailureKind::TooOld,
            other => return Err(format!("no failure kind `{other}`")),
        };
        self.active = None;
        self.queue_title(cx);
        self.open_state.provider_open_error = Some(crate::app::LaunchFailure {
            session_id: None,
            provider,
            kind,
            message: "provider unavailable: the agent process exited before answering".into(),
            stderr_excerpt: vec![
                "Error: failed to load config: ~/.config/sample/config.toml".into(),
                "  caused by: unknown field `sample_key` at line 3, column 1".into(),
                "the sample process exited with status 1".into(),
            ],
            retry: crate::app::ProviderOpenRetry::OpenNew { project: None, workspace: self.workspace(cx), draft: String::new() },
        });
        cx.notify();
        Ok(())
    }

    pub(crate) fn step_provider_action(&mut self, rest: &str, window: &mut gpui::Window, cx: &mut Context<Self>) -> StepResult {
        use aui::screens::{ProviderAction, ProviderIntent};
        let (wire, action) = rest.split_once(':').ok_or("expected <provider>:<action>")?;
        let action = match action {
            "recheck" => ProviderAction::Recheck,
            "signin" => ProviderAction::SignIn,
            "signout" => ProviderAction::SignOut,
            other => return Err(format!("no provider action `{other}`")),
        };
        if crate::providers::ProviderId::parse_known(wire).is_none() {
            return Err(format!("no provider `{wire}`"));
        }
        self.handle_provider_intent(ProviderIntent { id: wire.to_owned().into(), action }, window, cx);
        Ok(())
    }

    /// `right:<browser|changes|files|terminal|tasks|off>`: open the right pane on that kind
    /// idempotently (a kind OPENS it, never toggles), so the capture shows
    /// the production pane. The pre-merge `diff`/`git` slugs still parse —
    /// onto Changes — so older scripts keep working.
    /// Empty toggles the pane through [`Harness::toggle_right`]. An
    /// unknown slug records a step failure instead of capturing a window
    /// where nothing happened (a known verb that silently does nothing
    /// reports success). Free: no turn, no wire.
    pub(crate) fn step_right(&mut self, rest: &str, cx: &mut Context<Self>) -> StepResult {
        // Refused while the Settings page stands open (B11b), like the
        // keys: a capture aid must not flip hidden pane state either.
        if self.settings_open(cx) {
            return Err("the Settings page is open".into());
        }
        let slug = rest.trim();
        if slug.is_empty() {
            self.toggle_right(cx);
            return Ok(());
        }
        // `off` closes it; a kind OPENS it, idempotently, and neither
        // writes `layout.json`. With a session active the pane state IS
        // saved to that session (per-session pane, Z2) — which is still
        // idempotent, since the verb sets a state rather than toggling one.
        // A capture entry that opens a session first therefore persists
        // into its own throwaway `BAAZ_STATE_DIR`, never across entries.
        //
        // A capture aid must not write `layout.json`. These verbs did, and
        // the entries quietly contaminated each other: running `right-git`
        // left the pane open on disk, so `signed-in` — which scripts nothing
        // at all — booted with a right pane it never asked for and reported
        // 22 changed regions. Run order decided what the suite saw.
        //
        // Deliberately not `show_right`, which toggles when asked for the
        // kind already showing — correct for ⌘K, wrong for a capture. The
        // verb also persists, so a toggling verb made captures alternate:
        // run one opened the pane and saved it, run two booted open on that
        // kind and the same step closed it again, and the probe's
        // two-captures-must-agree rule failed four entries out of five.
        if slug.eq_ignore_ascii_case("off") || slug.eq_ignore_ascii_case("closed") {
            crate::layout::Layout::stage(cx, |layout| layout.right_open = false);
            self.save_right_for_active(cx);
            return Ok(());
        }
        match crate::layout::RightKind::parse(slug) {
            Some(kind) => {
                // The model's own open: a lower pane on that kind closes,
                // and `terminal` takes the terminal into the pane.
                crate::layout::Layout::stage(cx, |layout| layout.open_top(kind));
                self.save_right_for_active(cx);
                // Outside a capture the pane reads its data in the background
                // when it paints; a run on a locked screen never paints, so
                // the read starts here (a later `wait:` lets it land).
                if !crate::runtime::captures() {
                    self.with_right_pane(cx, |pane, cx| pane.refresh_now(cx));
                }
                Ok(())
            }
            None => {
                let known: Vec<&str> = crate::layout::RightKind::ALL.iter().map(|kind| kind.slug()).collect();
                Err(format!("unknown right pane kind `{rest}`; known kinds are {}", known.join(", ")))
            }
        }
    }

    /// `files-reveal:<path>`: reveal the project-relative file or folder
    /// `path` in the Files pane through the link's own path (B9) — a
    /// folder selects with every ancestor expanded and no preview, a file
    /// previews banded on the optional `#L…`/`:line` suffix the payload
    /// carries. Empty or project-less fails, free.
    pub(crate) fn step_files_reveal(&mut self, rest: &str, cx: &mut Context<Self>) -> StepResult {
        // Refused while the Settings page stands open (B11b): it opens the
        // pane on Files, a pane-kind command.
        if self.settings_open(cx) {
            return Err("the Settings page is open".into());
        }
        let raw = rest.trim();
        if raw.is_empty() {
            return Err("no path".into());
        }
        let (root, _) = self.right_project(cx).ok_or("no current project")?;
        let highlight = crate::session::render::parse_link_lines(raw);
        let abs = crate::session::render::resolve_link_path(&root, raw);
        let is_dir = std::fs::metadata(&abs).ok().map(|meta| meta.is_dir());
        self.reveal_link_path(&root, &abs, is_dir, highlight, cx);
        Ok(())
    }

    /// `files-select:<path>`: preview the project-relative file `path` in
    /// the Files pane through the same preview path a tree click takes
    /// (selection and loading state at once, bytes on a background task),
    /// opening the pane on Files first. A directory toggles instead. An
    /// empty payload, or no current project, records a step failure
    /// instead of capturing a window where nothing happened. Free: no
    /// turn, no wire.
    pub(crate) fn step_files_select(&mut self, rest: &str, cx: &mut Context<Self>) -> StepResult {
        // Refused while the Settings page stands open (B11b): it opens the
        // pane on Files, a pane-kind command.
        if self.settings_open(cx) {
            return Err("the Settings page is open".into());
        }
        let id = rest.trim();
        if id.is_empty() {
            return Err("no path".into());
        }
        let (root, _) = self.right_project(cx).ok_or("no current project")?;
        crate::layout::Layout::stage(cx, |layout| layout.open_top(crate::layout::RightKind::Files));
        self.refresh_right_now(cx);
        self.with_right_pane(cx, |pane, cx| pane.begin_file_preview_for(&root, id, cx));
        Ok(())
    }

    /// `right-width:<px>`: settle the right pane's divider at a width,
    /// clamped into the library range exactly like a released drag, so a
    /// capture never depends on whatever `layout.json` holds on the
    /// machine running the probe. An unparseable payload fails the step.
    /// Free: no turn, no wire.
    pub(crate) fn step_right_width(&mut self, rest: &str, cx: &mut Context<Self>) -> StepResult {
        let width = rest.trim().parse::<f32>().map_err(|_| format!("`{rest}` is not a width"))?;
        {
            let width = aui::shell::clamp_right_width(width);
            // BOTH, and the second one is the one that renders. The shell is
            // given the pane's `resize.width`; `layout.right_width` is only
            // what boot reads to seed it. Setting the layout field alone left
            // the verb inert for the capture, so every right-pane entry drew
            // at whatever width the person had last dragged to and persisted —
            // the baselines were pinned to user state without anyone noticing.
            crate::layout::Layout::stage(cx, |layout| layout.right_width = Some(width));
            self.right_pane.update(cx, |pane, _| pane.resize.width = width);
        }
        Ok(())
    }

    /// `right-lower:<kind|off>`: stack `kind` below the pane's top view, or
    /// close the lower pane, through the views menu's own path
    /// ([`Harness::set_right_lower`]) — a setter, so a second run lands where
    /// the first did. A kind the lower pane cannot show (the Page, the one
    /// on top, anything in Browse) fails the step instead of capturing a
    /// pane that did not change. Free.
    pub(crate) fn step_right_lower(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> StepResult {
        let slug = rest.trim();
        let kind = match slug {
            "off" => None,
            _ => Some(crate::layout::RightKind::parse(slug).ok_or_else(|| format!("unknown right pane kind `{slug}`"))?),
        };
        self.set_right_lower(kind, window, cx)
    }

    /// `right-split:<share>`: settle the lower pane's divider at `share` of
    /// the pane's height, held to 0.25–0.75 like a drag. With no lower pane
    /// showing there is no divider to settle, and the step fails. Free.
    pub(crate) fn step_right_split(&mut self, rest: &str, cx: &mut Context<Self>) -> StepResult {
        let share = rest.trim().parse::<f32>().ok().filter(|share| share.is_finite()).ok_or_else(|| format!("`{rest}` is not a share"))?;
        let live = crate::layout::Layout::get(cx);
        if !live.right_open || live.shown_lower().is_none() {
            return Err("no lower pane is open".into());
        }
        self.set_right_split(share, cx);
        Ok(())
    }

    /// `terminal-place:<bottom|pane>`: move the terminal there through its
    /// header control's path ([`Harness::move_terminal`]). Idempotent: the
    /// place is set, and a terminal already showing there stays. Fails on
    /// an unknown place, and while the Settings page stands open (B11b),
    /// where the move would change hidden state. Free.
    pub(crate) fn step_terminal_place(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> StepResult {
        let place = crate::layout::TerminalPlace::parse(rest.trim()).ok_or_else(|| format!("`{rest}` is not bottom or pane"))?;
        if self.settings_open(cx) {
            return Err("the Settings page is open".into());
        }
        self.move_terminal(place, window, cx);
        if crate::layout::Layout::get(cx).terminal_place != place || !crate::layout::Layout::get(cx).terminal_shown() {
            return Err(format!("the terminal did not move to {}", rest.trim()));
        }
        Ok(())
    }

    /// `mode:<sessions|browse>`: set the window's mode, idempotently —
    /// asking for the current mode sets nothing and never toggles, so a
    /// capture script lands where it says on every run. Entering Browse
    /// with no person tab opens one blank tab, like the switch does. An
    /// unknown payload records a step failure instead of capturing a
    /// window where nothing happened. Free: no turn, no wire.
    pub(crate) fn step_mode(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> StepResult {
        // A bare `mode` is the session verb this window verb shadows: it
        // opens the composer's approval-mode picker.
        if rest.trim().is_empty() {
            if let Some(view) = self.active.clone() {
                view.update(cx, |view, cx| view.toggle_picker(MenuKind::Mode, cx));
                return Ok(());
            }
        }
        match crate::layout::Mode::parse(rest.trim()) {
            Some(mode) => {
                self.set_mode(mode, window, cx);
                Ok(())
            }
            None => Err(format!(
                "unknown mode `{rest}`; known modes are {}, {}",
                crate::layout::Mode::Sessions.slug(),
                crate::layout::Mode::Browse.slug()
            )),
        }
    }

    /// `key:<keystroke>`: post a real keypress for that keystroke — down and
    /// up, through CoreGraphics into Baaz's own pid, so it travels through
    /// AppKit to whatever window is key exactly like a person's keypress. An
    /// unparseable or unsupported keystroke records a step failure instead of
    /// pressing nothing. Free: no turn, no wire.
    pub(crate) fn step_key(&mut self, rest: &str) -> StepResult {
        let (code, flags) = key_event_parts(rest).ok_or_else(|| format!("unknown keystroke `{rest}`"))?;
        post_key_event(code, flags);
        Ok(())
    }
}

/// Parse a `gpui::Keystroke::parse` keystroke into a macOS virtual-key code
/// plus `CGEvent` flags: the modifiers plus every key
/// `aui_cef::keystroke_of` produces (letters, digits, punctuation, enter,
/// escape, tab, backspace, delete, space, arrows, home/end/page-up/page-down,
/// f1–f12). Pure, so the unit tests pin it without posting anything.
pub(crate) fn key_event_parts(keystroke: &str) -> Option<(u16, u64)> {
    let parsed = crate::keymap::parse_keystroke(keystroke.trim()).ok()?;
    let mut flags: u64 = 0;
    if parsed.modifiers.control {
        flags |= 1 << 18;
    }
    if parsed.modifiers.alt {
        flags |= 1 << 19;
    }
    if parsed.modifiers.shift {
        flags |= 1 << 17;
    }
    if parsed.modifiers.platform {
        flags |= 1 << 20;
    }
    if parsed.modifiers.function {
        flags |= 1 << 23;
    }
    mac_key_code(&parsed.key).map(|code| (code, flags))
}

/// The macOS virtual-key code for a parsed key name (already lowercased by
/// [`gpui::Keystroke::parse`): letters, digits, punctuation and the named
/// keys above. `None` is "no such key" — the step fails rather than pressing
/// a neighbour.
fn mac_key_code(key: &str) -> Option<u16> {
    let code = match key {
        "a" => 0,
        "s" => 1,
        "d" => 2,
        "f" => 3,
        "h" => 4,
        "g" => 5,
        "z" => 6,
        "x" => 7,
        "c" => 8,
        "v" => 9,
        "b" => 11,
        "q" => 12,
        "w" => 13,
        "e" => 14,
        "r" => 15,
        "y" => 16,
        "t" => 17,
        "1" => 18,
        "2" => 19,
        "3" => 20,
        "4" => 21,
        "6" => 22,
        "5" => 23,
        "=" => 24,
        "9" => 25,
        "7" => 26,
        "-" => 27,
        "8" => 28,
        "0" => 29,
        "]" => 30,
        "o" => 31,
        "u" => 32,
        "[" => 33,
        "i" => 34,
        "p" => 35,
        "enter" => 36,
        "l" => 37,
        "j" => 38,
        "'" => 39,
        "k" => 40,
        ";" => 41,
        "\\" => 42,
        "," => 43,
        "/" => 44,
        "n" => 45,
        "m" => 46,
        "." => 47,
        "tab" => 48,
        "space" => 49,
        "`" => 50,
        "backspace" => 51,
        "escape" => 53,
        "f5" => 96,
        "f6" => 97,
        "f7" => 98,
        "f3" => 99,
        "f8" => 100,
        "f9" => 101,
        "f11" => 103,
        "f10" => 109,
        "f12" => 111,
        "home" => 115,
        "pageup" => 116,
        "delete" => 117,
        "f4" => 118,
        "end" => 119,
        "f2" => 120,
        "pagedown" => 121,
        "f1" => 122,
        "left" => 123,
        "right" => 124,
        "down" => 125,
        "up" => 126,
        _ => return None,
    };
    Some(code)
}

#[link(name = "CoreGraphics", kind = "framework")]
#[cfg(target_os = "macos")]
extern "C" {
    fn CGEventCreateKeyboardEvent(
        source: *const std::ffi::c_void,
        virtual_key: u16,
        key_down: bool,
    ) -> *mut std::ffi::c_void;
    fn CGEventSetFlags(event: *mut std::ffi::c_void, flags: u64);
    fn CGEventPostToPid(pid: i32, event: *mut std::ffi::c_void);
}

#[link(name = "CoreFoundation", kind = "framework")]
#[cfg(target_os = "macos")]
extern "C" {
    fn CFRelease(cf: *const std::ffi::c_void);
}

/// Post a down-and-up keypress for a virtual-key code with flags, into
/// Baaz's own pid — no new crate, just the framework. macOS only; elsewhere
/// the `key:` step logs and presses nothing.
#[cfg(target_os = "macos")]
fn post_key_event(code: u16, flags: u64) {
    unsafe {
        let pid = libc::getpid();
        for key_down in [true, false] {
            let event = CGEventCreateKeyboardEvent(std::ptr::null(), code, key_down);
            if event.is_null() {
                crate::baaz_log!("key: CoreGraphics refused virtual-key {code}");
                return;
            }
            CGEventSetFlags(event, flags);
            CGEventPostToPid(pid, event);
            CFRelease(event);
        }
    }
}

/// Nowhere to post outside macOS: the step logs and presses nothing.
#[cfg(not(target_os = "macos"))]
fn post_key_event(code: u16, _flags: u64) {
    crate::baaz_log!("key: no CoreGraphics here; not posting virtual-key {code}");
}

/// The open session's `--steps` verbs.
pub(crate) const SESSION_VERBS: &[SessionVerb] = &[
    SessionVerb { verb: "draft", run: |v, rest, window, cx| { v.set_draft(rest.to_owned(), window, cx); Ok(()) } },
    // ⌘F's bar on the transcript (docs/82 7.5): `transcript-find:<query>`
    // types the query (opening the bar); fails on an empty query or when
    // nothing matched. Next / previous fail with no bar or no match; close
    // fails with no bar.
    SessionVerb {
        verb: "transcript-find",
        run: |v, rest, window, cx| match v.step_find(rest, window, cx)? {
            label if label == "No matches" => Err(format!("transcript-find: no match for `{rest}`")),
            _ => Ok(()),
        },
    },
    SessionVerb { verb: "transcript-find-next", run: |v, _, _, cx| if v.find_step(true, cx) { Ok(()) } else { Err("transcript-find-next: no bar or no match".into()) } },
    SessionVerb { verb: "transcript-find-prev", run: |v, _, _, cx| if v.find_step(false, cx) { Ok(()) } else { Err("transcript-find-prev: no bar or no match".into()) } },
    SessionVerb { verb: "transcript-find-close", run: |v, _, window, cx| if v.close_find(window, cx) { Ok(()) } else { Err("transcript-find-close: no bar open".into()) } },
    SessionVerb { verb: "send", run: |v, rest, _, cx| v.runtime().clone().update(cx, |rt, cx| rt.step_send(rest, cx)) },
    // The billing banner's "Send anyway", as a verb: sends what the guard
    // held back — **costs a turn** when there is anything to send.
    SessionVerb { verb: "send-anyway", run: |v, _, window, cx| v.step_send_anyway(window, cx) },
    SessionVerb { verb: "steer", run: |v, rest, _, cx| v.runtime().clone().update(cx, |rt, cx| rt.step_steer(rest, cx)) },
    SessionVerb { verb: "model", run: |v, _, _, cx| { v.toggle_picker(MenuKind::Model, cx); Ok(()) } },
    SessionVerb { verb: "effort", run: |v, _, _, cx| { v.toggle_picker(MenuKind::Effort, cx); Ok(()) } },
    SessionVerb { verb: "fixture-models", run: |v, _, _, cx| v.runtime().clone().update(cx, |rt, cx| rt.step_fixture_models(cx)) },
    SessionVerb { verb: "submenu", run: |v, _, _, cx| v.step_submenu(cx) },
    SessionVerb { verb: "provider", run: |v, _, _, cx| { v.toggle_picker(MenuKind::Provider, cx); Ok(()) } },
    SessionVerb { verb: "mode", run: |v, _, _, cx| { v.toggle_picker(MenuKind::Mode, cx); Ok(()) } },
    // The new-session row's folder picker (docs/82 6.6), opened as its
    // control does; fails on a session that shows no row.
    SessionVerb { verb: "folder", run: |v, _, _, cx| v.step_folder(cx) },
    SessionVerb { verb: "confirm", run: |v, _, window, cx| v.step_confirm(window, cx) },
    // The session operations, so their screenshots come from a command
    // line rather than from a pointer.
    SessionVerb {
        verb: "name",
        run: |v, rest, window, cx| v.step_name(rest, window, cx),
    },
    SessionVerb { verb: "hide", run: |_, _, _, cx| { cx.emit(SessionEvent::Hide); Ok(()) } },
    SessionVerb { verb: "resume", run: |_, _, _, cx| { cx.emit(SessionEvent::Resume); Ok(()) } },
    SessionVerb { verb: "setmodel", run: |v, rest, _, cx| v.runtime().clone().update(cx, |rt, cx| rt.step_setmodel(rest, cx)) },
    SessionVerb { verb: "setprovider", run: |v, rest, _, cx| v.step_setprovider(rest, cx) },
    // `handoff:<provider>`: start the lossy re-prompt to that backend,
    // headless — the command line already asked, so no confirm dialog.
    // Refusals (no turns, a pending question or approval, a same-provider
    // pick) record a step failure and change nothing.
    SessionVerb { verb: "handoff", run: |v, rest, _, cx| v.runtime().clone().update(cx, |rt, cx| rt.step_handoff(rest, cx)) },
    SessionVerb { verb: "seteffort", run: |v, rest, _, cx| v.step_seteffort(rest, cx) },
    SessionVerb { verb: "compact", run: |v, _, _, cx| v.runtime().clone().update(cx, |rt, cx| rt.step_compact(cx)) },
    SessionVerb { verb: "meter", run: |v, _, _, cx| v.step_meter(cx) },
    SessionVerb { verb: "context", run: |v, rest, _, cx| v.runtime().clone().update(cx, |rt, cx| rt.step_context(rest, cx)) },
    // The Plan toggle, on: a provider with no plan mode has no toggle
    // (docs/82 5.5), so the step fails rather than doing nothing.
    SessionVerb { verb: "plan", run: |v, _, _, cx| {
        if v.rt(cx).plan_support() == provider::PlanMode::None {
            return Err("this session's provider has no plan mode".into());
        }
        v.runtime().clone().update(cx, |rt, cx| rt.set_plan(true, cx));
        Ok(())
    } },
    SessionVerb { verb: "image", run: |v, rest, _, cx| v.runtime().clone().update(cx, |rt, cx| rt.step_attach(rest, cx)) },
    SessionVerb { verb: "file", run: |v, rest, _, cx| v.runtime().clone().update(cx, |rt, cx| rt.step_attach(rest, cx)) },
    SessionVerb { verb: "plus", run: |v, _, _, cx| v.step_plus(cx) },
    SessionVerb { verb: "drop", run: |v, _, _, cx| v.step_drop(cx) },
    SessionVerb { verb: "command", run: |v, rest, window, cx| v.step_caret_menu("/", rest, window, cx) },
    SessionVerb { verb: "mention", run: |v, rest, window, cx| v.step_caret_menu("@", rest, window, cx) },
    // `session/userShell`: free on every provider, and the only way to raise a
    // real approval without spending a turn.
    SessionVerb { verb: "shell", run: |v, rest, _, cx| v.runtime().clone().update(cx, |rt, cx| rt.step_shell(rest, cx)) },
    SessionVerb { verb: "setmode", run: |v, rest, _, cx| v.runtime().clone().update(cx, |rt, cx| rt.step_setmode(rest, cx)) },
    SessionVerb { verb: "choose", run: |v, rest, window, cx| v.step_choose(rest, window, cx) },
    SessionVerb { verb: "feedback", run: |v, rest, window, cx| v.step_feedback(rest, window, cx) },
    SessionVerb { verb: "answer", run: |v, rest, _, cx| v.step_answer(rest, cx) },
    SessionVerb { verb: "answers", run: |v, rest, _, cx| v.step_answer(rest, cx) },
    SessionVerb { verb: "clarify", run: |v, rest, window, cx| v.step_clarify(rest, window, cx) },
    SessionVerb { verb: "preview", run: |v, rest, _, cx| v.step_preview(rest, cx) },
    SessionVerb { verb: "select", run: |v, rest, _, cx| v.step_select(rest, cx) },
    SessionVerb { verb: "skip", run: |v, _, _, cx| v.step_skip(cx) },
    // Transcript inspection: jump without touching the pointer, hold a
    // selection, and open every tool group for its screenshot.
    SessionVerb { verb: "select-text", run: |v, rest, _, cx| v.step_select_text(rest, cx) },
    SessionVerb { verb: "select-span", run: |v, rest, _, cx| v.step_select_span(rest, cx) },
    SessionVerb { verb: "quote", run: |v, _, window, cx| v.step_quote(window, cx) },
    SessionVerb { verb: "copy", run: |v, rest, window, cx| v.step_copy(rest, window, cx) },
    SessionVerb { verb: "top", run: |v, _, _, cx| v.step_top(cx) },
    SessionVerb { verb: "end", run: |v, _, _, cx| v.step_end(cx) },
    SessionVerb { verb: "mid", run: |v, _, _, cx| v.step_mid(cx) },
    SessionVerb { verb: "expand-groups", run: |v, _, _, cx| { v.expand_all_groups(cx); Ok(()) } },
    SessionVerb { verb: "bench", run: |v, rest, _, cx| v.step_bench(rest, cx) },
    SessionVerb { verb: "fork", run: |v, _, _, cx| v.runtime().clone().update(cx, |rt, cx| rt.step_fork(cx)) },
    SessionVerb { verb: "retry", run: |v, _, _, cx| v.runtime().clone().update(cx, |rt, cx| rt.step_retry(cx)) },
    // `wait` is handled by the runners, which are the only thing that can let
    // the wire catch up; reaching a table means it slipped through.
    SessionVerb { verb: "wait", run: |_, _, _, _| Err("`wait` belongs to the runner".into()) },
];

/// The `--login-steps` verbs.
pub(crate) const LOGIN_VERBS: &[LoginVerb] = &[
    LoginVerb {
        verb: "account",
        run: |this, _, window, cx| {
            this.login_intent(LoginIntent::StartAccount, window, cx);
            true
        },
    },
    LoginVerb {
        verb: "apikey",
        run: |this, _, window, cx| {
            this.login_intent(LoginIntent::UseApiKey, window, cx);
            true
        },
    },
    LoginVerb { verb: "key-from-env", run: |this, rest, window, cx| this.step_key_from_env(rest, window, cx) },
    LoginVerb {
        verb: "submit",
        run: |this, _, window, cx| {
            this.login_intent(LoginIntent::SubmitApiKey, window, cx);
            true
        },
    },
    // As above: the runner owns `wait:<ms>`.
    LoginVerb { verb: "wait", run: |_, _, _, _| true },
];

/// How often a scripted `image:` step asks whether the background decode has
/// landed, and how many times before it gives up (finding `performance-14`).
const ATTACH_POLL: std::time::Duration = std::time::Duration::from_millis(20);
/// 100 × 20 ms = two seconds, which is far past any image under the 10 MB cap.
const ATTACH_WAIT_POLLS: usize = 100;

/// One `--steps` item, tried against the window's verbs.
///
/// `None` when it is not one of them — it goes on to [`session_step`],
/// exactly as the two matches used to be tried in turn — else the verb's
/// own result.
pub(crate) fn window_step(this: &mut Harness, step: &str, window: &mut Window, cx: &mut Context<Harness>) -> Option<StepResult> {
    let (head, rest) = split(step);
    let verb = WINDOW_VERBS.iter().find(|v| v.verb == head)?;
    crate::log::crumb(verb.verb);
    let result = (verb.run)(this, rest, window, cx);
    cx.notify();
    Some(result)
}

/// One `--steps` item, run where it belongs: the window's verbs first,
/// then the open session's. A session verb with no session to run on, or
/// while a switch is still landing, fails rather than vanishing.
pub(crate) fn run_one_step(this: &mut Harness, step: &str, window: &mut Window, cx: &mut Context<Harness>) -> StepResult {
    if let Some(result) = window_step(this, step, window, cx) {
        return result;
    }
    if this.pending_switch_dest(cx).is_some() {
        return Err("a session switch is still landing".into());
    }
    let view = this.active.clone().ok_or("no open session")?;
    view.update(cx, |view, cx| session_step(view, step, window, cx))
}

/// `stall:<ms>`: hold the UI thread, so a live run can watch the stall
/// watchdog write (see [`crate::watchdog`]). Capped at 10 s.
fn step_stall(rest: &str) -> StepResult {
    let (ms, busy) = match rest.trim().strip_suffix(",busy") {
        Some(ms) => (ms, true),
        None => (rest.trim(), false),
    };
    let ms = ms.parse::<u64>().map_err(|_| format!("`{rest}` is not milliseconds"))?;
    let until = std::time::Instant::now() + std::time::Duration::from_millis(ms.min(10_000));
    if busy {
        // On CPU the whole time, as a runaway loop would be.
        while std::time::Instant::now() < until {
            std::hint::spin_loop();
        }
    } else {
        std::thread::sleep(until.saturating_duration_since(std::time::Instant::now()));
    }
    Ok(())
}

/// One `--steps` item, run against the open session.
pub(crate) fn session_step(view: &mut SessionView, step: &str, window: &mut Window, cx: &mut Context<SessionView>) -> StepResult {
    let (head, rest) = split(step);
    let verb = SESSION_VERBS.iter().find(|v| v.verb == head).ok_or_else(|| format!("unknown step `{head}`"))?;
    (verb.run)(view, rest, window, cx)
}

/// Whether a `--steps` item runs against the open session (as opposed to
/// the window): mirrors [`window_step`]'s lookup without running anything.
/// Whether every step in `steps` is a window verb — nothing that needs an
/// open session. `wait:` counts as window-only: it touches nothing.
///
/// This is what lets an offline capture script the window with no session
/// open. See [`crate::app::sessions::steps_ready_for`].
pub(crate) fn all_window_steps(steps: &[String]) -> bool {
    !steps.is_empty()
        && steps.iter().all(|step| {
            if step.strip_prefix("wait:").is_some() {
                return true;
            }
            let (head, _) = split(step);
            WINDOW_VERBS.iter().any(|v| v.verb == head)
        })
}

/// Whether a `--steps` script needs a session opened for it at boot: any
/// script but one that only touches the window. A window-only script runs
/// with no session (see `steps_ready_for`), and opening one anyway left an
/// empty draft that an offline run never loads: the live check's centre
/// went blank once the providers answered — the empty state, mascot and
/// all, gone (Stage 3 follow-up, 17 Nov).
pub(crate) fn steps_need_a_session(steps: &[String]) -> bool {
    !steps.is_empty() && !all_window_steps(steps)
}

fn is_session_step(step: &str) -> bool {
    let (head, _) = split(step);
    WINDOW_VERBS.iter().all(|v| v.verb != head)
}

/// Whether the script's head starts a session itself: the first step is
/// `new` (bare or `new:<project>`). The boot session must not start one
/// eagerly ahead of it — boot plus the head's own `new` opened two sessions
/// (two `provider lane open` lines, two sidebar rows) for one action.
/// See [`crate::app::sessions::boot_decision`].
pub(crate) fn steps_begin_with_new(steps: &[String]) -> bool {
    match steps.first() {
        None => false,
        Some(step) => matches!(split(step).0, "new" | "new-in"),
    }
}

/// Whether the script's head can run with no session open: a window verb
/// or `wait:`, neither of which touches the session. This is what lets a
/// mixed script start — `new;effort` begins with `new`, which opens the
/// session the later verbs need — where [`all_window_steps`] alone would
/// hold the whole list for a session its own head would create.
pub(crate) fn head_runs_without_session(steps: &[String]) -> bool {
    match steps.first() {
        None => false,
        Some(step) => {
            if step.strip_prefix("wait:").is_some() {
                return true;
            }
            let (head, _) = split(step);
            WINDOW_VERBS.iter().any(|v| v.verb == head)
        }
    }
}

/// How long session verbs wait for a `new:` switch: the `session/start`
/// round-trip is usually far under a second; the bound only fires when the
/// switch never comes, and then the step runs against whatever is open
/// (today's behaviour) with a log line.
const SWITCH_POLL: std::time::Duration = std::time::Duration::from_millis(50);
const SWITCH_WAIT_POLLS: usize = 200;

/// One `--login-steps` item. Returns whether the run continues; a failed or
/// unknown step ends it with a stderr line.
pub(crate) fn login_step(this: &mut Harness, step: &str, window: &mut Window, cx: &mut Context<Harness>) -> bool {
    let (head, rest) = split(step);
    match LOGIN_VERBS.iter().find(|v| v.verb == head) {
        Some(verb) => (verb.run)(this, rest, window, cx),
        None => {
            record_step_failure(step);
            crate::baaz_log!("unknown login step `{step}`");
            false
        }
    }
}

/// `--steps`: drive the open session from the command line.
///
/// The steps run on a task rather than in a loop, because `wait:<ms>` is the
/// only way a scripted approval round-trip can exist: a decision has to reach
/// the wire and its `approval/updated` has to come back before the next
/// `choose:` means anything.
pub(crate) fn run_steps(this: &mut Harness, cx: &mut Context<Harness>) {
    let steps = this.take_steps();
    if steps.is_empty() {
        return;
    }
    crate::baaz_log!("steps: running {} scripted steps", steps.len());
    let capture = this.capture.clone();
    capture.set_steps_running(true);
    let task = cx.spawn(async move |this, cx| {
        for step in steps {
            if let Some(ms) = step.strip_prefix("wait:") {
                STEPS_RAN.fetch_add(1, Ordering::Relaxed);
                let ms: u64 = ms.parse().unwrap_or(0);
                cx.background_executor().timer(std::time::Duration::from_millis(ms)).await;
                continue;
            }
            // A `new:` step's session opens on the `session/start`
            // round-trip, after the following steps would run: session
            // verbs wait for the switch (bounded) so `name:`/`draft:`/
            // `send:` reach the session the script meant, not the one
            // still open. Without this, `new:demo` followed at once by
            // `send:` could bill the turn on the session that was open
            // before.
            if is_session_step(&step) {
                for _ in 0..SWITCH_WAIT_POLLS {
                    let pending = this
                        .read_with(cx, |this, _| this.session_opening())
                        .unwrap_or(false);
                    if !pending {
                        break;
                    }
                    cx.background_executor().timer(SWITCH_POLL).await;
                }
                let still_pending = this
                    .read_with(cx, |this, _| this.session_opening())
                    .unwrap_or(false);
                if still_pending {
                    let active = this
                        .read_with(cx, |this, cx| this.active_id(cx))
                        .unwrap_or(None);
                    crate::baaz_log!(
                        "session switch never arrived; `{step}` runs against {active:?}"
                    );
                }
            }
            // `run-wait-preview[:<s>]` (docs/82 7.4): the step checks the
            // Page opened; the wait for the port to answer is the runner's,
            // like `wait:`, bounded (30 s unless named).
            if split(&step).0 == "run-wait-preview" {
                let secs: u64 = split(&step).1.trim().parse().unwrap_or(30);
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
                while std::time::Instant::now() < deadline && this.read_with(cx, |this, cx| this.preview_waiting(cx)).unwrap_or(false) {
                    cx.background_executor().timer(std::time::Duration::from_millis(100)).await;
                }
            }
            STEPS_RAN.fetch_add(1, Ordering::Relaxed);
            let Ok(result) = this.update_in(cx, |this, window, cx| run_one_step(this, &step, window, cx)) else {
                crate::baaz_log!("steps: `{step}` lost its window; ending script");
                capture.set_steps_running(false);
                capture.set_steps_done(true);
                return;
            };
            // The first step that did not do what it says ends the script:
            // everything after it would run against a screen the script
            // did not mean, and the capture exits non-zero on the count.
            if let Err(reason) = result {
                record_step_failure(&step);
                crate::baaz_log!("steps: `{step}` failed: {reason}; ending script");
                capture.set_steps_running(false);
                capture.set_steps_done(true);
                return;
            }
            // `image:` hands its read and decode to the background executor
            // (finding `performance-14`), so the step is not done when the
            // call returns — it is done when the chip stops being a
            // placeholder. Bounded, so a file that never decodes cannot hang
            // a scripted capture.
            for _ in 0..ATTACH_WAIT_POLLS {
                let pending = this
                    .read_with(cx, |this, cx| {
                        this.active.as_ref().is_some_and(|view| view.read(cx).rt(cx).attachments_pending())
                    })
                    .unwrap_or(false);
                if !pending {
                    break;
                }
                cx.background_executor().timer(ATTACH_POLL).await;
            }
        }
        capture.set_steps_running(false);
        capture.set_steps_done(true);
    });
    this.wire_tasks().push(task);
}

/// `--login-steps`: drive the login screen from the command line.
///
/// Raises the flags the capture waits on exactly like [`run_steps`] does, so
/// a headless `--screenshot` waits for the login script to finish before the
/// settling delay.
pub(crate) fn run_login_steps(this: &mut Harness, cx: &mut Context<Harness>) {
    let steps = this.take_login_steps();
    if steps.is_empty() {
        return;
    }
    crate::baaz_log!("login-steps: running {} scripted steps", steps.len());
    let capture = this.capture.clone();
    // The login screen is up — the list this runs against needs no boot
    // session — so readiness holds from the drain, like `maybe_run_steps`
    // raising it before [`run_steps`].
    capture.set_steps_ready(true);
    capture.set_steps_running(true);
    let task = cx.spawn(async move |this, cx| {
        for step in steps {
            STEPS_RAN.fetch_add(1, Ordering::Relaxed);
            if let Some(ms) = step.strip_prefix("wait:") {
                let ms: u64 = ms.parse().unwrap_or(0);
                cx.background_executor().timer(std::time::Duration::from_millis(ms)).await;
                continue;
            }
            // `update_in` for the window the field and the focus need.
            let ran = this.update_in(cx, |this, window, cx| login_step(this, &step, window, cx));
            match ran {
                Ok(true) => {}
                _ => {
                    crate::baaz_log!("login-steps: `{step}` failed; ending script");
                    capture.set_steps_running(false);
                    capture.set_steps_done(true);
                    return;
                }
            }
        }
        capture.set_steps_running(false);
        capture.set_steps_done(true);
    });
    this.wire_tasks().push(task);
}

#[cfg(test)]
mod tests {
    use super::{all_window_steps, LOGIN_VERBS, SESSION_VERBS, WINDOW_VERBS};
    use crate::layout::LayoutGlobal as _;
    use std::collections::BTreeSet;

    /// This very file, so the documented tables and the verb tables can be
    /// compared without a build script: the module rustdoc above is the one
    /// place `--steps` is documented, and a verb that is in the code and not
    /// in the table (or the other way round) is a verb nobody can find.
    const SOURCE: &str = include_str!("steps.rs");

    /// The verbs one `//! | `x` | … |` table documents, between two headings.
    fn documented(section: &str) -> BTreeSet<String> {
        let start = SOURCE.find(section).unwrap_or_else(|| panic!("no `{section}` heading in steps.rs"));
        let rest = &SOURCE[start + section.len()..];
        let end = rest.find("//! # ").unwrap_or(rest.len());
        rest[..end]
            .lines()
            .filter_map(|line| line.trim().strip_prefix("//! | "))
            .filter_map(|row| row.split('|').next())
            .filter_map(|cell| cell.trim().strip_prefix('`'))
            .map(|cell| {
                let cell = cell.split('`').next().unwrap_or(cell);
                cell.split(':').next().unwrap_or(cell).to_owned()
            })
            .collect()
    }

    #[test]
    fn every_steps_verb_is_documented_and_every_documented_verb_exists() {
        let known: BTreeSet<String> = WINDOW_VERBS
            .iter()
            .map(|v| v.verb.to_owned())
            .chain(SESSION_VERBS.iter().map(|v| v.verb.to_owned()))
            .collect();
        assert_eq!(documented("//! # `--steps <a;b;c>`"), known);
    }

    #[test]
    fn every_login_steps_verb_is_documented_and_every_documented_verb_exists() {
        let known: BTreeSet<String> = LOGIN_VERBS.iter().map(|v| v.verb.to_owned()).collect();
        assert_eq!(documented("//! # `--login-steps <a;b;c>`"), known);
    }

    #[test]
    fn a_window_only_script_is_recognised() {
        assert!(all_window_steps(&["right:files".into(), "right-width:400".into()]));
        assert!(all_window_steps(&["right:files".into(), "wait:100".into()]));
        // The mode switch is window-owned too: both directions, and mixed
        // with the tab verbs the Browse captures run.
        assert!(all_window_steps(&["mode:browse".into()]));
        assert!(all_window_steps(&["mode:sessions".into()]));
        assert!(all_window_steps(&["mode:browse".into(), "tab-new:https://example.com".into()]));
        assert!(super::is_known_step("mode:browse"));
        assert!(super::is_known_step("mode:sessions"));
        // One session verb is enough to need a session.
        assert!(!all_window_steps(&["right:files".into(), "draft:hello".into()]));
        // An unknown verb is not a window verb; the run must not be let
        // through on the strength of a typo.
        assert!(!all_window_steps(&["right:files".into(), "nonsense".into()]));
        assert!(!all_window_steps(&[]));
        // A window-only script opens no boot session; a session verb, or
        // `--send`'s own path, still does.
        assert!(!super::steps_need_a_session(&[]));
        assert!(!super::steps_need_a_session(&["wait:100".into(), "row-needs-you:Header spacing".into()]));
        assert!(super::steps_need_a_session(&["right:files".into(), "draft:hello".into()]));
    }

    #[test]
    fn a_step_splits_at_its_first_colon_only() {
        assert_eq!(super::split("plan"), ("plan", ""));
        assert_eq!(super::split("draft:hello"), ("draft", "hello"));
        assert_eq!(super::split("select-text:2:0-40"), ("select-text", "2:0-40"));
    }

    #[test]
    fn right_slugs_round_trip_and_unknown_is_none() {
        use crate::layout::RightKind;
        for kind in RightKind::ALL {
            assert_eq!(RightKind::parse(kind.slug()), Some(kind), "{kind:?} slug parses back");
        }
        assert_eq!(RightKind::parse("nope"), None);
        assert_eq!(RightKind::parse(""), None);
    }

    #[test]
    fn right_verbs_are_known_steps() {
        assert!(super::is_known_step("right:browser"));
        assert!(super::is_known_step("right:diff"));
        assert!(super::is_known_step("right:git"));
        assert!(super::is_known_step("right:files"));
        assert!(super::is_known_step("right-width:400"));
        assert!(super::is_known_step("right:"));
    }

    #[test]
    fn unknown_steps_classify_as_failures_and_wait_is_known() {
        assert!(super::is_known_step("palette"));
        assert!(super::is_known_step("draft:hello"));
        assert!(super::is_known_step("wait:3000"));
        assert!(!super::is_known_step("totally-bogus-verb"));
    }

    #[test]
    fn the_window_owns_new_and_the_session_owns_the_rest() {
        // The skip in `run_steps` ("no open session") keys off this: a
        // session verb must never classify as a window verb, and `wait:`
        // never reaches a table at all.
        assert!(!super::is_session_step("new"));
        assert!(!super::is_session_step("new:demo"));
        assert!(super::is_session_step("draft:hello"));
        assert!(super::is_session_step("send:hi"));
        assert!(super::is_session_step("wait:3000"));
        assert!(super::is_session_step("bogusverb"));
    }

    #[test]
    fn a_script_headed_by_new_starts_its_own_session() {
        // The boot session reads this: a list beginning with `new` starts
        // its own session, so booting one eagerly ahead of it opens two.
        assert!(super::steps_begin_with_new(&["new".into(), "setprovider:codex".into()]));
        assert!(super::steps_begin_with_new(&["new:demo".into(), "send:hi".into()]));
        assert!(super::steps_begin_with_new(&["new".into()]));
        assert!(!super::steps_begin_with_new(&["send:hi".into(), "new".into()]));
        assert!(super::steps_begin_with_new(&["new-in:/tmp/x".into()]));
        assert!(!super::steps_begin_with_new(&["provider".into()]));
        assert!(!super::steps_begin_with_new(&[]));
    }

    #[test]
    fn a_mixed_script_headed_by_a_window_verb_can_start() {
        // `new;effort` must start with no session open: `new` opens the
        // session the later verbs need. A session verb up front cannot.
        assert!(super::head_runs_without_session(&["new".into(), "effort".into()]));
        assert!(super::head_runs_without_session(&["new".into(), "model".into()]));
        assert!(super::head_runs_without_session(&["right:browser".into(), "draft:hi".into()]));
        assert!(super::head_runs_without_session(&["wait:100".into(), "effort".into()]));
        assert!(super::head_runs_without_session(&["new".into()]));
        assert!(!super::head_runs_without_session(&["effort".into(), "new".into()]));
        assert!(!super::head_runs_without_session(&["model".into()]));
        assert!(!super::head_runs_without_session(&["provider".into()]));
        assert!(!super::head_runs_without_session(&["bogusverb".into()]));
        assert!(!super::head_runs_without_session(&[]));
    }

    #[test]
    fn unrun_steps_fail_without_counting_as_ran() {
        // The capture drains this only when readiness never held, so none
        // of these executed: every one fails — a session verb that never
        // found a session most of all — and `ran` does not move. A skipped
        // step is not a run step. `wait:` touches nothing, so an unrun
        // sleep is neither.
        let ran_before = super::steps_ran();
        super::record_unrun_steps(&[
            "effort".into(),
            "right:browser".into(),
            "totally-bogus-verb".into(),
            "wait:50".into(),
        ]);
        assert_eq!(super::steps_ran(), ran_before, "unrun steps are not run steps");
        let names = super::step_failure_names();
        for failed in ["effort", "right:browser", "totally-bogus-verb"] {
            assert!(
                names.iter().any(|name| name == failed),
                "`{failed}` is recorded as a step failure"
            );
        }
        assert!(
            !names.iter().any(|name| name == "wait:50"),
            "an unrun `wait:` is not a failure"
        );
    }

    #[test]
    fn the_provider_picker_is_a_known_session_verb() {
        assert!(super::is_known_step("provider"));
        assert!(super::is_session_step("provider"));
    }

    use std::path::PathBuf;

    use crate::app::Harness;
    // `cx.new` is `AppContext`'s, and the trait has to be in scope for it.
    use gpui::AppContext as _;

    /// A real `Harness` would read and write the developer's state dir, so
    /// point `BAAZ_STATE_DIR` at a fresh temp dir for the test's duration.
    /// Mirrors the `app.rs` hermetic helper: the lock keeps two tests from
    /// sharing a dir.
    fn hermetic_state(name: &str) -> (std::sync::MutexGuard<'static, ()>, Option<std::ffi::OsString>, PathBuf) {
        let dir = std::env::temp_dir().join(format!("baaz-steps-right-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("probe state dir");
        let guard = crate::store::test_env_lock();
        let old = std::env::var_os("BAAZ_STATE_DIR");
        std::env::set_var("BAAZ_STATE_DIR", &dir);
        (guard, old, dir)
    }

    #[allow(clippy::needless_pass_by_value)]
    fn restore_state(state: (std::sync::MutexGuard<'static, ()>, Option<std::ffi::OsString>, PathBuf)) {
        let (guard, old, dir) = state;
        let _ = std::fs::remove_dir_all(&dir);
        match old {
            Some(value) => std::env::set_var("BAAZ_STATE_DIR", value),
            None => std::env::remove_var("BAAZ_STATE_DIR"),
        }
        drop(guard);
    }

    fn step_test_args(dir: &std::path::Path) -> crate::Args {
        crate::Args {
            workspace: dir.to_path_buf(),
            workspace_explicit: true,
            provider: "echo".into(),
            provider_explicit: false,
            program: "muse".into(),
            theme: Some(aui_tokens::ThemeKind::Dark),
            screenshot: None,
            delay: std::time::Duration::from_millis(500),
            session: None,
            send: None,
            offline: true,
            replay: None,
            steps: Vec::new(),
            tier: None,
            print_tier: false,
            approval_mode: None,
            login: crate::LoginSample::Choose,
            login_steps: Vec::new(),
            bench: None,
            bench_cadence: std::time::Duration::from_millis(4),
            bench_scroll: crate::bench::BenchScroll::Sweep,
            bench_frames: 600,
            bench_out: None,
            bench_open_turn: false,
            bench_bare: false,
            bench_shell: false,
            sidebar_fixture: None,
            no_project: false,
            terminal_socket_dir: Some(crate::terminal::service::test_socket_dir()),
        }
    }

    /// Every slug in `RightKind::ALL` opens the pane on that kind through
    /// `step_right`, with no session open.
    #[gpui::test]
    fn step_right_accepts_every_slug(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("slugs");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(step_test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        for kind in crate::layout::RightKind::ALL {
            vc.update(|_, cx| baaz.update(cx, |h, cx| h.step_right(kind.slug(), cx))).expect("a known kind opens");
            assert!(
                vc.update(|_, cx| crate::layout::Layout::get(cx).right_open),
                "{kind:?} opens the pane"
            );
            assert_eq!(vc.update(|_, cx| crate::layout::Layout::get(cx).right_kind), Some(kind));
        }
        restore_state(state);
    }

    /// The empty payload toggles, and an unknown slug fails the step while
    /// leaving the pane alone.
    #[gpui::test]
    fn step_right_empty_toggles_and_unknown_fails(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("toggle-fail");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(step_test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        let start = vc.update(|_, cx| crate::layout::Layout::get(cx).right_open);
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.step_right("", cx))).expect("empty toggles");
        assert_eq!(
            vc.update(|_, cx| crate::layout::Layout::get(cx).right_open),
            !start,
            "empty payload toggles the pane"
        );
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.step_right("", cx))).expect("empty toggles");
        assert_eq!(
            vc.update(|_, cx| crate::layout::Layout::get(cx).right_open),
            start,
            "empty payload toggles back"
        );
        let refused = vc.update(|_, cx| baaz.update(cx, |h, cx| h.step_right("not-a-kind", cx)));
        assert_eq!(
            vc.update(|_, cx| crate::layout::Layout::get(cx).right_open),
            start,
            "unknown slug changes nothing on screen"
        );
        assert!(
            refused.is_err_and(|reason| reason.contains("not-a-kind")),
            "unknown slug fails the step"
        );
        restore_state(state);
    }

    /// docs/82 8.1: `right-lower:` and `terminal-place:` are setters (a
    /// second run lands where the first did), `right-split:` needs a lower
    /// pane, and every refusal changes nothing.
    #[gpui::test]
    fn the_lower_pane_and_place_verbs_are_idempotent_and_fail_loudly(cx: &mut gpui::TestAppContext) {
        use crate::layout::{RightKind, TerminalPlace};
        let state = hermetic_state("lower-verbs");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(step_test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        let layout = |vc: &mut gpui::VisualTestContext| vc.update(|_, cx| crate::layout::Layout::get(cx).clone());
        let split = vc.update(|_, cx| baaz.update(cx, |h, cx| h.step_right_split("0.4", cx)));
        assert!(split.is_err(), "no lower pane, no divider: {split:?}");
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.step_right("changes", cx))).expect("Changes on top");
        for _ in 0..2 {
            vc.update(|window, cx| baaz.update(cx, |h, cx| h.step_right_lower("files", window, cx))).expect("Files below");
            assert_eq!(layout(vc).shown_lower(), Some(RightKind::Files));
        }
        for refused in ["browser", "changes", "nope"] {
            let result = vc.update(|window, cx| baaz.update(cx, |h, cx| h.step_right_lower(refused, window, cx)));
            assert!(result.is_err(), "`{refused}` below must fail");
            assert_eq!(layout(vc).shown_lower(), Some(RightKind::Files), "and change nothing");
        }
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.step_right_split("0.3", cx))).expect("a lower pane: the divider settles");
        assert_eq!(layout(vc).split(), 0.3);
        assert!(vc.update(|_, cx| baaz.update(cx, |h, cx| h.step_right_split("wide", cx))).is_err());
        for _ in 0..2 {
            vc.update(|window, cx| baaz.update(cx, |h, cx| h.step_right_lower("off", window, cx))).expect("off closes");
            assert_eq!(layout(vc).right_lower, None);
        }
        // The place, over a scripted tab so nothing spawns a shell.
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.step_terminal_dock("zsh", window, cx))).expect("scripted tab");
        for _ in 0..2 {
            vc.update(|window, cx| baaz.update(cx, |h, cx| h.step_terminal_place("pane", window, cx))).expect("into the pane");
            let now = layout(vc);
            assert_eq!(now.terminal_place, TerminalPlace::Pane);
            assert!(now.terminal_in_pane() && crate::layout::right_kind(&now) == RightKind::Terminal);
        }
        for _ in 0..2 {
            vc.update(|window, cx| baaz.update(cx, |h, cx| h.step_terminal_place("bottom", window, cx))).expect("to the dock");
            assert!(layout(vc).dock_open() && !layout(vc).terminal_in_pane());
        }
        assert!(vc.update(|window, cx| baaz.update(cx, |h, cx| h.step_terminal_place("ceiling", window, cx))).is_err());
        restore_state(state);
    }

    /// `step_right_width` pins the width and clamps into the library range.
    #[gpui::test]
    fn step_right_width_pins_and_clamps(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("width");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(step_test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        // Start somewhere the default is NOT, or the assertion below passes
        // whether or not the verb touches the field — the hermetic state opens
        // at RIGHT_WIDTH, which is the very value most captures ask for.
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.right_pane.update(cx, |pane, _| pane.resize.width = 615.0)));
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.step_right_width("400", cx))).expect("a width");
        assert_eq!(vc.update(|_, cx| crate::layout::Layout::get(cx).right_width), Some(400.0));
        // The field the shell actually renders (`app.rs` hands the shell
        // the pane's `resize.width`). Asserting only the layout field above is
        // what let this verb ship inert: the captures drew at whatever width
        // the person had last dragged to.
        assert_eq!(vc.update(|_, cx| baaz.read(cx).right_width(cx)), 400.0);
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.step_right_width("10000", cx))).expect("a width");
        assert_eq!(
            vc.update(|_, cx| crate::layout::Layout::get(cx).right_width),
            Some(aui::shell::RIGHT_MAX_WIDTH)
        );
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.step_right_width("1", cx))).expect("a width");
        assert_eq!(
            vc.update(|_, cx| crate::layout::Layout::get(cx).right_width),
            Some(aui::shell::RIGHT_MIN_WIDTH)
        );
        let refused = vc.update(|_, cx| baaz.update(cx, |h, cx| h.step_right_width("wide", cx)));
        assert!(refused.is_err(), "an unparseable width fails the step");
        restore_state(state);
    }

    /// The mac virtual-keycode table: modifiers become `CGEvent` flags and
    /// the key becomes its code — `cmd-k`, `cmd-shift-t`, `alt-cmd-b`,
    /// `enter`, `escape`, `f5` — while unknown names map to nothing.
    #[test]
    fn key_code_table_maps_shortcuts_and_named_keys() {
        const CMD: u64 = 1 << 20;
        const ALT: u64 = 1 << 19;
        const SHIFT: u64 = 1 << 17;
        assert_eq!(super::key_event_parts("cmd-k"), Some((40, CMD)));
        assert_eq!(super::key_event_parts("cmd-shift-t"), Some((17, CMD | SHIFT)));
        assert_eq!(super::key_event_parts("shift-cmd-t"), Some((17, CMD | SHIFT)));
        assert_eq!(super::key_event_parts("alt-cmd-b"), Some((11, ALT | CMD)));
        assert_eq!(super::key_event_parts("enter"), Some((36, 0)));
        assert_eq!(super::key_event_parts("escape"), Some((53, 0)));
        assert_eq!(super::key_event_parts("f5"), Some((96, 0)));
        assert_eq!(super::key_event_parts("cmd-f13"), None, "f13 is parseable but off the table");
        assert_eq!(super::key_event_parts("cmd-["), Some((33, CMD)));
        assert_eq!(super::key_event_parts("bogus-key"), None);
        assert_eq!(super::key_event_parts(""), None);
    }

    /// `key:` and `page-focus` run against the window, like the other window
    /// verbs — no open session needed.
    #[test]
    fn key_and_page_focus_are_window_steps() {
        assert!(super::all_window_steps(&["key:cmd-k".into(), "page-focus".into()]));
        assert!(super::all_window_steps(&["page-focus".into(), "wait:100".into()]));
    }
}
