//! The Codex status probe: a short-lived `codex app-server` that runs
//! `initialize`, `account/read` and `account/rateLimits/read`, then exits.
//!
//! It reuses the client code in [`crate::child`] (the request builders and
//! [`crate::child::RunningChild`]) — never a second spelling of the wire.
//! The pure decoders below are what the tests drive, against scripted
//! `result` objects; only [`probe_app_server`] spawns, and no test calls it.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam_channel::unbounded;
use serde_json::Value;

use crate::child::{initialize_request, initialized_notification, RunningChild};
use crate::fold::CodexFold;

/// Who `account/read` says holds this machine's Codex login.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodexAccount {
    /// False when `account` is null and the server asks for OpenAI auth.
    pub signed_in: bool,
    /// The ChatGPT email, when the account names one.
    pub email: Option<String>,
    /// The plan type (`pro`, `prolite`, …), when reported.
    pub plan: Option<String>,
    /// `chatgpt` or `apiKey`, when known.
    pub method: Option<String>,
}

impl CodexAccount {
    /// The signed-out answer: no account, auth required.
    pub fn signed_out() -> Self {
        Self { signed_in: false, email: None, plan: None, method: None }
    }
}

fn text_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// Decode one `account/read` result: `{account, requiresOpenaiAuth}`.
/// `account` null plus `requiresOpenaiAuth` is Signed out; a `chatgpt`
/// account carries its email and plan type; an `apiKey` account is signed
/// in with no email. Anything unrecognised reads signed out — an account
/// the probe cannot see is not one the person can use.
pub fn parse_account_read(result: &Value) -> CodexAccount {
    let account = result.get("account").filter(|account| !account.is_null());
    let Some(account) = account else {
        return CodexAccount::signed_out();
    };
    let kind = account.get("type").and_then(Value::as_str).unwrap_or_default();
    match kind {
        "chatgpt" => CodexAccount {
            signed_in: true,
            email: text_field(account, "email"),
            plan: text_field(account, "planType"),
            method: Some("chatgpt".into()),
        },
        "apiKey" => CodexAccount {
            signed_in: true,
            email: None,
            plan: None,
            method: Some("apiKey".into()),
        },
        _ => CodexAccount::signed_out(),
    }
}

/// One Codex usage window in the neutral shape the status service stores.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RateWindow {
    /// e.g. `Primary · 7d`, labelled from `windowDurationMins`.
    pub label: String,
    /// Fraction used, 0.0–1.0.
    pub used_fraction: f64,
    /// Unix time the window resets, when reported.
    pub resets_at: Option<i64>,
}

/// Name one `windowDurationMins`: 10080 reads `7d`, 60 reads `1h`.
fn duration_label(mins: u64) -> String {
    if mins % 10080 == 0 {
        format!("{}d", mins / 1440)
    } else if mins % 1440 == 0 {
        format!("{}d", mins / 1440)
    } else if mins % 60 == 0 {
        format!("{}h", mins / 60)
    } else {
        format!("{mins}m")
    }
}

fn decode_window(name: &str, window: &Value) -> Option<RateWindow> {
    let mins = window.get("windowDurationMins").and_then(Value::as_u64)?;
    let used = window.get("usedPercent").and_then(Value::as_f64).unwrap_or(0.0);
    Some(RateWindow {
        label: format!("{name} · {}", duration_label(mins)),
        used_fraction: (used / 100.0).clamp(0.0, 1.0),
        resets_at: window.get("resetsAt").and_then(Value::as_i64),
    })
}

/// Decode one `account/rateLimits/read` result (or the `rateLimits` params
/// of an `account/rateLimits/updated` push — same object): the plan plus
/// the primary/secondary windows, in that order. Accepts either the bare
/// limits object or `{rateLimits: …}`. An answer naming no window is no
/// windows, never a guess.
pub fn parse_rate_limit_windows(result: &Value) -> (Option<String>, Vec<RateWindow>) {
    let limits = result.get("rateLimits").unwrap_or(result);
    let plan = text_field(limits, "planType");
    let mut windows = Vec::new();
    if let Some(primary) = limits.get("primary") {
        if let Some(window) = decode_window("Primary", primary) {
            windows.push(window);
        }
    }
    if let Some(secondary) = limits.get("secondary") {
        if let Some(window) = decode_window("Secondary", secondary) {
            windows.push(window);
        }
    }
    (plan, windows)
}

