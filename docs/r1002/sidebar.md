# r1002 — Sidebar + account menu diagnosis

Read-only, 2026-10-02, baaz main @ 8bbbc41, agentic-ui v0.3.14 (b072936,
read from `~/.cargo/git/checkouts/agentic-ui-ceaee0e1248ba6d4/b072936`, cited
as `aui/…`). No source edited, no build, no turn sent. Live state read from
`~/Library/Application Support/baaz/{provider-status.json,tier.json,layout.json,baaz.db}`.

| # | Item | Root cause, short | Confidence |
|---|---|---|---|
| 1 | "New session" row appears at once | Y2a (351851f, 2026-09-28) re-added an "active session is never empty" exemption in `visible_sessions`, undoing v0.1 prep task 3 | **High** |
| 2 | Row "Settled" while transcript runs | The row's `running` and the transcript's phase come from different sources. A `session/list` reply overwrites the open session's row with the wire status, and nothing lays the live view back over it afterwards | Medium (mechanism certain; which side was stale in the screenshot is unproven) |
| 3 | No active project/session marker, no scroll-follow | `current` group draws nothing unless "Current-project bar" is on (default off). A brand-new session has no row to select or reveal, and the reveal gives up | High |
| 4 | Rows are 3 lines | Option B's title / context / status lines are hard-coded in the library row. The hover card already shows everything lines 2–3 carry | High (inventory) |
| 5 | Huge hover card / header | Titles are never length-clamped on several label paths, and the hover card title is "never truncated", with no max height | High |
| 6 | Usage stale / unavailable | Usage is refreshed only when the menu opens, by peeking lanes that are still open. Claude's reading is never pushed when it arrives. Muse's row reads a cold-host `/upgrade` scrape that is cached as a full answer | High (mechanism); Medium (why Claude emitted nothing new) |

---

## 1. REGRESSION — "New session · No reply yet" row appears immediately

### Evidence

- Here is the rule as v0.1 prep task 3 wrote it. `crates/baaz/src/sidebar.rs:570-587`, `SessionEntry::is_empty`:
  *"A new session has no row at all until its first message is sent — this
  holds even for the session that is currently open"*. Its body is
  `self.turns == 0 && !self.running && !self.named`, with no active carve-out.
- `crates/baaz/src/app/list.rs:620-642`, `visible_sessions`, filters with:
  ```rust
  || !entry.is_empty()
  || active_head.as_deref() == Some(entry.id.as_str())   // :640
  || pending_head.as_deref() == Some(entry.id.as_str())  // :641
  ```
  `git blame` puts :640-641 in **351851f "Y2a WIP (muse stopped mid-edit; does
  not compile)"**, 2026-09-28. The comment above it says *"the active/pending
  head is never filtered as empty, so the destination row stays highlighted
  while the pack runs"*. That was meant for handoff destinations.
- `ChainIndex::head` (`sidebar.rs:1186-1188`) returns **the id itself** for
  any unlinked session. So `active_head == active id` holds for every ordinary
  session, and every zero-turn open session passes the filter. Muse ≥1.3 lists
  zero-turn sessions (`lifecycle.rs:1437-1446` comment). The fresh session
  therefore lands in `self.sessions` from `load_sessions`
  (`lifecycle.rs:1517`), and the exemption draws it as `New session` (UNNAMED)
  with `No reply yet` (`sidebar.rs:687`).
- `new_session_open` itself is still correct. It inserts no row
  (`lifecycle.rs:1437-1446`) and relies on `is_empty`.
- **Why the gate stayed green:** the tests cover only the pure
  `is_empty` (`sidebar.rs:2561-2609`). No test runs `visible_sessions`
  with a zero-turn active session.

### Fix

`crates/baaz/src/app/list.rs:640-641`: exempt the active/pending head only
when it really is a chain head that differs from the raw id (a handoff
destination), or only when the head row has turns:

```rust
|| (active_head.as_deref() == Some(entry.id.as_str()) && active.as_deref() != Some(entry.id.as_str()))
|| (pending_head.as_deref() == Some(entry.id.as_str()) && pending.as_deref() != Some(entry.id.as_str()))
```

If the pack-run highlight needs it, also keep it for
`sidebar::is_handoff_dest(active)`. `local_started_row`'s `running: true`
(`sidebar.rs:939`) still reveals the row on the first send.

