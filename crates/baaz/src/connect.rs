//! First-run provider connect (design `docs/23-providers-connect.md` §3–§4).
//!
//! Launch is decided from stored facts only ([`decide_launch`]): a first run
//! shows the `connect_providers` screen, every other launch renders the shell
//! at once from the cached statuses — Muse's `account/read` never gates the
//! window. A cached-Connected provider that later probes Signed out gets a
//! quiet [`signed_out_banner_text`] on its composer, never a full-screen gate.
//!
//! The rows ([`build_rows`]) start at Checking and fill in as probes land: a
//! provider still probing never reads Not installed ([`headline_for`]).
//! Continue enables once any provider is Connected ([`continue_enabled`]);
//! Continue and Skip both set `onboarding_completed` ([`complete_onboarding`]).
//!
//! Row actions: Muse Sign in opens Muse's sign-in sheet; Codex Sign in runs
//! `account/login/start` on a short-lived app-server ([`run_codex_login`]);
//! Claude Code Sign in, every Install and every Update open the setup sheet
//! ([`crate::setup`]) with the command typed, not run — the person presses
//! Enter.

use std::collections::HashMap;
use std::io::BufRead as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use aui::screens::{ConnectRowData, ProviderActionDef, ProviderHeadline as AuiHeadline, StatusLine};
use aui_icons::Provider as AuiProvider;

use crate::providers::ProviderId;
use crate::provider_status::{Auth, ProviderStatus};

/// Serialize the tests that touch the scrubbed env names: the test
/// runner shares one environment across threads in this binary. Every
/// test module that sets a `CLAUDE_*` / `ANTHROPIC_*` name holds this
/// while it does — including the PTY tests in
/// [`crate::terminal::host`].
#[cfg(test)]
pub(crate) static SCRUB_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

// ------------------------------------------------------------ launch

/// What a launch shows first: the connect screen, or the shell immediately.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchDecision {
    /// Stored facts say first run: the Connect your providers screen.
    Connect,
    /// Any other launch: the shell at once, from the cached statuses.
    Shell,
}

/// The launch decision from stored facts only — never from a live probe.
/// First run (no `onboarding_completed`, no sessions, no projects) shows the
/// connect screen; a returning launch renders the shell even when the cache
/// is empty (rows then read Checking until probes land).
pub fn decide_launch(first_run: bool) -> LaunchDecision {
    if first_run { LaunchDecision::Connect } else { LaunchDecision::Shell }
}

/// Whether the connect screen's Continue is enabled: any provider Connected.
/// The component enforces this itself; this names the rule so the tests pin
/// the same gating the captures show.
#[cfg(test)]
pub fn continue_enabled(statuses: &[ProviderStatus]) -> bool {
    statuses.iter().any(|status| status.headline() == crate::provider_status::Headline::Connected)
}

/// Finish first-run setup (Continue and Skip both call this): presence of
/// the flag ends [`crate::provider_status::is_first_run`]. Best-effort, and
/// never in deterministic mode — like the flag write itself.
pub fn complete_onboarding() {
    crate::provider_status::set_onboarding_completed();
}

// ------------------------------------------------------------ rows

/// The connect screen's row order (the D5 board): Claude Code, Codex, Muse.
/// The footer picker lists Connected providers in this order too, and the
/// first of them is the default when the person has not picked one.
pub const CONNECT_ORDER: [ProviderId; 3] = [ProviderId::ClaudeCode, ProviderId::Codex, ProviderId::Muse];

/// The connect row's headline: the status headline, except a provider that
/// has not reported yet this launch (no probe result and no cache) always
/// reads Checking — never Not installed while a probe is pending.
pub fn headline_for(status: &ProviderStatus) -> AuiHeadline {
    use aui::screens::ProviderHeadline as H;
    if status.checked_at.is_none() {
        return H::Checking;
    }
    match status.headline() {
        crate::provider_status::Headline::Checking => H::Checking,
        crate::provider_status::Headline::Disabled => H::Disabled,
        crate::provider_status::Headline::NotInstalled => H::NotInstalled,
        crate::provider_status::Headline::CantRun => H::CantRun,
        crate::provider_status::Headline::SignedOut => H::SignedOut,
        crate::provider_status::Headline::Unverified => H::Unverified,
        crate::provider_status::Headline::Connected => H::Connected,
    }
}

