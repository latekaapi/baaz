//! The provider registry's marks: which of the aui icon set's provider
//! badges each [`ProviderId`] wears — the badge its adapter declares
//! ([`provider::ProviderDescriptor::icon`]). The registry itself (ids,
//! labels, capabilities, lane factories) is [`baaz_core::providers`],
//! re-exported here so `crate::providers::…` names both.

pub use baaz_core::providers::*;

/// A [`ProviderId`]'s badge in the library's icon set.
pub trait ProviderMark {
    /// The provider mark a sidebar row wears after its tags.
    fn row_mark(self) -> Option<aui_icons::Provider>;
    /// The mark beside the composer model chip.
    fn icon(self) -> aui_icons::Provider;
}

impl ProviderMark for ProviderId {
    /// The provider mark a sidebar row wears after its tags; none for a
    /// provider whose rows wear none ([`provider::ProviderTrait::UnmarkedRows`]:
    /// the sidebar's baselines were drawn when it was the only provider).
    fn row_mark(self) -> Option<aui_icons::Provider> {
        (!self.has(provider::ProviderTrait::UnmarkedRows)).then(|| self.icon())
    }

    /// The mark beside the composer model chip: each provider's own badge,
    /// so a session never wears another provider's mark.
    fn icon(self) -> aui_icons::Provider {
        icon_of(self.descriptor().icon)
    }
}

/// The icon set's badge for a protocol provider identity: the two enums
/// name the same seven agents.
fn icon_of(provider: aui_protocol::Provider) -> aui_icons::Provider {
    match provider {
        aui_protocol::Provider::Claude => aui_icons::Provider::Claude,
        aui_protocol::Provider::Codex => aui_icons::Provider::Codex,
        aui_protocol::Provider::Grok => aui_icons::Provider::Grok,
        aui_protocol::Provider::Gemini => aui_icons::Provider::Gemini,
        aui_protocol::Provider::Pi => aui_icons::Provider::Pi,
        aui_protocol::Provider::Cursor => aui_icons::Provider::Cursor,
        aui_protocol::Provider::Muse => aui_icons::Provider::Muse,
    }
}

/// The registered provider wearing a library mark; `None` for a mark no
/// registered provider wears (Baaz only draws its own).
pub fn provider_from_icon(provider: aui_icons::Provider) -> Option<ProviderId> {
    ProviderId::all().into_iter().find(|id| id.icon() == provider)
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
            assert_eq!(id.has(provider::ProviderTrait::NativeSessionList), muse);
            assert_eq!(id.has(provider::ProviderTrait::BillingTier), muse);
            assert_eq!(id.has(provider::ProviderTrait::LiveContextMeter), muse);
            assert_eq!(id.has(provider::ProviderTrait::ReminderTail), muse);
            assert_eq!(id.has(provider::ProviderTrait::CatalogFlagsSessionModel), muse);
            assert_eq!(id.has(provider::ProviderTrait::ModelsLimitModes), !muse);
            assert_eq!(id.has(provider::ProviderTrait::TurnLifecycleEvents), muse);
            assert_eq!(id.row_mark().is_none(), muse);
        }
    }
}
