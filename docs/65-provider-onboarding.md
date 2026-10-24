# 65 — Provider onboarding: detect, install, sign in, keep working (plan, 2026-10-23)

Status: **plan + build in progress.** Owner request 2026-10-23: review and compare how Baaz finds, installs, signs in
and starts Claude Code, Codex and Muse for a newly installed user, against the competitors; plan, design, implement.
This is docs/50's **P5 (detection)** plus the provider-screen half of D5 and the auth half of **P6**, done as one stage.
It does not change a docs/50 decision; §6 lists the questions only the owner can answer.

Evidence: a read-only trace of Baaz main `2262d34`; three competitor sweeps pulled to 2026-10-22/23 under
`~/Sandbox/baaz/orchestration-research/repos/` (t3code `807319c27`, synara `94407ef8`, bb `1e80ca7b`, paseo `4b9b2fe`,
orca `c43ec4ddb`, emdash `5bebb2e84`, zed `9e84b06`, superset `853e3e8d`); live probes of the real CLIs on this Mac
(claude 2.1.276, codex-cli 0.144.6, Muse Code 1.4.11) with throwaway `CLAUDE_CONFIG_DIR` / `CODEX_HOME` so no account
was touched. File:line references are from that trace.

## 1. What a new user meets today

Boot (`main.rs:770`) reads `<state>/provider-status.json`, starts one background probe of all three CLIs, then
`Harness::new` decides the launch from stored facts: no `onboarding_completed`, no sessions, no projects → the
**Connect your providers** screen (`connect.rs`, aui `screens/providers.rs`). Muse's `muse serve` is spawned eagerly
unless the cache says it is missing or off.

| Case | What the person sees | What goes wrong |
|---|---|---|
| (a) nothing installed | three rows: Muse "Not installed" with **two** Docs buttons; Claude/Codex "Install" + Docs; Continue disabled | Install types a command into a terminal **that is not on screen** (the connect screen is the whole window, `app.rs:6397`); nothing re-checks when the install finishes; Skip lands on a shell whose default provider is still **Muse** → "Muse is not running" |
| (b) all installed, none signed in | Claude "Signed out"; Codex and Muse "Installed · sign-in not verified" | Codex reads *not verified* because `rateLimits/read` errors when signed out and `probe.rs:166` throws away the good `account/read` (`{"account":null}`) — verified live; Muse can never read "Signed out" (`provider_status.rs:891`); Claude's Sign in types into the invisible terminal |
| (c) only Claude signed in | Continue enabled | default stays **Muse** (picked from all-Checking statuses at boot, kept by `finish_connect` because it is "installed", `connect.rs:603`) — new sessions open on a signed-out Muse |
| (d) all signed in | rows go Checking → Connected | Muse first flashes "not verified" + Sign in, and can flip back: every background refresh rewrites the **whole** cache from its own stale copy (`provider_status.rs:631,1519`) |
| any CLI launch from a project dir | **no onboarding at all** | `boot_projects()` adopts the launch dir (`app.rs:1894`) before `is_first_run()` checks "no projects" (`app.rs:1932`) |

After onboarding:
- **Muse "Sign in" from Settings and from the "Signed out of Muse" dialog does nothing visible** — both reset the login
  and drop the open session but never raise the sheet (`settings_providers.rs:716`, `dialogs.rs:1179`).
- **Settings › Muse › Install types `curl -fsSL https://www.muse.dev/install.sh | bash`.** muse.dev is an unrelated
  product; the URL 302s to `lift.sonatype.com`. Piping that into bash is unsafe and was never a Muse installer. Its Docs
  button goes to the same wrong domain; the connect screen's goes to this repo. Two install tables disagree.
- Settings' Codex sign-in shuts its app-server down right after it gets the auth URL (`settings_providers.rs:848`),
  which very likely kills the login callback; the connect screen's flow keeps it alive. The connect flow spawns
  `codex app-server` without the login-shell PATH (npm/nvm installs fail from the Dock); `claude auth logout` runs by
  bare name.
- Status changes reach the window only when a frame happens to run and sees the cache file's mtime move
  (`connect.rs:767`) — since P0 nothing renders at rest, so a finished probe can sit unseen.
- No version floors (`Advisory::TooOld` exists and is rendered but never produced); no diagnostics beyond the version;
  no path override except env vars; no single-flight (overlapping probes spawn duplicate app-servers).
- A Claude Code login that is an **API key** (`authMethod: "api_key"` — any `ANTHROPIC_API_KEY` in the environment
  makes `claude auth status` say so, verified) bills per token with no word from Baaz; Muse's pay-as-you-go has a guard,
  the other two have nothing.
