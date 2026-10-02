//! Settings → Providers: one card per provider from the status service
//! (design `docs/23-providers-connect.md` §4).
//!
//! The Settings page's Providers section is a `Note` plus one Enabled
//! switch per provider (`provider-enabled:<wire>` ids, flipping through
//! [`Harness::flip_provider_enabled`]). The section's page — the overview
//! plus one sub-page per provider
//! ([`Harness::render_settings_content`](crate::app::Harness::render_settings_content))
//! — renders one [`provider_card`](aui::screens::provider_card) per
//! provider from the live statuses.
//!
//! The overview ends in a "Set up providers…" row into the first-run
//! connect screen ([`Harness::open_connect_screen`]), which also serves
//! a returning person — the rows refresh from the status cache.
//!
//! Sign-in/out never touch the owner's real accounts from automation:
//! the actions exist in the UI, but verification runs them against the
//! scripted [`AuthCommander`] below, and every binary run sets
//! `BAAZ_STATE_DIR=$(mktemp -d)`.

use aui::data::button;
use aui::overlay::{SettingsRow, SettingsSection};
use aui::screens::{ProviderAction, ProviderActionDef, ProviderCardData, ProviderHeadline, ProviderIntent};
use aui_tokens::{scale, ActiveAui, AuiStyled};
use gpui::{prelude::*, px, AnyElement, Context, SharedString, Window};
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::switch::Switch;

use crate::app::Harness;
use crate::overlays::{Dialog, DialogAction};
use crate::provider_status::{Auth, Headline, ProviderStatus};
use crate::providers::ProviderId;

/// The Settings rail section id. `settings:providers` opens it.
pub(crate) const PROVIDERS_SECTION_ID: &str = "providers";

/// One card per state, as the spec's action table says: Connected →
/// Sign out + Re-check; Signed out → Sign in + Re-check; Not installed →
/// Install + Docs; Can't run → Re-check + Docs. Checking and Disabled
/// only re-check (the switch beside them is the action); Unverified
/// offers Sign in + Re-check like Signed out.
pub(crate) fn actions_for(headline: Headline) -> Vec<ProviderActionDef> {
    match headline {
        Headline::Connected => vec![ProviderActionDef::sign_out(), ProviderActionDef::recheck()],
        Headline::SignedOut | Headline::Unverified => {
            vec![ProviderActionDef::sign_in(), ProviderActionDef::recheck()]
        }
        Headline::NotInstalled => vec![ProviderActionDef::install(), ProviderActionDef::docs()],
        Headline::CantRun => vec![ProviderActionDef::recheck(), ProviderActionDef::docs()],
        Headline::Checking | Headline::Disabled => vec![ProviderActionDef::recheck()],
    }
}

/// The status headline in the card library's spelling.
pub(crate) fn card_headline(headline: Headline) -> ProviderHeadline {
    match headline {
        Headline::Checking => ProviderHeadline::Checking,
        Headline::Connected => ProviderHeadline::Connected,
        Headline::SignedOut => ProviderHeadline::SignedOut,
        Headline::NotInstalled => ProviderHeadline::NotInstalled,
        Headline::CantRun => ProviderHeadline::CantRun,
        Headline::Unverified => ProviderHeadline::Unverified,
        Headline::Disabled => ProviderHeadline::Disabled,
    }
}

/// `"Signed in as <email> · <plan>"`, degrading gracefully when the
/// probe reported only one half. `None` unless signed in.
pub(crate) fn account_line(auth: &Auth) -> Option<String> {
    let Auth::SignedIn { email, plan, .. } = auth else {
        return None;
    };
    match (email, plan) {
        (Some(email), Some(plan)) => Some(format!("Signed in as {email} · {plan}")),
        (Some(email), None) => Some(format!("Signed in as {email}")),
        (None, Some(plan)) => Some(format!("Signed in · {plan}")),
        (None, None) => Some("Signed in".to_owned()),
    }
}

/// The version chip and the advisory line. A `TooOld` advisory reads
/// `"Too old — need ≥ <need>"`; a `CantRun` advisory surfaces its
/// excerpt (timeouts name the timeout).
pub(crate) fn version_parts(status: &ProviderStatus) -> (Option<String>, Option<String>) {
    let version = match &status.installed {
        crate::provider_status::Installed::Yes { version, .. } if !version.is_empty() => {
            Some(version.clone())
        }
        _ => None,
    };
    let advisory = match &status.advisory {
        crate::provider_status::Advisory::None => None,
        crate::provider_status::Advisory::TooOld { need } => {
            Some(format!("Too old — need ≥ {need}"))
        }
        crate::provider_status::Advisory::CantRun { stderr, timed_out } => {
            if *timed_out {
                Some(format!("Couldn't check — {stderr}"))
            } else {
                Some(stderr.clone())
            }
        }
    };
    (version, advisory)
}

