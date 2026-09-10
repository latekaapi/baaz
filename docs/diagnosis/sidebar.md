# Section B — Sidebar (items Sidebar 1–8)

Reference screenshots (this pass):
`shots/sidebar-open-dark.png` (harness, replay `transcript-real.jsonl`),
`shots/sidebar-rename-dark.png` (harness, `--steps rename`),
`shots/sidebar-aui-card21.png` (gallery `sidebar/sidebar`: library sidebar + rail).

Collapsed harness state could not be screenshotted: there is no `--steps`
verb that toggles the sidebar (`Harness::step` in
`crates/harness/src/app.rs:825-856` handles `search/palette/resume/
fork-picker/rename/hidden/empty` only), and adding one would mean modifying
the app, which is outside this read-only pass. Item 1 therefore cites code
paths instead of a harness-collapsed screenshot.

Library anchors below are under `/Users/latekaapi/Projects/agentic-ui`;
`gpui-pre`/`gpui-kit`/`gpui-component` paths are under
`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/`.

---

## Sidebar 1 — Collapsed sidebar is empty

1. Symptom (owner): "In the collapsed state, the sidebar is empty."

2. Root cause. Collapsing is a shell state (`sidebar_open: bool`,
`crates/harness/src/app.rs:221`, toggled at `app.rs:2051-2054`). The shell
(`crates/aui/src/shell/app_shell.rs:150-224`) renders the collapsed column
from `self.rail`, falling back to nothing:

- `app_shell.rs:156-170` — when closed the column shrinks to `rail_width`
  (72 px with traffic lights, `RAIL_WIDTH_WITH_LIGHTS`), and `pane` becomes
  `self.rail`; "without a rail the column is simply empty"
  (`app_shell.rs:128-131`, doc on `AppShell::rail`).
- The harness never sets a rail: `grep \.rail( crates/harness` is empty,
  and the shell assembly at `app.rs:1977-2002` sets `.sidebar(sidebar)` but
  no `.rail(...)`. So the collapsed 72 px column renders the column
  background and divider only — the observed empty state. The aui
  `Sidebar::collapsed(true)` path (`crates/aui/src/nav/sidebar.rs:326-335`)
  is never used by the harness at all (the harness uses `sidebar_view`, not
  `sidebar()`).

3. Library inventory. The collapsed form exists and is fully designed:
`rail(id, items)` (`crates/aui/src/nav/rail.rs:127-130`), 48 px,
`RailItem::nav/separator/session` (`rail.rs:36-61`), avatar bottom
(`rail.rs:142-145`), tooltips-on-hover per the card-21 legend. And
`SidebarNav::rail_items()` (`sidebar.rs:199-226`) derives the rail from
expanded data (nav glyphs, warning badge, one cell per non-idle session).
`shots/sidebar-aui-card21.png` (right half) shows it: nav icons, session
dots in state colours, `B` avatar. Fix belongs in harness (supply a rail);
no library change needed except whatever nav-model mapping the harness
chooses (see item 2).

4. Proposed fix. In `Harness::render` (`app.rs:1977`) add
`.rail(harness_rail)` where `fn harness_rail` (new, `app.rs` near
`render_sidebar`) builds `aui::nav::rail("rail", items).flat(true)
.avatar(identity.initial)` with `items` = 1–2 nav cells (search via
`RailItem::nav("search", IconName::Search)`, new-session via
`RailItem::nav("new", IconName::Plus)`) + `RailItem::separator()` + one
`RailItem::session(id, state)` per running/waiting session
(`SessionEntry.running`, `sidebar.rs:36`), selected/pulse mirroring the
row. Wire `on_select` → `resume`, `on_action("search")` → `focus_search`.
`flat(true)` is required inside the shell column (double border otherwise;
`rail.rs:132-139`). gpui API: none beyond existing component; cursor/
tooltip come free with the rail. Size S. Risks: rail session dots need
`AgentState` mapping (running→Running, else Idle; `sidebar.rs:123-129`
already does this). Test: add a `--steps` verb or reuse `hidden` toggle
pattern to script collapse; screenshot collapsed 72 px column showing nav
+ dots + avatar. Proves item 1.

