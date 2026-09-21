# Changelog

All notable user-visible changes to this project are documented here.

## 0.1.0 — unreleased

First public release. A native macOS chat client for Muse Code
(`muse serve`, MSP over stdio), built with [gpui](https://www.gpui.rs) and
the `aui` component library.

- **Chat over Muse Code.** Drives `muse serve` as a child process and speaks
  MSP (JSON-RPC 2.0 as NDJSON over stdio); signs in the way the CLI does
  (device code or an API key).
- **Search finds a project by name.** The project's name is indexed now,
  rather than only reachable when its path happened to appear in transcript
  text. A renamed project answers to both the name on screen and the folder
  it lives in.
- **Stop always settles the view.** A turn the server had already finished
  left the composer counting and answered Stop with a red banner, with
  nothing else to try. Baaz now takes the server's answer and settles.
- **Sessions do not need a project.** Muse needs a folder for every session,
  so Baaz makes one — `~/baaz-sessions` — at first boot and starts there when nothing
  has been adopted. A first launch can ask a question straight away; adding a
  project folder is the other thing it can do rather than the only one.
  Sessions with no project are grouped under **Unfiled**, which is open
  unless you close it.
- **The app is called Baaz.** The crate, the binary, the bundle
  (`sh.baaz.app`) and the `BAAZ_*` environment variables all carry the name.
  A state directory written under the old name is carried onto
  `~/Library/Application Support/baaz` at startup: the whole tree moves when
  nothing is there yet, and when something already is — an early write such
  as the tier probe can create it before the migration runs — only the
  entries it is missing are filled in, nothing is overwritten, and the old
  directory is left in place.
- **A mascot on three surfaces.** The boot hero, the new-session composer
  and critical error dialogs. The composer mascot is one of five variants,
  picked by a seed that is stable per session so it never changes on a
  repaint, and cycles on click; it appears with a short fade and settle,
  breathes while idle, and lifts on hover. Every one of those rests at zero
  and asks for no frames while the window is inactive or the system asks for
  reduced motion, so an idle window costs nothing. It stands down whenever a
  banner or the queue needs the same strip. Error dialogs pick their sprite
  by what failed — disconnected for connectivity, blocked for limit and
  permission refusals — and toasts stay text-only.

### Sessions & sidebar

- **Sessions.** Resume, rename, fork, archive; an unsent draft (text,
  images, files) is kept per project; a full-text search palette over
  transcripts and the files a turn created.
- **Titles follow the conversation.** A session with no index title is named
  after its own earliest user prompt (earliest submission, then earliest
  folded user turn), with the first shell command only as the fallback for
  sessions with no user text at all. Replays title their row the same way
  instead of keeping the file's name.
- **Generated session titles.** On the first send, one cheap model call in a
  throwaway side session writes a 3–6 word title into `sessions.json`
  (`generated_title`, ranked under a `/name` name); the side session is
  hidden at once and the server record untouched. A `Naming this session…`
  placeholder holds the row meanwhile; a 90 s watchdog, wire errors and
  empty replies fall back silently to the first-prompt label, at most one
  retry, exactly one generation per session ever. A reply that arrives after
  the timeout still lands on the row — the turn was already paid for —
  unless the session was named or closed meanwhile.
- **Three-line status rows, hover detail.** Every session row is title,
  status verb, context: the status reads `Working · 14m`, `Needs approval`,
  `Asked: "…"`, `Settled · 12m · 5 turns`, `Failed · 1h` or `No reply yet`,
  coloured by state; the context reads the pending approval command or
  question, else the ask/result byline (the user's last request beside the
  last reply, excerpted free and refreshed every completed turn), else a
  preview, else `project · branch`, else blank space at full height. Rows
  keep a uniform height and the byline halves hug their content around a
  single `·` separator. Hovering a row opens the full picture (title, ask,
  reply, status with detail, project, branch, turns, last change) in a card
  that takes no focus and never covers the row. `Needs approval` / `Asked`
  ride the wire's `Session.attention` plus the `session/statusChanged`
  broadcast (the open session's pending words come live from its fold);
  `Failed` rides the last turn's terminal error from `turn/completed`,
  persisted in `sessions.json`. Rows report hover enter/leave themselves
  (the library's `on_hover` / `on_hover_bounds`), so the card's delay arms
  even on a settled sidebar that re-renders nothing. New `--steps
  row-detail:<session_id>` verb pins the card for captures.
- **Title/summary side sessions stay invisible.** They start with a
  bare-UUIDv7 client id (muse 1.3.0 rejects any `session/start` id that is
  not its own shape) and are recognised by an explicit record, so they stay
  hidden, uncounted, unindexed and untitled, including across a restart
  mid-flight. Cost guards unchanged: one generation per session ever, at
  most one retry, the 90 s watchdog, silent first-prompt fallback — with a
  late answer still landing on the row instead of being billed for nothing.
- **A click never starts work.** Clicking a session only re-attaches
  (`session/resume` + `view/page`); a `turn/started` the re-attach
  re-delivers for an already-completed turn no longer marks the view or its
  row running, so an interrupted session keeps its terminal state instead
  of showing a fresh `Working`.

### Transcript

- **Streaming transcript.** Markdown, reasoning, tool calls grouped and
  folded into readable cards, per-turn token counts, a context meter with
  compaction, and a queue strip for steering a running turn. A queued
  message can be edited back into the composer, dropped, or sent straight
  into the running turn; a send that cannot be delivered — the turn ended,
  the server refused it, another window reclaimed the row — puts the words
  back in the composer rather than losing them.
- **Approvals, questions and errors.** Multi-stage approval cards with the
  server's own choices and policy/judge resolutions, question cards with
  previews and a timeout, error banners with retry, plans and todos.
- **Cross-block text selection.** Dragging from one paragraph into another —
  or into a code block — highlights everything between: one span per
  transcript, held keyed (not positional) so it survives scrolling mid-drag,
  ⌘C copies it in document order (blank line between blocks, list markers
  kept, code byte-exact), a plain click or a new drag clears it. New
  `--steps select-span:<turn>` verb holds a whole turn for captures;
  `select-text:<turn>:<from>-<to>` works as before.
- **The status row names the phase.** While a turn runs the row reads the
  most specific phase the transcript can source truthfully — waiting on an
  approval or an answer, the running tool's family, a growing reasoning
  trace — falling back to `Working…` (and `Finishing up…` on the memory
  tail) when nothing is specific. Still one calm line with the timer and
  the `esc` hint.
- **Turns show their age.** The fold keeps the wire's `recorded_at` on each
  turn (live and backfilled transcripts agree), and the transcript renders
  it beside the action row — under your message, in the reply's footer
  (`just now`, minutes, hours, `yesterday`, else the date). Turns the wire
  never timed look exactly as before.
- **Copy confirms, then clears.** Either role's Copy button holds the
  success check for 1.2 s and then returns to the copy glyph; a free
  `--steps copy:<turn>` verb presses it for captures.
- **Every existing file path opens.** Transcript paths open in their
  default app (folders in Finder) wherever they live — absolute paths as
  is, relative ones against the workspace — gated on existence: only an
  existing path opens (never executed, never created), anything else toasts
  `No such file` quietly. http(s) links behave as before.

### Composer

- **Composer controls.** Model, effort and mode menus; `@`-mentions and a
  `/`-command menu (including skills from `muse skills list`); prompt
  history; image attachments.

### Terminal

- **The terminal dock.** A real terminal under the composer (⌃` toggles it,
  and the centre header carries its button): per-project tabs over `$SHELL
  -l -i` with shell integration, outliving session switches and dying with
  the app. Open state and height persist in `layout.json`; the grid runs
  under the `BaazTerminal` key context, so every key reaches the pty except
  ⌃`, ⌘K, ⌘B, ⌘W, ⌘Q, ⌘N and ⌘C with a selection. A `terminal-dock:` steps
  verb replays a scripted tab for deterministic captures
  (`docs/images/terminal-dock-dark.png`,
  `docs/images/terminal-dock-light.png`).

### Projects

- **Projects.** A sidebar over several workspaces at once — add a folder,
  group sessions by project or by date, per-project pinning and colour,
  per-project model/effort/approval defaults.
- **Deleted folders leave the sidebar.** A project whose root is gone is
  shown nowhere — sidebar, Projects palette, project menu, rail, current —
  while its sessions fall back to Unfiled and the adoption stays
  in `projects.json`, so an unmounted volume or a re-attached worktree comes
  back by itself.
- **Project running state is a left-edge bar.** A group with a running
  session draws a small accent bar at the row's left edge in the state
  colour (breathing with the shared pulse while running) instead of a
  pulsing dot beside the name; the current-project bar shares the slot.

### Settings

- **Settings dialog.** Defaults and behaviour flags, including "Name
  sessions automatically" and "Summarise sessions in the sidebar"
  (`layout.json`, both default ON), with `--steps auto-title` /
  `auto-summary` verbs; off means no model call ever for that feature.
- **Both themes**, a command palette, and a keyboard-first keymap
  (`docs/08-keymap.md`).

### Accounts & billing

- **Account awareness.** Baaz probes which billing tier a login is
  on and warns before a turn would bill pay-as-you-go (`docs/06-billing.md`).
- **Status card wording.** An unavailable usage reading renders as
  `Current usage: unavailable`, not `unavailable used`.

### Performance

- **The sidebar paints from the local index.** On a machine with 337
  sessions the list used to appear about 1.06 s after the window did: the
  index was read and joined within 130 ms, but nothing was drawn until
  `session/list` returned, behind the ~870 ms the `muse serve` child takes to
  start. Provisional rows now come from the index at ~160 ms and are replaced
  wholesale when the real list lands. They show only what the index knows — a
  label, a description, an elapsed tag — and reserve the meta line's height
  without drawing it, so no turn count is invented and no row moves when the
  real one arrives.
- **Calm rendering.** The sidebar and transcript paint through cached panes
  that rebuild only when their inputs change; an idle window schedules no
  frames. Scrolling, resize drags and the one-shot reveal steer the lists
  without rebuilding the panes around them.

### Scripting & debugging

- **A scripting surface for captures and testing**: `--replay`,
  `--no-connect`, `--steps`, `--screenshot` — see `CONTRIBUTING.md`.