/// The full card data for one status: headline, account, version +
/// advisory, the Enabled switch, and the state actions. Pure, so tests
/// drive every state without a window.
pub(crate) fn card_data(status: &ProviderStatus) -> ProviderCardData {
    let headline = status.headline();
    let (version, advisory) = version_parts(status);
    ProviderCardData {
        id: SharedString::from(status.provider.as_str()),
        provider: status.provider.icon(),
        headline: card_headline(headline),
        account: account_line(&status.auth).map(SharedString::from),
        version: version.map(SharedString::from),
        advisory: advisory.map(SharedString::from),
        enabled: status.enabled,
        actions: actions_for(headline),
    }
}

/// The providers a composer menu may list: the enabled ones, in
/// switcher order. A disabled provider disappears from the composer's
/// provider menu and from the new-session provider choice.
pub(crate) fn visible_provider_ids(statuses: &[ProviderStatus]) -> Vec<ProviderId> {
    ProviderId::all().into_iter().filter(|id| statuses.iter().any(|s| s.provider == *id && s.enabled)).collect()
}

/// The live-service version of [`visible_provider_ids`].
pub(crate) fn live_visible_provider_ids() -> Vec<ProviderId> {
    visible_provider_ids(&crate::provider_status::live_statuses())
}

/// The quiet banner an opened session on a disabled provider shows
/// instead of starting a child.
pub(crate) fn disabled_banner(id: ProviderId) -> String {
    format!("{} is disabled in Settings — Enable", id.label())
}

/// The install command typed (not run) into the dock terminal.
pub(crate) fn install_command(id: ProviderId) -> &'static str {
    match id {
        ProviderId::ClaudeCode => "curl -fsSL https://claude.ai/install.sh | bash",
        ProviderId::Codex => "npm i -g @openai/codex",
        ProviderId::Muse => "curl -fsSL https://www.muse.dev/install.sh | bash",
    }
}

/// Where the card's Docs button goes.
pub(crate) fn docs_url(id: ProviderId) -> &'static str {
    match id {
        ProviderId::ClaudeCode => "https://docs.anthropic.com/en/docs/claude-code",
        ProviderId::Codex => "https://developers.openai.com/codex",
        ProviderId::Muse => "https://www.muse.dev/docs",
    }
}

// ------------------------------------------------------------ sign out

/// What confirming sign-out says: the title names the provider, the
/// detail names the real command — a CLI sign-out signs the CLI out on
/// this Mac, not just Baaz out of it.
pub(crate) fn signout_confirm(id: ProviderId) -> (String, String) {
    let (title, detail) = match id {
        ProviderId::ClaudeCode => (
            format!("Sign out of {}?", id.label()),
            "This runs `claude auth logout` — Claude Code will be signed out on this Mac, not just in Baaz."
                .to_owned(),
        ),
        ProviderId::Codex => (
            format!("Sign out of {}?", id.label()),
            "This runs Codex `account/logout` — Codex will be signed out on this Mac, not just in Baaz."
                .to_owned(),
        ),
        ProviderId::Muse => (
            format!("Sign out of {}?", id.label()),
            "This runs Muse `account/logout` — Muse will be signed out on this Mac, not just in Baaz."
                .to_owned(),
        ),
    };
    (title, detail)
}

/// The sign-out surface behind [`run_sign_out`]: scripted in tests, real
/// processes and RPC in production. Never run against the owner's real
/// accounts except through the UI's own confirm.
pub trait AuthCommander: Send + Sync {
    /// Run `program` with `args` (Claude's `auth logout`).
    fn run_shell(&self, program: &str, args: &[&str]) -> Result<String, String>;
    /// Codex `account/logout` over a short-lived app-server.
    fn codex_logout(&self, program: &str) -> Result<(), String>;
    /// Muse's logout path (`account/logout` on the existing connection).
    fn muse_logout(&self);
}

/// What [`run_sign_out`] will do, for tests to assert before it runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignOutStep {
    /// A real subprocess: `claude auth logout`.
    Shell {
        /// The binary.
        program: String,
        /// Its argv.
        args: Vec<String>,
    },
    /// Codex `account/logout` against this binary.
    CodexLogout {
        /// The resolved `codex` binary.
        program: String,
    },
    /// Muse's `account/logout` on the existing connection.
    MuseLogout,
}

/// The steps signing `id` out takes (exactly one).
pub(crate) fn signout_steps(
    id: ProviderId,
    codex_program: Option<std::path::PathBuf>,
) -> Vec<SignOutStep> {
    match id {
        ProviderId::ClaudeCode => vec![SignOutStep::Shell {
            program: "claude".into(),
            args: vec!["auth".into(), "logout".into()],
        }],
        ProviderId::Codex => vec![SignOutStep::CodexLogout {
            program: codex_program
                .map(|program| program.to_string_lossy().into_owned())
                .unwrap_or_else(|| "codex".into()),
        }],
        ProviderId::Muse => vec![SignOutStep::MuseLogout],
    }
}

/// Run the sign-out steps through `commander`. The caller confirms first
/// and re-probes after.
pub(crate) fn run_sign_out(id: ProviderId, commander: &dyn AuthCommander) -> Result<(), String> {
    let codex_program = crate::provider_status::binary_path(id);
    for step in signout_steps(id, codex_program) {
        match step {
            SignOutStep::Shell { program, args } => {
                let argv: Vec<&str> = args.iter().map(String::as_str).collect();
                commander.run_shell(&program, &argv).map(|_| ())?;
            }
            SignOutStep::CodexLogout { program } => commander.codex_logout(&program)?,
            SignOutStep::MuseLogout => commander.muse_logout(),
        }
    }
    Ok(())
}

