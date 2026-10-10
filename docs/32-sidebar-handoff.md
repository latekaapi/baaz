# 32 — Handoff: sidebar round done, owner bug round next (2026-10-11)

## State (nothing pushed)
- **agentic-ui** `design-v2` @ `48ba59e`. **harness** `feedback-mode` @ `bd0fa2d` (+ this doc). Both trees clean.
- harness `Cargo.toml` still pins aui **v0.3.21**; the git-ignored `.cargo/config.toml` path-patches `aui`, `aui-cef`, … to
  `../agentic-ui`. Repin only after the owner pushes a tag.
- Bundle: `CEF_PATH=$HOME/.local/share/cef scripts/bundle.sh` → `target/bundle/Baaz.app` (currently running = latest build).
- Canvas "Baaz sidebar v2" https://claude.ai/artifact/GACTNjKyXA9necGvwPvovP **v26** (v25 = the sidebar build; v26 adds NewTab2 for approval + BrowseFixes as built); repo copy in `docs/30-sidebar-spec/`.
- Gates last run green: `cargo test -p baaz` 1486 · `cargo test -p aui -p aui-webview` 628 · `aui-cef` (own target dir,
  `cd crates/aui-cef && cargo test`) 44 · clippy `-D warnings` on all of them. Gallery clippy has one pre-existing failure
  (`aui-gallery/src/browse/screen.rs:2235`, not ours).

## Read first
1. This file. 2. `docs/31-fluid-design.md` §6–§8 (FF is the spec; what was built and adapted). 3. `docs/31a-ff-component-values.md`.
4. Memory: `feedback-ff-is-the-spec`, `project-sidebar-finish-2026-10-11`, `feedback-live-verify-without-screen-control`.
5. `CLAUDE.md` (18-entry probe; build first; baselines only on main).

