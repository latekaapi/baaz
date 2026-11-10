# 77 — Handoff: Browser R4, the daily-driver browser

Written 10 Nov 2026, after R3 closed and the owner's 9–10 Nov test rounds. Read this first in the next session; docs/76
covers R3 itself (per-step reports, owner decisions, how to recheck).

## 0. Where things stand

| Repo | Head | Pushed | Notes |
|---|---|---|---|
| harness (Baaz) | `main` 3e5bdd2 | **yes** | pinned to agentic-ui **v0.6.9**; local patch **parked** (`~/.baaz-dev/harness-cargo-config-patch.toml`), no `.cargo/` dir |
| agentic-ui | `main` d949390 = tag **v0.6.9** | **yes** | |

The owner's Baaz (`target/bundle/Baaz.app`) runs the 10 Nov build, which has everything below except the v0.6.9 repin
commit itself. The repin carries the same code.

Gates at the last push: harness 2,311 tests, aui-cef 53, clippy clean in both, Tier V clean (0 regions, 0 findings),
clone-leak-check passed. With the patch on, the two Files entries always differ by the `.cargo` row; that is expected,
never a reason to regenerate baselines.

## 1. What happened after docs/76 (9–10 Nov)

R3 was pushed (aui v0.6.7, baaz a53d578). Then the owner's own use found six bugs. Each was reproduced, fixed and seen
on screen before it was pushed:

| # | Symptom (owner) | Cause | Fix |
|---|---|---|---|
| 1 | ⌘-click opened a separate Chromium window that ⌘W couldn't close | ⌘-, middle- and ⇧-clicks reach CEF `OnOpenURLFromTab`, not `OnBeforePopup`; unhandled, Chrome style opened its own window | `on_open_urlfrom_tab` hands the URL to the host as a tab (aui fb950ab, v0.6.8) |
| 2 | The page "froze" after ⌘W | `close_tab` never unregistered the tab from the terminal service's `tab_pages` (added in R2), so the page was never dropped and its native view stayed over the next tab | unregister on close, discard and rebuild (baaz 6d6cf6d, b7ce8e0); test `closing_a_tab_by_hand_releases_its_page` |
| 3 | A new tab from ⌘-click or "Open Link in New Tab" appeared only after another click | the render re-armed the cached sidebar **before** draining the pages' open requests; a quiet window takes no frame until the next input | drain first, then re-arm and ask for a frame (baaz b7ce8e0); every open request is logged (`browse: open request …`) |
| 4 | The Field drew over every app, Chrome included | only the corner, line and card hid on deactivate; the Field, menus and modals closed on resign only after holding the keyboard | `setHidesOnDeactivate:true` on every float (baaz b0e7126) |
| 5 | Crash-report windows (test builds) | `Command::pre_exec(setsid)` makes std **fork** Baaz; the child died in libSystem's at-fork handlers (`_os_once_gate_corruption_abort`) before exec | `provider::session_spawn`: `posix_spawnp` + Apple `POSIX_SPAWN_SETSID` for the PATH probe and the Muse plan probe (baaz 4ddb7bd); guard test `no_spawn_forks_through_pre_exec` |
| 6 | Dark margins on Hacker News after a long session | a macOS appearance switch makes Chromium re-apply theme colours to every CEF view after ours; the white page ground became dark grey | re-set `PAGE_GROUND` in the views' `on_theme_changed` and the window's `on_theme_colors_changed` (aui d949390, v0.6.9) |

## 2. Lessons that cost time (add to docs/74 §2's list)

- **Test new-tab and redraw bugs with the pointer still.** Moving over a link changes the page's hover state, which
  redraws the window and hides a missing frame. Computer-use clicks made #3 look fixed until the owner's recording.
- **A tab that "does nothing" may be in the model and unseen.** Read `Browser/tabs.json` (ids, `last_shown`) and the
  `browse: open request` log line before guessing. Run the owner's own build with tracing:
  `open --env AUI_CEF_POPUP_TRACE=1 --stderr <file> target/bundle/Baaz.app`.
- **A copy of the owner's state reproduces state bugs safely.** rsync `~/Library/Application Support/Baaz` without
  `search.db*`, `codex-home`, `claude-home`, `run`, `logs` and `Singleton*`, then run the check build with
  `BAAZ_STATE_DIR=<copy> --no-connect --login signed-in`.
- **Never `pre_exec` in Baaz.** New sessions go through `provider::session_spawn`. On macOS, a session leader that
  opens a pty gets it as its controlling terminal, so `TIOCSCTTY` is not needed.
- **Theme bugs need a real appearance switch.** `CefWindow::ThemeChanged` (the `page-theme-changed` step) did **not**
  reproduce #6; only System Settings › Appearance did. Ask the owner to switch it: it's their system setting.
- **Every CEF view colour must be re-applied on theme change**, not set once. Applies to anything new in R4: Peek's
  window, Split's second page, Spaces' extra request contexts.
