# Handoff — maintaining the Harness

The Muse Code chat slice is **complete**. All five phases of `docs/00-spec.md`
landed; there is no next phase to pick up. This document is for whoever changes
it next.

Paste the block below into a new session opened in
`/Users/latekaapi/Projects/harness`.

```
You are maintaining the Harness: a macOS gpui chat interface to Meta's Muse Code agent
(`muse` CLI 1.0.3; `muse serve` = "MSP", JSON-RPC 2.0 as NDJSON over stdio), built on the
`aui` library at /Users/latekaapi/Projects/agentic-ui (path dependencies; gpui-pre 0.3.3 +
gpui-kit 0.6). Library changes go on a new agentic-ui branch off `main` (everything
through `transcript-2026-09-12` is merged), as their own commits; the owner merges.
Prefix every shell command with
export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"

READ FIRST, IN THIS ORDER:
  docs/04-approvals.md §0   — what costs a turn, and what does not
  docs/06-billing.md        — the two credential tiers and the guard
  README.md, docs/07-architecture.md, docs/08-keymap.md
  docs/00-spec.md           — the frozen spec
  docs/CHANGELOG.md         — what each phase landed and what it spent
  then whichever of 01-transport / 02-app / 03-composer / 04-approvals your change touches.

THE SPEND RULE. There is no free provider. `--provider echo` picks a route, not a bill.
Free: `--replay <capture>`, `--no-connect`, `session/start`, `session/userShell`,
`approval/*`, `userInput/*`, `session/fork`, `session/list`, `view/page`, `model/list`, and
the `muse` TUI opened without a prompt. Anything reaching `turn/start` spends a turn, and
what it costs depends on the login's tier. Count turns from the logs, never from a report:
  grep -c runtime.user_intent.accepted ~/.local/share/muse/sessions/*/*/*/*/session.jsonl

GATES, before every commit. Harness: cargo build --workspace; cargo test --workspace;
cargo clippy --workspace --all-targets -- -D warnings; RUSTDOCFLAGS="-D warnings" cargo doc
--workspace --no-deps; `cargo tree -d` shows one gpui-pre and one gpui-kit; snapshots
regenerated with UPDATE_SNAPSHOTS=1 and the diff READ; captures compared with
HARNESS_DETERMINISTIC=1 (byte-identical run to run); performance claims from
`--bench`, never from the old `bench:` step alone. Library: the same plus the
all-features build (aui-webview/wry, aui-terminal/pty, aui-terminal/tui) and
python3 scripts/api-doc.py, and a gallery entry for anything new.

Commit messages end with "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>".
Do not touch ~/Projects/cockpit.
```

---

## Where things are

The 2026-09-12 review and performance pass is recorded in `docs/audit/` (findings, plan,
status) and its changelog entry; the regression method it established — byte-identical
captures under `HARNESS_DETERMINISTIC=1` and `--bench` before/after — is the bar for
every refactor since. The same day's five-fault pass (transcript design, scroll, open path,
lights/header, reopen) is in `docs/diagnosis/transcript-pass-2026-09-12.md` and the
changelog; `scripts/captures.sh <dir>` takes the 53-capture set (park the pointer outside
the top-left 1440×900 first), `--bench-scroll wheel` is the scroll instrument, and
`HARNESS_TRACE=1` prints a session switch's timeline. Library `main` carries the whole
branch stack as of `b0b40bd`; the harness builds against the `main` checkout.

`docs/07-architecture.md` has the map. The short version: `muse-client` is the
wire, `muse-adapter` is the fold and has no gpui dependency at all, `harness` is
the window. Two entities own state (`Harness`, `SessionView`) and one owns
everything that floats (`Overlays`).

## The five things to be careful of

1. **Every turn is billed, and the login's tier decides what it costs.** Phases
   1–4 spent about forty turns believing them to be subscription turns while the
   login was on pay-as-you-go. `docs/06-billing.md` and `docs/04-approvals.md`
   §0 exist so that cannot happen quietly again. When you need a screenshot,
   reach for `--replay` first, `--no-connect` second, and a live session that
   only runs `session/start` and `session/userShell` third.

2. **The card is never ahead of the server.** A press sends a command and then
   waits; the card changes when — and only when — a notification says it
   changed. An acknowledgement is admission, not an outcome. Nothing about an
   approval's choices is cached, because they belong to a stage and the stage
   moves.

3. **A session read twice must say what it said the first time.** A block is
   placed by its item's log sequence, never by arrival, so a live stream and a
   backfilled `view/page` agree. The gate is
   `muse-adapter/tests/fixtures.rs::a_live_fold_and_a_backfilled_fold_agree`,
   and it costs nothing. Any new fold rule has to hold in **both** orders —
   finding F11 needed two halves for exactly that reason.

4. **Muse's own storage is Muse's.** `~/.config/muse` and `~/.local/share/muse`
   are read-only to this app, and the session index is a cache whose every
   failure mode is an empty map. What the harness has to remember lives under
   `~/Library/Application Support/harness`, written atomically. Note that Muse's
   index writes the literal string `"New session"` as a *title*; it is a
   placeholder, and `IndexEntry::label` treats it as one (finding F10).

5. **Secrets never reach a log.** The device-code URL and user code go from the
   login child's stderr straight to the screen. The billing probe never logs the
   terminal's bytes, because the `/upgrade` card carries a URL.

## Known limitations on this machine

- **The managed shell sandbox is unavailable.** A `userShell` under
  `promptUnmatched` never completes and no approval is minted, so the two
  stage-1 approval screenshots come from a truncated real capture
  (`fixtures/msp/transcript-approve-stage1.jsonl`) rather than from a live run.
  Under `denyUnmatched` the policy refuses before the sandbox is consulted,
  which is why that path still works live.
- **`session/setApprovalMode` does not reach `promptUnmatched`** on this server;
  `session/start`'s `approvalMode` does, which is what `--approval-mode` is for.
- **`session/read` on a session no host has loaded can serve no history**, so
  the F10 title derivation falls back to the fold of an open session.

## Deliberately not built

The right pane (diffs, terminal, browser) — the shell keeps its slot and
`ToggleRightPane` stays wired to nothing, so the keymap has no hole. Subagent
drill-in, workflow control, voice, worktrees, enterprise config, and any
provider other than Muse. Spec §1 lists them as out of scope and none of them
became less so.

## If you add a wire capture

Put it in `fixtures/msp/`. It must open without panicking
(`every_capture_opens_without_panicking`), fold to a checked-in snapshot, need
no `Block::Generic`, and — unless it is deliberately cut mid-item — fold the
same way live and backfilled. Regenerate snapshots with `UPDATE_SNAPSHOTS=1` and
**read the diff**: a changed snapshot is a changed transcript.

## Pointers

- Research (wire contract, live captures, auth, TUI, reference-app inventories,
  aui gaps): `/Users/latekaapi/Projects/agentic-ui/docs/10-muse-research.md`
- Exact schema for this muse build: `fixtures/msp/msp-ts/msp.d.ts`,
  `fixtures/msp/msp/`
- Wire captures (ground truth): `fixtures/msp/transcript-*.jsonl`. The
  `probe*.py`/`run*.py`/`harness-probe` scripts and binary that used to
  re-probe the wire (several sent turns) were removed 2026-09-12; git history
  has them. `fixtures/msp/drive.py` and `make-stress-300.py` remain, each with
  a header comment on what it sends and costs.
- Persistent memory for this project lives at
  `~/.claude/projects/-Users-latekaapi-Projects-agentic-ui/memory/`.
