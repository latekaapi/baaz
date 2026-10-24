//! Settings › Providers › <provider>: the card's Details (D5 B4, docs/66 §3)
//! — version, path, where the binary was found, how it is signed in — the
//! per-provider path override behind "Change path…", and "Copy
//! diagnostics".
//!
//! The override is stored in Baaz's own settings (`provider-paths.json` in
//! the support dir) and consulted by resolution after the env overrides
//! (`BAAZ_CLAUDE` / `BAAZ_CODEX` / `BAAZ_MUSE` / `MUSE_BIN` still win) and
//! before the login-shell `PATH`. Clearing it returns to the automatic
//! search.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use aui::screens::{AssistRun, CardDetails, DetailRow, ProviderActionDef};

use crate::provider_status::{Auth, Installed, ProviderStatus};
use crate::providers::ProviderId;

// ------------------------------------------------------------ the override

fn overrides_path() -> PathBuf {
    crate::store::support_dir().join("provider-paths.json")
}

fn read_overrides() -> BTreeMap<String, String> {
    std::fs::read_to_string(overrides_path())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// The binary the person chose for `id` in Settings, when they chose one.
pub fn path_override(id: ProviderId) -> Option<PathBuf> {
    read_overrides().get(id.as_str()).filter(|path| !path.trim().is_empty()).map(PathBuf::from)
}

/// Set (or, with `None`, clear) the override for `id`.
pub fn set_path_override(id: ProviderId, path: Option<&Path>) {
    let mut all = read_overrides();
    match path {
        Some(path) => {
            all.insert(id.as_str().to_owned(), path.to_string_lossy().into_owned());
        }
        None => {
            all.remove(id.as_str());
        }
    }
    if let Ok(text) = serde_json::to_string_pretty(&all) {
        let _ = crate::store::write_atomic(&overrides_path(), text.as_bytes());
    }
}

// ------------------------------------------------------------ where it came from

/// How the resolved binary was found, in the words the card uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Found {
    /// An env override (`BAAZ_CLAUDE`, `BAAZ_CODEX`, `BAAZ_MUSE`, `MUSE_BIN`).
    Env(&'static str),
    /// The person's Settings override.
    PathYouSet,
    /// A directory on the login shell's `PATH`.
    LoginPath,
    /// None of the above: a usual install place Baaz checks itself.
    KnownDir,
}

/// The env override that names `id`'s binary, when set and non-empty.
fn env_override_set(id: ProviderId) -> Option<&'static str> {
    let names: &[&'static str] = match id {
        ProviderId::ClaudeCode => &["BAAZ_CLAUDE"],
        ProviderId::Codex => &["BAAZ_CODEX"],
        ProviderId::Muse => &["BAAZ_MUSE", "MUSE_BIN"],
    };
    names.iter().copied().find(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
}

/// How `path` was found for `id`.
pub fn found(id: ProviderId, path: &Path) -> Found {
    if let Some(name) = env_override_set(id) {
        return Found::Env(name);
    }
    if path_override(id).is_some_and(|chosen| chosen == path) {
        return Found::PathYouSet;
    }
    let on_login_path = path
        .parent()
        .is_some_and(|dir| login_shell_dirs().iter().any(|candidate| candidate == dir));
    if on_login_path { Found::LoginPath } else { Found::KnownDir }
}

/// The directories the login shell itself put on `PATH` (without the
/// known install dirs Baaz appends).
fn login_shell_dirs() -> Vec<PathBuf> {
    std::env::split_paths(provider::env_path::login_path()).collect()
}

/// The login shell's name for the Found line (`zsh`).
fn shell_name() -> String {
    std::env::var("SHELL")
        .ok()
        .and_then(|shell| Path::new(&shell).file_name().map(|name| name.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "zsh".to_owned())
}

/// The Found line: "on your login shell's PATH (zsh)", "BAAZ_CLAUDE
/// override", "Path you set", "in a usual install place".
pub fn found_words(found: &Found) -> String {
    match found {
        Found::Env(name) => format!("{name} override"),
        Found::PathYouSet => "Path you set".to_owned(),
        Found::LoginPath => format!("on your login shell’s PATH ({})", shell_name()),
        Found::KnownDir => "in a usual install place".to_owned(),
    }
}

/// `path` with the home directory written `~`.
pub fn tilde(path: &str) -> String {
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && path.starts_with(&home) => format!("~{}", &path[home.len()..]),
        _ => path.to_owned(),
    }
}

// ------------------------------------------------------------ the rows

/// The Signed in line: "claude.ai · ada@… · Max", "API key from
/// ANTHROPIC_API_KEY · billed per token", "ChatGPT · …", "Meta account · …".
pub fn signed_in_words(status: &ProviderStatus) -> String {
    let Auth::SignedIn { email, plan, source, .. } = &status.auth else {
        return match status.auth {
            Auth::SignedOut => "Not signed in".to_owned(),
            _ => "Not checked".to_owned(),
        };
    };
    if crate::connect::api_key_login(status) {
        return match source.as_deref().filter(|s| !s.is_empty()) {
            Some(source) => format!("API key from {source} · billed per token"),
            None => "API key · billed per token".to_owned(),
        };
    }
    let via = match status.provider {
        ProviderId::ClaudeCode => "claude.ai",
        ProviderId::Codex => "ChatGPT",
        ProviderId::Muse => "Meta account",
    };
    let mut parts = vec![via.to_owned()];
    parts.extend(email.iter().chain(plan.iter()).map(|part| part.trim().to_owned()).filter(|part| !part.is_empty()));
    parts.join(" · ")
}

/// The advisory box under an API-key login (owner 2026-10-23: say it,
/// don't block). The Claude Code text is the board's; Codex says the same
/// for its own key.
pub fn api_key_notice(status: &ProviderStatus) -> Vec<AssistRun> {
    if !crate::connect::api_key_login(status) {
        return Vec::new();
    }
    match status.provider {
        ProviderId::ClaudeCode => {
            let source = match &status.auth {
                Auth::SignedIn { source: Some(source), .. } if !source.is_empty() => source.clone(),
                _ => "ANTHROPIC_API_KEY".to_owned(),
            };
            vec![
                AssistRun::Text("An ".into()),
                AssistRun::Code(source.into()),
                AssistRun::Text(
                    " in your shell signs Claude Code in: every turn is billed to that key, not a subscription. Remove it from your shell profile to use your Claude plan."
                        .into(),
                ),
            ]
        }
        ProviderId::Codex => vec![AssistRun::Text(
            "Codex is signed in with an API key: every turn is billed to that key, not a ChatGPT plan. Sign out and sign in with ChatGPT to use your plan."
                .into(),
        )],
        ProviderId::Muse => vec![AssistRun::Text(
            "Muse is signed in with an API key: every turn is billed to that key, not your Muse subscription.".into(),
        )],
    }
}

/// The card's Details for `status`: closed or `open`.
pub fn card_details(status: &ProviderStatus, open: bool) -> CardDetails {
    let (version, path) = match &status.installed {
        Installed::Yes { version, path } => (
            crate::provider_status::version_triplet(version)
                .map(|(a, b, c)| format!("{a}.{b}.{c}"))
                .unwrap_or_else(|| if version.trim().is_empty() { "Unknown".to_owned() } else { version.clone() }),
            Some(path.clone()),
        ),
        Installed::No => ("Not found".to_owned(), None),
        Installed::Unknown => ("Checking…".to_owned(), None),
    };
    let mut rows = vec![DetailRow::new("Version", version)];
    match &path {
        Some(path) => {
            rows.push(DetailRow::mono("Path", tilde(path)));
            rows.push(DetailRow::new("Found", found_words(&found(status.provider, Path::new(path)))));
        }
        None if status.installed == Installed::No && path_override(status.provider).is_some() => {
            let chosen = path_override(status.provider).unwrap_or_default();
            rows.push(DetailRow::mono("Path", tilde(&chosen.to_string_lossy())));
            rows.push(DetailRow::new("Found", "Path you set — no program there"));
        }
        None => {
            rows.push(DetailRow::new("Path", "—"));
            rows.push(DetailRow::new(
                "Found",
                match status.installed {
                    Installed::No => "not on your login shell’s PATH or in a usual install place".to_owned(),
                    _ => "—".to_owned(),
                },
            ));
        }
    }
    rows.push(DetailRow::new("Signed in", signed_in_words(status)));
    let mut actions = vec![ProviderActionDef::change_path()];
    if path_override(status.provider).is_some() {
        actions.push(ProviderActionDef::new(
            aui::screens::ProviderAction::ClearPath,
            "Use automatic path",
            aui::screens::ActionStyle::Secondary,
        ));
    }
    actions.push(ProviderActionDef::copy_diagnostics());
    CardDetails { open, rows, notice: api_key_notice(status), actions }
}

// ------------------------------------------------------------ diagnostics

/// One run's output for the diagnostics: exit, timing, stdout, stderr.
fn run_for_diagnostics(program: &Path, args: &[&str]) -> String {
    let began = std::time::Instant::now();
    let mut command = std::process::Command::new(program);
    command.args(args).env("PATH", provider::env_path::child_path_for(program));
    provider::child_env::scrub_command(&mut command);
    match command.output() {
        Ok(out) => format!(
            "$ {} {}  (exit {}, {} ms)\n{}{}",
            program.display(),
            args.join(" "),
            out.status.code().map(|c| c.to_string()).unwrap_or_else(|| "signal".into()),
            began.elapsed().as_millis(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        ),
        Err(error) => format!("$ {} {}  (could not run: {error})\n", program.display(), args.join(" ")),
    }
}

/// The plain-text diagnostics behind "Copy diagnostics": provider, the PATH
/// it searched and where that came from, every match on it (`which -a`),
/// the resolved path and how, `--version`, the raw auth probe, timings.
/// Blocking (it runs the CLI): call it off the UI thread. No secrets: the
/// auth probes print status, never tokens.
pub fn collect(id: ProviderId, status: &ProviderStatus) -> String {
    let began = std::time::Instant::now();
    let mut out = String::new();
    out.push_str(&format!("Baaz {} · {} diagnostics\n", env!("CARGO_PKG_VERSION"), id.label()));
    out.push_str(&format!("Status: {}\n", status.headline_text()));
    let login = provider::env_path::login_path().to_string_lossy().into_owned();
    out.push_str(&format!(
        "PATH source: login shell ({}) via `$SHELL -ilc`, path_helper as fallback, plus the usual install dirs\n",
        shell_name()
    ));
    out.push_str(&format!("Login PATH: {login}\n"));
    out.push_str(&format!("Inherited PATH: {}\n", std::env::var("PATH").unwrap_or_default()));
    let binary = match id {
        ProviderId::ClaudeCode => "claude",
        ProviderId::Codex => "codex",
        ProviderId::Muse => "muse",
    };
    let matches: Vec<String> = std::env::split_paths(&login)
        .map(|dir| dir.join(binary))
        .filter(|candidate| candidate.is_file())
        .map(|candidate| candidate.display().to_string())
        .collect();
    out.push_str(&format!("which -a {binary}: {}\n", if matches.is_empty() { "(none)".to_owned() } else { matches.join(", ") }));
    if let Some(name) = env_override_set(id) {
        out.push_str(&format!("Env override: {name}={}\n", std::env::var(name).unwrap_or_default()));
    }
    if let Some(chosen) = path_override(id) {
        out.push_str(&format!("Path you set: {}\n", chosen.display()));
    }
    let resolved = crate::provider_status::resolve_for_diagnostics(id);
    match &resolved {
        Some(path) => {
            out.push_str(&format!("Resolved: {} ({})\n", path.display(), found_words(&found(id, path))));
            out.push('\n');
            out.push_str(&run_for_diagnostics(path, &["--version"]));
            out.push('\n');
            match id {
                ProviderId::ClaudeCode => out.push_str(&run_for_diagnostics(path, &["auth", "status", "--json"])),
                ProviderId::Codex => out.push_str(&run_for_diagnostics(path, &["login", "status"])),
                ProviderId::Muse => out.push_str(&format!("Auth: from the live connection — {}\n", signed_in_words(status))),
            }
        }
        None => out.push_str("Resolved: (not found)\n"),
    }
    out.push_str(&format!("\nCollected in {} ms\n", began.elapsed().as_millis()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_status::Advisory;

    fn status(id: ProviderId, auth: Auth) -> ProviderStatus {
        ProviderStatus {
            provider: id,
            installed: Installed::yes("2.1.276 (Claude Code)", "/opt/homebrew/bin/claude"),
            auth,
            enabled: true,
            advisory: Advisory::None,
            checked_at: Some(1),
            usage: None,
        }
    }

    #[test]
    fn the_signed_in_line_names_the_account_or_the_key() {
        let oauth = status(
            ProviderId::ClaudeCode,
            Auth::SignedIn { email: Some("ada@example.com".into()), plan: Some("Max".into()), method: Some("claude.ai".into()), source: None },
        );
        assert_eq!(signed_in_words(&oauth), "claude.ai · ada@example.com · Max");
        assert!(api_key_notice(&oauth).is_empty());
        let key = status(
            ProviderId::ClaudeCode,
            Auth::SignedIn { email: None, plan: None, method: Some("api_key".into()), source: Some("ANTHROPIC_API_KEY".into()) },
        );
        assert_eq!(signed_in_words(&key), "API key from ANTHROPIC_API_KEY · billed per token");
        let notice = api_key_notice(&key);
        assert!(notice.contains(&AssistRun::Code("ANTHROPIC_API_KEY".into())));
        let codex = status(ProviderId::Codex, Auth::SignedIn { email: None, plan: None, method: Some("apiKey".into()), source: None });
        assert_eq!(signed_in_words(&codex), "API key · billed per token");
        assert!(!api_key_notice(&codex).is_empty());
    }

    #[test]
    fn details_rows_are_version_path_found_signed_in() {
        let card = card_details(&status(ProviderId::ClaudeCode, Auth::SignedOut), true);
        let keys: Vec<&str> = card.rows.iter().map(|row| row.key.as_ref()).collect();
        assert_eq!(keys, vec!["Version", "Path", "Found", "Signed in"]);
        assert_eq!(card.rows[0].value.as_ref(), "2.1.276");
        assert!(card.rows[1].mono, "the path is mono");
        assert_eq!(card.rows[3].value.as_ref(), "Not signed in");
        let labels: Vec<&str> = card.actions.iter().map(|a| a.label.as_ref()).collect();
        assert!(labels.contains(&"Change path…") && labels.contains(&"Copy diagnostics"));
    }

    #[test]
    fn home_reads_as_a_tilde() {
        let home = std::env::var("HOME").unwrap_or_default();
        if !home.is_empty() {
            assert_eq!(tilde(&format!("{home}/.local/bin/claude")), "~/.local/bin/claude");
        }
        assert_eq!(tilde("/opt/homebrew/bin/codex"), "/opt/homebrew/bin/codex");
    }

    #[test]
    fn the_override_round_trips_and_clears() {
        let _sandbox = crate::provider_status::TestSandbox::hold();
        assert_eq!(path_override(ProviderId::Codex), None);
        set_path_override(ProviderId::Codex, Some(Path::new("/tmp/my/codex")));
        assert_eq!(path_override(ProviderId::Codex), Some(PathBuf::from("/tmp/my/codex")));
        assert_eq!(path_override(ProviderId::ClaudeCode), None, "per provider");
        let mut missing = ProviderStatus::checking(ProviderId::Codex);
        missing.installed = Installed::No;
        missing.checked_at = Some(1);
        let card = card_details(&missing, true);
        assert_eq!(card.rows[1].value.as_ref(), "/tmp/my/codex");
        assert_eq!(card.rows[2].value.as_ref(), "Path you set — no program there");
        assert!(card.actions.iter().any(|a| a.label.as_ref() == "Use automatic path"));
        set_path_override(ProviderId::Codex, None);
        assert_eq!(path_override(ProviderId::Codex), None);
    }
}
