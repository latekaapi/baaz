//! Settings › Providers › <provider>: the card's Details as the library
//! draws them (D5 B4, docs/66 §3) — the rows and the API-key notice. The
//! override, the "Found" words and "Copy diagnostics" are
//! [`baaz_core::details`], re-exported here so `crate::details::…` names
//! both.

use aui::screens::{AssistRun, CardDetails, DetailRow, ProviderActionDef};

pub use baaz_core::details::*;

use std::path::Path;

use crate::provider_status::{Auth, Installed, ProviderStatus};
use crate::providers::ProviderId;

/// The advisory box under an API-key login (owner 2026-10-23: say it,
/// don't block). The Claude Code text is the board's; Codex says the same
/// for its own key. On the provider's own card, so it names the provider —
/// by its descriptor's label and account, never a literal (rule 14).
pub fn api_key_notice(status: &ProviderStatus) -> Vec<AssistRun> {
    if !crate::details::api_key_login(status) {
        return Vec::new();
    }
    let label = status.provider.label();
    let account = status.provider.descriptor().account_label;
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
                    format!(
                        " in your shell signs {label} in: every turn is billed to that key, not a subscription. Remove it from your shell profile to use your plan."
                    )
                    .into(),
                ),
            ]
        }
        ProviderId::Codex => vec![AssistRun::Text(
            format!(
                "{label} is signed in with an API key: every turn is billed to that key, not a {account} plan. Sign out and sign in with {account} to use your plan."
            )
            .into(),
        )],
        _ => vec![AssistRun::Text(format!(
            "{label} is signed in with an API key: every turn is billed to that key, not your {label} subscription."
        ).into())],
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

#[cfg(feature = "probe")]

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
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
