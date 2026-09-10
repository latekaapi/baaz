# Section D — Composer (Chat composer items 1–6)

Static diagnosis from source (harness `main`, `agentic-ui` `main`,
`gpui-pre 0.3.3`, `gpui-kit 0.6` / `gpui-base 0.6.0`). The app was not run;
no screenshots were taken. Every claim below cites the file and line that
was actually read.

## D1. Enter sends nothing; Shift+Enter newline (item 1)

1. Symptom (owner): "Clicking enter should send the message and clicking
   Shift+enter should put a new line" — today Enter inserts a newline.

2. Root cause. Two different crates bind `enter` over the same focused
   textarea, and the harness never opts into the input's submit mode:
   - Harness binding looks correct on its own:
     `crates/harness/src/app.rs:147`
     (`enter` → `SendTurn` in `HarnessComposer && !menu && !field`),
     with the context derived in `app.rs:1741-1766` and installed on the
     window root at `app.rs:1696`. Shift+Enter intentionally matches
     nothing (`app.rs:105-107`, `docs/08-keymap.md:23`), so the editor
     gets it — that half already behaves as the owner asks.
   - But gpui-base's input also binds `enter` in its own `Input` context:
     `gpui-base-0.6.0/src/input/base/state.rs:144-165`
     (`enter` → `Enter{secondary:false,shift:false}`, plus
     `shift-enter` → `Enter{shift:true}`), installed by `init` with
     `CONTEXT = "Input"` (`state.rs:124`).
   - The input's `enter()` handler (`state.rs:1597-1640`) inserts a
     newline whenever `is_multi_line() && !submit_on_enter`
     (`state.rs:1611`), and only takes the submit path (emit `PressEnter`
     + `cx.propagate()`, no newline) when `submit_on_enter` is set.
   - Nothing in the harness or in `aui` ever sets that flag:
     `aui/src/composer/composer.rs:153-155` (`composer_state_rows` sets
     only placeholder + auto-grow) and the harness composer entity
     (`session.rs:333`) uses it as-is. So the default
     (`state.rs:1024-1031`: "Default is `false` (both `Enter` and
     `Shift+Enter` insert a newline)") is live, and Enter's newline
     wins over `SendTurn`, which is exactly the reported symptom.
   - gpui dispatches every matching binding in turn
     (`gpui-pre-0.3.3/src/window.rs:5735-5745`, `for binding in
     match_result.bindings`), so this is a genuine competition between
     the two `enter` bindings not a missing binding. Which of the two
     actions runs first (deepest-node-first ordering) was not fully
     determined from static reading; either order produces a broken
     result today (newline-only if the input wins outright, send-plus-
     stray-newline if `SendTurn` runs first). Not determined: the exact
     precedence rule in `dispatch_key`.

3. Library inventory. The fix primitive already exists in gpui-base
   (`submit_on_enter`, `state.rs:1027-1031`); `aui::composer` exposes no
   wrapper for it (`composer.rs:148-155`). Nothing is missing from `aui`
   except a one-line opt-in. Fix belongs in **library** (expose/set the
   flag in `composer_state_rows` or a builder method) with zero harness
   changes needed — or, if preferred, in harness by calling the
   gpui-base builder directly. Either way `docs/08-keymap.md:19-23`
   already documents the intended behaviour, so no doc change.

4. Proposed fix. In `aui::composer::composer_state_rows`
   (`crates/aui/src/composer/composer.rs:153`) chain
   `.submit_on_enter(true)` (gpui-base `TextareaState` builder,
   `state.rs:1027-1031`); size S. Then plain Enter emits `PressEnter`
   without a newline and calls `cx.propagate()` (`state.rs:1624-1630`),
   letting `SendTurn` (`app.rs:147`) do the send, while Shift+Enter
   still inserts a newline (`state.rs:1611`: `!submit_on_enter ||
   action.shift`). Risk: low — search/rename fields also use
   `composer_state_rows` (`app.rs:272-273`) and are single-line
   (`1,1`), where the flag is a no-op (single-line never inserts).
   Test: replay `--replay fixtures/msp/transcript-echo.jsonl`,
   type `draft:hello`, press Enter, assert one turn starts and the
   draft clears with no residual newline; press Shift+Enter and assert
   a newline and no turn. Proof screenshots:
   `docs/diagnosis/shots/composer-enter-*.png` (draft cleared/sending
   vs multiline draft).

