# 86 — Patch cadence (docs/82 9.4)

What Baaz carries from upstream, how often it moves, and how a move is made. Written 25 Nov 2026.

## What is pinned

| Upstream | Where it is pinned | Today |
|---|---|---|
| gpui (`gpui-pre` family) | agentic-ui `Cargo.toml` (`gpui = { package = "gpui-pre", version = "=…" }` and its siblings) | 0.3.12 |
| Chromium, through CEF | the `cef` crate in harness `Cargo.lock` **and** `scripts/bundle.sh` (`CEF_VERSION`, `CEF_ARCHIVE`, `CEF_SHA1`) | CEF 154.0.33, Chromium 154.0.8037.94 |
| The provider CLIs (`claude`, `codex`, `muse`) | not bundled: each person's own install; Baaz checks a version floor per provider | floors in each adapter's `caps.rs` |

## Cadence

- **gpui: monthly.** On the first working day of the month, bump agentic-ui to the newest `gpui-pre` release, run the
  library gate (`cargo test`, `scripts/gallery-baseline.py`), then repin harness and run its standard gate. A month with
  no gpui release is a no-op, written as such in the release log below.
- **CEF: within a week of each Chromium stable milestone.** When Chrome stable moves to a new major (about every four
  weeks), take the first CEF build of that branch from the CEF build server, bump the `cef` crate (and `aui-cef`'s) to
  the matching version, and set the three pins in `bundle.sh` together — `bundle.sh` refuses a pin that does not match
  `Cargo.lock`. A Chromium security fix inside a milestone is taken the same week it reaches a CEF build.
- **Provider CLIs:** not ours to ship. When a CLI release changes a wire shape Baaz reads, the adapter is fixed and the
  floor raised in the same change, never before a live probe of the new version.

## The app refuses nothing

Baaz never refuses to run over its engine's age. About Baaz names the engine the bundle carries ("Web engine: Chromium
154.0.8037.94 (CEF 154.0.33)", from the `BaazEngine` key `bundle.sh` writes into `Info.plist`), so a person or a bug
report can tell which one it is.

## How a CEF bump is made

1. Find the new archive on https://cef-builds.spotifycdn.com/index.html (macOS ARM 64, *Minimal*), and its SHA-1.
2. Bump `cef` (harness and agentic-ui `aui-cef`) to the matching `x.y.z+<cef version>`.
3. Set `CEF_VERSION`, `CEF_ARCHIVE`, `CEF_SHA1` in `scripts/bundle.sh`.
4. Move the old `~/.local/share/cef` aside; `scripts/bundle.sh` fetches and checks the new one.
5. Standard gate, plus a Browse-mode live check on the check build.

## Release log

| Date | What moved | Gate |
|---|---|---|
| 25 Nov 2026 | Cadence written; CEF pinned at 154.0.33 with its checksum (9.1) | — |
