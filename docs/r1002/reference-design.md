# R1002 — reference design: settings, transcript noise, right-pane toggles (2026-10-02)

Research and recommendations only. No source was edited. Follows the method of
`round4-reference-apps.md`: every claim is tagged **[source]** (read in the app's own
repository at the commit named), **[documented]** (vendor docs or changelog),
**[reported]** (issue or forum thread, not a maintainer), **[baaz]** (read in this repo
today) or **[guess]**. Sources are listed in §7.

Reference set: T3 Code (`pingdotgg/t3code` @ `54084ae1`, 2026-10-01, cloned and read),
Synara (`Emanuele-web04/synara`, a T3 fork, files read through the GitHub API), Zed's agent
panel (`zed-industries/zed` `crates/agent_ui`, read through the API), Claude desktop Code
tab (code.claude.com docs and `anthropics/claude-code` issues), Codex app (learn.chatgpt.com
docs and `openai/codex` issues), Cursor 3 (changelog and forum), Warp (docs), Conductor
(docs). The design rules applied throughout are the owner's own (memory
`feedback-design-taste`): calm colour, real buttons, the action row (hint left, spacer,
secondary, primary right), a 3-part header whose columns each own their header cell, plain
hairline boxes, and **no action that nobody reaches for**.

---

## 0. Summary of recommendations

1. **Settings becomes a route, not a dialog.** It replaces the centre *and* right columns,
   the sidebar stays. The centre header cell reads `Settings / <Section>` with a close
   button on the right. Below it sit a 200 px section list and one readable column
   (max ≈ 680 px). Sections: **General, Sidebar, Providers (with one sub-page per
   provider), Shortcuts, Archived**. Esc, the close button, ⌘, or a click on any session
   returns you to exactly where you were. This is the route model Baaz's Skills page
   already uses.
2. **A settled turn folds to one line**: `Worked for 2m 14s · Read 12 files, edited 3,
   ran 5 commands   +48 −12`. The final reply is never inside the fold. A live turn shows
   interim prose verbatim and **one** live activity row per run of tool calls, not one
   card per call. Thinking is expanded with a capped height while it streams and collapses
   when it ends. Approvals dock above the composer and also leave a marker in the
   transcript. Long tool results never render inline past a few lines; they open in the
   right pane.
3. **The right pane gets a 3-segment control in its own header cell** (`Changes · Files ·
   Browser`), plus one shortcut per kind. *Diff review* and *Git changes* merge into
   **Changes**. The centre header keeps two toggles (terminal, right pane). Contextual
   entry points do most of the opening: a diffstat chip opens Changes, a path opens Files,
   a URL opens Browser.
4. **Sidebar rows**: one line when there is no byline, two when there is, as one uniform
   height per mode. The active row gets a filled ground and nothing else. Status sits in
   the glyph column.

---

## 1. Settings

### 1.1 What the references do

