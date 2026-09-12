# Brief — C2: structure, part 2 (Harness + one paired library change)

Repository `/Users/latekaapi/Projects/harness`, branch `main`, clean tree. Do NOT commit in
the harness. The one library change below is committed in
`/Users/latekaapi/Projects/agentic-ui` on its checked-out branch `audit-2026-09-12`. Do not
touch `~/Projects/cockpit`. Prefix every shell command with
`export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`. Spend rule: never
`turn/start`, `--send`, `send:`/`steer:`, live tests, `muse logout`, `account/logout`.
Read `docs/audit/01-plan.md`, `docs/07-architecture.md` (now lists `wire.rs`, `steps.rs`,
`login.rs`), then the findings named below in `docs/audit/app-core.md`,
`docs/audit/support.md`, `docs/audit/client-adapter.md`, `docs/audit/library-hotpaths.md`.

Pure refactor except where a finding says otherwise: reference captures byte-identical
(count reported), adapter snapshots unchanged, bench on `synthetic-stress-300.jsonl` not
regressed (before/after lines in the report).

## In this order

1. **C-STR-1, the rest** (`app-core-2`, `-3`, `-5`, `-6`): extract from `app.rs` into
   their own modules, each an `impl Harness` block plus its own types: the sidebar view
   (`sidebar_view.rs`: footer, account menu, session groups, search field, rename),
   the dialogs and toasts (into `overlays.rs` or `dialogs.rs`), the billing tier glue
   (`tier` probe calls, banner, `push_tier` → `billing.rs`), and the resize drag state
   (`ResizeDrag` type + handlers → `resize.rs`). `app.rs` keeps fields, boot, connect,
   route, render, and one call per seam.
2. **C-STR-3** (`app-core-9`, `-10`): title sync, resize settle and the session-swap kick
   move out of `render` into one `on_frame` pre-pass; `render` becomes pure.
3. **C-STR-4** (`app-core-11`, `-12`): one `refresh_render_cache` sync point in
   `session.rs`; `transcript::block` becomes a dispatch over per-card functions
   (`*_card`), each under ~60 lines.
4. **C-STR-2** (`app-core-7`, `-8`): split `SessionView` and `Harness` along their
   existing `// ----` section seams into `impl` blocks in sibling files
   (`session/{composer,commands,questions,approvals,render}.rs` or whatever the seams
   say); no method changes, only homes. Report the per-file line counts after.
5. **C-STR-7** (`client-adapter-16`): split `muse-client/src/schema.rs` by surface
   (`schema/{session,turn,view,approval,user_input,account,model,common}.rs` with one
   `mod.rs` re-exporting everything so no import elsewhere changes); the dispatch
   checklist in `schema_roundtrip.rs` generated from one inventory.
6. **C-STR-8** (`support-7`): `set_overrides` batches one rejoin/write/reindex for a
   batch hide. **C-STR-9** (`support-16`): `PENDING_APPROVAL` / `STEPS_RUNNING` move off
   process globals onto a capture token owned by `Harness` (shot.rs takes it).
7. **Paired library change** (E-LIB-8, E-LIB-11, E-LIB-12 from `library-hotpaths.md`):
   in agentic-ui, `tool_group`, `question_card`, `plan_card` take borrows / `&[T]` with
   `SharedString` payloads, turn selection takes `Option<&TextSelection>`, and the
   per-frame `footer_items` + `format_duration` strings become caller-owned pre-formatted
   `SharedString`s. Commit there (library gates: build, all-features build, test, clippy
   `-D warnings`, rustdoc `-D warnings`, `python3 scripts/api-doc.py`; gallery captures
   byte-identical), then migrate the harness call sites in `transcript.rs`/`session.rs`.
   Report the bench before/after for this item on its own.

Update `docs/07-architecture.md` (file table, the "two entities" section) and
`docs/02-app.md`'s file table.

## Gates (harness)

`cargo build --workspace`; `cargo test --workspace`;
`cargo clippy --workspace --all-targets -- -D warnings`;
`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`;
`UPDATE_SNAPSHOTS=1 cargo test -p muse-adapter` (no diff); `cargo tree -d` one gpui-pre, one
gpui-kit. Report: per item done/skipped, the new file list with line counts, the library
commit hash, capture comparison count, bench lines, gate output verbatim.