5. Open questions. None on cause. Whether the rail should also carry
pinned-session dots vs active-only is an item-2/7 design call.

---

## Sidebar 2 — No "New Session" action; no Tasks/Automations/Inbox/Workspaces structure

1. Symptom (owner): "Above 'Sessions', add an action to create 'New
Session' … In the similar style, we want New Session, Automations and
then the Sessions list."

2. Root cause. The harness sidebar body is a bare `SidebarView` date
grouping (`app.rs:1475-1479`: `sidebar_view("sessions",
grouping).caption("Sessions")`), with no primary-nav section and no create
affordance. Session creation exists only as ⌘N (`NewSession` action,
`app.rs:2027`) and the palette's resume flow — nothing clickable in the
sidebar. There is no Automations/Inbox concept anywhere in the harness.

3. Library inventory (exact names/props/intents):
- Primary nav rows: `SidebarNavItem::new(name, label, glyph)` + optional
  `.count(..)` / `.warning()` (`sidebar.rs:40-70`); rendered by
  `nav_item(id, glyph, label)` (`nav/parts.rs:57-60`) at 30 px rows in an
  8 px-padded block (`sidebar.rs:340-354`). Gallery reference
  (`aui-gallery/src/cards/sidebar.rs:20-56`): `Tasks` (count 7),
  `Automations`, `Inbox` (count 2, warning) inside `SidebarNav::new
  (workspace, footer)` (`sidebar.rs:158-170`).
- Group header row ("Workspaces" caps row + sliders view-menu + add):
  `Sidebar::groups_label` defaults to `"Workspaces"` (`sidebar.rs:165`);
  group row with `on_view_options`/`on_add` (`sidebar.rs:357-361`). The
  harness's `SidebarView::caption("Sessions")` + `on_view_options`
  (`views.rs:176-206`) is the grouping-level equivalent, but the harness
  never sets `on_view_options`, so no sliders icon appears.
- "New" affordances that exist: header `"new"` action name
  (`sidebar.rs:310`, search/new/collapse icons); group-row `"add"` action
  (`sidebar.rs:360`); assistant palette item `new-session` (⌘N)
  (`aui-gallery/src/assistant/view.rs:644,706-708`). There is no literal
  "New task" row anywhere in the gallery (searched); "New session" is the
  existing vocabulary.
- Collapsible groups with counts and pinned-first ordering:
  `SidebarGroup::new(id, label).count(..).closed()/.trailing(..).session(..)`
  (`sidebar.rs:89-118`); card 21 shows `Pinned 3` open first, then
  `acme-web 4`, closed `acme-internal`, `Done 37`
  (`cards/sidebar.rs:28-55`). The harness uses `Grouping::Date` only
  (`sidebar.rs:147-160` in harness), so it cannot express Pinned.
- Footer with usage meter: `sidebar_footer(..).meter(provider, usage)`
  (`nav/parts.rs:281-290`, `sidebar.rs:398-400`); assistant reference
  `sidebar_footer("assistant-footer", "B", "Bharani").meter(Provider::Claude,
  0.78).pad_y(8.0)` (`assistant/view.rs:287`).
- Fix belongs in both: harness adopts `Sidebar`+`SidebarNav` (or extends
  `SidebarView` with a nav block); library needs no new component, but a
  harness "Automations" row has no backing data — that row should be
  omitted until a real destination exists rather than added dead.

4. Proposed fix. In `Harness::render_sidebar` (`app.rs:1460-1502`):
replace the `sidebar_view`-only column with `aui::nav::sidebar("side",
nav)` where `nav = SidebarNav::new(workspace_name, account)` +
`.item(SidebarNavItem::new("new", "New session", IconName::Plus))` +
`.groups_label("Sessions")` + one `SidebarGroup` per date bucket
(reusing `sidebar::grouping` output mapped into groups, with a `Pinned`
group first once item 7 lands) + `.selected(..)`; `on_action("new")` →
`new_session`, `on_select` → `resume`. Alternatively (smaller, keeps date
view): keep `sidebar_view` and prepend a `nav_item("new", Plus, "New
session")` block in the scroll column. gpui API: existing components only.
Size M (full `Sidebar` swap incl. header/workspace-switcher reconciliation)
or S (prepend nav row). Risks: the full `Sidebar` brings its own header
(workspace switcher, `sidebar.rs:278-318`) which duplicates the shell's
`sidebar_header` — the S variant avoids that fight. Test: replay
screenshot showing New-session row above Sessions; click test via existing
keystroke-test pattern. Proves item 2.

