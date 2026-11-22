//! The login screen and the `account/*` lane behind it.
//!
//! Sign-in is on the wire: Baaz
//! sets `experimentalApi: true` at `initialize` and drives `account/*`
//! itself, so there is no stderr parser, no `muse login` child and no
//! `MUSE_LOGIN` any more. `auth.json` is still read, but only to supplement
//! the identity with a name and an email when the wire's label is absent.
//!
//! # The flow
//!
//! One screen with two methods (**D23**): *Continue with Meta account*, the
//! subscription lane, and *Use an API key*, the pay-as-you-go one. The idle
//! state is that choice; each method has its own states after it.
//!
//! * [`Harness::probe_account`] asks `account/read`, which is the **only**
//!   sign-in signal — `model/list` answers from the provider catalog while
//!   logged out, so it never was one (**D27**).
//! * [`Harness::apply_account`] turns one account reading (lane and label)
//!   into [`Auth`], and
//!   the probe answer and the `account/changed` notification share it exactly.
//! * [`Harness::login_intent`] is where every button, the Escape walk and the
//!   `--login-steps` verbs arrive.
//! * The device flow ([`Harness::start_device_flow`]) opens the browser once,
//!   on entering the device state (**D26**); the URL and the code come back in
//!   the `account/loginStart` result, never in a notification.
//! * The API-key form ([`Harness::submit_api_key`]) reads the field once,
//!   trims it, sends it and clears the field when the call returns: the key
//!   never lands in [`Harness`], a log, a screenshot argument or a fixture
//!   (**D24**).
//! * A granted login needs **no reconnect** (**D25**): the flow is host-owned,
//!   so the `muse serve` that ran it already holds the credential and the app
//!   proceeds on the signed-in `account/changed`. Verified live with a billed
//!   turn after a Meta-account login on 2026-09-12.
//! * [`Harness::logout`] signs out over the wire; an `envKey` lane survives it
//!   (the environment still holds the key), so that case keeps the shell and
//!   explains itself in a toast (**D28**).
//!
//! Nothing here logs a URL, a code or a key: the `baaz: login → …` lines
//! carry the state name and nothing else, which is what lets a headless
//! `--login-steps` run be followed from a log.

use aui::data::secret_field;
use aui::overlay::DialogKind;
use aui::screens::{login, LoginIntent, LoginMethod, LoginState};
use gpui::{prelude::*, Context, Entity, Focusable as _, SharedString, Window};
use gpui_kit::base::input::InputState;
use provider::{AccountLane, Ack, Command, LoginOutcome, ProviderError};

use crate::app::Harness;
use crate::auth::{self, Identity};
use crate::overlays::{Dialog, DialogAction};
use crate::tier::Tier;
use crate::wire::WireCall;
#[cfg(feature = "probe")]
use crate::LoginSample;

/// Where the boot probe got to: sign-in is on the wire now,
/// so this is the `account/read` answer.
pub(crate) enum Auth {
    /// Waiting for `account/read`.
    Probing,
    /// `loggedOut`: the login screen.
    SignedOut,
    /// Any other lane, with the wire's identity. Nothing reads the identity
    /// since the sidebar footer moved to provider statuses: its last reader
    /// was the sidebar's frame key, deleted in task 4.7. Kept rather than
    /// unpicked here — dropping it ripples through the login and auth
    /// reads — and named as a follow-up.
    SignedIn(#[allow(dead_code)] Identity),
}

/// The sign-in flow's own state, an entity (docs/82 4.9c): the screen itself
/// is stateless (it is the library's `aui::screens::login`); this owns the
/// [`LoginState`], the device flow's URL and code, which method is running,
/// and the API-key field. The Muse sheet ([`MuseSheetView`]) reads it and
/// redraws on its notify; the root only drives the flow (the `account/*`
/// lane is the root's). There is no task field: every wire call runs on the
/// background executor and returns through `update`, like every other
/// command.
pub(crate) struct Login {
    state: LoginState,
    url: Option<String>,
    code: Option<String>,
    method: Option<LoginMethod>,
    /// The masked API-key field, created once in [`Harness::new`]. The key
    /// text is read once on submit and the field is cleared when the call
    /// returns; the key never lands in `Harness`, a log, or a fixture.
    api_key: Entity<InputState>,
    /// Mirror of the field's masked flag, kept in sync by both toggle paths
    /// (the eye button and `ToggleReveal`) so the intent can flip from it.
    revealed: bool,
}

impl Login {
    /// The idle state: the method choice, with the masked API-key field the
    /// window created once (and keeps one subscription on).
    pub(crate) fn new(api_key: Entity<InputState>) -> Self {
        Self { state: LoginState::Choose, url: None, code: None, method: None, api_key, revealed: false }
    }