/// The production commander: a real `claude auth logout` subprocess and
/// a real short-lived app-server `account/logout`. (Muse's logout rides
/// the existing connection, so its arm is a no-op here — the caller runs
/// it on the UI thread instead.)
pub struct LiveCommander;

/// The auth-subprocess [`std::process::Command`] (`claude auth logout`
/// and its kin): a pure builder — no process spawned — so tests assert
/// the scrub on the built command via `get_envs`.
///
/// Sign-out changes the owner's own account, so no Baaz
/// `CLAUDE_CONFIG_DIR` is set: the child reads the owner's real
/// `~/.claude`, exactly what the sign-out clears. The inherited
/// desktop-agent env is still scrubbed per-`Command` (see
/// [`provider::child_env`]).
pub(crate) fn auth_command(program: &str, args: &[&str]) -> std::process::Command {
    let mut command = std::process::Command::new(program);
    command.args(args);
    provider::child_env::scrub_command(&mut command);
    command
}

impl AuthCommander for LiveCommander {
    fn run_shell(&self, program: &str, args: &[&str]) -> Result<String, String> {
        auth_command(program, args)
            .output()
            .map_err(|error| format!("could not run {program}: {error}"))
            .and_then(|output| {
                if output.status.success() {
                    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
                } else {
                    Err(format!(
                        "{program} answered {}: {}",
                        output.status,
                        String::from_utf8_lossy(&output.stderr).trim()
                    ))
                }
            })
    }

    fn codex_logout(&self, program: &str) -> Result<(), String> {
        codex_account_logout(program)
    }

    fn muse_logout(&self) {}
}

/// Codex `account/logout` over a short-lived app-server: handshake, one
/// request, hang up. Blocking; call it off the UI thread.
pub(crate) fn codex_account_logout(program: &str) -> Result<(), String> {
    use crossbeam_channel::unbounded;
    use provider_codex::child::RunningChild;
    use provider_codex::fold::CodexFold;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    let (events, _dropped) = unbounded();
    let mut child = RunningChild::spawn(
        program,
        &[],
        Arc::new(Mutex::new(CodexFold::new())),
        events,
    )
    .map_err(|error| format!("could not spawn {program} app-server: {error}"))?;
    let timeout = Duration::from_secs(12);
    let result = (|| {
        child
            .send_frame(provider_codex::child::initialize_request(
                child.next_request_id(),
                env!("CARGO_PKG_VERSION"),
            ))
            .map_err(|error| error.reason)?;
        child
            .send_notification(
                "initialized",
                provider_codex::child::initialized_notification()["params"].clone(),
            )
            .map_err(|error| format!("the session child is unreachable: {error}"))?;
        child
            .send_request_with_timeout("account/logout", serde_json::json!({}), timeout)
            .map(|_| ())
            .map_err(|error| error.reason)
    })();
    child.shutdown();
    result
}

// ------------------------------------------------------------- sign in

/// How the card's Sign in starts, per provider. Y5's flows are not on
/// this branch, so these are the spec's fallbacks: Muse reuses today's
/// login screen (which becomes its sign-in sheet), Claude prefills the
/// dock terminal with `claude auth login` (typed, not run), Codex starts
/// `account/login/start {type: chatgpt}` and opens the returned URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignInPlan {
    /// Back to the login screen's method choice (Muse's sign-in sheet).
    MuseSheet,
    /// Prefill the dock terminal with this command, typed not run.
    ClaudeTerminal {
        /// The command to type.
        command: String,
    },
    /// Start the ChatGPT login and open this provider's auth URL.
    CodexBrowser,
}

/// The sign-in start for `id`. Pure, so tests drive it.
pub(crate) fn signin_plan(id: ProviderId) -> SignInPlan {
    match id {
        ProviderId::Muse => SignInPlan::MuseSheet,
        ProviderId::ClaudeCode => SignInPlan::ClaudeTerminal { command: "claude auth login".into() },
        ProviderId::Codex => SignInPlan::CodexBrowser,
    }
}

// ------------------------------------------------------------------ UI

/// The Providers section: a note plus one Enabled switch per provider,
/// reading the live statuses. The switches flip through
/// [`Harness::flip_provider_enabled`] (`provider-enabled:<wire>` ids,
/// routed by [`parse_enabled_row`]); the cards live on the Providers
/// overview and sub-pages
/// ([`Harness::render_settings_content`](crate::app::Harness::render_settings_content)).
pub(crate) fn providers_section() -> SettingsSection {
    let statuses = crate::provider_status::live_statuses();
    let mut rows = vec![SettingsRow::Note {
        text: SharedString::from(
            "One card per provider — sign in or out, re-check, or switch one off. Details on the Providers page.",
        ),
    }];
    for status in &statuses {
        rows.push(SettingsRow::Switch {
            id: SharedString::from(format!("provider-enabled:{}", status.provider.as_str())),
            label: SharedString::from(format!("Enable {}", status.provider.label())),
            detail: Some(SharedString::from(status.headline_text())),
            on: status.enabled,
        });
    }
    SettingsSection {
        id: SharedString::from(PROVIDERS_SECTION_ID),
        label: SharedString::from("Providers"),
        rows,
    }
}

