# Brief — owner round 3 (Opus implementor): seven faults, scroll feel, a design audit, user journeys

You are the single implementor for this round. Two repositories, both path-linked:

- `/Users/latekaapi/Projects/harness` — the app (gpui). Branch `owner-round-3-2026-09-13` off
  `main` (create it).
- `/Users/latekaapi/Projects/agentic-ui` — the `aui` component library the app depends on by
  path. Branch `owner-round-3-2026-09-13` off `main` (create it).

**Do not touch either repository until the file `/tmp/round3-go` exists.** Until then do only
the research in R1 (reading, web, notes in your scratchpad). Another run is finishing on the
harness; Fable writes that file when the trees are yours. Do not touch `~/Projects/cockpit`.

Prefix every shell command with
`export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`.

Read first: `harness/CLAUDE.md`, `harness/docs/05-handoff.md`, `harness/docs/12-projects.md`
(§8 as built), `harness/docs/CHANGELOG.md` (the three "Owner round 2" entries and
"Projects"), `harness/docs/diagnosis/transcript-pass-2026-09-12.md` (the scroll instrument),
`harness/docs/07-architecture.md`, `agentic-ui/docs/00-agent-brief.md`,
`agentic-ui/docs/04-design-rules.md`. The working rules that follow are non-negotiable.

## Rules

- **Spend.** There is no free provider: `--provider echo` still reaches the real model on a
  signed-in machine, and anything that reaches `turn/start` is billed on the owner's
  subscription. Free: `--replay <capture>`, `--no-connect`, `session/start`, `session/list`,
  `session/read`, `session/resume`, `view/page`, `model/list`, `approval/*`, `userInput/*`,
  `--print-tier`, the `muse` TUI opened with no prompt, cargo. Never run the ignored live
  tests, `--send`, or `--steps` containing `send:`/`steer:`, `muse logout`, `account/logout`.
  **Exception, named by the owner for this round: at most six real turns**, only for the
  journeys in J2, each announced in your report with its session id and purpose. Count them
  from Muse's own logs, never from memory:
  `grep -c runtime.user_intent.accepted ~/.local/share/muse/sessions/*/*/*/*/session.jsonl | awk -F: '{s+=$2} END {print s}'`
  (record the baseline before you start).
- **Library first, then app, never both at once.** The app builds against the library's
  working tree. A library API change breaks the app until the app follows, so: finish and
  gate a library change, commit it on the library branch, then change the app. Never run a
  Muse subagent in one repo while you edit the other.
- **Muse.** You may delegate well-scoped, sequential subtasks to Muse Code:
  `muse exec --json --workspace "$PWD" --trust-workspace --approval-mode never
  --disable-sandbox --user-input-auto-resolve --max-model-steps N --prompt-file <brief>`,
  launched with `nohup` from a small script and watched through a sentinel file (a plain
  shell call dies at ten minutes). Its clippy claims need your own rerun. Prefer doing the
  work yourself when it is small.
- **Nothing is optimistic.** Cards, rows and chips move only on the server's notification.
- **Library rules.** No literal colours, sizes or durations outside `design/tokens/*.json`
  and `scale`; stateless `RenderOnce` components with intents out; both themes; a gallery
  entry for anything new; `python3 scripts/api-doc.py` regenerated.
- **Muse's storage is read-only** (`~/.config/muse`, `~/.local/share/muse`). The app's state
  is under `~/Library/Application Support/harness` (`HARNESS_STATE_DIR` for tests).
- **Gates before every commit.** Harness: `cargo build --workspace`, `cargo test
  --workspace`, `cargo clippy --workspace --all-targets -- -D warnings` (last, after every
  edit), `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`, exactly one
  `gpui-pre` and one `gpui-kit` in `cargo tree -d`, `UPDATE_SNAPSHOTS=1 cargo test -p
  muse-adapter` with the diff read, `scripts/captures.sh` byte-identical run to run under
  `HARNESS_DETERMINISTIC=1` (park the pointer outside the top-left 1440×900). Library: the
  same plus the all-features build (`--features aui-webview/wry,aui-terminal/pty,
  aui-terminal/tui`) and `python3 scripts/api-doc.py`.
- **Commits.** Commit on the branches as each package's gates pass, one commit per package,
  messages ending `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`. Never touch
  `main` in either repository; Fable fast-forwards after review.