### Verify

- New gpui test in `sidebar_view.rs` tests (or `app/list.rs`): `--no-connect`
  harness, `new_session` (→ `open_local_draft`), then insert a zero-turn wire
  row for that id. Assert `visible_sessions(cx)` does not contain it. Then
  apply a `turn/started` for it and assert it does. Add a twin for a
  handoff destination (head ≠ id) that must stay visible.
- Probe: add uiprobe entry `new-session` = `["--no-connect","--login","signed-in","--steps","new"]`, then
  `cargo build -p baaz && python3 ~/.claude/skills/relay/scripts/relay_visual.py --repo . --entry new-session`
  and **look at the image**: no "New session" row.

---

## 2. Row says "Settled" while the transcript shows "Approving… ls · Running" / "Running command… 8m 06s"

### Where each side reads from

- **Transcript.** `Approving…` + `Running` pill is `ApprovalState::Approving`
  (`aui/transcript/approval.rs:522-527`). The muse fold sets it when the
  user allows (`crates/muse-adapter/src/fold.rs:2357`) and settles it **only at
  `turn/completed`** (`fold.rs:948-952`). The footer's "Running command…" is
  `status_phase` (`crates/baaz/src/session/render.rs:1360-1390`), which
  requires `self.running` (the view's live-turn bit, set at `turn/started`,
  `session/events.rs:65`) plus an open tool block. So at that frame the view
  believed the turn was live.
- **Row.** `SessionEntry::row_status` (`sidebar.rs:667-688`) reads
  `self.running`. `Settled` is simply "not running, turns > 0". The pulse is
  `state()` / `.pulse()` off the same bit (`sidebar.rs:590-596, 822-824`).
  `entry.running` has three writers:
  1. `sync_row_live` (`app/list.rs:400-461`). This runs after `turn/*` /
     `approval/*` / `userInput/*` events (`app.rs:1688-1732`) and re-reads
     the live view's `busy()` (`session.rs:1435`). This agrees with the
     transcript.
  2. `session/statusChanged` (`app.rs:1735-1744`). It folds the wire status,
     then `sync_row_live` lays the open view back over it. This also agrees.
  3. **`load_sessions` reply** (`app/lifecycle.rs:510-518`). It does
     `this.sessions = sidebar::merge_session_list(wire, &this.sessions)`, a
     wholesale replace where each row's `running` is
     `session.status == Running` (`sidebar.rs:276`), and it clears
     `approval_command`/`pending_question`. **Nothing re-applies the open or
     parked views' live truth after this** (the only `sync_row_live` callers
     are `app.rs:1731,1743` and `lifecycle.rs:3631,3638,3692`).

So whenever a `session/list` reply lands while the wire's projection says
`idle` (or `notLoaded`) and the view says running, the row flips to
`Settled`. Then it stays there until the next live event for that session. A
command running after approval emits nothing for minutes. `load_sessions` runs
on every new session (`lifecycle.rs:1517`), every `turn/completed`
(including title/byline side sessions), switches, and renames. The owner had
just created a new session in office-samples, which is a reload trigger.

**Which side was wrong is not proven.** `ls` "running" for 8 minutes suggests
the transcript may be the stale one: the turn ended or died on the server, but
this view never folded its `turn/completed`, so the `Approving` card and
`running` never settled. Then the wire's `idle` was the truth, and the row was
right for the wrong reason. Either way the defect is the same: **two sources,
and no reconciliation**. No baaz log file exists (`baaz_log!` goes to
stderr, `log.rs:10-13`) and the muse `session.jsonl` was not found, so the
server's view of that turn could not be read.

### Fix

1. `app/lifecycle.rs:517-518`: after `merge_session_list` +
   `merge_provider_rows`, call a new `overlay_live_rows(cx)`. For the active
   view and each cached view, apply what `sync_row_live(id, false)` applies
   (busy → `running`, `row_pending()` → approval/question). The open view's
   live truth then survives a list reply, as the `statusChanged` path already
   promises (`app.rs:1733-1735`).
2. Reconcile disagreement rather than overlay blindly. When the wire
   (`statusChanged` or list) says not-running for a session whose view has
   `running = Some(turn)` for longer than about 30 s, re-sync the view
   (`top_up` / `session/resume` from its cursor, `lifecycle.rs` top_up). That
   folds the missing `turn/completed` and settles the `Approving` card, or
   proves the turn live. Log the disagreement with `baaz_log!`.
