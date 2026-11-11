# 78 — Browser R4, the daily driver: decisions and per-step reports

Started 11 Nov 2026 from docs/77. Design: docs/27 §5/§7/§8. Board: canvas "Baaz v3" v68, `R4Daily`
(generator `docs/34-refs/v3-redo/canvas/gen_r4.py`).

## 1. Owner decisions (11 Nov)

- **⇧-click → Peek.** ⌘-click, middle-click and right-click › Open Link in New Tab/Window stay tabs.
- **⌥-click → Split.** Chromium's ⌥-click download is given up; Save Link As… stays in the context menu.
- **Chrome import:** build bookmarks, history and passwords; test on the owner's real Chrome profile for
  **bookmarks + history only**, read-only. Passwords are tested only against a throwaway profile.
- **Boards first** for bookmarks, the import sheet, Spaces sign-ins and the footer: drawn as `R4Daily`,
  **approved as drawn** ("Approved, wire it"). Extensions and password import depend on a CEF spike; a failed
  spike comes back to the owner.

## 2. Steps

### Step 1 — `browser.db` history in the Field (commit 76be3af, baselines c05588a)

- `crates/baaz/src/browser/history.rs`: `Browser/browser.db` (WAL). `visits` (url, title, host, first/last ms,
  count) + `pages_fts` (FTS5 over title and page words) + `excluded` hosts. In-memory under
  `BAAZ_DETERMINISTIC`/tests, so a probe never touches the owner's history.
- Recorded from `sync_browser` (`record_history`): only `Owner::Person` tabs, only `http(s)`; a URL change is a
  visit, a late title updates it. The page in front gets its words read once it has stayed 10 s (a timer wakes the
  quiet window); every dwelt visit refreshes them. Kept 90 days, pruned at open.
- The Field: title/address matches first, then a words-only match showing its line (`“…”`); a page open as a tab
  shows once. Cached per query (render/arrows/Enter never re-query).
- Field commands **Never remember this site** (forgets what was kept too) and **Clear history…** (confirm dialog).
  Settings › Browser › **Your data** (board 2a): count, Clear…, the never-remembered sites with Remove.
- Steps: `history:<url>|<title>|<days>|<words>`; Tier V entry `field-history`.
- Live (check build, 11 Nov): a visited page shows in the Field as "Learn Rust … just now". **Found live:** page
  words were never kept — CEF wraps an eval in an async function body (needs `return`), and the answer arrived
  with no frame to collect it, by which time the tab had navigated and the answer was dropped. Fixed in step 2's
  commit (return + two wake-ups + keep words for the URL they were asked for).

### Step 2 — Bookmarks (board 1a–1c)

- `browser/bookmarks.rs`: `bm_folders` (built-in "Bookmarks bar" id 1, "Other" id 2) and `bookmarks`, both with a
  `source` so an import's Undo removes only what it wrote.
- ⌘D (`BookmarkPage`, Browse context) or the Field's **Bookmark this page**: adds to the Bookmarks bar and opens
  the popover under the address line (overlay kind `Bookmark`, takes the keyboard like the Field): Name, Folder
  (inline list), Remove, Done; ↩/Escape/click-away keep what's typed.
- The address line keeps a filled ★ while the page is bookmarked (library `BrowseModel::bookmarked`,
  `BrowseIntent::Bookmark`); the click opens the popover. Cached per URL, never a query per frame.
- The Field: a Bookmarks group between tabs and history (folder path as the hint); a bookmarked page's history
  row folds into it. **Bookmarks** command = the scoped view: scope pill (library `Field::scope`,
  `FieldIntent::Unscope`), folders then recently added; ↩ on a folder opens it; ⌫ on an empty query goes up/out.
- Library (agentic-ui, unpushed): `star`/`star-fill` icons, `FieldRowKind::Bookmark`/`Folder`, the scope pill.
- Live: ⌘D → popover → rename "HN front page" → Other → Done → found in the Field under Bookmarks with "Other";
  ★ reopens it; Remove clears the ★; the scoped view lists folders and opens one. **Found live:** ⌫ did nothing
  (the input eats it before the panel's listener; moved to a capture listener) and the scoped input kept the
  default placeholder (now "Search bookmarks").

### Steps 3–4 — Peek and Split (gallery design, wired)

- Library: `aui_webview::LinkGesture { Peek, Split }`, `WebBackend::take_link_gestures`, `WebEvent::LinkGesture`.
  aui-cef maps `on_open_urlfrom_tab`'s `NEW_WINDOW` (⇧-click) to Peek and `SAVE_TO_DISK` (⌥-click, Chromium's
  download) to Split; context-menu opens never come through there, so "Open Link in New Window" stays a tab.
- `browser/peek_split.rs` + `app/peek_split.rs`. **Peek** is a loose page (no tab), 72 % × 82 %, rounded 12; while
  it is open the page beneath freezes into its stand-in under the gallery's 35 % scrim (a click on it closes);
  the × is a hole cut through the Peek page; the caption ("host · ⌘↩ open as a tab · esc closes" + Open as tab)
  is a dark pill under the card. Esc / ⌘↩ work from the page (router claims them only while peeking) and from gpui.
  ⌘↩ hands the same page to a new tab after the opener — no reload. A gesture inside the Peek navigates it.
