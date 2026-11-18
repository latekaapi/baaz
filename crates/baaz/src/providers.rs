//! The provider registry's marks: which of the aui icon set's provider
//! badges each [`ProviderId`] wears. The registry itself (ids, labels,
//! capabilities, lane factories) is [`baaz_core::providers`], re-exported
//! here so `crate::providers::…` names both.

pub use baaz_core::providers::*;

/// A [`ProviderId`]'s badge in the library's icon set.
pub trait ProviderMark {
    /// The provider mark a sidebar row wears after its tags.
    fn row_mark(self) -> Option<aui_icons::Provider>;
    /// The mark beside the composer model chip.
    fn icon(self) -> aui_icons::Provider;
}

impl ProviderMark for ProviderId {
    /// The provider mark a sidebar row wears after its tags. Muse's rows
    /// wear none: the sidebar's baselines were drawn when Muse was the only
    /// provider.
    fn row_mark(self) -> Option<aui_icons::Provider> {
        match self {
            ProviderId::Muse => None,
            ProviderId::ClaudeCode => Some(aui_icons::Provider::Claude),
            ProviderId::Codex => Some(aui_icons::Provider::Codex),
        }
    }

    /// The mark beside the composer model chip: each provider's own badge,
    /// so a Codex session never wears the Muse "M".
    fn icon(self) -> aui_icons::Provider {
        match self {
            ProviderId::Muse => aui_icons::Provider::Muse,
            ProviderId::ClaudeCode => aui_icons::Provider::Claude,
            ProviderId::Codex => aui_icons::Provider::Codex,
        }
    }
}

/// The provider behind a library mark; marks Baaz does not run map to
/// Muse (never reached: Baaz only draws its own three).
pub fn provider_from_icon(provider: aui_icons::Provider) -> ProviderId {
    match provider {
        aui_icons::Provider::Claude => ProviderId::ClaudeCode,
        aui_icons::Provider::Codex => ProviderId::Codex,
        _ => ProviderId::Muse,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_chrome_names_the_session_provider() {
        // The composer placeholder (also the empty session's hint), chip badge and
        // needs-you headline all derive from the session's provider: a
        // future hardcode of any one of them fails here, on every provider.
        for id in ProviderId::all() {
            let label = id.label();
            assert_eq!(
                id.composer_placeholder(),
                format!("Ask {label}, or type / for commands")
            );
            assert_eq!(id.waiting_headline(), format!("{label} is waiting for you."));
        }
        assert_eq!(ProviderId::Muse.icon(), aui_icons::Provider::Muse);
        assert_eq!(ProviderId::ClaudeCode.icon(), aui_icons::Provider::Claude);
        assert_eq!(ProviderId::Codex.icon(), aui_icons::Provider::Codex);
        // No two providers share a badge: the chip mark always identifies
        // the session's lane.
        let icons: Vec<_> = ProviderId::all().iter().map(|p| p.icon()).collect();
        assert_ne!(icons[0], icons[1]);
        assert_ne!(icons[0], icons[2]);
        assert_ne!(icons[1], icons[2]);
    }

    /// Slash commands follow the same truth: `/compact` and `/fork` appear
    /// only where they work.
    #[test]
    fn slash_commands_are_offered_per_provider() {
        use crate::overlays::Command;
        assert!(!Command::Compact.offered_on(ProviderId::ClaudeCode));
        assert!(Command::Fork.offered_on(ProviderId::ClaudeCode));
        assert!(!Command::Compact.offered_on(ProviderId::Codex));
        assert!(!Command::Fork.offered_on(ProviderId::Codex));
        assert!(Command::Compact.offered_on(ProviderId::Muse));
        assert!(Command::Model.offered_on(ProviderId::Codex), "commands that need nothing are always offered");
    }

    /// The registry's properties are where a provider's own features are
    /// named: Muse's billing tier, live context, reminder tail and shared
    /// catalog; the CLI lanes' models that can rule a mode out.
    #[test]
    fn the_registry_names_what_each_provider_offers() {
        for id in ProviderId::all() {
            let muse = id == ProviderId::Muse;
            assert_eq!(id.lists_sessions(), muse);
            assert_eq!(id.has_billing_tier(), muse);
            assert_eq!(id.meters_context_live(), muse);
            assert_eq!(id.has_reminder_tail(), muse);
            assert_eq!(id.catalog_flags_session_model(), muse);
            assert_eq!(id.models_limit_modes(), !muse);
            assert_eq!(id.reports_turn_lifecycle(), muse);
            assert_eq!(id.row_mark().is_none(), muse);
        }
    }
}
