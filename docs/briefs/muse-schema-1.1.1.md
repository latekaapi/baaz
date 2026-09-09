# Brief — re-export the MSP schema from muse 1.1.1 and absorb the diff (Harness)

You are the single implementor for this work package in `/Users/latekaapi/Projects/harness`
(branch `main`). You own it end to end. Do NOT commit; leave the tree uncommitted for review.
Do not touch `/Users/latekaapi/Projects/agentic-ui` or `~/Projects/cockpit`. Prefix every
shell command with `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`.

## Spend rule — non-negotiable

Every model turn is billed. Never run the ignored live tests (`crates/muse-client/tests/live.rs`,
`live_backfill_parity`), `harness-probe`, the `fixtures/msp/probe*.py` scripts, `--send`, or
`--steps` containing `send:`/`steer:`. Free and allowed: `muse schema ...`, `muse serve` driven
only through `initialize`/`session/list`/`model/list` (no `turn/start`), `--replay`,
`--no-connect`, `cargo build/test/clippy/doc`.

## The situation

The `muse` CLI self-updated from 1.0.3 to 1.1.1 (`muse --version` → `Muse Code 1.1.1
(1.1.1-R2514.1)`). Everything in this repo was built against 1.0.3:

- `fixtures/msp/msp-ts/msp.d.ts` and `fixtures/msp/msp/{manifest.json,msp.schema.json}` were
  exported with `muse schema generate-ts --out fixtures/msp/msp-ts` and
  `muse schema generate-json-schema --out fixtures/msp/msp` (check `muse schema --help` for
  the exact 1.1.1 syntax).
- `crates/muse-client/src/schema.rs` is a hand-written Rust mirror of `msp.d.ts` (all 186
  types); `crates/muse-client/tests/schema_roundtrip.rs` round-trips it against the JSON
  schema and the fixture captures; `SCHEMA_FINGERPRINT` in `crates/muse-client/src/client.rs`
  is compared with the server's `initialize` result and a mismatch is a warning.
- `docs/01-transport.md` §4 lists the wire facts; `docs/10-muse-research.md` in agentic-ui
  (read-only for you) describes 1.0.3.
- One observed 1.1.1 behaviour change already: `muse serve` 1.1.1 prunes turn-less sessions
  shortly after they are created (found 2026-09-09; see the newest `docs/CHANGELOG.md` entry).

## What to do

1. **Re-export in place.** Regenerate the TypeScript and JSON-schema exports into the same
   paths so `git diff` shows exactly what changed. Do not keep 1.0.3 copies; git has them.
2. **Read the diff and write it up.** Produce `docs/10-msp-1.1.1-diff.md`: every added,
   removed or changed method, notification, type and field, grouped, with one line each on
   what it means for this client. Include the old and new schema fingerprints (the new one
   from `manifest.json` or from a free `initialize` against `muse serve`; `fixtures/msp/`
   captures show what `initialize` looks like on the wire).
3. **Absorb the diff in `schema.rs`** so the mirror matches 1.1.1: changed fields, renamed or
   new enum variants, new required-nullable fields. Keep the file's conventions (comments say
   which SS section a type comes from; optional-vs-nullable is modelled deliberately, read the
   header comment). New methods or notifications: add them to the `MSP_METHODS` /
   `MSP_NOTIFICATIONS` indexes and type them if their params/results are small; if a new
   surface is large (more than ~5 new types), list it in the diff doc under "not yet typed"
   instead of typing it. Never remove a type that a fixture capture still uses; captures are
   real 1.0.3 wire recordings and must keep replaying.
4. **Update `SCHEMA_FINGERPRINT`** to the 1.1.1 value and adjust the mismatch warning text if
   it names 1.0.3.
5. **Client behaviour.** If the diff changes anything the client relies on (framing, ids,
   `session/list` fields such as `turnCount`, approval or userInput shapes, error `data.kind`
   values), fix `crates/muse-client` and `crates/muse-adapter` accordingly and say so. Do not
   speculate about behaviour you cannot see in the schema; the session-pruning change is
   behavioural, not schematic, so just cross-reference it.
6. **Docs.** Add a `## 2026-09-09 — muse 1.1.1 schema` entry at the top of
   `docs/CHANGELOG.md` (above the sidebar-noise entry) summarising the diff and pointing at
   the new doc. Update the version strings in `README.md`, `CLAUDE.md` (line 1 area),
   `docs/01-transport.md` and the header comment of `schema.rs` from 1.0.3 to 1.1.1 where
   they describe the current binary; leave historical statements ("captures were recorded
   with 1.0.3") alone.
7. **Tests.** `cargo test --workspace` must pass, including `schema_roundtrip` and every
   replay/parity snapshot. If a muse-adapter snapshot changes because of a type change, run
   `UPDATE_SNAPSHOTS=1 cargo test -p muse-adapter`, read the snapshot diff, and quote it in
   the report; do not regenerate for any other reason.

## Gates (all must pass before you stop)

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
```

## Report

Files changed; the fingerprint pair; a count of added/removed/changed types, methods and
notifications; anything left "not yet typed"; any client behaviour change you made; gate
results verbatim (pass/fail per gate); anything you could not do and why. Do not claim a
gate passed that you did not run.