    /// Whether a sign-in flow is running (the device flow starting or
    /// waiting): closing the sheet cancels it.
    pub(crate) fn flow_running(&self) -> bool {
        matches!(self.state, LoginState::Starting | LoginState::Device { .. })
    }

    /// The state's log name and the key field, for tests outside this module.
    #[cfg(feature = "probe")]
    #[cfg(test)]
    pub(crate) fn state_name(&self) -> &'static str {
        login_state_name(&self.state)
    }

    #[cfg(feature = "probe")]
    #[cfg(test)]
    pub(crate) fn api_key_field(&self) -> Entity<InputState> {
        self.api_key.clone()
    }

    /// Back to the method choice: forget the flow, and redraw the sheet.
    pub(crate) fn reset_to_choose(&mut self, cx: &mut Context<Self>) {
        self.state = LoginState::Choose;
        self.url = None;
        self.code = None;
        self.method = None;
        crate::baaz_log!("login → choose");
        cx.notify();
    }

    /// Move to `state`, with the one stderr line a headless `--login-steps`
    /// run follows the flow by. The line carries the state name only — never
    /// a URL, a code or a key. The sheet redraws.
    fn set_state(&mut self, state: LoginState, cx: &mut Context<Self>) {
        crate::baaz_log!("login → {}", login_state_name(&state));
        self.state = state;
        cx.notify();
    }

    /// The running method, if any.
    fn method(&self) -> Option<LoginMethod> {
        self.method
    }
}

/// The [`LoginState`] name as the `baaz: login → …` stderr line spells it,
/// so a headless `--login-steps` run can be followed from a log. No URL,
/// code or key ever reaches that line.
fn login_state_name(state: &LoginState) -> &'static str {
    match state {
        LoginState::Choose => "choose",
        LoginState::Starting => "starting",
        LoginState::Device { .. } => "device",
        LoginState::ApiKey { .. } => "apikey",
        LoginState::Validating => "validating",
        LoginState::Success => "success",
        LoginState::Error { .. } => "error",
    }
}


/// The login half of [`Harness`]: `auth` stays on the struct in `app.rs`, the
/// flow's state is the [`Login`] entity the Muse sheet draws, the flow
/// lives here, and the shell reaches it through one call per seam —
/// `probe_account` from `connect`, `on_account_event` from the hub's
/// connection stream, `apply_login_sample` and `submit_api_key` from `new`,
/// `logout` from the account menu, `login_intent` from the sheet.
impl Harness {
    /// Back to the method choice ([`Login::reset_to_choose`]). The field
    /// keeps whatever it holds — callers that leave the key form (`Back`,
    /// `ChooseAnother`, a returned submit) clear it explicitly.
    pub(crate) fn login_back_to_choose(&mut self, cx: &mut Context<Self>) {
        self.login.update(cx, |login, cx| login.reset_to_choose(cx));
    }

    /// Move the flow to `state` ([`Login::set_state`]).
    pub(crate) fn set_login_state(&mut self, state: LoginState, cx: &mut Context<Self>) {
        self.login.update(cx, |login, cx| login.set_state(state, cx));
    }

    /// The boot probe and the re-probe after every `account/changed`:
    /// `account/read` is the only sign-in signal (`model/list` answers from
    /// the provider catalog while logged out, so it never was one).
    pub(crate) fn probe_account(&mut self, cx: &mut Context<Self>) {
        if !self.providers.shared_ready() {
            return;
        }
        let slot = self.providers.clone();
        crate::log::boot_mark("account/read-sent");
        self.wire_call(
            cx,
            move || {
                let at = std::time::Instant::now();
                let out = slot.control().and_then(|lane| lane.send(Command::ReadAccount)).and_then(account_of);
                crate::log::boot_mark(&format!(
                    "account/read-work-done ok={} in={}ms",
                    out.is_ok(),
                    at.elapsed().as_millis()
                ));
                out
            },
            |this, result, cx| match result {
            Ok((lane, label)) => {
                crate::log::boot_mark("account/read-reply");
                this.apply_account(lane, label, cx)
            }
            Err(error) => {
                // The wire is up but the probe failed: say so and show
                // the login screen, like the old probe did. Never logs
                // more than the failure itself.
                crate::baaz_log!("account/read failed: {error}");
                crate::baaz_log!("account → loggedOut");
                this.tier_retry_settled();
                this.auth = Auth::SignedOut;
                this.login_back_to_choose(cx);
                #[cfg(feature = "probe")]
                this.run_login_steps(cx);
                cx.notify();
            }
        });
    }

