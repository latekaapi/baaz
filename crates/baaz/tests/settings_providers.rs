//! Y6: Settings → Providers — see, re-check, enable/disable, sign in
//! and sign out of each provider (`docs/23-providers-connect.md` §4).
//!
//! Baaz is a binary, so this suite cannot call its functions: the
//! behaviour lives in unit tests beside the model
//! (`settings_providers.rs`: cards per state, the enabled filter, the
//! sign-out confirm text, the scripted-runner dispatch, the sign-in
//! plans; `provider_status.rs`: the switch persisting through the
//! cache, re-check probing once and never re-enabling;
//! `session/commands.rs`: the disabled provider leaving the composer
//! menu). What remains here is the file contract: the probe entry with
//! scripted statuses over several states, and the wiring pins that tie
//! the page, the switch, the confirm and the menu together. Remove any
//! arm and its pin names the regression.

use std::path::{Path, PathBuf};

fn manifest() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn read_repo(relative: &str) -> String {
    let path = manifest().join("..").join("..").join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("reads: {}", path.display()))
}

fn pin(haystack: &str, needle: &str, what: &str) {
    assert!(haystack.contains(needle), "{what}: `{needle}` went missing");
}

/// The `settings-providers` probe entry: offline, scripted statuses
/// covering Connected (Muse, signed in), Signed out (Claude Code) and
/// Not installed (Codex). No baseline is generated here.
#[test]
fn probe_entry_covers_several_states_offline() {
    let probe = read_repo("scripts/uiprobe.py");
    for needle in [
        "\"settings-providers\"",
        "settings:providers",
        "BAAZ_PROVIDER_STATUS_SCRIPT",
        "\"provider\":\"muse\"",
        "\"SignedIn\"",
        "\"provider\":\"claude-code\"",
        "\"auth\":\"SignedOut\"",
        "\"provider\":\"codex\"",
        "\"installed\":\"No\"",
    ] {
        pin(&probe, needle, "settings-providers entry");
    }
}

/// The Settings page has a Providers section whose overview and sub-pages
/// render the cards: the overview holds one row per provider into its
/// sub-page, and each sub-page renders that provider's card.
#[test]
fn providers_section_renders_cards_not_dialog_rows() {
    let settings = read_repo("crates/baaz/src/settings.rs");
    pin(&settings, "settings_providers_overview", "providers overview page branch");
    pin(&settings, "settings_provider_subpage", "provider sub-page with cards");
    pin(&settings, "settings-card-", "cards render on the sub-page");
    pin(&settings, "parse_enabled_row", "enabled switch routing");
    let module = read_repo("crates/baaz/src/settings_providers.rs");
    pin(&module, "pub(crate) fn providers_section", "providers section");
    pin(&module, "PROVIDERS_SECTION_ID", "providers section id");
    pin(&module, "parse_enabled_row", "enabled switch routing");
}

/// The enabled switch persists, hides the menu row, stops probes, and
/// opens disabled sessions read-only with the quiet banner.
#[test]
fn enabled_switch_gates_menu_probes_and_child_start() {
    let module = read_repo("crates/baaz/src/settings_providers.rs");
    pin(&module, "visible_provider_ids", "menu filter");
    pin(&module, "is disabled in Settings", "quiet banner");
    let status = read_repo("crates/baaz/src/provider_status.rs");
    pin(&status, "fn set_enabled", "persisted switch");
    pin(&status, "filter(|id| self.status(*id).enabled)", "probes skip disabled");
    let composer = read_repo("crates/baaz/src/session/composer.rs");
    pin(&composer, "live_visible_provider_ids", "menu hides disabled");
    let lifecycle = read_repo("crates/baaz/src/app/lifecycle.rs");
    pin(&lifecycle, "pending_disabled_notice", "no child on disabled open");
}

/// Sign-out confirms with the real command, then runs it: `claude auth
/// logout`, Codex `account/logout`, Muse's logout path — and re-probes.
#[test]
fn signout_confirms_then_runs_the_right_command() {
    let module = read_repo("crates/baaz/src/settings_providers.rs");
    pin(&module, "claude auth logout", "claude sign-out");
    pin(&module, "account/login/start", "codex sign-in start");
    pin(&module, "account/logout", "codex sign-out");
    pin(&module, "not just in Baaz", "confirm names what happens");
    let overlays = read_repo("crates/baaz/src/overlays.rs");
    pin(&overlays, "ProviderSignOut", "sign-out confirm action");
    let dialogs = read_repo("crates/baaz/src/dialogs.rs");
    pin(&dialogs, "confirm_provider_signout", "confirm runs sign-out");
}

