# Brief — owner round 2, wire package (harness): muse 1.2.1, two errors, the tier probe

Repository `/Users/latekaapi/Projects/harness`, branch `owner-round-2-2026-09-13` (already
checked out, off `main`). Work ONLY there. Do NOT commit. Do not touch
`/Users/latekaapi/Projects/agentic-ui` or `~/Projects/cockpit`. Prefix every shell command
with `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`.
Spend rule (non-negotiable): never `turn/start`, `--send`, `send:`/`steer:` steps, the
ignored live tests, `muse logout`, `account/logout`. Free: `muse schema …`, `muse serve`
driven through `initialize`, `session/list`, `session/read`, `session/resume`, `view/page`,
`model/list`; `--replay`; `--no-connect`; `--print-tier`; the `muse` TUI opened with no
prompt; `cargo build/test/clippy/doc`. Read `CLAUDE.md`, `docs/05-handoff.md`,
`docs/01-transport.md`, `docs/06-billing.md`, then `docs/briefs/muse-schema-1.1.1.md` (the
precedent for S1, done once before for 1.0.3 → 1.1.1).

Do not edit `crates/harness/src/sidebar_view.rs`, `dialogs.rs`, `project_menu.rs` or
`transcript.rs`: another package follows on this branch and owns them.

## S1 — The wire is muse 1.2.1; the schema is 1.1.1

`muse --version` → `Muse Code 1.2.1 (1.2.1-R2847.1)`. The harness's `initialize` logs
`FingerprintMismatch` and `session/read for a title failed: MSP payload did not match its
schema: missing field \`kind\``. Do what the precedent brief did for 1.1.1:

1. Re-export `fixtures/msp/msp-ts/msp.d.ts` and `fixtures/msp/msp/{manifest.json,
   msp.schema.json}` with `muse schema generate-ts` / `generate-json-schema` (check
   `muse schema --help`). Diff against the old export and list every changed type in the
   report. Known from a first diff: new `PatchSummary`, `ReasoningEffortState`,
   `RequestReceipt`, `SessionNameChangedParams`, `SessionReasoningEffortChangedParams`,
   `SessionRenameParams/Result`, `SessionSetReasoningEffortParams/Result`,
   `MspServerRequest`, `ReasoningEffortChangeSource`, `SessionMcpServerConfig/Mode/
   StdioFraming`; `Session` gains additive-optional `branch`, `firstUserPrompt`, `name`,
   `title`; something in `session/read`'s result now requires a `kind`.
2. Update the hand-written mirror in `crates/muse-client/src/schema/*` so every type
   round-trips (`crates/muse-client/tests/schema_roundtrip.rs`), the fingerprint constant
   matches, and every fixture capture still opens and folds identically (`UPDATE_SNAPSHOTS=1
   cargo test -p muse-adapter`; read the diff — nothing should change for old captures).
3. Use the new fields where they remove a dependency: `SessionEntry::join` takes the row's
   own `name` / `title` / `firstUserPrompt` ahead of the index (the six-step cascade in
   `sidebar.rs` gains them as steps 2–4 before the index's copies; the index stays as the
   fallback for older rows), and `branch` is available for the row meta. Do NOT wire
   `session/rename` yet — note it in the report as available.
4. Record a fresh capture of the new shapes with a free session (`session/start` on
   `--provider echo`, `session/list`, `session/read`, `view/page`; no turn) into
   `fixtures/msp/transcript-1.2.1-shapes.jsonl` with the usual header comment, and add its
   snapshot.
5. `docs/01-transport.md` §4 wire facts updated for 1.2.1.

## S2 — `-32603 … stale sidecar generation` when opening a session