- **Floats must hide on deactivate** (#4). Any new float window (Peek is one) goes through `drop_native_chrome`.
- **Two Baaz builds named "Baaz" share the screen.** Bring the check build forward with
  `open_application sh.baaz.check` before typing. The owner's screen locks after ~3 minutes idle, and CleanShot's
  overlay and Problem Reporter can sit in front.
- Push recipe (used twice on 9–10 Nov):
  1. Tag aui at its head and push main + tag.
  2. Repin both harness `Cargo.toml` files.
  3. `git checkout` both lockfiles, then `cargo update -p aui` and `-p aui-cef` (in `crates/baaz-cef-helper`).
  4. Remove `.cargo/config.toml` **and** `rmdir .cargo`.
  5. Build, run the gates, commit the repin, push.

## 3. The milestone: R4 (docs/27 §8)

> `browser.db` history + bookmarks in the Field; Chrome import (passwords via Chromium's importer, bookmarks, history);
> adblock-rust; Peek; Split; Spaces with own sign-ins; downloads/extensions in the footer. **Owner check: sets Baaz as
> default browser for a week.**

Design: docs/27 §5 (Peek, Split, sidebar), §7 (data: `browser.db` SQLite with history + page-text FTS5 at dwell
≥ 10 s, per-site exclusions, never private; bookmarks; spaces; task audit; Spaces with own sign-ins = separate
`CefRequestContext` cache dirs), §9–10 (owner decisions, the prototype).

What exists to build on:
- **Spaces** (`browser/tabs.rs` `Space`, sidebar footer Personal/Work). Still missing: per-space sign-ins.
- **The Field** (R1/R3): tabs, commands, search. It has no history rows yet; docs/27 says "history once R4's db
  exists".
- **The downloads popover**, default-browser registration, `tabs.json`.
- **Peek and Split are drawn** in the approved gallery prototype (`aui-gallery` `browse/screen.rs`, `pages.rs`):
  wire those, don't redraw. `AppShell` has a `peek` slot.
- Nothing yet for `browser.db`, bookmarks, ad blocking, Chrome import or `CefRequestContext`.

**Conflicts to settle first:**
1. **⇧-click.** docs/27 makes ⇧-click open **Peek**, but since v0.6.8 it opens a background tab: CEF reports it as
   `NEW_WINDOW` through `on_open_urlfrom_tab`, and `link_opens_as_host_tab` takes it. R4 must route `NEW_WINDOW`
   from a ⇧-click to Peek instead. Keep the right-click menu's "Open Link in New Window" as a tab, or ask the owner.
2. **⌥-click.** docs/27 makes ⌥-click open **Split**, but Chromium uses ⌥-click to **download** the link
   (`SAVE_TO_DISK`, deliberately left to Chromium by `link_opens_as_host_tab`). Taking ⌥-click for Split loses that.
   Owner's call.
3. **Passwords in Chrome import.** "Passwords via Chromium's importer": confirm with the owner before touching their
   real Chrome profile. Import reads it; never write to it.

Suggested order (each step ends with something the owner can use):
1. `browser.db` + history in the Field.
2. Bookmarks.
3. Peek.
4. Split.
5. Ad blocking.
6. Spaces with own sign-ins.
7. Chrome import.
8. Downloads/extensions in the footer.

Decide with the owner whether any step needs a board first. Peek and Split are already drawn; Bookmarks and the import
sheet are not.

## 4. Still open from R3 (owner's calls; also on the tracker)

- The Field's **Ask** row still opens a Sessions draft. docs/27 §4.4 wants the answer in the Field; pointing it at the
  ⌘J inline answer was the suggestion.
- The Assistant default resolved to Muse (first connected); the owner may prefer Claude Haiku.
- The owner hasn't run the R3 checklist yet. Anything it finds comes before R4 work.
- `idle.cpu_pct` budget (0.5 %) fails intermittently (gpui display link), unchanged.
- Test leftovers: `~/Western_Ghats_Images`, an empty `~/Downloads/western-ghats-images`, two Muse test chats.

## 5. Prompt for the next session

> Drive Browser R4, the daily-driver browser, to completion. Read `docs/77-handoff-browser-r4.md` first, then
> docs/76 §2–§4, docs/27 (§5, §7, §8, §9–10), docs/74 §2, and the live Baaz Tracker
> (https://claude.ai/artifact/Fr965AKBn8toZ7BcUk7wCP). Both repos are pushed (agentic-ui v0.6.9, Baaz pinned to it).
>
> **Start by asking me, in one go, the conflicts in docs/77 §3:**
> - ⇧-click: Peek or a background tab?
> - ⌥-click: Split, or Chromium's download?
> - Importing passwords from my real Chrome profile.
> - Which R4 steps need a board.
>
> Also ask anything from docs/77 §4 that blocks R4. Then make boards on the "Baaz v3" canvas for only what isn't
> drawn yet (at least bookmarks and the Chrome import sheet), and get my approval before wiring new UI. Peek and
> Split are in the agentic-ui gallery: wire those, don't redraw them.
>
> Then build R4 per docs/27 §8, in docs/77 §3's order unless I say otherwise:
> - `browser.db` history in the Field;
> - bookmarks;
> - Peek;
> - Split;
> - ad blocking (adblock-rust);
> - Spaces with their own sign-ins;
> - Chrome import;
> - downloads and extensions in the footer.
>
> Rules:
> - Work on main, one commit per step, and run docs/69 §4's gates every step (Tier V under bash).
> - Check every step visually on the live app, at my real pane width as well as wide. You drive computer use from the
>   main session; ask me while I'm here. Test redraw-type behaviour with the pointer still (docs/77 §2).
> - Restore the local patch from `~/.baaz-dev/` only when the library must change, and never commit `Cargo.lock` while
>   it is on.
> - My Baaz runs from `target/bundle`: don't delete or rebuild that bundle, or quit or kill it, without asking.
>   `bundle.sh` parks new builds in `next/`. Kill only pids you started.
> - Never read or write my real Chrome profile, passwords included, without asking me first.
> - Tell me before any model turn.
> - Anything the docs don't answer comes back to me.
> - Don't push until I say "push".
>
> At the end: the tracker (merge into the live version, short checklist), memory, a handoff doc, and a report per step.