/// What the short-lived app-server learned: the account plus the plan and
/// windows from the rate-limit read, when it answered.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CodexProbe {
    /// The decoded `account/read`.
    pub account: CodexAccount,
    /// The plan, when any rate-limit answer named one.
    pub plan: Option<String>,
    /// The decoded windows, when any were reported.
    pub windows: Vec<RateWindow>,
}

impl CodexProbe {
    /// A probe that learned only that nobody is signed in.
    pub fn signed_out() -> Self {
        Self { account: CodexAccount::signed_out(), plan: None, windows: Vec::new() }
    }
}

/// Run the status probe against `program`: spawn a short-lived
/// `codex app-server`, shake hands (`initialize` + `initialized`), then
/// `account/read` and `account/rateLimits/read`, and hang up. Read-only:
/// no thread starts, no turn runs, nothing is written. Blocking; call it
/// off the UI thread. `Err` carries the human reason — spawn failure,
/// handshake failure, or a per-method wait past `timeout`.
pub fn probe_app_server(program: &str, timeout: Duration) -> Result<CodexProbe, String> {
    let (events, _dropped) = unbounded();
    let mut child = RunningChild::spawn(
        program,
        &[],
        Arc::new(Mutex::new(CodexFold::new())),
        events,
    )
    .map_err(|error| format!("could not spawn {program} app-server: {error}"))?;
    let failed = |error: crate::child::RequestError| error.reason;
    let account = (|| {
        child
            .send_frame(initialize_request(child.next_request_id(), env!("CARGO_PKG_VERSION")))
            .map_err(failed)?;
        child
            .send_notification("initialized", initialized_notification()["params"].clone())
            .map_err(|error| format!("the session child is unreachable: {error}"))?;
        let account = child
            .send_request_with_timeout("account/read", serde_json::json!({}), timeout)
            .map_err(failed)?;
        let limits = child
            .send_request_with_timeout("account/rateLimits/read", serde_json::json!({}), timeout)
            .map_err(failed)?;
        let (plan, windows) = parse_rate_limit_windows(&limits);
        Ok::<CodexProbe, String>(CodexProbe {
            account: parse_account_read(&account),
            plan,
            windows,
        })
    })();
    child.shutdown();
    account
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn null_account_with_openai_auth_required_is_signed_out() {
        let result = json!({"account": null, "requiresOpenaiAuth": true});
        let account = parse_account_read(&result);
        assert!(!account.signed_in);
        assert_eq!(account.email, None);
    }

    #[test]
    fn a_chatgpt_account_decodes_email_and_plan() {
        let result =
            json!({"account": {"type": "chatgpt", "email": "a@x.com", "planType": "pro"}});
        let account = parse_account_read(&result);
        assert!(account.signed_in);
        assert_eq!(account.email.as_deref(), Some("a@x.com"));
        assert_eq!(account.plan.as_deref(), Some("pro"));
        assert_eq!(account.method.as_deref(), Some("chatgpt"));
    }

    #[test]
    fn an_api_key_account_is_signed_in_without_email() {
        let result = json!({"account": {"type": "apiKey"}});
        let account = parse_account_read(&result);
        assert!(account.signed_in);
        assert_eq!(account.method.as_deref(), Some("apiKey"));
    }

    #[test]
    fn rate_limit_windows_label_from_window_duration() {
        let result = json!({
            "planType": "pro",
            "primary": {"usedPercent": 19, "windowDurationMins": 10080, "resetsAt": 1790588038},
            "secondary": {"usedPercent": 50, "windowDurationMins": 60, "resetsAt": 1790187000}
        });
        let (plan, windows) = parse_rate_limit_windows(&result);
        assert_eq!(plan.as_deref(), Some("pro"));
        assert_eq!(windows.len(), 2);
        assert!(windows[0].label.contains("7d"), "unexpected label {}", windows[0].label);
        assert!((windows[0].used_fraction - 0.19).abs() < 1e-9);
        assert_eq!(windows[0].resets_at, Some(1790588038));
        assert!(windows[1].label.contains('h'), "unexpected label {}", windows[1].label);
    }

    #[test]
    fn an_empty_rate_limit_answer_is_no_windows_never_a_guess() {
        let (plan, windows) = parse_rate_limit_windows(&json!({}));
        assert_eq!(plan, None);
        assert!(windows.is_empty());
    }
}
