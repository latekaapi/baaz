# Brief — integrate the seven `wf/*` branches into `wf-improvements` (single package)

The workflow orchestrator died before its integration step; you are the integrator.
Repository: `/Users/latekaapi/Projects/harness` (work in this checkout; branch `main` must
stay untouched). Branches to merge, all committed and individually gate-clean:
`wf/fold`, `wf/transcript`, `wf/composer`, `wf/search`, `wf/resize`, `wf/platform`,
`wf/shell`. Their tasks are Tasks B, C, D, E, F, G, A of
`docs/briefs/muse-wf-harness-improvements.md` — read that brief's hard rules and the
"Integration step" section; they apply verbatim (PATH prefix, spend rule, no library edits;
the library at `/Users/latekaapi/Projects/agentic-ui` stays on `improvements-2026-09-10`).

1. `git checkout -b wf-improvements main`. Merge with `--no-ff` in this order: `wf/fold`,
   `wf/transcript`, `wf/composer`, `wf/search`, `wf/resize`, `wf/platform`, `wf/shell`.
   Resolve every conflict by keeping every change (CHANGELOG keeps all entries, newest
   first; where two branches changed the same function in `app.rs`/`session.rs`, integrate
   both behaviours — read both sides, do not pick one).
2. Cross-wiring the children could not do alone:
   - the collapsed rail's "search" cell (Task A) opens the search palette (Task E's
     `PaletteKind::Search`), as do the header search icon and ⌘⇧F;
   - Task C's transcript renders Task B's `Block::ToolGroup` through the library
     `tool_group` with open state (if Task C already did this, verify it survives the merge);
   - Task E's file recorder and Task A's `last_summary` both hook turn completion — make
     sure both run;
   - the menu bar (Task G) View menu lists Toggle Sidebar, Command Palette and Search, and
     File → New Session matches the sidebar's New session row.
3. Gates on the merged tree, all must pass: `cargo build --workspace`,
   `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`,
   `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`, `cargo tree -d` (one
   `gpui-pre`, one `gpui-kit`). If `muse-adapter` snapshots differ, run
   `UPDATE_SNAPSHOTS=1 cargo test -p muse-adapter`, read the diff, quote it.
4. Screenshots of the merged tree, dark and light, `--screenshot-delay 15000`:
   `docs/images/improve-integrated-{dark,light}.png` (`--replay fixtures/msp/transcript-real.jsonl`),
   `docs/images/improve-integrated-markdown-dark.png` (`--replay fixtures/msp/synthetic-markdown.jsonl`),
   `docs/images/improve-integrated-toolgroup-dark.png` (`--replay fixtures/msp/synthetic-toolgroup.jsonl`),
   and `--steps search:harness` / `--steps overflow` / `--steps 'file:docs/CHANGELOG.md'`
   captures if those verbs exist on the merged tree.
5. Commit the merges and any integration fixes on `wf-improvements` (messages ending with
   `Co-Authored-By: Muse Code <noreply@meta.com>`). Remove the seven `../harness-wt-*`
   worktrees and `../harness-wt-before` (`git worktree remove --force`), keep the branches.
6. Report: merge order; every conflict (file + what you did); the cross-wiring outcomes;
   gate results verbatim; screenshot paths; a consolidated list of every numbered item in
   Tasks A–G that any child reported as not done, and anything you could not integrate.
   Never claim a gate you did not run.
