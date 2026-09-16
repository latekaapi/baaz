# Changelog

All notable user-visible changes to this project are documented here.

## 0.1.0 — unreleased

First public release. A native macOS chat client for
[Muse Code](https://github.com/facebookresearch/muse) (`muse serve`, MSP over
stdio), built with [gpui](https://www.gpui.rs) and the `aui` component
library.

- **Chat over Muse Code.** Drives `muse serve` as a child process and speaks
  MSP (JSON-RPC 2.0 as NDJSON over stdio); signs in the way the CLI does
  (device code or an API key).
- **Streaming transcript.** Markdown, reasoning, tool calls grouped and
  folded into readable cards, per-turn token counts, a context meter with
  compaction, and a queue strip for steering a running turn.
- **Approvals, questions and errors.** Multi-stage approval cards with the
  server's own choices and policy/judge resolutions, question cards with
  previews and a timeout, error banners with retry, plans and todos.
- **Projects.** A sidebar over several workspaces at once — add a folder,
  group sessions by project or by date, per-project pinning and colour,
  per-project model/effort/approval defaults.
- **Sessions.** Resume, rename, fork, archive; an unsent draft (text, images,
  files) is kept per project; a full-text search palette over transcripts
  and the files a turn created.
- **Composer controls.** Model, effort and mode menus; `@`-mentions and a
  `/`-command menu (including skills from `muse skills list`); prompt
  history; image attachments.
- **Settings and account awareness.** A settings dialog for defaults and
  behaviour flags; the harness probes which billing tier a login is on and
  warns before a turn would bill pay-as-you-go (`docs/06-billing.md`).
- **Both themes**, a command palette, and a keyboard-first keymap
  (`docs/08-keymap.md`).
- **A scripting surface for captures and testing**: `--replay`, `--no-connect`,
  `--steps`, `--screenshot` — see `CONTRIBUTING.md`.
- **Titles say what the person said.** A session with no index title used to
  be named after the first shell command its agent ran; the derived title is
  now the transcript's earliest user prompt (earliest submission, then
  earliest folded user turn), with the shell command only as the fallback
  for sessions with no user text at all, and both cut the row's own way.
  Replays title their row the same way instead of keeping the file's name.
- **Deleted folders leave the sidebar.** A project whose root is gone is
  shown nowhere — sidebar, Projects palette, project menu, rail, current —
  while its sessions fall back to "Other workspaces" and the adoption stays
  in `projects.json`, so an unmounted volume or a re-attached worktree comes
  back by itself.
- **Generated session titles.** On the first send, one cheap model call
  (`muse-spark-1.3` when listed) in a throwaway side session writes a 3–6
  word title into `sessions.json` (`generated_title`, ranked under a
  `/name` name); the side session is hidden at once and the server record
  untouched. A `Naming this session…` placeholder holds the row meanwhile;
  a 90 s timeout (set from measured turn times: the title call itself took
  ~21 s against the old 20 s ceiling), wire errors and empty replies fall
  back silently to the first-prompt label, at most one retry, exactly one
  generation per session ever. A reply that arrives after the timeout still
  lands on the row — the turn was already paid for — unless the session was
  named or closed meanwhile.
- **Two-line bylines, uniform rows.** Every session row always shows two
  lines: `Working…` while a turn runs, the pending placeholder while a
  title is in flight, the owner's last request beside the last reply once
  known (free excerpt, refreshed every completed turn; one debounced model
  rewrite only when it is poor), else the preview/`N turns` meta, else `No
  reply yet` — never blank.
- **Sidebar switches for both.** "Name sessions automatically" and
  "Summarise sessions in the sidebar" in the Settings dialog (`layout.json`,
  both default ON), with `--steps auto-title` / `auto-summary` verbs; off
  means no model call ever for that feature.
- **Status card wording.** An unavailable usage reading renders as
  `Current usage: unavailable`, not `unavailable used`.
- **Cross-block text selection.** Dragging from one paragraph into another —
  or into a code block — highlights everything between: one span per
  transcript, held keyed (not positional) so it survives scrolling mid-drag,
  ⌘C copies it in document order (blank line between blocks, list markers
  kept, code byte-exact), a plain click or a new drag clears it. New
  `--steps select-span:<turn>` verb holds a whole turn for captures;
  `select-text:<turn>:<from>-<to>` works as before.
- **Side sessions by record, not by id.** Title/summary side sessions start
  with a bare-UUIDv7 client id — muse 1.3.0 rejects any `session/start` id
  that is not its own shape (`invalid length: found 50` for the old
  namespaced id) — and are recognised by an explicit record (remembered in
  memory and as `side_session` in `sessions.json` before the start runs), so
  they stay hidden, uncounted, unindexed and untitled, including across a
  restart mid-flight. Cost guards unchanged: one generation per session
  ever, at most one retry, the 90 s watchdog, silent first-prompt fallback —
  with the watchdog standing the row down rather than giving up, so a late
  answer still lands instead of being billed for nothing.