5. Open questions. None for the fix; the dispatch-precedence detail
   above is undecided but does not change the fix.

## D2. Image uploads show no thumbnails (item 2)

1. Symptom (owner): "Image uploads — should show thumbnails."

2. Root cause. Attached images are rendered only as text chips. The
   harness maps every `images::Image` to a `ComposerChip` with
   `kind: Image`, `label: name` (`session.rs:2249-2259`), and the
   library draws chips as a 1-line row: glyph + label + remove-x
   (`aui/src/composer/composer.rs:329-342`; `Image` → `IconName::Image`
   at `:334`). No pixel preview exists anywhere in that path. The image
   bytes are held (base64 in `images::Image`, `images.rs:23-38`) but
   never decoded into a renderable bitmap for the chip.

3. Library inventory. `aui` has `attachment_row`
   (`aui/src/composer/attachments.rs:107-114`): 44 px rows with
   icon-per-kind (`:171-174`), meta line and upload/failed states
   (`:203-204`, `:244-290`), and the protocol already distinguishes
   `AttachmentKind::{Image, File, Text}`
   (`aui-protocol/src/turn.rs:101-111`). What is missing: no thumbnail
   tile — the `Image` kind draws `IconName::Image`, never image bytes.
   The gallery reference for attachments is card `composer/attachments`
   (`aui-gallery/src/registry.rs:324`); the harness does not use
   `attachment_row` at all. Fix belongs in **both**: library gains a
   thumbnail-capable chip/row (decode via the existing `image` crate
   dependency in harness, or pass a small RGBA/bitmap), harness passes
   bytes or a thumb handle through `ComposerChip`.

4. Proposed fix. In `aui::composer` add an optional thumbnail payload
   to `ComposerChip` (or a new `attachment_row` thumbnail variant), and
   in harness `session.rs:2249-2259` supply a downscaled preview
   (decode once with `image::load_from_memory`, resize to ~48 px —
   `images.rs:65-66` already decodes dimensions at attach time, so cache
   the thumb there); size M. Risks: per-image memory if full base64
   kept for thumbs (keep thumbs separate, drop on remove/send);
   `gpui::img` source must outlive the frame. Test: `--steps
   image:<png>` replay, assert chip shows pixels not glyph; remove-x
   still works; send still carries full-res part (`images.rs:92-101`).
   Proof: `docs/diagnosis/shots/composer-thumb-*.png`.

5. Open questions. None on cause. Thumbnail size/shape (tile vs
   Portrait strip) is a design choice for the owner, not deducible
   from code.

## D3. No non-image file upload (item 3)

1. Symptom (owner): "there should also be a way to upload files like
   PDF, md, excel, word etc."

2. Root cause. Three independent walls, all confirmed:
   - Picker accepts anything but the decoder refuses it:
     `prompt_for_image` (`session.rs:1519-1530`) uses
     `PathPromptOptions{files:true...}`, then `attach_paths`
     (`session.rs:1498-1507`) routes every path through
     `images::from_path`, which rejects non-images
     (`images.rs:41-49`: only Png/Jpeg/Gif/WebP; `:63-64` guess-from-
     bytes) with the reason shown in the banner (`session.rs:1503`).
     A dropped PDF therefore becomes a banner, never an attachment.
   - The composer model holds images only: `session.rs:288-289`
     (`images: Vec<images::Image>`) and `parts()` (`session.rs:825-835`)
     emits text + image parts and nothing else.
   - The wire has nowhere to put a file: `TurnInputPart` is
     `{text,image}` only, closed enum (`muse-client/src/schema.rs:2729-
     2750`, `:2780-1786`: `Text="text", Image="image"`, unknown → 
     `invalidParams`). Mentions are likewise plain text in the text
     part (`files.rs:1-6`, `docs/03-composer.md:191-193`), so "@ the
     PDF" only sends a path string the model cannot read.

3. Library inventory. `aui` already models the missing shapes:
   `ComposerChipKind::File` exists (`composer.rs:68-74`) but the
   harness never emits it (`session.rs:2252-2256` hard-codes
   `ComposerChipKind::Image`); `attachment_row` supports
   `AttachmentKind::{File,Text}` + `UploadState`
   (`attachments.rs:121-139`, `aui-protocol/src/turn.rs:101-130`).
   What is missing is wire + policy: no `file` part type on MSP, no
   text-extraction (pdf/md/xlsx/docx → text) anywhere in either repo.
   Fix belongs in **harness** for extraction/packaging (library only
   needs the `File` chip the harness starts emitting); a true binary
   file part needs a server/wire change outside both repos.

