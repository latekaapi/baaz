# Brief — second fix-up on `wf-improvements` (single package)

Repository: `/Users/latekaapi/Projects/harness`, branch `wf-improvements` at `daa5773`
(work directly on it; commit on it when the gates pass, message ending
`Co-Authored-By: Muse Code <noreply@meta.com>`; never touch `main`). Library:
`/Users/latekaapi/Projects/agentic-ui` on `improvements-2026-09-10`; do not modify it. The
hard rules of `docs/briefs/muse-wf-harness-improvements.md` apply verbatim (PATH prefix,
spend rule, tokens not literals, gates, honest report). Read the two 2026-09-10 "Fix-up"
CHANGELOG entries and `docs/briefs/muse-fixup-improvements.md` first.

The owner re-read the retaken screenshots. Three faults remain; fix all, retake the named
proofs (dark + light, `--screenshot-delay 15000`, same file names):

1. **Sidebar description still duplicates the title** (`improve-shell-open-dark.png`,
   `improve-integrated-dark.png`): the row shows "Run the shell command…" as the label and
   "Run the shell command `ls` in the" as the second line. `describe()`'s "label is not
   derived from the prompt" test is wrong for this case — the label came from the derived
   title, which itself came from the first prompt. Rule, precisely: second line =
   `last_summary` if set; else the first prompt only when the label is a user-given name or
   a Muse-provided title that is not a prefix/elision of that prompt (compare normalised
   text: lowercase, whitespace-collapsed, first 40 chars); else no second line. Unit test
   the predicate with the three cases.
2. **Rename fields clip vertically** (`improve-shell-rename-dark.png`): in both the sidebar
   row and the header, the field's text is cut at the top — the 22 px wrapper crops the
   glyphs. Centre the field vertically (align the wrapper with `items_center`, let the
   editor's own line-height define the height, `overflow_hidden` only horizontally), keep
   single-line. Both fields must show the full glyph height with the caret; nothing else
   moves.
3. **Search palette renders half-transparent** (`improve-integrated-search-dark.png`): the
   whole palette (input and panel) sits at roughly 50 % opacity over the transcript, and
   the input floats above the panel with a gap. Before the first fix-up the same palette
   rendered opaque. Find what changed (a presence/tween whose animation never reaches rest
   in a static frame, a wrapper opacity, an `occlude` layer ordering) and restore an
   opaque palette; the panel and its input must form one surface like the Resume palette
   (`--steps resume` screenshot for comparison, take it). Retake
   `improve-integrated-search-{dark,light}.png`.
4. **Pin action**: another package is adding an opt-out to the library
   (`AssistantTurn::actions(..)` or `.without_pin()`, see `../agentic-ui/docs/06-api.md`
   after `git -C ../agentic-ui log -1`). At the END of your work check whether it has
   landed; if yes, hide Pin on assistant turns and retake `improve-integrated-markdown-dark.png`;
   if not, say so in the report and leave the toast.

Gates, all must pass: `cargo build --workspace`, `cargo test --workspace`,
`cargo clippy --workspace --all-targets -- -D warnings`,
`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`, `cargo tree -d` (one
`gpui-pre`, one `gpui-kit`). CHANGELOG "Fix-up 2" entry dated 2026-09-10. Report: what
changed per item, gate results verbatim, screenshot paths. Never claim a gate you did not run.