5. Open questions. Whether "Automations" should appear with no backing
surface — undecidable from code; recommend omitting until it has a
destination.

---

## Sidebar 3 — No per-session description line

1. Symptom (owner): "Each session thread should have some description of
what was done here last … 1 or 2 lines, in the same muted text as per our
sidebar design (eg: the style in which acme-web feature/checkout is shown)".

2. Root cause. `SessionEntry::summary` (`crates/harness/src/sidebar.rs:
132-144`) fills only `SessionSummary { id, label, state, elapsed,
provider, meta: ["N turns"], pulse }` — no `repo`/`branch`/`activity`,
because the harness has no per-session repo/branch data and never asks for
last-message text. The library already renders the muted second line; the
harness just never supplies it.

3. Library inventory. The "acme-web feature/checkout" style is the meta
line: `repo` + `branch` mono tags + `meta` items, ink-3, truncating
(`session_row.rs:276-305`, `meta_items`; `MetaItem::{Text,Tag,Danger,
Warning}` in `nav/types.rs:105-116`). The optional third line is
`activity(kind, text)` (`types.rs:87-90`, rendered at
`session_row.rs:307-320,524-534`) with kinds Working/Waiting/Failed/Plain.
Gallery example: `.repo("acme-web").branch("feature/checkout-flow-v2")`
(`cards/sidebar.rs:31-35`), `MetaItem::Text("PR #2491 open")`,
`MetaItem::Warning("awaiting permission")`. Compact rows render both lines
(`session_row.rs:511-539`). Fix belongs in harness (supply the data);
possibly library if a two-line-clamp variant is wanted — not needed, the
slot already truncates.

4. Proposed fix. What the harness has for free (no turn spent, no new
I/O):
- `IndexEntry { session_name, title, first_user_prompt, search_text }`
  (`index.rs:25-33`), read from the read-only sqlite index
  (`index.rs:83`); `IndexEntry::label` precedence
  (`index.rs:46-53`). `first_user_prompt` is the strongest "what was done
  here" signal and is already inside `haystack` (`sidebar.rs:196-218`).
- `SessionMeta { name, hidden, derived_title }` (`sessions.rs:29-40`) plus
  `first_shell_command` from a `session/read` (`app.rs:1949-1960`, free —
  `session/read` makes no model call, though on a session no host has
  loaded it serves no history per 05-handoff).
- `session/list` gives `turn_count`/status/timestamps only (used in
  `SessionEntry::join`, `sidebar.rs:68-88`).
- The open session's full fold (`MuseFold::session`, turns/blocks) is in
  memory — richest source, but only for the open session.
- NOT free per session: last-message text of background sessions (needs a
  `view/page` or `session/read` per row; `history.rs:30` is the global
  composer prompt history, not per-session messages).

  In `SessionEntry::summary` (`sidebar.rs:132-144`) add
  `.meta(MetaItem::Text(one_line(first_user_prompt or derived_title)))`
  and, when present, `.activity(ActivityKind::Plain, …)` as the second
  line; keep the "N turns" meta after it. Cap with existing `one_line`
  (`sidebar.rs:222-228`, 80 chars). gpui API: none new. Size S. Risks:
  prompt text may be long/noisy — one_line + row truncation bounds it;
  stale index rows (cache; empty-map failure mode per 05-handoff §5.4) must
  keep falling back to `UNNAMED`. Test: replay screenshot with an index
  fixture carrying `first_user_prompt`; rows show two muted lines.
  Proves item 3.

5. Open questions. Auto-generated summaries beyond first-prompt reuse
would need a model call per session — explicitly out of scope without an
owner decision on spend.

