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
use aui::screens::{ProviderAction, ProviderActionDef, ProviderCardData, ProviderIntent, StatusLine};
use aui_tokens::{scale, ActiveAui, AuiStyled};
use gpui::{prelude::*, px, AnyElement, Context, SharedString, Window};
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::switch::Switch;

use crate::app::Harness;
use crate::overlays::{Dialog, DialogAction};
use crate::provider_status::{Headline, ProviderStatus};
use crate::providers::ProviderId;

/// The Settings rail section id. `settings:providers` opens it.
pub(crate) const PROVIDERS_SECTION_ID: &str = "providers";

/// The card's top-row buttons (D5 B4): the text Re-check always, then the
/// state's one action — Update when too old, Sign out when Connected, Sign
/// in when signed out or unverified, Install (or "Get Muse ↗") when
/// missing. Checking, Disabled and Can't run have only the Re-check.
pub(crate) fn actions_for(status: &ProviderStatus) -> Vec<ProviderActionDef> {
    let mut actions = vec![ProviderActionDef::recheck_text()];
    let headline = status.headline();
    if headline != Headline::Checking
        && crate::connect::too_old(status).is_some()
        && status.provider.update_command().is_some()
    {
        actions.push(ProviderActionDef::update());
        return actions;
    }
    match headline {
        Headline::Connected => actions.push(ProviderActionDef::sign_out()),
        Headline::SignedOut | Headline::Unverified => actions.push(ProviderActionDef::sign_in()),
        Headline::NotInstalled if status.provider.install_command().is_none() => {
            actions.push(crate::connect::get_action(status.provider));
        }
        Headline::NotInstalled => actions.push(ProviderActionDef::install()),
        Headline::CantRun | Headline::Checking | Headline::Disabled => {}
    }
    actions
}

/// The full card data for one status: the status line ("Connected ·
/// email · plan", an API key said with a warning dot, too old, Not
/// installed…) and the state actions. While Codex signs in from here its
/// card follows the flow like its connect row: a spinner and Cancel. Pure,
/// so tests drive every state without a window.
pub(crate) fn card_data(status: &ProviderStatus, codex: &crate::connect::CodexLogin) -> ProviderCardData {
    use crate::connect::CodexLogin;
    let mut line = crate::connect::status_line(status, true);
    let mut actions = actions_for(status);
    if status.provider == ProviderId::Codex && status.headline() != Headline::Connected {
        match codex {
            CodexLogin::Waiting { .. } | CodexLogin::Device { .. } => {
                line = StatusLine::busy("Finish signing in with your browser…");
                actions = vec![ProviderActionDef::recheck_text(), ProviderActionDef::cancel()];
            }
            CodexLogin::Completed => line = StatusLine::busy("Signed in — checking…"),
            CodexLogin::Failed { reason } => line = StatusLine::warn(reason.clone()),
            CodexLogin::Idle | CodexLogin::Cancelled => {}
        }
    }
    let mut data = ProviderCardData::new(SharedString::from(status.provider.as_str()), status.provider.icon(), line)
        .actions(actions);
    if status.provider == ProviderId::Codex {
        data = data.assist(crate::connect::codex_assist_runs(codex));
    }
    data
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

/// The provider new sessions start on when the person has not picked one
/// (or picked one that is now off or missing): the first Connected, then the
/// first installed, then the first enabled, in switcher order. No provider
/// is the default for being Muse (docs/50 P1).
pub(crate) fn default_new_provider(statuses: &[ProviderStatus]) -> Option<ProviderId> {
    use crate::provider_status::{Headline, Installed};
    let enabled: Vec<&ProviderStatus> = ProviderId::all()
        .into_iter()
        .filter_map(|id| statuses.iter().find(|s| s.provider == id && s.enabled))
        .collect();
    enabled
        .iter()
        .find(|s| s.headline() == Headline::Connected)
        .or_else(|| enabled.iter().find(|s| matches!(s.installed, Installed::Yes { .. })))
        .or_else(|| enabled.first())
        .map(|s| s.provider)
}

/// Whether `id` can start a session now: on, and not found missing.
pub(crate) fn usable_provider(statuses: &[ProviderStatus], id: ProviderId) -> bool {
    statuses
        .iter()
        .any(|s| s.provider == id && s.enabled && s.installed != crate::provider_status::Installed::No)
}

/// [`default_new_provider`] over the live statuses; the switcher's first
/// provider when nothing is enabled at all.
pub(crate) fn live_default_new_provider() -> ProviderId {
    default_new_provider(&crate::provider_status::live_statuses())
        .unwrap_or_else(|| ProviderId::all()[0])
}

/// The quiet banner an opened session on a disabled provider shows
/// instead of starting a child.
pub(crate) fn disabled_banner(id: ProviderId) -> String {
    format!("{} is disabled in Settings — Enable", id.label())
}

/// Where the card's Docs button goes.
pub(crate) fn docs_url(id: ProviderId) -> &'static str {
    id.docs_url()
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
    resolved: Option<std::path::PathBuf>,
) -> Vec<SignOutStep> {
    // The binary the probe resolved, not the bare name: a Dock launch's
    // PATH holds neither `~/.local/bin` nor an nvm install (docs/65 §1).
    let program = |name: &str| {
        resolved
            .as_ref()
            .map(|program| program.to_string_lossy().into_owned())
            .unwrap_or_else(|| name.into())
    };
    match id {
        ProviderId::ClaudeCode => vec![SignOutStep::Shell {
            program: program("claude"),
            args: vec!["auth".into(), "logout".into()],
        }],
        ProviderId::Codex => vec![SignOutStep::CodexLogout { program: program("codex") }],
        ProviderId::Muse => vec![SignOutStep::MuseLogout],
    }
}