    /// Rebuild [`Auth`] from an account reading — the probe answer and the
    /// `AccountChanged` event share this exactly.
    ///
    /// `loggedOut` clears the shell and shows the login screen (with the
    /// "removed outside the app" dialog when a signed-in session loses its
    /// credential); any other lane signs in, loads the sessions, and works
    /// out the tier — the TUI probe for `accountLogin`, pay-as-you-go by
    /// construction for the key lanes.
    pub(crate) fn apply_account(&mut self, lane: AccountLane, label: Option<String>, cx: &mut Context<Self>) {
        let name = lane_name(&lane);
        match Identity::from_account(&lane, label) {
            Some(identity) => {
                crate::baaz_log!("account → {name}");
                let api_key = identity.is_api_key();
                // The status service reads this live connection, never a
                // second one: remember what it said and re-probe Muse so
                // the logged line follows sign-in.
                let email = identity.email.trim().to_owned();
                crate::provider_status::note_muse_account(Some(
                    crate::provider_status::MuseAccount {
                        email: (!email.is_empty()).then_some(email),
                        plan: None,
                    },
                ));
                crate::provider_status::refresh_muse_status();
                self.auth = Auth::SignedIn(identity);
                // A Muse sign-in lands wherever it started: it closes the
                // sheet, clears Muse's quiet banner, and marks Muse seen
                // so a later sign-out earns the banner instead of a gate.
                self.close_muse_sheet_entry(cx);
                self.provider_model.update(cx, |model, _| model.muse_signed_in());
                self.load_sessions(cx);
                // `--tier` fakes the probe for a screenshot, and nothing else:
                // it wins over the lane, exactly as `probe_tier` does, so a
                // scripted capture can get past the pay-as-you-go guard.
                if let Some(faked) = self.args.tier.clone() {
                    self.tier = Some(faked);
                    self.push_tier(cx);
                } else if api_key {
                    // The TUI probe is about subscriptions; a stored key or
                    // `META_API_KEY` bills pay-as-you-go by construction, so
                    // the footer says so without probing.
                    self.tier = Some(Tier::PayAsYouGo);
                    self.push_tier(cx);
                } else {
                    // A fresh login is also the re-probe a login asks for.
                    self.probe_tier(false, cx);
                }
                cx.notify();
            }
            None => {
                crate::baaz_log!("account → loggedOut");
                self.tier_retry_settled();
                // The live connection reports no login: forget what it
                // said and re-probe Muse so the logged line follows.
                crate::provider_status::note_muse_account(None);
                crate::provider_status::refresh_muse_status();
                let was_in = matches!(self.auth, Auth::SignedIn(_));
                self.auth = Auth::SignedOut;
                self.login_back_to_choose(cx);
                // Never a full-screen gate: on the connect screen the rows
                // already show it; in the shell the sessions wait on the
                // connection and a quiet banner names the way back.
                if self.connect_open(cx) {
                    self.forget_muse_rows(cx);
                } else if was_in {
                    self.provider_model.update(cx, |model, _| model.muse_signed_out());
                }
                #[cfg(feature = "probe")]
                self.run_login_steps(cx);
                cx.notify();
            }
        }
    }