3. Optional: a row `Working` should also count an `Approving` card or open
   tool block in the active turn, the same predicate as `status_phase`.

### Verify

- Unit (gpui): open a view, apply `turn/started` T1, then call the list-apply
  path with a wire row `status: idle` for that id. Assert
  `visible_sessions[..].running == true` and `row_status` is `Working`.
  Today this asserts `Settled`.
- Unit: view with `running = Some(T1)` + `statusChanged idle` older than the
  grace period → the reconcile hook fires (spy on the resume call).
- Replay: a fixture with `approval/resolved (user, allow)` and no
  `turn/completed`, then a `session/statusChanged idle`. Captures must not
  show `Approving…` beside an idle row after the reconcile. Note the CLAUDE.md
  warning: a never-terminating fixture blocks a capture for 120 s, so drive it
  as a unit test, not `--replay`.

---

## 3. No active project/session marker; sidebar does not follow the active session

### Evidence

- **Selection exists but has nothing to land on for a new session.**
  `sidebar_view.rs:489-493` selects `pending_id` or the active id
  (chain-headed), and the library tints the selected row `surface_3`
  (`aui/nav/session_row.rs:918-920`). After fix #1, a new session has **no
  row**, so nothing is selected. Even with a row, `surface_3` versus hover
  `surface_2` is a faint ground with no accent.
- **The project marker is off by default.** `grouping_by_project` computes
  `current_project` correctly: the open session's project, else the store's
  current (`sidebar.rs:1539-1550`), then `group.current(true)` (`:1619-1620`).
  But the library draws nothing for `current` alone. See
  `aui/nav/views.rs:274-293` (*"`current` alone changes nothing visible"*) and
  `:1226-1228`. Only `.current_bar(layout.group_bar)` (`sidebar.rs:1604`)
  draws it, and `group_bar` defaults to **false** (`layout.rs:217`, from
  56399c4 owner round 4). The owner's `layout.json` persists
  `"groupBar": false` explicitly. Two more problems:
  - Even when it is on, the group's rolled-up **state bar takes the same slot
    first**: `if let Some(bar) = state_bar … else if self.current &&
    self.current_bar` (`aui/nav/views.rs:1306-1308`). A current project with a
    running or attention session shows no current marker.
  - Stale doc comment: `layout.rs:140-141` claims `current(true)` "keeps only
    the semibold ink name". The library no longer restyles the label.
- **Reveal gives up on a rowless session.** Opening arms `self.reveal`
  (`lifecycle.rs:1893-1902`, `3277-3284`; a sidebar click is quiet by
  design). `reveal_sidebar_row` (`sidebar_view.rs:1058-1093`) looks for the
  row, then `reveal_head_row` (`:1139-1146`). That fallback finds the project
  head only via `self.sessions.iter().find(id)` + `entry.project`. A new
  session that is known but has no landing row hits *"A known session with
  nowhere to go"* and **disarms on the first miss** (`:1069-1074`). An unknown
  one waits `REVEAL_UNKNOWN_FRAMES` and then disarms. A brand-new Claude Code /
  Codex session is never in `self.sessions` before its first send, and nothing
  falls back to the override's project (`set_override(…project…)`,
  `lifecycle.rs:1447`) or to `current_project`. After the first send,
  `app.rs:1781-1784` re-arms, but only on the branch where the row already
  existed. The local-row insert branch (`app.rs:1786-1810`) does not re-arm.

### Fix

1. Default `group_bar: true` (`layout.rs:217`) and fix the doc comment. The
   owner's explicit `false` needs either the Settings toggle or a one-time
   layout migration (version bump in `layout.json`).
