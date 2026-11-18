//! Settings › Providers › <provider>: the card's Details (D5 B4, docs/66 §3)
//! — version, path, where the binary was found, how it is signed in — the
//! per-provider path override behind "Change path…", and "Copy
//! diagnostics". The rows as the library draws them (`card_details`) and
//! the API-key notice are the app's `details` module.
//!
//! The override is stored in Baaz's own settings (`provider-paths.json` in
//! the support dir) and consulted by resolution after the env overrides
//! (`BAAZ_CLAUDE` / `BAAZ_CODEX` / `BAAZ_MUSE` / `MUSE_BIN` still win) and
//! before the login-shell `PATH`. Clearing it returns to the automatic
//! search.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::provider_status::{Auth, ProviderStatus};
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

/// Whether the login in force is an API key (Claude Code's
/// `authMethod: "api_key"`, Codex's `account.type == "apiKey"`): every turn
/// bills that key, which the status line says (owner 2026-10-23: say it,
/// don't block).
pub fn api_key_login(status: &ProviderStatus) -> bool {
    matches!(&status.auth, Auth::SignedIn { method: Some(method), .. }
        if matches!(method.to_ascii_lowercase().replace('_', "").as_str(), "apikey"))
}

/// The Signed in line: "claude.ai · ada@… · Max", "API key from
/// ANTHROPIC_API_KEY · billed per token", "ChatGPT · …", "Meta account · …".
pub fn signed_in_words(status: &ProviderStatus) -> String {
    let Auth::SignedIn { email, plan, source, .. } = &status.auth else {
        return match status.auth {
            Auth::SignedOut => "Not signed in".to_owned(),
            _ => "Not checked".to_owned(),
        };
    };
    if api_key_login(status) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_reads_as_a_tilde() {
        let home = std::env::var("HOME").unwrap_or_default();
        if !home.is_empty() {
            assert_eq!(tilde(&format!("{home}/.local/bin/claude")), "~/.local/bin/claude");
        }
        assert_eq!(tilde("/opt/homebrew/bin/codex"), "/opt/homebrew/bin/codex");
    }
}
