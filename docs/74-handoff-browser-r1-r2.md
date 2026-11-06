# 74 — Handoff: next milestone, the clone leak (step 0), then the browser (R1 re-shell ∥ R2 agent core)

Written 31 Oct 2026, at the close of "close both tracks" (docs/69–73). Read this first in the next session.

## 0. Where things stand

| Repo | Head | Pushed | Notes |
|---|---|---|---|
| harness (Baaz) | `main`, pinned to agentic-ui **v0.6.4** | yes (31 Oct) | `.cargo/config.toml` patch parked again at `~/.baaz-dev/harness-cargo-config-patch.toml` |
| agentic-ui | tag **v0.6.4** | yes | |

Both tracks (providers equal, Zed-fast) are closed: docs/73 lists what landed, the defects found live, and what is
still open. The owner's Baaz is `target/bundle/Baaz.app`; `scripts/bundle.sh` now **parks** a new build at
`target/bundle/next/Baaz.app` while a Baaz is running (a mid-run swap mixed two builds' Chromium helpers and is the
likely cause of the 31 Oct hang); `scripts/bundle.sh --swap` after the owner quits. Baaz now writes
`<state>/logs/baaz.log` and a stall watchdog (`stall:` lines, `logs/stall-*.txt` samples) — read them first when the
owner reports a hang.

## 1. Step 0, before any browser work: the code-sign clone leak

**The problem (owner, 5 Nov).** Baaz leaks Chromium's code-sign clones, about 400 MB listed per launch, and they filled
the disk: `$(dirname "$TMPDIR")/X/<bundle-id>.code_sign_clone/code_sign_clone.XXXXXX`. Counts before the owner cleaned
up, in another session: `sh.baaz.app` 200 clones (69 GB listed, 5–31 Oct), `sh.baaz.check` 142 (52 GB), `sh.baaz.d6check`
8, `sh.baaz.m3check` 2. The CEF spike apps `apps.tauri.cef-rs.cef-s1` and `.cefsimple` leaked too; Chrome had only 4. It
comes first because R1 and R2 launch Baaz constantly, and this is the disk trap that killed earlier runs.

**Checked 5 Nov (read-only).** These are facts:
- CEF is 154.0.33.0. The framework contains `CodeSignCloneManager`, `../../chrome/browser/mac/code_sign_clone_manager.mm`,
  `kMacAppCodeSignClone`, `kMacAppCodeSignCloneRenameAsBundle`, `Mac.AppCodeSignCloneCount` and a process type
  `code-sign-clone-cleanup`. So the owner's recalled names are real in this build, and clones are created by the
  Chrome layer CEF embeds.
- Now: `sh.baaz.app` has 1 clone (`Baaz.app.bundle`, 400 MB listed) and the running Baaz has files open in it (`lsof +D`
  shows it). The other IDs have 0. Chrome holds 3, one dated 28 Oct, so Chrome also keeps some for a while.
- No `code-sign-clone-cleanup` process runs alongside Baaz or Chrome, so if that helper is what removes clones, it does
  not sit waiting for the whole session. When it is launched is **unknown**.
- `aui_cef::run_helper` passes every process type to `execute_process` with no `App`
  (`agentic-ui/crates/aui-cef/src/runtime.rs:323`). `aui_cef::shutdown` calls `cef::shutdown()` from `on_app_quit`
  (`crates/baaz/src/main.rs:977`). `--steps` runs can leave through `std::process::exit` (`main.rs:569`), which skips it.

**Not known yet. Prove these, don't assume them:**
1. Which step removes a clone, and whether it runs under CEF at all. Candidates: the manager's teardown never runs under
   CEF's shutdown; the cleanup helper is never launched; it is launched but `Baaz Helper` / `execute_process` doesn't
   honour that process type; it runs but fails, for example on a path, or because the bundle moved or the signature is
   ad hoc. The spike apps leaking points at CEF rather than Baaz code, which is suggestive and nothing more.
2. How much disk a clone really costs. APFS clones share blocks, so "400 MB listed" may not be 400 MB used until the
   original bundle is rebuilt or deleted. Measure physical use (`df` deltas), and report it beside the listed size.
3. What launched the 200 clones in 26 days, at about 8 a day. Compare bundle launches, `--steps` runs that exit early,
   and `--bench`/perf runs. Unbundled `target/debug` runs may not clone at all.

