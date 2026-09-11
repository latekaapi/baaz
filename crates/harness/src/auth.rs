//! Who is signed in, from the wire's account state plus two display strings.
//!
//! Sign-in itself is on the wire now (`docs/diagnosis/login.md`, D22): the
//! app drives `account/read` / `account/loginStart` / `account/loginCancel` /
//! `account/logout` on its `muse serve` connection, and the screens in
//! `aui::screens::login` show what those report. This module owns none of
//! that. What it owns is the supplement: `auth.json` is read — never
//! written — only for the stored name and email the wire's `label` does not
//! always carry.
//!
//! Two rules this module keeps:
//!
//! * `auth.json` is read-only. The harness never logs in, logs out, or
//!   edits the credential out-of-band; `Muse`'s storage is Muse's.
//! * **The verification URL, the user code and the API key never reach a
//!   log.** They travel from the `account/loginStart` result or the masked
//!   field straight onto the screen or back into the wire call, and nowhere
//!   else.

use std::path::PathBuf;

use muse_client::schema::{AccountState, AccountStateKind};

/// Who the credential belongs to, as the sidebar footer shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    /// Which wire lane this identity came from.
    pub lane: AccountStateKind,
    /// The wire's `label`, or the stored `user_full_name` when the label is
    /// absent — or a lane fallback when both are missing.
    pub name: String,
    /// The stored `user_email`; empty when nothing stored one.
    pub email: String,
}

impl Identity {
    /// From the wire's state plus the stored name/email when `label` is
    /// absent. `None` for `loggedOut`: no lane, no identity.
    pub fn from_account(state: &AccountState) -> Option<Identity> {
        if state.state == AccountStateKind::LoggedOut {
            return None;
        }
        let (stored_name, stored_email) = stored_name_and_email();
        let name = state
            .label
            .clone()
            .filter(|label| !label.trim().is_empty())
            .or(stored_name)
            .unwrap_or_else(|| match &state.state {
                AccountStateKind::EnvKey | AccountStateKind::ApiKey => "API key".to_owned(),
                _ => "Signed in".to_owned(),
            });
        Some(Identity {
            lane: state.state.clone(),
            name,
            email: stored_email.unwrap_or_default(),
        })
    }

    /// The avatar initial for the sidebar footer, from what the footer
    /// shows — so "A" on both key lanes, whose footer reads "API key".
    pub fn initial(&self) -> String {
        self.footer_name().chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_else(|| "?".into())
    }

    /// The footer name: "API key" on the stored-key lane, "API key
    /// (environment)" on the environment lane (which cannot be signed out
    /// from the app), otherwise the person's name.
    pub fn footer_name(&self) -> String {
        match self.lane {
            AccountStateKind::EnvKey => "API key (environment)".to_owned(),
            AccountStateKind::ApiKey => "API key".to_owned(),
            _ => self.name.clone(),
        }
    }

    /// Whether this identity bills pay-as-you-go by construction: the
    /// `apiKey` and `envKey` lanes.
    pub fn is_api_key(&self) -> bool {
        matches!(self.lane, AccountStateKind::ApiKey | AccountStateKind::EnvKey)
    }
}

/// `~/.config/muse/auth.json`, honouring `MUSE_AUTH_PATH` and
/// `XDG_CONFIG_HOME` the way the launcher does.
pub fn auth_path() -> PathBuf {
    if let Some(path) = std::env::var_os("MUSE_AUTH_PATH") {
        return PathBuf::from(path);
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"));
    base.join("muse").join("auth.json")
}

/// The name and email stored in `auth.json`'s `providers.meta`, when the
/// file has them. A supplement only: the wire owns sign-in, and this is
/// just the two display strings for when its `label` is absent.
pub fn stored_name_and_email() -> (Option<String>, Option<String>) {
    let text = std::fs::read_to_string(auth_path()).ok();
    let meta = text
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|value| value.get("providers")?.get("meta").cloned());
    let field = |name: &str| {
        meta.as_ref()
            .and_then(|meta| meta.get(name))
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    (field("user_full_name"), field("user_email"))
}

/// Hand the verification page to the browser (`open <url>`).
///
/// The URL never reaches a log; it goes straight into the child's argv.
pub fn open_in_browser(url: &str) -> std::io::Result<()> {
    std::process::Command::new("open")
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(kind: AccountStateKind, label: Option<&str>) -> AccountState {
        AccountState {
            credential_required: true,
            label: label.map(str::to_owned),
            state: kind,
        }
    }

    #[test]
    fn logged_out_has_no_identity() {
        assert_eq!(Identity::from_account(&state(AccountStateKind::LoggedOut, Some("nobody"))), None);
    }

    #[test]
    fn the_wire_label_wins_over_the_store() {
        let identity = Identity::from_account(&state(AccountStateKind::AccountLogin, Some("Ada Lovelace")))
            .expect("signed-in lane");
        assert_eq!(identity.name, "Ada Lovelace");
        assert_eq!(identity.footer_name(), "Ada Lovelace");
        assert!(!identity.is_api_key());
        assert_eq!(identity.initial(), "A");
    }

    #[test]
    fn the_key_lanes_name_themselves_and_bill_pay_as_you_go() {
        let stored = Identity::from_account(&state(AccountStateKind::ApiKey, None)).expect("signed-in lane");
        assert!(stored.is_api_key());
        assert_eq!(stored.footer_name(), "API key");
        assert_eq!(stored.initial(), "A");
        let env = Identity::from_account(&state(AccountStateKind::EnvKey, None)).expect("signed-in lane");
        assert!(env.is_api_key());
        assert_eq!(env.footer_name(), "API key (environment)");
        assert_eq!(env.initial(), "A");
    }

    #[test]
    fn the_key_lanes_initial_comes_from_the_footer_not_the_wire_label() {
        let stored =
            Identity::from_account(&state(AccountStateKind::ApiKey, Some("stored key"))).expect("signed-in lane");
        assert_eq!(stored.name, "stored key");
        assert_eq!(stored.footer_name(), "API key");
        assert_eq!(stored.initial(), "A");
        let env =
            Identity::from_account(&state(AccountStateKind::EnvKey, Some("stored key"))).expect("signed-in lane");
        assert_eq!(env.footer_name(), "API key (environment)");
        assert_eq!(env.initial(), "A");
    }

    #[test]
    fn an_unknown_lane_is_still_signed_in() {
        let identity =
            Identity::from_account(&state(AccountStateKind::Unknown("futureLane".to_owned()), None))
                .expect("open enum");
        assert!(!identity.is_api_key());
        assert_eq!(identity.footer_name(), identity.name);
    }
}
