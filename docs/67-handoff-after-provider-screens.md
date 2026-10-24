# 67 — Handoff: the provider screens are built (D5 · Providers), 2026-10-24

Read this first in the next session. It replaces docs/66 as "where are we" for providers. docs/66 stays the build
spec (§3 the board screen by screen); docs/65 the why and the evidence. docs/64 §4.1 (P4's A/B decision) is still
open and untouched.

**Stage status:** every step of docs/66 §4 is **built, gated and seen live** on BaazCheck with throwaway logins.
**Nothing is pushed.** The owner's look (tracker checklist "Provider screens, 24 Oct") comes next, then "push".

## 1. State of the code

| Repo | Head | Pushed | Notes |
|---|---|---|---|
| harness (Baaz) | main head = the commit adding this doc (13 on `2262d34`; code complete at `e134d36`) | **no** | `Cargo.toml` still pins agentic-ui **v0.6.2**; the local `[patch]` → `../agentic-ui` is on, so `Cargo.lock` shows modified — **never commit it** while the patch is on |
| agentic-ui | `de24f42`, tag **v0.6.3 (local only)** | **no** | 2 commits on `c2ae1a1` (v0.6.2) |

At "push": push agentic-ui `main` + tag `v0.6.3`; in harness set the four `aui*` pins in `Cargo.toml` to `v0.6.3`,
remove the local patch, `cargo update -p aui` (Cargo.lock then commits cleanly), rebuild, run the gates once more,
push harness `main`.

Harness commits (oldest first): `8cd0603` `d8d46c6` `ac6b6d8` `e50f307` `e00eeba` (the S1–S6 detection work and the
docs, from 23–24 Oct), then this stage:

| Step | Commit | What |
|---|---|---|
| 1 · aui | agentic-ui `3ce707d` + `de24f42` | setup_sheet / sheet_frame / sheet_scrim; refined connect_providers (StatusLine, one action, picker footer, assist line); provider_card with Details; signed_out_banner; Login::sheet(); ButtonSize::Row; gallery cards screens/connect, screens/setup-sheet, screens/providers for every board state |
| B1 | `8c1493a` | connect screen: order, copy, one rows container, footer picker over Connected providers; card adopts the new shape; **fix:** `claude auth status --json` exits 1 when signed out → read "not verified" |
| B2 | `1c67f0c` | `crate::setup`: the setup sheet with a real aui-terminal session per provider; every typed-command path routed through it; status line from the watch |
| B3 | `32576e7` | Codex in-row waiting + Copy the link + device code (generation-guarded); Muse sign-in as a sheet; **fix:** Codex sign-in's `initialize` lacked `clientInfo.version` → codex 0.144.6 refused it, so the sign-in never started live |
| B4 | `979bb21` | Settings card on the page with Details; per-provider path override (`provider-paths.json`) used by resolution and Muse's connection; Copy diagnostics; API-key source + advisory box |
| B5 | `295d1ee` | typed auth signals (Claude `authentication_failed`, Codex `codexErrorInfo` unauthorized/401) → `TurnSettled{signed_out}` → the one banner above the composer for all three (the "Signed out of Muse" dialog is gone) |
| 7 · probe | `928b527` | 13 uiprobe entries, one per board state |
| baselines | `e134d36` | deliberate: the 13 new + connect-first-run, connect-checking, launch-cached-shell (stale since 2026-09-28) + settings-provider-codex (redrawn) |

## 2. Gates (every step, docs/66 §5)

Last full run (B5): agentic-ui 831 tests pass, clippy clean (aui-cef separately: clean); harness `cargo test
--workspace` **2,141 passed, 0 failed, 9 ignored**; clippy `--workspace --all-targets` clean; perf budgets **0 missed**
(idle CPU read 0 this time); probe over 35 entries after the baseline commit: **all match except
`right-files-scrolled`** — its fixture lists this repo's own `crates/baaz/src`, which gained `details.rs` and
`setup.rs`. Not regenerated on purpose (it is fixture drift, not a pane change); regenerate with the push.

The gate script used every step is reproducible from docs/66 §5; the probe command for provider screens is in
`CLAUDE.md` (local file).

## 3. Seen live (BaazCheck, fresh `BAAZ_STATE_DIR`, Dock-like `env -i`)

The screen was **locked** all session, so window captures (`screencapture -l`) failed; every live look used Baaz's
own `--screenshot` against the bundled BaazCheck binary with real probes (works while locked). Throwaway logins:
`CLAUDE_CONFIG_DIR`, `CODEX_HOME` and — new — `XDG_CONFIG_HOME` (Muse's binary reads `$XDG_CONFIG_HOME/muse`;
`MUSE_AUTH_PATH` only feeds the launcher's download auth). No Muse turn; no turn at all.

- B1: all three signed out → "Nothing is ready yet"; `ANTHROPIC_API_KEY=sk-test` → Claude Code "API key · billed
  per token" + ✓ Connected, picker defaults to Claude Code, Continue on.
- B2: the sheet over the connect screen with the owner's real login shell; the command typed once, after the prompt
  (first build pasted too early and zsh drew it twice — fixed); Codex/Claude installs show "Installed · <v>".
- B3: Muse's sheet live; the device-code request run against the real codex app-server on a throwaway home:
  `{type, loginId, verificationUrl, userCode}` as parsed. Codex waiting/device rows via capture verbs.
- B4: API-key card with Details (real `apiKeySource`), a Codex override pointing at nothing → Not installed, "Path
  you set — no program there", "Use automatic path". Diagnostics printed via `--steps diagnostics:<id>`: this Mac
  resolves Codex from nvm (0.153.2) ahead of Homebrew (0.144.6).
- B5: the banner over a live Claude Code session.

**Not seen live** (needs hands or a real sign-in): clicking through Install → Enter → Connected in the sheet (the
transition is unit-tested; the watch is S6's), finishing a Codex browser or device sign-in, pressing Copy diagnostics
(the clipboard), the footer menu by pointer (captured open via `connect-menu`), a real `authentication_failed` /
401 mid-turn (it would need a turn on a bad key).

## 4. Decisions taken where the board was silent (tell the owner; tracker "Decisions")

1. Codex device code: in the line under the rows — **asked and answered by the owner (2026-10-24)**.
2. An install that lands signed out: sheet status "Installed · <version>", footnote "<Provider> is installed. Sign in
   from its row next.", Done.
3. "Use automatic path" beside Change path… when an override is set (clearing needed a control).
4. Codex and Muse API-key notices reuse the Claude note's shape.
5. The card has no Enabled switch (the board has none; Settings' overview keeps the switches).
6. Scrim clicks do nothing on the setup sheet (it holds a terminal); Muse's sheet likewise (✕/Escape close).

## 5. New `--steps` verbs (all documented in `steps.rs`, all free)

`setup:<provider>:<install|signin|update>`, `connect-menu`, `codex-login:<waiting|device|failed>`, `muse-sheet`,
`details:<provider>`, `diagnostics:<provider>`, `signed-out`. Window verbs now also run on the connect screen.

## 6. Open / follow-ups

- Push (above). Regenerate `right-files-scrolled` at push.
- The dock terminal still gets the inherited PATH (the setup sheet now gets the login PATH); same fix would help it.
- The owner's `.zshrc` calls `starship`, which is not installed — every Baaz terminal prints that error. Theirs.
- P4 A/B (docs/64 §4.1) still open.
- docs/65 S7's leftover states (Unverified, non-timeout Can't run) have no probe entry; both render through the same
  status-line code that the too-old entry covers.

## 7. Prompt for the next session

> Read `docs/67-handoff-after-provider-screens.md` (the provider screens are built, gated and seen live, nothing
> pushed) and the live Baaz Tracker (https://claude.ai/artifact/Fr965AKBn8toZ7BcUk7wCP). If I said "push": push
> agentic-ui main + tag v0.6.3, repin harness to v0.6.3 and drop the local patch, re-run docs/66 §5's gates,
> regenerate `right-files-scrolled`, push harness. Otherwise work through my checklist notes. Then P4's A/B call
> (docs/64 §4.1).
