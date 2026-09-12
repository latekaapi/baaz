# Brief — H1: transcript design, native lights, header collapse, close/reopen, D25 cleanup

Repository `/Users/latekaapi/Projects/harness`, branch `main`, clean tree. **Do NOT commit.**
Do not touch `~/Projects/cockpit`. The library at `/Users/latekaapi/Projects/agentic-ui` is on
branch `transcript-2026-09-12` and already carries `SidebarHeader::native_lights`,
`aui::shell::traffic_light_position` and `AppShell::header_follows_sidebar` (read their
rustdoc; do not change the library). Prefix every shell command with
`export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`.
Spend rule: never `turn/start`, `--send`, `send:`/`steer:`, live tests, `muse logout`,
`account/logout`. Everything here runs on `--replay` and `--no-connect`. Read `CLAUDE.md`,
`docs/05-handoff.md`, `docs/02-app.md` (§1 flags, "Measuring", the transcript section) first.

## Diagnosis (done; do not redo)

Side-by-side of `--replay` captures against `agentic-ui/design/reference/{screens,cards}` and
the gallery's own `transcript/*` cards showed the library cards match the design; what is off
is the harness's composition:

- **Blocks inside an assistant turn touch.** `session/render.rs::transcript_list` builds one
  `v_flex` row per turn with no gap; `transcript::turn` returns the blocks as bare children. The
  design (`design/src/cards/shell/10-app-shell.html` `.grp2{gap:8px}`, `harness-Main.png`)
  puts 8 px between stacked cards and between prose and a card.
- **Turn rhythm.** The row's `pb(SP_5)` is the only spacing between turns; the design's
  transcript column is `.tr{gap:16px}`.
- **No measure.** The column stretches to the pane (1112 px at 1440 wide); every reference
  screen bounds the transcript to ~760–800 px and centres it, composer included.
- **Native traffic lights overlap the header** (the window keeps macOS's lights,
  `app.rs` passes `.traffic_lights(false)` to both the shell and the sidebar header, and
  `main.rs` never sets `traffic_light_position`), and **collapsing the sidebar collapses the
  header cell** (`sidebar_header(..).collapsed(!self.sidebar_open)`).
- **Closing the window strands the app.** `main.rs` `on_window_should_close` returns `true`,
  the window is removed, and nothing handles `on_reopen`, so the Dock icon and ⌘-Tab find no
  window to show.

Colour and type were checked and are not at fault (tokens are generated from the same
`tokens.css`; the 1.1 text scale is decision 2026-09-05).

## What to build, in this order

1. **Gaps.** In `transcript_list`, the per-turn row gets `.gap(px(scale::SP_2))` (8) between
   its blocks, and the between-turn step becomes 16 px (the `scale` step that equals 16; keep
   `TRANSCRIPT_PAD_TOP` for the first row and the tail padding as they are). The silent-footer
   row and the user turn's in-flow action row keep their own spacing.
2. **Measure.** One `const TRANSCRIPT_MEASURE: f32` next to `TRANSCRIPT_PAD_X` in
   `session.rs`, 880 px, with a comment naming the design's 760 px card at the 1.1 text scale.
   Every centre-pane row that today uses `TRANSCRIPT_PAD_X` — the list rows, the loading row,
   the status row, banners, caret menus, the queue strip — and the composer's content are
   bounded to that width and centred (`max_w` + `mx_auto` on an inner `w_full` wrapper; the
   composer's docked border keeps spanning the pane, only its inner content is bounded). The
   empty state stays as it is. Check every replay capture at both themes: nothing may overflow,
   clip or jump; the `--steps sidebar-width:<px>` captures at min and max widths still fit.
3. **Native lights and header.** `main.rs` (both `WindowOptions`): `traffic_light_position:
   Some(aui::shell::traffic_light_position(cx))`. `app.rs`: `sidebar_header(..)
   .native_lights(true)` instead of `.traffic_lights(false)`, drop `.collapsed(..)`, and
   `app_shell(..).header_follows_sidebar(false)`. The centre header no longer needs the
   expand-sidebar button when collapsed (the toggle stays in the sidebar header); remove that
   branch if it only existed for the collapsed header. The `--steps sidebar` capture must show
   the full header row over a rail-width body. Screenshots cannot show the OS lights; say so in
   the report and leave the on-screen check to the owner.
4. **Close and reopen.** In `main.rs`: the window's `on_window_should_close` keeps
   `tier::cleanup_probes()`, then hides the app (`cx.hide()`) and returns `false` — the window
   and the `muse serve` child survive, so ⌘-Tab and the Dock bring the same session back.
   Register `cx.on_reopen` once: if `cx.windows()` is empty (the window was removed some other
   way), rebuild the shell window through one shared `open_shell_window(..)` function factored
   out of `main`; otherwise `cx.activate(true)`. `--screenshot` runs and `--bench` are untouched
   (they quit themselves). ⌘Q still quits through `on_app_quit`. Document in `docs/08-keymap.md`
   (the ⌘W row) and `docs/02-app.md`.
5. **D25 is verified live.** Delete `Harness::reconnect_after_login` and its
   `#[allow(dead_code)]` in `login.rs`, the sentence in that file's module doc that keeps it,
   and the "Still open … `reconnect_after_login`" clause in `docs/CHANGELOG.md`; note in
   `docs/diagnosis/login.md` (D25) that the Meta-account login was verified live 2026-09-12
   with no reconnect.

## Regression proof

`scripts/captures.sh <dir>` takes the deterministic set (53 PNGs). The reference set from
before this brief is at `/private/tmp/claude-501/-Users-latekaapi-Projects-harness/f7a85ce4-a78a-46db-b3ec-e131d3878ca7/scratchpad/set-before`.
Take the set after your change, `cmp` file by file, and list in the report which captures
changed. Items 1–3 change every session capture (intended: gaps, measure, header); the five
`login-*` captures must be byte-identical. Adapter snapshots (`UPDATE_SNAPSHOTS=1 cargo test
-p muse-adapter`) must show no diff. `--bench` on `synthetic-stress-300.jsonl` before and
after (debug): `bench-element`/`bench-frame` must not regress beyond noise; put both lines in
the report.

Refresh the checked-in images the changes touch: the whole `docs/status/*.png` set is this
script's output (copy the new set over it), and every `docs/images/improve-transcript-*`,
`improve-shell-open-*`, `improve-shell-collapsed-*`, `improve-integrated-*` and
`phase2-session-*` image whose command is recorded in `docs/` (grep `--screenshot docs/images`
in `docs/*.md` and `docs/briefs/*.md`; rerun those exactly, under `HARNESS_DETERMINISTIC=1`).
Do not invent commands for images with none recorded; list them as not refreshed.

## Gates (run all; report verbatim; never claim a gate you did not run)

`cargo build --workspace`; `cargo test --workspace`; `cargo clippy --workspace --all-targets
-- -D warnings`; `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`; `cargo tree -d`
shows one `gpui-pre` and one `gpui-kit`. Report per item, the capture diff list, the bench
lines, the refreshed image list, gate output.
