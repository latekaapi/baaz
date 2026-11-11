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