**The plan, in order:**
1. **Test rig first.** A separate bundle ID and output, never `target/bundle`. Note `scripts/bundle.sh` has a fixed `OUT`
   and parks at `target/bundle/next`, so add an output override, or copy `next` and rename its ID as BaazCheck was built
   (`PlistBuddy` + `codesign -s - --force --deep`). `target/debug` is gone, so the first build is a full rebuild.
2. **Reproduce and measure.** Clone count and physical disk before and after each: normal quit, SIGTERM, SIGKILL, a dev
   rebuild and relaunch, and a `sh.baaz.check` run. Repeat each at least 3 times. Also trace a normal quit (`--type=`
   processes via `ps`, Chromium's log at verbose level) to see whether cleanup starts and how it ends.
3. **Root cause, with evidence.** Read `code_sign_clone_manager.mm` for this Chromium version. State plainly if the
   hypothesis is wrong.
4. **Fix normal exits** in the right layer (`aui-cef` if it is the runtime or helper; that means an agentic-ui tag and
   repin, restoring the patch from `~/.baaz-dev/`). Also check the `std::process::exit` paths.
5. **Startup sweep for hard kills,** own bundle IDs only, off the UI thread, safe with several instances. Suggested
   in-use test, to be confirmed by the measurements: a clone is in use if any process has files mapped or open inside it
   (`proc_pidinfo` region paths, or `lsof +D` as the running Baaz shows), and a clone younger than a grace period is
   skipped so an instance that has cloned but not yet mapped is safe. Explain why the check is reliable, and test
   running-instance, concurrent-launch and crashed-instance cases. Keep Chrome and every other app's directory out of
   reach by construction: an allowlist of exact directory names, no globbing on `*.code_sign_clone`.
6. **Check and test builds clean up after themselves,** including on failure (a `trap` in the launch helper). Decide
   whether the spike IDs need anything: they are the owner's old experiments, so ask before touching them.
7. **Disabling clones** (`MacAppCodeSignClone` through `DISABLED_FEATURES`, `runtime.rs`) is only an option to explain,
   with its cost. Clones keep signature validation working if the bundle changes while the app runs, and Baaz is rebuilt
   a lot. Possibly fine for test builds only, if the evidence supports it.
8. **Regression script:** launch, quit, kill, then assert the clone count stays bounded. It must skip safely on a machine
   without CEF. Add it to the gates in §2.
9. **Same step, small:** Tier V leaves one `$TMPDIR/baaz-capture-state-<pid>` per capture (1,079 had piled up, from
   `store::capture_state_dir`). Make captures remove theirs.

**Constraints from the owner:** never touch another app's clone directory. The owner's Baaz launches from `target/bundle`:
do not delete or rebuild that bundle, and do not quit or kill that instance, without asking. Kill only pids you started.

**Finish with:** the root cause, what changed, and before/after clone counts per scenario. Then move on to §1b.

### 1a. Result (5 Nov): root cause and fix

**The hypothesis was wrong.** Teardown does run under CEF, the `code-sign-clone-cleanup` helper is launched, `Baaz Helper`
honours that type, and it removes the clone about a second after a normal quit (three times out of three, `ps` shows the
helper with `--unique-temp-dir-suffix`, parent gone, directory gone). Upstream: `~CodeSignCloneManager` launches the helper,
which polls `getppid() != 1` and deletes; the only other cleanup is a TODO (crbug.com/343784575) and the reboot.

**What leaks is any run that never reaches CEF's shutdown:**

| How the run ended | Clones left, before | After the fix |
|---|---|---|
| Normal quit (`cx.quit`, ×3) | 0 | 0 |
| Quit before Chromium clones (first ~4 s) | 0 | 0 |
| **SIGTERM** (`pkill`, `kill`) (×3) | **1 each**, status 0, no teardown | 0 |
| SIGINT, SIGHUP (×3) | 0, but the process dies of a trace trap | 0, clean exit |
| **SIGKILL** (×3), force quit, crash | **1 each**, they add up | 1 each, removed by the next launch's sweep |
| **Failed `--steps` run** (`process::exit(1)`) (×3) | **1 each** | 0 |
| Rebuild swapped under a running app, then quit (×3) | 0 | 0 |
| **Bundle deleted while running**, then quit (×3) | **1 each** (no helper binary to launch) | 1 each, swept |
| Check recipe (main ID renamed, helper IDs not) (×3) | 0 | n/a: `baaz-check.sh` renames both |
| Unbundled `target/*/baaz` (Tier V, perf budgets) | no clone at all: no framework to load | same |

Chromium owns the first case: measured, with CEF off SIGTERM ends with 143 (default), with CEF on with 0 and no teardown.
It installs handlers about 1.1 s after launch, later than `CefInitialize` returning.

**What launched 200 + 142 clones.** Not the owner's use: the log shows 2 real launches of `sh.baaz.app` in the last five
days. My own and my agents' scripted runs: in these projects' transcripts, 678 Bash commands launch a Baaz bundle, 271 of
them kill it in the same command, and 137 are `pkill`/`killall`. That is an upper-bound attribution, not a count per clone
(the clones themselves were already deleted).

**Real disk cost** (`df` on the data volume): a clone costs **0 MB** while its source bundle exists; deleting that bundle
frees **0 MB**; deleting the clone then frees **400 MB**. So each stale clone pins one build's 400 MB as soon as a rebuild
replaces the bundle. Listed size and real cost converge exactly because Baaz is rebuilt so often.

**What changed (harness only, no library change):**
- `crates/baaz/src/quit_signals.rs`: SIGTERM, SIGINT, SIGHUP all quit through `cx.quit` → `on_app_quit`, so a signal also
  saves pending state now. A second signal or 8 s without finishing ends the process: a hung Baaz stays killable. Chromium's
  handlers are taken back as they appear. **Known window:** a signal in the ≤25 ms between Chromium's install and the next
  check still ends Chromium's way (1 of 6 when deliberately timed there, on a 100 ms check); the sweep covers it.
- `crates/baaz/src/browser/clone_sweep.rs`: sweep at launch, off the UI thread, and `baaz --sweep-clones`. In-use test:
  Chromium hard-links the main executable into the clone, so the clone's `Contents/MacOS/*` inode *is* what its owner runs,
  even after the bundle is swapped or deleted (confirmed: `ls -i` shows the same inode, and `lsof +D` shows the running
  process holding it and nothing else). Every other process's executable inode comes from the kernel (`proc_listallpids`,
  `proc_pidpath`, `stat`; spawns nothing), taken **before** the clone list; an instance clones seconds after it execs, so
  the owner of any listed clone is in the list. A list that does not include this very process removes nothing. (The first
  version spawned `lsof` at launch; it raced a provider probe's `fork` and the forked child crashed in `_notify_fork_child`:
  7 crash reports on 5–6 Nov, a Problem Reporter window seen live on 6 Nov. Fixed 6 Nov.) Two instances of one build share the inode, so while another
  runs, stale clones of that build wait for a later sweep: the safe direction. Exact directory names (`OWN_IDS`), no glob,
  no symlinks, only `code_sign_clone.XXXXXX`; `other_apps_and_unexpected_names_are_out_of_reach` tests that.
