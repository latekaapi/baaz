# 75 — Handoff: next milestone, Browser R3 (the agent window)

Written 7 Nov 2026, evening, at the close of R1 ∥ R2 (docs/74). Read this first in the next session.

## 0. Where things stand

| Repo | Head | Pushed | Notes |
|---|---|---|---|
| harness (Baaz) | `main`, pinned to agentic-ui **v0.6.6** (dcf4f9e, helper too) | yes | `.cargo/config.toml` patch parked at `~/.baaz-dev/harness-cargo-config-patch.toml` |
| agentic-ui | tag **v0.6.6** (f44799c) | yes | |

**R1 ∥ R2 is CLOSED** (owner, 7 Nov: "looks okay for now"). docs/74 §1c–§1e has what landed, what was seen live and the
two owner test rounds. The owner's Baaz is `target/bundle/Baaz.app`, swapped to the f44799c build. After the three checks
asked for (the recoloured Pay card, question cards, screenshot links) the owner said "looks okay for now"; nobody but the
owner has seen them in the app.

Carried open from R1/R2 (docs/74 §1c "Known gaps"; not R3's job unless R3 touches them):
- `browser_ask` is answerable only through the line.
- Blur over a live page was never proven: the overlay windows draw solid.
- Some gpui UI still covers a Browse page by the old method: Settings and its sheets, the ⋯ and extensions popovers,
  the Field's rows with the sidebar closed, feedback, and the crash page.
- Dead code was left in for the owner to decide on: the ⋯/extensions popovers and `MenuKind::BackHistory`.
- `idle.cpu_pct` misses its 0.5 budget at about 1–1.5 % with the window visible. This is the display-link wake from
  docs/51 §9 and docs/73 §5, and it is a known perf-gate finding, not a regression.
- A real provider session using the browser tools has not been run (a turn costs money; announce it).

## 1. The milestone: R3, the agent window (docs/27 §8)

docs/27 §4.5, §6 and §8. What R3 delivers:
1. **The ⌘J line** with the composer's chips ("This page" ×, `+` for another tab or a file) and the site's actions.
   Small asks answer inline: a card grows out of the line, and a draft lands in the page's own box with nothing sent.
2. **The floating card (⌘E) and the docked column (⌘⇧E).** These are one window in three sizes. Its content is a Baaz
   session of kind `browser` in a hidden "Browser" project, rendered by the transcript component in a narrow style.
   - The card is about 420 × 560 and draggable.
   - It shows the title and model, one line of what the agent is doing (click it for every step), and the steps
     collapsed above the answer.
   - The agent's questions take the input's place. ↩ steers, ⌘↩ queues.
3. **Settings › Browser › Assistant:** the provider and model. The default is the first connected provider's cheapest
   model, the same as the auto-titles picker. The Agents level (Ask / Auto, per site, your own tabs) is in docs/27 §6.2.
   On the tracker this is "the Assistant page (rest of D5)".
4. **Handoff to a project session** ("continue this in *Fix login*", "send this to a new session in harness") through
   Baaz's handoff (docs/22).
5. **Task folders with faces**, and **Done closes tabs**.

Owner checks (docs/27 §8): "summarise this and save the images to a folder"; "research ten sites, then continue in
harness".

### 1a. What already exists (reuse, don't rebuild)

- **Agent line, approval card, working rows, compact layout:** agentic-ui `crates/aui/src/browse/agent_window.rs`
  (v0.6.6). The harness float window is `OverlayKind::PageFloat` in `crates/baaz/src/overlay.rs`. It has **no themed
  root**: every component drawn there must set its own text colour (the cause of the 7 Nov black title).
- **Gallery prototype the owner approved on 5 Nov:** `browse/agent-card`, `browse/agent-column`, `browse/line-states`,
  `browse/line-narrow`, `browse/task-faces`, `browse/field`, `browse/sidebar`. These are the visual spec; wire them,
  don't redraw them.
- **R2 tools and approvals:** `crates/baaz/src/browser/automation/`, the `browser` profile, and the docs/15 table.
  `scripts/agent-live-check.py` proves them on a real Chromium page with no model.
- **Live driver:** `scratchpad/line_demo2.py` from the last session drove BaazCheck to the real Pay ask through the
  socket, with no model. Copy its approach (seed `layout.json` with `rightWidth` and the `onboarding_completed` marker).
  The scratchpad itself does not survive.

### 1b. Decisions owed by the owner before wiring (on the tracker since 5 Nov)

1. Keep the "From sessions" heading?
2. Task folders: a status dot plus a word, or the round faces? R3 lists "faces", so confirm.
3. The line stays under the floating card because Take over lives there. Keep that, or move Take over into the card?
4. The Field: keep the approved October board (anchored on the address line), or switch to the prototype's centred one?

Also design work that is new and so needs a board and approval before it is built (rule: mock first):
- The Settings › Browser › Assistant page.
- The handoff picker from the card.
- The inline answer and draft-in-page states of the line, if the gallery doesn't already cover them.

Check the gallery first. Only draw what is missing, as boards on the "Baaz v3" canvas.

### 1c. Owner answers, 8 Nov (the four §1b questions)

1. "From sessions" heading: **drop it**, hairline only.
2. Task folders: **round faces + word**.
3. The line **stays under the floating card** (Take over stays on the line).
4. The Field: **switch to the prototype's centred Field** (gallery `browse/field`), replacing the October anchored board.

Boards for the undrawn pieces (inline answer, draft in page, handoff picker, Settings › Browser › Assistant) are on
"Baaz v3" v64 as `R3Agent` (generator `docs/34-refs/v3-redo/canvas/gen_r3.py`), awaiting approval.

## 2. Rules that bit (carry over; docs/74 §2 has the full list)

- New UI: a board, then owner approval, then build. FF values are the spec. Count the concepts (≤ 6).
- The native CEF page draws above all gpui UI (memory `feedback-native-page-above-gpui`). Anything over the page is an
  overlay window, and a page that is off screen takes no input.
- **Float windows have no theme root.** A library component placed in one must set `text_color` itself. Gallery frames
  hide this because the gallery root supplies colour, so check a bare host too.
- Size a float window from an exact height function, as `approval_height` does, never from a minimum width. The
  owner's right pane is about 400 pt (a line of about 356 pt). Test narrow.
- Gates every step:
  - docs/69 §4: tests, clippy, Tier V on every CLAUDE.md entry under **bash** (zsh does not split `$E`), pointer parked,
    perf budgets.
  - `scripts/clone-leak-check.sh` when startup, quit or CEF changed.
  - `scripts/agent-live-check.py` when the automation changed.
  - Build checks with `scripts/baaz-check.sh build`, not `bundle.sh`.
- `bundle.sh` parks at `target/bundle/next` while the owner's Baaz runs. The owner quits, then runs `--swap`. Ask
  which build they are on before diagnosing a report.
- Live checks: the main session drives computer use (in subagents it aborts). Before any windowed run, check that the
  screen is unlocked (`CGSSessionScreenIsLocked`) and read HIDIdleTime, because every launch steals focus.
- **Never `cargo fmt --all`.**
- Disk: keep ≥ 25 GB free.
- A float window doesn't appear in window screenshots, so the owner's screenshot tool omits it.
- Library changes: restore the patch, never commit `Cargo.lock` while it is on, then at push tag, repin both
  `Cargo.toml` files, `cargo update -p aui` in both workspaces, and park the patch.
- No push until the owner says "push". No model turns unless announced.

## 3. Prompt for the next session

> Drive Browser R3, the agent window, to completion. Read `docs/75-handoff-browser-r3.md` first, then docs/27 (§4.5,
> §6, §8 at least), docs/74 §1c–§2, and the live Baaz Tracker (https://claude.ai/artifact/Fr965AKBn8toZ7BcUk7wCP).
> Both repos are pushed (agentic-ui v0.6.6, Baaz pinned to it).
>
> **Start by asking me the four open prototype questions (docs/75 §1b) in one go.** Then look at the agentic-ui
> gallery's `browse/*` entries and list what R3 needs that isn't drawn yet: the Assistant settings page, the handoff
> picker, and the line's inline answer and draft states. Make boards for only those, on the "Baaz v3" canvas, and get
> my approval before wiring any new UI.
>
> Then build R3 per docs/27 §8:
> - the ⌘J line with chips;
> - the card (⌘E) and the column (⌘⇧E) as a `browser`-kind session in a hidden project, rendered by the transcript
>   component;
> - Settings › Browser › Assistant;
> - handoff to a project session;
> - task folders with faces;
> - Done closes tabs.
>
> Wire the approved gallery components; don't redraw them.
>
> Rules:
> - Work on main, one commit per step, and run docs/69 §4's gates every step (Tier V under bash).
> - Check every step visually on the live app, at my real pane width as well as wide. You drive computer use from the
>   main session; ask me while I'm here.
> - Restore the local patch from `~/.baaz-dev/` only when the library must change, and never commit `Cargo.lock` while
>   it is on.
> - My Baaz runs from `target/bundle`: don't delete or rebuild that bundle, or quit or kill it, without asking.
>   `bundle.sh` parks new builds in `next/`. Kill only pids you started.
> - Tell me before any model turn; the owner checks in docs/27 §8 need one.
> - Anything the docs don't answer comes back to me.
> - Don't push until I say "push".
>
> At the end: the tracker (merge into the live version, short checklist), memory, a handoff doc, and a report per step.