- Credential loss mid-session is noticed only for Muse; a Claude/Codex child that dies is a dead session (P6).
- Copy: Muse's sheet still says "Muse Code today. Codex, Claude and more to come."; the sheet is a full-window 97 %
  layer with a second mascot, no close control and no Escape; "2 needs sign-in"; "Waiting for sign-in… Cancel" in text
  beside a button that still says "Sign in".

## 2. The bar: what the peers do, per step

| Step | Best peer | What they do | Baaz today |
|---|---|---|---|
| First-run decision | synara, orca | stored marker scoped to the install; an existing workspace counts as onboarded; never decided from a live probe | right idea, broken by project adoption order |
| Agents in first run | bb, orca | "Connect a coding agent — you need one installed and signed in"; Continue disabled until one is ready, explicit Skip; Orca: "Detected on your system", pick a default | the same shape — and **the only one that also checks sign-in for all three**; nobody else does |
| PATH capture | synara, paseo | `$SHELL -ilc` with markers, `-lc` fallback, `launchctl getenv PATH`, known dirs; typed failure reasons; disk-cached by rc-file fingerprint | `-ilc` + path_helper + known dirs, 3 s, per process — adequate; no launchctl, no diagnostics |
| Binary resolution | paseo, orca | `which -a` + `--version` per candidate; a hung or non-zero binary is *present*, only ENOENT is absent; per-provider path override in Settings | same rule for absent; env-var override only |
| Version floors | t3code, superset | data: supported / graceful / unsupported bands; error names the upgrade command | none |
| Auth detection | t3code, orca | protocol-level: Codex `account/read`; Claude SDK init `tokenSource` (or `claude auth status` under a FIFO lock, synara) | `claude auth status --json` (good), Codex `account/read` (good but discarded on a rate-limit error), Muse from the live connection |
| Cache / single-flight | bb, synara | single-flight per provider; never cache a failure as "absent"; a timeout keeps the last good status; generation counters | no single-flight; whole-cache rewrites race |
| Re-probe triggers | synara | focus (15 s), entering the step, after sign-in, after the setup terminal closes | focus only (15 s) + Codex login |
| Install | bb, emdash | run the native installer in a visible PTY; **re-probe and verify instead of trusting exit 0** ("exited 0 but not found"); t3code: typed for review, Enter to run | typed into an invisible terminal; no verify |
| Sign-in | bb, orca | the CLI's own login in a visible PTY; URL / device code scraped into real buttons; completion by re-probe poll (3 s) or exit | Claude: invisible terminal; Codex: browser OAuth over app-server (good); Muse: device flow / API key over the wire (good) |
| Sign-out | t3code | typed `canLogout`; stops that provider's sessions | all three have it (Baaz leads) |
| Billing honesty | zed, synara | strip `ANTHROPIC_API_KEY` so the subscription wins, or say which credential is in force | Muse only |
| Settings card | t3code, orca | status + account + version advisory + path + override + diagnostics in one card | status + version only |
| Diagnostics | paseo | one copyable sheet: PATH source, which-matches, resolved path, version, raw auth output | none |
| Runtime auth failure | orca, bb | typed signals only (`authentication_failed`, 401 → `authRequired`) → inline Sign in | Muse only |
| Failed to launch | zed | exit status + drained stderr + Retry that resumes; input kept | inline "Couldn't start" + Retry (good) for new/reopen; nothing for a child that dies later |

Where nobody is good and Baaz can lead: sign-in checked during onboarding for every provider (only Baaz), sign-out for
every provider (only Baaz + t3code), installed · version · account · billing as one status line, re-probe on focus and
after every setup action, and saying which credential is billing.

Anti-patterns we will not copy: reading CLI-private keychain items or credential files (bb, superset, orca), scraping
"Login successful" (zed), "installed" meaning "configured" (zed, paseo), `unknown` shown as Connected (synara), four
different sign-in commands for one CLI (synara), plaintext secret files (all), Yolo/full-access default (orca, superset).

## 3. Target

Concepts a person holds (five): **provider**, its **status line**, its **one next action**, the **setup sheet** where
that action happens, and the **default provider**.

### 3.1 Detection (no screens; build now)

1. **One probe per provider at a time.** The live service owns an in-flight set; a refresh asked for while one runs
   is coalesced into one re-run after it. Background probes publish **only the providers they probed** into the live
   service, and the cache is written from the live service (a merge), so a Muse result can never be overwritten by a
   thread that started earlier.