## What this round built (where to look)
| Area | Code |
|---|---|
| Sidebar screen (Sessions + Browse), menus, view options, Browse menus | aui `crates/aui/src/screens/sidebar/{model,view,menus}.rs` |
| FF tooltip, lit icons, disabled 50 % | aui `data/tooltip.rs`, `data/button.rs`, `aui-icons/build.rs` |
| Scrollbar (sidebar list), `Palette::scrollbar_thumb` | `view.rs::list_scrollbar`, `aui-tokens/src/lib.rs` |
| Collapse: pinned toggle + header clear | aui `shell/app_shell.rs` (`edge_control`, `centre_clear`) |
| Browser spaces / folders / migration (Personal + folder per project) | harness `browser/tabs.rs` (`ensure_spaces` …), `browse_space()` in `browser/mod.rs` |
| Browse intents (tab/folder/task/space menus, Ask, close all, delete-space dialog) | harness `browser/sidebar_actions.rs`, `sidebar_view.rs::browse_intent` |
| Overlay window (Browse menus, the ⌘L Field) | harness `overlay.rs` |
| Page card (inset 8, radius 10, hairline) + native corner radius | harness `app.rs::render_browse_centre`, `browser/mod.rs::page_card_rect`; aui-cef `shim.m::aui_cef_set_corner_radius` |
| Page scrollbar CSS | aui-cef `backend.rs::SCROLLBAR_JS` |
| Apple Music prompt fix (HardwareMediaKeyHandling off, MERGED into CEF's list) | aui-cef `runtime.rs::merge_features` |
| View menu wiring, Sort by, Clear empty, Search all | harness `sidebar_view.rs::apply_view_option`, `app/list.rs`, `layout.rs::SortBy` |

Owner decisions this round (don't re-ask): view rows back (Show hidden, Clear empty, Search all); PR status disabled "Soon";
Sort by Recent/Created/Name; width 256; Browse boards approved (New tab under your tabs); Personal + project folders;
overlay windows over the page; task folders = status dot + word; horizontal strip only with the sidebar hidden; page = card.

## Traps (cost real time this round)
- **Live checks without computer use** (owner declines it): run the bundle binary isolated —
  `env -i HOME=$HOME USER=$USER PATH=/usr/bin:/bin TMPDIR=$TMPDIR BAAZ_STATE_DIR=$(mktemp -d "$TMPDIR/bz.XXXX") target/bundle/Baaz.app/Contents/MacOS/Baaz --no-connect --login signed-in --steps '…;…'`
  then `screencapture -x -o -l <CGWindowID>`. Helper scripts in the old session scratchpad are gone — rebuild a tiny
  Swift `CGWindowListCopyWindowInfo` id-lister. `--steps` items are `;`-separated; `wait:<ms>` exists.
- **Overlay panel in a background launch** never holds the key → resign-close fires at birth. Use `BAAZ_OVERLAY_STICKY=1`
  (+ `BAAZ_OVERLAY_TRACE=1`). The panel is its own window: capture it separately and composite at its anchor (×2 Retina).
- Never refresh the overlay per frame blindly (looped: 23k renders/10 s); it repaints on a content fingerprint.
- Opening a gpui window inside a Harness update/render double-leases → open from a spawned task at app level.
- CEF: a second `--disable-features` REPLACES CEF's own list and crashes window creation; always merge.
  `MediaSessionService` must stay on.
- A script that opens a page then focuses the address loses the field when the page loads (page-took-keyboard rule) —
  add `wait:` before `omnibox:`.
- Step verbs added: `folder-new:<name>`, `space-new:<name>`; `tab-menu:<n>` opens the new Browse menu.

## Owner bug round (2026-10-11 late) — four fixed, one waiting on approval
Screenshots: a double frame round the page card, no divider above Personal, the Field's double top edge
and empty box, no loader, an undesigned new tab. Owner decisions asked up front: page frame = **FF inset
card** (8 px, no hairlines); loader = **line + tab spinner**; new tab = **Field + pick-up, redesigned**.

| Fix | Root cause | Commits |
|---|---|---|
| Divider above the spaces strip, always | the strip had none; only the task pane drew one | aui c8d1624 |
| Page card = FF `variant=inset` | card hairline beside the window rim and the sidebar border | aui 33322b4 (`AppShell::inset`), harness f866822 |
| Field: no empty box, single edge | gpui opens a `PopUp` as a **titled** NSPanel → the window server rims + shadows the whole fixed transparent rect; plus the input's own ring | harness 237bfd7 (`setHasShadow:NO`, 4 px shadow margin, FF `DropdownSearch` row) |
| Loader | old line lived in the centre header Browse never draws; and the cached sidebar pane re-armed inside the draw, so a quiet window never repainted it until the load ended | aui 48ba59e (`Spinner::fluid`, `BrowseTab.loading`), harness bd0fa2d (card-edge line, `request_animation_frame` on a key change) |

Adaptations written down: the progress line rides the 8 px gap above the card (the native page covers anything
drawn over it); `Spinner::fluid` turns in 2 s like FF but does not breathe its dash; task-folder tabs keep their
favicon while loading (the groups key carries no page facts). Seen live in the bundle (scripted runs +
`screencapture`): no sidebar border, card 8 off the rim, line at 10 % then gone, tab arc spinning then the
favicon, Field with one edge and nothing below its rows.

**Waiting on the owner:** canvas v26 board "NewTab2" (Field on FF composer chrome, ⌘↵ asks, "Pick up where you
left off" as FF menu rows with project · age) — build only after approval. Board "BrowseFixes" = the four fixes.
Not reproduced: the "border appears on hover" over the spaces strip (no pointer hover in scripted runs) — the
always-on divider should make it moot; owner to confirm. Browse right-click menus still carry the native rim
(their window is exactly menu-sized, so it reads as one edge); same `drop_native_chrome` + margin if wanted.
The 18-entry probe reports findings on every entry against pre-design-v2 baselines; none of them draws Browse.

## Still open (known, not yet done)
- Collapse animation not reviewed frame by frame since the fix — ask for a recording, review with ffmpeg contact sheets.
- Hover-only states not seen live: Group/Sort submenus, Move to folder submenu.
- Task pane only seen in the gallery (a scripted run can't give sessions tabs) — check with the owner's real sessions.
- ⌘S hide strip (R1), light theme review, Apple Music prompt confirmation from the owner.
- Centre content still reflows during collapse (FF-faithful; a slide would need a jump elsewhere).

## Method (unchanged)
Mock first on the canvas for anything new, owner approves; Fable/Opus writes the gpui code; Sonnet only for tests,
mechanical edits, research; tokens only; every interactive element role + label; gates = aui + baaz (+ aui-cef when touched)
tests and clippy, the 18-entry probe (`cargo build -p baaz` first), then look live in the bundle. Commit per fix; never push.
