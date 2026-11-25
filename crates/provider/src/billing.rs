//! How a login pays for a turn (docs/85 11.6): one vocabulary every adapter
//! reports in, so the app keeps one billing rule instead of one provider's
//! guard.

/// How the signed-in account pays for turns.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum BillingMode {
    /// A plan: turns count against its limits, not a bill.
    Subscription,
    /// Every turn bills API usage (an API key, pay-as-you-go).
    PerToken,
    /// The provider has not said (signed out, a failed probe, a plan it
    /// would not name).
    #[default]
    Unknown,
}

impl BillingMode {
    /// From what a login says of itself: a key bills per token; a login
    /// with a named method or plan is a subscription; otherwise unknown.
    /// Claude Code's `auth status` (`authMethod`, `apiKeySource`) and
    /// Codex's `account/read` (`chatgpt` / `apiKey`, `planType`) both read
    /// through this.
    pub fn from_login(method: Option<&str>, key_source: Option<&str>, plan: Option<&str>) -> BillingMode {
        let method = method.map(|m| m.to_ascii_lowercase().replace(['-', '_'], ""));
        if key_source.is_some() || method.as_deref() == Some("apikey") {
            BillingMode::PerToken
        } else if method.is_some() || plan.is_some() {
            BillingMode::Subscription
        } else {
            BillingMode::Unknown
        }
    }

    /// The stored name (`subscription`, `perToken`, `unknown`).
    pub fn as_str(self) -> &'static str {
        match self {
            BillingMode::Subscription => "subscription",
            BillingMode::PerToken => "perToken",
            BillingMode::Unknown => "unknown",
        }
    }

    /// The mode a stored name names; anything else is unknown.
    pub fn parse(raw: &str) -> BillingMode {
        match raw {
            "subscription" => BillingMode::Subscription,
            "perToken" => BillingMode::PerToken,
            _ => BillingMode::Unknown,
        }
    }

    /// Whether the provider said anything.
    pub fn is_known(self) -> bool {
        self != BillingMode::Unknown
    }

    /// The words for it, lower-case, as a sentence uses them.
    pub fn words(self) -> &'static str {
        match self {
            BillingMode::Subscription => "a subscription",
            BillingMode::PerToken => "per-token billing",
            BillingMode::Unknown => "unknown billing",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_login_says_how_it_pays() {
        // Claude Code: an API key from the environment, or the claude.ai login.
        assert_eq!(BillingMode::from_login(None, Some("ANTHROPIC_API_KEY"), None), BillingMode::PerToken);
        assert_eq!(BillingMode::from_login(Some("api_key"), None, None), BillingMode::PerToken);
        assert_eq!(BillingMode::from_login(Some("claude.ai"), None, Some("Claude Max")), BillingMode::Subscription);
        // Codex: `apiKey` or `chatgpt`.
        assert_eq!(BillingMode::from_login(Some("apiKey"), None, None), BillingMode::PerToken);
        assert_eq!(BillingMode::from_login(Some("chatgpt"), None, Some("pro")), BillingMode::Subscription);
        // Nothing said.
        assert_eq!(BillingMode::from_login(None, None, None), BillingMode::Unknown);
    }
}