4. Proposed fix (staged). (a, S) Emit `ComposerChipKind::File` chips
   for accepted non-images so they are visible. (b, M) Extract text
   client-side for text-ish types (md/txt/csv, pdf text layer, xlsx
   sheet text) and append as a fenced text part or `path + content`
   in `parts()` (`session.rs:825-835`), with size cap + truncation
   note; refuse or down-sample genuinely binary blobs (docx without a
   parser, xls) with a banner reason. Risks: context-window blowup
   (cap bytes, default ~32–64 KB, announce truncation); binary
   misdetection (reuse `images::from_bytes` + content sniffing, never
   the extension). Test: replay + attach one .md, one .pdf, one .xlsx,
   one .docx; assert text-carrying types arrive in `turn/start` input
   and binary ones banner. Proof:
   `docs/diagnosis/shots/composer-file-*.png`.
   Not proposed (no located cause to fix): inventing a `file` wire
   part — the closed enum (`schema.rs:2780-1786`) forbids it.

5. Open questions. Which file types must actually reach the model vs
   merely ride along as names? And the per-file byte cap — owner call.

## D4. Plus-menu transition vs other dropdowns + contents (item 4)

1. Symptom (owner): "The transition that happens on clicking the plus
   icon is too slow... different from the transition in other dropdowns
   like model picker. Remove that transition or make it snappier. All
   the dropdowns should be uniform" — with items "Attach file or
   photo", "@ Mention file", "/ Slash commands".

2. Root cause (two halves).
   - Transition sameness, not difference: the `+` menu and the model
     picker run the *same* morph. `plus_menu`
     (`aui/src/composer/menu.rs:90-154`): `spring_phase(...,
     SpringKind::Gentle)` at `:103`, scale from `.85` (`:31`,
     `:108`), opacity = phase (`:121`). `PickerMenu` (model/effort/
     mode, `aui/src/composer/pickers.rs:197-209`): identical
     `spring_phase(..., SpringKind::Gentle)` at `:209`, same
     `MORPH_FROM = 0.85`. `Gentle` is stiffness 200
     (`spring.rs:147`, tokens `springs::GENTLE`), the slowest-feeling
     of the four springs, so both menus feel slow — but they cannot
     differ from each other by construction. What *does* differ is the
     `/` and `@` caret menus: they use a tweened presence
     (`EnterExit::DEFAULT`, `menus.rs:152-153,192` / `:315,353`) with
     a 6 px rise + .98 scale (`menus.rs:31-34`) over
     enter 220 ms / exit 160 ms (`aui-motion/src/tween.rs:28-30`,
     asserted at `:94-95`), i.e. a fast fade-rise, not a spring
     morph. So the owner's "different from model picker" perception is
     not in the curves (verified identical); the real non-uniformity
     is spring-morph (plus + chip pickers) vs tween fade-rise (caret
     menus). Also note the picker shell carries `.occlude()`
     (`pickers.rs:244`) while `plus_menu` (`menu.rs:109-122`) and the
     caret `popover_frame` (`menus.rs:374-392`) do not.
   - Contents: the harness wires exactly one row —
     `PlusMenuItem::new("attach-image", Image, "Attach image")`
     (`session.rs:2267-2270`), handled at `session.rs:2235-2241` +
     `:2222` (`ComposerIntent::Attach` → `prompt_for_image`). The
     gallery reference (the "meant to look and be wired" spec) shows
     four rows: "Attach file" (⌘U), "Add knowledge source", "Mention a
     document" (@), "Commands" (/)
     (`aui-gallery/src/assistant/view.rs:484-490`). The owner's three
     items map 1:1 onto gallery rows 1, 3, 4.

3. Library inventory. `PlusMenu`/`PlusMenuItem`
   (`aui/src/composer/menu.rs:33-74`) already support N rows, icons,
   keycaps — nothing missing; the one-row list is pure harness data
   (`session.rs:2269`). Transition helpers both exist (`spring_phase`
   + `presence`/`EnterExit` in `aui-motion`). Fix belongs in
   **harness** for contents; **library** (or harness-local wrapper)
   for unifying the enter motion.