/// Whether row id `id` is a provider Enabled switch, and which provider
/// it names. Pure, so tests drive it without a window.
pub(crate) fn parse_enabled_row(id: &str) -> Option<ProviderId> {
    let wire = id.strip_prefix("provider-enabled:")?;
    let parsed = ProviderId::parse(wire);
    // `parse` falls back to Muse: only accept the wire id round-trip.
    (parsed.as_str() == wire).then_some(parsed)
}

/// The providers whose sessions Baaz scopes down to its own tools — the
/// only cards that gain a "Use my own MCP servers" switch (Z5). Muse
/// rides the legacy pump and never spawns through the scoped argv.
pub(crate) fn own_mcp_providers() -> [ProviderId; 2] {
    [ProviderId::ClaudeCode, ProviderId::Codex]
}

/// The switch label, identical on both cards.
pub(crate) fn use_own_mcp_label() -> &'static str {
    "Use my own MCP servers"
}

/// The switch detail per provider: what off/on means, plus the
/// sessions-started-after-the-change sentence the task requires the row
/// to carry. Pure, so tests drive it without a window.
pub(crate) fn use_own_mcp_detail(id: ProviderId) -> &'static str {
    match id {
        ProviderId::Codex => "Off: sessions see only Baaz's tools. On: your ~/.codex/config.toml \
            servers and plugins load too. Applies to sessions started after this change.",
        ProviderId::ClaudeCode => "Off: sessions see only Baaz's tools. On: your Claude Code MCP \
            servers and connectors load too. Applies to sessions started after this change.",
        ProviderId::Muse => "Muse sessions always ride the legacy pump.",
    }
}

/// The switch state for `id` in `layout`. Muse has no switch (see
/// [`own_mcp_providers`]) and reads false.
pub(crate) fn use_own_mcp_state(layout: &crate::layout::Layout, id: ProviderId) -> bool {
    match id {
        ProviderId::Codex => layout.use_own_mcp.codex,
        ProviderId::ClaudeCode => layout.use_own_mcp.claude_code,
        ProviderId::Muse => false,
    }
}

impl Harness {
    /// Flip a provider Enabled switch from either the section rows or a
    /// card. Persists through the status cache; a disabled provider
    /// stops being probed and leaves the composer menu.
    pub(crate) fn flip_provider_enabled(&mut self, id: ProviderId, on: bool, cx: &mut Context<Self>) {
        crate::provider_status::set_provider_enabled(id, on);
        // A re-enable re-probes at once so its card stops reading stale;
        // a disable needs no probe (probes skip disabled providers).
        if on {
            crate::provider_status::recheck_provider(id);
        }
        // A disabled pick is never the default for new sessions again.
        if !on && self.new_provider == id.as_str() {
            let fallback = live_first_visible().unwrap_or(ProviderId::Muse);
            self.new_provider = fallback.as_str().to_owned();
            crate::providers::write_last_provider(fallback);
        }
        cx.notify();
    }

    /// Flip a "Use my own MCP servers" switch and persist it. Sessions
    /// started after the flip build their argv with (on) or without (off)
    /// Baaz's scoping; live sessions keep the argv they spawned with.
    pub(crate) fn flip_use_own_mcp(&mut self, id: ProviderId, on: bool, cx: &mut Context<Self>) {
        match id {
            ProviderId::Codex => self.layout.use_own_mcp.codex = on,
            ProviderId::ClaudeCode => self.layout.use_own_mcp.claude_code = on,
            ProviderId::Muse => return,
        }
        crate::layout::write(&self.layout);
        cx.notify();
    }

    /// The "Use my own MCP servers" row under the Claude Code / Codex
    /// card: label + detail left, the switch right, flipping through
    /// [`Self::flip_use_own_mcp`]. Shared with the Settings provider
    /// sub-pages (B11).
    pub(crate) fn use_own_mcp_row(&self, id: ProviderId, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.aui().colors;
        let on = use_own_mcp_state(&self.layout, id);
        let flip = cx.listener(move |this: &mut Self, next: &bool, _, cx| {
            this.flip_use_own_mcp(id, *next, cx);
        });
        // Inset like the card above it: the card's padding plus its 20 px
        // provider mark and gap, so this row's text starts under the
        // provider's name and its switch sits under the card's switch.
        h_flex()
            .w_full()
            .items_center()
            .gap(px(scale::SP_3))
            .pl(px(scale::SP_4 + 20.0 + scale::SP_2))
            .pr(px(scale::SP_4))
            .child(
                v_flex()
                    .flex_1()
                    .min_w(px(0.0))
                    .child(
                        gpui::div()
                            .text_color(p.ink)
                            .ui(scale::FS_13)
                            .child(use_own_mcp_label()),
                    )
                    .child(
                        gpui::div()
                            .text_color(p.ink_3)
                            .ui(scale::FS_12)
                            .child(use_own_mcp_detail(id)),
                    ),
            )
            .child(
                Switch::new(format!("providers-use-own-mcp-{}", id.as_str()))
                    .checked(on)
                    .color(p.accent)
                    .accessibility_label(SharedString::from(format!(
                        "{} · {}",
                        use_own_mcp_label(),
                        id.label()
                    )))
                    .on_click(move |next, window, cx| flip(next, window, cx)),
            )
            .into_any_element()
    }

