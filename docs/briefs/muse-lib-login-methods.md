# Brief — sign-in screen with two methods, and a masked secret field (library package)

Repository: `/Users/latekaapi/Projects/agentic-ui`. First run
`git switch -c login-methods` from the branch that is checked out
(`improvements-2026-09-10`; the harness builds against it, so do **not** branch
from `main`). Work on `login-methods` and commit on it when the gates pass,
message ending `Co-Authored-By: Muse Code <noreply@meta.com>`. Do not touch
`/Users/latekaapi/Projects/harness` or `~/Projects/cockpit`. Prefix every shell
command with `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`.

Read first: `docs/00-agent-brief.md`, `docs/04-design-rules.md`,
`crates/aui/src/screens/login.rs`, `crates/aui-gallery/src/cards/login.rs`,
`crates/aui/src/nav/session_row.rs` (`dense_field`, the one existing wrapper of
a gpui-base text state). No literal colours, sizes or durations outside the
named constants at the top of a file; stateless `RenderOnce` components with
intents out; both themes.

## Why

The Muse CLI signs in two ways: a Meta-account device-code flow (the
subscription lane) and an API key (pay-as-you-go). The library's sign-in
screen only knows the device flow and has no way to take a key. The harness
will drive both over the wire; the library owns how they look.

## 1. `crates/aui/src/data/secret_field.rs` — a masked single-line field

gpui-base has a single-line `InputState` (`gpui_kit::base::input::{Input,
InputState}`, `InputBaseState<InputMode>`) with `.masked(bool)` /
`set_masked` / `is_masked`; the multi-line `TextareaState` has no masking, so
a secret needs the single-line kind. Add:

```rust
pub fn secret_field(id: impl Into<ElementId>, state: &Entity<InputState>) -> SecretField
```

- Draws the caller's `Input` inside the library's bordered control box (same
  metrics as the login screen's URL row: 34 px tall, 6 px radius, surface-2,
  hairline border, mono text) with a trailing ghost `icon_button` (eye / eye-off,
  add the icons to `aui-icons` if missing) that emits `.on_toggle_reveal(f)`.
  The field reads `state.read(cx).is_masked()` to pick the icon; it never
  changes the state itself — the caller flips `set_masked`.
- `.placeholder(...)` passes through; `.disabled(bool)`; a focus ring on the
  wrapper per the design rules (the ring lives on the box, not the input).
- Export from `aui::data`. Rustdoc says what it is for and that the text
  inside is the caller's secret: the component never logs, copies or formats
  it.

## 2. `crates/aui/src/screens/login.rs` — two methods

Replace `LoginState::Idle` with a method choice and add the API-key states.
Keep every existing constant and row helper; the card stays 420 px.

```rust
pub enum LoginMethod { Account, ApiKey }

pub enum LoginState {
    /// Nothing started: choose a method.
    Choose,
    Starting,                                   // device flow starting (unchanged)
    Device { url, code, expires: Option<SharedString>, waiting: bool },   // unchanged
    /// The API-key form. The field itself is the caller's (`.api_key_field`).
    ApiKey { can_submit: bool, error: Option<SharedString> },
    /// `account/loginStart {apiKey}` in flight.
    Validating,
    Success,
    /// `method` says which method failed, so "Try again" restarts that one.
    Error { message: SharedString, method: Option<LoginMethod> },
}

pub enum LoginIntent {
    StartAccount,      // Choose → device flow
    UseApiKey,         // Choose → ApiKey form
    SubmitApiKey,      // ApiKey → Validating
    ToggleReveal,      // eye button in the ApiKey form
    Back,              // ApiKey → Choose
    OpenBrowser, CopyCode,
    Retry,             // Error → the method in `method`, or Choose when None
    ChooseAnother,     // Error → Choose
    Cancel,            // Starting / Device → Choose
}
```

Builder: `.api_key_field(impl IntoElement)` — the element the ApiKey state
draws in its field slot (the harness passes a `secret_field`). Drawn rows:

| state | rows below the masthead | action row (hint · buttons, primary last) |
|---|---|---|
| Choose | one muted centred line: "A Meta account draws on your Muse subscription. An API key bills usage to that key." | — · *Use an API key* · **Continue with Meta account** |
| Starting | spinner "Starting sign-in…" | — · *Cancel* |
| Device | URL row, code line, spinner "Waiting for you to finish in the browser…" when `waiting` | `expires` or "Approve the request in your browser, then come back here." · *Cancel* · *Copy code* · **Open in browser** |
| ApiKey | field label "Meta API key" (11 px, ink-3), the caller's field, muted hint "Saved by the muse CLI to ~/.config/muse/auth.json. Never logged.", `error` in the attention border when present | — · *Back* · **Sign in** (disabled unless `can_submit`) |
| Validating | spinner "Checking the key…" | — |
| Success | unchanged | — |
| Error | message box | — · *Choose another way* · **Try again** |

Enter inside the field is the caller's business (gpui-base
`set_submit_on_enter` + its event); Escape is the caller's too. Update the
doc comment example and the `headline_defaults_to_the_product` test.

## 3. Gallery

`crates/aui-gallery/src/cards/login.rs` becomes a multi-state card (follow
whichever existing card shows several states with a legend line): Choose,
Device (the existing one), ApiKey with a sample field that has masked text
in it and the reveal toggle, ApiKey with an inline error, Error with both
buttons. Add a `data/secret_field` entry (masked and revealed). Both themes
in the capture. Regenerate `docs/06-api.md` with `python3 scripts/api-doc.py`.

## Gates, all must pass

`cargo build --workspace`;
`cargo build --workspace --features aui-webview/wry,aui-terminal/pty,aui-terminal/tui`;
`cargo test --workspace`; `cargo clippy --workspace --all-targets -- -D warnings`;
`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`;
`python3 scripts/api-doc.py`. Take the gallery captures the way the gallery's
own docs say (both themes) and name their paths. Report files changed, the
API added or renamed, gate results verbatim. Never claim a gate you did not run.
