# 23 — Connecting and managing providers

Status: design, 2026-09-28; Y5 (2026-09-28) implements §3's launch and §4's
first bullet — the Connect your providers screen, the stored-facts launch
decision, the quiet sign-out banner, and the row actions below. The rest
(Settings cards, footer, usage card) stays design.
Muse names no Install command: no installer is documented in this repo or in
`muse --help`, so its missing row offers Docs (this repo) instead.
Research: T3 Code
(`pingdotgg/t3code`, `apps/server/src/provider/Layers/*Provider.ts`, `components/onboarding/*`,
`settings/ProviderInstanceCard.tsx`) and Synara (`Emanuele-web04/synara`, MIT, T3-derived:
`ProviderHealth.ts`, `OnboardingProvidersStep`, `providerSetupStatus.ts`). Backend surfaces below were
probed on this machine (claude 2.1.276, codex-cli 0.144.6, Muse Code 1.4.0).

## 1. Why

Baaz drives three backends, but the app still opens on "Sign in to Muse" and flashes it whenever
Muse's `account/read` is slow. A person who uses only Claude Code or Codex is gated behind a Muse login;
nothing tells them whether `claude` or `codex` is installed or signed in; and the footer shows a Muse-only
meter and a raw account id.

## 2. The model: a provider status, probed in the background, cached on disk

One `ProviderStatus` per provider (Muse, Claude Code, Codex):

    installed: Yes{version, path} | No | Unknown
    auth:      SignedIn{email?, plan?, method} | SignedOut | Unverified | Unknown
    enabled:   bool (the person's switch; default true)
    advisory:  None | TooOld{need} | CantRun{stderr}
    checked_at

Headline state, first match wins: **Checking** (no probe result yet this launch and no cache) →
**Disabled** → **Not installed** → **Can't run** → **Signed out** → **Installed · sign-in not verified** →
**Connected · email · plan**. A timeout reads "Couldn't check — Re-check", never "Not installed".

Probes (all free, none sends a model turn):

| provider | installed/version | auth | sign in | sign out |
|---|---|---|---|---|
| Claude Code | `claude --version` | `claude auth status --json` → `loggedIn, authMethod, email, orgName, subscriptionType` | open Baaz's terminal with `claude auth login` typed, not run | `claude auth logout` |
| Codex | `codex --version` | short-lived `codex app-server`: `initialize`, `account/read` → `{account: apiKey \| chatgpt{email,planType}, requiresOpenaiAuth}`, `account/rateLimits/read` | in-app: `account/login/start {type: chatgpt}` (open `authUrl`), `{type: chatgptDeviceCode}`, or `{type: apiKey}`; completes on `account/login/completed` | `account/logout` |
| Muse | `muse --version` | the existing connection's `account/read` | the existing Meta-account / API-key flow (today's login screen becomes Muse's sign-in sheet) | existing `account/logout` path |

The status is written atomically to `<state>/provider-status.json` after every probe, and read at boot.
Probes run in parallel at launch (timeouts: 8s; Codex app-server 12s), again when the window regains
focus (at most every 15s), and on the person's Re-check. `BAAZ_DETERMINISTIC=1` never reads or writes the
cache and never probes (captures use scripted statuses).

## 3. Launch without a flash

- **First run** is decided only from stored facts: an `onboarding_completed` flag in the state dir, or any
  existing session/project. Never from a live probe.
- Not first run → render the shell immediately from the cached statuses. A provider whose cache says
  Connected is usable at once; if its probe later says Signed out, show a quiet banner on that provider's
  composer and its Settings card — never a full-screen gate.
- First run → the **Connect your providers** screen (§4), rendered from "Checking…" rows that fill in as
  probes land. Never shows "Not installed" while a probe is pending.
- The Muse `account/read` no longer gates the window. Muse sessions wait on Muse's own connection the way
  provider sessions wait on their child.

## 4. Surfaces

**Connect your providers** (first run; also reachable from Settings → Providers → "Set up providers"):
the Baaz mark, one line of intro, a tally ("1 connected · 1 needs sign-in · 1 not installed"), and one row
per provider — mark, name, state headline, account line, one primary action (Install / Sign in /
Re-check / Connected ✓) and a secondary "Docs". **Continue** enables once one provider is Connected;
"Skip for now" always works. Install opens the dock terminal with the vendor's install command typed and
not run (Claude: `curl -fsSL https://claude.ai/install.sh | bash`; Codex: `npm i -g @openai/codex`;
Muse: its documented installer), and says so.
(Y5: no Muse installer is documented anywhere this repo or `muse --help`
can point at, so the Muse row offers Docs instead of Install; the Claude
and Codex commands above are exact.)

**Settings → Providers**: one card per provider — status dot + headline, "Signed in as <email> · <plan>",
version (with a Too-old advisory), Enabled switch, Re-check, Sign in / Sign out (sign out confirms).
Disabling hides the provider from the composer's provider menu and stops its probes; its sessions stay
listed and readable. Sign-out of a CLI is a real `logout` of that CLI and the confirm says so.

**Account footer** (sidebar bottom): the person's avatar initial and email only — no account id, no inline
meter. Its menu opens a **Usage** card listing every Connected provider: Codex from `account/rateLimits/read`
(primary/secondary windows labelled from `windowDurationMins`, used %, reset time; live via
`account/rateLimits/updated`), Claude Code from the last rate-limit event a turn reported ("Not reported yet"
until one has), Muse from what it reports today. Each row: bar, "resets in …", "as of …", warning from 80%.

## 5. What this does not do

No account switching inside a provider (T3's answer is per-instance config dirs; later). No installing on
the person's behalf (the command is typed, the person presses Enter). No provider beyond the three.

## 6. Build order

1. Library (aui): a provider card row for Settings and a connect screen component, both with roles/labels;
   a usage card. Additive, tagged.
2. Baaz: the status service + cache + probes + focus refresh (no UI change yet; unit-tested on scripted
   command output).
3. Baaz: first-run screen, launch-without-flash, Muse login demoted to a sheet.
4. Baaz: Settings → Providers, sign in/out actions, enabled switch gating the provider menu.
5. Baaz: footer + usage card.
