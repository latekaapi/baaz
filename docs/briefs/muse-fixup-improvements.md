# Brief — fix-up pass on `wf-improvements` after the owner-side audit (single package)

Repository: `/Users/latekaapi/Projects/harness`, branch `wf-improvements` (work on it
directly, no worktree; **commit on it** when the gates pass, message ending with
`Co-Authored-By: Muse Code <noreply@meta.com>`; never touch `main`). The library at
`/Users/latekaapi/Projects/agentic-ui` is on `improvements-2026-09-10` at `da14800` — do not
modify it. Hard rules of `docs/briefs/muse-wf-harness-improvements.md` apply verbatim
(PATH prefix, spend rule: `--replay`/`--no-connect` only, tokens not literals, gates,
honest report). Read `docs/CHANGELOG.md`'s 2026-09-10 entries first to see what landed.

Screenshots that show each fault are in `docs/images/` (`improve-integrated-dark.png`,
`improve-integrated-markdown-dark.png`, `improve-shell-rename-dark.png`,
`improve-integrated-search-dark.png`). Fix all of the following; each item names the
proof screenshot to retake (dark + light, `--screenshot-delay 15000`, overwrite the same
file names).

1. **Transcript gutter and alignment** (`crates/harness/src/session.rs` `render_transcript`,
   `transcript.rs`). Since the `list()` virtualisation the turns are flush against the
   sidebar divider and the right edge, and a short transcript is pushed to the bottom
   with a void above it (`ListAlignment::Bottom`). Restore the horizontal inset and the
   content max-width the pre-virtualised transcript had (the same tokens the composer
   column uses, so the composer and the turns line up), and make short transcripts start
   at the top: use `ListAlignment::Top` with the existing follow-the-tail logic
   (`scroll_to_reveal_item`/scroll-to-bottom when the reader was at the tail), verifying
   the stress capture still sticks to the tail while streaming. Proof:
   `improve-integrated-dark/light.png`, `improve-integrated-markdown-dark.png`,
   `improve-transcript-stress-tail-dark.png`.
2. **Header title elision** (`app.rs` header cell). A derived title that is the whole first
   prompt spans the header; elide it to one line with `text_ellipsis`/`overflow_hidden`
   and a max width (token), keeping the provider mark and the overflow button visible.
   Proof: `improve-integrated-dark.png`.
3. **Sidebar description line** (`sidebar.rs` `SessionEntry::summary`). The second line
   currently repeats the first prompt that the derived title already shows. Rule: show
   `last_summary` when present; otherwise show the first prompt only when the row's label
   is NOT derived from it (a user-given name or a Muse title); otherwise show nothing but
   the turns meta. Proof: `improve-shell-open-dark.png`.
4. **Rename field single-line** (`app.rs` `rename_field`, the dense-field recipe). The
   editing field wraps the name onto a clipped second line; it must be single-line
   (no wrap, horizontal overflow hidden, caret visible) and fill the row up to the
   trailing meta. Applies to the header rename too. Proof: `improve-shell-rename-dark/light.png`.
5. **Search palette rows** (`search.rs`, palette rendering in `app.rs`). Rows show raw
   session ids and the unstripped index text (`^_` separators, "valid", paths). Primary
   text = the session label (the same label the sidebar shows); secondary = a clean
   snippet: strip the `\x1f`/`^_` unit separators and the id/status prefix, collapse
   whitespace, ~90 chars around the first match, proportional font, match emphasised if
   the palette row supports it. Files section keeps path + owning session label. Also the
   old sidebar quick-filter field must not open together with the palette (⌘⇧F and the
   icon open only the palette). Proof: `improve-integrated-search-dark.png` and a light one.
6. **Turn actions**: hide the Pin action on assistant turns (no meaning here). Proof:
   `improve-integrated-markdown-dark.png`.
7. **Text selection wiring** (Task C item 8b): the library now exposes
   `UserTurn/AssistantTurn::selection(..)`, `on_selection_change(..)` and
   `turn_selected_text(..)` (`../agentic-ui/docs/06-api.md`). Pass the view's
   `TextSelection` to every turn, update it from the intent, clear on Escape / click
   elsewhere, and make ⌘C copy `turn_selected_text` of the selected turn when the composer
   has no focus. Screenshot a selection if a `--steps` verb can express it (e.g.
   `select:<turn>:<from>-<to>`); otherwise state that it was code-verified only.
8. Re-read the owner's list (`docs/diagnosis/inputs/owner-issue-list.md`) against the
   merged tree and list, item by item, done / done-with-caveat / not done, in the report.

Gates, all must pass: `cargo build --workspace`, `cargo test --workspace`,
`cargo clippy --workspace --all-targets -- -D warnings`,
`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`, `cargo tree -d` (one
`gpui-pre`, one `gpui-kit`); regenerate and read `muse-adapter` snapshots only if a fold
change requires it (quote the diff). CHANGELOG entry ("Fix-up") dated 2026-09-10.
Report: what changed per item, gate results verbatim, screenshot paths, the item-by-item
owner-list status. Never claim a gate you did not run.
