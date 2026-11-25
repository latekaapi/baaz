//! One billing rule for every provider (docs/85 11.6, W3).
//!
//! Each provider reports how its login pays ([`BillingMode`]): Claude Code
//! and Codex through the sign-in their status probe read
//! ([`crate::provider_status::Auth::billing_mode`]), the provider with a
//! billing tier through its tier probe ([`report`]). The rule is the same
//! everywhere:
//!
//! * The session header shows per-token billing whenever it is the mode
//!   ([`mode`]).
//! * A send is held only when the mode **changed** since the last send on
//!   that account, or is unknown now when it used to be known ([`check`]).
//!   The hold is one question, the same on every provider
//!   ([`Hold::question`]); answering it ([`note_sent`]) makes the new mode
//!   the expected one.
//!
//! The last send's mode per provider and account is kept in
//! `<state dir>/billing-last-send.json` (a capture's isolated state dir; a
//! test build keeps it in memory, per thread).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
#[cfg(not(feature = "test-support"))]
use std::sync::Mutex;

pub use provider::BillingMode;

use crate::providers::ProviderId;

/// What the rule keeps between calls: modes a probe outside the status
/// reported (the billing tier's), accounts whose unknown mode the person
/// sent through this run (an unknown never overwrites the known mode it is
/// compared against), and — in a test build — the last sends themselves.
#[derive(Default)]
struct State {
    reported: HashMap<ProviderId, BillingMode>,
    unknown_accepted: HashSet<String>,
    #[cfg(feature = "test-support")]
    last: HashMap<String, String>,
}

/// The rule's state. Process-wide in the app; per thread in a test build
/// (`test-support`), so one test's mode change never holds another
/// test's send.
fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
    #[cfg(feature = "test-support")]
    {
        thread_local!(static STATE: std::cell::RefCell<State> = std::cell::RefCell::default());
        STATE.with(|state| f(&mut state.borrow_mut()))
    }
    #[cfg(not(feature = "test-support"))]
    {
        static STATE: Mutex<Option<State>> = Mutex::new(None);
        f(STATE.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(State::default))
    }
}

/// A provider whose mode comes from its own probe (the billing tier)
/// reports it here, and then it wins over what the sign-in says; `None`
/// (no tier yet, or signed out) hands the answer back to the sign-in.
pub fn report(id: ProviderId, mode: Option<BillingMode>) {
    with_state(|state| match mode {
        Some(mode) => state.reported.insert(id, mode),
        None => state.reported.remove(&id),
    });
}

/// How `id`'s login pays for turns now.
pub fn mode(id: ProviderId) -> BillingMode {
    if let Some(mode) = with_state(|state| state.reported.get(&id).copied()) {
        return mode;
    }
    crate::provider_status::live_auth(id).billing_mode()
}

/// The account a send is remembered under: provider and email (empty when
/// the login named none).
fn account_key(id: ProviderId) -> String {
    let auth = crate::provider_status::live_auth(id);
    format!("{}|{}", id.as_str(), auth.email().unwrap_or(""))
}

fn store_path() -> PathBuf {
    crate::store::support_dir().join("billing-last-send.json")
}

