# Brief — D1: performance, adapter and wire (Harness)

Repository `/Users/latekaapi/Projects/harness`, branch `main`, clean tree. Do NOT commit.
Do not touch `/Users/latekaapi/Projects/agentic-ui` or `~/Projects/cockpit`. Prefix every
shell command with `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`.
Spend rule: never `turn/start`, `--send`, `send:`/`steer:`, live tests, `muse logout`,
`account/logout`. Read `docs/audit/01-plan.md`, `docs/02-app.md` "Measuring", then the
findings named below in `docs/audit/client-adapter.md`, `performance.md`, `support.md`.

Measure first: `./target/debug/harness --bench fixtures/msp/synthetic-stress-300.jsonl
--bench-out /tmp/d1-before.json` and the same for `transcript-echo.jsonl`; after each item
re-run and keep the table. A change that does not move `bench-apply` or `bench-rss` on the
stress capture is still fine if it removes an O(transcript) path — say which.

## Scope, in this order

- **D-PERF-6** (`client-adapter-8`): per-turn slot index so `shift_slots`, `relocate_call`
  and `reindex` touch only the affected turn; deserialize from `&Value` instead of cloning
  params per event; drop the per-chunk `String` + `delta.clone()` in `item_delta`.
- **D-PERF-2** (`performance-3`, `performance-4`): the fold hands out `Rc`-shared turns (or
  the harness splices deltas into its cached `Rc<Vec<Turn>>`) so a streaming chunk no
  longer deep-clones every turn; `render_transcript`'s per-frame clones of `toggled`,
  `titles`, `cached_full_output`, `text_selections` become `Rc` snapshots refreshed in
  `refresh_render_cache`.
- **D-PERF-5** (`client-adapter-6`) + **D-PERF-12** (`performance-16`): prune per-session
  maps when a turn is removed or completes; store queued text once; evict a session's
  folded state when its view closes, keeping the active session plus a bounded MRU
  (name the bound as a constant; document it).
- **D-PERF-7** (`client-adapter-7`): bound the gap-fill page loop and buffer, track the
  thread, emit a visible event when backfill aborts; the `view/gap` marker row reflects it.
- **C-STR-10** (`client-adapter-4`): `client/protocolError` becomes a marker/banner delta
  handled in `SessionView::apply`.
- **D-PERF-13** (`support-4`): `INSERT OR IGNORE` + a uniqueness index for `record_files`.

Constraints: reference captures byte-identical except where a new marker row is the point
(`client/protocolError`, gap abort) — those need a new `synthetic-*.jsonl` fixture and a
snapshot, and the report names them; all other adapter snapshots unchanged.

## Gates

`cargo build --workspace`; `cargo test --workspace`;
`cargo clippy --workspace --all-targets -- -D warnings`;
`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`;
`UPDATE_SNAPSHOTS=1 cargo test -p muse-adapter` then read the diff. Report per finding id,
the bench table before/after, new fixtures and snapshots, capture comparison count, gate
output verbatim.