4. Proposed fix. Contents (S, harness only): in
   `session.rs:2267-2270` pass three items —
   `attach` (Paperclip/Image, "Attach file or photo"),
   `mention` (At, "@ Mention file"), `commands` (Slash, "/ Slash
   commands") — and extend the `plus` listener (`session.rs:2235-2241`)
   so `mention` focuses the composer and types `@` (reuse the
   `mention:` step path, `session.rs:1574-1579` →
   `on_draft_changed`), `commands` types `/` (same path), `attach`
   keeps `prompt_for_image`. Transition (S): make all composer menus
   use the caret-menu tween — replace the `spring_phase(Gentle)` in
   `menu.rs:103` and `pickers.rs:209` with
   `presence(..., EnterExit::DEFAULT)` + `fade_rise_scale(6px, .98)`
   exactly as `menus.rs:192,353` do (gpui API:
   `aui_motion::{presence, EnterExit, PresenceStyle}`, same source
   files), or add `.at_rest()`-style instant enter. Uniformity means
   one curve everywhere: pick the 220 ms fade-rise. Risks: spring
   morph also drives width (`menu.rs:114`,
   `pickers.rs:243-244` scale `* scale_now`); switching to presence
   keeps scale via `PresenceStyle` (`menus.rs:379-384` does the same),
   so layout risk is low but snapshot-test every menu. Test:
   `--steps plus` / `model` / `command:/` replays side by side;
   assert identical enter feel and three working plus rows. Proof:
   `docs/diagnosis/shots/composer-plus-*.png`,
   `composer-picker-*.png`.
   Alternative if the owner prefers zero motion: `present(false)`-exit
   + `at_rest()` enter already exist (`menu.rs:77-81`,
   `menus.rs:166-172`) — wire them behind a flag.

5. Open questions. Snappy-tween vs no-motion — owner call (item text
   allows either). Whether "Add knowledge source" (gallery row 2) is
   also wanted — owner listed only three.

## D5. Slash/@ menu full width (item 5)

1. Symptom (owner): "The slash commands menu and @ files menu takes
   the entire width. It should look like a dropdown — should have some
   max width."

2. Root cause. Width is inherited, twice, with no cap at either
   level. Library: `command_menu`/`mention_picker` are documented "at
   the width of its container" (`menus.rs:142-146`, `:304-308`), and
   `popover_frame` (`menus.rs:374-392`) sets no width/max-width of its
   own (only `w(relative(scale))` for the enter-scale at `:384`).
   Harness: the anchor row is `w_full` (`session.rs:1750`) and the
   popover wrapper pins `left(0).right(0)` (`session.rs:2173-2175`),
   so container width = full composer width and the menu fills it.
   Compare: the chip pickers deliberately chose a floor+ceiling
   (`MENU_W_MIN 360 / MENU_W_MAX 520`, `pickers.rs:59-79`, with the
   rationale at `:60-79` that absolute layout needs an explicit
   width), while the caret menus got neither.

3. Library inventory. `aui::overlay::popover_layer`
   (`aui/src/overlay/mod.rs:44-46`) only lifts paint order
   (deferred, priority 1); it offers no max_w/anchoring knobs. No
   existing `aui` popover offers max-width — the pickers' min/max
   (`pickers.rs:243-244`) is the closest precedent. Fix belongs in
   **library** (a `max_w` on `popover_frame` or the two menus, default
   ~480–520 px left-anchored like the picker ceiling) with the harness
   anchor (`session.rs:2168-2180`) optionally overriding per case.

4. Proposed fix. In `menus.rs:popover_frame` (`:374-392`) add
   `.max_w(px(480.0))` (keep left edge: the harness wrapper already
   anchors left, `session.rs:2174`) — or thread a builder param
   (`max_w`) through `command_menu`/`mention_picker` defaulting to
   480; size S. Claude-Code parity argues for ~440–520 px; reuse the
   picker ceiling 520 (`pickers.rs:59`) to keep one number. Risks:
   long skill/path rows must wrap not clip (rows already wrap command
   column, `menus.rs:47-49`; mention detail is mono 11, `:53-56` —
   verify wrap on a 520 px menu with a 100-char path). Test: replay
   `command:review` and `mention:mainrs`, assert menu ≤ cap and
   left-aligned over the composer on narrow (800 px) and wide
   (1600 px) windows. Proof:
   `docs/diagnosis/shots/composer-menuwidth-*.png`.

5. Open questions. Exact cap value (480 vs 520) — owner call; code
   cannot decide taste.

## D6. Scroll bleed: menu open + wheel scrolls menu and transcript (item 6)

1. Symptom (owner): "when slash commands menu or @files menu is open,
   then scrolling causes scroll of both the dropdown/popup and the
   transcript pane behind it."

2. Root cause. Neither scroll container stops the wheel, and gpui
   delivers one wheel event to *every* scrollable hitbox under the
   cursor. Concretely:
   - Menu scroller: the caret popover wrapper
     (`session.rs:2168-2180`) is `.overflow_y_scroll()` (`:2177`)
     with **no** `on_scroll_wheel` / `cx.stop_propagation()` and **no**
     `.occlude()`. A repo-wide grep confirms zero
     `on_scroll_wheel`/`stop_propagation` in harness `src/` and in
     `aui/src/composer/*` + `aui/src/overlay/*`.
   - Transcript scroller: `div().id("transcript").track_scroll(...).
     overflow_y_scroll()` (`session.rs:1860-1866`). The menu is
     absolutely positioned (`:2172-2175`) *over* the transcript's
     bottom area, so a wheel over the menu hit-tests both boxes.
   - gpui semantics make the double-scroll inevitable: each
     `overflow_y_scroll` div registers its own `ScrollWheelEvent`
     bubble handler gated only on `hitbox.should_handle_scroll`
     (`gpui-pre-0.3.3/src/elements/div.rs:3272-3316`), which is true
     for *all* hitboxes containing the point
     (`window.rs:808-812`); that handler never calls
     `stop_propagation` (`div.rs:3272-3316` end-to-end — clamp,
     accumulate, `cx.notify`, return). `List` behaves the same
     (`list.rs:1601-1618`). Dispatch runs capture front-to-back then
     bubble back-to-front over *all* listeners
     (`window.rs:5548-5590`), so both scrollers consume the same
     event. (The `List` unit test at `list.rs:1892` shows the blessed
     pattern — a child `on_scroll_wheel` that calls
     `cx.stop_propagation()` — which this codebase never uses.)
   - `popover_layer` does not save it: `deferred` "moves the child to
     the end of the frame while leaving its *layout* where it was"
     (`overlay/mod.rs:27-43`), i.e. paint order only, no hit-test
     isolation.

3. Library inventory. No `aui` composer/overlay element stops wheel
   propagation today (grep-verified). Precedent exists in two places:
   chip-picker shells use `.occlude()` (`pickers.rs:244`), whose
   `BlockMouse` semantics make every hitbox behind report
   `should_handle_scroll() == false`
   (`window.rs:895-935`, `HitboxBehavior::BlockMouse` docs) — i.e. the
   transcript would not scroll at all under an occluding menu. The
   caret `popover_frame` (`menus.rs:374-392`) lacks it. Fix belongs
   in **library** (add `.occlude()` — or a wheel-stop — to
   `popover_frame`, covering both caret menus for every consumer),
   optionally mirrored on the harness wrapper.

4. Proposed fix. Minimal (S): add `.occlude()` to `popover_frame` in
   `aui/src/composer/menus.rs:374-392` (one line, same call the
   pickers use at `pickers.rs:244`; gpui API
   `InteractiveElement::occlude` → `HitboxBehavior::BlockMouse`,
   `gpui-pre-0.3.3/src/window.rs:895-935`). Belt-and-braces
   alternative: `.on_scroll_wheel(|_,_,cx| cx.stop_propagation())`
   (gpui API `InteractiveElement::on_scroll_wheel`,
   `div.rs:1017-1024`) on the harness wrapper
   (`session.rs:2168-2180`); prefer `occlude()` because it also kills
   hover/click-through behind the open menu, matching picker
   behaviour. Risks: `occlude()` suppresses transcript hover styles
   under the menu while open — desired for a modal-ish menu, and the
   menu closes on select/Escape anyway; verify clicks on transcript
   while menu open still dismiss-or-select sanely (currently
   `close_menu` paths at `session.rs:1329-1332`). Test: replay,
   open `command:` menu, wheel over menu — assert menu scrolls,
   `scroll.offset()` unchanged; wheel over bare transcript still
   scrolls; same for `mention:`. Proof:
   `docs/diagnosis/shots/composer-scrollbleed-*.png` (menu scrolled,
   transcript pinned).

5. Open questions. None on cause or fix location. Whether an open
   menu should *dismiss* on outside wheel (Claude Code behaviour) vs
   merely not bleed — owner call; the proposed fix preserves open.

## Dependencies between items

- D1 is independent (key bindings only).
- D4-contents and D3 share the attach entry point (`prompt_for_image`,
  `session.rs:1519-1530`): do D4's "Attach file or photo" row together
  with D3's file acceptance so the row does not keep refusing PDFs.
- D4-transition and D5 are both "caret/chip menu look": land them
  together in `aui` (`menus.rs`, `pickers.rs`, `menu.rs`) to keep one
  visual-review pass; D5's cap interacts with D4's width-morph
  (`MENU_W * scale_now`), so decide final widths once.
- D6 (`occlude` on `popover_frame`) touches the same `menus.rs:374-392`
  function as D5's cap — one edit, one review.
- D2 thumbnails and D3 file chips both change the chip row
  (`composer.rs:329-342` / `session.rs:2249-2259`): design the chip
  once (glyph vs thumbnail vs file tile) and implement both.

## What I ran

- `ls /Users/latekaapi/Projects/harness/docs/diagnosis/inputs/`
- `ls /Users/latekaapi/Projects/harness/crates/harness/src/`
- `grep -rn "bind_keys\|HarnessComposer\|on_key\|Enter\|Shift" /Users/latekaapi/Projects/harness/crates/harness/src/main.rs`
- `grep -rn "plus\|Plus\|popover\|command_menu\|mention_picker\|max_w\|max-width\|max_width" /Users/latekaapi/Projects/harness/crates/harness/src/*.rs`
- `grep -n "bind_keys" -A 120 /Users/latekaapi/Projects/harness/crates/harness/src/app.rs`
- `grep -n "COMPOSER_CONTEXT\|menu\b\|KeyContext\|..." /Users/latekaapi/Projects/harness/crates/harness/src/app.rs` + same for `session.rs`
- `grep -n "plus_menu\|PlusMenuItem\|..." /Users/latekaapi/Projects/agentic-ui/crates/aui/src/composer/menus.rs` (+ menu.rs, overlay/mod.rs)
- `grep -n "POPOVER_GAP\|POPOVER_MAX_H\|..." /Users/latekaapi/Projects/harness/crates/harness/src/session.rs`
- `grep -n "TurnInputPart\b" -A 30 /Users/latekaapi/Projects/harness/crates/muse-client/src/schema.rs`
- `grep -n "AttachmentKind\|UploadState" /Users/latekaapi/Projects/agentic-ui/crates/aui-protocol/src/*.rs`
- `grep -n "on_scroll_wheel\|stop_propagation\|ScrollWheel" /Users/latekaapi/Projects/harness/crates/harness/src/*.rs /Users/latekaapi/Projects/agentic-ui/crates/aui/src/composer/*.rs /Users/latekaapi/Projects/agentic-ui/crates/aui/src/overlay/*.rs`
- `find ~/.cargo/registry/src -maxdepth 2 -name 'gpui-pre-*'`; reads under `gpui-pre-0.3.3/src/window.rs` (`:760-940`, `:5250-5330`, `:5548-5610`, `:5680-5840`, `:6050-6130`, `:6780-6860`), `src/elements/div.rs` (`:2755-2800`, `:3260-3360`, `:3675-3700`), `src/elements/list.rs` (`:1595-1640`, `:1880-1920`)
- `grep -rn "ScrollWheelEvent" ~/.cargo/registry/src/.../gpui-pre-0.3.3/src/`
- gpui-kit → gpui-base → `gpui-base-0.6.0/src/input/base/state.rs` (`:100-170` bindings, `:1020-1030` `submit_on_enter`, `:1597-1650` `enter()`, `:3790-3800` test)
- `grep -rn "composer\|plus_menu\|command_menu" /Users/latekaapi/Projects/agentic-ui/crates/aui-gallery/src/...`
- `ls /Users/latekaapi/Projects/harness/fixtures/msp/*.jsonl`
- Read: `docs/diagnosis/inputs/owner-issue-list.md`, `docs/08-keymap.md`, `docs/03-composer.md`, `CLAUDE.md` (head), `agentic-ui/docs/04-design-rules.md` (head), `docs/09-handoff-improvements.md` (D-grep)
- The app was NOT run and no screenshots were taken (static diagnosis only).

