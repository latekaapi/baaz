# 79 — Handoff after Browser R4 (the daily driver)

Written 12 Nov 2026. **PUSHED 12 Nov** (owner said "push"): agentic-ui **v0.7.0** (f14eb0d), Baaz main repinned to
it with the local patch parked. The same day's owner rounds are in docs/78 ("Owner round, 12 Nov" 1–3): only Baaz's
own Muse sessions are listed, no random Muse names, no byline on click, Peek corners, white margins under a cover,
Split resizing, ad boxes hidden, Chrome account bookmarks. The rest of this doc is the state before the push; §0's
table is history. Per-step reports and every live-found defect: docs/78. Design: board `R4Daily` on canvas "Baaz v3"
(v68, approved 11 Nov).

## 0. Where things stand

| Repo | Head | Pushed | Notes |
|---|---|---|---|
| harness (Baaz) | `main` (after 6869251 + this doc) | **no** (origin at 7098eb9) | builds against agentic-ui through the **local patch** (`.cargo/config.toml` is ON); `Cargo.lock` + `crates/baaz-cef-helper/Cargo.lock` are modified and deliberately uncommitted (they carry `adblock` + `flate2`, re-resolved at repin) |
| agentic-ui | `main` 50ffd21 | **no** (origin at d949390 = v0.6.9) | 5 commits: bookmarks, tests, link gestures/request blocker/focus, spaces/footer/clicks |

The owner's own Baaz (`target/bundle/Baaz.app`) is untouched and still runs the 10 Nov build. A new build waits in
`target/bundle/next/` (perf-budget run); `scripts/bundle.sh --swap` after quitting Baaz installs it — only after push.

Gates on the final state: harness `cargo test --workspace` all green (1,6xx unit + integration), clippy clean in
both repos; aui 395 + aui-webview 76 + aui-cef 56 tests green; Tier V: every standing entry unchanged except
expected ones (right-files ×2 = the patch's `.cargo` row, as always with the patch on), new baselines committed for
the R4 entries (`field-history`, `field-bookmarks`, `field-bookmarks-scope`, `browse-peek`, `browse-split`,
`chrome-import-sheet`, `settings-browser`). Perf budgets (release, 12 Nov): 0 missed, 0 unmeasured. Clone-leak: no new crash reports from the check runs (one SIGSEGV on 11 Nov at quit with a Peek open — fixed, re-checked clean).

## 1. What R4 delivers (docs/27 §8)

1. **History in the Field** — `Browser/browser.db`; person tabs only; page words after 10 s; 90 days; exclusions;
   Clear… / Never remember this site.
2. **Bookmarks** — ⌘D popover, ★ in the address line, Bookmarks group + scoped view in the Field.
3. **Peek** (⇧-click) and 4. **Split** (⌥-click) — gallery design, wired.
5. **Ad blocking** — adblock-rust, bundled EasyList + EasyPrivacy, weekly refresh, per-site allow.
6. **Spaces with their own sign-ins** — a Chromium request context per space.
7. **Chrome import** — bookmarks + history, read-only, Undo; **no passwords**.
8. **Footer** — downloads and extensions popovers; **extensions do not run** (spike).

## 2. Owner decisions still open

1. **Extensions.** CEF lists them (`chrome://extensions` renders) but Baaz blocks `chrome-extension://` pages
   (only the PDF viewer passes) and has no action toolbar, so popups can't open. Options: (a) leave as is
   (ad blocking built in); (b) a spike that allows extension pages and installs one from the Web Store — that is
   a download, so it needs the owner's OK; (c) drop the button.
2. **Password import.** Not built: CEF exposes no importer and Chrome's password DB needs the "Chrome Safe
   Storage" keychain secret. Options: leave out (sheet says so) or a CSV route via Chromium's own
   `chrome://password-manager` if CEF ships it (unverified).
3. **Space sign-ins UI.** Built as a space-menu toggle + confirm instead of the board's New-space sheet; switching
   back keeps the own profile's data (the board said it signs out). Confirm or ask for the sheet.
4. **Ad toggle placement.** "Allow ads on this site" is a Field command (the board put it on the address line's
   menu, which the library draws). Confirm.
5. From docs/77 §4, still open: the Field's Ask row, the Assistant default, the R3 checklist, test leftovers.

## 3. Lessons (add to the list in docs/74 §2 / docs/77 §2)

- **Never `open_application` the check build** (memory `feedback-never-open-application-check-build`): it launched a
  second instance on the owner's real state on 11 Nov. Bring the scripted run forward by pid.
- **A killed cargo build can leave corrupt rlibs** (undefined `drop_glue` symbols at link). `cargo clean -p aui …`
  for the patched crates and remove `target/debug/incremental/baaz-*`.
- **A check run open during `baaz-check.sh build` parks the new bundle in `target/check/next/`** — the next launch
  is the *old* build. Quit first, or `BAAZ_BUNDLE_DIR=target/check BAAZ_BUNDLE_ID=sh.baaz.check scripts/bundle.sh --swap`.
- **CEF evals are function bodies**: `eval_with_result` wraps the script in `async () => { … }`; it needs `return`.
- **A click into a native page makes no Baaz frame**: anything that must react to it needs a page event
  (`GotFocus`, the page-script mousedown) to wake the view.
- **One visibility decision per page per frame**: flipping a page obscured/live twice in one frame never lets the
  stand-in settle.
- **Chromium in CEF reports nothing for ⌥-click** (`SAVE_TO_DISK` never reaches `on_open_urlfrom_tab`); the page
  script catches it.
- **Router claims**: ⌘↩ is now page-owned (forms) except while a Peek is open.
- FortiClient keeps an invisible window over parts of the screen that computer use refuses to click through; pick
  targets elsewhere.

## 4. Push recipe (when the owner says "push")

1. agentic-ui: tag `v0.7.0` at its head, push main + tag.
2. harness: repin both `Cargo.toml` files to `v0.7.0`; `git checkout` both lockfiles, then `cargo update -p aui` and
   `-p aui-cef` (in `crates/baaz-cef-helper` too) — the lock then picks up `adblock`/`flate2` as well.
3. Park the patch (`mv .cargo/config.toml ~/.baaz-dev/harness-cargo-config-patch.toml; rmdir .cargo`).
4. Build, run the gates, commit the repin, push. (The Files baselines did *not* simply match again: R4 added files to
   this repo, which the Files fixture lists. They were regenerated after the repin, on main, from a clean tree.)
5. Owner: quit Baaz, `scripts/bundle.sh --swap`, open it.

## 5. Owner's R4 check (docs/27 §8: "sets Baaz as default browser for a week")

The tracker's R4 checklist. Short form: import from Chrome; browse a day; find old pages by words in ⌘L; ⌘D a few
pages; ⇧-click to peek, ⌥-click to split; make a Work space with its own sign-ins and sign into a second account;
note pages that break with ad blocking and allow ads there.