/// `"<email> · <plan>"`, omitting what the probe did not say; `None` when
/// it said neither (or the login is not a signed-in one).
pub fn account_words(status: &ProviderStatus) -> Option<String> {
    let Auth::SignedIn { email, plan, .. } = &status.auth else { return None };
    let parts: Vec<&str> = [email.as_deref(), plan.as_deref()]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// Whether the login in force is an API key (Claude Code's
/// `authMethod: "api_key"`, Codex's `account.type == "apiKey"`): every turn
/// bills that key, which the status line says (owner 2026-10-23: say it,
/// don't block).
pub fn api_key_login(status: &ProviderStatus) -> bool {
    matches!(&status.auth, Auth::SignedIn { method: Some(method), .. }
        if matches!(method.to_ascii_lowercase().replace('_', "").as_str(), "apikey"))
}

/// The installed version, when the CLI is below Baaz's floor: the status
/// line then reads "<version> is older than Baaz supports (<floor>)".
pub fn too_old(status: &ProviderStatus) -> Option<(String, String)> {
    let crate::provider_status::Advisory::TooOld { need } = &status.advisory else { return None };
    let version = match &status.installed {
        crate::provider_status::Installed::Yes { version, .. } => {
            crate::provider_status::version_triplet(version)
                .map(|(a, b, c)| format!("{a}.{b}.{c}"))
                .unwrap_or_else(|| version.clone())
        }
        _ => return None,
    };
    Some((version, need.clone()))
}

/// The status line every provider surface shows (D5 board): the tone and
/// the words, first match wins — Checking, too old, couldn't check,
/// Connected (`email · plan`; an API key says so with a warning dot),
/// Signed out, Not installed. `card` prefixes Connected lines with
/// "Connected · " (the Settings card has no ✓ beside it).
pub fn status_line(status: &ProviderStatus, card: bool) -> StatusLine {
    use AuiHeadline as H;
    let headline = headline_for(status);
    if headline == H::Checking {
        return StatusLine::busy("Checking…");
    }
    if let Some((version, need)) = too_old(status) {
        return StatusLine::warn(format!("{version} is older than Baaz supports ({need})"));
    }
    match headline {
        H::Checking => StatusLine::busy("Checking…"),
        H::Disabled => StatusLine::off("Turned off in Settings"),
        H::NotInstalled => StatusLine::off("Not installed"),
        H::CantRun => match &status.advisory {
            crate::provider_status::Advisory::CantRun { timed_out: true, .. } => {
                StatusLine::bad("Couldn’t check — the CLI timed out")
            }
            crate::provider_status::Advisory::CantRun { stderr, .. } if !stderr.trim().is_empty() => {
                StatusLine::bad(format!("Couldn’t run — {}", stderr.lines().next().unwrap_or("").trim()))
            }
            _ => StatusLine::bad("Couldn’t run"),
        },
        H::SignedOut => StatusLine::warn("Signed out"),
        H::Unverified => StatusLine::warn("Installed · sign-in not verified"),
        H::Connected => {
            if api_key_login(status) {
                return StatusLine::warn(if card {
                    "Connected · API key · billed per token"
                } else {
                    "API key · billed per token"
                });
            }
            let words = account_words(status);
            match (card, words) {
                (true, Some(words)) => StatusLine::ok(format!("Connected · {words}")),
                (false, Some(words)) => StatusLine::ok(words),
                (_, None) => StatusLine::ok("Connected"),
            }
        }
    }
}

/// One row's action: Update when too old, Install (or "Get Muse ↗") when
/// missing, Sign in when signed out or unverified, Re-check when the check
/// failed, nothing when Connected, Checking or Disabled.
pub fn primary_action(status: &ProviderStatus) -> Option<ProviderActionDef> {
    use AuiHeadline as H;
    let headline = headline_for(status);
    if headline != H::Checking && too_old(status).is_some() && status.provider.update_command().is_some() {
        return Some(ProviderActionDef::update());
    }
    match headline {
        H::Connected | H::Checking | H::Disabled => None,
        H::NotInstalled => {
            if install_command(status.provider).is_some() {
                Some(ProviderActionDef::install())
            } else {
                Some(get_action(status.provider))
            }
        }
        H::SignedOut | H::Unverified => Some(ProviderActionDef::sign_in()),
        H::CantRun => Some(ProviderActionDef::recheck()),
    }
}

fn aui_provider(id: ProviderId) -> AuiProvider {
    match id {
        ProviderId::Muse => AuiProvider::Muse,
        ProviderId::ClaudeCode => AuiProvider::Claude,
        ProviderId::Codex => AuiProvider::Codex,
    }
}

/// Whether `status` counts as Connected on the connect screen: signed in,
/// and not flagged too old (that row offers Update instead).
pub fn row_connected(status: &ProviderStatus) -> bool {
    headline_for(status) == AuiHeadline::Connected && too_old(status).is_none()
}

/// The three connect rows in [`CONNECT_ORDER`]. Codex's row follows its
/// sign-in while one runs (`codex`): a spinner and Cancel while waiting,
/// the failure reason when it failed.
pub fn build_rows(statuses: &[ProviderStatus], codex: &CodexLogin) -> Vec<ConnectRowData> {
    let by_id: HashMap<ProviderId, &ProviderStatus> = statuses.iter().map(|s| (s.provider, s)).collect();
    CONNECT_ORDER
        .into_iter()
        .map(|id| {
            let status = by_id
                .get(&id)
                .map(|status| (*status).clone())
                .unwrap_or_else(|| ProviderStatus::checking(id));
            let mut line = status_line(&status, false);
            let mut action = primary_action(&status);
            let connected = row_connected(&status);
            if id == ProviderId::Codex && !connected {
                match codex {
                    CodexLogin::Waiting { .. } | CodexLogin::Device { .. } => {
                        line = StatusLine::busy("Finish signing in with your browser…");
                        action = Some(ProviderActionDef::cancel());
                    }
                    CodexLogin::Completed => {
                        line = StatusLine::busy("Signed in — checking…");
                        action = None;
                    }
                    CodexLogin::Failed { reason } => line = StatusLine::warn(reason.clone()),
                    CodexLogin::Idle | CodexLogin::Cancelled => {}
                }
            }
            ConnectRowData {
                id: gpui::SharedString::from(id.as_str()),
                provider: aui_provider(id),
                status: line,
                action,
                connected,
            }
        })
        .collect()
}

/// The assist line's runs for a Codex sign-in in `state` (see
/// [`Harness::codex_assist`]). Pure, so tests read it without a window.
pub fn codex_assist_runs(state: &CodexLogin) -> Vec<aui::screens::AssistRun> {
    use aui::screens::AssistRun as R;
    match state {
        CodexLogin::Waiting { auth_url } if !auth_url.is_empty() => vec![
            R::Text("Browser didn’t open?".into()),
            R::Link("copy-link".into(), "Copy the link".into()),
            R::Text("·".into()),
            R::Link("use-code".into(), "Use a code instead".into()),
        ],
        CodexLogin::Device { url, code } => {
            let place = url.trim_start_matches("https://").trim_start_matches("http://").trim_end_matches('/');
            vec![
                R::Text("Enter".into()),
                R::Code(code.clone().into()),
                R::Text(format!("at {place} ·").into()),
                R::Link("copy-code".into(), "Copy code".into()),
                R::Text("·".into()),
                R::Link("open-page".into(), "Open page".into()),
            ]
        }
        _ => Vec::new(),
    }
}

/// The provider new sessions start in, as the footer shows it: `current`
/// while it is Connected, else the first Connected in [`CONNECT_ORDER`];
/// `None` when nothing is Connected (the footer reads "Nothing is ready").
pub fn connect_default(statuses: &[ProviderStatus], current: ProviderId) -> Option<ProviderId> {
    let connected: Vec<ProviderId> = connected_choices(statuses);
    if connected.contains(&current) {
        return Some(current);
    }
    connected.first().copied()
}

/// The Connected providers, in [`CONNECT_ORDER`]: what the footer picker
/// lists.
pub fn connected_choices(statuses: &[ProviderStatus]) -> Vec<ProviderId> {
    CONNECT_ORDER
        .into_iter()
        .filter(|id| statuses.iter().any(|s| s.provider == *id && s.enabled && row_connected(s)))
        .collect()
}

/// The quiet composer banner's words when the session's provider is
/// signed out (D5 B5).
pub fn signed_out_banner_text(id: ProviderId) -> String {
    format!("{} is signed out. Sign in to send.", id.label())
}

// ------------------------------------------------------------ terminal prefill + docs

/// The vendor install command, typed into the setup sheet's terminal and
/// not run (the registry's table, [`ProviderId::install_command`]).
pub fn install_command(id: ProviderId) -> Option<&'static str> {
    id.install_command()
}

/// The vendor docs behind every row's Docs button.
pub fn docs_url(id: ProviderId) -> &'static str {
    id.docs_url()
}

/// "Get Muse": the missing row's way to the CLI when there is no command to
/// type — a link, and the row's only button (no second Docs beside it).
pub fn get_action(id: ProviderId) -> ProviderActionDef {
    ProviderActionDef::link(format!("Get {}", id.label()))
}

// ------------------------------------------------------------ Codex login

/// The Codex Sign in state machine: idle, waiting on the browser handshake,
/// then done, cancelled or failed. The row reads "Waiting for sign-in…
/// Cancel" while waiting.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum CodexLogin {
    /// No login running.
    #[default]
    Idle,
    /// `account/login/start` answered with an `authUrl` the browser holds.
    Waiting {
        /// The URL the browser opened.
        auth_url: String,
    },
    /// `account/login/completed` arrived: re-probe Codex now.
    Completed,
    /// The device-code fallback ("Use a code instead"): the person types
    /// `code` at `url` in any browser.
    Device {
        /// Where the code goes (`verificationUrl`).
        url: String,
        /// The one-time code (`userCode`).
        code: String,
    },
    /// The person pressed Cancel.
    Cancelled,
    /// The flow failed with the human reason.
    Failed {
        /// What went wrong, in the person's words.
        reason: String,
    },
}

impl CodexLogin {
    /// `account/login/completed` arrived on the app-server: done, re-probe.
    pub fn completed(&mut self) {
        *self = CodexLogin::Completed;
    }

    /// The person pressed Cancel: abandon the flow.
    pub fn cancel(&mut self) {
        *self = CodexLogin::Cancelled;
    }
}

/// The `account/login/start` request frame: `{type: "chatgpt"}` (the
/// browser flow) or `{type: "chatgptDeviceCode"}` ("Use a code instead",
/// codex-cli 0.144.6's `LoginAccountParams`).
pub fn login_start_request(id: u64, device: bool) -> serde_json::Value {
    let kind = if device { "chatgptDeviceCode" } else { "chatgpt" };
    serde_json::json!({
        "id": id,
        "method": "account/login/start",
        "params": {"type": kind},
    })
}

/// The `userCode` and `verificationUrl` in a device-code start answer
/// (`{loginId, type: "chatgptDeviceCode", userCode, verificationUrl}`).
pub fn parse_login_start_device(result: &serde_json::Value) -> Option<(String, String)> {
    let field = |name: &str| {
        result.get(name).and_then(serde_json::Value::as_str).map(str::trim).filter(|v| !v.is_empty()).map(str::to_owned)
    };
    Some((field("verificationUrl")?, field("userCode")?))
}

/// The `authUrl` in an `account/login/start` result, when it names one.
pub fn parse_login_start_auth_url(result: &serde_json::Value) -> Option<String> {
    result
        .get("authUrl")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(str::to_owned)
}

/// Whether one app-server line is the `account/login/completed` push that
/// ends the flow.
pub fn is_login_completed_frame(frame: &serde_json::Value) -> bool {
    frame.get("method").and_then(serde_json::Value::as_str) == Some("account/login/completed")
}

/// How long the Codex browser handshake may take before the row fails it.
pub const CODEX_LOGIN_TIMEOUT: Duration = Duration::from_secs(600);

/// The blocking Codex login, on a background thread: handshake, send
/// `account/login/start {type: "chatgpt"}`, report the `authUrl` for the
/// browser, and end on `account/login/completed` (then the caller
/// re-probes), Cancel, a timeout, or a failure. Read-only besides the login
/// itself; never a real login in tests — drive it through [`LoginIo`].
pub trait LoginIo {
    /// Send one frame to the child.
    fn write(&mut self, frame: &serde_json::Value);
    /// Read one line from the child, or `None` at EOF. Blocking with the
    /// transport's own timeout.
    fn read_line(&mut self) -> Option<String>;
}

