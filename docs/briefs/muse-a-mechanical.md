# Brief — A: mechanical fixes, including the two HIGH bugs (Harness)

Repository `/Users/latekaapi/Projects/harness`, branch `main`, clean tree. Do NOT commit. Do
not touch `/Users/latekaapi/Projects/agentic-ui` or `~/Projects/cockpit`. Prefix every shell
command with `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`.
Spend rule: never `turn/start`, `--send`, `send:`/`steer:`, live tests, `harness-probe`,
`fixtures/msp/probe*.py`, `muse logout`, `account/logout`. Read `docs/audit/01-plan.md`,
then every finding named below in `docs/audit/client-adapter.md`, `app-core.md`, `support.md`.

## Scope: A-MECH-1 … A-MECH-23 (`docs/audit/00-findings.md` §A)

Order: the two HIGHs first, each with a test that fails before and passes after:

- **A-MECH-1** (`client-adapter-1`): `reindex` must remap every cached slot's `turn` through
  the rebuilt positions map and drop slots whose turn is gone. Test: a synthetic capture
  (add `fixtures/msp/synthetic-turn-removed.jsonl`, built like the other `synthetic-*`
  files) with three turns, a `turn/retracted`/`turn/unqueued` removing the middle one, then
  an item update to the last turn; assert the update lands on the right turn in the fold
  snapshot.
- **A-MECH-2** (`client-adapter-2`): `dispatch` parks `ServerRequest`s carrying a `sessionId`
  during a `view/gap` backfill and releases them with the same cursor-dedup pass. Test in
  `muse-client` with the in-memory pipe the existing tests use: gap open → server request →
  page → release order.

Then A-MECH-3 … A-MECH-23 as the findings describe. Two decisions taken for you:
A-MECH-10: retain the raw string in `Unknown` variants (a `String` payload), not a lossy
test. A-MECH-4: add the dispatch arms and, for `session/modelRouteUnserved`, fold it into
side state as a marker so it is visible.

Constraints: no behaviour change outside the named findings; all `docs/images` captures
and the reference set must stay byte-identical (`HARNESS_DETERMINISTIC=1` from P0; the
reviewer compares, you compare too and report the count); adapter snapshots may change only
for the new synthetic fixture.

## Gates

`cargo build --workspace`; `cargo test --workspace`;
`cargo clippy --workspace --all-targets -- -D warnings`;
`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`;
`UPDATE_SNAPSHOTS=1 cargo test -p muse-adapter` then read the diff (only the new fixture's
snapshot should appear). Report per finding id: done / skipped-with-reason, the two new
tests' names, the capture comparison count, gate output verbatim.
