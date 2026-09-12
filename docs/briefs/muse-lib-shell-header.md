# Brief — library: native traffic lights and a header that stays while the sidebar collapses

Repository `/Users/latekaapi/Projects/agentic-ui`, branch `transcript-2026-09-12` (checked
out, clean, stacked on `audit-2026-09-12`). **Commit on this branch** when the gates pass,
message ending `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`. Do not touch
`/Users/latekaapi/Projects/harness` or `~/Projects/cockpit`. Prefix every shell command with
`export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`. Read
`docs/00-agent-brief.md` and `docs/04-design-rules.md` first (no literal colours/sizes/durations
outside named consts, stateless `RenderOnce`, both themes, a gallery entry for anything new).

## The two faults, with evidence

1. **The native traffic lights overlap the sidebar header.** `crates/aui/src/shell/header.rs`
   `SidebarHeader::render` reserves room for the lights only when it *paints* them
   (`self.traffic_lights`). A host window that keeps macOS's real lights
   (`WindowOptions.titlebar.traffic_light_position`) and passes `.traffic_lights(false)` gets
   the back/forward buttons drawn under the lights (harness, measured on screen: lights at
   x≈13–70 pt, the back button at x≈20 pt). The lights also sit ~3 pt above the header's
   vertical centre because nothing tells the window where a 44 px header wants them.
2. **Collapsing the sidebar collapses the header cell too.** `AppShell::render`
   (`crates/aui/src/shell/app_shell.rs`, `header_row`) gives the sidebar header cell the same
   width as the pane below it, so a collapsed sidebar leaves a 48 px header cell with nothing in
   it (or, with native lights, the lights over the rail). The product wants the header row
   untouched by the collapse: back/forward, search and the toggle stay where they are, and only
   the pane below (from "New session" down) collapses to the rail.

## What to build

1. `SidebarHeader::native_lights(bool)` (default `false`): when on, the header reserves the
   footprint of macOS's own lights — a leading spacer `NATIVE_LIGHTS_WIDTH` wide — and paints
   nothing there. Define the geometry once in `header.rs` as named consts with a doc comment
   citing the macOS metrics: light diameter 12 pt, centre-to-centre stride 20 pt, the first
   light's left edge at `NATIVE_LIGHTS_X` = 12 pt; `NATIVE_LIGHTS_WIDTH` = 12 + 2·20 + 12 + the
   existing `LIGHTS_MARGIN_RIGHT`, which lands on the existing `RAIL_WIDTH_WITH_LIGHTS` (72)
   — assert that in a unit test so the two never drift. `traffic_lights(true)` and
   `native_lights(true)` together is a caller error: `debug_assert!` against it.
2. `pub fn traffic_light_position(cx: &App) -> gpui::Point<gpui::Pixels>` in `shell/header.rs`,
   exported from `aui::shell`: `(NATIVE_LIGHTS_X, (metrics.header − 12) / 2)` so the lights'
   centre is the header's centre at every density. A host passes it to
   `TitlebarOptions.traffic_light_position` (document that in the fn's rustdoc).
3. `AppShell::header_follows_sidebar(bool)` (default `true`, today's behaviour): when
   `false`, the header row's sidebar cell keeps `sidebar_rest` width (the divider under it too)
   whether or not `sidebar_open`; only the panes row springs to the rail. The centre header
   then begins at the same x in both states. `SidebarHeader::collapsed` keeps working for hosts
   that still follow.
4. Gallery: a new entry `shell/app-shell-native` (registry, card, screenshot) showing the
   native-lights reservation (an empty 72 px lead, since the gallery window paints its own
   lights) with `header_follows_sidebar(false)` and the sidebar collapsed, so the screenshot
   proves the header stays. The existing `shell/app-shell` card is unchanged and its screenshot
   must stay byte-identical: take it before and after (`cargo run -p aui-gallery -- --theme dark
   --screenshot shell/app-shell /tmp/a.png`, both themes) and `cmp`.
5. `docs/06-api.md` regenerated (`python3 scripts/api-doc.py`), and a line in the shell section
   of `docs/02-component-spec.md` (or wherever the shell header is specified) naming the two new
   builders.

## Gates (run all; report the output verbatim; never claim a gate you did not run)

`cargo build --workspace`; `cargo build --workspace --features
aui-webview/wry,aui-terminal/pty,aui-terminal/tui`; `cargo test --workspace`;
`cargo clippy --workspace --all-targets -- -D warnings`; `RUSTDOCFLAGS="-D warnings" cargo doc
--workspace --no-deps`; `python3 scripts/api-doc.py`; the gallery `--idle-frames` check if the
gallery main documents one. Then commit. Report: the diff summary, the `cmp` result for the
unchanged card, the new card's screenshot path, gate output.
