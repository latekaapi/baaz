# 76 — Handoff: Browser R3 (the agent window) built, and what comes next

Written 8 Nov 2026, at the close of R3 (docs/27 §8, docs/75). Read this first in the next session.

## 0. Where things stand

| Repo | Head | Pushed | Notes |
|---|---|---|---|
| harness (Baaz) | `main` 447ae10 (10 commits on 496356c) | **no** | `.cargo/config.toml` patch **ON**; `Cargo.lock` and `crates/baaz-cef-helper/Cargo.lock` modified — **do not commit them** |
| agentic-ui | `main` e997ac6 (5 commits on v0.6.6 f44799c) | **no**, not tagged | |

**At push** (only when the owner says "push"): tag agentic-ui **v0.6.7** at e997ac6, push it; repin both harness
`Cargo.toml` files (`crates/baaz`, `crates/baaz-cef-helper`) to v0.6.7; `cargo update -p aui` in both workspaces; park
the patch (`mv .cargo/config.toml ~/.baaz-dev/harness-cargo-config-patch.toml`); build; commit the repin; push both.
Then regenerate the Files baselines (`right-files`, `right-files-reveal`) on main with the patch parked — their only
diff in every Tier V run this session was the patch's `.cargo/` row.

The owner's Baaz (`target/bundle/Baaz.app`) was never touched. All live checks ran on the check build
(`target/check`, `sh.baaz.check`).

## 1. Owner decisions this session (8 Nov)

- The four §1b questions: **no "From sessions" heading** (hairline only); **round faces + word**; **the line stays
  under the card**; **the Field moves to the prototype's centred one**.
- R3 board (canvas "Baaz v3" v64 → v65, `R3Agent`): **approved as drawn** — inline answer opens no task folder until
  ⌘E; Undo + Done on the draft, posting is the person's; ↗ handoff in the card header, the chat stays usable after.
- **Agents block built in R3** (Ask/Auto, your tabs, sites).
- **⌘⇧E docks the column everywhere**; the effort picker moved to **⌃⌘E**.
- Model turns for the two owner checks: approved ("Yes, run both"), on Claude Code / Haiku.

## 2. Reports, step by step

Every step: `cargo test --workspace` (harness 2305 / agentic-ui 870, 0 failed), clippy clean in both, Tier V on every
CLAUDE.md entry under bash (only the two Files entries differ: the `.cargo/` row), perf budgets (final: **0 missed**;
`idle.cpu_pct` missed once at 1.1 mid-session — the known display-link finding of docs/75 §1), clone-leak-check
passed, agent-live-check 18/18. Live checks on BaazCheck with computer use, at 1440 wide, at Baaz's 900 minimum, with
the column docked (~200 px page) and in the Sessions pane at the owner's 400 pt.

**S1 — task folders with faces** (`5204f24`; aui `569d071`). The folder head draws the round `TaskFace` and its word:
needs you → paused (you drive) → typing → reading → done (`task_face_for`, tested). The "From sessions" heading is
gone. Seen live: the folder's face and word; "needs you" in the warning colour during an approval.

**S2 — the centred Field** (`e55065f`). Baaz's Field is now the library's `browse/field`, 640 wide, 68 from the window
top, Baaz's ranking kept. Live finds, fixed: empty keycaps; Search grouped under TABS; Enter opened the typed text
instead of the arrowed row (`browse_omnibox_moved`); no theme font in the float (see S4).

**S3 — browser chats + Assistant** (`c2d00e2`). A browser chat is a hidden session in `<state>/browser-chats` on
Settings › Browser › Assistant's pick (default: the first connected provider's cheapest model). Live: the default
resolved to **Muse** on this machine (it is first connected) — the owner's choice of Claude Haiku is one click in the
menu; worth knowing.

**S4 — the ⌘J line** (`0706a14`; aui `569d071`, `bd05c84`). Ask row with "This page" (the person's tab is granted to
the chat; read-only under Your tabs: Read only) and site actions; inline answer; draft-in-page with Undo/Done. Live
finds, fixed: **float windows had no gpui-kit Root** (16 px rem, system font → text 14 % large, heights wrong); **a
non-modal float lands 6 px in from its frame on every side** (`r3_frame` asks 6 more; the docked column does not lose
it); ⌘J/⌘E/⌘⇧E dead while a float held the keyboard; the chip not returning; narrow "Continue in a card" overflow.

**S5 — card and column** (`db39232`). The card floats above the line's real window; the column narrows the page.
Live finds, fixed: the column overlaid the page (the centre now pads); card over the line; answers repeated on line and
card. Seen: card at 1440 and with the column at the 900 minimum.

