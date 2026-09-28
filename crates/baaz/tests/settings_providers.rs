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

/// The Settings rail has a Providers section whose page renders the
/// cards: the library dialog only takes `SettingsRow`s, so the rail row
/// opens a dedicated sheet (see `settings_providers`).
#[test]
fn providers_section_renders_cards_not_dialog_rows() {
    let settings = read_repo("crates/baaz/src/settings.rs");
    pin(&settings, "providers_section()", "settings sections");
    pin(&settings, "render_providers_page", "providers page branch");
    pin(&settings, "PROVIDERS_SECTION_ID", "providers section id");
    pin(&settings, "parse_enabled_row", "enabled switch routing");
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

/// Sign-in reuses the spec fallbacks (Y5 is not on this branch, so no
/// "Set up providers" row): the Muse sheet, the Claude terminal
/// prefill, the Codex browser flow.
#[test]
fn signin_starts_each_providers_own_flow() {
    let module = read_repo("crates/baaz/src/settings_providers.rs");
    pin(&module, "MuseSheet", "muse sign-in sheet");
    pin(&module, "claude auth login", "claude terminal prefill");
    pin(&module, "CodexBrowser", "codex browser flow");
}
