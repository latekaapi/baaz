//! The `model/*` lane (SS3.10): what a provider offers and the reasoning
//! effort a turn asks for.
//!
//! Part of [`crate::schema`]; see that module for the conventions every
//! type here follows.

use serde::{Deserialize, Serialize};

/// One visible catalog row (tdd SS3.10). Catalog rows marked hidden never reach the wire, so a
/// client cannot select one.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCatalogEntry {
    /// Context limit; `null` when the catalog source declared nothing. Required-nullable.
    #[serde(default)]
    pub context_limit: Option<u64>,
    /// Cost block; `null` when the catalog source declared nothing. Required-nullable.
    #[serde(default)]
    pub cost: Option<ModelCost>,
    /// Description; `null` when the catalog source declared nothing. Required-nullable.
    #[serde(default)]
    pub description: Option<String>,
    /// Presentation label.
    pub display_label: String,
    /// Marks the session's effective model when `sessionId` was supplied. May be false for every
    /// row — a client MUST NOT assume exactly one.
    pub is_active: bool,
    /// Marks the catalog's default row. May be false for every row.
    pub is_default: bool,
    /// The selectable model id.
    pub model_id: String,
    /// Output limit; `null` when the catalog source declared nothing. Required-nullable.
    #[serde(default)]
    pub output_limit: Option<u64>,
    /// Provider profile; `null` when the catalog source declared nothing. Required-nullable.
    #[serde(default)]
    pub profile_id: Option<String>,
    /// Provider routing.
    pub provider_id: String,
    /// Release date; `null` when the catalog source declared nothing. Required-nullable.
    #[serde(default)]
    pub release_date: Option<String>,
}

open_enum! {
    /// Where a model catalog came from (tdd SS3.10) — how a client tells a live provider catalog
    /// from a test fake and labels it honestly.
    ModelCatalogSource {
        ProviderCatalog = "providerCatalog",
        FakeCatalog = "fakeCatalog",
        UnresolvedCatalog = "unresolvedCatalog",
        BundledCatalog = "bundledCatalog",
        ConfigCatalog = "configCatalog",
    }
}

open_enum! {
    /// What drove a model selection (tdd SS4.6.1).
    ModelChangeSource {
        User = "user",
        Default = "default",
        Policy = "policy",
    }
}

/// Per-1M-token catalog cost, carried **verbatim** for display and never rounded or re-formatted by
/// the host (tdd SS3.10). Cost arithmetic stays client-local view math.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCost {
    /// Cached-input cost, a decimal string.
    pub cached: String,
    /// Currency; nullable — today's catalogs are USD. Required-nullable.
    #[serde(default)]
    pub currency: Option<String>,
    /// Input cost, a decimal string.
    pub input: String,
    /// Output cost, a decimal string.
    pub output: String,
}

/// `model/list` params (tdd SS3.10): the discovery half of the model-picker gesture. **A query, not
/// a command** — no `commandId`, no durable intake record, no view event.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelListParams {
    /// When present, the row matching that session's effective model is flagged `isActive`. When
    /// absent, no row is active — a client may list before it has a session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// `model/list` result (tdd SS3.10). A snapshot at call time: v1 has no catalog subscription.
/// `models` MAY be empty — a build shipped with no bundled models is a supported configuration.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelListResult {
    /// Visible rows only, newest `releaseDate` first.
    pub models: Vec<ModelCatalogEntry>,
    /// The catalog's profile; `null` when none. Required-nullable.
    #[serde(default)]
    pub profile_id: Option<String>,
    /// The catalog's provider.
    pub provider_id: String,
    /// Where the catalog came from.
    pub source: ModelCatalogSource,
}

/// A model selection (tdd SS3.8). Empty strings are normalized to absent.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSelection {
    /// Presentation label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_label: Option<String>,
    /// Catalog model id. Required.
    pub model_id: String,
    /// Provider profile. Omitted and explicit `null` both mean "no profile".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    /// Provider routing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
}

/// Rich content the model saw beyond text (tdd SS4.5.5): metadata only — fetch bytes via
/// `item/readOutput` when an `outputRef` exists.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelVisibleContent {
    /// Pixel height, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    /// The content media type.
    pub media_type: String,
    /// The recorded content path.
    pub path: String,
    /// The tool that produced the content.
    pub source_tool_name: String,
    /// Content type, `"image"` in v1 (free string, grows with the durable vocabulary).
    #[serde(rename = "type")]
    pub r#type: String,
    /// Pixel width, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
}

closed_enum! {
    /// The reasoning-effort tier sampled at submission (tdd SS3.2, SS3.3). The **same closed tier
    /// vocabulary** on both the fresh-turn and steer lanes; invalid tiers are invalid params.
    /// `none` is a tier (ask for no reasoning), not a way to say "unset".
    ReasoningEffort {
        None = "none",
        Minimal = "minimal",
        Low = "low",
        Medium = "medium",
        High = "high",
        Xhigh = "xhigh",
        Max = "max",
        Ultra = "ultra",
    }
}
