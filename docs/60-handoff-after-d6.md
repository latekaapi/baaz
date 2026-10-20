# 60 — Handoff after D6 (2026-10-21)

Read this first in the next session. It replaces docs/40 §4 as the "where are we" for the build.

## 1. State of the code

| Repo | Branch | Head | Ahead of GitHub | Notes |
|---|---|---|---|---|
| harness (Baaz) | `main` only | `5b534ce` | 22 commits | speed P1 + D6 right pane + live-drive fixes + Shift-swipe spec + bundle.sh fix |
| agentic-ui (library) | `main` only | `2a8c493` | 12 commits | D6 components + live-drive fixes + Shift-swipe |

- **Nothing is pushed.** No side branches, no worktrees. Never push without the owner's OK.
- `Cargo.toml` still pins the library at tag `v0.5.2`. Local builds use the git-ignored `.cargo/config.toml`
  `[patch]` that points at `../agentic-ui`. `Cargo.lock` shows as modified because of that patch: **never commit it**.
- At push time (with OK): tag agentic-ui `v0.6.0`, repin Baaz to it, drop nothing else, regenerate the visual
  baselines on `main` in their own commit (`relay_visual.py … --update-baseline`, see CLAUDE.md), push both.
- Gates at `5b534ce` / `2a8c493`: Baaz 1,536 tests + clippy; library 820 (+1 swipe test) + aui-cef 47 + clippy — green.

## 2. What was built recently (all in the owner's current app)

- **D6 right pane + quote flow** — spec `docs/43-d6-spec.md` (incl. §2.11 settled details, §2.7 Recent pages,
  §7 live-check list). Canvas "Baaz v3" v51.
- **Performance M / P1** and **providers P3** — `docs/51-performance.md` §6–7, `docs/50-provider-parity.md`.
- **Owner change 2026-10-21:** pane screens switch only on **Shift + sideways swipe**; a plain sideways swipe
  scrolls the content (`aui/src/workbench/pane.rs`, capture-phase listener, test `only_a_shift_swipe_moves_screens`).
- **bundle.sh** now assembles `target/bundle/.Baaz.app.new` and renames it over `Baaz.app`, so rebuilding no
  longer kills a running Baaz (it used to: overwriting the running signed binary = silent SIGKILL).

## 3. Known open items (not bugs the owner reported — keep separate)

1. Shift-swipe over a **live web page** goes to the page (native CEF view takes the wheel), not the strip.
2. Tool-card shell output is not selectable (D4) → no code quote from it.
3. A pin's note can't be typed over a native page (the overlay window never takes the keyboard).
4. The sent quote's source-span flash is not built (the 260 ms scroll is).

## 4. How to verify (do this for every fix; "tests pass" is not enough)

**The owner's app:** `scripts/bundle.sh` → `target/bundle/Baaz.app` (`sh.baaz.app`, real state in
`~/Library/Application Support/Baaz`). Never stop the owner's running Baaz; rebuild, then tell them to quit and
reopen.

**A check build of your own (BaazCheck):** after `scripts/bundle.sh`, copy the app aside and re-identify it:
```
cp -R target/bundle/Baaz.app /tmp/x/BaazCheck.app   # any folder outside target/bundle
/usr/libexec/PlistBuddy -c "Set :CFBundleIdentifier sh.baaz.check" -c "Set :CFBundleName BaazCheck" /tmp/x/BaazCheck.app/Contents/Info.plist
codesign -s - --force --deep /tmp/x/BaazCheck.app
SD=$(mktemp -d "$TMPDIR/bz.XXXX")   # short path: unix sockets cap at 104 chars
env -i HOME=$HOME USER=$USER PATH=/usr/bin:/bin:/usr/sbin:/sbin:$HOME/.local/bin:/opt/homebrew/bin TMPDIR=$TMPDIR \
  BAAZ_STATE_DIR=$SD /tmp/x/BaazCheck.app/Contents/MacOS/Baaz --workspace <scratch git repo> &
```
`--theme light` for light. A scratch repo with a failing pre-commit hook and a 350-file folder exercises Changes
and Files; a local bare repo as `origin` exercises Push without GitHub.

**Computer use:** works once the owner has **"computer control" on** in the Claude app and the screen is unlocked
(an instant "denied" = one of those two). Then `request_access(["sh.baaz.check"])`.
- Drags and text selection need `left_mouse_down` + several `mouse_move`s with short waits + `left_mouse_up`;
  `left_click_drag` is too fast for gpui.
- Escape never reaches the app through computer use — test Esc paths in code/tests.
- Real agent turns: send them by typing in the app's composer (Claude Code on the Max login; Muse is
  pay-as-you-go). Launching `--steps "send:…"` from Bash is blocked by the auto-mode classifier.

**Traces (env vars):** `BAAZ_STICKY_TRACE=1` (Changes sticky row), `BAAZ_DRAFT_TRACE=1` (commit/PR drafts),
`AUI_CEF_SETUP_TRACE=1` (page script setup replies), `AUI_CEF_CAPTURE_TRACE=1`.

**Where things live:** Claude Code sessions Baaz starts → `~/Library/Application Support/Baaz/claude-home/projects/`
(not `~/.claude`). Baaz's log → stderr of the process.

**Visual probe:** `cargo build -p baaz` then the 18-entry `relay_visual.py` command in CLAUDE.md. On `main` it
reports the D6 right-pane changes until the baselines are regenerated at push time (expected, exit 1).

## 5. Rules carried over

- Diagnose before fixing: reproduce, find the cause, fix it, then **see it fixed live** on a rebuilt app.
- One commit per fix, with what was seen and why it happened. Tests that drive real events where possible.
- Never push without asking. Never commit `Cargo.lock` while the local patch is on.
- Design questions the spec does not answer go to the owner; do not invent.
- Tracker: **"Baaz Tracker"** https://claude.ai/artifact/Fr965AKBn8toZ7BcUk7wCP — read the live version first,
  merge, then publish. Update its checklist/plan when bugs are fixed.