Seen on opening a listed session: dialog "Muse hit an internal error: muse error -32603:
internal error: read materialized session view: forward fold range read failed:
materialized projection head: stale sidecar generation; a leased load regenerates it at
first touch (#29473)". The message says it: a `view/page` (or `session/read`) reached a
session whose `.msp-view-v1` sidecar is stale, before a **leased** load (`session/resume`)
regenerated it. Diagnose with a capture (`MUSE_CAPTURE=<file> cargo run -p harness --
--session <id> --screenshot /tmp/x.png`, free) against a session another host touched
(any of today's `muse exec` sessions in this workspace): find the order the harness issues
`session/resume` and `view/page` in `lifecycle.rs` (`open`, `resume`) and
`session/events.rs` (the backfill page), and whether `derive_titles`'s `session/read` runs
on sessions no host has loaded. Fix so: the page is requested only after the resume result
(the lease) has arrived; a `-32603` whose message contains `stale sidecar` is retried once
after the resume settles, and only a second failure reaches the dialog; `derive_titles`
treats it as "no title this time" (log one line, no dialog). Unit-test the classification
(`muse-adapter/src/failure.rs` or wherever `-32603` is mapped) and the retry decision.

## S3 — `-32021 session already in use` must never take the wire down

Seen as a red top banner "Muse is not running: muse error -32021: session … is already in
use", after which no sidebar row opened anything. `Wire::Down` is set only on a failed
connect (`app.rs` connect) or reconnect (`app.rs` `reconnect`): the child exited, the
harness respawned it and resumed the active session, and **that resume** was rejected
because another host (a second harness window, or the child that exited but still held the
lease) owned the session. A session-scoped rejection is not a transport failure.

Fix: in `reconnect`, a successful `initialize` is `Wire::Ready` regardless of what the
resume then says; a resume rejection (`ErrorKind::SessionInUse` and the other session-scoped
kinds — read `conn.rs`'s `ErrorKind` and decide per kind, listing the decision in the
report) becomes a banner on that session's view ("This session is open in another window.
Close it there, or start a new session.") with the view read-only until a later resume
succeeds; the sidebar, palette and `⌘N` keep working. Log the child's exit reason (its
stderr tail and exit status) at `harness:` level so the next diagnosis has it. Reproduce
without spend: open the same session from two harness processes (`--session <id>` on both,
`--screenshot` on the second) and capture the second's screen to
`docs/images/round2-session-in-use-dark.png`.

## S4 — The tier probe and "Check again"

Files: `crates/harness/src/tier.rs`, `crates/harness/src/app.rs` (`tier_probing`), the
banner in `crates/harness/src/session/render.rs` (`render_status`'s "Check again" row —
you may edit that function only).

- Today's probe against 1.2.1 says `Subscription: Muse Code Power Usage / Current: — used /
  Weekly: — used`: the plan parses, the percentages no longer do. Open the TUI with no
  prompt as the probe does, read what the 1.2.1 card actually prints (paste the redacted
  card text into the report), and parse the usage it carries; if 1.2.1 no longer prints
  percentages, say so and make the footer show the plan without a meter rather than "—".
- The footer read "Plan unknown" for a while on the owner's screen while a second harness
  window was open. Make the probe robust to a concurrent instance: a lock file under the
  state dir with a bounded wait, and a fresh `tier.json` (< 1 h, same `authMtime`) is
  reused instead of re-probed.
- "Check again" gives no feedback. While probing, the button reads "Checking…" and is
  disabled; on completion a toast says the result ("Plan: Muse Code Power Usage" /
  "Still unknown: <reason>"); the banner leaves when the plan is known.

## S5 — Docs

CHANGELOG entry "2026-09-13 — Owner round 2, wire" with S1–S4 one line each;
`docs/06-billing.md` for the probe changes; `docs/05-handoff.md` "Known limitations" for
whatever 1.2.1 still does not give.

## Gates

`cargo build --workspace`; `cargo test --workspace`;
`cargo clippy --workspace --all-targets -- -D warnings` (last, after every edit);
`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`; one `gpui-pre` and one
`gpui-kit` in `cargo tree -d`; `UPDATE_SNAPSHOTS=1 cargo test -p muse-adapter` with the diff
read; `HARNESS_DETERMINISTIC=1` replay of `fixtures/msp/transcript-real.jsonl` byte-identical
before and after. Report per item: done / skipped-with-reason, test names, the schema diff
list, screenshot paths, gate output verbatim (last lines); never claim a gate you did not
run. When finished write the single word `done` to `/tmp/muse-round2-wire.done`.