/// Run the sign-out steps through `commander`. The caller confirms first
/// and re-probes after.
pub(crate) fn run_sign_out(id: ProviderId, commander: &dyn AuthCommander) -> Result<(), String> {
    for step in signout_steps(id, crate::provider_status::binary_path(id)) {
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
    command.args(args).env("PATH", provider::env_path::child_path_for(std::path::Path::new(program)));
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
    /// The setup sheet with `claude auth login` typed, not run.
    ClaudeSheet,
    /// Start the ChatGPT login and open this provider's auth URL.
    CodexBrowser,
}

/// The sign-in start for `id`. Pure, so tests drive it.
pub(crate) fn signin_plan(id: ProviderId) -> SignInPlan {
    match id {
        ProviderId::Muse => SignInPlan::MuseSheet,
        ProviderId::ClaudeCode => SignInPlan::ClaudeSheet,
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
            let fallback = live_default_new_provider();
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
                    .color(p.accent_ring)
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
        let (n_claude, n_codex) = crate::session_migration::session_counts(plan);
        let title = crate::session_migration::prompt_title(n_claude, n_codex);
        let paths_label = if self.migration_paths_expanded { "Hide paths" } else { "Show paths" };
        let paths_toggle_label = format!("{paths_label} ({} files)", plan.len());
        let go = cx.listener(|this: &mut Self, _: &(), _, cx| this.confirm_session_migration(cx));
        let toggle = cx.listener(|this: &mut Self, _: &(), _, cx| this.toggle_migration_paths(cx));
        let mut text = v_flex().flex_1().min_w(px(0.0)).child(
            gpui::div().text_color(p.ink).ui(scale::FS_13).semibold().child(SharedString::from(title)),
        ).child(
            gpui::div().text_color(p.ink_3).ui(scale::FS_12).child(
                "Sessions you started in Baaz before this update still sit in your Claude and Codex \
                 folders, so Claude for Mac and Codex for Mac list them. Moving them keeps them \
                 resumable here; nothing is deleted. Codex for Mac may keep stale entries until you \
                 archive them there.",
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
            ProviderAction::Install => self.open_setup_sheet(id, crate::setup::SetupKind::Install, window, cx),
            ProviderAction::Update => self.open_setup_sheet(id, crate::setup::SetupKind::Update, window, cx),
            ProviderAction::Cancel => {
                if id == ProviderId::Codex {
                    self.codex_cancel.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            }
            ProviderAction::Docs => {
                cx.open_url(docs_url(id));
            }
            // B4: the Details disclosure, the path override and diagnostics.
            ProviderAction::ToggleDetails | ProviderAction::ChangePath | ProviderAction::CopyDiagnostics => {}
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

    /// Start a sign-in: the Muse sheet, the setup sheet with `claude auth
    /// login` typed, or the Codex browser flow.
    pub(crate) fn begin_provider_signin(
        &mut self,
        id: ProviderId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match signin_plan(id) {
            // Over the Settings page, like every other sign-in (D5 B3).
            SignInPlan::MuseSheet => {
                self.open_muse_sheet();
                cx.notify();
            }
            SignInPlan::ClaudeSheet => self.open_setup_sheet(id, crate::setup::SetupKind::SignIn, window, cx),
            // The one Codex sign-in: the connect screen's, which keeps its
            // app-server alive until `account/login/completed` and runs it
            // with the login-shell PATH (docs/65 §3.1 #10).
            SignInPlan::CodexBrowser => {
                if !matches!(
                    self.codex_login,
                    crate::connect::CodexLogin::Waiting { .. } | crate::connect::CodexLogin::Device { .. }
                ) {
                    self.start_codex_login(false, cx);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {

    fn p1_status(id: ProviderId, installed: crate::provider_status::Installed, auth: crate::provider_status::Auth) -> ProviderStatus {
        let mut status = ProviderStatus::checking(id);
        status.installed = installed;
        status.auth = auth;
        status
    }

    /// docs/50 P1: new sessions start on what can run, never on Muse for
    /// being Muse — the first Connected, then the first installed.
    #[test]
    fn the_default_provider_is_the_first_that_can_run() {
        use crate::provider_status::{Auth, Installed};
        let signed_in = || Auth::SignedIn { email: Some("a@b".into()), plan: None, method: None };
        let yes = || Installed::yes("1.0", "/bin/x");
        // Claude connected, Muse missing: Claude.
        let statuses = [
            p1_status(ProviderId::Muse, Installed::No, Auth::Unknown),
            p1_status(ProviderId::ClaudeCode, yes(), signed_in()),
            p1_status(ProviderId::Codex, Installed::No, Auth::Unknown),
        ];
        assert_eq!(default_new_provider(&statuses), Some(ProviderId::ClaudeCode));
        assert!(!usable_provider(&statuses, ProviderId::Muse), "a missing Muse is not usable");
        // Nothing connected, Codex installed: Codex, not Muse first in line.
        let statuses = [
            p1_status(ProviderId::Muse, Installed::Unknown, Auth::Unknown),
            p1_status(ProviderId::ClaudeCode, Installed::No, Auth::Unknown),
            p1_status(ProviderId::Codex, yes(), Auth::SignedOut),
        ];
        assert_eq!(default_new_provider(&statuses), Some(ProviderId::Codex));
        // Muse connected: Muse, on the same rule as any other.
        let statuses = [
            p1_status(ProviderId::Muse, yes(), signed_in()),
            p1_status(ProviderId::ClaudeCode, yes(), Auth::SignedOut),
        ];
        assert_eq!(default_new_provider(&statuses), Some(ProviderId::Muse));
        // A disabled provider is never the default.
        let mut off = p1_status(ProviderId::ClaudeCode, yes(), signed_in());
        off.enabled = false;
        assert_eq!(default_new_provider(&[off]), None);
    }
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

    fn labels(data: &aui::screens::ProviderCardData) -> Vec<String> {
        data.actions.iter().map(|def| def.label.to_string()).collect()
    }

    const IDLE: crate::connect::CodexLogin = crate::connect::CodexLogin::Idle;

    #[test]
    fn connected_card_names_email_plan_and_signs_out() {
        let data = card_data(&connected(), &IDLE);
        assert_eq!(data.status, StatusLine::ok("Connected · ada@example.com · pro"));
        assert_eq!(labels(&data), vec!["Re-check", "Sign out"]);
        assert_eq!(data.actions[0].style, aui::screens::ActionStyle::Text, "the card's Re-check is a text button");
    }

    #[test]
    fn signed_out_card_signs_in() {
        let data = card_data(
            &status(ProviderId::ClaudeCode, Installed::yes("2.1.276", "/bin/claude"), Auth::SignedOut, true),
            &IDLE,
        );
        assert_eq!(data.status, StatusLine::warn("Signed out"));
        assert_eq!(labels(&data), vec!["Re-check", "Sign in"]);
    }

    #[test]
    fn an_api_key_login_says_it_bills_per_token() {
        let data = card_data(
            &status(
                ProviderId::ClaudeCode,
                Installed::yes("2.1.276", "/bin/claude"),
                Auth::SignedIn { email: None, plan: None, method: Some("api_key".into()) },
                true,
            ),
            &IDLE,
        );
        assert_eq!(data.status, StatusLine::warn("Connected · API key · billed per token"));
    }

    #[test]
    fn a_too_old_card_offers_update() {
        let mut old = connected();
        old.installed = Installed::yes("codex-cli 0.120.0", "/bin/codex");
        old.advisory = Advisory::TooOld { need: "0.144.6".into() };
        let data = card_data(&old, &IDLE);
        assert_eq!(data.status, StatusLine::warn("0.120.0 is older than Baaz supports (0.144.6)"));
        assert_eq!(labels(&data), vec!["Re-check", "Update"]);
    }

    #[test]
    fn a_codex_card_follows_its_sign_in() {
        let signed_out = status(ProviderId::Codex, Installed::yes("0.144.6", "/bin/codex"), Auth::SignedOut, true);
        let waiting = crate::connect::CodexLogin::Waiting { auth_url: "https://auth".into() };
        let data = card_data(&signed_out, &waiting);
        assert_eq!(data.status.tone, aui::screens::StatusTone::Busy);
        assert_eq!(labels(&data), vec!["Re-check", "Cancel"]);
    }

    #[test]
    fn missing_card_installs() {
        let data = card_data(&status(ProviderId::Codex, Installed::No, Auth::Unknown, true), &IDLE);
        assert_eq!(data.status, StatusLine::off("Not installed"));
        assert_eq!(labels(&data), vec!["Re-check", "Install"]);
        // Muse has no command to type: its missing card is the one link.
        let muse = card_data(&status(ProviderId::Muse, Installed::No, Auth::Unknown, true), &IDLE);
        assert_eq!(labels(&muse), vec!["Re-check", "Get Muse"]);
        assert!(muse.actions[1].external);
    }

    #[test]
    fn cant_run_card_rechecks() {
        let mut cant = status(ProviderId::Codex, Installed::yes("", "/bin/codex"), Auth::Unknown, true);
        cant.advisory = Advisory::cant_run("boom", false);
        let data = card_data(&cant, &IDLE);
        assert_eq!(data.status, StatusLine::bad("Couldn’t run — boom"));
        assert_eq!(labels(&data), vec!["Re-check"]);
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
    fn auth_children_carry_the_login_path() {
        // From the Dock the inherited PATH lacks ~/.local/bin and nvm: the
        // sign-out and the Codex sign-in run with the login-shell PATH, the
        // program's own directory first (docs/65 §3.1 #10).
        for command in [
            auth_command("/opt/tools/claude", &["auth", "logout"]),
            crate::connect::codex_login_command("/opt/tools/codex"),
        ] {
            let path = command
                .get_envs()
                .find(|(name, _)| *name == "PATH")
                .and_then(|(_, value)| value)
                .map(|value| value.to_string_lossy().into_owned())
                .expect("PATH is set");
            assert!(path.starts_with("/opt/tools"), "program dir first: {path}");
        }
    }

    #[test]
    fn signout_runs_the_right_command() {
        let commander = Recording {
            shells: std::sync::Mutex::new(Vec::new()),
            codex: std::sync::Mutex::new(Vec::new()),
            muse: std::sync::Mutex::new(0),
        };
        run_sign_out(ProviderId::ClaudeCode, &commander).unwrap();
        {
            let shells = commander.shells.lock().unwrap();
            assert_eq!(shells.len(), 1);
            assert!(shells[0].0.ends_with("claude"), "the claude binary: {:?}", shells[0].0);
            assert_eq!(shells[0].1, vec!["auth".to_owned(), "logout".to_owned()]);
        }
        // The resolved binary, never the bare name a Dock PATH can't find;
        // the bare name only when nothing resolved.
        assert_eq!(
            signout_steps(ProviderId::ClaudeCode, Some("/Users/a/.local/bin/claude".into())),
            vec![SignOutStep::Shell {
                program: "/Users/a/.local/bin/claude".into(),
                args: vec!["auth".into(), "logout".into()],
            }]
        );
        assert_eq!(
            signout_steps(ProviderId::ClaudeCode, None),
            vec![SignOutStep::Shell { program: "claude".into(), args: vec!["auth".into(), "logout".into()] }]
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
            SignInPlan::ClaudeSheet
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
