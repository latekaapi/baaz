# 66 — Handoff: build the provider screens (D5 · Providers), 2026-10-24

Read this first in the next session. It replaces docs/64 as "where are we" for providers; docs/64 §4.1 (the P4 A/B
decision) is still open and untouched. The why, the evidence and the competitor comparison are **docs/65**; this doc
is the build spec.

**Stage status:** detection + sign-in fixes (docs/65 S1–S6) **built on main, NOT pushed**. The screens are
**APPROVED by the owner (2026-10-24, "Looks good")** as drawn on the canvas "Baaz v3"
(https://claude.ai/artifact/7SwC39oc2nEakd4TpCdp8e), **version 58, board `D5Providers`**. Nothing on the board
is built. The board is the spec: where this doc and the board disagree, the board wins; where neither answers,
ask the owner — never invent a design answer (docs/40 §6).

## 1. State of the code

| Repo | Head | Pushed | Notes |
|---|---|---|---|
| harness (Baaz) | `e50f307` (on `2262d34`, 4 commits) | **no** | pins agentic-ui **v0.6.2**; the local `[patch]` → `../agentic-ui` is on, so `Cargo.lock` shows modified: **never commit it** |
| agentic-ui | `c2ae1a1`, tag v0.6.2 | yes | untouched by this stage so far |

Commits: `8cd0603` (docs/65 + S1/S2), `d8d46c6` (S3–S6), `ac6b6d8` (redraw only on a visible change), `e50f307`
(docs/65 progress + the board generator). Gates at `e50f307`: 2,123 tests pass (9 ignored), clippy clean, perf
budgets all met but `idle.cpu_pct` 0.6 > 0.5 (identical on the P4 build — the known gpui display-link wake), the
18-entry probe + `right-files-scrolled` 0 changes. `connect-first-run`, `connect-checking`, `launch-cached-shell`
baselines are from 2026-09-28 (pre-v3) and differ — looked at, correct; regenerate on main deliberately.

## 2. What already exists to build on (S1–S6, docs/65 §3.1)

- `ProviderId::{install_command, update_command, version_floor, docs_url}` (`providers.rs`) — the one table.
- `provider_status`: `MuseAuthRead` (three-valued Muse auth), `Advisory::TooOld` from `version_triplet` /
  `floor_advisory`, single-flight `refresh_with` + `probe_and_publish`, `status_generation()`, `status_wake()` /
  `window_status_wake()` (gpui test windows get a closed channel), `watch_setup(id)` (re-probe every 3 s ≤ 10 min
  until the headline moves), `first_run_at_boot()`.
- `connect.rs`: `status_stamp()`, `visible()` compare, `open_muse_sheet()`, `start_muse_once_available()`
  (`MUSE_OFF`), `follow_default_provider()`, `get_action()` ("Get Muse"), `start_codex_login()` (the one Codex flow,
  login-shell PATH via `codex_login_command`).
- `settings_providers.rs`: `prefer_connected`, `actions_for(id, headline)`, `signout_steps` via the resolved binary,
  `auth_command` with the login-shell PATH.
- aui (v0.6.2): `screens::providers` — `connect_providers`, `provider_card`, `ProviderActionDef` (public fields),
  `ConnectRowData`; `aui-terminal` backs the dock terminal. The setup sheet is new.

## 3. The approved board, screen by screen

Measured values are the board's own (`docs/34-refs/v3-redo/canvas/gen_d5p.py`, CSS block `p5*`), on the v3 tokens
(docs/35; `--l0/--l1/--l2/--l2h`, `--edge`, `--float`, `--line`, `--muted`, `--faint`, `--succ/--warn/--dang`,
`--well`; primary button = light fill). Text sizes are already at the app's 1.1 scale (14.3 body, 13.2 caption,
12.65 mono, 19.8 title).

**B1 · Connect your providers (refined)** — boards "First run · proposed", "Nothing ready yet", "Checking", "Too old ·
couldn't check".
- Column 440 wide, centred; mascot 72×81; title 19.8/26 semibold; intro 14.3/20 muted, copy: *"Baaz runs the
  coding agents you already use. Connect at least one to start."* (the tally line goes).