- **Screenshots are the proof.** Read every capture you take (you can view images). Muse
  cannot, so never accept its word for a visual.

## Diagnoses already made (start from these, verify, do not redo)

- **D1 Approval buttons do nothing** (Allow once / Always allow / Reject) while a turn runs.
  The app log shows no `approval/decide` at all and no wire error, and the 1.2.1 schema for
  `approval/*` is unchanged: the click never reached the handler. Last round the same
  symptom in the palette was a scrim dismissing on mouse-down while gpui synthesises the
  click on mouse-up against the *release* frame (`gpui-pre-0.3.3/src/elements/div.rs`).
  Here the transcript re-lays out every tick (status timer, streaming), and the block list
  hints rows until measured (`transcript::turn_rows`), so an element id or bounds that
  changes between down and up kills the click. Check whether the keyboard path (`1`–`3`,
  `ApproveOnce`/`ApproveAlways`/`Deny` actions) works — if it does, the fault is confined to
  pointer synthesis. Reproduce without spend on `--replay fixtures/msp/transcript-approve-
  stage1.jsonl` (a capture cut with the approval pending) with a click test in the gpui test
  harness (the library's `aui/tests/keyboard.rs` and the round-2 `nav_round2.rs` click-sweep
  tests show how). Fix at the root, not by rebinding.
- **D2 Sidebar rows under a project** don't span the width at a wide sidebar and lose the
  right margin when open: the round-2 indent wrapper is `v_flex().pl(PJ_CHILD_INDENT)` around
  a `w_full` column, so the body overflows by the indent on one side, and the selected row's
  ground is content-sized on the other. Same class as the project row's `w_full + mx` bug.
  Rows must end at the same right edge as the group row at 240, 400 and 520 px sidebar
  widths (`--steps sidebar-width:<px>`).
- **D3 New projects sink.** `Projects::sorted` keys on the newest session; an adopted project
  with no sessions has no key. Sort on `max(newest session, last_opened_at, added_at)`, pinned
  first, so a newly added project appears at the top.
- **D4 No active-project indicator.** Add `ProjectGroup::current(bool)` to the library (name
  in `ink` at semibold with a 2 px accent bar at the row's left edge inside the margin — no
  new colour); the app sets it on the group of the active session.
- **D5 Usage twice in the footer.** The app passes "Power Usage · 38% weekly" as the plan
  label and the meter shows 38% again. The label is the plan name only; the meter row
  carries the number.
- **D6 The hover tray covers the branch** on a group row. While the tray is visible the
  branch and count hide (opacity 0, no layout shift).
