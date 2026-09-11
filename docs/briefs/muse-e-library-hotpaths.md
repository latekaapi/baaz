# Brief — E: library hot paths (agentic-ui package)

Repository: `/Users/latekaapi/Projects/agentic-ui`. First `git switch -c audit-2026-09-12`
from the checked-out `login-methods`; work and commit on `audit-2026-09-12`, one commit per
coherent group, messages ending `Co-Authored-By: Muse Code <noreply@meta.com>`. Do not touch
`/Users/latekaapi/Projects/harness` (read-only there: `docs/audit/library-hotpaths.md`,
`docs/audit/00-findings.md` §E, `docs/audit/01-plan.md`) or `~/Projects/cockpit`. Prefix every
shell command with `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`.
Library rules: `docs/00-agent-brief.md`, `docs/04-design-rules.md` — no literal colours/sizes/
durations, stateless `RenderOnce` components, both themes, gallery entry for anything new,
`python3 scripts/api-doc.py` after API changes.

## Scope: findings E-LIB-1 … E-LIB-13 (read them in `library-hotpaths.md`)

In this order, measuring where the finding says "if profiling justifies":

1. E-LIB-1, E-LIB-13, E-LIB-4 — one parse cache: `prose()` and `markdown_selected_text`
   go through `parsed_markdown`; the memo keys on a cheap identity (hash stored beside
   the source, or `Arc<str>` pointer + len), evicts per entry (LRU or clock), never
   `clear()`s everything, and a streaming turn hits it (key the closed prefix separately
   from the open tail if that is what makes streaming hit).
2. E-LIB-2, E-LIB-3 — code-block tokenisation and ANSI parsing memoised per line with a
   bound; document the bound as a named constant.
3. E-LIB-5, E-LIB-6, E-LIB-7, E-LIB-9 — per-frame rebuilds in `span_runs`, the streaming
   caret's trailing width, `format!`-built element ids, and `SelectableText` cloning. For
   each, add a `#[cfg(test)]` or gallery-driven micro-measurement **before** changing it
   (a loop of 1 000 renders of the stress-shaped input, `Instant` timed) and keep the
   before/after numbers for the report; skip an item whose measurement shows it under
   5 % of the row cost and say so.
4. E-LIB-8, E-LIB-11 — API changes: `tool_group`, `question_card`, `plan_card` take
   borrows / `&[T]` with `SharedString` payloads, turn selection takes
   `Option<&TextSelection>`. These break the harness's call sites on purpose: list every
   changed signature in the report with the one-line migration for each (the harness
   package C2 applies them).
5. E-LIB-10, E-LIB-12 — the browser body pulse gated on activity; small per-frame
   `format!`s hoisted to their owners.
6. E-LIB-14 — a gallery-side idle check: with the gallery's screenshot path, after a card
   settles, count frames for 2 s and assert zero for the transcript, sidebar, composer and
   login cards; if any keeps requesting frames, fix the clock gating (finding performance-13
   names `caret_visible`, activity/status shimmer, question/code tweens) so a settled card is
   silent. Report the per-card count before and after.

Do not change visuals: the gallery captures for transcript, sidebar, composer and login
cards must be pixel-identical before and after (take them before you start, compare with
`cmp`, and report the result; if the gallery capture is itself non-deterministic, say which
card and why).

## Gates, all must pass

`cargo build --workspace`;
`cargo build --workspace --features aui-webview/wry,aui-terminal/pty,aui-terminal/tui`;
`cargo test --workspace`; `cargo clippy --workspace --all-targets -- -D warnings`;
`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`; `python3 scripts/api-doc.py`.
Report: commits, files, the measurement tables, the signature changes with migrations,
the idle-frame counts, gate output verbatim. Never claim a gate you did not run.
