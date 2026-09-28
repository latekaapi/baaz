//! Who is signed in, from the wire's account state plus two display strings.
//!
//! Sign-in itself is on the wire now: the
//! app drives `account/read` / `account/loginStart` / `account/loginCancel` /
//! `account/logout` on its `muse serve` connection, and the screens in
//! `aui::screens::login` show what those report. This module owns none of
//! that. What it owns is the supplement: `auth.json` is read — never
//! written — only for the stored name and email the wire's `label` does not
//! always carry.
//!
//! Two rules this module keeps:
//!
//! * `auth.json` is read-only. Baaz never logs in, logs out, or
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
    /// From the wire's state plus the stored name/email. An id-looking
    /// wire `label` never wins over the stored name — it stays on the
    /// record only so [`Self::display_name`] can fall back past it to the
    /// email. `None` for `loggedOut`: no lane, no identity.
    pub fn from_account(state: &AccountState) -> Option<Identity> {
        if state.state == AccountStateKind::LoggedOut {
            return None;
        }
        let (stored_name, stored_email) = stored_name_and_email();
        let name = pick_name(state.label.clone(), stored_name, &state.state);
        Some(Identity {
            lane: state.state.clone(),
            name,
            email: stored_email.unwrap_or_default(),
        })
    }

    /// Whether `text` looks like an account id rather than a person's
    /// name: digits-only, or carrying a long numeric segment. The wire's
    /// `label` ("Display label for the credential in effect") has carried
    /// the id, and the footer must never show it — an id-looking label
    /// falls back to the email, then to the stored name.
    pub fn is_id_like(text: &str) -> bool {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return false;
        }
        if trimmed.chars().all(|c| c.is_ascii_digit()) {
            return true;
        }
        let mut run = 0u32;
        for c in trimmed.chars() {
            if c.is_ascii_digit() {
                run += 1;
                if run >= 8 {
                    return true;
                }
            } else {
                run = 0;
            }
        }
        false
    }

    /// The name the sidebar footer shows. The key lanes name themselves
    /// ("API key", never the stored key's label); otherwise the stored
    /// name (which [`Self::from_account`] already preferred over an
    /// id-looking wire label), unless it still looks like an account id
    /// — then the email, then a bare "Signed in". Never an id, never a
    /// plan, never a meter.
    pub fn display_name(&self) -> String {
        match self.lane {
            AccountStateKind::EnvKey => return "API key (environment)".to_owned(),
            AccountStateKind::ApiKey => return "API key".to_owned(),
            _ => {}
        }
        if !Self::is_id_like(&self.name) {
            return self.name.clone();
        }
        if !self.email.is_empty() {
            return self.email.clone();
        }
        "Signed in".into()
    }

    /// The avatar initial for the sidebar footer, from what the footer
    /// shows — so "A" on both key lanes, whose footer reads "API key".
    pub fn initial(&self) -> String {
        self.display_name().chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_else(|| "?".into())
    }

    /// Whether this identity bills pay-as-you-go by construction: the
    /// `apiKey` and `envKey` lanes.
    pub fn is_api_key(&self) -> bool {
        matches!(self.lane, AccountStateKind::ApiKey | AccountStateKind::EnvKey)
    }
}