#[cfg(not(feature = "test-support"))]
fn load(path: &Path) -> HashMap<String, String> {
    std::fs::read_to_string(path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

#[cfg(not(feature = "test-support"))]
fn save(path: &Path, map: &HashMap<String, String>) {
    if let Ok(text) = serde_json::to_string_pretty(map) {
        let _ = crate::store::write_atomic(path, text.as_bytes());
    }
}

#[cfg(feature = "test-support")]
fn load(_path: &Path) -> HashMap<String, String> {
    with_state(|state| state.last.clone())
}

#[cfg(feature = "test-support")]
fn save(_path: &Path, map: &HashMap<String, String>) {
    with_state(|state| state.last = map.clone());
}

/// Forget everything the rule kept on this thread: a test that changed
/// modes leaves the next test on its thread a clean slate.
#[cfg(feature = "test-support")]
pub fn reset() {
    with_state(|state| *state = State::default());
}

/// Why a send is held: the one question.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hold {
    /// The mode moved from `from` to `to` since the last send.
    Changed {
        /// The mode at the last send.
        from: BillingMode,
        /// The mode now.
        to: BillingMode,
    },
    /// The last send's mode was known; now nobody can say.
    NoLongerKnown {
        /// The mode at the last send.
        was: BillingMode,
    },
}

impl Hold {
    /// The one question, the same on every provider; `who` is the
    /// provider's label.
    pub fn question(self, who: &str) -> String {
        match self {
            Hold::Changed { from, to } => format!(
                "{who}'s billing changed from {} to {} since your last message. Send anyway?",
                from.words(),
                to.words()
            ),
            Hold::NoLongerKnown { was } => format!(
                "{who} no longer says how this login is billed (it was {}). Send anyway?",
                was.words()
            ),
        }
    }
}

/// The rule itself: `last` is the mode at the last send on the account
/// (`None` when there was none), `now` the mode now.
pub fn rule(last: Option<BillingMode>, now: BillingMode) -> Option<Hold> {
    let last = last.filter(|mode| mode.is_known())?;
    match now {
        BillingMode::Unknown => Some(Hold::NoLongerKnown { was: last }),
        now if now != last => Some(Hold::Changed { from: last, to: now }),
        _ => None,
    }
}

/// Whether a send on `id` is held now, and why.
pub fn check(id: ProviderId) -> Option<Hold> {
    check_at(&store_path(), &account_key(id), mode(id))
}

fn check_at(path: &Path, key: &str, now: BillingMode) -> Option<Hold> {
    if now == BillingMode::Unknown && with_state(|state| state.unknown_accepted.contains(key)) {
        return None;
    }
    rule(load(path).get(key).map(|raw| BillingMode::parse(raw)), now)
}

/// A send on `id` left (or the person answered the hold): its mode is the
/// expected one from now on. An unknown mode is accepted for this run only
/// and never overwrites the known one.
pub fn note_sent(id: ProviderId) {
    note_sent_at(&store_path(), &account_key(id), mode(id));
}

fn note_sent_at(path: &Path, key: &str, now: BillingMode) {
    if now == BillingMode::Unknown {
        with_state(|state| state.unknown_accepted.insert(key.to_owned()));
        return;
    }
    let mut map = load(path);
    if map.get(key).map(String::as_str) != Some(now.as_str()) {
        map.insert(key.to_owned(), now.as_str().to_owned());
        save(path, &map);
    }
}

/// Set the last send's mode for `id` directly: a capture's fixture for the
/// hold (`billing-last:<provider>:<mode>`).
#[cfg(feature = "probe")]
pub fn fixture_last_send(id: ProviderId, mode: BillingMode) {
    let mut map = load(&store_path());
    map.insert(account_key(id), mode.as_str().to_owned());
    save(&store_path(), &map);
}

#[cfg(test)]
mod tests {
    use super::*;
    use BillingMode::*;

    #[test]
    fn the_rule_holds_only_a_change_or_a_mode_that_went_unknown() {
        assert_eq!(rule(None, PerToken), None, "a first send is never held: the header says per-token");
        assert_eq!(rule(Some(Subscription), Subscription), None);
        assert_eq!(rule(Some(PerToken), PerToken), None, "per-token as expected is not a surprise");
        assert_eq!(rule(Some(Subscription), PerToken), Some(Hold::Changed { from: Subscription, to: PerToken }));
        assert_eq!(rule(Some(PerToken), Subscription), Some(Hold::Changed { from: PerToken, to: Subscription }));
        assert_eq!(rule(Some(Subscription), Unknown), Some(Hold::NoLongerKnown { was: Subscription }));
        assert_eq!(rule(Some(Unknown), PerToken), None, "nothing was known to change from");
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("baaz-billing-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp");
        dir.join("billing-last-send.json")
    }

    /// One walk per provider shape: Claude Code (a key appears), Codex
    /// (`chatgpt` → `apiKey`), the billing tier (plan → unknown).
    #[test]
    fn each_provider_walks_the_same_rule() {
        let path = temp("walk");
        // Claude Code: a subscription, then ANTHROPIC_API_KEY shows up.
        let claude = "claude-code|a@b";
        let sub = Auth::SignedIn { email: None, plan: Some("Claude Max".into()), method: Some("claude.ai".into()), source: None };
        let key = Auth::SignedIn { email: None, plan: None, method: Some("claude.ai".into()), source: Some("ANTHROPIC_API_KEY".into()) };
        note_sent_at(&path, claude, sub.billing_mode());
        assert_eq!(check_at(&path, claude, sub.billing_mode()), None);
        let held = check_at(&path, claude, key.billing_mode()).expect("held");
        assert_eq!(held, Hold::Changed { from: Subscription, to: PerToken });
        assert!(held.question("Claude Code").ends_with("Send anyway?"));
        note_sent_at(&path, claude, key.billing_mode());
        assert_eq!(check_at(&path, claude, key.billing_mode()), None, "answered once, expected after");

        // Codex: chatgpt → apiKey.
        let codex = "codex|";
        let chatgpt = Auth::SignedIn { email: None, plan: Some("pro".into()), method: Some("chatgpt".into()), source: None };
        let api = Auth::SignedIn { email: None, plan: None, method: Some("apiKey".into()), source: None };
        note_sent_at(&path, codex, chatgpt.billing_mode());
        assert!(check_at(&path, codex, api.billing_mode()).is_some());

        // The billing tier: a plan, then the probe cannot say.
        let tier = "muse|";
        note_sent_at(&path, tier, Subscription);
        assert_eq!(check_at(&path, tier, Unknown), Some(Hold::NoLongerKnown { was: Subscription }));
        note_sent_at(&path, tier, Unknown);
        assert_eq!(check_at(&path, tier, Unknown), None, "sent through once: not asked again this run");
        assert_eq!(load(&path).get(tier).map(String::as_str), Some("subscription"), "an unknown never overwrites the known");
        assert!(check_at(&path, tier, PerToken).is_some(), "a later change still holds");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    use crate::provider_status::Auth;
}
