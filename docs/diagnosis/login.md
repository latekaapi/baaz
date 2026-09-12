# Diagnosis — sign-in hangs on "Starting sign-in…" (2026-09-11)

Reported by the owner with a screenshot: the login card shows the spinner row
"Starting sign-in…" and never moves. Nothing was spent diagnosing this: every
probe below was `muse login` killed before approval, `muse schema`, and
`muse serve` driven through `initialize`, `account/*` and `model/list`.

## 1. Root cause: the harness parses the wrong stream of the wrong program

`crates/harness/src/auth.rs` spawns `muse login` with stdout → `/dev/null` and
parses **stderr** for `Open this page to sign in:` / `Confirm this code matches:`
/ `Waiting for approval (link expires in …)` / `Signed in.`. That is the
**launcher's** device-code flow (`~/.local/bin/muse`, a bash script, function
`device_login`), which only ever runs to authorise a *binary download*. The
`login` subcommand is `exec`'d straight through to the real binary
(`~/.local/bin/muse-bin-1.1.1-R2514.1`), and that binary:

- prints on **stdout**, not stderr;
- uses different wording: `confirm this code matches:` (lower-case), `Waiting for
  approval…` (Unicode ellipsis, no expiry), success is `Logged in. Credential
  saved to <path>.`, failures are `The login request was denied.` / `…expired
  before it was approved.` / `device login isn't available yet`, none prefixed
  `muse:`.

So no event ever reaches `on_login`, the state never leaves `Starting`, and the
child keeps polling until the code expires. Captured headlessly on this machine
(stdin null, stderr not a tty, `MUSE_LOGIN=1`):

```
stdout:  Open this page to sign in:
           <URL>
         confirm this code matches:
           <CODE>

         Waiting for approval…
stderr:  (empty)
```

Two smaller faults found on the way:

- The "live" half of the boot probe is meaningless. `model/list` answers
  `source: "providerCatalog"` **while logged out** (verified), so only the
  `auth.json` half ever decided anything.
- There is no API-key path at all. The CLI has one: `muse auth set
  --api-key-stdin`, and `META_API_KEY` in the environment.

## 2. What the CLI actually offers (1.1.1)

| surface | what it does |
|---|---|
| `muse login` | RFC 8628 device code against `auth.meta.com`; saves `providers.meta` with `mechanism`, `access_token`, `refresh_token`, `expires_at`, `user_full_name`, `user_email`. Subscription-tier lane. |
| `muse auth set --api-key-stdin` | stores a Meta API key (`providers.meta.api_key`); validates it ("Model API access verified." / "META_API_KEY was rejected"). Pay-as-you-go lane. |
| `META_API_KEY` env | overrides both; `muse logout` does not touch it. |
| `muse logout` | removes the stored credential of either kind. |
| TUI onboarding | states `env_api_key`, `stored_api_key`, `unsaved_api_key`, `subscription`, `payment_banner`; a masked key field with "Enter validate & save · Esc back". |

## 3. The wire has an account API (experimental surface)

`muse schema generate-json-schema --experimental` exports, and a free
`muse serve` run confirms, six account operations gated on
`initialize.capabilities.experimentalApi: true` (without it every one answers
`-32601` with `data.kind: "experimentalRequired"`):

| method / notification | shape |
|---|---|
| `account/read` → `AccountState` | `{state: loggedOut\|envKey\|apiKey\|accountLogin, label?, credentialRequired}` |
| `account/loginStart {type: deviceCode}` → `{verificationUrl, userCode}` | starts the device flow, host-owned; supersedes a pending one |
| `account/loginStart {type: apiKey, apiKey}` → `{}` | stores and validates the key synchronously |
| `account/loginCancel` → `{cancelled}` | abandons the pending device code |
| `account/logout` → `AccountState` | clears the stored credential |
| `account/loginCompleted {outcome: granted\|denied\|expired\|cancelled\|failed, message?}` | terminal outcome; a granted flow's `account/changed` follows |
| `account/changed` = `AccountState` | any change, including a terminal `muse logout` |

Observed on the wire: `account/read` while logged out → `{"state":"loggedOut",
"credentialRequired":true}`; `loginStart deviceCode` returns the URL and code
in the **result** (never in a notification); `loginCancel` → the
`loginCompleted {cancelled}` notification arrives **before** the cancel result;
`loginStart apiKey` with an empty key → `invalidParams` "the apiKey login type
requires a non-empty apiKey member". Diffing the stable and experimental
bundles: the experimental surface adds **only** the eight `Account*` types, the
four methods and the two notifications — no existing type gains a field or a
variant, so opting in is safe for the mirror in `schema.rs`.

## 4. Decisions