    /// The B4M migration row under the cards, or `None` when nothing
    /// waits: the "Move N …" title, the dry-run count with an expandable
    /// path list, and the Move button through the same journaling
    /// executor the one-time prompt uses. Reads only the cached plan —
    /// never the owner-home walk (see `MigrationCache::plan_for_render`).
    /// Both buttons carry accessibility labels, like every Settings
    /// switch.
    pub(crate) fn migration_row(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let empty: &[crate::session_migration::PlannedMove] = &[];
        let plan = self.migration_cache.as_ref().map(|cache| cache.plan_for_render()).unwrap_or(empty);
        if plan.is_empty() {
            return None;
        }
        let p = cx.aui().colors;
        let (n_claude, n_codex) = crate::session_migration::counts(plan);
        let title = crate::session_migration::prompt_title(n_claude, n_codex);
        let paths_label = if self.migration_paths_expanded { "Hide paths" } else { "Show paths" };
        let paths_toggle_label = format!("{paths_label} ({} files)", plan.len());
        let go = cx.listener(|this: &mut Self, _: &(), _, cx| this.confirm_session_migration(cx));
        let toggle = cx.listener(|this: &mut Self, _: &(), _, cx| this.toggle_migration_paths(cx));
        let mut text = v_flex().flex_1().min_w(px(0.0)).child(
            gpui::div().text_color(p.ink).ui(scale::FS_13).semibold().child(SharedString::from(title)),
        ).child(
            gpui::div().text_color(p.ink_3).ui(scale::FS_12).child(
                "Baaz's own sessions still live in the owner's homes. Moving keeps resume working; \
                 nothing is deleted. Codex for Mac may keep listing moved threads until archived there.",
            ),
        );
        if self.migration_paths_expanded {
            let mut list = v_flex().gap(px(scale::SP_1)).pt(px(scale::SP_1));
            for path in crate::session_migration::row_paths(plan) {
                list = list.child(
                    gpui::div()
                        .text_color(p.ink_3)
                        .ui(scale::FS_12)
                        .child(SharedString::from(path)),
                );
            }
            text = text.child(list);
        }
        Some(
            h_flex()
                .w_full()
                .items_start()
                .gap(px(scale::SP_3))
                .p(px(scale::SP_3))
                .rounded(px(scale::R_SM))
                .border_1()
                .border_color(p.line_strong)
                .child(text)
                .child(
                    v_flex()
                        .gap(px(scale::SP_2))
                        .child(
                            button("migration-move", "Move")
                                .primary()
                                .accessibility_label("Move sessions into Baaz")
                                .on_click(move |_, window, cx| go(&(), window, cx)),
                        )
                        .child(
                            button("migration-paths", paths_toggle_label.clone())
                                .ghost()
                                .accessibility_label(paths_toggle_label)
                                .on_click(move |_, window, cx| toggle(&(), window, cx)),
                        ),
                )
                .into_any_element(),
        )
    }

    /// One provider intent from a card: switch, re-check, sign in/out,
    /// install, docs.
    pub(crate) fn handle_provider_intent(
        &mut self,
        intent: ProviderIntent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = ProviderId::parse(intent.id.as_ref());
        match intent.action {
            ProviderAction::SetEnabled(on) => self.flip_provider_enabled(id, on, cx),
            ProviderAction::Recheck => {
                crate::provider_status::recheck_provider(id);
                cx.notify();
            }
            ProviderAction::SignIn => self.begin_provider_signin(id, window, cx),
            ProviderAction::SignOut => self.ask_provider_signout(id, cx),
            ProviderAction::Install => {
                self.open_terminal_typed(install_command(id), window, cx);
                self.overlays.update(cx, |overlays, _| {
                    overlays.toast(
                        format!("Install {}", id.label()),
                        "The install command is typed in the terminal — press Enter to run it.",
                    );
                });
            }
            ProviderAction::Docs => {
                cx.open_url(docs_url(id));
            }
        }
    }

    /// Raise the sign-out confirm. The detail names the real command, so
    /// the person knows this signs the CLI out on this Mac.
    pub(crate) fn ask_provider_signout(&mut self, id: ProviderId, cx: &mut Context<Self>) {
        let (title, detail) = signout_confirm(id);
        self.set_dialog(
            cx,
            Dialog {
                title,
                detail,
                kind: aui::overlay::DialogKind::Warning,
                primary: "Sign out",
                action: DialogAction::ProviderSignOut,
                archive_target: Some(id.as_str().to_owned()),
            },
        );
    }

