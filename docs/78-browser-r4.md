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

(one section per step, written when it lands)