| App | Where settings lives | Navigation | Search | Exit | Evidence |
|---|---|---|---|---|---|
| T3 Code | A route (`/settings/<section>`). **The app sidebar's content is swapped for the settings nav**; the page fills the inset to its right. | Left list with an icon per row: General, Appearance, Project, Keybindings, SnapShots, Providers, Integrations, Source Control, Storage, Connections, Archive. Diagnostics and Licences are sub-pages reached from General. | Search field at the top of the nav (`/` focuses it). Typing replaces the nav list with results, each showing title plus section, navigable with ↑/↓/Enter. A hit navigates to `section#row`, scrolls the row into view and highlights it. Esc clears the search first. | `useEscapeToGoBack`: page-level Esc goes back to the previous app page (or home) *unless a control already consumed Esc*. | [source] `SettingsSidebarNav.tsx`, `routes/settings.tsx`, `hooks/useNavigateBack.ts`, `settingsSearch.ts` |
| T3 page chrome | The header is a breadcrumb, `Settings / Section`. Content sits in a centred `max-w-4xl` column. Rows are grouped into rounded cards with hairline dividers between rows (`SettingsGroup`). "Restore device defaults" appears on General only. | — | — | — | [source] `SettingsBreadcrumb.tsx`, `WorkspacePageContainer.tsx`, `SettingsGroup.tsx` |
| Synara | A route. The section taxonomy is shared "between the main sidebar and the settings screen". The nav is grouped **Personal / Integrations / Coding / System / Archived** (16 sections: General, Profile, Appearance, Notifications, Behavior, AppSnap, Computer, Shortcuts, Worktrees, Archived, Models, Providers, Skills, Usage, Integrations, Advanced). Each page opens with an H1, a one-line description and a **Restore defaults** button, in a `max-w-2xl` column. | Left list, grouped | — | — | [source] `settingsNavigation.ts`, `_chat.settings.tsx` |
| Codex app | Settings has its **own left sidebar**, which takes the place of the chat-history sidebar (a bug appears only when the chat sidebar was collapsed). Sections: General, Profile, Keyboard shortcuts, Notifications, Appearance, Pets, Browser, Computer Use, Personalization, Suggested prompts, Memories, Archived chats, Keep a chat near your work. The shortcuts page has search and lets you rebind. | Left list | On Keyboard shortcuts only | ⌘[ / back | [documented] settings reference; [reported] #26160, #48855 |
| Claude desktop | A Settings page with General ("Desktop app" group: Computer use, Unhide apps…), **Claude Code** (worktree location, branch prefix, Browser toggles, auto-archive), Connectors and Usage. Keyboard shortcuts are a separate **⌘/ sheet**, not a settings page. Plugins and skills live in a separate **Customize** surface. | Left list | — | — | [documented] desktop docs; [reported] #93503 (`/usage` opens Settings → Usage) |
| Zed | Agent settings is a panel of per-provider sections (`agent_configuration`). | — | — | — | [source] file list only |

What they share: settings is **a page, not a modal**. There is a **left list of 5–16
sections**. Providers have **their own section, and per-provider detail sits inside it**,
never as top-level entries. Archived sessions are a settings section in three of the four.
Shortcuts are a searchable, rebindable list. Each page opens with a title and one line of
description. Esc returns you to the work, and a focused control gets to consume Esc first.

What they disagree on: three of them swap the app sidebar for the settings nav. The owner
wants settings to take the area *right of* the sidebar. That is the Claude-style framing,
and it is the better fit for Baaz (§1.3).

### 1.2 Every setting Baaz has today [baaz]

From `crates/baaz/src/settings.rs`, `settings_providers.rs`, `layout.rs`, `keymap.rs`:

| Setting | Store | Current home |
|---|---|---|
| Collapse chevron (`group_chevron`) | layout.json | Settings → Sidebar |
| Current-project bar (`group_bar`) | layout.json | Settings → Sidebar |
| Branch name (`group_branch`) | layout.json | Settings → Sidebar |
| Name sessions automatically (`auto_title`) | layout.json | Settings → Sidebar *(misfiled: it spends a model call)* |
| Summarise sessions in the sidebar (`auto_summary`) | layout.json | Settings → Sidebar *(model call)* |
| Summarise handoffs with a model (`handoff_model_summary`) | layout.json | Settings → Sidebar *(model call; has nothing to do with the sidebar)* |
| Enable Muse / Claude Code / Codex (`provider-enabled:<id>`) | provider status | Settings → Providers (a list of switches) |
| Provider card per provider: headline, account/plan, version, Sign in / Sign out (with a confirm naming the real command) / Install / Re-check / Docs | live status | A **separate** Providers *page*: a 640×600 centred modal card with its own scrim (`render_providers_page`) |
| Use my own MCP servers (Claude Code, Codex: `use_own_mcp.*`) | layout.json | Under each card on that page |
| Set up providers… (the connect screen) | — | A button on that page |
| Every key binding (≈45 rows, editable, recorded in place, with "where it fires" captions) | keymap file | Settings → Shortcuts |
| Group by (Date / Project), search-all-projects | layout.json | The sidebar's view-options menu. **This is view state; leave it there.** |
| Pane widths, terminal open/height, right open/kind, collapsed groups | layout.json | View state. Never surfaced in Settings. |
| Archived sessions (archive / unarchive) | sessions | Row menus and an archived toggle (`step_toggle_archived`) |

Defects the IA should fix while it moves things:

- **Two provider surfaces.** One is a section of switches, the other a modal page of cards
  that differs in shape and size from the dialog. The owner's complaint about "a modal of
  varying height" is partly this.
- **Three model-spending switches under "Sidebar".**
- **The Shortcuts list shows "Terminal takes the key" seven times** (`keymap.rs`
  category `terminal`). It needs grouping, or these rows should be hidden as
  reserved/non-editable.

### 1.3 Proposed IA and layout

**Frame.** Settings is a `Route::Settings` beside `Route::Skills` (the Skills page already
"replaces the transcript area while open", `app.rs` ~3013). While it is open:

- The **sidebar stays** exactly as it was. Running sessions keep their status dots, and
  one click on any row is an exit (see below). This is the reason not to copy T3's sidebar
  swap: Baaz is a multi-session cockpit, and hiding the list hides "something needs you".
- **Centre and right columns merge** into one settings surface. The right pane is hidden,
  not closed: its open/kind/width are untouched and come back on exit. The terminal dock
  is hidden for the same reason.
- **Header (3-part rule kept).** The sidebar header cell is unchanged. The centre header
  cell holds the breadcrumb `Settings / Providers / Codex` (ink-3 / ink-3 / ink),
  a spacer, and one ghost **close** button (X, "Close settings", tooltip `esc`). The right
  header cell is absent while the right column is absent. Nothing else goes in the header.
- **Body.** A **section list 200 px wide** on the left of the settings surface,
  hairline-separated from the content. The content is **one column, max 680 px**, centred
  in the remaining width, scrolling on its own, top padding 32 px. The section list never
  scrolls with the page.
- **Each page**: H1 (section name), one muted line of description, then **groups**. A
  group is a small caps-free label (ink-3) over **one plain hairline box** (`border_1`
  `line`, `R_SM`, `surface_2`, the same frame as the approval card) with hairline dividers
  between rows. A row is label + one detail line on the left, the control on the right
  (switch, segmented, select, or a real bordered button). No left accent rails, no colour
  except a status dot where status *is* the content.
- **Height never varies**: the surface is always the full column, so moving between
  sections never resizes anything. That is the owner's modal complaint, solved
  structurally.

**Sections** (list order; a muted divider before Archived):

| Section | Groups → rows |
|---|---|
| **General** | *Model housekeeping* (description: "Each of these spends one short call on the cheapest model"): Name sessions automatically · Summarise sessions in the sidebar · Summarise handoffs with a model. *(Future rows go here: appearance/theme, text size, notifications.)* |
| **Sidebar** | *Project rows*: Collapse chevron · Current-project bar · Branch name. A one-line note: "Grouping and sort live in the sidebar's view menu", with no link button (rule: no unnecessary actions). |
| **Providers** | The overview page holds one row per provider in switcher order: provider mark, name, headline ("Signed in · Max plan · v2.1.276", or "Not installed"), the **Enable** switch on the right, and a chevron row-affordance into the sub-page. Footer action row: hint "Signing a CLI out signs it out on this Mac" left, **Set up providers…** (secondary) right. |
| ↳ **Claude Code / Codex / Muse** (sub-pages, breadcrumb `Settings / Providers / Codex`, nested under Providers in the list, indented, shown only while Providers is selected) | *Account*: the provider card as today (headline, account, plan, version) with its own action row (Docs · Re-check · Sign out / Sign in / Install, primary on the right). *Tools*: Use my own MCP servers (Claude Code, Codex only, with the existing detail sentence). *Muse*: the account group only. This is where future per-provider defaults go (default model, approval mode, effort), and there is one place to put them. |
| **Shortcuts** | A search field pinned at the top of the page (⌘F focuses it). Groups by the keymap's `category`, relabelled for people: Session, Composer, Panes, Terminal, Palette, Skills, Window. Rows: label, "where it fires" caption (existing `context_label`), keycap on the right. Click the keycap to record (existing behaviour). Non-editable rows go in a final *Reserved* group, each **listed once** (fold the seven "Terminal takes the key" rows into one row that lists the keys). A **Restore defaults** button appears in the page's H1 row only when something differs (T3/Synara pattern). |
| **Archived** | A list of archived sessions grouped by project: title, archived date, an **Unarchive** button. Empty state: one muted line. Moves archive management out of a toggle. Optional, but all three references have it. |

Not proposed: a General → About page, telemetry, Pets. Nothing is added that has no setting
behind it today.

**Search across settings.** At ≈10 non-shortcut rows a global search is not earned yet.
Put search on the Shortcuts page (as Codex does) and revisit when General grows past
roughly two screens. If it is added later, T3's model is the one to copy: a field at the
top of the section list, results replace the list, Enter jumps to the row and flashes its
ground once.

**Opening.** ⌘, (opens the last-visited section this run, General on first open). The
account menu's "Settings…" and "Providers…" rows. Existing deep links `settings:<section>`,
plus new `settings:providers/codex`. The step verbs keep working for Tier V and stay
idempotent: `settings:<x>` opens, it never toggles (CLAUDE.md lesson).

**Exit and back behaviour** (in priority order; T3's rule is that a control consumes Esc
first):

1. Esc while recording a shortcut cancels the recording.
2. Esc in the Shortcuts search clears it. A second Esc leaves.
3. Esc while on a provider sub-page goes up to Providers. (A breadcrumb click does the
   same.)
4. Otherwise Esc, the close button, or ⌘, again **closes Settings and restores** the
   active session, its scroll offset, the composer draft and focus, and the right
   pane/terminal exactly as they were.
5. A click on any sidebar session row, ⌘N, or a palette jump closes Settings and goes
   there.

Settings changes apply live and persist on change. There is no Save/Cancel, and no
"unsaved changes" state.

**Accessibility**: the section list is a `list` of `tab`-role items with names. The
breadcrumb is a `navigation` landmark. Every switch keeps Role=Switch with its label
(already true). The close button is "Close settings". Tier V needs entries
`settings-general`, `settings-providers`, `settings-provider-codex`, `settings-shortcuts`,
`settings-archived`.

---

## 2. Transcript noise

### 2.1 What the references do

**Turn-level fold after the turn settles** — the dominant pattern.

- **T3 Code** [source] `MessagesTimeline.logic.ts` `deriveTurnFolds`:
  - "Settled turns fold activity before their terminal assistant message behind a
    'Worked for …' row."
  - **Nothing folds while the turn is live**, "which is when traces are watched."
  - Thinking folds with the work ("dozens of 'Thought' rows" otherwise).
  - A single trailing activity after the answer joins the fold. Larger trailing groups and
    **failures stay visible**.
  - **User-answered questions and subagent spawns stay visible.**
  - A lone compaction or thought does not get a fold that "hides nothing else".
  - An interrupted turn reads "You stopped after 40s". The duration runs from the user's
    message, not from the first output.
- **T3 group summaries** [source] `packages/client-runtime/src/work-log/presentation.ts`
  `summarizeToolGroup`:
  - One sentence joined with commas and "and": "Read 4 files, changed 2 files, and ran 3
    commands".
  - Other actions: "Searched code N times", "Searched the web", "Used browser N times",
    "Used <MCP server> integration".
  - Lifecycle markers that a later "completed" supersedes are dropped from the count.
  - A group of one shows the tool's own label instead of a count.
- **Codex app** folds to "Worked for Xs", with "Analyzed"/"Explored" sub-sections inside
  [reported]. Its bug list is the main caution: the **final reply ends up inside the fold**
  and the turn looks empty (#23221, #47494, #28261). Users ask for a collapse-all for the
  sub-sections (#48860). The TUI groups consecutive successful commands as "Ran N commands"
  with no opt-out, and users who audit agents live object to that (#39903).
- **Claude desktop** has **Transcript view** Normal / Thinking / Verbose next to the send
  button, cycled with Ctrl+O [documented].
  - Normal is "tool calls collapsed into summaries" ("Used N tools" folds).
  - Users ask for Codex-style auto-collapse when the turn ends (#96497), and for interim
    prose to fold into the group when the final reply arrives (#97507).
  - A live bug: interim assistant text was **replaced by a paraphrased one-line summary**
    with no way to see the original (#94354). Lesson: never rewrite the agent's words.
    Fold them; do not paraphrase them.
- **Cursor 3** [documented/reported]:
  - Conversation density Compact / Balanced / Detailed.
  - Reads, searches and MCP calls always fold into "Explored N tools", even on Detailed.
    The forum calls that "a huge bug", because the setting's name promises more than it
    does.
  - Edits and terminal commands are pulled inline at Detailed.
- **Zed** [source] `thread_view.rs`, `entry_view_state.rs`:
  - Plain tool calls are **one-line rows** whose disclosure chevron shows only on hover.
  - Only three kinds get a **card**: those needing confirmation, edits, and terminal
    commands (`use_card_layout = needs_confirmation || is_edit || is_terminal_tool`).
  - Terminal output is truncated, with a header tooltip "Output was truncated…".
  - Thinking has four modes (`ThinkingBlockDisplay`): **Auto** expands while streaming and
    collapses when done (the default), Preview has a constrained height, plus Always
    expanded and Always collapsed.
  - An **"Edits" accordion bar sits above the message editor** (files and line counts),
    with **Review Changes** and **Reject All / Keep All**.
- **Warp** [documented]:
  - "Most render as a one-line status row with a state glyph and a label", e.g. "reading
    a file".
  - Shell commands stream output. Edits are expandable diffs that **collapse to their
    headers once applied**.
  - The approval card replaces the input. `E` expands or collapses all diffs.
  - Hovering a collapsed command shows the full command (PR #16218).

**Approvals.**

- T3 docks the pending approval **in the composer**: "Command approval" in warning ink, the
  app name, a `1/3` counter, and a monospace command capped at 5 lines that scrolls
  [source] `ComposerPendingApprovalPanel.tsx`.
- Zed puts Allow Once / Always Allow / Reject on the card itself.
- Claude: **Allow once / Always allow / Deny** for site actions [documented].
- Sidebar rows flag "needs you" in all of them.

### 2.2 Baaz today [baaz]

The pieces exist, but the policy does not:

- `activity_group` (folded runs of small steps).
- `tool_group` (folded groups, with each call opening into a full card).
- `thinking_block`: open while live, collapsed when done (`folds.open(key, !done)`).
- `skill_load_row` (a quiet "Loaded skill `x`" row).
- A terminal mirror (6 lines).
- Edits fold above 12 changed lines, with diffs capped at 40 rows.
- Approval cards titled e.g. "Allow Muse to run this command?".
- `needs_you_banner`, `status_row`, `retry_row` in the library.

What is missing:

- A **turn-level fold**.
- **Shell cards start open** (`shell_cards_start_open`), which is the largest single source
  of noise.
- A single **live status line**.
- **Folding interim prose**.
- A **rule for long results**.

### 2.3 Proposed design

**Vocabulary.**

| Term | Meaning |
|---|---|
| *step* | One tool call, shown as one row: state glyph, verb, target, elapsed. |
| *run* | Consecutive steps with no prose between them. |
| *turn fold* | The single row a settled turn collapses to. |

**During a live turn** (what is visible by default):

1. The user message.
2. **Interim prose**: verbatim, full width, as it streams. Never summarised or rewritten.
3. **Each run is one row**, not N cards:
   `● Reading crates/baaz/src/app.rs   · 6 files, 2 searches · 14s   ⌄`.
   - The leading label is the **current** step, in present tense, and it is the only text
     that changes.
   - The counts accumulate.
   - When the run ends (prose arrives), the row settles to its past-tense summary
     ("Read 6 files and searched code twice").
   - The chevron opens the run's step list.
4. Four step kinds **break out of the run as their own row**, because the user may act on
   them (Zed's card rule, narrowed):
   - an **edit**: one header row, `Edited app.rs  +12 −3`. The body is closed by default;
     clicking the row expands the capped diff inline, and the `+12 −3` chip opens the file
     in **Changes**;
   - a **command running longer than ~2 s**: the command line plus a **2-line live tail**
     in mono ink-3 (replacing today's 6-line mirror). On exit 0 it settles back into the
     run's count;
   - a **failure**: the step row in error ink with the exit code or error, staying visible;
   - an **approval** (below).
5. **Thinking** follows Zed's Auto mode:
   - while streaming, it is expanded at a **4-line height** that follows its own tail,
     with the label "Thinking";
   - when done, it collapses to `Thought for 12s`.

   This is already Baaz's behaviour except for the height cap.
6. **One live status line**, docked at the top edge of the composer band (not in the
   scrolling transcript, so it never jumps):
   `◌ Working · 1m 12s · Running cargo test` with **Stop** (secondary) on the right.
   It is the single place a glance answers "is it still going?". Retries replace its text
   (`retry_row`).

**After the turn settles** (what remains):

```
You  ▸ make the sidebar rows one line when there is no byline

▸ Worked for 2m 14s · Read 12 files, edited 3, ran 5 commands        +48 −12
  ─────────────────────────────────────────────────────────────────────────
  <final reply, verbatim, full width>
```

- **One turn fold** above the final reply. Its label uses T3's grammar.
  - The **diffstat chip** on the right opens Changes scoped to this turn (or this session
    when there is no per-turn checkpoint).
  - When the user stopped the turn: `You stopped after 40s`.
- Folded inside it: every run, interim prose (verbatim, in order), thinking, approvals
  that were granted, a lone trailing step, and edits that only re-touched files already
  counted.
- Still visible below the fold:
  - **the final reply** (an invariant, tested: a turn whose only assistant text is
    "commentary" shows that text, never an empty turn; this is Codex #28261);
  - **unrecovered failures**, as a one-line error row with **Retry** if the turn itself
    failed;
  - **denied approvals** (`You denied: rm -rf target/`), because the denial shaped the
    answer;
  - **questions the agent asked and the user answered**;
  - **plans and todo lists**;
  - **subagent/handoff cards**.
- **Recovered failures** (a failed command later fixed) stay inside the fold. The fold
  row adds a muted `· 1 failed` so they can be found.
- A turn with no tool work gets **no fold**: a fold must hide something (T3 rule).

**Expanding** (two levels and no more):

- **Fold → steps.** A click on the turn fold (or Space or Enter on it) lists the turn's
  runs and prose in order. Each step is one row and each run is collapsible. The open fold
  has a **"Collapse all"** affordance only on its own header row (Codex #48860), which
  replaces per-section clicking.
- **Step → body**:

| Step kind | Body |
|---|---|
| Command | The command (wrapped, copyable) and the **last 12 lines** of output, with an exit-code chip. When longer: `Show all 340 lines`, which opens the full output in the right pane (Files kind, a read-only doc). The full output is never inlined in the transcript. |
| Read | Path and line range only. A click opens the file at that range in **Files**. File content is never inlined. |
| Search / glob | The query and the first 8 hits; the rest is counted. |
| Edit | The capped diff (40 rows, as today). |
| Skill load, MCP/tool result, web fetch, any long text | The first **6 lines** at ink-3, then `Open`, which shows the whole thing in the right pane's doc view. A skill body opens on the Skills page. **No inline result is ever taller than ~12 lines.** |

**Approvals.**

- Live:
  - An **approval panel docked above the composer**, in the composer band (T3 model).
    Title in warning ink ("Allow Codex to run this command?"), the command in mono (max 5
    lines, scrolls), and a `1/3` counter if several are queued.
  - Action row: hint "⏎ allow once" left, spacer, **Deny** (secondary), **Always allow…**
    (secondary, with a menu of scopes when the provider offers them), **Allow once**
    (primary).
  - A matching quiet marker row at the transcript position ("Waiting for approval: cargo
    test"), so scrolling back shows where it happened.
  - The sidebar row shows the amber "needs you" status, which is already the case.
- After resolution, the marker becomes `Allowed: cargo test` and folds into the turn, or
  `You denied: …` and stays visible.

**One display setting, not three.** Add **Settings → General → Transcript**:
`Tool steps: Fold when the turn ends (default) / Always show`. "Always show" leaves runs
expanded after settle, for people auditing agents (Codex #39903). Do not copy Cursor's
three densities, whose names over-promise (forum #165292), and do not add a Claude-style
dropdown beside Send, because the composer already carries model, mode and effort.
Optional: a keyboard toggle for "expand every fold in this session", without a button.

**What this removes by default**:

- open shell cards, together with their Run/Open buttons on every row;
- 6-line mirrors;
- per-call cards inside groups;
- interim prose stacking above the answer;
- thinking blocks left behind after settle.

**What it must not remove**: the final reply, the agent's own words (fold, never
paraphrase; Claude #94354), failures that matter, and anything waiting on the user.

---

## 3. Right-pane toggles

### 3.1 What the references do

- **Claude desktop** [documented]:
  - A **Views** menu in the session toolbar, plus a shortcut per pane: ⌘⇧D diff,
    ⌘⇧B Browser, ⌃` terminal, ⌘\ close the focused pane.
  - Panes are dragged into any layout and can pop out into a window.
  - The **`+12 −1` diffstat indicator opens the diff view**. File paths in chat open the
    file pane. HTML, PDF and image paths open the Browser.
- **Codex app** [documented]:
  - ⌃⇧G opens the review tab, ⌘⇧E toggles the file tree, ⌘T opens a browser tab, ⌃`
    toggles the terminal, ⌘J toggles the bottom panel, ⌘⇧B cycles the workspace layout
    (full / split / hidden tabs), ⌘⇧F toggles full view.
  - The right side is **tabs**.
- **T3 Code** [source] `PanelLayoutControls.tsx`, `RightPanelTabs.tsx`:
  - The header has **exactly two toggles**: "Toggle terminal drawer" and "Toggle right
    panel", the second with an agent count in its label.
  - The right panel has a **tab strip of surfaces** (browser, terminal, diff, files, PR,
    device) with a **+** menu.
  - An empty panel shows an "Open a surface" launcher: one row per kind, each with a
    **single-key shortcut**.
- **Cursor 3** [documented]:
  - Right-panel tabs for Files, changes, canvases, PRs, browsers and terminals.
  - Any tab can go **full-screen** (⌘⇧M), leaving a floating prompt bar.
- **Zed**: changes surface as the **Edits bar above the composer**, which opens a
  **Review Changes** multibuffer [documented].
- **Conductor**: ⌘⇧D opens the diff viewer, and a Checks tab covers git, CI and todos
  [documented].

Common pattern: **one or two toggles in the header, the choice of *what* lives inside the
pane (tabs), a shortcut per kind, and contextual entry points that do most of the opening.**
Nobody puts four kind-icons in the main header.

### 3.2 Baaz today [baaz]

- The centre header has a terminal toggle and a right-pane toggle (⌘⌥B), plus overflow.
- The right header cell shows only the kind's **title** (`hd-right-title`). Switching kind
  happens through ⌘K rows or step verbs, so there is no visible way to switch.
- Four kinds: Browser, Diff, Git, Files. *Diff review* and *Git changes* are two surfaces
  for one question ("what changed?").

### 3.3 Proposed

1. **Merge Diff and Git into "Changes".** One surface with a scope control in its body:
   `This turn · This session · Uncommitted` (the third is the git working tree, with the
   staging actions). Every reference has one diff surface.
2. **Right header cell = a 3-segment control** `Changes · Files · Browser` (text labels,
   not icons: three short words fit at the pane's minimum width), left-aligned, then a
   spacer and the pane's one **close** button. This matches the owner's round-2 rule
   ("tabs live in its header cell with one close control"). The active segment gets a
   filled ground. No **+**, no per-tab close, and no browser-like tab strip: Baaz has one
   surface per kind.
3. **Centre header unchanged**: terminal toggle, right-pane toggle, overflow. The right
   toggle reopens the last kind. Its tooltip names the kind ("Show Changes ⌘⌥B").
4. **Shortcuts, one per kind, aligned with the references**: ⌘⇧D Changes (Claude,
   Conductor), ⌘⇧E Files (Codex), ⌘⇧B Browser (Claude), ⌘⌥B toggle pane (kept), ⌃`
   terminal (kept, as everywhere).
   - A kind shortcut **opens** the pane on that kind, or **closes** it if that kind is
     already showing.
   - The `right:<kind>` step verb stays open-only and idempotent (CLAUDE.md).
   - All of these appear in Settings → Shortcuts under *Panes*.
5. **Contextual entry points do the work**:
   - the turn fold's and the edit row's diffstat chip opens **Changes**, scoped;
   - a file path in prose or a step opens **Files** at the line;
   - a `localhost`/http URL opens **Browser**;
   - `Show all N lines` / `Open` on long results opens the doc view in **Files**.
   
   Optional, later: Zed's Edits bar idea as a quiet `3 files changed +48 −12` line in the
   composer band, only while a session has unreviewed changes.
6. **The terminal stays the bottom dock.** Do not add it as a right-pane kind. T3 and
   Cursor allow it, but Baaz's dock is a decided, shipped design.
7. **Optional: full-width pane** (Cursor ⌘⇧M), for Changes on a big review. Defer until
   asked.

---

## 4. Sidebar row density and the active row

- **Claude desktop**: **one line**, a hollow status circle in the glyph column, a
  truncated title, a rounded filled ground on the active row, small status dots
  ([observed] round 4 §1.5).
- **T3 Code**: **one line, `h-9` (36 px)**, title, compact relative time on the right.
  The active row gets the row surface and full-ink title, while inactive titles sit at
  muted ink and brighten on hover or focus [source] `Sidebar.tsx` `SidebarThreadRow`.
  Pills only for states (Woke, settle).
- **Codex**: title plus preview, a spinner when running, a blue dot when unread, amber when
  awaiting approval ([reported] round 4).
- **Synara**: inherits T3, and adds Compact / Comfortable / Spacious density
  [source] `_chat.settings.tsx`.
- **Baaz** (O3): two lines, title then byline, elapsed on the right.

Recommendation:

- Keep O3's two lines **only when the byline exists**. With "Summarise sessions in the
  sidebar" off, rows become **one line (32 px)**. With it on, every row is **two lines
  (48 px)**.
- Choose the height **per mode, not per row**: the virtualised list wants uniform heights,
  and mixed heights read as jitter.
- **Active row**: the filled ground (surface step) plus full-ink title. No accent bar, no
  bold, no colour, which matches round 3's "only the active session gets a highlight".
- **Status**: in the glyph column, the way Claude and T3 do it. A spinner while running,
  amber for needs-you, a dot for unread, nothing when idle. Elapsed time stays right-
  aligned at ink-3.
- While Settings or Skills is open, the active row keeps its ground, because clicking it
  is the way back.

---

## 5. Open questions for the owner

1. Settings keeps the sidebar (recommended) rather than swapping it for the settings nav
   (T3/Synara/Codex). Confirm.
2. Is merging *Diff review* and *Git changes* into one **Changes** kind acceptable? It is
   the change that makes a 3-segment header fit.
3. Should interim prose fold into the turn fold after settle (T3, Codex; requested in
   Claude #97507), or stay above the final reply? Recommended: fold.
4. Is an Archived section in Settings wanted, or should archive stay a sidebar view?

## 6. Not verified

- Codex app screenshots (header buttons, the exact fold wording beyond "Worked for" and
  "Analyzed") were not seen. The claims rest on docs and issue text.
- Conductor's panel layout beyond its shortcut docs.
- Cursor's actual row anatomy.

---

## 7. Sources

- T3 Code, github.com/pingdotgg/t3code @ 54084ae1:
  - `apps/web/src/components/settings/SettingsSidebarNav.tsx`, `SettingsBreadcrumb.tsx`,
    `SettingsGroup.tsx`, `settingsLayout.tsx`, `settingsSearch.ts`
  - `apps/web/src/routes/settings.tsx`, `apps/web/src/hooks/useNavigateBack.ts`
  - `apps/web/src/components/WorkspacePageContainer.tsx`, `AppSidebarLayout.tsx`
  - `apps/web/src/components/chat/MessagesTimeline.logic.ts`,
    `ComposerPendingApprovalPanel.tsx`, `ComposerActivityStatus.tsx`,
    `PanelLayoutControls.tsx`
  - `apps/web/src/components/RightPanelTabs.tsx`, `Sidebar.tsx`
  - `packages/client-runtime/src/work-log/presentation.ts`, `docs/user/thread-sidebar.md`
- Synara, github.com/Emanuele-web04/synara: `apps/web/src/settingsNavigation.ts`,
  `apps/web/src/routes/_chat.settings.tsx`; trysynara.com/docs.
- Zed, github.com/zed-industries/zed: `crates/agent_ui/src/conversation_view/thread_view.rs`
  (`render_tool_call`, `render_terminal_tool_call`, `render_thinking_block`,
  `render_edits_summary`), `crates/agent_ui/src/entry_view_state.rs`;
  zed.dev/docs/ai/agent-panel.
- Claude desktop: code.claude.com/docs/en/desktop (Arrange your workspace, Switch view
  modes, Keyboard shortcuts, Review changes with diff view); anthropics/claude-code issues
  #93503, #94354, #96497, #97507.
- Codex: learn.chatgpt.com/docs/reference/commands, learn.chatgpt.com/docs/reference/settings,
  developers.openai.com/codex/app/review; openai/codex issues #23221, #26160, #28261,
  #39903, #47494, #48855, #48860.
- Cursor: cursor.com/changelog/3-4 (Full-screen Tabs and Compact Chats);
  forum.cursor.com/t/new-version-hides-agent-tool-call-details-in-defiance-of-setting/165292;
  forum.cursor.com/t/cursor-3-agents-window/156509.
- Warp: docs.warp.dev/agents/cli/agent-conversations/; warpdotdev/warp PR #16218.
- Conductor: conductor.build/docs/concepts/workflow.
- Baaz: `crates/baaz/src/settings.rs`, `settings_providers.rs`, `layout.rs`, `keymap.rs`,
  `transcript.rs`, `right.rs`, `app.rs`; `docs/diagnosis/round4-reference-apps.md`.
