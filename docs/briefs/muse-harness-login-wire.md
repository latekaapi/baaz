# Brief — sign-in over the wire: Meta account and API key (Harness)

You are the single implementor for this package in
`/Users/latekaapi/Projects/harness` (branch `main`). Do NOT commit; leave the
tree uncommitted for review. Do not touch `/Users/latekaapi/Projects/agentic-ui`
(it already has what you need on its checked-out branch `login-methods`) or
`~/Projects/cockpit`. Prefix every shell command with
`export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`.

## Spend rule — non-negotiable

Every model turn is billed. Never run the ignored live tests
(`crates/muse-client/tests/live.rs`), `harness-probe`, the
`fixtures/msp/probe*.py` scripts, `--send`, or `--steps` containing
`send:`/`steer:`. Never call `turn/start`. Free and allowed: `muse schema …`,
`muse serve` driven only through `initialize`, `account/*`, `session/list`,
`model/list`; `--replay`; `--no-connect`; `cargo build/test/clippy/doc`. Do
**not** complete a device-code login and do not run `account/loginStart` with a
real key: start the device flow and cancel it, nothing more.

**Never call `account/logout`, run `muse logout`, or touch `~/.config/muse`.**
The machine is signed in on the API-key lane and *that credential is the one
your own model calls use*: a previous run of this brief logged the machine out
while recording the fixture and died with "missing meta credentials" on its
next step. The `account/logout` shape is covered by its result type being
`AccountState`, the same as `account/read`; no capture of it is needed.

## Read first

`docs/diagnosis/login.md` (the diagnosis, the wire facts and decisions
D22–D29 — this brief implements it), `docs/05-handoff.md`, `docs/02-app.md`
§auth, `docs/01-transport.md` §4, `docs/06-billing.md`,
`crates/harness/src/auth.rs`, `crates/harness/src/conn.rs`,
`crates/harness/src/app.rs` (`Auth`, `Login`, `probe_catalog`, `start_login`,
`on_login`, `reconnect_after_login`, `logout`, `render_login`,
`render_footer`, `render_account_menu`, the `SessionEvent::SignedOut` arm,
`DialogAction::SignIn`, `route`), `crates/muse-client/src/{client,schema}.rs`,
`crates/muse-client/tests/schema_roundtrip.rs`, and in agentic-ui (read-only)
`crates/aui/src/screens/login.rs` and `crates/aui/src/data/secret_field.rs`.

## 1. `muse-client`: the account surface

1. Export the experimental bundle next to the stable one:
   `muse schema generate-json-schema --out fixtures/msp/msp-experimental --experimental`
   (and the `-ts` twin into `fixtures/msp/msp-ts-experimental`). The stable
   exports and `SCHEMA_FINGERPRINT` do not change (the fingerprint covers the
   stable surface only; verified).
2. `schema.rs`: mirror the eight `Account*` types (`AccountState`,
   `AccountStateKind`, `AccountLoginType`, `AccountLoginStartParams`,
   `AccountLoginStartResult`, `AccountLoginOutcome`,
   `AccountLoginCompletedParams`, `AccountLoginCancelResult`) in the file's
   conventions. `AccountStateKind` and `AccountLoginOutcome` are **open**
   server vocabularies: give them an `#[serde(other)] Unknown` arm like the
   file does for other open enums. `AccountLoginStartParams` must have a
   hand-written `Debug` that prints `apiKey: <redacted>`; add a unit test for it.
   Add the four methods and two notifications to the dispatch table the
   round-trip test reads.
3. `client.rs`: `account_read`, `account_login_start`, `account_login_cancel`,
   `account_logout`. Rustdoc on each: requires `experimentalApi: true` at
   `initialize`, else `-32601` / `data.kind: "experimentalRequired"`.
4. A recorded fixture `fixtures/msp/transcript-account.jsonl`, captured from a
   free `muse serve` run: `initialize` (with the opt-in), `initialized`,
   `account/read`, `account/loginStart {deviceCode}`, `account/loginCancel`,
   the `account/loginCompleted {cancelled}` notification, and one
   `account/read` **without** the opt-in on a second connection
   showing the `experimentalRequired` error. Look at an existing
   `fixtures/msp/transcript-*.jsonl` for the capture format and at
   `fixtures/msp/drive.py` for how captures are driven. **Before saving,
   replace the real verification URL with `https://example.invalid/device`
   and the user code with `XXXX-XXXX`** — they are secrets for the life of
   the flow and must not be checked in. The round-trip test must pass over it.

