//! What `claude auth status --json` says, decoded once, here.
//!
//! The verified output shape carries `loggedIn, authMethod, apiProvider,
//! email, orgId, orgName, subscriptionType`. `subscriptionType` "max"/"pro"
//! map to the human plan labels "Claude Max"/"Claude Pro".

/// One decoded `claude auth status --json` answer. `None` fields are
/// absent-or-null on the wire, never guessed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClaudeAuthStatus {
    /// Whether the CLI holds a login.
    pub logged_in: bool,
    /// e.g. `oauth`, `api-key`.
    pub auth_method: Option<String>,
    /// e.g. `anthropic`.
    pub api_provider: Option<String>,
    /// The signed-in email, when the CLI reports one.
    pub email: Option<String>,
    /// The org id, when reported.
    pub org_id: Option<String>,
    /// The org name, when reported.
    pub org_name: Option<String>,
    /// The raw `subscriptionType`, when reported.
    pub subscription_type: Option<String>,
}

impl ClaudeAuthStatus {
    /// The human plan label: `subscriptionType` "max"/"pro" read
    /// "Claude Max"/"Claude Pro" (case-insensitive); any other non-empty
    /// value passes through verbatim; absent or empty is `None`.
    /// `None` while signed out too — there is no plan without a login.
    pub fn plan_label(&self) -> Option<String> {
        if !self.logged_in {
            return None;
        }
        subscription_label(self.subscription_type.as_deref())
    }
}

/// The human label for one raw `subscriptionType`: "max" reads
/// "Claude Max", "pro" reads "Claude Pro", anything else non-empty passes
/// through verbatim, and absent or blank is `None`.
pub fn subscription_label(subscription_type: Option<&str>) -> Option<String> {
    let raw = subscription_type.map(str::trim).filter(|raw| !raw.is_empty())?;
    if raw.eq_ignore_ascii_case("max") {
        Some("Claude Max".into())
    } else if raw.eq_ignore_ascii_case("pro") {
        Some("Claude Pro".into())
    } else {
        Some(raw.to_owned())
    }
}

fn text_field(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// Decode one `claude auth status --json` document. `None` when the text
/// is not a JSON object at all — never a guessed status.
pub fn parse_auth_status(text: &str) -> Option<ClaudeAuthStatus> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    if !value.is_object() {
        return None;
    }
    Some(ClaudeAuthStatus {
        logged_in: value.get("loggedIn").and_then(serde_json::Value::as_bool).unwrap_or(false),
        auth_method: text_field(&value, "authMethod"),
        api_provider: text_field(&value, "apiProvider"),
        email: text_field(&value, "email"),
        org_id: text_field(&value, "orgId"),
        org_name: text_field(&value, "orgName"),
        subscription_type: text_field(&value, "subscriptionType"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signed_in_max_account_decodes() {
        let text = r#"{"loggedIn":true,"authMethod":"oauth","apiProvider":"anthropic","email":"a@x.com","orgId":"o1","orgName":"Acme","subscriptionType":"max"}"#;
        let status = parse_auth_status(text).expect("decodes");
        assert!(status.logged_in);
        assert_eq!(status.email.as_deref(), Some("a@x.com"));
        assert_eq!(status.plan_label().as_deref(), Some("Claude Max"));
    }

    #[test]
    fn pro_maps_to_claude_pro() {
        let text = r#"{"loggedIn":true,"subscriptionType":"pro"}"#;
        let status = parse_auth_status(text).expect("decodes");
        assert_eq!(status.plan_label().as_deref(), Some("Claude Pro"));
    }

    #[test]
    fn unknown_subscription_types_pass_through_verbatim() {
        let text = r#"{"loggedIn":true,"subscriptionType":"team"}"#;
        let status = parse_auth_status(text).expect("decodes");
        assert_eq!(status.plan_label().as_deref(), Some("team"));
    }

    #[test]
    fn signed_out_decodes_as_not_logged_in() {
        let text = r#"{"loggedIn":false}"#;
        let status = parse_auth_status(text).expect("decodes");
        assert!(!status.logged_in);
        assert_eq!(status.plan_label(), None);
    }

    #[test]
    fn garbage_is_none_never_a_guess() {
        assert_eq!(parse_auth_status("not json"), None);
        assert_eq!(parse_auth_status(r#"{"loggedIn":true"#), None);
    }
}
