# Brief — owner round 2, library fix-up 2: the palette takes a lead element

Repository `/Users/latekaapi/Projects/agentic-ui`. Create branch `round2-palette-lead` off
`main` and work ONLY there. Do NOT commit. Do not touch `/Users/latekaapi/Projects/harness`
or `~/Projects/cockpit`. Prefix every shell command with
`export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`.

## F1 — `PaletteSection::lead`

File: `crates/aui/src/overlay/command_palette.rs`.

The harness wanted its `folder_drop_card` as the first thing in the Projects palette's
"Add" section; the palette only takes rows, so the card ended up floating above the palette
as a detached box. Give `PaletteSection` a `.lead(el: impl IntoElement)` slot: an element
rendered inside the palette, under the section title and before the section's rows, with the
rows' horizontal padding and a `SP_2` gap below it. It is not a row: keyboard selection skips
it, hover does nothing to it, and it scrolls with the list. A section may have a lead and no
rows. Keep every existing signature.

Gallery: the `shell/command-palette` card's "Projects" section gets a `folder_drop_card` as
its lead so the composition is visible; screenshot both themes to
`/tmp/aui-round2/palette-lead-<theme>.png`.

## Gates

`cargo build --workspace`; the all-features build
`cargo build --workspace --features aui-webview/wry,aui-terminal/pty,aui-terminal/tui`;
`cargo test --workspace`; `cargo clippy --workspace --all-targets -- -D warnings` (last);
`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`; `python3 scripts/api-doc.py`.
Report: done / skipped-with-reason, screenshot paths, gate output verbatim (last lines).
When finished write the single word `done` to `/tmp/muse-round2-lib-fixup2.done`.