- `shot.rs`: a failed-steps exit shuts CEF down first.
- `scripts/bundle.sh`: `BAAZ_BUNDLE_DIR` / `BAAZ_BUNDLE_ID`, so a test build never lands on `target/bundle`.
- `scripts/baaz-check.sh`: `build` (`target/check/Baaz.app`, `sh.baaz.check`) and `run` with a trap that stops its pid
  gently, then sweeps, on normal exit, failure, Ctrl-C or its own SIGTERM. **Use this for every check run from now on.**
- `scripts/clone-leak-check.sh`: the regression, 27 checks on its own ID `sh.baaz.leaktest`: every ending above, sweep with a
  running instance, three concurrent sweeps, three concurrent launches, a dead clone of another build, other apps' directories
  unchanged. Skips with a message where there is no CEF. Now part of the gates (§2). Its first version had a false green: a
  second build at `Baaz.app.b` is not a bundle, so Chromium's startup check failed, no clone appeared, and the assertion passed
  on nothing. A wait that sees no clone is now a failure.
- `store.rs`: a capture removes its `baaz-capture-state-<pid>` at quit and on a failed exit; any launch removes those whose pid
  is gone.

**Disabling clones** (`MacAppCodeSignClone`, `DISABLED_FEATURES`): not done. Clones exist so code-signature checks on the
running app keep passing when its bundle changes underneath it. Without one, a rebuild or update while Baaz runs can make
Keychain and other signature-based checks fail. It matters most in a shipped signed build that auto-updates. For check builds it
would probably be harmless, but it would make them differ from what ships, and with the above it saves nothing measurable.

**Not touched:** `apps.tauri.cef-rs.*` directories are empty now, and those spikes are the owner's. Nothing here reaches them.

## 1b. The milestone