    /// One [`LoginIntent`]: the login screen's buttons, the Escape walk, and
    /// the `--login-steps` verbs all arrive here. Needs the window for the
    /// field (focus, clearing) — background completions that touch the field
    /// go through `update_in` to get one.
    pub(crate) fn login_intent(&mut self, intent: LoginIntent, window: &mut Window, cx: &mut Context<Self>) {
        match intent {
            LoginIntent::StartAccount => self.start_device_flow(cx),
            LoginIntent::UseApiKey => {
                self.login.update(cx, |login, cx| {
                    login.method = Some(LoginMethod::ApiKey);
                    login.set_state(LoginState::ApiKey { can_submit: false, error: None }, cx);
                });
                let field = self.login.read(cx).api_key.focus_handle(cx);
                window.focus(&field, cx);
            }
            LoginIntent::SubmitApiKey => self.submit_api_key(cx),
            // The field draws its own masked flag (its notify redraws it);
            // `revealed` only mirrors it for the next flip.
            LoginIntent::ToggleReveal => {
                let api_key = self.login.update(cx, |login, _| {
                    login.revealed = !login.revealed;
                    login.api_key.clone()
                });
                let next = self.login.read(cx).revealed;
                api_key.update(cx, |state, cx| state.set_masked(next, window, cx));
            }
            LoginIntent::Back | LoginIntent::ChooseAnother => {
                self.login_back_to_choose(cx);
                let api_key = self.login.read(cx).api_key.clone();
                api_key.update(cx, |state, cx| state.clean(window, cx));
            }
            LoginIntent::OpenBrowser => {
                if let Some(url) = self.login.read(cx).url.clone() {
                    // Straight into the child's argv; never into a log.
                    let _ = auth::open_in_browser(&url);
                }
            }
            LoginIntent::CopyCode => {
                if let Some(code) = self.login.read(cx).code.clone() {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(code));
                }
            }
            LoginIntent::Retry => match self.login.read(cx).method() {
                Some(LoginMethod::Account) => self.start_device_flow(cx),
                Some(LoginMethod::ApiKey) => self.login_intent(LoginIntent::UseApiKey, window, cx),
                None => {
                    self.login_back_to_choose(cx);
                }
            },
            LoginIntent::Cancel => self.cancel_login_flow(cx),
            LoginIntent::Close => self.close_muse_sheet(window, cx),
        }
    }

    /// Start the device-code flow: `account/loginStart {deviceCode}` on the
    /// background executor. The URL and code come back in the result — never
    /// in a notification — and the browser opens once, on entering the
    /// device state (D26). Nothing here is logged but the state name.
    pub(crate) fn start_device_flow(&mut self, cx: &mut Context<Self>) {
        self.login.update(cx, |login, _| login.method = Some(LoginMethod::Account));
        if !self.providers.shared_ready() {
            self.set_login_state(
                LoginState::Error { message: format!("{} is not running.", crate::providers::shared_label()).into(), method: Some(LoginMethod::Account) },
                cx,
            );
            return;
        }
        self.set_login_state(LoginState::Starting, cx);
        let slot = self.providers.clone();
        let work = move || slot.control().and_then(|lane| lane.send(Command::BeginLogin { api_key: None }));
        self.wire_call(cx, work, |this, result, cx| match result {
            Ok(Ack::LoginChallenge { verification_url, user_code }) => match (verification_url, user_code) {
                (Some(url), Some(code)) => {
                    this.login.update(cx, |login, _| {
                        login.url = Some(url.clone());
                        login.code = Some(code.clone());
                    });
                    this.set_login_state(
                        LoginState::Device {
                            url: url.clone().into(),
                            code: code.clone().into(),
                            expires: None,
                            waiting: true,
                        },
                        cx,
                    );
                    let _ = auth::open_in_browser(&url);
                }
                _ => {
                    this.set_login_state(
                        LoginState::Error {
                            message: "The server started no device flow.".into(),
                            method: Some(LoginMethod::Account),
                        },
                        cx,
                    );
                }
            },
            Ok(other) => {
                crate::baaz_log!("account/loginStart answered {other:?}");
                this.set_login_state(
                    LoginState::Error {
                        message: "The server started no device flow.".into(),
                        method: Some(LoginMethod::Account),
                    },
                    cx,
                );
            }
            Err(error) => {
                this.set_login_state(
                    LoginState::Error { message: error_words(&error).into(), method: Some(LoginMethod::Account) },
                    cx,
                );
            }
        });
    }

    /// Submit the API-key form: read the field once, trim, send
    /// `account/loginStart {apiKey}` on the background executor. The key is
    /// a local in the task closure and nowhere else; the field is cleared
    /// when the call returns, whatever it returned (D24). An empty field
    /// does nothing — the Sign in button is disabled until there is text.
    pub(crate) fn submit_api_key(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.login.read(cx).state, LoginState::ApiKey { .. }) {
            return;
        }
        let key = self.login.read(cx).api_key.read(cx).value().to_string();
        let trimmed = key.trim().to_owned();
        if trimmed.is_empty() {
            return;
        }
        if !self.providers.shared_ready() {
            self.set_login_state(
                LoginState::ApiKey {
                    can_submit: false,
                    error: Some(format!("{} is not running.", crate::providers::shared_label()).into()),
                },
                cx,
            );
            return;
        }
        self.login.update(cx, |login, _| login.method = Some(LoginMethod::ApiKey));
        self.set_login_state(LoginState::Validating, cx);
        let slot = self.providers.clone();
        let work = move || slot.control().and_then(|lane| lane.send(Command::BeginLogin { api_key: Some(trimmed) }));
        // `wire_call_in` for the window the field-clear needs.
        self.wire_call_in(cx, work, |this, result, window, cx| {
            let api_key = this.login.read(cx).api_key.clone();
            api_key.update(cx, |state, cx| state.clean(window, cx));
            match result {
                // A stored key: the signed-in `account/changed` follows
                // and enters the app; until then the spinner stays (the
                // cleared field redraws itself).
                Ok(_) => {}
                Err(error) => {
                    this.set_login_state(
                        LoginState::ApiKey {
                            can_submit: false,
                            error: Some(error_words(&error).into()),
                        },
                        cx,
                    );
                }
            }
        });
    }

    /// Abandon the running flow: back to the method choice immediately, and
    /// `account/loginCancel` in the background. The `cancelled`
    /// notification that precedes its result is then a no-op — the screen is
    /// already where it would go.
    pub(crate) fn cancel_login_flow(&mut self, cx: &mut Context<Self>) {
        self.login_back_to_choose(cx);
        if !self.providers.shared_ready() {
            return;
        }
        let slot = self.providers.clone();
        cx.background_spawn(async move {
            let _ = slot.control().and_then(|lane| lane.send(Command::CancelLogin));
        })
        .detach();
    }

    /// Sign out over the wire: `account/logout` in the background, and its
    /// result — an [`AccountState`] — applied like `account/changed`, except
    /// a deliberate sign-out never raises the "removed outside the app"
    /// dialog. An `envKey` lane survives this (the environment still holds
    /// the key), so that case keeps the shell and explains itself in a toast
    /// (D28).
    /// A Muse sign-out takes Muse's rows with it — and the open session
    /// only when it is a Muse one. Claude Code and Codex rows are not
    /// Muse's to clear (docs/50 P1).
    pub(crate) fn forget_muse_rows(&mut self, cx: &mut Context<Self>) {
        let Some(signed_out) = crate::providers::ProviderId::with(provider::ProviderTrait::AccountOnConnection) else { return };
        if self.active.as_ref().is_some_and(|view| view.read(cx).rt(cx).provider_kind() == signed_out) {
            self.active = None;
        }
        self.session_list.update(cx, |list, _| list.sessions.retain(|entry| entry.provider != signed_out));
        self.invalidate_list(cx);
    }

    pub(crate) fn logout(&mut self, cx: &mut Context<Self>) {
        if !self.providers.shared_ready() {
            self.forget_muse_rows(cx);
            self.auth = Auth::SignedOut;
            self.login_back_to_choose(cx);
            return;
        }
        let slot = self.providers.clone();
        let work = move || slot.control().and_then(|lane| lane.send(Command::LogOut)).and_then(account_of);
        self.wire_call(cx, work, |this, result, cx| match result {
            Ok((lane, label)) => match Identity::from_account(&lane, label) {
                Some(identity) => {
                    let env_key = identity.lane == AccountLane::EnvKey;
                    this.auth = Auth::SignedIn(identity);
                    if env_key {
                        this.overlays.update(cx, |overlays, cx| {
                            overlays.toast(
                                "Still signed in",
                                "META_API_KEY is set in the environment; unset it and relaunch to sign out.",
                             cx);
                        });
                    }
                    cx.notify();
                }
                None => {
                    crate::tier::forget();
                    this.tier = None;
                    this.tier_retry_settled();
                    this.forget_muse_rows(cx);
                    this.auth = Auth::SignedOut;
                    this.login_back_to_choose(cx);
                    // A deliberate sign-out still clears, but outside the
                    // connect screen it lands in the shell with the quiet
                    // banner — never the old full-screen gate.
                    if !this.connect_open(cx) {
                        this.provider_model.update(cx, |model, _| model.muse_signed_out());
                    }
                    cx.notify();
                }
            },
            Err(error) => {
                this.set_dialog(cx, Dialog {
                    title: "Sign out failed".into(),
                    detail: error_words(&error),
                    kind: DialogKind::Error,
                    primary: "Dismiss",
                    action: DialogAction::Dismiss,
                    archive_target: None,
                });
            }
        });
    }

    /// The connection's account events: `AccountChanged` rebuilds [`Auth`]
    /// exactly as [`Self::probe_account`] does — a signed-in lane while on
    /// the login screen enters the app with no reconnect (D25) — and
    /// `LoginCompleted` advances the login screen's own state. The hub drops
    /// a frame that does not decode: the wire owns the flow, and a
    /// malformed outcome must not move the screen.
    pub(crate) fn on_account_event(&mut self, event: provider::ProviderEvent, cx: &mut Context<Self>) {
        match event {
            provider::ProviderEvent::AccountChanged { lane, label } => self.apply_account(lane, label, cx),
            provider::ProviderEvent::LoginCompleted { outcome, message } => self.on_login_completed(outcome, message, cx),
            _ => {}
        }
    }

    /// The terminal outcome of the running login flow.
    ///
    /// `granted` shows Success (the signed-in `account/changed` that follows
    /// enters the app); `denied` / `expired` / `failed` show the screen for
    /// the running method — the full error card, except an API-key `failed`,
    /// which goes back to the key form so the key can be fixed;
    /// `cancelled` returns to the method choice unless already there.
    pub(crate) fn on_login_completed(&mut self, outcome: LoginOutcome, message: Option<String>, cx: &mut Context<Self>) {
        let message = message.filter(|message| !message.trim().is_empty());
        // The outcome and its display message are the server's typed
        // vocabulary — never the URL, the code or a key — so a headless run
        // can be followed from stderr.
        crate::baaz_log!(
            "loginCompleted → {}{}",
            outcome_name(&outcome),
            message.as_deref().map(|m| format!(": {m}")).unwrap_or_default()
        );
        match outcome {
            LoginOutcome::Granted => {
                self.set_login_state(LoginState::Success, cx);
            }
            LoginOutcome::Denied | LoginOutcome::Expired | LoginOutcome::Failed => {
                let fallback = match outcome {
                    LoginOutcome::Denied => "The sign-in request was denied.",
                    LoginOutcome::Expired => "The sign-in request expired before it was approved.",
                    _ => "Sign-in failed.",
                };
                let text: SharedString =
                    message.unwrap_or_else(|| fallback.to_owned()).into();
                // An API-key failure belongs on the key form, where the key
                // can be fixed — not on the error card with its way back.
                if outcome == LoginOutcome::Failed
                    && self.login.read(cx).method() == Some(LoginMethod::ApiKey)
                {
                    self.set_login_state(LoginState::ApiKey { can_submit: false, error: Some(text) }, cx);
                } else {
                    self.set_login_state(
                        LoginState::Error { message: text, method: self.login.read(cx).method() },
                        cx,
                    );
                }
            }
            LoginOutcome::Cancelled => {
                if !matches!(self.login.read(cx).state, LoginState::Choose) {
                    self.login_back_to_choose(cx);
                }
            }
            // An outcome a newer server invented: the honest card is the
            // method's error, with the server's message when it sent one.
            LoginOutcome::Other(_) => {
                let text: SharedString =
                    message.unwrap_or_else(|| "The sign-in ended in a way this build does not understand.".to_owned())
                        .into();
                if self.login.read(cx).method() == Some(LoginMethod::ApiKey) {
                    self.set_login_state(LoginState::ApiKey { can_submit: false, error: Some(text) }, cx);
                } else {
                    self.set_login_state(
                        LoginState::Error { message: text, method: self.login.read(cx).method() },
                        cx,
                    );
                }
            }
        }
    }

    #[cfg(feature = "probe")]
    /// `--login-steps <a;b;c>`: drive the login screen from the command line
    /// so a signed-in capture is reproducible without a pointer. Honoured
    /// only when the app is really connected — the offline and replay boots
    /// never call the probe that calls this — one step per item, the same
    /// `;`-separated parsing as `--steps`. Consumed, so a later re-probe
    /// does not replay them. The verbs, and the loop that runs them, are
    /// [`crate::steps`].
    pub(crate) fn run_login_steps(&mut self, cx: &mut Context<Self>) {
        crate::steps::run_login_steps(self, cx);
    }

    #[cfg(feature = "probe")]
    /// `--login-steps` and `--steps`, taken out of the arguments so a later
    /// re-probe or refresh cannot replay them. Drains through the same
    /// [`crate::app::sessions::drain_steps`] as `--steps`: the first caller
    /// gets the script, every later caller gets nothing.
    pub(crate) fn take_login_steps(&mut self) -> Vec<String> {
        crate::app::sessions::drain_steps(&mut self.args.login_steps)
    }

    #[cfg(feature = "probe")]
    /// `key-from-env:<VAR>`: put the value of an environment variable into
    /// the API-key field.
    ///
    /// The value travels from the environment into the field and then into
    /// the wire call: it never appears in argv, a log, or a screenshot
    /// argument. An unset variable fails naming the variable, not its value.
    pub(crate) fn step_key_from_env(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        match std::env::var(rest) {
            Ok(value) => {
                let api_key = self.login.read(cx).api_key.clone();
                api_key.update(cx, |state, cx| state.set_value(value, window, cx));
                true
            }
            Err(_) => {
                crate::baaz_log!("login step `key-from-env:{rest}` failed: variable is not set");
                false
            }
        }
    }

    #[cfg(feature = "probe")]
    /// `--no-connect --login <state>`: sample state for captures — the
    /// signed-in shell with sample identity, or the first-run connect
    /// screen. (The whole-window Muse login screen and its five sample
    /// states are gone, docs/82 0.16.)
    pub(crate) fn apply_login_sample(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        match self.args.login {
            LoginSample::Choose => {
                self.login_back_to_choose(cx);
            }
            // Sample identity, for captures of the signed-in shell. No child runs behind it: the chrome
            // draws, and anything needing the wire quietly does nothing.
            LoginSample::SignedIn => {
                self.auth = Auth::SignedIn(Identity {
                    lane: AccountLane::AccountLogin,
                    name: "Sample".into(),
                    email: String::new(),
                });
                self.wire = crate::app::Wire::Ready;
                cx.notify();
            }
            // The first-run Connect your providers screen, for captures:
            // rows from the scripted statuses, all Checking when unset.
            LoginSample::Connect => {
                self.connect_screen.update(cx, |screen, _| screen.show_first_run());
                self.close_muse_sheet_entry(cx);
                self.provider_model.update(cx, |model, cx| model.reload(cx));
                // The sample is a first run: the import offer may stand
                // under the rows (a capture scans only a fixture home).
                self.import.first_run = true;
                self.refresh_import_scan(cx);
                cx.notify();
            }
        }
    }
}