    /// Run a confirmed sign-out, then re-probe so the card follows.
    /// Sign-outs run off the UI thread; the re-probe lands after.
    pub(crate) fn confirm_provider_signout(
        &mut self,
        id: ProviderId,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_dialog(cx);
        if id == ProviderId::Muse {
            // Muse's logout path: `account/logout` on the existing
            // connection, applied like any other sign-out.
            self.logout(cx);
            crate::provider_status::refresh_muse_status();
            return;
        }
        // The CLI sign-outs run off the UI thread through the same seam
        // the tests script; the card re-probes when the logout lands.
        std::thread::spawn(move || {
            let commander = LiveCommander;
            match run_sign_out(id, &commander) {
                Ok(()) => crate::provider_status::recheck_provider(id),
                Err(error) => eprintln!("baaz: sign out of {}: {error}", id.as_str()),
            }
        });
        cx.notify();
    }

    /// Start a sign-in: the Muse sheet, the Claude terminal prefill, or
    /// the Codex browser flow.
    pub(crate) fn begin_provider_signin(
        &mut self,
        id: ProviderId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match signin_plan(id) {
            SignInPlan::MuseSheet => {
                self.close_settings(cx);
                self.auth = crate::login::Auth::SignedOut;
                self.active = None;
                self.login.reset_to_choose();
                cx.notify();
            }
            SignInPlan::ClaudeTerminal { command } => {
                self.open_terminal_typed(&command, window, cx);
                self.overlays.update(cx, |overlays, _| {
                    overlays.toast(
                        "Sign in to Claude Code",
                        "`claude auth login` is typed in the terminal — press Enter and follow it there.",
                    );
                });
            }
            SignInPlan::CodexBrowser => {
                self.codex_browser_signin(window, cx);
            }
        }
    }

    /// Codex ChatGPT sign-in: `account/login/start`, open the returned
    /// URL, then poll `account/read` until the login completes and
    /// re-probe. All blocking work stays off the UI thread.
    fn codex_browser_signin(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let program =
            crate::provider_status::binary_path(ProviderId::Codex).unwrap_or_else(|| "codex".into());
        let program = program.to_string_lossy().into_owned();
        std::thread::spawn(move || {
            match codex_login_start(&program) {
                Ok(url) => {
                    if let Err(error) = crate::auth::open_in_browser(&url) {
                        eprintln!("baaz: codex sign-in: could not open the browser: {error}");
                        return;
                    }
                    // Completion: poll `account/read` until the login
                    // lands (up to ~2 minutes), then re-probe.
                    for _ in 0..24 {
                        std::thread::sleep(std::time::Duration::from_secs(5));
                        if codex_signed_in(&program) {
                            break;
                        }
                    }
                    crate::provider_status::recheck_provider(ProviderId::Codex);
                }
                Err(error) => eprintln!("baaz: codex account/login/start: {error}"),
            }
        });
        self.overlays.update(cx, |overlays, _| {
            overlays.toast(
                "Sign in to Codex",
                "Complete sign-in in the browser — the card re-checks when it lands.",
            );
        });
    }

    /// Open the dock terminal with `command` typed but not run: the
    /// person presses Enter, for installs and `claude auth login` alike.
    pub(crate) fn open_terminal_typed(
        &mut self,
        command: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.layout.terminal_open = true;
        let Some(root) = self.current_project().map(|project| project.root.clone()) else {
            // No project: still open the dock so the command has a home;
            // without a root there is no tab to type into.
            self.open_terminal_tab(None, window, cx);
            cx.notify();
            return;
        };
        if self.terminal_host.read(cx).tabs_for(&root).is_empty() {
            let origin = self.active.as_ref().map(|view| view.read(cx).session_id.clone());
            self.terminal_host.update(cx, |host, cx| {
                host.open(&root, "shell".to_owned(), crate::terminal::TabOwner::User, origin, cx);
            });
        }
        self.open_terminal_tab(None, window, cx);
        if let Some(session) = self.terminal_host.read(cx).active_for(&root).map(|tab| tab.session.clone()) {
            let bytes = command.as_bytes().to_vec();
            session.update(cx, |session, _| session.write(&bytes));
        }
        cx.notify();
    }
}

/// The first enabled provider in switcher order, for falling back when
/// the default's own provider is switched off.
pub(crate) fn live_first_visible() -> Option<ProviderId> {
    live_visible_provider_ids().into_iter().next()
}

/// Start Codex's ChatGPT login and return the browser URL:
/// `account/login/start {type: chatgpt}` over a short-lived app-server.
/// Blocking; call it off the UI thread.
pub(crate) fn codex_login_start(program: &str) -> Result<String, String> {
    use crossbeam_channel::unbounded;
    use provider_codex::child::RunningChild;
    use provider_codex::fold::CodexFold;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    let (events, _dropped) = unbounded();
    let mut child = RunningChild::spawn(
        program,
        &[],
        Arc::new(Mutex::new(CodexFold::new())),
        events,
    )
    .map_err(|error| format!("could not spawn {program} app-server: {error}"))?;
    let timeout = Duration::from_secs(30);
    let result = (|| {
        child
            .send_frame(provider_codex::child::initialize_request(
                child.next_request_id(),
                env!("CARGO_PKG_VERSION"),
            ))
            .map_err(|error| error.reason)?;
        child
            .send_notification(
                "initialized",
                provider_codex::child::initialized_notification()["params"].clone(),
            )
            .map_err(|error| format!("the session child is unreachable: {error}"))?;
        let started = child
            .send_request_with_timeout(
                "account/login/start",
                serde_json::json!({"type": "chatgpt"}),
                timeout,
            )
            .map_err(|error| error.reason)?;
        started
            .get("authUrl")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| "account/login/start answered without an authUrl".to_owned())
    })();
    child.shutdown();
    result
}