---

## Sidebar 4 — Content flush right, unequal margins

1. Symptom (owner): "The contents of the sidebar are flushed to the right.
There is no margin. The left and right margin should be equal."

2. Root cause. Measured on `shots/sidebar-open-dark.png` (ImageMagick
pixel scan of the selected row at y=130): row background starts at x=8
and runs to x≈249, with the column divider at x≈252 — i.e. 8 px left
gutter, ~2–3 px right gutter. Code declares symmetric margins:
`CompactSessionRow` sets `.ml(8).mr(8)` (`SR_MARGIN_X`,
`session_row.rs:54-58,549-552`) on a `w_full` row
(`session_row.rs:541`) inside `rows()`' `v_flex().w_full()`
(`views.rs:241`) inside the harness scroll wrapper
`div().id("sessions-scroll").flex_1().overflow_y_scroll().child(view)`
(`app.rs:1491-1499`). Likely mechanism (verify by fixing and
re-screenshotting): `w_full` resolves against the full column width and
the horizontal margins then overflow to the right, where the shell
column's `overflow_hidden` (`app_shell.rs:205-215`) clips them — leaving
the left 8 px intact and ~0 px on the right (plus rounding from
`R_MD`). A ±1 px contributor is the shell's right divider border drawn
inside the 252 px column (`app_shell.rs:205-215`). The gallery card 21
(`shots/sidebar-aui-card21.png`, left panel) shows near-equal gutters
because the standalone `Sidebar` card (`sidebar.rs:405-417`) sizes the
same `margin_x(8)` rows (`sidebar.rs:386-390`) inside its own bordered
box rather than a clipped shell column — same row code, different
container.

3. Library inventory. Gutter tokens live with the rows, not the shell:
`SR_MARGIN_X 8.0` / `SR_PAD_*` (`session_row.rs:54-64`), `NAV_PAD 8.0`
(`sidebar.rs:26`), `SEARCH_MARGIN_X 8.0` (`parts.rs:425-431`), footer
`FOOTER_PAD_X 12.0` (`parts.rs:33-36`). No shell-level content padding
exists to compensate. Fix belongs in harness first (container), possibly
library (row sizing idiom) if the same clip reproduces in `SidebarView`
consumers.

4. Proposed fix. In `Harness::render_sidebar` (`app.rs:1486-1501`):
give the scroll wrapper symmetric horizontal breathing room and stop the
rows from overflowing it — either `px(8)` on the scroll div and rows with
`ml/mr 0`, or keep row margins and change the row width idiom from
`w_full`+`mx` to `flex_1`/`min_w(0)` so margins are inside the layout
(the library-side equivalent edit would be in `CompactSessionRow::render`,
`session_row.rs:541-552`). Do both sides consistently (caption `group_row`
`GROUP_PAD 12`, `parts.rs:21-24`, and `date_group_header` `DG_PAD_X 12`,
`views.rs:33-38`, are already symmetric). gpui API: layout only, no new
API. Size S. Risks: changing the library row idiom affects all three
groupings and card parity (≤2 % rule) — prefer the harness-container fix.
Test: re-run the same replay screenshot command and re-scan row y≈130;
accept 8/8 px (±1 for radius/border). Proves item 4.

5. Open questions. None — mechanism is measured; only the choice of
container-side vs row-side fix is review taste.

---

## Sidebar 5 — Footer design; "Show empty"/"Clear empty" placement

1. Symptom (owner): "The sidebar footer is not designed properly. It
should look exactly like the sidebar design in agentic-ui library. What
is show empty/clear empty? Can you rethink this UI."

