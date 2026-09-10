# Brief — selectable transcript text in the `aui` library (single package)

Repository: `/Users/latekaapi/Projects/agentic-ui`, currently on branch
`improvements-2026-09-10` (work directly on this branch; do not create worktrees; commit
on this branch when the gates pass). Do not touch `/Users/latekaapi/Projects/harness`.

Prefix every shell command with
`export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`. Library rules:
`docs/00-agent-brief.md`, `docs/04-design-rules.md` — tokens only, stateless `RenderOnce`
with intents out, both themes, gallery entry for anything new. Gates before the commit:
`cargo build --workspace`; `cargo build --workspace --features aui-webview/wry,aui-terminal/pty,aui-terminal/tui`;
`cargo test --workspace`; `cargo clippy --workspace --all-targets -- -D warnings`;
`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`; `python3 scripts/api-doc.py`.
Report gate results verbatim; never claim a gate you did not run. Commit message ends with
`Co-Authored-By: Muse Code <noreply@meta.com>`.

## Problem

Transcript text cannot be selected: every cell paints `StyledText`, and gpui-pre 0.3.3 has
no selectable text element (see
`/Users/latekaapi/Projects/harness/docs/diagnosis/transcript.md` §C3, which cites
`~/.cargo/registry/src/*/gpui-pre-0.3.3/src/elements/text.rs` — `InteractiveText` offers
click/hover over character ranges only). The new `transcript/markdown.rs` renderer on this
branch already uses `InteractiveText` for link ranges.

## What to build

1. `crates/aui/src/transcript/selectable.rs`: a selection model + painter for markdown
   paragraphs. State lives with the caller (stateless component): a `TextSelection
   { cell: SelectionKey, range: Range<usize> }` value the app stores, and intents
   `on_selection_change(|Option<TextSelection>, window, cx|)`. Mechanics: on left
   `on_mouse_down` inside a text run record the anchor index (use the shaped line's
   `index_for_position` — see how `InteractiveText` maps positions to indices in
   `text.rs`), on `on_mouse_move` with the button held extend the range, on mouse-up
   commit; double-click selects the word, triple-click the paragraph. Paint the selection
   behind the glyphs with the theme's selection colour token (add one to `aui-tokens` if
   absent). Since hover-gated moves stop at the cell edge, expose a capture overlay
   pattern like `shell/resize_handle.rs` does, or document the limitation.
2. Wire it into `markdown(...)`: `.selection(Option<&TextSelection>)` and
   `.on_selection_change(...)`, so every paragraph/heading/list item/quote/table cell is
   selectable; code blocks too. Link click vs selection drag: a press-release without
   movement on a link range is a click; movement starts a selection.
3. Copy: `markdown(...)` exposes `selected_text(&TextSelection) -> Option<String>` (or a
   free function) so the app can put it on the clipboard on ⌘C; the app owns the keybinding.
4. Gallery: `transcript/markdown` entry gains a selectable state (the gallery card holds
   the selection in its own state) and a legend line explaining it.
5. Unit tests for word/paragraph selection ranges and `selected_text` over a paragraph
   with inline code and a link.

Report: files changed, public API added, gallery entry, gate results verbatim, limitations
(e.g. selection across cells) stated plainly.