- **D7 Trackpad scroll feel** (owner's recording: visibly stepped, not smooth). See R1.

## R1 — Scroll research (do this first, before `/tmp/round3-go`)

Research how gpui applications get smooth trackpad scrolling and write your findings to
`harness/docs/diagnosis/scroll-research-2026-09-13.md` before changing code:

- gpui itself (`~/.cargo/registry/src/*/gpui-pre-0.3.3/`): `ScrollWheelEvent`,
  `ScrollDelta::{Pixels, Lines}`, `ScrollHandle`, `list`/`ListState` (Zed's variable-height
  list), `uniform_list`, `overflow_scroll`, momentum/inertia handling on macOS
  (`platform/mac/events.rs`: `hasPreciseScrollingDeltas`, `momentumPhase`), and the frame
  scheduling while a scroll is in flight.
- Zed's own transcript-like surfaces (agent panel, editor) — how they use `list()` with
  measured rows and avoid height jumps; GitHub issues and discussions in `zed-industries/zed`
  on trackpad scrolling, inertia, "jumpy" or "stepped" scroll, `ScrollDelta` line-to-pixel
  conversion (e.g. the 20 px per line constant), and pixel snapping.
- gpui-component (`longbridge/gpui-component`, checkout at
  `~/.cargo/git/checkouts/gpui-component-*`) — its scrollable/virtual list and scrollbar,
  how it smooths wheel input.
- gpui-kit (`~/.cargo/registry/src/*/gpui-kit-0.6.0`, `gpui-base-0.6.0`) — anything on
  scrolling, springs, `Tween`.
- Then the harness: `crates/harness/src/session/render.rs` (`transcript_list`,
  `sync_virtual_list`, the wheel path), `transcript::turn_rows` (rows hinted until
  measured), `bench.rs` (`--bench <capture> --bench-scroll wheel --bench-out out.json`,
  `frame.series_us`). Measure before touching anything on `fixtures/msp/transcript-real.jsonl`
  and a 300-turn stress capture (`fixtures/msp/make-stress-300.py`): frame times, and a
  trace of scroll offset per frame during a synthetic wheel sequence (add the instrument if
  the bench lacks it). Name the cause: height hints replaced by measurements under the
  viewport (content jumps), line-delta quantisation, a scroll handle clamped during
  re-measure, frames not requested while momentum events arrive, or something else.

Then implement the fix (library `aui` if the list element is the library's, else the
harness), keep captures byte-identical, and show before/after `frame.series_us` and the
offset trace. Smoothness must hold on a real trackpad: run the app on `--replay` of the
stress capture and scroll it yourself with the instrument on; report what the trace shows.

## Work packages, in order

1. **Library package A** (`owner-round-3-2026-09-13`): D2 (row width and margin at any
   sidebar width; gallery `sidebar/views` gains a 520 px and a 240 px state), D4
   (`ProjectGroup::current`), D6 (tray hides branch and count), and whatever R1 needs in
   the library. Gates, commit.
2. **Harness package A**: D1 (approval clicks, with the click-test regression), D3 (ordering),
   D4 wiring, D5 (footer label), D7 (scroll), and the harness half of R1. Gates, captures,
   commit.
3. **UI audit** — `harness/docs/audit/ui-audit-2026-09-13.md`. Look at the app as a senior
   product designer would. Take fresh captures of every surface in both themes (login
   states, empty states, the hero, sidebar at three widths with groups open and closed, the
   header crumb and its menu, the Projects palette, the search palette, the view menu, the
   account menu, the archive and remove dialogs, a transcript with every block kind from
   the fixtures — thinking, tool cards, tool groups, approvals at each stage, questions,
   plans, todos, markdown, code, diffs, errors, markers — the composer with menus and
   attachments, the footer, the collapsed rail). For each surface list what is wrong under
   these headings: alignment and rhythm (does everything share the 4/8 px grid and the row
   metrics?), hierarchy (is the most important thing the most visible?), truncation and
   overflow at narrow and wide widths, states (hover, selected, disabled, running, error),
   consistency (same thing drawn the same way everywhere: counts, times, marks, chips),
   copy (labels, empty states, error text — plain, short, no jargon), theme parity
   (light matches dark), motion (nothing flashes, nothing jumps). Rank findings P1 (wrong
   or broken), P2 (visibly off), P3 (polish). **Fix every P1 and P2 in this round** —
   library package B then harness package B, gated and committed — and leave P3 in the
   document as a numbered follow-up list.
4. **User journeys** — `harness/docs/audit/journeys-2026-09-13.md`. J1, free: every journey
   that `--replay`, `--no-connect` and `--steps` can drive, scripted so it reruns:
   boot with no project → hero → adopt via `project:`; add a second project; switch via
   the header menu and via the palette; ⌘N in each; rename and recolour a project; pin,
   rename, hide, archive, unarchive a session; fold and unfold a group; search across
   projects and scoped; open every menu and dismiss it by Escape and by outside click;
   approvals and questions on the replay captures by keyboard and by click; collapse the
   sidebar and use the rail. Each journey is a `--steps` script plus a capture, with a
   pass/fail and the capture path. J2, live, at most six billed turns: a new session in a
   freshly adopted folder → a prompt that provokes a shell approval → Allow once → the
   result arrives → a prompt that provokes a question → answer it → the reply arrives;
   then reopen that session from the sidebar and from a second harness process (the
   session-in-use banner). Record every turn's session id and what it proved.
5. **Docs.** CHANGELOG entry "2026-09-13 — Owner round 3" per package; `docs/02-app.md`
   where the sidebar or scroll changed; `docs/05-handoff.md` "Where things are";
   `docs/12-projects.md` §8 appended.

## Report

When everything is committed, write `/tmp/round3-done` and end with a report that stands on
its own: per item done / skipped-with-reason; the scroll diagnosis in three sentences with
the before/after numbers; the audit's P1/P2 counts fixed and the P3 list; the journeys'
pass/fail table; the billed-turn count from Muse's logs against the baseline; every gate's
last lines verbatim per package; every capture path; every commit hash on both branches.
Never claim a gate you did not run.