2. Root cause. Two compounded facts. (a) The harness footer
(`app.rs:1568-1636`) starts from the library `sidebar_footer`
(`parts.rs:281`) but replaces its identity: `.trailing(Sign out button)`
instead of the design's `.meter(provider, usage)` + chevron
(`sidebar.rs:398-403`; assistant reference `view.rs:287`), and adds
`.detail(email)` + `.plan(tier label)` + `plan_trailing(toggle buttons)`.
(b) The toggles exist because of the sidebar-noise change: screenshot/test
runs leave dozens of zero-turn sessions, so empties are hidden by default
(`SessionEntry::is_empty`, `sidebar.rs:118-120`) and the footer carries
"Show empty (n)"/"Hide empty"/"Clear empty" (`app.rs:1608-1634`). Full
rationale in `docs/CHANGELOG.md:157-186` and the footer contract in
`docs/02-app.md:146-161` (toggles appear only when n>0; stacked
right-aligned rows because the plan label truncates first).

3. Library inventory. The reference footer (`nav/parts.rs:264-418`):
avatar + name (+ optional `detail` email line, `parts.rs:297-300`, and
`plan` entitlement line, `parts.rs:311-314`) with `meter(provider,
fraction)` + chevron, or a custom `trailing` element (`parts.rs:327-331`);
`plan_trailing` is the one sanctioned overflow slot
(`parts.rs:316-325`). Design rules (`04-design-rules.md:17`): "Provider
usage lives in the sidebar footer." The library footer has no room for
list-management toggles by design — that is why they look alien there.
Fix belongs in both: harness moves list management out of the footer;
library gains nothing (or a `trailing` menu if the account-menu pattern
is adopted).