/// The lane and label an account ack carries.
fn account_of(ack: Ack) -> Result<(AccountLane, Option<String>), ProviderError> {
    match ack {
        Ack::Account { lane, label, .. } => Ok((lane, label)),
        other => Err(ProviderError::Rejected { reason: format!("an account read answered {other:?}") }),
    }
}

/// A lane's name in the `account → …` log line (the wire's spelling, as
/// before).
fn lane_name(lane: &AccountLane) -> &str {
    match lane {
        AccountLane::LoggedOut => "loggedOut",
        AccountLane::EnvKey => "envKey",
        AccountLane::ApiKey => "apiKey",
        AccountLane::AccountLogin => "accountLogin",
        AccountLane::Other(name) => name,
    }
}

/// An outcome's name in the `loginCompleted → …` log line.
fn outcome_name(outcome: &LoginOutcome) -> &str {
    match outcome {
        LoginOutcome::Granted => "granted",
        LoginOutcome::Denied => "denied",
        LoginOutcome::Expired => "expired",
        LoginOutcome::Cancelled => "cancelled",
        LoginOutcome::Failed => "failed",
        LoginOutcome::Other(name) => name,
    }
}

/// What a refused account call shows: the provider's reason, never a
/// wire code.
fn error_words(error: &ProviderError) -> String {
    match error {
        ProviderError::Refused(refusal) => refusal.reason.clone(),
        other => other.to_string(),
    }
}