/// Sign-in reuses the spec fallbacks: the Muse sheet, the Claude terminal
/// prefill, the Codex browser flow.
#[test]
fn signin_starts_each_providers_own_flow() {
    let module = read_repo("crates/baaz/src/settings_providers.rs");
    pin(&module, "MuseSheet", "muse sign-in sheet");
    pin(&module, "claude auth login", "claude terminal prefill");
    pin(&module, "CodexBrowser", "codex browser flow");
}

/// The Providers overview is a page, not a modal: the centre header holds
/// the breadcrumb plus the close button (Esc still closes), and a
/// "Set up providers…" row opens the connect screen.
#[test]
fn providers_page_has_close_and_setup_row() {
    let settings = read_repo("crates/baaz/src/settings.rs");
    pin(&settings, "settings_breadcrumb", "breadcrumb in the header");
    pin(&settings, "settings-close", "close button id");
    pin(&settings, "Close settings", "close button label");
    pin(&settings, "settings-providers-setup", "set-up row");
    pin(&settings, "Set up providers", "set-up row label");
    pin(&settings, "open_connect_screen", "set-up row opens connect");
    let connect = read_repo("crates/baaz/src/connect.rs");
    pin(&connect, "fn open_connect_screen", "connect open entry point");
    pin(&connect, "show_connect = true", "connect screen opens");
}

/// Every Settings page has a probe entry: General, Providers, one provider
/// sub-page (Codex), Shortcuts and Archived.
#[test]
fn probe_entries_cover_every_settings_page() {
    let probe = read_repo("scripts/uiprobe.py");
    for needle in [
        "\"settings-general\"",
        "\"settings-providers\"",
        "\"settings-provider-codex\"",
        "\"settings-shortcuts\"",
        "\"settings-archived\"",
        "settings:general",
        "settings:providers/codex",
        "settings:shortcuts",
        "settings:archived",
    ] {
        pin(&probe, needle, "settings probe entry");
    }
}

/// Z5: each Claude Code / Codex card carries a "Use my own MCP
/// servers" switch row (Role=Switch + label) that persists through the
/// layout and reaches the session argv; the row says it applies to
/// sessions started after the change.
#[test]
fn own_mcp_switch_rides_under_both_cards_and_reaches_argv() {
    let module = read_repo("crates/baaz/src/settings_providers.rs");
    pin(&module, "Use my own MCP servers", "switch label");
    pin(&module, "use_own_mcp_row", "row under each card");
    pin(&module, "flip_use_own_mcp", "switch flip");
    pin(&module, "Applies to sessions started after this change", "row says when it applies");
    pin(&module, "~/.codex/config.toml servers and plugins load too", "codex detail");
    pin(&module, "Claude Code MCP servers and connectors load too", "claude detail");
    let layout = read_repo("crates/baaz/src/layout.rs");
    pin(&layout, "use_own_mcp", "layout switch state");
    pin(&layout, "struct UseOwnMcp", "per-provider switch");
    let providers = read_repo("crates/baaz/src/providers.rs");
    pin(&providers, "set_use_own_mcp", "argv wiring reads the switch");
    let codex = read_repo("crates/provider-codex/src/lib.rs");
    pin(&codex, "set_use_own_mcp", "codex opt-in");
    let claude = read_repo("crates/provider-claude-code/src/lib.rs");
    pin(&claude, "set_use_own_mcp", "claude opt-in");
}

/// File cards show workspace-relative paths: the display helper with
/// its inside/outside/home/root/shared-prefix cases, wired through the
/// transcript's card targets with the full path kept for reveals.
#[test]
fn file_cards_show_workspace_relative_paths() {
    let transcript = read_repo("crates/baaz/src/transcript.rs");
    pin(&transcript, "fn display_path", "display helper");
    pin(&transcript, "workspace_root", "workspace context");
    pin(&transcript, "display_target(call, &folds.workspace_root", "lone cards shorten");
    pin(&transcript, "display_group_block", "grouped cards shorten");
    let render = read_repo("crates/baaz/src/session/render.rs");
    pin(&render, "workspace_root: self.workspace.clone()", "view passes its root");
}