/// Pick the record's name from the wire `label` and the stored name: a
/// clean label wins, then the stored name, then an id-looking label (kept
/// so [`Identity::display_name`] can still fall back past it to the
/// email), then the lane fallback. Pure, so tests drive it without a
/// credential file.
fn pick_name(
    label: Option<String>,
    stored: Option<String>,
    lane: &AccountStateKind,
) -> String {
    let label = label.filter(|label| !label.trim().is_empty());
    let clean = label.clone().filter(|label| !Identity::is_id_like(label));
    clean
        .or(stored)
        .or(label)
        .unwrap_or_else(|| match lane {
            AccountStateKind::EnvKey | AccountStateKind::ApiKey => "API key".to_owned(),
            _ => "Signed in".to_owned(),
        })
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

/// The two display strings, and the `auth.json` mtime they were read at.
///
/// Keyed the same way [`crate::tier::cached`] is, and for the same reason: a
/// logout and a re-login rewrite the file, and nothing else changes what is
/// in it. Process-global because `auth.json` is.
static STORED: std::sync::Mutex<Option<(Option<u64>, NameAndEmail)>> = std::sync::Mutex::new(None);

/// The pair [`stored_name_and_email`] hands back: a display name and an
/// email, either of which the file may not have.
type NameAndEmail = (Option<String>, Option<String>);

/// The name and email stored in `auth.json`'s `providers.meta`, when the
/// file has them. A supplement only: the wire owns sign-in, and this is
/// just the two display strings for when its `label` is absent.
///
/// Cached against the file's modification time (finding `support-12`):
/// `Identity::from_account` calls this on every `account/changed`, and the
/// file read and the JSON parse behind two strings do not need repeating
/// until the file itself changes.
pub fn stored_name_and_email() -> (Option<String>, Option<String>) {
    let mtime = crate::tier::auth_mtime();
    if let Ok(cache) = STORED.lock() {
        if let Some((cached, pair)) = cache.as_ref() {
            if *cached == mtime {
                return pair.clone();
            }
        }
    }
    let pair = read_name_and_email();
    if let Ok(mut cache) = STORED.lock() {
        *cache = Some((mtime, pair.clone()));
    }
    pair
}

/// [`stored_name_and_email`] without the cache: the read and the parse.
fn read_name_and_email() -> (Option<String>, Option<String>) {
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
        assert_eq!(identity.display_name(), "Ada Lovelace");
        assert!(!identity.is_api_key());
        assert_eq!(identity.initial(), "A");
    }

    #[test]
    fn the_key_lanes_name_themselves_and_bill_pay_as_you_go() {
        let stored = Identity::from_account(&state(AccountStateKind::ApiKey, None)).expect("signed-in lane");
        assert!(stored.is_api_key());
        assert_eq!(stored.display_name(), "API key");
        assert_eq!(stored.initial(), "A");
        let env = Identity::from_account(&state(AccountStateKind::EnvKey, None)).expect("signed-in lane");
        assert!(env.is_api_key());
        assert_eq!(env.display_name(), "API key (environment)");
        assert_eq!(env.initial(), "A");
    }

    #[test]
    fn the_key_lanes_initial_comes_from_the_footer_not_the_wire_label() {
        let stored =
            Identity::from_account(&state(AccountStateKind::ApiKey, Some("stored key"))).expect("signed-in lane");
        assert_eq!(stored.name, "stored key");
        assert_eq!(stored.display_name(), "API key");
        assert_eq!(stored.initial(), "A");
        let env =
            Identity::from_account(&state(AccountStateKind::EnvKey, Some("stored key"))).expect("signed-in lane");
        assert_eq!(env.display_name(), "API key (environment)");
        assert_eq!(env.initial(), "A");
    }

    #[test]
    fn id_looking_labels_never_show() {
        // Digits-only, or a long numeric segment, is the credential's id
        // wearing the label's clothes — the footer must never print it.
        assert!(Identity::is_id_like("27681631238169"));
        assert!(Identity::is_id_like("user-27681631238169"));
        assert!(Identity::is_id_like("  27681631238169…  "));
        assert!(!Identity::is_id_like("Ada Lovelace"));
        assert!(!Identity::is_id_like("API key"));
        assert!(!Identity::is_id_like("abc123"));
        assert!(!Identity::is_id_like(""));
        // An id-looking wire label loses to the stored name; with no
        // stored name it survives on the record but the display falls
        // back to the email, then to a bare "Signed in".
        assert_eq!(
            pick_name(
                Some("27681631238169".into()),
                Some("Ada Lovelace".into()),
                &AccountStateKind::AccountLogin
            ),
            "Ada Lovelace"
        );
        assert_eq!(
            pick_name(Some("27681631238169".into()), None, &AccountStateKind::AccountLogin),
            "27681631238169"
        );
        let by_email = Identity {
            lane: AccountStateKind::AccountLogin,
            name: "27681631238169".into(),
            email: "latekaapi@gmail.com".into(),
        };
        assert_eq!(by_email.display_name(), "latekaapi@gmail.com");
        assert_eq!(by_email.initial(), "L");
        let bare = Identity {
            lane: AccountStateKind::AccountLogin,
            name: "27681631238169".into(),
            email: String::new(),
        };
        assert_eq!(bare.display_name(), "Signed in");
        let clean = Identity {
            lane: AccountStateKind::AccountLogin,
            name: "Ada Lovelace".into(),
            email: "ada@example.com".into(),
        };
        assert_eq!(clean.display_name(), "Ada Lovelace");
    }

    #[test]
    fn an_unknown_lane_is_still_signed_in() {
        let identity =
            Identity::from_account(&state(AccountStateKind::Unknown("futureLane".to_owned()), None))
                .expect("open enum");
        assert!(!identity.is_api_key());
        assert_eq!(identity.display_name(), identity.name);
    }
}
