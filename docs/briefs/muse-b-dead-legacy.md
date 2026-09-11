# Brief — B: dead and legacy code removal (Harness)

Repository `/Users/latekaapi/Projects/harness`, branch `main`, clean tree. Do NOT commit. Do
not touch `/Users/latekaapi/Projects/agentic-ui` (B-DEAD-4 is a library item and is handled
in package E) or `~/Projects/cockpit`. Prefix every shell command with
`export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`. Spend rule as in
`docs/briefs/muse-a-mechanical.md`. Read `docs/audit/01-plan.md` and the findings named
below in `docs/audit/support.md`, `client-adapter.md`, `app-core.md`.

## Scope: B-DEAD-1 … B-DEAD-9 with these decisions

- B-DEAD-1, B-DEAD-2, B-DEAD-3: delete as the findings say (for B-DEAD-3 delete
  `right_open`, the no-op action and the "in this phase" comments; the right pane is not
  upcoming).
- B-DEAD-5: **keep** `reconnect_after_login` and its `allow(dead_code)`; change its doc
  comment to name the condition for deletion (one billed turn after a Meta-account login
  confirms D25). Nothing else.
- B-DEAD-6 + B-DEAD-8: **delete** `crates/muse-adapter/src/bin/harness-probe.rs` and
  `fixtures/msp/{probe.py,probe_approve.py,probe_echo.py,probe_phase3.py,probe_real.py,
  probe_wire.py,run2.py,run3.py,run4.py,run_slash.py}` and the `__pycache__`; keep
  `fixtures/msp/drive.py` and `make-stress-300.py` with a header comment stating what
  each sends and costs. Update every reference: `CLAUDE.md` spend rule line, `README.md`,
  `docs/05-handoff.md`, `docs/09-handoff-improvements.md`, `docs/01-transport.md` §6, the
  briefs under `docs/briefs/` are historical — leave them. Say "removed 2026-09-12; git
  history has them" where a doc listed them.
- B-DEAD-7: one place (the `--help` text in `main.rs` and `docs/02-app.md`'s steps table)
  marks scripting-only verbs and the ones that cost a turn.
- B-DEAD-9: no action.
- Also: `cargo machete` flags `gpui_platform` unused in `crates/harness/Cargo.toml` —
  remove it if the build agrees; the three caller-less `muse-client` methods
  (`turn_cancel`, `view_subscribe`, `view_unsubscribe`) stay — they are wire surface with
  tests, not dead code; say so in a one-line doc comment on each.

Constraints: reference captures byte-identical (compare and report); adapter snapshots
unchanged; `cargo tree -d` still one `gpui-pre`, one `gpui-kit`.

## Gates

`cargo build --workspace`; `cargo test --workspace`;
`cargo clippy --workspace --all-targets -- -D warnings`;
`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`; `cargo machete`. Report per
finding id, files deleted, docs touched, capture comparison count, gate output verbatim.