/// Muse's sign-in sheet as its own view (D5 B3, plan 4.6): the setup
/// sheet's frame at 460 wide over the 45 % scrim — mark, "Sign in to Muse",
/// ✕ — with the login's own steps inside. It draws the [`Login`] entity it
/// holds (the state, the API-key field) and redraws on its notify; the
/// sign-in itself is the root's (the `account/*` lane, the field that must
/// never hand the key to anything but the wire), which runs what the sheet
/// asks for ([`MuseSheetEvent`]). ✕ and Escape dismiss it through the
/// overlay stack (`DismissEvent`); the scrim does not.
pub(crate) struct MuseSheetView {
    /// The flow it draws.
    login: Entity<Login>,
    focus: gpui::FocusHandle,
}

/// What the Muse sheet asks the root for.
#[derive(Clone, Copy, Debug)]
pub(crate) enum MuseSheetEvent {
    /// A login button.
    Intent(LoginIntent),
}

impl gpui::EventEmitter<MuseSheetEvent> for MuseSheetView {}
impl gpui::EventEmitter<gpui::DismissEvent> for MuseSheetView {}

impl gpui::Focusable for MuseSheetView {
    fn focus_handle(&self, _: &gpui::App) -> gpui::FocusHandle {
        self.focus.clone()
    }
}

