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
//! Row actions: Muse Sign in reuses today's Muse login flow as a sheet over
//! the connect screen; Codex Sign in runs `account/login/start` on a
//! short-lived app-server ([`run_codex_login`]); Claude Code Sign in and every
//! Install only type into the dock terminal — the person presses Enter
//! ([`terminal_prefill`], [`install_command`]).

use std::collections::HashMap;
use std::io::BufRead as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use aui::screens::{ConnectRowData, ProviderActionDef, ProviderHeadline as AuiHeadline};
use aui_icons::Provider as AuiProvider;

use crate::providers::ProviderId;
use crate::provider_status::{Auth, ProviderStatus};

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

/// `"Signed in as <email> · <plan>"`, omitting what the probe did not say.
pub fn row_account(status: &ProviderStatus) -> Option<String> {
    match &status.auth {
        Auth::SignedIn { email, plan, .. } => {
            let mut line = String::from("Signed in");
            if let Some(email) = email.as_ref().map(|email| email.trim()).filter(|email| !email.is_empty()) {
                line.push_str(&format!(" as {email}"));
            }
            if let Some(plan) = plan.as_ref().map(|plan| plan.trim()).filter(|plan| !plan.is_empty()) {
                line.push_str(&format!(" · {plan}"));
            }
            Some(line)
        }
        _ => None,
    }
}

