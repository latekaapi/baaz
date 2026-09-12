//! The `account/*` lane: which credential tier is in effect, and the two
//! login flows (device code and API key) that put one there.
//!
//! Part of [`crate::schema`]; see that module for the conventions every
//! type here follows.

use serde::{Deserialize, Serialize};

/// The one account payload shape: the `account/read` result and the
/// `account/changed` params.
///
/// Every `account/*` method is gated on `initialize.capabilities.
/// experimentalApi: true`; without the opt-in each one answers `-32601`
/// with `data.kind: "experimentalRequired"`. `label` is a display string
/// that never carries key material, and is omitted — never `null` — when
/// absent.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountState {
    /// `false` exactly when the effective provider endpoint needs no caller
    /// credential (keyless/gateway deployments), so those clients never nag
    /// the user to sign in.
    pub credential_required: bool,
    /// Display label for the credential in effect, when the server sends one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Which credential lane is in effect.
    pub state: AccountStateKind,
}

open_enum! {
    /// Which credential lane is in effect. Server-produced, so the
    /// vocabulary is open: a future lane projects additively.
    AccountStateKind {
        LoggedOut = "loggedOut",
        EnvKey = "envKey",
        ApiKey = "apiKey",
        AccountLogin = "accountLogin",
    }
}

closed_enum! {
    /// Which login flow `account/loginStart` runs. Client-produced, so the
    /// vocabulary is closed: an unknown `type` is an invalid-params error.
    AccountLoginType {
        DeviceCode = "deviceCode",
        ApiKey = "apiKey",
    }
}

/// `account/loginStart` params: `{type, apiKey?}`.
///
/// `apiKey` is required non-empty for the `apiKey` type and forbidden for
/// the `deviceCode` type; both violations are invalid-params errors. `Debug`
/// is hand-written: the derive would print the raw key bytes into any
/// `{:?}` context, and the key must never reach a log.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountLoginStartParams {
    /// The API key to store (the `apiKey` type only). The only
    /// secret-bearing member of the account surface.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Which flow to run.
    #[serde(rename = "type")]
    pub r#type: AccountLoginType,
}

impl std::fmt::Debug for AccountLoginStartParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountLoginStartParams")
            .field("type", &self.r#type)
            .field("apiKey", &"<redacted>")
            .finish()
    }
}

/// `account/loginStart` result: `{verificationUrl?, userCode?}` — both
/// present for a started device-code flow, both absent for the synchronous
/// `apiKey` type. The device-code artifacts travel only here, to the
/// requesting connection; completion notifications never carry them.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountLoginStartResult {
    /// The code the user confirms or enters at the verification URL
    /// (device-code flow only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_code: Option<String>,
    /// Where the user signs in (device-code flow only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_url: Option<String>,
}

open_enum! {
    /// How a login flow ended. Server-produced, so the vocabulary is open:
    /// a future flow may end in a new way additively.
    AccountLoginOutcome {
        Granted = "granted",
        Denied = "denied",
        Expired = "expired",
        Cancelled = "cancelled",
        Failed = "failed",
    }
}

/// `account/loginCompleted` params: `{outcome, message?}`. `message` is a
/// display string for the `denied` / `expired` / `failed` outcomes only —
/// never key material.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountLoginCompletedParams {
    /// Display message, on the non-granting outcomes only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// How the flow ended.
    pub outcome: AccountLoginOutcome,
}

/// `account/loginCancel` result: `{cancelled}` — `true` iff a pending flow
/// existed and was cancelled.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountLoginCancelResult {
    /// Whether a pending flow was cancelled.
    pub cancelled: bool,
}