/// Whether Codex `account/read` reports a login right now. Blocking.
fn codex_signed_in(program: &str) -> bool {
    provider_codex::probe::probe_app_server(program, std::time::Duration::from_secs(12))
        .map(|probe| probe.account.signed_in)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_status::{Advisory, Auth, Installed, ProviderStatus};

    fn status(
        provider: ProviderId,
        installed: Installed,
        auth: Auth,
        enabled: bool,
    ) -> ProviderStatus {
        ProviderStatus {
            provider,
            installed,
            auth,
            enabled,
            advisory: Advisory::None,
            checked_at: Some(1),
            usage: None,
        }
    }

    fn connected() -> ProviderStatus {
        status(
            ProviderId::Codex,
            Installed::yes("0.144.6", "/bin/codex"),
            Auth::SignedIn {
                email: Some("ada@example.com".into()),
                plan: Some("pro".into()),
                method: Some("chatgpt".into()),
            },
            true,
        )
    }

    #[test]
    fn connected_card_names_email_plan_and_switch() {
        let data = card_data(&connected());
        assert_eq!(data.headline, ProviderHeadline::Connected);
        assert_eq!(
            data.account.as_deref(),
            Some("Signed in as ada@example.com · pro")
        );
        assert_eq!(data.version.as_deref(), Some("0.144.6"));
        assert!(data.enabled);
        let labels: Vec<&str> =
            data.actions.iter().map(|def| def.label.as_ref()).collect();
        assert_eq!(labels, vec!["Sign out", "Re-check"]);
    }

    #[test]
    fn signed_out_card_signs_in() {
        let data = card_data(&status(
            ProviderId::ClaudeCode,
            Installed::yes("2.1.276", "/bin/claude"),
            Auth::SignedOut,
            true,
        ));
        assert_eq!(data.headline, ProviderHeadline::SignedOut);
        assert_eq!(data.account, None);
        let labels: Vec<&str> =
            data.actions.iter().map(|def| def.label.as_ref()).collect();
        assert_eq!(labels, vec!["Sign in", "Re-check"]);
    }

    #[test]
    fn missing_card_installs() {
        let data = card_data(&status(
            ProviderId::Muse,
            Installed::No,
            Auth::Unknown,
            true,
        ));
        assert_eq!(data.headline, ProviderHeadline::NotInstalled);
        let labels: Vec<&str> =
            data.actions.iter().map(|def| def.label.as_ref()).collect();
        assert_eq!(labels, vec!["Install", "Docs"]);
    }

    #[test]
    fn cant_run_card_rechecks() {
        let mut cant = status(
            ProviderId::Codex,
            Installed::yes("", "/bin/codex"),
            Auth::Unknown,
            true,
        );
        cant.advisory = Advisory::cant_run("boom", false);
        let data = card_data(&cant);
        assert_eq!(data.headline, ProviderHeadline::CantRun);
        let labels: Vec<&str> =
            data.actions.iter().map(|def| def.label.as_ref()).collect();
        assert_eq!(labels, vec!["Re-check", "Docs"]);
    }

    #[test]
    fn enabled_row_ids_round_trip() {
        assert_eq!(parse_enabled_row("provider-enabled:codex"), Some(ProviderId::Codex));
        assert_eq!(parse_enabled_row("provider-enabled:claude-code"), Some(ProviderId::ClaudeCode));
        assert_eq!(parse_enabled_row("provider-enabled:muse"), Some(ProviderId::Muse));
        assert_eq!(parse_enabled_row("provider-enabled:nope"), None);
        assert_eq!(parse_enabled_row("auto_title"), None);
    }

    #[test]
    fn visible_ids_skip_disabled() {
        let mut statuses =
            vec![connected(), status(ProviderId::Muse, Installed::No, Auth::Unknown, true)];
        statuses.push(status(
            ProviderId::ClaudeCode,
            Installed::yes("2.1.276", "/bin/claude"),
            Auth::SignedOut,
            false,
        ));
        assert_eq!(
            visible_provider_ids(&statuses),
            vec![ProviderId::Muse, ProviderId::Codex]
        );
    }

    #[test]
    fn signout_confirms_name_the_real_command() {
        let (title, detail) = signout_confirm(ProviderId::ClaudeCode);
        assert!(title.contains("Claude Code"));
        assert!(detail.contains("claude auth logout"));
        assert!(detail.contains("not just in Baaz"));
        let (_, detail) = signout_confirm(ProviderId::Codex);
        assert!(detail.contains("account/logout"));
        let (_, detail) = signout_confirm(ProviderId::Muse);
        assert!(detail.contains("account/logout"));
    }

    struct Recording {
        shells: std::sync::Mutex<Vec<(String, Vec<String>)>>,
        codex: std::sync::Mutex<Vec<String>>,
        muse: std::sync::Mutex<u32>,
    }

    impl AuthCommander for Recording {
        fn run_shell(&self, program: &str, args: &[&str]) -> Result<String, String> {
            self.shells.lock().unwrap().push((
                program.to_owned(),
                args.iter().map(|arg| (*arg).to_owned()).collect(),
            ));
            Ok(String::new())
        }

        fn codex_logout(&self, program: &str) -> Result<(), String> {
            self.codex.lock().unwrap().push(program.to_owned());
            Ok(())
        }

        fn muse_logout(&self) {
            *self.muse.lock().unwrap() += 1;
        }
    }

    /// The sign-out shell carries the scrub and keeps the owner's home:
    /// `CLAUDECODE` is an explicit removal on the built command, while no
    /// Baaz `CLAUDE_CONFIG_DIR` is set — `claude auth logout` signs the
    /// owner's real account out, not a Baaz shadow. (Codex's logout rides
    /// `RunningChild::spawn`, scrubbed in `provider-codex`.)
    #[test]
    fn the_signout_shell_is_scrubbed_and_keeps_the_owner_home() {
        let _lock = crate::connect::SCRUB_ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = std::env::var_os("CLAUDECODE");
        std::env::set_var("CLAUDECODE", "1");
        let command = auth_command("claude", &["auth", "logout"]);
        if let Some(previous) = previous {
            std::env::set_var("CLAUDECODE", previous);
        } else {
            std::env::remove_var("CLAUDECODE");
        }
        let envs: Vec<_> = command.get_envs().collect();
        assert!(
            envs.iter().any(|(name, value)| *name == "CLAUDECODE" && value.is_none()),
            "sign-out removes the desktop inheritance: {envs:?}"
        );
        assert!(
            !envs.iter().any(|(name, _)| *name == "CLAUDE_CONFIG_DIR" || *name == "CODEX_HOME"),
            "one account per provider — the owner's home stands: {envs:?}"
        );
    }

    #[test]
    fn signout_runs_the_right_command() {
        let commander = Recording {
            shells: std::sync::Mutex::new(Vec::new()),
            codex: std::sync::Mutex::new(Vec::new()),
            muse: std::sync::Mutex::new(0),
        };
        run_sign_out(ProviderId::ClaudeCode, &commander).unwrap();
        assert_eq!(
            commander.shells.lock().unwrap().as_slice(),
            &[("claude".to_owned(), vec!["auth".to_owned(), "logout".to_owned()])]
        );
        assert!(commander.codex.lock().unwrap().is_empty());
        run_sign_out(ProviderId::Codex, &commander).unwrap();
        assert_eq!(commander.codex.lock().unwrap().len(), 1);
        run_sign_out(ProviderId::Muse, &commander).unwrap();
        assert_eq!(*commander.muse.lock().unwrap(), 1);
    }

    #[test]
    fn signin_plans_match_the_spec_fallbacks() {
        assert_eq!(signin_plan(ProviderId::Muse), SignInPlan::MuseSheet);
        assert_eq!(
            signin_plan(ProviderId::ClaudeCode),
            SignInPlan::ClaudeTerminal { command: "claude auth login".into() }
        );
        assert_eq!(signin_plan(ProviderId::Codex), SignInPlan::CodexBrowser);
    }

    #[test]
    fn disabled_banner_names_the_provider() {
        assert_eq!(
            disabled_banner(ProviderId::ClaudeCode),
            "Claude Code is disabled in Settings — Enable"
        );
    }

    #[test]
    fn the_own_mcp_switch_belongs_to_both_scoped_providers_only() {
        // Z5: Claude Code and Codex cards gain the row; Muse never does.
        assert_eq!(own_mcp_providers(), [ProviderId::ClaudeCode, ProviderId::Codex]);
        assert_eq!(use_own_mcp_label(), "Use my own MCP servers");
        // The detail names the off/on contract per provider plus the
        // sessions-started-after-the-change sentence.
        for (id, owned) in [
            (ProviderId::Codex, "~/.codex/config.toml servers and plugins load too"),
            (ProviderId::ClaudeCode, "your Claude Code MCP servers and connectors load too"),
        ] {
            let detail = use_own_mcp_detail(id);
            assert!(
                detail.contains("Off: sessions see only Baaz's tools."),
                "{id:?}: off names the default"
            );
            assert!(detail.contains(owned), "{id:?}: on names the owner's servers");
            assert!(
                detail.contains("Applies to sessions started after this change."),
                "{id:?}: the row says when it applies"
            );
        }
        // State reads the layout; Muse reads false with no switch.
        let mut layout = crate::layout::Layout::default();
        assert!(!use_own_mcp_state(&layout, ProviderId::Codex));
        assert!(!use_own_mcp_state(&layout, ProviderId::ClaudeCode));
        assert!(!use_own_mcp_state(&layout, ProviderId::Muse));
        layout.use_own_mcp.codex = true;
        layout.use_own_mcp.claude_code = true;
        assert!(use_own_mcp_state(&layout, ProviderId::Codex));
        assert!(use_own_mcp_state(&layout, ProviderId::ClaudeCode));
        assert!(!use_own_mcp_state(&layout, ProviderId::Muse));
    }
}
