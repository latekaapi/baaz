# Brief — C1: structure, part 1 (Harness)

Repository `/Users/latekaapi/Projects/harness`, branch `main`, clean tree. Do NOT commit.
Do not touch `/Users/latekaapi/Projects/agentic-ui` or `~/Projects/cockpit`. Prefix every
shell command with `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`.
Spend rule: never `turn/start`, `--send`, `send:`/`steer:`, live tests, `muse logout`,
`account/logout`. Read `docs/audit/01-plan.md`, `docs/07-architecture.md`, then findings
`app-core-14`, `app-core-4`, `app-core-13`, `app-core-1` in `docs/audit/app-core.md`.

This is a **pure refactor**: no behaviour change, no visual change. Proof: the reference
captures (`HARNESS_DETERMINISTIC=1`, every `fixtures/msp/transcript-*.jsonl` and
`synthetic-*.jsonl` × dark|light at `--screenshot-delay 2500`, plus `--no-connect --login
choose|device|apikey|apikey-error|error` dark) are byte-identical before and after — take
them before you start into a scratch dir, compare with `cmp` at the end, report the count.
Adapter snapshots unchanged.

## 1. C-STR-6 — one `wire_call` helper (`app-core-14`)

About 46 sites in `app.rs` and `session.rs` follow the shape "clone the client, run a
blocking request on `cx.background_spawn`, then `this.update(cx, |this, cx| match result
{ … })`". Introduce one helper (a free function or a method on a small `Wire` type in a
new `crates/harness/src/wire.rs`) whose signature makes the call site say only what the
request is and what to do with the result, e.g.

```rust
wire_call(cx, client, move |client| client.account_read(), |this, result, cx| { … })
```

with the task pushed onto the entity's `tasks` the way the sites do today, and the
`update_in` variant for sites that need the window. Migrate **every** site that fits; list
the ones that do not (with why) in the report. Keep each site's error handling identical.

## 2. C-STR-5 — `steps.rs` (`app-core-4`, `app-core-13`)

Move the `--steps` and `--login-steps` verb parsing and dispatch out of `app.rs`/`session.rs`
into `crates/harness/src/steps.rs`: one table of verbs → handler, one parser, one place for
the "(scripting only)" / cost notes, and a unit test that enumerates every verb the table
knows and every verb the `--help` text documents and asserts the two sets are equal.
`shot.rs`'s `set_steps_running` calls stay where the loops are.

## 3. C-STR-1, first slice — `login.rs` (`app-core-1`)

Move the login-screen state, the intent handler, the `account/*` notification handling,
`probe_account`/`apply_account`, `submit_api_key`, `start_account`, `cancel`, `logout` and
`render_login` out of `app.rs` into `crates/harness/src/login.rs` as an `impl Harness`
block (or a `Login` struct with methods that take `&mut Harness`), leaving `app.rs` with
the fields and one call per seam. Rustdoc at the top of the new module explains the flow
in the words of `docs/diagnosis/login.md` §4 (cite D22–D28 by number).

Update `docs/07-architecture.md`'s file table for the three new modules.

## Gates

`cargo build --workspace`; `cargo test --workspace`;
`cargo clippy --workspace --all-targets -- -D warnings`;
`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`;
`UPDATE_SNAPSHOTS=1 cargo test -p muse-adapter` (no diff). Report: files, line counts of
`app.rs` and `session.rs` before and after, the number of sites migrated to `wire_call`
and the exceptions, the capture comparison count, gate output verbatim.