/// Drive the login over `io` until it ends: returns the terminal state.
/// `on_started` runs once, when the start answer lands: with the Waiting
/// state (the browser URL) or, for `device`, the Device state (the code and
/// where to type it). Pure against [`LoginIo`] — the unit tests script the
/// whole RPC here.
pub fn drive_codex_login(
    io: &mut impl LoginIo,
    state: &mut CodexLogin,
    cancel: &AtomicBool,
    deadline: Instant,
    device: bool,
    mut on_started: impl FnMut(&CodexLogin),
) -> CodexLogin {
    let mut next_id: u64 = 1;
    io.write(&serde_json::json!({
        "id": next_id, "method": "initialize",
        // codex 0.144.6 refuses an `initialize` without `clientInfo.version`
        // (-32600 "missing field `version`"), and every request after it:
        // the sign-in never started live (found D5 B3).
        "params": {"clientInfo": {"name": "baaz", "title": "Baaz", "version": env!("CARGO_PKG_VERSION")},
                   "capabilities": {"experimentalApi": true}},
    }));
    next_id += 1;
    io.write(&serde_json::json!({"method": "initialized", "params": {}}));
    let start_id = next_id;
    io.write(&login_start_request(start_id, device));
    let mut started = false;
    loop {
        if cancel.load(Ordering::SeqCst) {
            state.cancel();
            return state.clone();
        }
        if Instant::now() >= deadline {
            *state = CodexLogin::Failed { reason: "The sign-in timed out.".into() };
            return state.clone();
        }
        let Some(line) = io.read_line() else {
            // EOF: the watchdog kills the child on Cancel or the deadline
            // (that is what unblocks a read waiting on the browser), so
            // name those first; anything else is a death mid-flow.
            if cancel.load(Ordering::SeqCst) {
                state.cancel();
            } else if Instant::now() >= deadline {
                *state = CodexLogin::Failed { reason: "The sign-in timed out.".into() };
            } else {
                *state = CodexLogin::Failed { reason: "The Codex helper exited.".into() };
            }
            return state.clone();
        };
        let Ok(frame) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        if is_login_completed_frame(&frame) {
            state.completed();
            return state.clone();
        }
        let is_start_answer =
            frame.get("id").and_then(serde_json::Value::as_u64) == Some(start_id);
        if is_start_answer && !started {
            let result = frame.get("result");
            let begun = if device {
                result.and_then(parse_login_start_device).map(|(url, code)| CodexLogin::Device { url, code })
            } else {
                result.and_then(parse_login_start_auth_url).map(|auth_url| CodexLogin::Waiting { auth_url })
            };
            match begun {
                Some(begun) => {
                    *state = begun;
                    on_started(state);
                    started = true;
                }
                None => {
                    let reason = frame
                        .get("error")
                        .and_then(|error| error.get("message"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("The sign-in did not start.");
                    *state = CodexLogin::Failed { reason: reason.into() };
                    return state.clone();
                }
            }
        }
    }
}

/// The `codex app-server` [`std::process::Command`] the login drives:
/// a pure builder — no process spawned — so tests assert the scrub on the
/// built command via `get_envs`.
///
/// The login verifies the owner's own account, so no Baaz `CODEX_HOME`
/// is set: the child reads the owner's real `~/.codex`, exactly what the
/// sign-in changes. The inherited desktop-agent env is still scrubbed
/// per-`Command` (see [`provider::child_env`]).
pub(crate) fn codex_login_command(program: &str) -> std::process::Command {
    let mut command = std::process::Command::new(program);
    command
        .arg("app-server")
        // The login-shell PATH with the program's own directory first, as
        // every lane gets: from the Dock an npm/nvm `codex` is an
        // `env node` script whose node is not on the inherited PATH.
        .env("PATH", provider::env_path::child_path_for(std::path::Path::new(program)))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    provider::child_env::scrub_command(&mut command);
    command
}

/// Run the real Codex login against `program`'s short-lived app-server and
/// return the terminal state. The browser flow opens `authUrl` itself; the
/// device flow (`device`) opens nothing — the person copies the code first.
/// `on_started` reports the Waiting / Device state the moment it is known,
/// so the row can offer "Copy the link" and show the code. Blocking; call it
/// off the UI thread.
pub fn run_codex_login(
    program: &str,
    cancel: &AtomicBool,
    device: bool,
    mut on_started: impl FnMut(&CodexLogin),
) -> CodexLogin {
    struct PipeIo {
        stdin: std::process::ChildStdin,
        lines: std::io::Lines<std::io::BufReader<std::process::ChildStdout>>,
    }
    impl LoginIo for PipeIo {
        fn write(&mut self, frame: &serde_json::Value) {
            use std::io::Write as _;
            if let Ok(line) = serde_json::to_string(frame) {
                let _ = writeln!(self.stdin, "{line}");
            }
        }
        fn read_line(&mut self) -> Option<String> {
            self.lines.next()?.ok()
        }
    }
    let spawned = codex_login_command(program).spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(error) => {
            return CodexLogin::Failed { reason: format!("Could not start Codex: {error}") };
        }
    };
    let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        return CodexLogin::Failed { reason: "Could not talk to Codex.".into() };
    };
    let mut io = PipeIo { stdin, lines: std::io::BufReader::new(stdout).lines() };
    let deadline = Instant::now() + CODEX_LOGIN_TIMEOUT;
    let mut state = CodexLogin::Idle;
    // The read below blocks while the person is in the browser, so it can
    // never notice Cancel or the deadline itself. A watchdog owns the child
    // and kills it on either (or once the drive finishes): the kill closes
    // stdout, the read returns EOF, and the drive names the reason.
    let child = std::sync::Mutex::new(child);
    let finished = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| loop {
            if finished.load(Ordering::SeqCst)
                || cancel.load(Ordering::SeqCst)
                || Instant::now() >= deadline
            {
                if let Ok(mut child) = child.lock() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
        });
        // The browser opens once, when the start answer names its URL.
        let terminal = drive_codex_login(&mut io, &mut state, cancel, deadline, device, |begun| {
            if let CodexLogin::Waiting { auth_url } = begun {
                let _ = crate::auth::open_in_browser(auth_url);
            }
            on_started(begun);
        });
        finished.store(true, Ordering::SeqCst);
        terminal
    })
}

// ------------------------------------------------------------ app wiring

use aui::screens::{connect_providers, ConnectIntent, ProviderAction, ProviderIntent};
use gpui::{AnyElement, AppContext as _, Context, Window};

use crate::app::Harness;
use crate::setup::SetupKind;

/// The statuses a boot renders from: scripted in deterministic mode (or all
/// Checking when nothing is scripted), the on-disk cache over Checking
/// defaults live. Never probes — probes only write the cache file, and the
/// stamp guard in [`Harness::sync_provider_state`] picks their results up.
pub(crate) fn initial_connect_statuses() -> Vec<ProviderStatus> {
    let mut statuses: HashMap<ProviderId, ProviderStatus> = ProviderId::all()
        .into_iter()
        .map(|id| (id, ProviderStatus::checking(id)))
        .collect();
    if crate::provider_status::deterministic() {
        if let Some(scripted) = crate::provider_status::scripted_statuses() {
            for status in scripted {
                statuses.insert(status.provider, status);
            }
        }
    } else {
        // The live service: seeded from the cache at boot, and the one copy
        // every probe publishes into (the file can lag it).
        for live in crate::provider_status::live_statuses() {
            statuses.insert(live.provider, live);
        }
    }
    ProviderId::all().into_iter().map(|id| statuses.remove(&id).expect("all ids seeded")).collect()
}

/// The provider-status cache's mtime, for the refresh guard. `None` when
/// the cache was never written (or in deterministic mode, where no file is
/// ever read).
pub(crate) fn status_stamp() -> StatusStamp {
    if crate::provider_status::deterministic() {
        return (None, 0);
    }
    let mtime = std::fs::metadata(crate::provider_status::cache_path())
        .and_then(|meta| meta.modified())
        .ok();
    (mtime, crate::provider_status::status_generation())
}

/// The part of the statuses a person can see: all of it but when each was
/// last checked — except whether it has been checked at all (Checking).
fn visible(statuses: &[ProviderStatus]) -> Vec<ProviderStatus> {
    statuses
        .iter()
        .cloned()
        .map(|mut status| {
            status.checked_at = status.checked_at.map(|_| 0);
            status
        })
        .collect()
}

/// Why Muse was not started at launch: the person has it off, or the last
/// probe found it missing. [`Harness::start_muse_once_available`] starts it
/// when that stops being true.
pub(crate) const MUSE_OFF: &str = "Muse is off or not installed";

/// What [`Harness::sync_provider_state`] compares: the cache file's mtime
/// (writes from usage readings and the Enabled switch) and the live
/// service's publish count (probe results, which also wake the window).
pub(crate) type StatusStamp = (Option<std::time::SystemTime>, u64);