2. **A status change wakes the window** over a channel (the terminal service's pattern), instead of waiting for a frame
   to notice the cache mtime.
3. **Codex:** `account/read` decides signed in / signed out on its own; `rateLimits/read` is best-effort and only
   feeds usage.
4. **Muse:** the live connection's answer is three-valued — not read yet (Unverified), signed out (**Signed out**),
   signed in. A signed-out Muse finally reads Signed out.
5. **Version floors as data** in the provider registry: Muse ≥ 1.2.1 (the seam's floor, `provider-muse/src/caps.rs`),
   Claude Code ≥ 2.1.276 and Codex ≥ 0.144.6 (the versions Baaz's adapters were probed and verified against, docs/23).
   Below the floor: `Advisory::TooOld` — advisory, never a block — with the provider's update command.
6. **Re-probe while a setup action is open:** after Baaz types an install or sign-in command, or opens a sign-in flow,
   that provider is re-probed every 3 s for up to 10 min, stopping when its headline changes. Install success is
   judged by the re-probe, never by an exit code.
7. **First run decided before the launch directory is adopted** (the boot fact, computed once).
8. **Default provider:** at the end of onboarding and at boot, if the current default is not Connected, the first
   Connected provider in registry order becomes the default; otherwise it is kept. Never "first enabled".
9. **One install/docs/update table** in `providers.rs`. Muse has **no** install command (none is documented; the
   muse.dev line is removed); its Not-installed action is **Get Muse** → https://dev.meta.ai/lp/muse-code (§6 Q4).
10. Codex sign-in, everywhere, is the one flow that keeps its app-server alive until `account/login/completed`, spawned
    with the login-shell PATH; Claude sign-out runs the resolved binary with the login-shell PATH.
11. Fixes: Muse Sign in from Settings and from the signed-out dialog raises the sheet (and keeps the open session).

### 3.2 Screens (mock first, owner approves, then build)

Per the 2026-10-09 process (mock on the Baaz v3 canvas from captures of the built screens, change only what is being
designed, owner approval, then a measured spec). Five boards:

1. **Connect your providers**, refined: the rows as today; a Not-installed Muse row says *how* to get Muse, once;
   the footer gains the default line — "New sessions start in **Claude Code** ⌄" — showing only Connected providers.
2. **Setup sheet** (new): a card over whatever screen asked (connect screen or Settings) holding a real terminal with
   the command typed, not run ("Review the command, then press Enter"), a status line at the top that follows the
   re-probe ("Waiting for sign-in…" → "✓ Connected · ada@… · Max"), and Done. One sheet for every Install, every
   Claude sign-in and every Update. Closing it does not cancel what runs in it.
3. **Muse sign-in as a real sheet**: the same card frame, close ✕ and Escape, no second mascot, copy that no longer
   says "Codex, Claude and more to come".
4. **Settings › Providers › <provider>** card: one status line ("Connected · ada@… · Max", or "API key · billed per
   token"), a **Details** disclosure — version (with "Update" when below the floor), path and how it was found,
   **Path…** override, **Copy diagnostics** (PATH source, every `which -a` hit, resolved path, version, raw auth
   output). Same actions as today.
5. **Signed out mid-session** (all three providers): the composer's quiet banner "Claude Code is signed out" with Sign
   in → the setup sheet; set from typed signals (Claude `authentication_failed`, Codex 401/`unauthorized`, Muse's
   account change) as well as a re-probe.

## 4. Build steps

Each ships alone and is gated by: `cargo test --workspace`, `cargo clippy --workspace --all-targets`, the perf budgets,
the 18-entry probe from CLAUDE.md, and a look at BaazCheck with a fresh state dir.

| # | What | Screens? |
|---|---|---|
| S1 | Safety + one table: drop the muse.dev install/docs, one install/docs/update/floor table in the registry, Muse row shows Docs once | copy only |
| S2 | Detection correctness: Codex `account/read` alone decides; Muse three-valued auth; version floors → TooOld | no |
| S3 | One probe at a time, publish-only-what-you-probed, cache merge, wake the window on change | no |
| S4 | First run before adoption; default provider from Connected | no |
| S5 | Muse Sign in from Settings / dialog raises the sheet; Codex sign-in one flow with login PATH; Claude logout via resolved path | no |
| S6 | Re-probe every 3 s while a setup action is open (≤ 10 min) | no |
| S7 | Probe entries for the uncovered states (all-not-installed, all-signed-out, unverified, can't run, TooOld, the Muse sheet over connect) | captures |
| D1 | Mock the five boards (§3.2) on the canvas → owner | **stop for approval** |
| B1–B5 | Build the approved boards | yes |

## 5. Not in this stage

Running installers on the person's behalf (docs/23 §5 stands: the command is typed, the person presses Enter);
multiple accounts per provider (T3/Superset do it with `CLAUDE_CONFIG_DIR` / `CODEX_HOME` per instance — later);
storing API keys in Baaz (every peer uses plaintext files; if Baaz ever holds one it goes in the Keychain); importing
native sessions (P7); a provider-version manifest fetched from the network.

## 6. Questions for the owner

1. **Setup sheet with a visible terminal, command typed and not run** (recommended; keeps docs/23 §5) — or should Baaz
   run the installer itself and verify by re-probe (bb, emdash)?
2. **Codex install command:** keep `npm i -g @openai/codex`, or the vendor's `curl -fsSL https://chatgpt.com/codex/install.sh | sh`
   (live, 200; t3code chose native installers so the CLI's own updater works), or `brew install codex` when Homebrew is present?
3. **API-key billing on Claude Code / Codex:** show it on the status line only (recommended), or also extend the
   pay-as-you-go send guard (docs/06 §4) to them, or blank `ANTHROPIC_API_KEY` when a subscription login exists (Zed,
   Synara)?
4. **Muse:** what is the real install instruction and docs link for a new user? (The `muse` on this Mac is Meta's
   launcher from `api.meta.ai/muse-launcher.sh`; nothing public documents installing it.) Until answered, the Muse row
   says only "Not installed" with no action.
5. **Codex sign-in alternatives:** add "Use a code instead" (device code, works when the browser callback can't reach
   localhost) and/or "Use an API key"? Recommended: device code yes, API key no (not asked for).

### Answers (owner, 2026-10-23)

1. **Visible setup sheet, command typed, not run.**
2. **Codex install stays `npm i -g @openai/codex`.**
3. **API-key billing: say it on the status line only** ("API key · billed per token"); no send guard, no env stripping.
4. **Muse:** no install command; the row and Settings link to **https://dev.meta.ai/lp/muse-code** ("Get Muse").
5. Not asked yet: device-code sign-in for Codex is drawn on the mock for the owner to keep or cut.

## 7. Progress

| Step | Commit | State |
|---|---|---|
| plan + S1 + S2 | `8cd0603` | built, gated |
| S3–S6 | `d8d46c6` | built, gated |
| S3 fix (idle frames) | `ac6b6d8` | built, gated |
| S7 (new probe entries) | — | not done: the uncovered states are drawn on the D5 board instead; entries follow the build |
| D1 mocks | canvas v58, board `D5Providers` | approved |
| D1 approval | — | owner approved canvas v58, 2026-10-24 |
| B1–B5 + S7 entries | `8c1493a` … `e134d36` | **built, gated, seen live; not pushed** — docs/67 |

Gates on `ac6b6d8`: `cargo test --workspace` 2,123 passed, 0 failed, 9 ignored; clippy clean; perf budgets: every
budget met except `idle.cpu_pct` 0.6 > 0.5, which measures the same on the P4 baseline `2262d34` (the known gpui
display-link wake on the tracker). S3 first added 3 idle frame requests (every probe landing woke the window; a
landing that changed only `checked_at` redrew the sidebar) — fixed in `ac6b6d8`, 0 again. Visual probe: the 18
entries + `right-files-scrolled`: 0 changed regions. `connect-first-run`, `connect-checking` and `launch-cached-shell`
differ from their 2026-09-28 baselines, which predate the v2/v3 redesign; the captures were looked at and read
right. Baselines not regenerated (that is a deliberate act on main, with the owner).

Live (BaazCheck, fresh `BAAZ_STATE_DIR`, Dock-like `env -i`, launched with `--workspace` on a scratch git repo — the
case that used to skip onboarding): the Connect your providers screen came up; all three real probes landed and
the rows filled in with the pointer parked (the S3 wake). Log: `first_run=true`, Muse `Installed · sign-in not
verified` → `Connected` once its connection answered (a fresh state has no cache to keep), Claude Code and Codex
Connected. No turn was sent.

Not seen live yet: the S5 Muse Sign in from Settings / the dialog (needs a signed-out Muse), Codex sign-in from
Settings (needs a signed-out Codex), the S6 re-check after an install (needs a missing CLI). Each is covered by a
unit test; each wants a hand check on a throwaway account or `CODEX_HOME`.

Two traps met: a gpui test window must not take status wakes (another test's probe thread wakes it → "Your test is
not deterministic"; tests get a closed channel); the Muse fallback-resolution test depended on being first to fill
the process-wide login-PATH cache (now resolves against an uncached one). Canvas CSS: never reuse short class names
(`.s .t .n .k .v .dl .sp .pick` collided with gen_d6) — the board's classes are `p5`-prefixed. `sips --cropOffset`
ignored its offset here; crop with PIL.