**S6 + S7 — handoff, task folders, Done closes tabs** (`5dbe6d3`). Picker (recent sessions, New session in each
project) → the target opens with the chat's answer and pages as an **unsent draft**; tabs move; the card records
"Continued in … · Open". Done closes a seen-working session's tabs after 15 min (not the tab on screen). Live finds,
fixed: no wake for an idle window (timer); raw instruction as the folder name; handoff draft lacking "This page".
Known: after a close in an idle window the sidebar's highlight can sit stale until the next frame.

**S8 — Settings › Browser › Assistant + Agents** (`a2c73f1`). Model menu (drops down; aui `PickerMenu::drop_down`).
Ask/Auto, your tabs, sites with Remove; Never here refuses acting and scripts; Always here never asks; Ask asks before
any action; "Always on this site" on an approval adds a rule. Seen live through the socket on a real page, in Browse and
in the 400 pt pane.

**End to end with a real model** (`447ae10`; aui `e997ac6`). The two owner checks with Claude Code / Haiku found nine
more defects, all fixed (first ask dropped while opening; Claude Code's allow/deny refused as "accept"; prompts unseen;
first navigate built no page; the card's ↩ started a second chat; narration as the answer; WebFetch out of sight; Muse
listing browser chats; Settings change not starting a new chat). Then:
- **"summarise this and save the images to a folder"** — passed: summary in the card (markdown), both images saved
  (`~/Western_Ghats_Images/lake.png`, `trail.png`, real 480×300 PNGs) after one Bash approval shown with its command.
- **"research ten sites, then continue in harness"** — the mechanics passed: the chat opened its own tabs (Wikipedia,
  Lonely Planet, Rough Guides, Karnataka Tourism …) in a task folder (reading → done), the line was the driving bar,
  ↩ steered it from the card, and ↗ → harness opened a new session with the answer + pages as an unsent draft. Haiku
  itself researched shallowly (it first used one local page, then gave up reading some sites Baaz could read fine — the
  socket read Wikipedia/Lonely Planet/Rough Guides text without trouble). A stronger model in the Assistant fixes that;
  not a Baaz defect.

## 3. Left behind, for the owner to know

- Test artefacts on this Mac from the owner checks: `~/Western_Ghats_Images/` (2 PNGs) and an empty
  `~/Downloads/western-ghats-images/`. Delete when convenient.
- Two Muse test chats now live on the owner's Muse account (from the first attempts before the Assistant was set to
  Claude Haiku); they no longer show in Baaz (recognised by preamble), but Muse's own list has them.
- The Field's **Ask** row still opens a new Sessions draft (pre-R3 behaviour). docs/27 §4.4 wants the answer in the
  Field itself, which was never drawn; pointing it at the ⌘J inline answer is the suggestion. **Owner's call.**
- The Assistant's model list is fixed per provider (Claude Code's haiku/sonnet/opus aliases, Codex default, Muse Spark
  1.3), not each provider's live catalogue (Baaz keeps none without a session).
- Not done in R3 (by design or out of scope): queue strip for ⌘↩ in the card (it queues on the provider; no strip);
  the card's ＋ (attach) does nothing yet; Take over lives on the line only (owner's choice).
- Carried from docs/75 §1: blur over a live page unproven; old-method covers (Settings sheets, popovers…); dead code
  for the owner to decide; `idle.cpu_pct`.

## 4. How to check it again (no model)

`python3 <scratchpad>/r3live.py start` was the driver; its approach is in this doc's commits' free steps:
`--steps "fixture-session:Live;wait:9000;browser-chat-fixture:<answer|steps|draft>;agent-line;agent-card;agent-column;handoff-picker"`,
`BAAZ_WINDOW_SIZE=900x860` for the narrow window, `BAAZ_DONE_CLOSE_SECS=20` for Done closes tabs, the socket for tools
(as `scripts/agent-live-check.py` does). Float windows don't show in window captures; `screencapture -l <id>` on the
float's own window id (from CGWindowList) does. The owner's screen locks after a couple of idle minutes; Baaz draws
nothing then (steps stall, captures stale) — `caffeinate -d -i -u -t 7200` (owner-approved) held it.

## 5. Next

**Superseded by docs/77** (10 Nov): R3 was pushed (v0.6.7) and six owner-found bugs were fixed and pushed after it
(v0.6.8, v0.6.9, baaz 3e5bdd2). docs/77 has those, the lessons, and the prompt for Browser **R4, the daily driver**.