impl Harness {
    /// The first-run Connect your providers screen (D5 B1): rows from the
    /// status service (Checking until probes land) in the board's order, the
    /// footer's default picker over the Connected providers, Continue
    /// (enabled once any provider is Connected) and Skip. The Muse sign-in
    /// sheet renders over it when its row asked for sign-in.
    pub(crate) fn render_connect(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        use gpui::prelude::*;
        let rows = build_rows(&self.connect_statuses, &self.codex_login);
        let current = ProviderId::parse(&self.new_provider);
        let picker = connect_default(&self.connect_statuses, current).map(|shown| aui::screens::DefaultPicker {
            current: aui_provider(shown),
            choices: connected_choices(&self.connect_statuses).into_iter().map(aui_provider).collect(),
            open: self.connect_menu_open,
        });
        let weak = cx.entity().downgrade();
        let mark = crate::mascot::connect_mascot(window, cx);
        let screen = connect_providers("connect", rows)
            .mark(mark)
            .default_picker(picker)
            .assist(self.codex_assist())
            .on_intent(move |intent, window, cx| {
                let _ = weak.update(cx, |this, cx| {
                    this.connect_intent(intent, window, cx);
                    cx.notify();
                });
            });
        let mut stack = gpui::div().size_full().relative().child(screen);
        if self.muse_sheet {
            stack = stack.child(self.muse_sheet_layer(window, cx));
        }
        stack = stack.children(self.render_setup_sheet(cx));
        stack.into_any_element()
    }

    /// The muted line under the rows while Codex signs in (D5 B3): "Browser
    /// didn't open? Copy the link · Use a code instead" while the browser
    /// flow waits; "Enter <code> at <host/path> · Copy code · Open page"
    /// once the person asked for a code (owner 2026-10-24: the code goes in
    /// this line, no new surface). Empty otherwise.
    pub(crate) fn codex_assist(&self) -> Vec<aui::screens::AssistRun> {
        codex_assist_runs(&self.codex_login)
    }

    /// The Muse sign-in sheet over the shell (the account menu's Sign in,
    /// Settings, the provider banner). `None` unless the sheet is up outside
    /// the connect screen, which renders its own.
    pub(crate) fn render_muse_sheet(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.muse_sheet || self.show_connect {
            return None;
        }
        Some(self.muse_sheet_layer(window, cx))
    }

    /// Muse's sign-in as a sheet (D5 B3): the setup sheet's frame at 460
    /// wide over the 45 % scrim — mark, "Sign in to Muse", ✕ — with the
    /// login's own steps inside. ✕ and Escape close it; the scrim does not.
    fn muse_sheet_layer(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        use gpui::prelude::*;
        let card = self.render_login_sheet(window, cx);
        let escape = cx.listener(|this: &mut Self, event: &gpui::KeyDownEvent, window, cx| {
            if event.keystroke.key == "escape" {
                this.close_muse_sheet(window, cx);
                cx.stop_propagation();
            }
        });
        gpui::div()
            .absolute()
            .inset_0()
            .capture_key_down(move |event, window, cx| escape(event, window, cx))
            .child(aui::screens::sheet_scrim("muse-sheet-scrim", card, None))
            .into_any_element()
    }

    /// Close Muse's sign-in sheet (✕, Escape): a flow it started is
    /// cancelled with it, so nothing keeps polling behind a closed sheet.
    pub(crate) fn close_muse_sheet(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.login.flow_running() {
            self.cancel_login_flow(cx);
        }
        self.muse_sheet = false;
        cx.notify();
    }

    /// Raise Muse's sign-in sheet from its first step, over whatever is on
    /// screen. Every Muse "Sign in" comes here: the Settings card and the
    /// "Signed out of Muse" dialog used to reset the login and drop the open
    /// session without ever raising the sheet, so nothing appeared
    /// (docs/65 §1). The open session stays; it waits on the sign-in.
    pub(crate) fn open_muse_sheet(&mut self) {
        self.login.reset_to_choose();
        self.muse_sheet = true;
    }

    /// Leave the connect screen: Continue and Skip both set
    /// `onboarding_completed`, so this launch and the next open the shell.
    pub(crate) fn finish_connect(&mut self) {
        complete_onboarding();
        self.show_connect = false;
        self.muse_sheet = false;
        self.connect_menu_open = false;
        // New sessions start where the footer said: the stored pick while it
        // is Connected, else the first Connected in the board's order.
        let statuses = crate::provider_status::live_statuses();
        let current = ProviderId::parse(&self.new_provider);
        if let Some(id) = connect_default(&statuses, current) {
            if id != current {
                self.new_provider = id.as_str().to_owned();
                crate::providers::write_last_provider(id);
            }
        } else if !crate::settings_providers::usable_provider(&statuses, current) {
            let id = crate::settings_providers::live_default_new_provider();
            self.new_provider = id.as_str().to_owned();
            crate::providers::write_last_provider(id);
        }
    }

    /// Reopen the connect screen over the shell (Settings → Providers →
    /// "Set up providers…"). Works for a returning person too: the rows
    /// start from the status cache at once, and [`Self::sync_provider_state`]
    /// keeps them fresh while the screen is up. The screen owns the whole
    /// window while it is up; the Settings page underneath keeps its
    /// section, so closing the screen lands back on it.
    pub(crate) fn open_connect_screen(&mut self, cx: &mut Context<Self>) {
        self.muse_sheet = false;
        self.connect_statuses = initial_connect_statuses();
        self.connect_status_stamp = status_stamp();
        self.show_connect = true;
        cx.notify();
    }

    /// One connect-screen intent: row buttons, Continue, Skip.
    fn connect_intent(&mut self, intent: ConnectIntent, window: &mut Window, cx: &mut Context<Self>) {
        match intent {
            ConnectIntent::Continue | ConnectIntent::Skip => self.finish_connect(),
            ConnectIntent::Provider(ProviderIntent { id, action }) => {
                self.connect_menu_open = false;
                self.connect_provider_action(ProviderId::parse(&id), action, window, cx);
            }
            ConnectIntent::ToggleDefaultMenu => self.connect_menu_open = !self.connect_menu_open,
            ConnectIntent::CloseDefaultMenu => self.connect_menu_open = false,
            ConnectIntent::SetDefault(provider) => {
                self.connect_menu_open = false;
                let id = ProviderId::from_icon(provider);
                self.new_provider = id.as_str().to_owned();
                crate::providers::write_last_provider(id);
            }
            ConnectIntent::Assist(key) => self.codex_assist_link(&key, cx),
        }
    }