docs/27 (browser redesign v2, decided by the owner 7 Oct) §8: **R1 re-shell** in harness and **R2 agent core** in the
library, in parallel, merging at R3. The owner asked for a clickable prototype in the agentic-ui gallery before wiring
(docs/27 §10). The canvas boards for Browse are on "Baaz v3" (`docs/34-refs/v3-redo/canvas/`).

- **R1 re-shell (harness):** docs/27 §4 cut list (delete the old Browse shell); overlay windows (the Field, menus, the
  line) over the native page; no header over the page — nav + address line in the sidebar, page full height; pins /
  your tabs / folders / task folders / Space footer; the Field (tabs + commands + search); ⌘S hide with the 28 px
  lights strip and edge reveal; the right pane shares the address component. Owner check: Browse looks like the mockup,
  nothing freezes, a session's dev-server tab shows as a task folder.
- **R2 agent core (library + seam):** `PageAutomation` on CEF; the §6.1 tools with `tab`; snapshot/refs/diff;
  console/network/errors/emulate; `browser_ask`; approvals (§6.2); the line's take-over / hand-back; session tabs render
  off-screen "as if visible". Owner check: from a coding session, "open :5173, click through checkout, show me console
  errors" works; "pay" stops at the approval bar.

Alternative the owner may pick instead: D7/D8 (Skills page, dialogs, app chrome) — the tracker's "Next" list.

### 1c. Status of §1b (5 Nov): built, live check owed, nothing pushed

| Repo | Head | State |
|---|---|---|
| harness | `eadd0a5` (5 commits on 33a555c) | not pushed; `.cargo/config.toml` patch ON; `Cargo.lock` and the helper's lock modified, **do not commit** |
| agentic-ui | `d7e2505` (2 commits on v0.6.4) | not pushed, not tagged |

**Owner approved the prototype 5 Nov** ("Approved, wire it"). Four open questions on the tracker: "From sessions" heading, dot vs
faces, line under the card, Field's position (I kept the approved Oct board).

**As built.** docs/40 §2's sidebar supersedes docs/27 §5. R1: cut list deleted (b6a0e19: −983 lines), downloads in an overlay, Field
commands, menus close on a mode switch (eadd0a5). R2: docs/15 has the table (0674cc4); `crates/baaz/src/browser/automation/` plus
`terminal/service.rs` `RunningJob`; each tool is an async job, testable with `script::run_deciding`.

**Proven:** `scripts/agent-live-check.py` on a real Chromium page, screen locked, no model: snapshot, console, uncaught error, failed
request, trusted fill, click diff, `since: last_action`, Pay waits and clicks nothing, screenshot, phone viewport, a second tab read
in the background.