- **One rows container** (not three cards): r 10, `--l1`, edge; rows ≥ 60 tall, padding 12/14, gap 12, hairline
  `--line` between rows. Mark 24 (r 6). Name 14.3/20 semibold; status line 13.2/18 muted with a 6 px dot (ok =
  succ, needs action = warn, absent = faint, can't check = dang) or a 12 px spinner while Checking; status line
  ellipsizes, never wraps (the live check found "· Claude Max" wrapping today).
- Row order: **Claude Code, Codex, Muse** (the board's order; today it is Muse first).
- One action per row, right: Install (primary) · Sign in (primary) · Update (primary, TooOld) · Re-check (secondary,
  can't check) · **Get Muse ↗** (secondary, opens dev.meta.ai/lp/muse-code) · Connected = `✓ Connected` text in
  succ, no button. **No Docs buttons on this screen.**
- Status copy: Connected → `email · plan`; Signed out → "Signed out" (or "Installed · signed out"); missing → "Not
  installed"; TooOld → "<version> is older than Baaz supports (<floor>)"; timeout → "Couldn't check — the CLI timed
  out"; Checking → "Checking…" (never "Not installed" while a probe is pending).
- Footer row, margin-top 8: left **"New sessions start in [mark Claude Code ⌄]"** — a picker (24 tall, `--l2h`, edge,
  r 6, 16 px mark) listing **Connected providers only**, defaulting to the first Connected; choosing writes the
  last-provider store. With nothing Connected the left side reads *"Nothing is ready yet. Set up later in Settings ›
  Providers."* (wraps at ~230). Right: "Skip for now" (text button) + Continue (primary; disabled until one is
  Connected).

**B2 · The setup sheet (new)** — boards "Setup sheet · Install", "· Sign in to Claude Code", "· done".
- Modal over the screen that asked (connect screen or Settings): scrim `rgba(0,0,0,.45)`; sheet 560 wide, r 12,
  `--l1`, `--float`. Header 44: mark 20, title 14.3 semibold ("Install Claude Code", "Sign in to Claude Code",
  "Update Codex"…), ✕ (28 ghost); hairline under.
- Status line (padding 12/16/0, 13.2 muted): spinner + "Waiting for the install to finish…" / "Waiting for
  sign-in…" → on Connected a succ ✓ + "Connected · email · plan". Driven by the live status (`watch_setup` + the
  status wake) — no polling of the terminal's text.
- **A real terminal** (Baaz's own `aui-terminal` session, not a picture): well, r 8, edge, 150 tall, mono 12.65/19,
  margin 10/16. The command is **typed, not run** (docs/23 §5, owner 2026-10-23): `curl -fsSL
  https://claude.ai/install.sh | bash`, `claude auth login`, `npm i -g @openai/codex`, the `update_command`.
- Footnote (13.2/18 faint): install — *"The command is typed, not run: read it, then press Enter. Baaz checks every
  few seconds and shows Connected here when Claude Code answers."*; sign-in — *"Claude Code signs in through your
  browser. This terminal is Claude Code's own; Baaz never sees your credentials."*; done — *"Claude Code is ready. New
  sessions can start in it."*
- Actions (right): Close (secondary) while waiting → **Done** (primary) once Connected. Close / ✕ / Escape never
  cancel what runs in the terminal; the watch keeps going and the row updates.
- It replaces every "typed in the dock terminal" path (connect rows, Settings Install / Sign in, TooOld Update) and
  the prefill notes/toasts ("press Enter to run it").

**B3 · Codex signing in + Muse sheet** — boards "Codex · signing in (in the row)", "Muse · sign-in sheet".
- Codex stays in its row (no sheet): status "Finish signing in with your browser…" with a spinner, the row's button
  is **Cancel** (secondary). Under the rows, one muted line: *"Browser didn't open? Copy the link · Use a code
  instead"* — Copy the link copies the `authUrl`; **Use a code instead** starts `account/login/start {type:
  "chatgptDeviceCode"}` and shows the code (owner approved the board with this option; check the exact app-server
  shape against codex-cli 0.144.6 before building — docs/23 §2 names it).
- Muse's sign-in moves into the setup sheet's frame (460 wide): header mark + "Sign in to Muse" + ✕; body 14.3/21
  *"A Meta account draws on your Muse subscription."* + muted *"An API key bills every turn to that key."*; actions
  "Use an API key" (secondary) + "Continue with Meta account" (primary). ✕ and **Escape** close it; no second
  mascot; the "Muse Code today. Codex, Claude and more to come." copy is gone. The device-code and API-key steps
  that follow keep today's logic (`login.rs`), restyled into this frame.

**B4 · Settings › Providers › <provider> card** — boards "Settings › Providers › Claude Code", "API-key login".
- Card r 10, `--l1`, edge. Top row (padding 14, gap 12): mark 24, name 14.3 semibold over the status line (dot +
  13.2 muted), then **Re-check** (text button) and the state's action (Sign out secondary / Sign in / Install …).
- **Details** disclosure under a hairline (closed by default; chevron + "Details" 13.2 muted): a 96 px / 1fr grid,
  13.2/18, keys faint: **Version**, **Path** (mono 12.65, `~` for home), **Found** ("on your login shell's PATH
  (zsh)" / "BAAZ_CLAUDE override" / "Path you set"), **Signed in** ("claude.ai · email · plan" or "API key from
  ANTHROPIC_API_KEY · billed per token"). Then **Change path…** and **Copy diagnostics** (secondary).
- **Change path…**: a per-provider binary override stored in Baaz settings, used by `default_resolve` /
  `resolve_program` ahead of PATH (the env vars still win); clearing it returns to auto.
- **Copy diagnostics**: plain text — provider, PATH source (login shell / path_helper / inherited), every candidate
  found (`which -a` equivalent over the login PATH), resolved path, `--version` output, raw auth-probe output, timing.
- **API-key logins** (owner 2026-10-23: say it, don't block): status line "Connected · API key · billed per token"
  with a warn dot; an advisory box (r 8, `--l2`, edge, warn glyph) — *"An ANTHROPIC_API_KEY in your shell signs Claude
  Code in: every turn is billed to that key, not a subscription. Remove it from your shell profile to use your Claude
  plan."* Detect from `claude auth status --json` (`authMethod: "api_key"`, `apiKeySource`) and Codex `account/read`
  (`account.type == "apiKey"`). No send guard, no env stripping.
- TooOld shows here too (the advisory text already names the update command); its action opens the setup sheet.

**B5 · Signed out mid-session (every provider)** — board "Signed out mid-session".
- One quiet banner above the composer (margin 0 20, r 8, `--l2`, edge, padding 8/8/8/12, 13.2): warn dot,
  *"<Provider> is signed out. Sign in to send."*, **Sign in** (primary) → the setup sheet (Claude), the Codex flow,
  or the Muse sheet. Replaces today's "X is signed out — Sign in" text.
- Raised from **typed signals**, not text matching: Claude Code `authentication_failed` (stream/result error),
  Codex 401 / `unauthorized` (`codexErrorInfo`), Muse's account change (`provider-muse/src/errors.rs` already maps
  it) — plus any re-probe that reads Signed out. Today only Muse is typed; the Claude Code and Codex adapters need
  the mapping (a `ProviderEvent` / recovery hint, docs/50 §2 "typed recovery hints").

## 4. Build order (one commit per step, same gates each)

Library first, then Baaz adopts (docs/40 §4 step 4); library changes ship as an agentic-ui tag + repin.

1. **aui**: `setup_sheet` component (frame, header, status line, terminal slot, footnote, actions) with roles and
   labels; the refined `connect_providers` (one container, picker footer, no Docs) and `provider_card` with the
   Details disclosure; a `signed_out_banner`. Gallery cards for every board state. Tag (v0.6.3).
2. **B1** in Baaz: connect screen rows/order/copy/footer picker.
3. **B2**: the sheet hosting an `aui-terminal` session; every typed-command path routed through it; `watch_setup`
   drives its status line.
4. **B3**: Codex in-row waiting + Copy link + device code; Muse sign-in in the sheet frame (✕, Escape).
5. **B4**: Settings card Details, path override (persisted + used by resolution), Copy diagnostics, API-key line.
6. **B5**: typed auth signals in the Claude Code and Codex adapters → the one banner for all three.
7. **Probe entries** for every board state (docs/65 S7): connect all-missing / all-signed-out / too-old / checking;
   the sheet waiting + done (scripted status); Codex waiting; Muse sheet; card with Details; API-key card; banner.
   Regenerate the stale connect baselines on main as their own commit.

## 5. Gates and live checks

Every step: `cargo test --workspace`, `cargo clippy --workspace --all-targets` (both repos; aui-cef separately),
`python3 scripts/perf-budgets.py --binary target/release/baaz` (after `scripts/bundle.sh`), the 18-entry probe in
CLAUDE.md + `right-files-scrolled` + the new entries, and **BaazCheck** with a fresh state.

**Seeing the states live without touching the owner's real logins** (found this stage):
- Claude Code signed out: launch with `CLAUDE_CONFIG_DIR=<empty dir>` — `claude auth status --json` reads
  `loggedIn: false` (verified). Signing in there writes a separate login, not the owner's.
- Codex signed out: `CODEX_HOME=<empty dir>` — `account/read` → `{"account":null}` (verified).
- Muse signed out: the `muse` launcher reads `MUSE_AUTH_PATH` (default `~/.config/muse/auth.json`) — point it at an
  empty path (check that `muse serve` honours it before relying on it).
- Claude API-key login: `ANTHROPIC_API_KEY=sk-test` with an empty `CLAUDE_CONFIG_DIR` → `authMethod: "api_key"`
  (verified; never send a turn with it).
- Not installed: harder (the resolver merges the login-shell PATH and known dirs); use the new path override pointing
  at a missing file, or a fake `HOME` (Claude lives in `~/.local/bin`).

BaazCheck recipe: docs/60 §4 / docs/40 §4.6 (copy the bundle, `PlistBuddy` id `sh.baaz.check`, `codesign -s -`,
`env -i HOME USER PATH=/usr/bin:/bin TMPDIR SHELL=/bin/zsh BAAZ_STATE_DIR=$(mktemp -d "$TMPDIR/bz.XXXX")`, launch
with `--workspace <scratch git repo>`). Window capture: a CGWindowList id helper + `screencapture -x -o -l <id>`
(largest layer-0 window of the pid). Park the pointer first (a 6-line Swift `CGWarpMouseCursorPosition`). Check
`HIDIdleTime` before any windowed run.

## 6. Rules (unchanged)

Tests never reach a real CLI or server. A Muse turn costs money on this login (pay-as-you-go): none is needed for
this stage. Never capture the full screen. Prune `target/*/incremental` before long chains (disk fell to 4.7 GB this
session). Don't push until the owner says "push". Design questions the board doesn't answer go to the owner.
Never reuse short CSS class names on the canvas (`p5`-prefix for this board). At the end: tracker (merge into the
live version, short checklist), memory, a handoff doc, report per step.

## 7. Prompt for the next session

> Build the approved provider screens. Read `docs/66-handoff-provider-screens.md` first (state, the approved board
> screen by screen, build order, live-check recipes), then `docs/65-provider-onboarding.md` (why, evidence, owner
> answers) and the canvas "Baaz v3" (https://claude.ai/artifact/7SwC39oc2nEakd4TpCdp8e) — board `D5Providers`,
> approved by me at version 58 — and the live Baaz Tracker (https://claude.ai/artifact/Fr965AKBn8toZ7BcUk7wCP).
> Harness main is `e50f307`, not pushed; agentic-ui is v0.6.2; work on main with the local `[patch]`, never commit
> `Cargo.lock` while it is on.
>
> Build docs/66 §4 in order: the agentic-ui components (setup sheet, refined connect screen, provider card with
> Details, signed-out banner) with gallery cards and a tag, then B1–B5 in Baaz, then the probe entries. One commit
> per step, with docs/66 §5's gates every step, and drive BaazCheck (fresh state) for each screen using the
> throwaway-login recipes in §5 — never my real Claude, Codex or Muse logins, and no Muse turns. The board is the
> spec; anything it doesn't answer comes back to me. Don't push until I say "push". At the end: update the tracker
> (merge into the live version, add a short checklist), memory, a handoff doc, and report per step.