impl MuseSheetView {
    pub(crate) fn new(login: Entity<Login>, cx: &mut Context<Self>) -> Self {
        MuseSheetView { login, focus: cx.focus_handle() }
    }
}

impl Render for MuseSheetView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // `can_submit` tracks the field's non-empty trimmed text, worked out
        // every frame (the stored bool is only the shape the state needs);
        // a keystroke redraws the window the field is drawn in.
        let (state, api_key) = {
            let login = self.login.read(cx);
            let state = match &login.state {
                LoginState::ApiKey { error, .. } => {
                    LoginState::ApiKey { can_submit: !login.api_key.read(cx).value().trim().is_empty(), error: error.clone() }
                }
                other => other.clone(),
            };
            (state, login.api_key.clone())
        };
        let me = cx.entity().downgrade();
        let intent = move |intent: LoginIntent, _: &mut Window, cx: &mut gpui::App| {
            let _ = me.update(cx, |_, cx| match intent {
                // ✕ is the sheet's own dismissal: the stack closes it.
                LoginIntent::Close => cx.emit(gpui::DismissEvent),
                intent => cx.emit(MuseSheetEvent::Intent(intent)),
            });
        };
        // The eye flips the field's masked flag and the flow's mirror of
        // it, so the next `ToggleReveal` flips from the truth. The component
        // never sees the key: it only reads the masked flag for the glyph.
        let toggle = api_key.clone();
        let flow = self.login.clone();
        let field = secret_field("login-key", &api_key).placeholder("Paste your key").on_toggle_reveal(move |window, cx| {
            let next = !toggle.read(cx).presentation().is_masked();
            toggle.update(cx, |state, cx| state.set_masked(next, window, cx));
            flow.update(cx, |login, _| login.revealed = next);
        });
        // The sheet signs in over the shared connection: that provider's
        // own sheet, named and marked from its descriptor (rule 14).
        let (product, mark) = match crate::providers::Registry::shared_id() {
            Some(id) => (id.label(), crate::providers::ProviderMark::icon(id)),
            None => (crate::providers::shared_label(), aui_icons::Provider::Muse),
        };
        let card = login("login", state)
            .product(product)
            .headline(format!("Sign in to {product}"))
            .provider(mark)
            .api_key_field(field)
            .on_intent(intent)
            .sheet();
        let card = if crate::runtime::captures() { card.at_rest() } else { card };
        let escape = cx.listener(|_: &mut Self, event: &gpui::KeyDownEvent, _, cx| {
            if event.keystroke.key == "escape" {
                cx.emit(gpui::DismissEvent);
                cx.stop_propagation();
            }
        });
        gpui::div()
            .absolute()
            .inset_0()
            .capture_key_down(escape)
            .child(aui::screens::sheet_scrim("muse-sheet-scrim", card.into_any_element(), None))
            .into_any_element()
    }
}

impl Harness {
    /// What the Muse sheet asked for, on the main window.
    pub(crate) fn on_muse_sheet_event(&mut self, event: MuseSheetEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            MuseSheetEvent::Intent(intent) => self.login_intent(intent, window, cx),
        }
    }
}