**Seen live on 6 Nov** (check build, computer use, a real Chromium checkout page driven over the socket, no model): the approval
card over the live page in Browse; Don't allow (the tool answered `stopped`, nothing clicked); Pay (clicked, `window.paid` true);
"needs you" on the task folder; the line's label following the job ("waiting for the page"); Take over → "You're driving · The agent is
waiting"; Hand back (the wait finished); the agent's click on a download link and the downloads list over the live page
("report.csv · Done · saved by Live"); ⌘T's field finding "Find in page"; quit from the menu with a page open (exit 0, 0 clones).
Five defects found and fixed in the same pass (stale folder word and label, card wording, "e5" in the label, a stale `navigated`, and
the launch sweep's `lsof` crashing a forked child; see the commit after bfe8a1c).

**Still owed:**
1. **Typing in the ⌘L Field did not land** through computer use, on a tab the agent never touched too (the page kept the keyboard,
   `document.hasFocus()` true). Either a real regression or the tool's key events not reaching the overlay panel. **Ask the owner to
   try ⌘L + typing in their own Baaz (33a555c)**: if it works there, test the check build by hand.
2. A real provider session using the tools (announce it: a turn costs money).
3. Blur over a live page was never proven (the overlay windows draw solid).

**Known gaps:** `browser_ask` is not answerable except through the line; `browse_ask` stays until R3; still covering a Browse page for
gpui UI: Settings and its sheets, the ⋯ and extensions popovers (steps only), the Field's rows with the sidebar closed, feedback, the
crash page. Dead but left, for the owner: ⋯/extensions popovers, `MenuKind::BackHistory`. Behaviour the deletion changed is listed in
the b6a0e19 report: a held tab survives a mode switch, ⌘W with one showing stops showing it, relaunch in Browse then Sessions opens
no session.

**At "push":** tag agentic-ui v0.6.5 and push; set every `aui*` pin (also `crates/baaz-cef-helper/Cargo.toml`); park the patch
(`mv .cargo/config.toml ~/.baaz-dev/harness-cargo-config-patch.toml`); `cargo update -p aui` in both workspaces; regenerate
`right-files` and `right-files-reveal` baselines with the patch off, in their own commit; run the gates; commit locks + pins; push both.

**A gate trap from this run:** in zsh an unquoted `$E` does not split, so `relay_visual.py $E` exits 2 having tested nothing; run it
under bash. `cmd && git commit` after a piped test run does not gate on the test: check the failure count.

## 2. Rules that bit (carry over)

- New UI → a board on the "Baaz v3" canvas and owner approval before building (feedback: mock first); FF values are the
  spec; count concepts (≤ 6).
- The native CEF page draws above all gpui UI; a page that is not on screen must take no input and draw nothing
  (fixed 31 Oct in aui-cef `shim.m`: event monitor, bubble order-out, mouse-leave on hide) — keep it that way.
- Gates every step: docs/69 §4 (tests, clippy, Tier V on every CLAUDE.md entry with the pointer parked, bundle + perf
  budgets), plus `scripts/clone-leak-check.sh` when anything near startup, quit or CEF changed. Launch check builds only
  through `scripts/baaz-check.sh`. `idle.cpu_pct` can miss at ~1 % with the window visible: known display-link wake (docs/51 §9, docs/73 §5).
- Live checks: `computer_batch` aborted with "user interrupt" in **subagents** even with the Mac idle (343 s), three
  runs in a row, and worked first try from the **main session**. Background `app_*` clicks do not reliably drive gpui. So
  the main session drives the live test itself on `sh.baaz.app` with the owner's OK, or a cheap agent runs scripted
  `--screenshot` captures on a state copy that includes `claude-home`/`codex-home`. Offscreen captures cannot see the
  native page or real pointer input.
- Never write the owner's real store from a probe (captures fall back to a temp dir now); throwaway logins for
  anything that signs in; no Muse turns unless announced.
- Disk: keep ≥ 25 GB free; `target/debug` grows to 40+ GB — delete it when it does. `$(dirname "$TMPDIR")/X/` is the other place to look (§1).
- Don't push until the owner says "push"; library changes ship as an agentic-ui tag + repin.
- Gate builds use `scripts/baaz-check.sh build`, not `bundle.sh`, while the owner's Baaz may quit mid-chain.

## 3. Prompt for the next session

> Drive the next Baaz milestone to completion. Read `docs/74-handoff-browser-r1-r2.md` first (it has uncommitted edits;
> commit them with your first commit), then docs/27 (all of it), docs/73 (what just closed), and the live Baaz Tracker
> (https://claude.ai/artifact/Fr965AKBn8toZ7BcUk7wCP). Both repos are pushed (agentic-ui v0.6.4).
>
> **Step 0, before any browser work: fix the Chromium code-sign clone leak, per docs/74 §1.** It filled my disk. Follow
> §1's plan: build a separate test bundle and ID, reproduce and measure (normal quit, SIGTERM, SIGKILL, dev rebuild and
> relaunch, `sh.baaz.check` run), find the root cause with evidence (say so if the hypothesis is wrong), fix normal
> exits, add a startup sweep for our own bundle IDs that can never remove a running instance's clone and is safe with
> several instances, make check and test builds clean up after themselves including on failure, explain what disabling
> clones would cost rather than doing it to avoid the work, and add a repeatable script that asserts the count stays
> bounded. Never touch Chrome's or any other app's clone directory. My Baaz runs from `target/bundle`: don't delete or
> rebuild that bundle, and don't quit or kill that instance, without asking me first. `target/debug` is gone, so expect
> a full rebuild. Report the root cause, what changed, and before/after clone counts per scenario, then continue.
>
> **Then the browser: R1 re-shell (harness) ∥ R2 agent core (library)** per docs/27 §8–§10. Start with the clickable
> gallery prototype docs/27 §10 asks for and any board the design still lacks, and get my approval before wiring new UI.
>
> Restore the local patch from `~/.baaz-dev/` only when the library must change, and never commit `Cargo.lock` while it
> is on. Work on main, one commit per step, docs/69 §4's gates every step, and check every step visually on the live app
> (docs/74 §2 says who can drive it; ask me for computer use while I'm here). Anything the docs don't answer comes back
> to me. Don't push until I say "push". At the end: tracker (merge into the live version, short checklist), memory,
> handoff doc, report per step.