- **D22. Sign-in moves onto the wire.** The harness sets `experimentalApi: true`
  at `initialize` and drives `account/*`. The stderr parser, `MUSE_LOGIN=1`, the
  `muse login`/`muse logout` children and the `model/list` half of the probe are
  deleted. `auth.json` is still read, only to supplement the identity with
  `user_full_name` / `user_email` when the wire's `label` is absent.
- **D23. One screen, two methods.** The idle state of the login screen is the
  method choice: *Continue with Meta account* (primary; the subscription lane)
  and *Use an API key* (the pay-as-you-go lane), with one line saying which
  bills what. Each method has its own states after that.
- **D24. The key is never held.** The API-key field is a masked single-line
  input (gpui-base `InputState::masked`, reveal toggle). On submit the text is
  read once, trimmed, sent in `account/loginStart`, and the field is cleared
  when the call returns. The key never lands in `Harness`, a log, a screenshot
  argument or a fixture.
- **D25. No reconnect after a granted login.** The flow is host-owned, so the
  `muse serve` that ran it holds the credential. The app proceeds on
  `account/changed` reporting a signed-in lane. Verified live 2026-09-12: the
  Meta-account login proceeded with no reconnect (the owner's billed turn),
  so `Harness::reconnect_after_login` is deleted.
- **D26. The browser opens once, automatically,** when the device state is
  entered; "Open in browser" re-opens it.
- **D27. `model/list` is not a sign-in signal.** `account/read` is the only probe.
- **D28. `envKey` cannot be signed out from the app.** The footer names it
  "API key (environment)"; Sign out explains that `META_API_KEY` must be unset.
- **D29. Spec §3.2 is superseded** by this document. The spec stays frozen; a
  one-line pointer is added at §3.2 and the behaviour is documented in
  `docs/02-app.md`, `docs/01-transport.md` §4 and `docs/06-billing.md`.

## 5. Screens

| state | what is drawn | actions |
|---|---|---|
| Choose | masthead; one muted line on billing | *Use an API key* · **Continue with Meta account** |
| Starting | spinner "Starting sign-in…" | *Cancel* |
| Device | URL row (globe button), the code, spinner "Waiting for you to finish in the browser…", hint "Approve the request in your browser, then come back here." | *Cancel* · *Copy code* · **Open in browser** |
| ApiKey | label "Meta API key", masked field with reveal toggle, hint "Saved by the muse CLI to ~/.config/muse/auth.json. Never logged.", optional inline error | *Back* · **Sign in** (disabled while empty; Enter submits) |
| Validating | spinner "Checking the key…" | — |
| Success | "Signed in." | — |
| Error | message in the attention border | *Choose another way* · **Try again** |

Signed in: the footer shows the label from `account/read` (or the `auth.json`
name and email), and for the `apiKey` / `envKey` lanes the plan row reads
"Pay-as-you-go · API key" without running the TUI tier probe.

## 6. Packages

1. `docs/briefs/muse-lib-login-methods.md` — agentic-ui: `LoginState`/`LoginIntent`
   extended, `secret_field`, gallery, api-doc. Branch `login-methods` off the
   checked-out `improvements-2026-09-10` (the harness builds against it).
2. `docs/briefs/muse-harness-login-wire.md` — harness: schema types, client
   methods, `experimentalApi`, the flow, captures, docs, fixture.

## 7. Test log (2026-09-11/12)

First-time-user runs, headless, from a machine signed out with `muse logout`:

| lane | steps | result |
|---|---|---|
| API key | `--login-steps 'apikey;key-from-env:MUSE_TEST_KEY;submit'` | `choose → apikey → validating → success`, `account → apiKey`; shell with footer "API key / Pay-as-you-go · API key"; `account/read` on a fresh `muse serve` → `apiKey` / "stored key" (the credential is in the keychain; `auth.json` holds only `storage`) |
| API key, turn | `--tier subscription --send 'Reply with exactly the word OK…'` | "OK", 6.6 s, 24.2k tokens, `muse-spark-1.3-contributor` |
| API key, guard | `--send` without `--tier` | the pay-as-you-go banner intercepts the send (correct) |
| Meta account | `--login-steps account`, windowed | `starting → device`, browser opened; three codes expired unapproved after 611 s (`loginCompleted → expired: login failed: the request expired`); approval not yet exercised |

Wire facts learned: an unattended device code expires after 611 s with
outcome `expired`; `--screenshot-delay` is milliseconds (a capture at 20 ms
shows the card mid-fade and precedes the probe's answer — that, not executor
starvation, was the first "steps never ran" symptom).

Hazard recorded: a brief that lets Muse call `account/logout` or `muse logout`
signs out the machine its own run depends on.
