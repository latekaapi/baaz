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
//! * [`Harness::apply_account`] turns one [`AccountState`] into [`Auth`], and
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
use gpui::{prelude::*, AnyElement, Context, Entity, Focusable as _, SharedString, Window};
use gpui_kit::base::input::InputState;
use muse_client::schema::{
    AccountLoginCompletedParams, AccountLoginOutcome, AccountLoginStartParams, AccountLoginType,
    AccountState, AccountStateKind,
};

use crate::app::Harness;
use crate::auth::{self, Identity};
use crate::overlays::{Dialog, DialogAction};
use crate::tier::Tier;
use crate::wire::WireCall;
use crate::LoginSample;

/// Where the boot probe got to: sign-in is on the wire now,
/// so this is the `account/read` answer.
pub(crate) enum Auth {
    /// Waiting for `account/read`.
    Probing,
    /// `loggedOut`: the login screen.
    SignedOut,
    /// Any other lane, with the wire's identity.
    SignedIn(Identity),
}

/// The login screen's own state. The screen itself is stateless (it is the
/// library's `aui::screens::login`): this owns the [`LoginState`], the
/// device flow's URL and code, which method is running, and the API-key
/// field. There is no task field: every wire call runs on the background
/// executor and returns through `update`, like every other command.
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

    /// The method choice: forget the flow. The field keeps whatever it
    /// holds — callers that leave the key form (`Back`, `ChooseAnother`,
    /// a returned submit) clear it explicitly — so notify after calling.
    pub(crate) fn reset_to_choose(&mut self) {
        self.state = LoginState::Choose;
        self.url = None;
        self.code = None;
        self.method = None;
        crate::baaz_log!("login → choose");
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


/// The login half of [`Harness`]: the fields stay on the struct in `app.rs`,
/// the flow lives here, and the shell reaches it through one call per seam —
/// `probe_account` from `connect`, `route_account` from `route`,
/// `apply_login_sample` and `submit_api_key` from `new`, `logout` from the
/// account menu, `login_escape` from Escape and `render_login` from `render`.
impl Harness {
    /// Move to `state`, with the one stderr line a headless `--login-steps`
    /// run follows the flow by. The line carries the state name only — never
    /// a URL, a code or a key.
    pub(crate) fn set_login_state(&mut self, state: LoginState, cx: &mut Context<Self>) {
        crate::baaz_log!("login → {}", login_state_name(&state));
        self.login.state = state;
        cx.notify();
    }

    /// The boot probe and the re-probe after every `account/changed`:
    /// `account/read` is the only sign-in signal (`model/list` answers from
    /// the provider catalog while logged out, so it never was one).
    pub(crate) fn probe_account(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        crate::log::boot_mark("account/read-sent");
        self.wire_call(
            cx,
            move || {
                let at = std::time::Instant::now();
                let out = client.account_read();
                crate::log::boot_mark(&format!(
                    "account/read-work-done ok={} in={}ms",
                    out.is_ok(),
                    at.elapsed().as_millis()
                ));
                out
            },
            |this, result, cx| match result {
            Ok(state) => {
                crate::log::boot_mark("account/read-reply");
                this.apply_account(state, cx)
            }
            Err(error) => {
                // The wire is up but the probe failed: say so and show
                // the login screen, like the old probe did. Never logs
                // more than the failure itself.
                crate::baaz_log!("account/read failed: {error}");
                crate::baaz_log!("account → loggedOut");
                this.auth = Auth::SignedOut;
                this.login.reset_to_choose();
                this.run_login_steps(cx);
                cx.notify();
            }
        });
    }

    /// Rebuild [`Auth`] from an [`AccountState`] — the probe answer and the
    /// `account/changed` notification share this exactly.
    ///
    /// `loggedOut` clears the shell and shows the login screen (with the
    /// "removed outside the app" dialog when a signed-in session loses its
    /// credential); any other lane signs in, loads the sessions, and works
    /// out the tier — the TUI probe for `accountLogin`, pay-as-you-go by
    /// construction for the key lanes.
    pub(crate) fn apply_account(&mut self, state: AccountState, cx: &mut Context<Self>) {
        let lane = state.state.as_wire().unwrap_or("unknown").to_owned();
        match Identity::from_account(&state) {
            Some(identity) => {
                crate::baaz_log!("account → {lane}");
                let api_key = identity.is_api_key();
                self.auth = Auth::SignedIn(identity);
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
                let was_in = matches!(self.auth, Auth::SignedIn(_));
                self.active = None;
                self.sessions.clear();
            self.invalidate_list();
                self.auth = Auth::SignedOut;
                self.login.reset_to_choose();
                if was_in {
                    self.set_dialog(cx, Dialog {
                        title: "Signed out of Muse".into(),
                        detail: "The credential was removed outside the app.".into(),
                        kind: DialogKind::Warning,
                        primary: "Sign in",
                        action: DialogAction::SignIn,
                        archive_target: None,
                    });
                }
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
                self.login.method = Some(LoginMethod::ApiKey);
                self.set_login_state(LoginState::ApiKey { can_submit: false, error: None }, cx);
                window.focus(&self.login.api_key.focus_handle(cx), cx);
            }
            LoginIntent::SubmitApiKey => self.submit_api_key(cx),
            LoginIntent::ToggleReveal => {
                let next = !self.login.revealed;
                self.login.revealed = next;
                let api_key = self.login.api_key.clone();
                api_key.update(cx, |state, cx| state.set_masked(next, window, cx));
                cx.notify();
            }
            LoginIntent::Back | LoginIntent::ChooseAnother => {
                self.login.reset_to_choose();
                let api_key = self.login.api_key.clone();
                api_key.update(cx, |state, cx| state.clean(window, cx));
                cx.notify();
            }
            LoginIntent::OpenBrowser => {
                if let Some(url) = self.login.url.clone() {
                    // Straight into the child's argv; never into a log.
                    let _ = auth::open_in_browser(&url);
                }
            }
            LoginIntent::CopyCode => {
                if let Some(code) = self.login.code.clone() {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(code));
                }
            }
            LoginIntent::Retry => match self.login.method {
                Some(LoginMethod::Account) => self.start_device_flow(cx),
                Some(LoginMethod::ApiKey) => self.login_intent(LoginIntent::UseApiKey, window, cx),
                None => {
                    self.login.reset_to_choose();
                    cx.notify();
                }
            },
            LoginIntent::Cancel => self.cancel_login_flow(cx),
        }
    }

    /// Start the device-code flow: `account/loginStart {deviceCode}` on the
    /// background executor. The URL and code come back in the result — never
    /// in a notification — and the browser opens once, on entering the
    /// device state (D26). Nothing here is logged but the state name.
    pub(crate) fn start_device_flow(&mut self, cx: &mut Context<Self>) {
        self.login.method = Some(LoginMethod::Account);
        let Some(client) = self.client.clone() else {
            self.set_login_state(
                LoginState::Error { message: "Muse is not running.".into(), method: Some(LoginMethod::Account) },
                cx,
            );
            return;
        };
        self.set_login_state(LoginState::Starting, cx);
        let work = move || {
            client.account_login_start(&AccountLoginStartParams {
                api_key: None,
                r#type: AccountLoginType::DeviceCode,
            })
        };
        self.wire_call(cx, work, |this, result, cx| match result {
            Ok(start) => match (start.verification_url, start.user_code) {
                (Some(url), Some(code)) => {
                    this.login.url = Some(url.clone());
                    this.login.code = Some(code.clone());
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
            Err(error) => {
                this.set_login_state(
                    LoginState::Error { message: error.to_string().into(), method: Some(LoginMethod::Account) },
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
        if !matches!(self.login.state, LoginState::ApiKey { .. }) {
            return;
        }
        let key = self.login.api_key.read(cx).value().to_string();
        let trimmed = key.trim().to_owned();
        if trimmed.is_empty() {
            return;
        }
        let Some(client) = self.client.clone() else {
            self.set_login_state(
                LoginState::ApiKey {
                    can_submit: false,
                    error: Some("Muse is not running.".into()),
                },
                cx,
            );
            return;
        };
        self.login.method = Some(LoginMethod::ApiKey);
        self.set_login_state(LoginState::Validating, cx);
        let work = move || {
            client.account_login_start(&AccountLoginStartParams {
                api_key: Some(trimmed),
                r#type: AccountLoginType::ApiKey,
            })
        };
        // `wire_call_in` for the window the field-clear needs.
        self.wire_call_in(cx, work, |this, result, window, cx| {
            let api_key = this.login.api_key.clone();
            api_key.update(cx, |state, cx| state.clean(window, cx));
            match result {
                // A stored key: the signed-in `account/changed` follows
                // and enters the app; until then the spinner stays.
                Ok(_) => cx.notify(),
                Err(error) => {
                    this.set_login_state(
                        LoginState::ApiKey {
                            can_submit: false,
                            error: Some(error.to_string().into()),
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
        self.login.reset_to_choose();
        cx.notify();
        let Some(client) = self.client.clone() else { return };
        cx.background_spawn(async move {
            let _ = client.account_login_cancel();
        })
        .detach();
    }

    /// Sign out over the wire: `account/logout` in the background, and its
    /// result — an [`AccountState`] — applied like `account/changed`, except
    /// a deliberate sign-out never raises the "removed outside the app"
    /// dialog. An `envKey` lane survives this (the environment still holds
    /// the key), so that case keeps the shell and explains itself in a toast
    /// (D28).
    pub(crate) fn logout(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            self.active = None;
            self.sessions.clear();
            self.invalidate_list();
            self.auth = Auth::SignedOut;
            self.login.reset_to_choose();
            cx.notify();
            return;
        };
        self.wire_call(cx, move || client.account_logout(), |this, result, cx| match result {
            Ok(state) => match Identity::from_account(&state) {
                Some(identity) => {
                    let env_key = identity.lane == AccountStateKind::EnvKey;
                    this.auth = Auth::SignedIn(identity);
                    if env_key {
                        this.overlays.update(cx, |overlays, _| {
                            overlays.toast(
                                "Still signed in",
                                "META_API_KEY is set in the environment; unset it and relaunch to sign out.",
                            );
                        });
                    }
                    cx.notify();
                }
                None => {
                    this.active = None;
                    this.sessions.clear();
                    this.invalidate_list();
                    this.auth = Auth::SignedOut;
                    this.login.reset_to_choose();
                    cx.notify();
                }
            },
            Err(error) => {
                this.set_dialog(cx, Dialog {
                    title: "Sign out failed".into(),
                    detail: error.to_string(),
                    kind: DialogKind::Error,
                    primary: "Dismiss",
                    action: DialogAction::Dismiss,
                    archive_target: None,
                });
            }
        });
    }

    /// One `account/*` notification, folded before the session view sees
    /// anything. `account/changed` rebuilds [`Auth`] exactly as
    /// [`Self::probe_account`] does — a signed-in lane while on the login
    /// screen enters the app with no reconnect (D25) — and
    /// `account/loginCompleted` advances the login screen's own state. A
    /// frame that does not decode is stderr and nothing else: the wire owns
    /// the flow, and a malformed outcome must not move the screen.
    pub(crate) fn route_account(&mut self, method: &str, params: &serde_json::Value, cx: &mut Context<Self>) {
        match method {
            "account/changed" => match serde_json::from_value::<AccountState>(params.clone()) {
                Ok(state) => self.apply_account(state, cx),
                Err(error) => crate::baaz_log!("ignoring malformed account/changed: {error}"),
            },
            "account/loginCompleted" => {
                match serde_json::from_value::<AccountLoginCompletedParams>(params.clone()) {
                    Ok(completed) => self.on_login_completed(completed, cx),
                    Err(error) => crate::baaz_log!("ignoring malformed account/loginCompleted: {error}"),
                }
            }
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
    pub(crate) fn on_login_completed(&mut self, completed: AccountLoginCompletedParams, cx: &mut Context<Self>) {
        let message = completed.message.filter(|message| !message.trim().is_empty());
        // The outcome and its display message are the server's typed
        // vocabulary — never the URL, the code or a key — so a headless run
        // can be followed from stderr.
        crate::baaz_log!(
            "loginCompleted → {}{}",
            completed.outcome.as_wire().unwrap_or("unknown"),
            message.as_deref().map(|m| format!(": {m}")).unwrap_or_default()
        );
        match completed.outcome {
            AccountLoginOutcome::Granted => {
                self.set_login_state(LoginState::Success, cx);
            }
            AccountLoginOutcome::Denied | AccountLoginOutcome::Expired | AccountLoginOutcome::Failed => {
                let fallback = match completed.outcome {
                    AccountLoginOutcome::Denied => "The sign-in request was denied.",
                    AccountLoginOutcome::Expired => "The sign-in request expired before it was approved.",
                    _ => "Sign-in failed.",
                };
                let text: SharedString =
                    message.unwrap_or_else(|| fallback.to_owned()).into();
                // An API-key failure belongs on the key form, where the key
                // can be fixed — not on the error card with its way back.
                if completed.outcome == AccountLoginOutcome::Failed
                    && self.login.method == Some(LoginMethod::ApiKey)
                {
                    self.set_login_state(LoginState::ApiKey { can_submit: false, error: Some(text) }, cx);
                } else {
                    self.set_login_state(
                        LoginState::Error { message: text, method: self.login.method },
                        cx,
                    );
                }
            }
            AccountLoginOutcome::Cancelled => {
                if !matches!(self.login.state, LoginState::Choose) {
                    self.login.reset_to_choose();
                    cx.notify();
                }
            }
            // An outcome a newer server invented: the honest card is the
            // method's error, with the server's message when it sent one.
            AccountLoginOutcome::Unknown(_) => {
                let text: SharedString =
                    message.unwrap_or_else(|| "The sign-in ended in a way this build does not understand.".to_owned())
                        .into();
                if self.login.method == Some(LoginMethod::ApiKey) {
                    self.set_login_state(LoginState::ApiKey { can_submit: false, error: Some(text) }, cx);
                } else {
                    self.set_login_state(
                        LoginState::Error { message: text, method: self.login.method },
                        cx,
                    );
                }
            }
        }
    }

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

    /// `--login-steps` and `--steps`, taken out of the arguments so a later
    /// re-probe or refresh cannot replay them. Drains through the same
    /// [`crate::app::lifecycle::drain_steps`] as `--steps`: the first caller
    /// gets the script, every later caller gets nothing.
    pub(crate) fn take_login_steps(&mut self) -> Vec<String> {
        crate::app::lifecycle::drain_steps(&mut self.args.login_steps)
    }

    /// `key-from-env:<VAR>`: put the value of an environment variable into
    /// the API-key field.
    ///
    /// The value travels from the environment into the field and then into
    /// the wire call: it never appears in argv, a log, or a screenshot
    /// argument. An unset variable fails naming the variable, not its value.
    pub(crate) fn step_key_from_env(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        match std::env::var(rest) {
            Ok(value) => {
                let api_key = self.login.api_key.clone();
                api_key.update(cx, |state, cx| state.set_value(value, window, cx));
                true
            }
            Err(_) => {
                crate::baaz_log!("login step `key-from-env:{rest}` failed: variable is not set");
                false
            }
        }
    }

    /// Escape on the login screen, which has no session stack to walk:
    /// `Cancel` in `Starting` / `Device`, `Back` in `ApiKey`,
    /// `ChooseAnother` in `Error`, nothing in the other states.
    pub(crate) fn login_escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match &self.login.state {
            LoginState::Starting | LoginState::Device { .. } => {
                self.login_intent(LoginIntent::Cancel, window, cx);
            }
            LoginState::ApiKey { .. } => {
                self.login_intent(LoginIntent::Back, window, cx);
            }
            LoginState::Error { .. } => {
                self.login_intent(LoginIntent::ChooseAnother, window, cx);
            }
            _ => {}
        }
    }

    /// `--no-connect --login <state>`: the login screen's sample data for
    /// captures. The URL, the code and the key-shaped field text are the
    /// example values, never anything the wire sent.
    pub(crate) fn apply_login_sample(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        const URL: &str = "https://example.invalid/device";
        const CODE: &str = "WXYZ-2946";
        // A mask with something behind it: the dots the capture wants.
        const SAMPLE_KEY: &str = "capture-sample-key";
        match self.args.login {
            LoginSample::Choose => {
                self.login.reset_to_choose();
                cx.notify();
            }
            LoginSample::Device => {
                self.login.url = Some(URL.to_owned());
                self.login.code = Some(CODE.to_owned());
                self.login.method = Some(LoginMethod::Account);
                self.set_login_state(
                    LoginState::Device { url: URL.into(), code: CODE.into(), expires: None, waiting: true },
                    cx,
                );
            }
            LoginSample::ApiKey => {
                self.login.method = Some(LoginMethod::ApiKey);
                self.set_login_state(LoginState::ApiKey { can_submit: true, error: None }, cx);
                let api_key = self.login.api_key.clone();
                api_key.update(cx, |state, cx| state.set_value(SAMPLE_KEY, window, cx));
            }
            LoginSample::ApiKeyError => {
                self.login.method = Some(LoginMethod::ApiKey);
                self.set_login_state(
                    LoginState::ApiKey {
                        can_submit: true,
                        error: Some("That key was rejected. Check the key and try again.".into()),
                    },
                    cx,
                );
                let api_key = self.login.api_key.clone();
                api_key.update(cx, |state, cx| state.set_value(SAMPLE_KEY, window, cx));
            }
            LoginSample::Validating => {
                self.login.method = Some(LoginMethod::ApiKey);
                self.set_login_state(LoginState::Validating, cx);
            }
            LoginSample::Error => {
                self.login.method = Some(LoginMethod::Account);
                self.set_login_state(
                    LoginState::Error {
                        message: "The sign-in request expired before it was approved.".into(),
                        method: Some(LoginMethod::Account),
                    },
                    cx,
                );
            }
            // Past the login screen with sample identity, for captures of
            // the signed-in shell. No child runs behind it: the chrome
            // draws, and anything needing the wire quietly does nothing.
            LoginSample::SignedIn => {
                self.auth = Auth::SignedIn(Identity {
                    lane: AccountStateKind::AccountLogin,
                    name: "Sample".into(),
                    email: String::new(),
                });
                self.wire = crate::app::Wire::Ready;
                cx.notify();
            }
        }
    }


    pub(crate) fn render_login(&self, cx: &mut Context<Self>) -> AnyElement {
        let intent = cx.listener(|this: &mut Self, intent: &LoginIntent, window, cx| {
            this.login_intent(*intent, window, cx);
        });
        // `can_submit` tracks the field's non-empty trimmed text, recomputed
        // every frame; the stored bool is only the shape the state needs.
        let state = match &self.login.state {
            LoginState::ApiKey { error, .. } => LoginState::ApiKey {
                can_submit: !self.login.api_key.read(cx).value().trim().is_empty(),
                error: error.clone(),
            },
            other => other.clone(),
        };
        // The eye flips the field's masked flag and keeps `revealed` in sync,
        // so `ToggleReveal` flips from the truth. The component never sees
        // the key: it only reads the masked flag for the glyph.
        let baaz = cx.entity().downgrade();
        let toggle = self.login.api_key.clone();
        let field = secret_field("login-key", &self.login.api_key)
            .placeholder("Paste your key")
            .on_toggle_reveal(move |window, cx| {
                let next = !toggle.read(cx).presentation().is_masked();
                toggle.update(cx, |state, cx| state.set_masked(next, window, cx));
                let _ = baaz.update(cx, |this, _| this.login.revealed = next);
            });
        // A deterministic capture draws the card settled: the login screen's
        // enter presence never lands on the same frame twice.
        let card = login("login", state)
            .product("Muse")
            .headline("Sign in to Muse")
            .subtitle("Baaz signs in over the wire, the same way the muse CLI does.")
            .provider(aui_icons::Provider::Muse)
            .api_key_field(field);
        let card = if crate::clock::deterministic() { card.at_rest() } else { card };
        card.on_intent(move |i, window, cx| intent(&i, window, cx)).into_any_element()
    }
}