## 2. `conn.rs`

`ClientCapabilities { experimental_api: Some(true), requested_capabilities:
Some(vec!["userShell"]), .. }`. Rustdoc why (D22). `looks_like_signed_out`
stays.

## 3. `auth.rs`

Delete `LoginEvent`, `spawn_login`, `logout`, `Want`, `expiry`, `strip_sgr`
and their tests. Keep `auth_path`, `open_in_browser`, and reduce `identity()`
to `stored_name_and_email() -> Option<(String, String)>` read from
`auth.json` — a supplement only. Add:

```rust
pub struct Identity { pub lane: AccountStateKind, pub name: String, pub email: String }
impl Identity {
    /// From the wire's state plus the stored name/email when `label` is absent.
    pub fn from_account(state: &AccountState) -> Option<Identity>   // None for loggedOut
    pub fn initial(&self) -> String
    /// "API key" / "API key (environment)" / the name
    pub fn footer_name(&self) -> String
    pub fn is_api_key(&self) -> bool                                 // apiKey | envKey
}
```

Module docs rewritten: the wire owns sign-in now; `auth.json` is read-only and
only for the two display strings; the URL, the code and the key never reach a
log.

## 4. `app.rs` — the flow (decisions D22–D28)

- `Auth::Probing` now means "waiting for `account/read`". `probe_catalog` is
  replaced by `probe_account`: call `account/read` after connect; `loggedOut`
  → `SignedOut`; anything else → `SignedIn(Identity)`, then `load_sessions`
  and, for `accountLogin` only, `probe_tier(false)`. For `apiKey`/`envKey`
  set `self.tier = Some(Tier::PayAsYouGo)` without probing (the TUI probe is
  about subscriptions) and push it the way `probe_tier` does.
- `Login` struct: `state: LoginState`, `url`, `code`, `method:
  Option<LoginMethod>`, `api_key: Entity<InputState>` (created once in
  `Harness::new`, `masked(true)`, placeholder "Paste your key",
  submit-on-Enter → `SubmitApiKey`), `revealed: bool`. No task field.
