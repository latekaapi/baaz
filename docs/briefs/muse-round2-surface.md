# Brief — owner round 2, surface package (harness)

Repository `/Users/latekaapi/Projects/harness`, branch `owner-round-2-2026-09-13` (already
checked out; the wire package is committed on it). Work ONLY there. Do NOT commit. Do not
touch `/Users/latekaapi/Projects/agentic-ui` (its `main` already carries the library
package: `PJ_CHILD_INDENT`, `ProjectGroup::folded`, `GroupAction::ToggleMore`, pinned
glyph and `PinOff`, the inline footer plan, `folder_drop_card` — read
`crates/aui/src/nav/views.rs`, `session_row.rs`, `parts.rs`, `folder_drop.rs`) or
`~/Projects/cockpit`. Prefix every shell command with
`export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`.
Spend rule (non-negotiable): never `turn/start`, `--send`, `send:`/`steer:` steps, the
ignored live tests, `muse logout`, `account/logout`. Read `CLAUDE.md`, `docs/05-handoff.md`,
`docs/12-projects.md` §5 and §8, `docs/02-app.md` §5–§6.

Eight items from the owner's screenshots.

## P1 — "Choose folder…" does nothing

File: `crates/harness/src/dialogs.rs` (`choose_project_folder`, the Projects palette's
`run_palette_row`/`on_select` path).

`choose_project_folder` builds `cx.prompt_for_paths` and pushes the task onto `self.tasks`,
which looks right, so the fault is upstream or in the panel: add a `harness:` log line at
the row's select, one at `choose_project_folder`'s entry, one when the future resolves
(with its outcome), run the app (`cargo run -p harness`, no turn), click the row, read the
log, and fix what the log shows. Candidates, in order: the palette dismisses and returns
before the row's action runs; the row id for "Choose folder…" is not matched by
`run_palette_row`; the panel needs the app activated (`cx.activate(true)`) before
`prompt_for_paths`. Keep the log lines (they are cheap and this will regress). Also wire
the hero's "Choose folder…" button to the same function.

## P2 — The folder card

Files: `crates/harness/src/dialogs.rs` (Projects palette), `crates/harness/src/app.rs`
(`render_no_session` hero).

The Add section's first row becomes the library's `folder_drop_card` ("Drop a folder here /
or click to choose one", `⌘⇧O` keycap): click → P1's function; drop → adopt every dropped
directory (`adopt_root` each, the first becomes current). The palette's keyboard selection
skips the card (it is not a row) — `↩` on nothing selected in the Add section opens the
panel. The hero keeps its two buttons but the whole hero column also accepts a drop.

## P3 — Nesting and five recent per project

Files: `crates/harness/src/sidebar.rs` (`grouping_by_project`), `crates/harness/src/layout.rs`,
`crates/harness/src/app/list.rs`, `crates/harness/src/sidebar_view.rs`.

Per project group: pinned rows, then the five most recent others; the rest are held back and
the group is `.folded(hidden, expanded)`; `GroupAction::ToggleMore` flips the group's id in a
new `layout.expanded_groups` (persisted like `closed_groups`) and regroups. The open session's
row is always among the visible ones even when it is older than the fifth. The library
indents the rows. Tests in `sidebar.rs`: nine sessions → five visible plus "4 hidden";
expanded → nine; the open session survives the cut; pinned rows never count toward the five.

## P4 — Any open menu closes on a click outside

Files: `crates/harness/src/project_menu.rs`, `crates/harness/src/dialogs.rs`.

The view menu and the account menu pass `.on_dismiss` to `popover_layer`; the project menu
and its colour submenu do not. Add it (dismiss closes both). Check the model/effort/mode
pickers and the overflow menu the same way and fix any that lack it.

## P5 — Pinned rows show it

File: `crates/harness/src/sidebar_view.rs`, `crates/harness/src/sidebar.rs` (`summary`).

`SessionEntry::summary` already sets `.pinned()`; confirm the glyph and the `PinOff` tray
button appear in the app, and that the rail tile of a pinned session is unchanged.

## P6 — The plan label sits beside the name

File: `crates/harness/src/sidebar_view.rs` (`render_footer`).

Use the library's inline `.plan(..)`; the footer is two rows (name + label, then detail/
meter), never three.

## P7 — Room under the last block

Files: `crates/harness/src/session/render.rs` (the transcript list's tail),
`crates/harness/src/transcript.rs` if the spacer belongs there.

The last block sits against the status row / banner. Add a trailing spacer of `scale::SP_7`
below the final block (a fixed-height row in the block-granular list, hinted like any row so
the virtual list's measurement rules hold — read `transcript::turn_rows`). The bench
(`--bench fixtures/msp/transcript-real.jsonl --bench-scroll wheel`) must not regress:
compare `frame.series_us` before and after and report both.

## P8 — Captures and docs

Retake `docs/images/projects-sidebar-{dark,light}.png` and `projects-palette-dark.png` with
`scripts/captures.sh` (the fixture now folds `acme-web` past five: extend
`fixtures/sidebar/projects.json` to seven `acme-web` sessions so the "Show 2 more" row
appears), add `docs/images/round2-pinned-dark.png` (steps `…;pin`), byte-identical run to
run. CHANGELOG entry "2026-09-13 — Owner round 2, surface" with P1–P8 one line each;
`docs/02-app.md` §5 for folding and the drop card; `docs/12-projects.md` §8 appended.

## Gates

`cargo build --workspace`; `cargo test --workspace`;
`cargo clippy --workspace --all-targets -- -D warnings` (last, after every edit);
`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`; one `gpui-pre` and one
`gpui-kit` in `cargo tree -d`; `UPDATE_SNAPSHOTS=1 cargo test -p muse-adapter` with the diff
read. Report per item: done / skipped-with-reason, test names, the P1 log lines verbatim,
screenshot paths, bench numbers, gate output verbatim (last lines); never claim a gate you
did not run. When finished write the single word `done` to `/tmp/muse-round2-surface.done`.