- **Split**: ⌥-click opens the link as a tab beside the opener, side by side (6 px gap in the sidebar ground);
  the shown tab is the focused half (address line), a click into the other half shows it; each half's × (a hole)
  leaves Split and keeps the tab (owner default).
- Live-found: the stand-in never settled (the visibility pass and the Peek pass flipped the page each frame —
  now one decision per page); ⌘↩ never reached the router (focus sits in gpui after opening — a centre key
  handler covers it); a quit with a Peek open SIGSEGV'd in Chromium shutdown (the Peek now closes before
  `aui_cef::shutdown`); the caption was unreadable over the dimmed page (now a pill).
- Steps `peek:<url>`, `split:<url>`; Tier V entries `browse-peek`, `browse-split`.

### Step 5 — Ad blocking

- Owner default: EasyList + EasyPrivacy bundled (`resources/adblock/*.txt.gz`, fetched 11 Nov) and refreshed
  weekly into `Browser/adblock/` with `/usr/bin/curl` (a fresher download wins at the next launch).
- Library: `aui_cef::set_request_blocker` — asked on Chromium's IO thread for every sub-resource with the ABP
  request type; navigations and downloads are never asked; a blocked load gets a handler that cancels it.
- `browser/adblock.rs`: adblock-rust 0.13.5 (multi-thread build: Send + Sync), engine built on a background thread
  at the first Browse page (~1 s), prefs in `layout.json` (`adblock`: enabled, allowed sites). Settings › Browser ›
  **Ad blocking** (switch + allowed sites with Remove); Field command **Allow ads on this site** (toggles, reloads,
  toast). Deviation from the board: the per-site toggle is a Field command, not the address line's menu (the ⋯
  menu is library-drawn; the Field is where every other page command lives).

### Step 6 — Spaces with their own sign-ins

- Library: `aui_cef::CefBackend::new_at_in(…, profile)` — a `CefRequestContext` per profile folder under the
  Chromium root (`Browser/Chromium/Spaces/<id>`), made once and reused; popups inherit it. `BrowseSpace::own_signins`
  draws a lock on the chip; the space menu gains "Use its own sign-ins…" / "Share sign-ins with other spaces…".
- Baaz: `Space::own_signins` (tabs.json); every page (tabs, Peek) is built in its space's profile
  (`tab_profile`); flipping it asks first, drops the space's pages (they rebuild in the other profile when shown)
  and keeps the own profile's data on disk (switching back finds the sign-ins again — less destructive than the
  board's "signs it out", which would delete them).
- Deviation from the board: no separate New-space sheet; the choice is on the space's menu (the chip's
  right-click), behind a confirm dialog. The lock on the chip is as drawn.
- Live (check build, 12 Nov): httpbin cookie set in Personal is absent in Work after "Use its own sign-ins"; a
  cookie set in Work is absent in Personal. Found live: the space menu ran off the window's bottom (its size table
  still said 2 rows) — fixed, and menus now flip up / clamp inside the window.

### Step 7 — Chrome import

- `browser/chrome_import.rs` + `chrome_import_ui.rs`: profiles from Chrome's `Local State`; `Bookmarks` JSON read in
  place; `History` copied to a temp file and opened read-only (last 90 days, never an excluded site, never a page
  already known); every row carries `import-<ms>` so Undo removes exactly that import. Settings › Browser › Your
  data › Import… opens the sheet (profile list, two ticks with counts read off the main thread, Import, Done with
  Undo). Captures and tests only ever read `BAAZ_CHROME_ROOT` (a synthetic profile in `fixtures/chrome-profile`).
- Passwords: **not built** (the board marked it as a spike). Writing into CEF's password store from outside isn't
  possible without Chromium's importer, which CEF doesn't expose; the sheet says passwords and sign-ins stay in
  Chrome.
- Live (owner's real Chrome, read-only, into the check build's throwaway state): two profiles found; both have no
  `Bookmarks` file (verified against the files); 6,879 and 648 history pages counted; importing 648 worked, Undo
  restored exactly the previous 3 pages, re-import found the pages in the Field with their dates.

### Step 8 — Downloads and extensions in the footer

- Library: the downloads button moved from the page controls to the footer (always there, dot while running),
  beside a new extensions button (`IconName::Puzzle`, `BrowseIntent::Extensions`).
- Baaz: both popovers open above their button in the overlay window (below when the button is high); an empty
  downloads list says "No downloads yet." with "Open Downloads folder".
- **Extensions spike result:** `chrome://extensions` renders and could list extensions, but Baaz blocks every
  `chrome-extension://` page except the PDF viewer and has no action toolbar, so an installed extension could not
  open its popup or pages; installing one from the Web Store (a download) was not tried without the owner's OK.
  The popover says extensions don't run yet and points to ad blocking. Owner decision needed (see handoff).