- `LoginIntent` handling in `render_login`'s listener:
  - `StartAccount`: state `Starting`; background `account_login_start
    {deviceCode}`; on result set `url`/`code`, state `Device { expires: None,
    waiting: true }`, and call `auth::open_in_browser(&url)` once (D26). On
    error → `Error { method: Some(Account) }`.
  - `UseApiKey`: state `ApiKey { can_submit: false, error: None }`, focus
    the field. `can_submit` tracks the field's non-empty trimmed text
    (subscribe to the state's change event, or recompute in render).
  - `SubmitApiKey`: read the text, trim; if empty do nothing; state
    `Validating`; background `account_login_start {apiKey}`; **clear the
    field when the call returns, whatever it returned** (D24). The key is a
    local in the task closure and nowhere else. An `invalidParams` or other
    error → `ApiKey { error: Some(message) }`.
  - `ToggleReveal`: flip `set_masked` on the state.
  - `Back`, `ChooseAnother`: state `Choose`, clear the field.
  - `Cancel`: background `account_login_cancel`; state `Choose` immediately
    (the `cancelled` notification is ignored once already in `Choose`).
  - `OpenBrowser`, `CopyCode`: as today. `Retry`: re-run `method`, or `Choose`.
- Notifications, in `route` before the session view sees anything, keyed on
  `method.starts_with("account/")`:
  - `account/loginCompleted`: `granted` → `LoginState::Success`; `denied` /
    `expired` / `failed` → `Error { message: message or a fixed sentence per
    outcome, method: self.login.method }`; `cancelled` → `Choose` unless
    already there. For the API-key method `failed` goes to `ApiKey { error }`
    instead, so the person can fix the key.
  - `account/changed`: rebuild `Auth` from it exactly as `probe_account`
    does. Signed-in lane while on the login screen → enter the app (no
    reconnect, D25; keep `reconnect_after_login` compiled but unused, with a
    doc comment saying why it is kept). `loggedOut` while signed in → clear
    `active`/`sessions`, `Auth::SignedOut`, `Choose`, and the existing
    "Signed out of Muse" dialog with detail "The credential was removed
    outside the app."
- Sign out: `account_logout` in the background; the result is an
  `AccountState` — apply it like `account/changed`. If the lane is still
  `envKey`, show a toast "META_API_KEY is set in the environment; unset it
  and relaunch to sign out." (D28). The account menu row reads "Sign out"
  for stored lanes and "Sign out (set by META_API_KEY)" for `envKey`.
- Footer: `footer_name()`; email only when present; plan row "Pay-as-you-go
  · API key" (warning tint, as `is_warning` already says) for the key lanes.
- Escape on the login screen: `Cancel` in `Starting`/`Device`, `Back` in
  `ApiKey`, `ChooseAnother` in `Error`. Nothing in the other states.
- `SessionEvent::SignedOut` and `DialogAction::SignIn` keep working; they go
  to `Choose`.
- `--replay` and `--no-connect` boot paths are unchanged, except that
  `--no-connect` now honours a new `--login <state>` for captures:
  `choose` (default), `device`, `apikey`, `apikey-error`, `validating`,
  `error`. Sample data for captures: URL `https://example.invalid/device`,
  code `WXYZ-2946`, and for `apikey` put a sample string in the field so the
  mask shows dots. Document the flag in `main.rs` next to `--tier`.

## 4b. Scripted login steps, for an end-to-end test with no pointer

Add `--login-steps <a;b;c>` (honoured only when the app is really connected,
never with `--no-connect`/`--replay`), run once the login screen is up, one
step per item, same parsing as `--steps`:

| step | what it does |
|---|---|
| `account` | `LoginIntent::StartAccount` (the device flow; the browser opens) |
| `apikey` | `LoginIntent::UseApiKey` |
| `key-from-env:<VAR>` | put the value of environment variable `VAR` into the API-key field. The value never appears in argv, a log or a screenshot argument; if `VAR` is unset the step fails with a stderr line naming the variable, not its value |
| `submit` | `LoginIntent::SubmitApiKey` |
| `wait:<ms>` | as in `--steps` |

Print one stderr line per login-state transition, `harness: login → <state
name>` and `harness: account → <lane>`, with no URL, code or key in it, so a
headless run can be followed from a log. After sign-in the ordinary `--steps`
run as today, so `--login-steps 'apikey;key-from-env:MUSE_TEST_KEY;submit;wait:8000'
--screenshot …` captures the signed-in shell on the API-key lane.

## 5. Docs and captures

- `docs/CHANGELOG.md`: a dated entry at the top: what was wrong (one
  paragraph, cite `docs/diagnosis/login.md`), what changed, spend (must be
  zero; count it as `docs/05-handoff.md` says and print the number).
- `docs/02-app.md` §auth rewritten for the wire flow and the two methods;
  the `--login` flag in the flags table.
- `docs/01-transport.md` §4: add the account wire facts from
  `docs/diagnosis/login.md` §3 (gating error, result-only URL/code, the
  cancel notification preceding the cancel result, empty-key
  `invalidParams`).
- `docs/06-billing.md`: a short section "The API-key lane": stored key or
  `META_API_KEY` = pay-as-you-go by construction; the footer says so without
  a probe.
- `docs/00-spec.md` §3.2: one line at the top of the section:
  "Superseded 2026-09-11 by docs/diagnosis/login.md (D22–D29)." Change nothing
  else in the spec.
- `docs/07-architecture.md`: `auth.rs`'s one-line description.
- `README.md`/`docs/02-app.md`: the `--login-steps` table.
- Captures into `docs/images/login-*.png`, both themes, 15 s delay:
  `choose`, `device`, `apikey`, `apikey-error`, `error` — e.g.
  `cargo run -p harness -- --no-connect --login apikey --theme dark --screenshot-delay 15 --screenshot docs/images/login-apikey-dark.png`
  (check `main.rs` for the exact flag names). List every path in the report.

## Gates, all must pass

`cargo build --workspace`; `cargo test --workspace`;
`cargo clippy --workspace --all-targets -- -D warnings`;
`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`;
`cargo tree -d` shows exactly one `gpui-pre` and one `gpui-kit`;
`UPDATE_SNAPSHOTS=1 cargo test -p muse-adapter` then read the diff (there should
be none). Report files changed, the wire methods now used, gate results
verbatim, the spend count, and the capture paths. Never claim a gate you did
not run.
