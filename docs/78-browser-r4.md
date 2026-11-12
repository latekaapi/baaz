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

## Owner round, 12 Nov — CLI sessions in the sidebar, random titles, the byline on click

From the owner's recording (Muse rows titled `muted-aster`, `brook-vulpecula`…; clicking one changed its second line).

**Diagnosis (measured on the owner's Mac).**
- Muse keeps one session list for every client. Baaz drew all of it: 2,874 sessions, of which Baaz made 187.
  1,702 were CLI runs in temp folders: `muse exec` from the owner's exam script (`T/exam-muse-*`, 1,450),
  `condense-muse`, and relay lanes. Muse's own index records the creating client
  (`msp_created_by_client_name`; NULL = the CLI) since 13 Sep, and every CLI log says `host="cli" mode="exec"`.
- The "random word" titles are Muse 1.4's automatic session names. Every one of the 2,618 named sessions has one,
  and none was typed. `session/list` carries no `nameOrigin`, so Baaz read each as a name a person gave.
- Clicking a row replays its history. The replay "settles", and every settle wrote the free byline excerpt, so the
  row's first prompt became its last reply. This was decided on 30 Oct ("the free excerpt still lands") and is now
  reversed.

**Fix.**
1. `index.rs` reads the creating client. `sidebar::made_elsewhere` treats as not the person's (like a side session:
   no row, no Show-hidden count, no search entry, no title call) a session that is:
   - made by another client, or
   - run in a temp folder (Import's rule; this run's own state dir excepted),
   - unless it was pinned, named, put in a project or handed off in Baaz.
   A stored byline is not evidence: the click bug wrote one onto every exam run the owner opened. Unknown (older
   Muse, an old cache) never hides.
2. Muse's `name` and the index's `session_name` are no longer labels. A row reads Baaz's title, Muse's title or
   the first prompt.
3. The byline lands only for a turn this window ran (`live`).
4. Baaz's own helper sessions are recognised by their start labels (`titles::SIDE_LABELS`) from any state dir.

**Result on the owner's data:** 2,108 listed rows → about 100. Checked live in the check build (connected, on a copy
of the owner's `sessions.json`/`projects.json`): Unfiled 64 → 6, with no exam runs, no `baaz auto-title` rows and no
slug titles. `open:<id>` on a probe session left its second line and its store entry unchanged (only the project the
reopen restates).

**Left as is:** a handful of Baaz-made probe sessions in real folders ("Reply with one word: OK" under harness).
Nothing tells them from the owner's; archive them by hand. A CLI session is no longer reachable from Baaz at all;
a Muse lane in Import would bring chosen ones in, if wanted.

## Owner round, 12 Nov (2): Peek corners and black page margins

From the owner's screenshot: HN under a Peek had black margins (white again after closing), and the Peek card was
square beside the rounded page.

- **Black margins:** the covered page shows as a JPEG stand-in (`Page.captureScreenshot`). The white behind a page
  with no background of its own is the native view's ground, which the screenshot never includes, so HN's margins
  were transparent and the JPEG made them black. aui-cef now sets each page's default background to white
  (`Emulation.setDefaultBackgroundColorOverride`) at page setup.
- **Square Peek:** the × is a hole cut with a layer mask, and that mask replaced the content view's rounded clip. The
  mask's outline now uses the page's corner radius (`shim.m`). The pane's own ground also gets that radius when it
  has no nav row (`aui-webview`), otherwise its square corners showed over the dim.
- Seen live in the check build (HN, then Peek on example.org): four rounded corners with the dim outside them, and
  the margins under the dim white. The Peek/Split baselines are unchanged (the deterministic capture never had the
  hole). agentic-ui f14eb0d.

## Owner round, 12 Nov (3) — Split resizing, empty ad boxes, Chrome bookmarks

- **Split panes not resizable.** The 6 px gap between the halves is now a divider (aui `resize_handle` plus a
  `drag_capture_overlay` mid-drag; role Splitter, "Resize Split"). Each half holds at least a fifth of the
  centre. The ratio resets to 50/50 for each new Split. Geometry is in `peek_split::{split_rects,
  split_ratio_at, split_left_width}`, with the × holes following the halves. Step `split-ratio:<f>`. Checked
  live: the scripted 30/70 split, and a real drag with computer use (30 % → about 65 %). Pages and ×s follow it.
- **Empty ad boxes.** Network blocking left each page's ad slot behind (Indian Express: a big grey
  "ADVERTISEMENT" box). Now each loaded http(s) page is scanned for its classes and ids, once at load and again
  2.5 s later (`COSMETIC_SCAN_JS`). adblock-rust's `url_cosmetic_resources` and `hidden_class_id_selectors`
  produce the hiding rules, installed as one `display:none` stylesheet (`#baaz-adblock`). The site's
  `#@#`/generichide exceptions and "Allow ads on this site" are honoured. The scan's answers share the page's
  script-answer queue with the page-word reads, which used to drop unknown answers; one drain now routes every
  answer by id. Checked live: indianexpress.com logs "hiding 445 ad boxes' rules", and both boxes are gone.
- **No bookmarks imported.** Recent Chrome keeps a signed-in account's bookmarks in `AccountBookmarks`. The
  owner's profile has no `Bookmarks` file and 4,257 bookmarks there. Both files are read now, and merged per root
  (repeats are dropped by the store). The result line no longer points at an "Imported from Chrome" folder when
  nothing came in. Checked live, read-only: the sheet counts 4,226 web bookmarks for "Bharani". Nothing was
  imported in the check run; the owner imports in their own Baaz.