    /// One row action. Terminal work (prefills) and the Codex login only
    /// park flags here — `on_frame` runs them with the window and the
    /// entity context; everything else applies at once.
    fn connect_provider_action(
        &mut self,
        id: ProviderId,
        action: ProviderAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            ProviderAction::SignIn => match id {
                ProviderId::Muse => {
                    self.open_muse_sheet();
                }
                ProviderId::Codex => {
                    if !matches!(self.codex_login, CodexLogin::Waiting { .. } | CodexLogin::Device { .. }) {
                        self.start_codex_login(false, cx);
                    }
                }
                ProviderId::ClaudeCode => self.open_setup_sheet(id, SetupKind::SignIn, window, cx),
            },
            ProviderAction::Install => self.open_setup_sheet(id, SetupKind::Install, window, cx),
            ProviderAction::Docs => {
                let _ = crate::auth::open_in_browser(docs_url(id));
            }
            ProviderAction::Update => self.open_setup_sheet(id, SetupKind::Update, window, cx),
            ProviderAction::Cancel => {
                if id == ProviderId::Codex {
                    self.codex_cancel.store(true, Ordering::SeqCst);
                }
            }
            ProviderAction::Recheck => crate::provider_status::reprobe_provider(id),
            // Connect rows offer no sign-out, switch or Details.
            ProviderAction::SignOut
            | ProviderAction::SetEnabled(_)
            | ProviderAction::ToggleDetails
            | ProviderAction::ChangePath
            | ProviderAction::ClearPath
            | ProviderAction::CopyDiagnostics => {}
        }
    }

    /// `codex-login:<waiting|device|failed>` (`--steps`): Codex's sign-in in
    /// that state with sample values, for captures. Starts nothing.
    pub(crate) fn step_codex_login(&mut self, rest: &str, cx: &mut Context<Self>) {
        self.codex_login = match rest {
            "waiting" => CodexLogin::Waiting { auth_url: "https://example.invalid/oauth/authorize".into() },
            "device" => CodexLogin::Device { url: "https://auth.openai.com/codex/device".into(), code: "K7QX-9P2M".into() },
            "failed" => CodexLogin::Failed { reason: "The sign-in timed out.".into() },
            _ => {
                crate::steps::record_step_failure(&format!("codex-login:{rest}"));
                return;
            }
        };
        cx.notify();
    }

    /// A link in the assist line (the connect screen's, or the Codex card's
    /// in Settings): copy the browser link, switch to a device code, copy the
    /// code, open the page the code goes in.
    pub(crate) fn codex_assist_link(&mut self, key: &str, cx: &mut Context<Self>) {
        match (key, &self.codex_login) {
            ("copy-link", CodexLogin::Waiting { auth_url }) if !auth_url.is_empty() => {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(auth_url.clone()));
            }
            ("use-code", CodexLogin::Waiting { .. }) => self.start_codex_login(true, cx),
            ("copy-code", CodexLogin::Device { code, .. }) => {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(code.clone()));
            }
            ("open-page", CodexLogin::Device { url, .. }) => {
                let _ = crate::auth::open_in_browser(url);
            }
            _ => {}
        }
    }

    /// Start the Codex Sign in flow: the Waiting row applies at once, the
    /// blocking app-server login runs off the UI thread, its start answer
    /// (the browser link, or the device code) reaches the row as it lands,
    /// and its terminal state re-probes Codex on success. `device` is "Use a
    /// code instead". A newer start (Use a code after the browser flow)
    /// supersedes the older one: its updates carry an older generation and
    /// are dropped.
    pub(crate) fn start_codex_login(&mut self, device: bool, cx: &mut Context<Self>) {
        let program = crate::providers::resolve_program(ProviderId::Codex)
            .map(|program| program.to_string_lossy().into_owned());
        let Some(program) = program else {
            self.codex_login = CodexLogin::Failed { reason: "Codex is not installed — Install first.".into() };
            cx.notify();
            return;
        };
        // A running flow is abandoned: its thread sees the old flag.
        self.codex_cancel.store(true, Ordering::SeqCst);
        self.codex_cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancel = Arc::clone(&self.codex_cancel);
        self.codex_generation += 1;
        let generation = self.codex_generation;
        self.codex_login = CodexLogin::Waiting { auth_url: String::new() };
        cx.notify();
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<CodexLogin>();
        cx.background_spawn(async move {
            let progress = tx.clone();
            let terminal = run_codex_login(&program, &cancel, device, move |begun| {
                let _ = progress.unbounded_send(begun.clone());
            });
            let _ = tx.unbounded_send(terminal);
        })
        .detach();
        cx.spawn(async move |this, cx| {
            use futures::StreamExt as _;
            while let Some(state) = rx.next().await {
                let alive = this.update(cx, |this, cx| {
                    if this.codex_generation != generation {
                        return;
                    }
                    if state == CodexLogin::Completed {
                        crate::provider_status::reprobe_provider(ProviderId::Codex);
                    }
                    this.codex_login = state;
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Follow the status cache while shown, and the sign-out banners while
    /// in the shell: background probes only write the cache file, so its
    /// mtime (or the scripted source, in deterministic mode) is the clock.
    pub(crate) fn sync_provider_state(&mut self, cx: &mut Context<Self>) {
        if crate::provider_status::deterministic() {
            let fresh = initial_connect_statuses();
            if fresh != self.connect_statuses {
                self.connect_statuses = fresh;
                self.sync_banners_from_statuses();
                cx.notify();
            }
            return;
        }
        let stamp = status_stamp();
        if stamp == self.connect_status_stamp {
            return;
        }
        self.connect_status_stamp = stamp;
        // Before the no-visible-change return: a Re-check that found what
        // the card already showed still owes the person its answer.
        self.settle_provider_busy(cx);
        let fresh = initial_connect_statuses();
        // A re-probe that found what the window already shows moves only
        // `checked_at`: nothing to redraw (nothing renders at rest, P0).
        if visible(&fresh) == visible(&self.connect_statuses) {
            self.connect_statuses = fresh;
            return;
        }
        self.connect_statuses = fresh;
        self.sync_banners_from_statuses();
        self.follow_default_provider();
        self.start_muse_once_available(cx);
        self.advance_setup_sheet(cx);
        cx.notify();
    }

    /// Muse was parked at launch as off or not installed, and a probe now
    /// finds it on and installed (installed since, or switched on in
    /// Settings): start it now. Before, its sign-in said "Muse is not
    /// running" until the next launch (docs/65 §1). A Muse that failed to
    /// start for any other reason keeps its own dialog and retry.
    fn start_muse_once_available(&mut self, cx: &mut Context<Self>) {
        if self.args.offline || self.args.replay.is_some() {
            return;
        }
        let parked = matches!(&self.wire, crate::app::Wire::Down(reason) if reason == MUSE_OFF);
        if parked
            && crate::provider_status::muse_wanted()
            && crate::provider_status::muse_known_installed()
        {
            self.wire = crate::app::Wire::Connecting;
            self.connect(cx);
        }
    }

    /// With no provider picked by the person (nothing stored, no
    /// `--provider`), the default follows the probes as they land: boot
    /// chose it from Checking statuses, which is a placeholder.
    fn follow_default_provider(&mut self) {
        if self.args.provider_explicit || crate::providers::read_last_provider().is_some() {
            return;
        }
        if let Some(id) = crate::settings_providers::default_new_provider(&self.connect_statuses) {
            if id.as_str() != self.new_provider {
                self.new_provider = id.as_str().to_owned();
            }
        }
    }

    /// The quiet banners: a provider the cache once called Connected that
    /// now reads Signed out gets "X is signed out — Sign in" on its
    /// composer; Connected clears it. Never a full-screen gate.
    fn sync_banners_from_statuses(&mut self) {
        for status in &self.connect_statuses {
            let id = status.provider;
            match status.headline() {
                crate::provider_status::Headline::Connected => {
                    self.seen_connected.insert(id);
                    if !self.typed_signed_out.contains(&id) {
                        self.provider_banners.remove(&id);
                    }
                    if id == ProviderId::Codex && matches!(self.codex_login, CodexLogin::Completed)
                    {
                        self.codex_login = CodexLogin::Idle;
                    }
                }
                crate::provider_status::Headline::SignedOut
                    if self.seen_connected.contains(&id) =>
                {
                    self.provider_banners.insert(id, signed_out_banner_text(id));
                }
                crate::provider_status::Headline::SignedOut => {}
                _ => {}
            }
        }
    }

    /// The quiet banner above the composer when the open session's provider
    /// is signed out (D5 B5): a warning dot, "<Provider> is signed out. Sign
    /// in to send.", Sign in. On the composer's measure, inside the band the
    /// transcript ends above. `None` anywhere else.
    pub(crate) fn render_provider_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        use gpui::prelude::*;
        let id = self.active.as_ref().map(|view| view.read(cx).provider_kind())?;
        let text = self.provider_banners.get(&id)?.clone();
        let weak = cx.entity().downgrade();
        Some(
            gpui::div()
                .w_full()
                .px(gpui::px(crate::session::TRANSCRIPT_PAD_X))
                .pb(gpui::px(10.0))
                .child(
                    gpui::div().w_full().max_w(gpui::px(crate::session::TRANSCRIPT_MEASURE)).mx_auto().child(
                        aui::screens::signed_out_banner("provider-signed-out", text).on_action(move |window, cx| {
                            let _ = weak.update(cx, |this, cx| this.banner_sign_in(id, window, cx));
                        }),
                    ),
                )
                .into_any_element(),
        )
    }

    /// The provider banner's Sign in: Muse opens the sheet, Codex starts
    /// (or cancels) its flow, Claude types its sign-in into the terminal.
    fn banner_sign_in(&mut self, id: ProviderId, window: &mut Window, cx: &mut Context<Self>) {
        // The person is acting on it: from here the status decides when the
        // banner goes (the next probe that reads Connected).
        self.typed_signed_out.remove(&id);
        // Window-needing work parks flags for `on_frame`, like the rows.
        let action = ProviderAction::SignIn;
        match id {
            ProviderId::Muse => {
                self.open_muse_sheet();
            }
            ProviderId::Codex => self.connect_provider_action(id, action, window, cx),
            ProviderId::ClaudeCode => self.connect_provider_action(id, action, window, cx),
        }
    }
}

#[cfg(test)]
mod tests {

    /// Cancel reaches a login blocked on a silent helper (the person is in
    /// the browser): the watchdog kills the child, the read unblocks, the
    /// row reads Cancelled — and no helper process outlives it.
    #[test]
    fn cancel_kills_a_login_blocked_on_a_silent_helper() {
        use std::sync::Arc;
        let dir = std::env::temp_dir().join(format!("baaz-codex-login-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let script = dir.join("codex");
        std::fs::write(&script, "#!/bin/sh\nexec sleep 30\n").expect("script");
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            flag.store(true, Ordering::SeqCst);
        });
        let started = Instant::now();
        let terminal = run_codex_login(&script.to_string_lossy(), &cancel, false, |_| {});
        assert_eq!(terminal, CodexLogin::Cancelled);
        assert!(started.elapsed() < Duration::from_secs(5), "cancel took {:?}", started.elapsed());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The login's `app-server` carries the scrub and keeps the owner's
    /// home: `CLAUDECODE` (scrubbed under either entrypoint flag) is an
    /// explicit removal on the built command, while no Baaz `CODEX_HOME`
    /// (or `CLAUDE_CONFIG_DIR`) is set — the sign-in reads and writes the
    /// owner's real `~/.codex`.
    #[test]
    fn the_login_command_is_scrubbed_and_keeps_the_owner_home() {
        let _lock = super::SCRUB_ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = std::env::var_os("CLAUDECODE");
        std::env::set_var("CLAUDECODE", "1");
        let command = codex_login_command("codex");
        if let Some(previous) = previous {
            std::env::set_var("CLAUDECODE", previous);
        } else {
            std::env::remove_var("CLAUDECODE");
        }
        let envs: Vec<_> = command.get_envs().collect();
        assert!(
            envs.iter().any(|(name, value)| *name == "CLAUDECODE" && value.is_none()),
            "the login removes the desktop inheritance: {envs:?}"
        );
        assert!(
            !envs.iter().any(|(name, _)| *name == "CODEX_HOME" || *name == "CLAUDE_CONFIG_DIR"),
            "one account per provider — the owner's home stands: {envs:?}"
        );
    }
    use super::*;
    use crate::provider_status::{Advisory, Headline, Installed};
    use gpui::prelude::*;
    use std::collections::VecDeque;

    /// A scripted [`LoginIo`]: the RPC the tests assert on, line by line.
    struct ScriptedIo {
        written: Vec<serde_json::Value>,
        lines: VecDeque<String>,
    }

    impl ScriptedIo {
        fn new(lines: Vec<&str>) -> Self {
            Self { written: Vec::new(), lines: lines.into_iter().map(str::to_owned).collect() }
        }
    }

    impl LoginIo for ScriptedIo {
        fn write(&mut self, frame: &serde_json::Value) {
            self.written.push(frame.clone());
        }
        fn read_line(&mut self) -> Option<String> {
            self.lines.pop_front()
        }
    }

    fn connected(id: ProviderId) -> ProviderStatus {
        ProviderStatus {
            provider: id,
            installed: Installed::yes("1.0", "/bin/x"),
            auth: Auth::SignedIn { email: Some("a@x.com".into()), plan: None, method: None, source: None },
            enabled: true,
            advisory: Advisory::None,
            checked_at: Some(1),
            usage: None,
        }
    }

    fn signed_out(id: ProviderId) -> ProviderStatus {
        ProviderStatus {
            provider: id,
            installed: Installed::yes("1.0", "/bin/x"),
            auth: Auth::SignedOut,
            enabled: true,
            advisory: Advisory::None,
            checked_at: Some(1),
            usage: None,
        }
    }

    #[test]
    fn a_first_run_shows_connect_and_a_returning_launch_shows_the_shell() {
        assert_eq!(decide_launch(true), LaunchDecision::Connect);
        assert_eq!(decide_launch(false), LaunchDecision::Shell);
    }

    #[test]
    fn a_returning_launch_without_any_cache_still_shows_the_shell() {
        // No cache row at all: the shell renders Checking rows, never a gate.
        let rows = build_rows(&[], &CodexLogin::Idle);
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|row| row.status == StatusLine::busy("Checking…")));
        assert_eq!(decide_launch(false), LaunchDecision::Shell);
    }

    #[test]
    fn continue_needs_one_connected_provider() {
        use ProviderId as P;
        assert!(!continue_enabled(&[]));
        assert!(!continue_enabled(&[signed_out(P::Muse), signed_out(P::Codex)]));
        assert!(continue_enabled(&[signed_out(P::Muse), connected(P::Codex)]));
    }

    #[test]
    fn skip_and_continue_both_end_first_run() {
        let sandbox = crate::provider_status::TestSandbox::hold();
        assert!(crate::provider_status::is_first_run());
        complete_onboarding();
        assert!(!crate::provider_status::is_first_run());
        assert!(crate::provider_status::onboarding_completed_path().exists());
        let _ = sandbox.state_dir();
    }

    #[test]
    fn a_pending_provider_never_reads_not_installed() {
        let mut status = ProviderStatus::checking(ProviderId::ClaudeCode);
        status.checked_at = None;
        assert_eq!(
            headline_for(&status),
            aui::screens::ProviderHeadline::Checking
        );
        let cached = ProviderStatus {
            provider: ProviderId::Codex,
            installed: Installed::No,
            auth: Auth::Unknown,
            enabled: true,
            advisory: Advisory::None,
            checked_at: Some(1),
            usage: None,
        };
        assert_eq!(
            headline_for(&cached),
            aui::screens::ProviderHeadline::NotInstalled
        );
    }

    #[test]
    fn rows_follow_the_boards_order_words_and_one_action() {
        use ProviderId as P;
        let rows = build_rows(&[connected(P::Muse), signed_out(P::Codex)], &CodexLogin::Idle);
        let ids: Vec<&str> = rows.iter().map(|row| row.id.as_ref()).collect();
        assert_eq!(ids, vec!["claude-code", "codex", "muse"], "Claude Code, Codex, Muse");
        let muse = rows.iter().find(|row| row.id == "muse").expect("muse row");
        assert!(muse.status.text.contains("a@x.com"), "{:?}", muse.status);
        assert!(!muse.status.text.starts_with("Connected"), "the ✓ says Connected; the line says who");
        assert!(muse.connected && muse.action.is_none(), "Connected shows ✓, no button");
        let codex = rows.iter().find(|row| row.id == "codex").expect("codex row");
        assert_eq!(codex.status, StatusLine::warn("Signed out"));
        assert!(codex.action.as_ref().is_some_and(|def| *def == ProviderActionDef::sign_in()));
    }

    #[test]
    fn too_old_and_timeouts_read_as_the_board_says() {
        let mut old = connected(ProviderId::ClaudeCode);
        old.installed = Installed::yes("2.0.9 (Claude Code)", "/bin/claude");
        old.advisory = Advisory::TooOld { need: "2.1.276".into() };
        let rows = build_rows(&[old], &CodexLogin::Idle);
        let claude = &rows[0];
        assert_eq!(claude.status, StatusLine::warn("2.0.9 is older than Baaz supports (2.1.276)"));
        assert_eq!(claude.action, Some(ProviderActionDef::update()));
        assert!(!claude.connected, "too old offers Update, not ✓");
        let mut slow = connected(ProviderId::Muse);
        slow.auth = Auth::Unknown;
        slow.advisory = Advisory::cant_run("timed out after 5s", true);
        let rows = build_rows(&[slow], &CodexLogin::Idle);
        let muse = rows.iter().find(|row| row.id == "muse").expect("muse row");
        assert_eq!(muse.status, StatusLine::bad("Couldn’t check — the CLI timed out"));
        assert_eq!(muse.action, Some(ProviderActionDef::recheck()));
    }

    #[test]
    fn the_footer_defaults_to_the_first_connected_in_row_order() {
        use ProviderId as P;
        // docs/65 §1 case (c): Muse installed but signed out, only Claude
        // Code signed in — new sessions start in Claude Code.
        let statuses = vec![signed_out(P::Muse), connected(P::ClaudeCode), connected(P::Codex)];
        assert_eq!(connect_default(&statuses, P::Muse), Some(P::ClaudeCode));
        assert_eq!(connect_default(&statuses, P::Codex), Some(P::Codex), "a Connected pick stays");
        assert_eq!(connected_choices(&statuses), vec![P::ClaudeCode, P::Codex], "Connected only, row order");
        assert_eq!(connect_default(&[signed_out(P::Muse)], P::Muse), None, "nothing ready");
        let mut off = connected(P::Codex);
        off.enabled = false;
        assert_eq!(connect_default(&[signed_out(P::Muse), off], P::Muse), None, "a disabled provider is no default");
    }

    #[test]
    fn a_codex_row_follows_its_sign_in() {
        let waiting = CodexLogin::Waiting { auth_url: "https://example.invalid/auth".into() };
        let rows = build_rows(&[signed_out(ProviderId::Codex)], &waiting);
        let codex = rows.iter().find(|row| row.id == "codex").expect("codex row");
        assert_eq!(codex.status, StatusLine::busy("Finish signing in with your browser…"));
        assert_eq!(codex.action, Some(ProviderActionDef::cancel()));
        let failed = CodexLogin::Failed { reason: "no browser".into() };
        let rows = build_rows(&[signed_out(ProviderId::Codex)], &failed);
        let codex = rows.iter().find(|row| row.id == "codex").expect("codex row");
        assert_eq!(codex.status, StatusLine::warn("no browser"));
        assert_eq!(codex.action, Some(ProviderActionDef::sign_in()), "try again");
    }

    #[test]
    fn the_codex_login_runs_start_waiting_completed_then_reprobes() {
        let mut io = ScriptedIo::new(vec![
            r#"{"id":2,"result":{"authUrl":"https://example.invalid/auth"}}"#,
            r#"{"method":"account/login/completed","params":{}}"#,
        ]);
        let cancel = AtomicBool::new(false);
        let mut state = CodexLogin::Idle;
        let terminal =
            drive_codex_login(&mut io, &mut state, &cancel, Instant::now() + Duration::from_secs(60), false, |_| {});
        assert_eq!(terminal, CodexLogin::Completed);
        assert_eq!(state, CodexLogin::Completed, "completed is terminal");
        // The start request went out as chatgpt on the short-lived server.
        assert!(io.written.iter().any(|frame| frame.get("method").and_then(
            serde_json::Value::as_str
        ) == Some("account/login/start")));
        let waiting_seen = CodexLogin::Waiting { auth_url: "https://example.invalid/auth".into() };
        assert_ne!(terminal, waiting_seen, "the run ends Completed, not Waiting");
    }

    /// "Use a code instead": the start asks for `chatgptDeviceCode` and the
    /// answer's code and page reach the row before the person finishes.
    #[test]
    fn the_device_code_flow_reports_the_code_then_completes() {
        let mut io = ScriptedIo::new(vec![
            r#"{"id":2,"result":{"type":"chatgptDeviceCode","loginId":"l1","userCode":"K7QX-9P2M","verificationUrl":"https://auth.openai.com/codex/device"}}"#,
            r#"{"method":"account/login/completed","params":{"success":true}}"#,
        ]);
        let cancel = AtomicBool::new(false);
        let mut state = CodexLogin::Idle;
        let mut seen = Vec::new();
        let terminal = drive_codex_login(&mut io, &mut state, &cancel, Instant::now() + Duration::from_secs(60), true, |begun| {
            seen.push(begun.clone())
        });
        assert_eq!(terminal, CodexLogin::Completed);
        assert_eq!(
            seen,
            vec![CodexLogin::Device { url: "https://auth.openai.com/codex/device".into(), code: "K7QX-9P2M".into() }]
        );
        let start = io
            .written
            .iter()
            .find(|frame| frame.get("method").and_then(serde_json::Value::as_str) == Some("account/login/start"))
            .expect("a start frame");
        assert_eq!(start["params"]["type"], "chatgptDeviceCode");
        let init = io.written.first().expect("initialize first");
        assert!(init["params"]["clientInfo"]["version"].is_string(), "codex 0.144.6 requires clientInfo.version");
    }

    #[test]
    fn the_assist_line_offers_the_link_then_the_code() {
        use aui::screens::AssistRun as R;
        assert!(codex_assist_runs(&CodexLogin::Idle).is_empty());
        assert!(codex_assist_runs(&CodexLogin::Waiting { auth_url: String::new() }).is_empty(), "no link yet");
        let waiting = codex_assist_runs(&CodexLogin::Waiting { auth_url: "https://auth".into() });
        assert!(waiting.contains(&R::Link("copy-link".into(), "Copy the link".into())));
        assert!(waiting.contains(&R::Link("use-code".into(), "Use a code instead".into())));
        let device = codex_assist_runs(&CodexLogin::Device {
            url: "https://auth.openai.com/codex/device".into(),
            code: "K7QX-9P2M".into(),
        });
        assert!(device.contains(&R::Code("K7QX-9P2M".into())));
        assert!(device.contains(&R::Text("at auth.openai.com/codex/device ·".into())));
        assert!(device.contains(&R::Link("copy-code".into(), "Copy code".into())));
    }

    #[test]
    fn cancel_ends_a_waiting_codex_login() {
        let mut state = CodexLogin::Waiting { auth_url: "https://example.invalid/auth".into() };
        state.cancel();
        assert_eq!(state, CodexLogin::Cancelled);
    }

    #[test]
    fn a_cancelled_codex_login_never_completes() {
        let mut io = ScriptedIo::new(vec![]);
        let cancel = AtomicBool::new(true);
        let mut state = CodexLogin::Idle;
        let terminal =
            drive_codex_login(&mut io, &mut state, &cancel, Instant::now() + Duration::from_secs(60), false, |_| {});
        assert_eq!(terminal, CodexLogin::Cancelled);
    }

    #[test]
    fn a_failed_start_fails_the_row_with_the_servers_reason() {
        let mut io = ScriptedIo::new(vec![
            r#"{"id":2,"error":{"code":-32603,"message":"no browser"}}"#,
        ]);
        let cancel = AtomicBool::new(false);
        let mut state = CodexLogin::Idle;
        let terminal =
            drive_codex_login(&mut io, &mut state, &cancel, Instant::now() + Duration::from_secs(60), false, |_| {});
        assert!(matches!(terminal, CodexLogin::Failed { .. }));
    }

    #[test]
    fn claude_sign_in_types_but_never_runs_and_installs_are_exact() {
        assert_eq!(
            install_command(ProviderId::ClaudeCode),
            Some("curl -fsSL https://claude.ai/install.sh | bash")
        );
        assert_eq!(install_command(ProviderId::Codex), Some("curl -fsSL https://chatgpt.com/codex/install.sh | sh"));
        assert_eq!(ProviderId::Codex.update_command(), Some("codex update"));
        assert_eq!(install_command(ProviderId::Muse), None);
        for id in ProviderId::all() {
            assert!(!docs_url(id).is_empty(), "{id:?} names its docs");
        }
    }

    #[test]
    fn a_missing_muse_offers_one_get_link_and_never_a_command() {
        // docs/65 S1: Settings once typed `curl … www.muse.dev/install.sh | bash`;
        // muse.dev is another product (it redirects to sonatype.com). Muse has
        // no install command anywhere, and its missing row is one link.
        assert_eq!(ProviderId::Muse.install_command(), None);
        for id in ProviderId::all() {
            assert!(!id.docs_url().contains("muse.dev"), "{id:?} must not link muse.dev");
        }
        assert_eq!(ProviderId::Muse.docs_url(), "https://dev.meta.ai/lp/muse-code");
        let mut status = ProviderStatus::checking(ProviderId::Muse);
        status.installed = crate::provider_status::Installed::No;
        status.checked_at = Some(1);
        let rows = build_rows(&[status.clone()], &CodexLogin::Idle);
        let muse = rows.iter().find(|row| row.id.as_ref() == "muse").expect("muse row");
        let action = muse.action.as_ref().expect("a way in");
        assert_eq!(action.label.as_ref(), "Get Muse");
        assert!(action.external, "Get Muse ↗ leaves the app");
        assert_eq!(action.style, aui::screens::ActionStyle::Secondary);
        let actions = crate::settings_providers::actions_for(&status);
        assert_eq!(
            actions.iter().map(|a| a.label.as_ref()).collect::<Vec<_>>(),
            vec!["Re-check", "Get Muse"],
            "Settings shows the one link, no Install"
        );
        // The other two install, with no Docs beside it (the board).
        let mut claude = ProviderStatus::checking(ProviderId::ClaudeCode);
        claude.installed = crate::provider_status::Installed::No;
        claude.checked_at = Some(1);
        let rows = build_rows(&[claude], &CodexLogin::Idle);
        let row = rows.iter().find(|row| row.id.as_ref() == "claude-code").expect("claude row");
        assert_eq!(row.action, Some(ProviderActionDef::install()));
    }

    #[test]
    fn the_signed_out_banner_names_the_provider_and_its_way_back() {
        assert_eq!(
            signed_out_banner_text(ProviderId::Codex),
            "Codex is signed out. Sign in to send."
        );
    }

    /// A bootable [`crate::Args`] pointed at a hermetic state dir, offline.
    fn harness_args(dir: &std::path::Path, login: crate::LoginSample) -> crate::Args {
        crate::Args {
            workspace: dir.to_path_buf(),
            workspace_explicit: true,
            provider: "echo".into(),
            provider_explicit: false,
            program: "muse".into(),
            theme: aui_tokens::ThemeKind::Dark,
            screenshot: None,
            delay: std::time::Duration::from_millis(500),
            session: None,
            send: None,
            offline: true,
            replay: None,
            steps: Vec::new(),
            tier: None,
            print_tier: false,
            approval_mode: None,
            login,
            login_steps: Vec::new(),
            bench: None,
            bench_cadence: std::time::Duration::from_millis(4),
            bench_scroll: crate::bench::BenchScroll::Sweep,
            bench_frames: 600,
            bench_out: None,
            bench_open_turn: false,
            bench_bare: false,
            bench_shell: false,
            sidebar_fixture: None,
            no_project: false,
            terminal_socket_dir: Some(crate::terminal::service::test_socket_dir()),
        }
    }

    /// The mixed script behind the connect captures: Muse Connected, Claude
    /// signed out, Codex missing.
    fn mixed_script() -> String {
        serde_json::json!([
            {"provider": "muse",
             "installed": {"Yes": {"version": "1.4.0", "path": "/usr/local/bin/muse"}},
             "auth": {"SignedIn": {"email": "ada@example.com", "plan": "Pro", "method": "account"}},
             "enabled": true, "advisory": "None", "checked_at": 1790000000, "usage": null},
            {"provider": "claude-code",
             "installed": {"Yes": {"version": "2.1.276", "path": "/opt/homebrew/bin/claude"}},
             "auth": "SignedOut",
             "enabled": true, "advisory": "None", "checked_at": 1790000000, "usage": null},
            {"provider": "codex", "installed": "No", "auth": "Unknown",
             "enabled": true, "advisory": "None", "checked_at": 1790000000, "usage": null},
        ])
        .to_string()
    }

    /// Hold the store env lock and point the state, determinism and script
    /// at a hermetic temp dir. Returns what the caller restores at the end.
    fn hermetic_scripted(
        script: Option<&str>,
    ) -> (std::sync::MutexGuard<'static, ()>, Option<std::ffi::OsString>, Option<std::ffi::OsString>, Option<std::ffi::OsString>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "baaz-connect-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("connect state dir");
        let guard = crate::store::test_env_lock();
        let old_state = std::env::var_os("BAAZ_STATE_DIR");
        let old_det = std::env::var_os("BAAZ_DETERMINISTIC");
        let old_script = std::env::var_os("BAAZ_PROVIDER_STATUS_SCRIPT");
        std::env::set_var("BAAZ_STATE_DIR", &dir);
        std::env::set_var("BAAZ_DETERMINISTIC", "1");
        match script {
            Some(script) => std::env::set_var("BAAZ_PROVIDER_STATUS_SCRIPT", script),
            None => std::env::remove_var("BAAZ_PROVIDER_STATUS_SCRIPT"),
        }
        (guard, old_state, old_det, old_script, dir)
    }

    fn restore_scripted(
        state: (std::sync::MutexGuard<'static, ()>, Option<std::ffi::OsString>, Option<std::ffi::OsString>, Option<std::ffi::OsString>, std::path::PathBuf),
    ) {
        let (guard, old_state, old_det, old_script, dir) = state;
        let _ = std::fs::remove_dir_all(&dir);
        match old_state {
            Some(value) => std::env::set_var("BAAZ_STATE_DIR", value),
            None => std::env::remove_var("BAAZ_STATE_DIR"),
        }
        match old_det {
            Some(value) => std::env::set_var("BAAZ_DETERMINISTIC", value),
            None => std::env::remove_var("BAAZ_DETERMINISTIC"),
        }
        match old_script {
            Some(value) => std::env::set_var("BAAZ_PROVIDER_STATUS_SCRIPT", value),
            None => std::env::remove_var("BAAZ_PROVIDER_STATUS_SCRIPT"),
        }
        drop(guard);
    }

    /// The `--login connect` capture boots into the connect screen with the
    /// scripted mixed rows, Continue gated open, and leaving (Continue or
    /// Skip) ends first run. Draws once, so a render panic fails here.
    #[gpui::test]
    fn the_connect_sample_boots_into_the_connect_screen(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext as _;
        let script = mixed_script();
        let held = hermetic_scripted(Some(&script));
        let dir = held.4.clone();
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| {
                crate::app::Harness::new(
                    harness_args(&dir, crate::LoginSample::Connect),
                    crate::shot::CaptureToken::default(),
                    window,
                    cx,
                )
            })
        });
        vc.update(|_, cx| {
            baaz.update(cx, |harness, _| {
                assert!(harness.show_connect, "the connect sample boots into the screen");
                assert_eq!(harness.connect_statuses.len(), 3);
                assert!(
                    crate::connect::continue_enabled(&harness.connect_statuses),
                    "Muse Connected gates Continue open"
                );
            })
        });
        vc.draw(
            gpui::point(gpui::px(0.), gpui::px(0.)),
            gpui::size(gpui::px(1440.), gpui::px(900.)),
            |_, _| baaz.clone().into_any_element(),
        );
        // Leaving ends first run — but the flag never writes in
        // deterministic mode, so drop the hermeticity first.
        std::env::remove_var("BAAZ_DETERMINISTIC");
        vc.update(|_, cx| {
            baaz.update(cx, |harness, _| {
                harness.finish_connect();
                assert!(!harness.show_connect, "Continue/Skip lands in the shell");
            })
        });
        assert!(
            !crate::provider_status::is_first_run(),
            "Continue/Skip sets onboarding_completed"
        );
        restore_scripted(held);
    }

    /// No script, no rows reported: every row reads Checking, Continue is
    /// gated shut, and the screen still draws.
    #[gpui::test]
    fn the_connect_screen_without_a_script_reads_checking(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext as _;
        let held = hermetic_scripted(None);
        let dir = held.4.clone();
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| {
                crate::app::Harness::new(
                    harness_args(&dir, crate::LoginSample::Connect),
                    crate::shot::CaptureToken::default(),
                    window,
                    cx,
                )
            })
        });
        vc.update(|_, cx| {
            baaz.update(cx, |harness, _| {
                assert!(harness.show_connect);
                assert!(!crate::connect::continue_enabled(&harness.connect_statuses));
            })
        });
        vc.draw(
            gpui::point(gpui::px(0.), gpui::px(0.)),
            gpui::size(gpui::px(1440.), gpui::px(900.)),
            |_, _| baaz.clone().into_any_element(),
        );
        restore_scripted(held);
    }

    /// The returning launch: cached statuses and no first run opens the
    /// shell, never the connect screen and never the sign-in screen.
    #[gpui::test]
    fn a_returning_launch_lands_in_the_shell(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext as _;
        let script = mixed_script();
        let held = hermetic_scripted(Some(&script));
        let dir = held.4.clone();
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| {
                crate::app::Harness::new(
                    harness_args(&dir, crate::LoginSample::SignedIn),
                    crate::shot::CaptureToken::default(),
                    window,
                    cx,
                )
            })
        });
        vc.update(|_, cx| {
            baaz.update(cx, |harness, _| {
                assert!(
                    !harness.show_connect,
                    "a returning launch never shows the connect screen"
                );
            })
        });
        vc.draw(
            gpui::point(gpui::px(0.), gpui::px(0.)),
            gpui::size(gpui::px(1440.), gpui::px(900.)),
            |_, _| baaz.clone().into_any_element(),
        );
        restore_scripted(held);
    }

    #[test]
    fn headline_mapping_covers_every_state() {
        use aui::screens::ProviderHeadline as H;
        use ProviderId as P;
        // Pending (never reported this launch) always reads Checking.
        assert_eq!(headline_for(&ProviderStatus::checking(P::Muse)), H::Checking);
        // Every reported state maps onto its own headline.
        let mut disabled = connected(P::Muse);
        disabled.enabled = false;
        assert_eq!(headline_for(&disabled), H::Disabled);
        let mut missing = connected(P::ClaudeCode);
        missing.installed = Installed::No;
        assert_eq!(headline_for(&missing), H::NotInstalled);
        let mut cant = connected(P::Codex);
        cant.advisory = Advisory::cant_run("boom", false);
        assert_eq!(headline_for(&cant), H::CantRun);
        assert_eq!(headline_for(&signed_out(P::Codex)), H::SignedOut);
        let mut unverified = connected(P::ClaudeCode);
        unverified.auth = Auth::Unverified;
        assert_eq!(headline_for(&unverified), H::Unverified);
        assert_eq!(headline_for(&connected(P::Muse)), H::Connected);
        let _ = Headline::Checking;
    }
}
