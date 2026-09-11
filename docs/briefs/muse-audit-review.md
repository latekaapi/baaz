# Brief — full code review and performance audit (findings only, no edits)

You are the lead for a read-only audit of `/Users/latekaapi/Projects/harness` (branch
`main`, clean tree) and of the parts of `/Users/latekaapi/Projects/agentic-ui` (checked-out
branch `login-methods`) that the harness renders through. **Change no source file in either
repository.** Your only writes are new files under `harness/docs/audit/`. Do not commit.
Prefix every shell command with `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`.
Spend rule: never `turn/start`, `--send`, `send:`/`steer:`, the live tests, `harness-probe`,
`fixtures/msp/probe*.py`, `muse logout` or `account/logout`. Free: `--replay`, `--no-connect`,
cargo build/test/clippy/doc, `muse schema`.

Use a workflow with five children, one per area below, run in parallel (they only read),
then one integration step. Each child writes `docs/audit/<area>.md`; the integrator writes
`docs/audit/00-findings.md`. Use `--parallel-tool-calls`.

## What a finding looks like

One bullet each, in this exact shape so they can be triaged and turned into packages:

`- **[<area>-<n>] <title>** — `path:line` — <severity: high|medium|low> — <what is wrong,
one or two sentences, citing the code you read> — <the fix, concretely>`

Severity: high = user-visible bug, data loss, unbounded per-frame cost, blocking IO on the UI
thread; medium = dead/legacy/duplicated code, a function that mixes two concerns, an
allocation per frame that scales with data; low = naming, docs, lint-level polish. Report
what you verified, not what you suspect: if you could not confirm, say "unverified".

Read first: `docs/07-architecture.md`, `docs/05-handoff.md`, `docs/09-handoff-improvements.md`,
`docs/diagnosis/login.md` §4, `CLAUDE.md`. Mechanical facts already known (do not re-derive,
do build on them): `cargo clippy -W clippy::pedantic -W clippy::nursery` reports ~600
warnings, the biggest classes being 82 `missing_const_for_fn`-style "this could be a const
fn", 78 `use_self`, 70 `needless_pass_by_ref_mut`, 26 "called X on an X value", 25 redundant
closures, 19 needless pass-by-value, 11 redundant clones, 11 identical match arms;
`cargo machete` flags `gpui_platform` unused in `crates/harness` and, in agentic-ui,
`aui-webview` (aui-icons, aui-motion, aui-protocol), `aui-gallery` (gpui_platform),
`aui-terminal` (aui-icons), `aui-tokens` (serde), `aui-protocol` (anyhow); the `muse-client`
methods `turn_cancel`, `view_subscribe`, `view_unsubscribe` have no caller in the workspace;
`crates/harness/src/app.rs` carries one `#[allow(dead_code)]` (`reconnect_after_login`, kept
by decision D25 pending a live check); `fixtures/msp/{probe_*.py,run*.py,drive.py}` and
`crates/muse-adapter/src/bin/harness-probe.rs` are phase-era scripts; the transcript is a
virtualised gpui `list` over `Rc<Vec<Turn>>` (good); the sidebar builds every row every
frame; the library's `prose()` re-parses markdown on every render; `record_frame_stats`
measures element construction only, not layout/paint; a `--replay` of the 300-turn stress
capture costs 0.39 s CPU and 200 MB RSS in a debug build (mostly gpui baseline).

## Areas

1. **client-adapter** — `crates/muse-client` (client.rs, frame.rs, schema.rs, tests) and
   `crates/muse-adapter` (fold.rs, failure.rs, tests, the `harness-probe` bin). Structure,
   dead/legacy code, duplicated logic, error handling, the fold's per-event cost
   (does an event touch only its turn?), allocation patterns, test coverage gaps, the
   3.7k-line schema mirror's organisation.
2. **app-core** — `crates/harness/src/app.rs` (4k lines), `session.rs` (4.2k),
   `transcript.rs`. Concern separation (what should be its own module: login, sidebar,
   dialogs, menus, steps/scripting, tier, drag/resize…), oversized functions
   (`render` 165 lines, `render_transcript` 207, `block` 263, `step` 174), duplicated
   patterns (the background-call-then-update shape appears dozens of times — is a helper
   warranted?), stale comments and decision references that no longer match the code,
   state that is never read.
3. **support** — every other harness module: sidebar, overlays, search, index, history,
   sessions, files, images, skills, tier, auth, conn, main, shot, plan, full_output,
   attachments. Same questions, plus: what is legacy from a phase that later work
   superseded, what is only reachable from a scripting flag, what the phase scripts
   under `fixtures/msp` and the `scripts/` dir still serve.
4. **performance** — the whole harness, measured not guessed: per-frame work in every
   `render*` (allocations, clones, string formatting, sorting, filtering, env lookups),
   blocking IO or process spawns on the UI thread, `cx.notify()` storms (who notifies on
   every event, what re-renders as a result), the sidebar and palette with hundreds of
   rows, the composer on every keystroke, the fold on a 300-turn stream, image decoding,
   sqlite access, startup path (what runs before the first frame), memory growth over a
   long session (what is retained forever), animations that keep frames running while
   idle. Use `HARNESS_FRAME_STATS=1` and `--replay fixtures/msp/synthetic-stress-300.jsonl`
   with `--steps` to drive frames; write down the numbers you got and the exact command.
   Propose a `--bench` mode design (streaming replay at a cadence + programmatic scroll +
   whole-frame timing) as one finding, with the metrics it should print.
5. **library-hotpaths** — in agentic-ui, only what the harness renders each frame:
   `crates/aui/src/transcript/*` (prose/markdown parsing per render, turns, tool cards,
   selection), `nav/*` (sidebar rows), `composer/*`, `screens/login.rs`,
   `data/*` used by them, `aui-motion` (do springs/tweens keep requesting frames when
   settled?). Per-frame parsing and allocation, API that forces the caller to clone,
   dead code. Read-only there too.

## Integration step

`docs/audit/00-findings.md`: every finding from the five files, de-duplicated, grouped
into these implementation packages with an estimated size (S/M/L) each and the order they
must run in (library packages before harness packages; mechanical cleanups before
structural refactors before performance work): **A. mechanical cleanup**, **B. dead and
legacy code removal**, **C. structure/refactor**, **D. performance**, **E. library**.
Finish with the ten highest-value items in one list. Report the file list and the counts
per severity.
