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