2. Library (`aui/nav/views.rs:1306-1320`): let the current marker coexist with
   the state bar. For example, `current` restyles the label to `ink` +
   semibold (what the spec `docs/02-component-spec.md` §2.1 says: *"`current`
   alone only restyles the label"*), and the state bar keeps the left edge.
3. Selected session row: add an accent cue (2 px accent left bar or
   `accent_soft` ground) in `CompactSessionRow::render`
   (`aui/nav/session_row.rs:918`), so it is not just surface_3.
4. Reveal for a rowless active session: in `reveal_head_row`
   (`sidebar_view.rs:1139`) resolve the project from
   `self.overrides.get(id).project`, then `self.provider_sessions` record
   project, then `current_project_id()`, before giving up. Keep the arm alive
   (do not disarm) while the active session is rowless. Also re-arm the reveal
   in the local-row insert branch (`app.rs:1808`) for the active session.
5. Make the project group of a rowless active session count as `current` (it
   already does through `effective_current`), so 1+2 mark it.

### Verify

- Unit: `grouping_by_project` with an active id that has no row → the group
  for `projects.current` is `current`. Already true, so pin it with a test.
- Unit (gpui, `sidebar_view.rs` tests like
  `b3c_rowless_boot_reveal_waits_without_moving_the_list`): 40 sessions, active
  = new rowless session whose project is the last group → after frames
  settle, `logical_scroll_top` shows that group's head.
- Probe: entry `new-session-scrolled` with the `--sidebar-fixture
  fixtures/sidebar/stress.json` steps `new` in a lower project, and look for
  the accent bar on the project head in view.

---

## 4. Row density — 3 lines → 1 (or 2)

### What renders today

Library `CompactSessionRow::render` (`aui/nav/session_row.rs:912-1010`), fed by
`SessionEntry::summary` (`crates/baaz/src/sidebar.rs:771-826`). The comment at
`session_row.rs:935-938` reads: *"Option B: title, context, status — three
lines, each keeping its line when empty"*.

| Line | Carries | Source |
|---|---|---|
| 1 | status dot (pulses when running; state colour), title (truncate), elapsed (mono, right; swaps for the hover action tray) | `summary()` → `SessionSummary::new(id, label, state(), elapsed)` `sidebar.rs:772`; dot `session_row.rs:996-1003` |
| 2 (context) | priority: approval command → pending question → byline `ask · result` → preview → `project · branch` → `Archived` tag; plus `· N terminals running` and the provider mark | `sidebar.rs:775-809`, `second_line()` `:702-714` |
| 3 (status) | `Needs approval` / `Asked: "…"` / `Working · 4m` / `Failed · 1h` / `Settled · 12m · 5 turns` / `No reply yet`; empty for provisional rows | `row_status()` `sidebar.rs:667-688`, set at `:816-818` |

### Hover card already shows

`detail_data()` (`sidebar.rs:724-748`) → `aui/nav/detail.rs`. It includes the
full title + age, the ask (2 lines), the latest reply's first line in the state
colour, or the attention box (pending approval/question, 3 lines), status with
detail (error text for `Failed`), branch · turns · updated meta row, and the
project/workspace footer. **Everything on lines 2 and 3 is already in the card**
except the terminal-running hint and the provider mark.

### Earlier compact designs in the repos

- `~/Projects/agentic-ui/docs/02-component-spec.md:82`: *"Compact session row
  (`sr`, used in project and date views): min 30 px, padding 4 10 4 12, one
  line + optional meta line."* This was the original design before option B.
- `~/Projects/agentic-ui/design/reference/cards/23-sidebar-views.png` and
  `20-worktree-rows.png` are the reference cards with the one-line `sr` row.
- `docs/briefs/muse-lib-row-b.md` is the brief that chose the 3-line option B.
  Its mockups lived in `/tmp/row-mockups/`, which is gone. No other compact
  mockup is in `docs/mockups/` (only terminal-skills) or `design/`
  (baselines only).

### Fix

Library: add `RowDensity { One, Two, Three }` on `CompactSessionRow` /
`SidebarView` (additive). It would draw:

- **One**: dot + title + trailing elapsed. The state lives in the dot colour
  and pulse. Attention/failed tint the dot.
- **Two**: line 2 merges status and context:
  `Needs approval · ls`, `Working · 4m`, `Settled · 12m · ask…`.

Baaz: a Settings → Sidebar "Row density" switch persisted in `layout.json`,
default One or Two per the owner. Pass density from `sidebar_view.rs` where
rows are built. Keep the uniform-height rule per density. The virtual list
measures rows, so check that `repair_sidebar_scroll` and reveal still hold.

### Verify

Library gallery entries for each density, both themes, at 260/420 px. Baaz
Tier V `signed-in` with `--sidebar-fixture fixtures/sidebar/rowb.json` per
density. Regenerate baselines **on main only, in their own commit**.

---

## 5. Hover card overflow and header crumb with a huge pasted title

### Evidence

- **The label is unclamped on several paths.** `one_line` caps at 80 chars
  (`sidebar.rs:1753-1759`), but only some paths use it:
  - `SessionEntry::join_cached` / siblings pick `session.title`,
    `session.first_user_prompt`, `IndexEntry::label`, and `name` / `generated_title`
    raw (`sidebar.rs:232-243`, `351-359`, `453-459`). The comment at
    `:244-247` notes *"Muse writes whole first prompts into the index title"*.
  - The local row on first send uses `first_prompt_text()` raw
    (`app.rs:1799-1801`; `session/commands.rs:1102-1114` takes the first
    line only, with **no char cap**). A pasted brief that is one long
    paragraph is the whole title.
  - Only `local_started_row`'s relabel (`sidebar.rs:947`),
    `first_user_title` (`sessions.rs:241`) and handoff titles clamp.
- **The hover card title is unbounded by design.** `aui/nav/detail.rs:80`,
  `:148`: *"Full title, wrapping, never truncated"*, rendered at `:315-318`
  with no clamp. The ask budget (`SESSION_DETAIL_ASK_LINES = 2`, `:68-70`)
  counts **newline** lines (`first_lines`, `:248-257`), so one long line
  wraps without bound. The card has a fixed width (`:479`, 320 px) and **no
  max height** (no `max_h` anywhere in `detail.rs`).
- **Header.** `render_centre_header` (`app.rs:2669-2790`) uses
  `collapsed_head_label` and `.truncate()`. It is single-line, but fills the
  whole header width with the brief, because the label itself is the brief.

### Fix

1. Baaz: clamp at the source. Wrap every label pick in `join*`
   (`sidebar.rs:236-243`, `355-359`, `453-459`), provider-lane row titles,
   and `app.rs:1799` with `one_line` (80 chars, whitespace-flattened).
   `generated_title` is model-written and short anyway. The user `name` can
   stay unclamped.
2. Baaz `detail_data` (`sidebar.rs:739-741`): pass a title clamped to ~160
   chars and an ask clamped to ~240 chars (char budget, not newline budget).
3. Library `detail.rs`: clamp the title to a char budget (the same `first_lines`
   idea, by chars), give the card `max_h` (~60 % of the window), and add
   `overflow_hidden`, so no caller can produce a sidebar-covering card.
4. Header: optionally cap at ~60 chars in `render_centre_header`, since the
   tooltip/hover can hold the rest.

### Verify

- Unit: `join_cached` with `session.title` = 2,000-char single line →
  `label.chars().count() <= 80` and ends with `…`. The same for the
  `turn/started` local-row path.
- Library unit: `SessionDetail` with a 2,000-char title lays out no taller
  than `max_h` (the existing placement tests at `detail.rs:715-751` already
  size cards).
- Probe: `fixtures/sidebar/titles.json` + a 2k-char title row, hover via
  `forced_detail` (`lifecycle.rs:934`, the `detail:` step), shot.

---

## 6. Account menu usage: Claude "as of 3d ago", Muse "Usage currently unavailable"

### Persisted state (2026-10-02 ~14:50 IST)

`provider-status.json`:

- claude-code `usage.as_of = 1790633238` = **Tue Sep 29 03:37**, Weekly 0.41,
  resets 1790989200.
- muse `usage = {plan: "Power Usage", windows: [], as_of: 13:32:46 today}`.
- codex `as_of` = 14:27 today (fresh).

`baaz.db usage_turns` shows Claude Code turns today, the last at **14:13:31**,
and Muse at 14:17. So turns ran, but no Claude reading landed.

`tier.json`: probedAt 13:40:57, `currentPct 0 / weeklyPct 47`,
`usageUnavailable: false`. The wire answered at some point after 13:32.

### When each provider refreshes

- **The only trigger is opening the account menu.** `open_menu(MenuKind::Account)`
  → `refresh_account_usage` (`dialogs.rs:612-615`;
  `app/lifecycle.rs:2123-2157`). It is throttled to once every 60 s
  (`provider_status.rs:1039, 1099-1109`). There is no timer and no hook at
  turn end.
- **Claude Code** is a *peek* at views that are open right then: `active` +
  `session_cache` (`lifecycle.rs:2141-2152`) → `lane_usage()`
  (`session/lane.rs:336-340`), which is `try_lock` (**`None` while the lane is
  busy**) → `read_usage()` → the fold's latest `rate_limit_event`
  (`provider-claude-code/src/lib.rs:1316-1318`, `fold.rs:1030-1031`). So a
  reading is recorded only if (a) the CLI emitted a `rate_limit_event` in
  this process's lifetime, (b) that lane's view is still active/cached, (c)
  it is not mid-turn, and (d) the menu is opened. A reading that arrives and
  whose lane closes, is evicted from the MRU, or lives through an app restart
  before the next menu open is **lost**. `rate_limit_event` is not a per-turn
  frame either: docs/18-claude-code.md §5 captured it as an
  `allowed_warning` / threshold frame. That it was not emitted on today's
  turns is **unverified** (the stream is not persisted). The 3-day age is
  honest about the data, but the pipeline cannot do better than "whatever a
  still-open lane last heard".
- **Muse** is `record_muse_usage(tier.footer_label(), tier.weekly_fraction())`
  from the in-memory `self.tier` (`lifecycle.rs:2153-2159`). The menu row reads
  `self.tier` (`account_usage.rs:69-131`). "Usage currently unavailable" is
  `Tier::Subscription { usage_unavailable: true }` (`account_usage.rs:93-98`),
  which **only the pty `/upgrade` scrape produces**
  (`tier.rs:1205`; the wire builder always sets `false`, `tier.rs:809`). The
  scrape runs when `usage/read` answers `{}`, which muse does on a cold host
  until a turn has observed a provider response (`tier.rs:28-30`). So after
  launch, before any Muse turn, the tier is the cold-host card's "usage
  currently unavailable". `complete()` accepts it as a full answer
  (`tier.rs:757-762`) and it is cached for `CACHE_TTL` = 1 h (`tier.rs:359`),
  re-served by `probe_tier(false)` (`billing.rs:50-55`). It is replaced only by
  `usage/changed` (`billing.rs:142-148`, after a Muse turn), `/usage`, or
  "Check again". The menu-open refresh re-probes **only** for PayAsYouGo
  (`lifecycle.rs:2133-2140`). The stored snapshot (13:32, `windows: []`) is
  exactly this state. `tier.json` at 13:40 shows the wire later supplied
  numbers.

### Fix

1. **Push, don't peek (Claude, Codex).** When a lane folds a
   `rate_limit_event` / account push, emit a `SessionEvent::UsageObserved`
   (or have the lane call back). Baaz records it immediately with
   `record_refreshed_usage` + `save_cache` (`provider_status.rs:1115-1128`).
   Also record at every lane `turn/completed`. Drop the `try_lock` peek as
   the only path.
2. **Muse on menu open:** when signed in and the tier is
   `usage_unavailable` or has no numbers or is older than about 5 min, issue
   `usage/read` on the live client (wire, free) via `tier::read_usage_value`
   + `probe_primary_confirmed`, off-thread, the same as `probe_tier`. Do not
   run the pty scrape on every open.
3. **Don't cache a cold-host "unavailable" for an hour:** in `tier::remember`
   / `cached()`, give `usage_unavailable` a short TTL (e.g. 5 min) or treat it
   as not `complete` for caching.
4. Optional: a refresh timer while the menu is open, plus refresh on
   `turn/completed` for any provider (throttled).
5. Copy: when the only Claude reading is old, say why ("Claude Code reports
   usage only near limits; last reading 3d ago").

### Verify

- Unit (`provider_status.rs` tests, sandboxed like
  `refreshed_claude_reading_persists_and_restores_from_cache` `:1710`): the
  lane fold observes a `rate_limit_event`, then the view is dropped → the
  cache holds the reading with `as_of` ≈ now (fails today).
- Unit (`account_usage.rs`): tier `usage_unavailable` plus a fresh wire read
  → the row shows windows.
- Unit (`tier.rs`): `remember(usage_unavailable)` → `cached_at(now + 6 min)`
  is `None`.
- Live, free: launch the bundle, open the account menu before any turn, and
  check whether `usage/read` was issued (`BAAZ_TRACE=1` stderr). Then send one
  Claude Code turn, close that session, open the menu, and look for
  `provider-status.json` claude-code `as_of` ≈ now. That costs a turn: the
  owner should do it.
