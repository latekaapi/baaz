# Brief — finish Task C — transcript rendering of the Harness improvements workflow in its existing worktree

The workflow orchestrator died (provider idle timeout) after this task's child had done
most of its work but before it committed. You are the replacement. Work ONLY in the
existing worktree `/Users/latekaapi/Projects/harness-wt-transcript` (branch `wf/transcript`, uncommitted
changes present — **inventory them first** with `git status` and `git diff --stat`, read
the diff, and keep what is good). The task specification is section "Task C — transcript rendering: smooth scroll, no flicker, links, actions, groups" of
`docs/briefs/muse-wf-harness-improvements.md` in that worktree; every hard rule in that
brief applies verbatim (PATH prefix, spend rule, no library edits, worktree is a sibling,
gates, CHANGELOG entry, screenshots, honest summary). The library at
`/Users/latekaapi/Projects/agentic-ui` is on `improvements-2026-09-10`; do not touch it.

1. Inventory: list what the previous child already implemented against the task's numbered
   items, what is missing, and whether the tree builds (`cargo build --workspace`).
2. Finish every numbered item of the task. Do not widen scope.
3. Gates, all must pass: `cargo build --workspace`, `cargo test --workspace`,
   `cargo clippy --workspace --all-targets -- -D warnings`,
   `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`, `cargo tree -d` shows one
   `gpui-pre` and one `gpui-kit`; `muse-adapter` snapshots regenerated only if a fold
   change requires it (quote the diff).
4. Screenshots as the task asks, into `docs/images/`; CHANGELOG entry dated 2026-09-10.
5. **Commit on `wf/transcript`** in the worktree (message ending with
   `Co-Authored-By: Muse Code <noreply@meta.com>`). Never touch `main`.

Report: inventory findings, what you finished, gate results verbatim, screenshot paths,
anything not done and why. Never claim a gate you did not run.
