//! The browser assistant (docs/27 §6, R3): which provider and model a
//! browser chat runs on, where its sessions live, and how a session's turns
//! become the agent card's transcript.
//!
//! A browser chat is an ordinary Baaz session started in one hidden
//! workspace ([`BROWSER_CHATS_DIR`] under the state dir), never a project:
//! its record is hidden from the Sessions sidebar (docs/27 §9.1 Q2), and it
//! is reached from the line, the card and its task folder.


use crate::providers::ProviderId;

pub use baaz_core::browser_prefs::{AssistantPrefs, BROWSER_CHATS_DIR};
use crate::layout::LayoutGlobal as _;

/// The workspace a browser chat starts in, created on the way in.
pub fn browser_workspace_dir() -> std::path::PathBuf {
    let dir = crate::store::support_dir().join(BROWSER_CHATS_DIR);
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// One model the Assistant menu offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelChoice {
    /// The provider it runs on.
    pub provider: ProviderId,
    /// The id the provider takes; empty is the provider's default.
    pub model: &'static str,
    /// What the menu and the card show.
    pub label: &'static str,
    /// The provider's cheapest model (the default's pick).
    pub cheapest: bool,
}

/// The models each provider offers the assistant, cheapest first. Claude
/// Code takes its CLI aliases, which always name the current model of that
/// tier; Codex reports no prices, so it offers its own default (the
/// auto-titles rule runs it at low effort); Muse names its utility model.
pub fn model_choices(provider: ProviderId) -> Vec<ModelChoice> {
    // Each adapter declares its own, cheapest first.
    provider
        .descriptor()
        .utility_models
        .iter()
        .enumerate()
        .map(|(at, &(model, label))| ModelChoice { provider, model, label, cheapest: at == 0 })
        .collect()
}

/// The provider and model a new browser chat starts with now, from the
/// layout's stored pick and the live statuses: what the browser chat starts
/// on and what Settings › Browser › Assistant shows. `None` when no provider
/// can start a session.
pub(crate) fn current_choice(cx: &gpui::App) -> Option<ModelChoice> {
    let statuses = crate::provider_status::live_statuses();
    let default = crate::settings_providers::default_new_provider(&statuses);
    resolve(&crate::layout::Layout::get(cx).assistant, |p| crate::settings_providers::usable_provider(&statuses, p), default)
}

/// The provider and model a new browser chat starts with: the stored pick
/// while its provider is usable, else `default_provider`'s cheapest model.
/// `None` when no provider can start a session.
pub fn resolve(prefs: &AssistantPrefs, usable: impl Fn(ProviderId) -> bool, default_provider: Option<ProviderId>) -> Option<ModelChoice> {
    if let Some(provider) = prefs.provider.as_deref().and_then(ProviderId::parse_known).filter(|p| usable(*p)) {
        let model = prefs.model.clone().unwrap_or_default();
        let choices = model_choices(provider);
        let found = choices.iter().find(|choice| choice.model == model).cloned();
        return Some(found.unwrap_or_else(|| choices[0].clone()));
    }
    let provider = default_provider.filter(|p| usable(*p))?;
    model_choices(provider).into_iter().next()
}

#[cfg(feature = "probe")]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_the_default_providers_cheapest_model() {
        let picked = resolve(&AssistantPrefs::default(), |_| true, Some(ProviderId::ClaudeCode)).unwrap();
        assert_eq!((picked.provider, picked.model), (ProviderId::ClaudeCode, "haiku"));
        assert!(picked.cheapest);
    }

    #[test]
    fn a_stored_pick_wins_while_its_provider_is_usable() {
        let prefs = AssistantPrefs { provider: Some("claude-code".into()), model: Some("sonnet".into()) };
        let picked = resolve(&prefs, |_| true, Some(ProviderId::Codex)).unwrap();
        assert_eq!(picked.label, "Claude Sonnet");
        let fallback = resolve(&prefs, |p| p != ProviderId::ClaudeCode, Some(ProviderId::Codex)).unwrap();
        assert_eq!(fallback.provider, ProviderId::Codex);
    }

    #[test]
    fn nothing_usable_resolves_to_none() {
        assert!(resolve(&AssistantPrefs::default(), |_| false, Some(ProviderId::Muse)).is_none());
    }

    #[test]
    fn an_unknown_stored_model_falls_back_to_the_cheapest() {
        let prefs = AssistantPrefs { provider: Some("claude-code".into()), model: Some("retired".into()) };
        assert_eq!(resolve(&prefs, |_| true, None).unwrap().model, "haiku");
    }
}