4. Proposed fix. In `Harness` (`app.rs`): (i) restore the footer to the
reference shape — `sidebar_footer(initial, name).detail(email)
.plan(tier).meter(Provider::Muse, usage_fraction)` + keep Sign-out inside
an account click/menu rather than a trailing button; (ii) relocate
Show/Hide-empty + Clear-empty into the caption row's view-options menu
(`SidebarView::on_view_options`, `views.rs:202-206`, sliders icon +
`view_menu`, cf. gallery `sidebar/views` entry) — i.e. where the library
says list options live ("The sliders icon on the Workspaces row opens the
view options", card-21 legend) — or into the empty-state body
(`render_sidebar_empty`, `app.rs:1528-1565`, which already names the
toggle). Keep the Undo toast (`app.rs:2064-2101`, `unhide_newest`) as the
archive/undo pattern. gpui API: `aui::nav::view_menu` (see
`cards/views.rs:7` usage), `popover_layer` for the menu. Size M (menu +
footer restore + steps coverage). Risks: discoverability of the empty
filter drops — mitigate by keeping the empty-state hint text that names
the menu location. Test: replay screenshots footer-only (matches card 21:
avatar, name, meter, chevron) + open view-menu screenshot. Proves item 5.

5. Open questions. Whether the tier/plan line stays in the footer (it is
harness-specific and warning-tinted; recommend keeping — it is the one
entitlement the footer is for) vs moving with the toggles.

---

## Sidebar 6 — Resizable sidebar (research)

1. Symptom (owner): "The sidebar is not resizeable. Resizing panes is a
key component. It should work flawlessly and be very
responsive/performant."

2. Current state. No resizable/split-pane component exists in `aui` or
`gpui-kit`: case-insensitive grep for `resiz|splitpane|split_pane|
drag.*width|sidebar_width` over `crates/aui*` finds only the static
`SIDEBAR_WIDTH` constants (`shell/app_shell.rs:15`,
`nav/sidebar.rs:14`), terminal grid `resize(cols, rows)` (character
cells, unrelated), and image-resize helpers. `gpui-kit` 0.6 is a
re-export facade (`src/lib.rs` re-exports `gpui`, `gpui-base`,
`gpui-component`), so it contributes no pane component either. Width is a
fixed constant plumbed as `AppShell::sidebar_width`
(`app_shell.rs:29,66-69`, default 252). Zed's source is not on this
machine (`find ~ -maxdepth 4 -type d -name zed` empty) — the Zed notes
below are from memory and marked not verified.

3. Verified gpui-pre 0.3.3 APIs (all paths checked on disk):
- `div().on_mouse_down(MouseButton, f)` (`elements/div.rs:126`), also
  `on_mouse_up` (`div.rs:210`) and `on_mouse_move` (`div.rs:303`); move
  fires only while hovered (`div.rs:303-315`, `hitbox.is_hovered` gate).
- `MouseMoveEvent { position: Point<Pixels>, pressed_button,
  modifiers }` (`interactive.rs:494-518`) with `dragging()` = left held.
- Cursor: `style.mouse_cursor` → `window.set_cursor_style(style, hitbox)`
  (`div.rs:2514-2519`); `Window::set_cursor_style` is paint-phase only
  (`window.rs:3699-3707`); `CursorStyle::ResizeLeftRight` (ew) and
  `ResizeColumn` both exist (`platform.rs:2338-2405`).
- `App::stop_propagation` (`app.rs:2281`) for the press.
- Animation: `spring_px((id, "sidebar-width"), target,
  SpringKind::Layout, …)` (`app_shell.rs:158`); the shell already keeps
  pane resting width while the column springs so content slides under the
  divider instead of reflowing (`app_shell.rs:162-170`).

4. Recommended design (harness state + small library addition).
Render a 5–7 px transparent handle div, absolutely positioned over the
sidebar/centre divider, full height, with the resize cursor. On left
`on_mouse_down`: set `resizing=true`, record `grab_x`/`start_w`,
`stop_propagation`. While `resizing`, render a full-window transparent
overlay capturing `on_mouse_move` (needed because handle-hovered move
stops firing once the pointer outruns the 5 px strip) and `on_mouse_up`
(clears `resizing`, persists). Each move sets `sidebar_width =
clamp(start_w + (pos.x - grab_x), 180, 420)` + `cx.notify()`. During the
drag, bypass the layout spring (feed the width straight through, or add
an `AppShell` flag) — otherwise `spring_px` chases a moving target and
the pane lags the pointer; re-enable the spring on mouse-up for the
settle. Persist to the harness store (`store.rs:22-50`, `support_dir` +
`write_atomic`/`read_json`; same pattern as `sessions.json`) and pass via
existing `.sidebar_width(..)`. Cost: one `cx.notify` + relayout per move
event — the sidebar/centre elements rebuild every frame anyway (pure
render fns; `docs/02-app.md` §2), so a drag frame costs the same as any
other frame; no per-frame allocation beyond the existing tree. Clamp
[min 180, max ~420] keeps rows (min content ~200 px at 12.5 px text) and
transcript usable. Size M (handle + overlay + store + spring-bypass flag
in `AppShell`). Risks: pointer capture outside window (mouse-up missed —
mitigate by also clearing on window blur/focus loss); text selection in
transcript during drag (overlay swallows moves while up); HiDPI: all
`Pixels`, no conversion. Test: scripted drag is hard headless — verify
by (a) replay screenshots at widths 180/252/420, (b) manual drag
smoothness check, (c) unit test on the clamp fn. Zed parallel (not
verified): same handle + window-move + clamp + persisted width shape.

5. Open questions. Exact min/max (180/420 are proposals from row metrics,
not owner-confirmed); whether width persists per-workspace or globally
(`sessions.json` is workspace-scoped — recommend global tier-style file).

---

## Sidebar 7 — Row hover actions differ; need pin / rename / archive-with-confirm

1. Symptom (owner): "On the session list in the sidebar — when we mouse
over, the icons/buttons shown in the original agentic-ui library is
different from how it is shown here. We need Pin, Rename, Archive (after
confirmation, in-place or modal)."

2. Root cause. One line: harness passes `.row_actions(vec!
[RowAction::Rename, RowAction::Hide])` (`app.rs:1475-1479`) and handles
only those two (`app.rs:1468-1474`, `_ => {}` swallows the rest), while
the library default tray is `RowAction::ALL = [Terminal, Browser, Pin,
More]` (`session_row.rs:83-86`). So the owner sees pencil+eye where the
gallery shows terminal/globe/pin/dots. There is additionally no Archive
concept: `RowAction` has no `Archive` variant (`session_row.rs:67-81`),
and `SessionMeta`/`SessionEntry` have no pinned/archived fields
(`sessions.rs:29-40`, harness `sidebar.rs:28-51`) — only `hidden`
(`/hide`).

3. Library inventory.
- Tray mechanics shared by both row kinds: `action_tray`
  (`session_row.rs:111-154`) — absolute, fades/slides in on hover,
  `Xs` ghost 12 px glyphs; compact rows show it only when
  `actions` non-empty and not editing (`session_row.rs:510,564-566`).
- `RowAction::{Pin (Pin icon), Rename (Edit), Hide (Eye), Terminal,
  Browser, More (Dots)}` with glyph+name maps (`session_row.rs:88-109`).
- Grouping for pinned-first exists in gallery (`Pinned` group,
  `cards/sidebar.rs:28-46`) but harness grouping is date-only.
- Confirm patterns in aui: modal `dialog(..)` with `.danger(true)` +
  primary/secondary actions (`overlay/dialog.rs:106-140`); NO inline-row
  confirmation pattern exists in the nav components. Harness precedent
  for destructive undo is the toast with one Undo action
  (`app.rs:2064-2101`, `unhide_newest`) used by hide/Clear-empty.
- Fix belongs in both: library gains `RowAction::Archive` (+ icon) or
  reuses `Hide`; harness gains pinned state, archive flow, Pinned group.

4. Proposed fix. (i) Library (`session_row.rs:67-109`): add
`RowAction::Archive` (archive/box icon) — or rule it out and reuse
`Hide`, decided with owner; gallery `sidebar/rows` entry update per
library gates. (ii) Harness: `SessionMeta.pinned: bool`
(`sessions.rs:29-40`) + toggle in `act` (`app.rs:1468-1474`) +
`sidebar::grouping` emits `Pinned` group first (needs `Grouping`
extension or a second `sidebar_view`; `sidebar.rs:147-160`); row_actions
become `[Pin, Rename, Archive]` with `on_action` handling each; Archive
→ `aui::overlay::dialog` danger modal ("Archive 'name'?") OR toast-Undo
pattern (recommend modal per owner wording "after confirmation", with
toast-Undo as fallback if modal-on-row feels heavy). gpui API: existing
`dialog`, `popover_layer`. Size M. Risks: archive has no wire surface —
define it as hidden+unlisted locally (like `/hide`) and say so; pin
ordering vs date buckets interaction. Test: replay + `--steps`
rename/hide/empty pattern extended with pin/archive steps; screenshots of
hover tray (force via `flags.hovered`? headless hover is hard — at
minimum screenshot Pinned group + archive dialog). Proves item 7.

5. Open questions. Modal vs toast-Undo for archive (owner allows either);
whether Archive reuses `hidden` or needs its own flag (recommend own
flag — Clear-empty must not nuke archives).

---

## Sidebar 8 — Rename field too big, pushes layout

1. Symptom (owner): "when we click rename on the sidebar session item,
the text area that is rendered is too big (and has too much
padding/margin), thus pushing the other things in the layout around."

2. Root cause. Confirmed in `shots/sidebar-rename-dark.png` (sidebar
crop): the editing row is ~2× normal height, bordered box spanning the
row width, provider mark wrapped below. Code chain:
- The library slot just swaps the name for the caller's element:
  `CompactSessionRow::editor` (`session_row.rs:481-484`, rendered at
  `session_row.rs:512-515` inside the same title `h_flex`).
- The harness fills it with a full composer-grade field:
  `rename_field` (`app.rs:1518-1525`) = `div().w_full()` +
  `gpui-component Textarea` built by `aui::composer::composer_state_rows
  ("Name this session", 1, 1)` (`composer.rs:153-155`, i.e.
  `TextareaState::new().placeholder().auto_grow(1, 1)`) at default
  `Size::Medium` with border + appearance on (`textarea.rs:33-44`
  constructor defaults `appearance: true, bordered: true`).
- Medium multi-line editor padding is 10 px horizontal / 8 px vertical
  (`sizing.rs:147-168`, applied at `input.rs:411-420`), inside a compact
  row whose own padding is 4 px vertical / 10–12 px horizontal
  (`SR_PAD_Y 4.0`, `SR_PAD_LEFT 12.0`, `SR_PAD_RIGHT 10.0`,
  `session_row.rs:54-59`). So the editor alone is ~8+8+border+line-height
  ≈ 36+ px tall in a 30 px (`metrics.row`) row — the row grows, siblings
  shift. No aui input component is involved: the row renders whatever the
  caller passes; the oversize styling is all harness-side defaults.

3. Library inventory. No dense single-line input exists in aui (the
composer is the only field owner; `composer_state_rows` is tuned for
composing, not rows). `CompactSessionRow::editor` slot itself is fine.
Fix belongs in harness (dense styling at the call site); optionally
library (a shared `dense_field` helper + gallery entry if a second
consumer appears).

4. Proposed fix. In `Harness::rename_field` (`app.rs:1518-1525`):
render the same `Textarea` with `.appearance(false).bordered(false)`
(borderless look while editing, or a 1 px `line` border via the wrapping
div instead of the component chrome), `.size(Size::XSmall)` (4 px / 0 px
paddings, `sizing.rs:147-168`), `.text_size(FS_12-ish to match `SR_TEXT
12.5`, `session_row.rs:59`) and a fixed single-line height, keeping the
`RENAME_CONTEXT` + `ConfirmRename` commit path unchanged. One-line
height ≈ line-height only → row keeps 30 px, no sibling shift. gpui API:
existing `Textarea` builders (`textarea.rs:49-90`). Size S. Risks:
XSmall may look cramped at 1.1 text scale — check both themes; focus ring
loss from `appearance(false)` — keep an explicit focus border on the
wrapper. Test: re-run the exact rename screenshot command; the editing
row must keep 30 px height and siblings must not move (pixel-diff the two
shots outside the edited row). Proves item 8.

5. Open questions. None on cause; visual sign-off on borderless-vs-haired
editor is review taste.

---

## Dependencies between items

- 1 → 2, 7: the rail's session dots reuse whatever nav/pin model items
  2/7 introduce (`rail_items`, `sidebar.rs:199-226`). Build rail after the
  row-action set is decided.
- 7 → 2, 5: Pinned group changes the grouping both the nav structure (2)
  and the empty-filter counts (5, `is_empty` callers) must handle.
- 5 → 2: the view-options menu is the proposed home for the empty
  toggles; do the nav/caption work once.
- 8 → 7: rename editor styling is independent of which actions open it,
  but touch the same `editing` slot — sequence 7 before 8 or coordinate.
- 6 is independent (shell-level); 3 is independent (data-only); 4 is
  independent (container CSS) but re-verify its pixel scan after any of
  2/7/8 changes row geometry.

## What I ran

- `cargo run -p harness -- --replay fixtures/msp/transcript-real.jsonl
  --theme dark --screenshot docs/diagnosis/shots/sidebar-open-dark.png
  --screenshot-delay 15000` (from `/Users/latekaapi/Projects/harness`)
- `cargo run -p harness -- --replay fixtures/msp/transcript-real.jsonl
  --theme dark --steps "rename" --screenshot
  docs/diagnosis/shots/sidebar-rename-dark.png --screenshot-delay 15000`
- `cargo run -p aui-gallery -- --list` and `cargo run -p aui-gallery --
  --entry sidebar/sidebar --screenshot sidebar/sidebar
  /Users/latekaapi/Projects/harness/docs/diagnosis/shots/sidebar-aui-card21.png
  --screenshot-delay 2000` (from `/Users/latekaapi/Projects/agentic-ui`)
- `magick docs/diagnosis/shots/sidebar-open-dark.png -crop 220x900+0+0
  +repage /tmp/sb-open-crop.png` (+ rename equivalent, 200 % resize) and
  `magick … -crop 260x1+0+130 +repage txt:` row scans for the item-4
  measurement (8 px left / ~2–3 px right at row y=130).
- Read-only greps: `grep -rn "\.rail(" crates/harness` (empty);
  `grep -rn -i "resiz|splitpane|split_pane" crates/aui*` (constants only);
  `find ~ -maxdepth 4 -type d -name zed` (empty — Zed comparison not
  verified).
- No live turns, no `--send`, no git writes; three PNGs under
  `docs/diagnosis/shots/sidebar-*` are the only files created besides
  this report.