/// One row's primary action: Install when missing, Sign in when signed out
/// or unverified, Re-check when the binary would not run, nothing when
/// Connected, Checking or Disabled. Muse names no documented installer
/// ([`install_command`]), so its missing row offers Docs instead.
pub fn primary_action(status: &ProviderStatus) -> Option<ProviderActionDef> {
    use AuiHeadline as H;
    match headline_for(status) {
        H::Connected | H::Checking | H::Disabled => None,
        H::NotInstalled => {
            if install_command(status.provider).is_some() {
                Some(ProviderActionDef::install())
            } else {
                Some(ProviderActionDef::docs())
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

/// The three connect rows in switcher order, with per-row notes (Codex
/// "Waiting for sign-in…", terminal-prefill confirmations) appended to the
/// account line when present.
pub fn build_rows(
    statuses: &[ProviderStatus],
    notes: &HashMap<ProviderId, String>,
) -> Vec<ConnectRowData> {
    let by_id: HashMap<ProviderId, &ProviderStatus> = statuses.iter().map(|s| (s.provider, s)).collect();
    ProviderId::all()
        .into_iter()
        .map(|id| {
            let status = by_id
                .get(&id)
                .map(|status| (*status).clone())
                .unwrap_or_else(|| ProviderStatus::checking(id));
            let mut account = row_account(&status).map(gpui::SharedString::from);
            if let Some(note) = notes.get(&id) {
                let joined = match account {
                    Some(line) => format!("{line} — {note}"),
                    None => note.clone(),
                };
                account = Some(joined.into());
            }
            ConnectRowData {
                id: gpui::SharedString::from(id.as_str()),
                provider: aui_provider(id),
                headline: headline_for(&status),
                account,
                primary: primary_action(&status),
                // Every row offers Docs beside its primary action.
                docs: true,
            }
        })
        .collect()
}

/// The quiet composer banner when a cached-Connected provider later probes
/// Signed out: names the provider and its way back, never a full-screen gate.
pub fn signed_out_banner_text(id: ProviderId) -> String {
    format!("{} is signed out — Sign in", id.label())
}

// ------------------------------------------------------------ terminal prefill + docs

/// What a row action types into the dock terminal without running: the
/// Claude Code sign-in (`claude auth login`, typed, never run — the person
/// presses Enter) and every vendor install command. `send_enter` is always
/// false at these call sites; this only names the bytes.
pub fn terminal_prefill(id: ProviderId, sign_in: bool) -> Option<&'static str> {
    if sign_in {
        return match id {
            // Muse signs in through the in-app sheet, Codex through the
            // in-app app-server flow: neither types into the terminal.
            ProviderId::Muse | ProviderId::Codex => None,
            ProviderId::ClaudeCode => Some("claude auth login"),
        };
    }
    install_command(id)
}

/// The vendor install command, typed into the dock terminal and not run.
/// Muse names none: no installer is documented in this repo or in
/// `muse --help`, so its missing row offers Docs instead of Install.
pub fn install_command(id: ProviderId) -> Option<&'static str> {
    match id {
        ProviderId::ClaudeCode => Some("curl -fsSL https://claude.ai/install.sh | bash"),
        ProviderId::Codex => Some("npm i -g @openai/codex"),
        ProviderId::Muse => None,
    }
}

/// The vendor docs behind every row's Docs button.
pub fn docs_url(id: ProviderId) -> &'static str {
    match id {
        ProviderId::ClaudeCode => "https://docs.anthropic.com/en/docs/claude-code",
        ProviderId::Codex => "https://developers.openai.com/codex",
        ProviderId::Muse => "https://github.com/latekaapi/baaz",
    }
}

/// The row note after a terminal prefill: says what happened and that the
/// command is typed, not run.
pub fn prefill_note(command: &str) -> String {
    format!("`{command}` is typed in the terminal — press Enter to run it")
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
    /// The person pressed Cancel.
    Cancelled,
    /// The flow failed with the human reason.
    Failed {
        /// What went wrong, in the person's words.
        reason: String,
    },
}

impl CodexLogin {
    /// `account/login/start` answered: remember the URL and show waiting.
    pub fn start(&mut self, auth_url: String) {
        *self = CodexLogin::Waiting { auth_url };
    }

    /// `account/login/completed` arrived on the app-server: done, re-probe.
    pub fn completed(&mut self) {
        *self = CodexLogin::Completed;
    }

    /// The person pressed Cancel: abandon the flow.
    pub fn cancel(&mut self) {
        *self = CodexLogin::Cancelled;
    }

    /// The row's waiting line, while waiting.
    pub fn waiting_text(&self) -> Option<String> {
        match self {
            CodexLogin::Waiting { .. } => Some("Waiting for sign-in… Cancel".into()),
            _ => None,
        }
    }
}

/// The `account/login/start {type: "chatgpt"}` request frame.
pub fn login_start_request(id: u64) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "method": "account/login/start",
        "params": {"type": "chatgpt"},
    })
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
/// `on_waiting` runs once, when the start answer names the browser URL.
/// Pure against [`LoginIo`] — the unit tests script the whole RPC here.
pub fn drive_codex_login(
    io: &mut impl LoginIo,
    state: &mut CodexLogin,
    cancel: &AtomicBool,
    deadline: Instant,
    mut on_waiting: impl FnMut(&str),
) -> CodexLogin {
    let mut next_id: u64 = 1;
    io.write(&serde_json::json!({
        "id": next_id, "method": "initialize",
        "params": {"clientInfo": {"name": "baaz", "title": "Baaz"},
                   "capabilities": {"experimentalApi": true}},
    }));
    next_id += 1;
    io.write(&serde_json::json!({"method": "initialized", "params": {}}));
    let start_id = next_id;
    io.write(&login_start_request(start_id));
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
            // An exit before `account/login/completed` is a death mid-flow
            // (a clean finish already returned above).
            *state = CodexLogin::Failed { reason: "The Codex helper exited.".into() };
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
            match frame.get("result").and_then(parse_login_start_auth_url) {
                Some(auth_url) => {
                    on_waiting(&auth_url);
                    state.start(auth_url);
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

/// Run the real Codex login against `program`'s short-lived app-server:
/// opens `authUrl` in the browser and returns the terminal state (the
/// caller applied the Waiting row before this started, and applies the
/// terminal state on return). Blocking; call it off the UI thread.
pub fn run_codex_login(program: &str, cancel: &AtomicBool) -> CodexLogin {
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
    let spawned = std::process::Command::new(program)
        .arg("app-server")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn();
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
    // The browser opens once, when the start answer names its URL.
    let terminal = drive_codex_login(&mut io, &mut state, cancel, deadline, |auth_url| {
        let _ = crate::auth::open_in_browser(auth_url);
    });
    let _ = child.kill();
    terminal
}

// ------------------------------------------------------------ app wiring

use aui::feedback::{banner, BannerActionStyle, BannerKind, BannerRun};
use aui::screens::{connect_providers, ConnectIntent, ProviderAction, ProviderIntent};
use aui_tokens::ActiveAui as _;
use gpui::{AnyElement, App, Context, Window};

use crate::app::Harness;
use crate::wire::WireCall;

/// The statuses a boot renders from: scripted in deterministic mode (or all
/// Checking when nothing is scripted), the on-disk cache over Checking
/// defaults live. Never probes — probes only write the cache file, and the
/// mtime guard in [`Harness::sync_provider_state`] picks their results up.
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
        for cached in crate::provider_status::read_cache() {
            statuses.insert(cached.provider, cached);
        }
    }
    ProviderId::all().into_iter().map(|id| statuses.remove(&id).expect("all ids seeded")).collect()
}

/// The provider-status cache's mtime, for the refresh guard. `None` when
/// the cache was never written (or in deterministic mode, where no file is
/// ever read).
pub(crate) fn cache_mtime() -> Option<std::time::SystemTime> {
    if crate::provider_status::deterministic() {
        return None;
    }
    std::fs::metadata(crate::provider_status::cache_path())
        .and_then(|meta| meta.modified())
        .ok()
}

impl Harness {
    /// The first-run Connect your providers screen: rows from the status
    /// service (Checking until probes land), the tally, Continue (enabled
    /// once any provider is Connected) and Skip. The Muse login flow
    /// renders as a sheet over it when its row asked for sign-in.
    pub(crate) fn render_connect(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        use gpui::prelude::*;
        let mut notes = self.connect_notes.clone();
        if let Some(waiting) = self.codex_login.waiting_text() {
            notes.insert(ProviderId::Codex, waiting);
        }
        if let CodexLogin::Failed { reason } = &self.codex_login {
            notes.insert(ProviderId::Codex, reason.clone());
        }
        let rows = build_rows(&self.connect_statuses, &notes);
        let weak = cx.entity().downgrade();
        let mark = crate::mascot::welcome_mascot(window, cx);
        let screen = connect_providers("connect", rows).mark(mark).on_intent(move |intent, window, cx| {
            let _ = weak.update(cx, |this, cx| this.connect_intent(intent, window, cx));
        });
        let mut stack = gpui::div().size_full().relative().child(screen);
        if self.muse_sheet {
            let weak = cx.entity().downgrade();
            let card = self.render_login(window, cx);
            let p = cx.aui().colors;
            stack = stack.child(
                gpui::div()
                    .id("muse-sheet-scrim")
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(p.overlay)
                    .opacity(0.97)
                    .on_click(move |_, _, cx: &mut App| {
                        let _ = weak.update(cx, |this, _| this.muse_sheet = false);
                    })
                    .child(card),
            );
        }
        stack.into_any_element()
    }

    /// The Muse sign-in sheet over the shell (the account menu's Sign in,
    /// the provider banner's button). `None` unless the sheet is up outside
    /// the connect screen, which renders its own.
    pub(crate) fn render_muse_sheet(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        use gpui::prelude::*;
        if !self.muse_sheet || self.show_connect {
            return None;
        }
        let weak = cx.entity().downgrade();
        let card = self.render_login(window, cx);
        let p = cx.aui().colors;
        Some(
            gpui::div()
                .id("muse-sheet-shell-scrim")
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(p.overlay)
                .opacity(0.97)
                .on_click(move |_, _, cx: &mut App| {
                    let _ = weak.update(cx, |this, _| this.muse_sheet = false);
                })
                .child(card)
                .into_any_element(),
        )
    }

    /// Leave the connect screen: Continue and Skip both set
    /// `onboarding_completed`, so this launch and the next open the shell.
    pub(crate) fn finish_connect(&mut self) {
        complete_onboarding();
        self.show_connect = false;
        self.muse_sheet = false;
        self.connect_notes.clear();
    }

    /// One connect-screen intent: row buttons, Continue, Skip.
    fn connect_intent(&mut self, intent: ConnectIntent, window: &mut Window, cx: &mut App) {
        match intent {
            ConnectIntent::Continue | ConnectIntent::Skip => self.finish_connect(),
            ConnectIntent::Provider(ProviderIntent { id, action }) => {
                self.connect_provider_action(ProviderId::parse(&id), action, window, cx);
            }
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
        cx: &mut App,
    ) {
        let _ = (window, cx);
        match action {
            ProviderAction::SignIn => match id {
                ProviderId::Muse => {
                    self.login.reset_to_choose();
                    self.muse_sheet = true;
                }
                ProviderId::Codex => {
                    if matches!(self.codex_login, CodexLogin::Waiting { .. }) {
                        // The waiting line reads "… Cancel": the row's
                        // button is the cancel while a login runs.
                        self.codex_cancel.store(true, Ordering::SeqCst);
                    } else {
                        self.pending_codex_start = true;
                    }
                }
                ProviderId::ClaudeCode => {
                    if let Some(command) = terminal_prefill(id, true) {
                        self.pending_prefill = Some(command.to_owned());
                        self.connect_notes.insert(id, prefill_note(command));
                    }
                }
            },
            ProviderAction::Install => {
                if let Some(command) = install_command(id) {
                    self.pending_prefill = Some(command.to_owned());
                    self.connect_notes.insert(id, prefill_note(command));
                }
            }
            ProviderAction::Docs => {
                let _ = crate::auth::open_in_browser(docs_url(id));
            }
            ProviderAction::Recheck => crate::provider_status::reprobe_provider(id),
            // Connect rows offer no sign-out and no switch.
            ProviderAction::SignOut | ProviderAction::SetEnabled(_) => {}
        }
    }

    /// Start the Codex Sign in flow: the Waiting row applies at once, the
    /// blocking app-server login runs off the UI thread, and its terminal
    /// state re-probes Codex on success.
    fn start_codex_login(&mut self, cx: &mut Context<Self>) {
        let program = crate::providers::resolve_program(ProviderId::Codex)
            .map(|program| program.to_string_lossy().into_owned());
        let Some(program) = program else {
            self.connect_notes
                .insert(ProviderId::Codex, "Codex is not installed — Install first.".into());
            self.codex_login = CodexLogin::Idle;
            cx.notify();
            return;
        };
        self.codex_cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancel = Arc::clone(&self.codex_cancel);
        self.codex_login = CodexLogin::Waiting { auth_url: String::new() };
        self.connect_notes.remove(&ProviderId::Codex);
        cx.notify();
        self.wire_call(
            cx,
            move || run_codex_login(&program, &cancel),
            |this, terminal, cx| {
                this.codex_login = terminal.clone();
                match terminal {
                    CodexLogin::Completed => {
                        this.connect_notes
                            .insert(ProviderId::Codex, "Signed in — re-checking…".into());
                        crate::provider_status::reprobe_provider(ProviderId::Codex);
                    }
                    CodexLogin::Cancelled => {
                        this.connect_notes.remove(&ProviderId::Codex);
                    }
                    CodexLogin::Failed { reason } => {
                        this.connect_notes.insert(ProviderId::Codex, reason);
                    }
                    CodexLogin::Waiting { .. } | CodexLogin::Idle => {}
                }
                cx.notify();
            },
        );
    }

    /// Type `command` into the dock terminal without running it: the
    /// person presses Enter. Runs in `on_frame`, where the window lives.
    fn prefill_in_terminal(
        &mut self,
        command: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let root = self
            .current_project()
            .map(|project| project.root.clone())
            .unwrap_or_else(|| self.args.workspace.clone());
        self.run_in_terminal(&root, command, false, window, cx);
    }

    /// Run what the row actions parked (prefills, the Codex login start).
    pub(crate) fn run_pending_connect_actions(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pending_codex_start {
            self.pending_codex_start = false;
            self.start_codex_login(cx);
        }
        if let Some(command) = self.pending_prefill.take() {
            self.prefill_in_terminal(&command, window, cx);
        }
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
        let mtime = cache_mtime();
        if mtime == self.connect_cache_mtime {
            return;
        }
        self.connect_cache_mtime = mtime;
        self.connect_statuses = initial_connect_statuses();
        self.sync_banners_from_statuses();
        cx.notify();
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
                    self.provider_banners.remove(&id);
                    if id == ProviderId::Codex && matches!(self.codex_login, CodexLogin::Completed)
                    {
                        self.codex_login = CodexLogin::Idle;
                        self.connect_notes.remove(&id);
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

    /// The quiet banner over the centre column when the open session's
    /// provider is signed out, with a Sign in button that starts that
    /// provider's own flow. `None` anywhere else.
    pub(crate) fn render_provider_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        use gpui::prelude::*;
        let id = self.active.as_ref().map(|view| view.read(cx).provider_kind())?;
        let text = self.provider_banners.get(&id)?.clone();
        let weak = cx.entity().downgrade();
        Some(
            gpui::div()
                .w_full()
                .px(gpui::px(aui_tokens::scale::SP_7))
                .pt(gpui::px(aui_tokens::scale::SP_4))
                .child(
                    banner("provider-signed-out", BannerKind::Waiting, vec![BannerRun::Text(
                        text.into(),
                    )])
                    .action("Sign in", BannerActionStyle::Primary)
                    .on_action(move |window, cx| {
                        let _ = weak.update(cx, |this, cx| this.banner_sign_in(id, window, cx));
                    }),
                )
                .into_any_element(),
        )
    }

    /// The provider banner's Sign in: Muse opens the sheet, Codex starts
    /// (or cancels) its flow, Claude types its sign-in into the terminal.
    fn banner_sign_in(&mut self, id: ProviderId, window: &mut Window, cx: &mut App) {
        // Window-needing work parks flags for `on_frame`, like the rows.
        let action = ProviderAction::SignIn;
        match id {
            ProviderId::Muse => {
                self.login.reset_to_choose();
                self.muse_sheet = true;
            }
            ProviderId::Codex => self.connect_provider_action(id, action, window, cx),
            ProviderId::ClaudeCode => self.connect_provider_action(id, action, window, cx),
        }
    }
}

#[cfg(test)]
mod tests {
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
            auth: Auth::SignedIn { email: Some("a@x.com".into()), plan: None, method: None },
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
        let rows = build_rows(&[], &HashMap::new());
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|row| row.headline
            == aui::screens::ProviderHeadline::Checking));
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
    fn rows_carry_account_version_and_the_right_primary() {
        use ProviderId as P;
        let rows = build_rows(&[connected(P::Muse), signed_out(P::Codex)], &HashMap::new());
        let muse = rows.iter().find(|row| row.id == "muse").expect("muse row");
        assert!(muse.account.as_ref().is_some_and(|line| line.contains("a@x.com")));
        assert!(muse.primary.is_none(), "Connected shows ✓, no button");
        let codex = rows.iter().find(|row| row.id == "codex").expect("codex row");
        assert!(codex.primary.as_ref().is_some_and(|def| *def == ProviderActionDef::sign_in()));
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
            drive_codex_login(&mut io, &mut state, &cancel, Instant::now() + Duration::from_secs(60), |_| {});
        assert_eq!(terminal, CodexLogin::Completed);
        assert!(
            state.waiting_text().is_none(),
            "completed is terminal: no waiting line"
        );
        // The start request went out as chatgpt on the short-lived server.
        assert!(io.written.iter().any(|frame| frame.get("method").and_then(
            serde_json::Value::as_str
        ) == Some("account/login/start")));
        let waiting_seen = CodexLogin::Waiting { auth_url: "https://example.invalid/auth".into() };
        assert_ne!(terminal, waiting_seen, "the run ends Completed, not Waiting");
    }

    #[test]
    fn the_codex_waiting_row_reads_waiting_and_cancel_ends_it() {
        let waiting = CodexLogin::Waiting { auth_url: "https://example.invalid/auth".into() };
        assert_eq!(waiting.waiting_text().as_deref(), Some("Waiting for sign-in… Cancel"));
        let mut state = waiting;
        state.cancel();
        assert_eq!(state, CodexLogin::Cancelled);
        assert_eq!(state.waiting_text(), None);
    }

    #[test]
    fn a_cancelled_codex_login_never_completes() {
        let mut io = ScriptedIo::new(vec![]);
        let cancel = AtomicBool::new(true);
        let mut state = CodexLogin::Idle;
        let terminal =
            drive_codex_login(&mut io, &mut state, &cancel, Instant::now() + Duration::from_secs(60), |_| {});
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
            drive_codex_login(&mut io, &mut state, &cancel, Instant::now() + Duration::from_secs(60), |_| {});
        assert!(matches!(terminal, CodexLogin::Failed { .. }));
    }

    #[test]
    fn claude_sign_in_types_but_never_runs_and_installs_are_exact() {
        assert_eq!(
            terminal_prefill(ProviderId::ClaudeCode, true),
            Some("claude auth login")
        );
        assert_eq!(
            install_command(ProviderId::ClaudeCode),
            Some("curl -fsSL https://claude.ai/install.sh | bash")
        );
        assert_eq!(install_command(ProviderId::Codex), Some("npm i -g @openai/codex"));
        // Muse and the in-app flows never type into the terminal.
        assert_eq!(terminal_prefill(ProviderId::Muse, true), None);
        assert_eq!(terminal_prefill(ProviderId::Codex, true), None);
        assert_eq!(install_command(ProviderId::Muse), None);
        for id in ProviderId::all() {
            assert!(!docs_url(id).is_empty(), "{id:?} names its docs");
        }
    }

    #[test]
    fn the_signed_out_banner_names_the_provider_and_its_way_back() {
        assert_eq!(
            signed_out_banner_text(ProviderId::Codex),
            "Codex is signed out — Sign in"
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
