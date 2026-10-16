//! The `model/*` lane (SS3.10): what a provider offers and the reasoning
//! effort a turn asks for.
//!
//! Part of [`crate::schema`]; see that module for the conventions every
//! type here follows.

use serde::{Deserialize, Serialize};

/// One visible catalog row (tdd SS3.10). Catalog rows marked hidden never reach the wire, so a
/// client cannot select one.
///
/// The three effort fields (1.4.9, ADR 33400 D2) carry tiers as their **wire strings**, not
/// [`ReasoningEffort`]: that enum is closed, and a tier a newer host adds must never fail the row
/// or the list. A field whose shape does not decode at all reads as absent, for the same reason.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCatalogEntry {
    /// Context limit; `null` when the catalog source declared nothing. Required-nullable.
    #[serde(default)]
    pub context_limit: Option<u64>,
    /// Cost block; `null` when the catalog source declared nothing. Required-nullable.
    #[serde(default)]
    pub cost: Option<ModelCost>,
    /// The row's catalog default tier; always a member of `variants` when present. Absent from
    /// older hosts and under `"unknown"`.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "tolerant")]
    pub default_reasoning_effort: Option<String>,
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
    /// Described selectable tiers, a same-order subset of `variants`. Absent from older hosts
    /// and under `"unknown"`.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "tolerant")]
    pub reasoning_effort_variants: Option<Vec<ModelDescribedReasoningEffort>>,
    /// Release date; `null` when the catalog source declared nothing. Required-nullable.
    #[serde(default)]
    pub release_date: Option<String>,
    /// Complete ordered selectable tiers, or the explicit unknown capability. Absent from older
    /// hosts; current hosts always send it.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "tolerant")]
    pub variants: Option<ModelReasoningEffortVariants>,
}

impl ModelCatalogEntry {
    /// The tiers this row offers, in host order, as wire strings: `variants` when it lists them,
    /// else the described tiers. `None` when the host sent neither (an older host) or said
    /// `"unknown"` — the caller falls back to the whole vocabulary.
    #[must_use]
    pub fn effort_tiers(&self) -> Option<Vec<&str>> {
        match &self.variants {
            Some(ModelReasoningEffortVariants::Tiers(tiers)) => {
                Some(tiers.iter().map(String::as_str).collect())
            }
            Some(ModelReasoningEffortVariants::Unknown(_)) => None,
            None => self
                .reasoning_effort_variants
                .as_ref()
                .map(|described| described.iter().map(|d| d.tier.as_str()).collect()),
        }
    }

    /// The catalog's display string for `tier`, when it declared one.
    #[must_use]
    pub fn effort_description(&self, tier: &str) -> Option<&str> {
        self.reasoning_effort_variants
            .as_ref()?
            .iter()
            .find(|d| d.tier == tier)?
            .description
            .as_deref()
    }
}

/// One described tier of a `model/list` row (1.4.9, ADR 33400 D2): a selectable tier plus the
/// catalog's display string for it when declared. The tier is its wire string (see
/// [`ModelCatalogEntry`]).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelDescribedReasoningEffort {
    /// The catalog's per-tier display string; absent when the catalog declared none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The tier, as sent.
    pub tier: String,
}

/// `ModelCatalogEntry.variants`: a complete ordered tier set, or the explicit `"unknown"`
/// capability (any other scalar is kept as sent rather than failing the row).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ModelReasoningEffortVariants {
    /// The ordered tiers, as wire strings.
    Tiers(Vec<String>),
    /// `"unknown"`: the host cannot say which tiers the model accepts.
    Unknown(String),
}

/// Decode an optional field, reading a shape that does not decode as absent instead of failing
/// the enclosing row — the effort fields must never cost the person the whole model list.
fn tolerant<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).ok())
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

open_enum! {
    /// What drove a reasoning-effort default change (tdd SS4.6.9). Open; v1's only producer is
    /// `user` (an accepted `session/setReasoningEffort`).
    ReasoningEffortChangeSource {
        User = "user",
        Default = "default",
        Policy = "policy",
    }
}

/// The snapshot's standing session-default reasoning effort (tdd SS4.9.1, ADR 31255 D1): the fold
/// of the latest completed `runtime.reasoning_effort_reconfigure` fact — the same pair
/// `session/reasoningEffortChanged` carries.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningEffortState {
    /// The standing default tier — the closed SS3.2 vocabulary, spelled identically to
    /// `turn/start.reasoningEffort`.
    pub reasoning_effort: ReasoningEffort,
    /// Which actor class set the default.
    pub source: ReasoningEffortChangeSource,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(extra: serde_json::Value) -> serde_json::Value {
        let mut base = json!({
            "contextLimit": null, "cost": null, "description": null, "displayLabel": "M",
            "isActive": false, "isDefault": false, "modelId": "m", "outputLimit": null,
            "profileId": null, "providerId": "p", "releaseDate": null,
        });
        base.as_object_mut().expect("object").extend(extra.as_object().expect("object").clone());
        base
    }

    #[test]
    fn effort_fields_decode_and_round_trip() {
        let wire = row(json!({
            "defaultReasoningEffort": "medium",
            "reasoningEffortVariants": [{"tier": "low", "description": "Quick"}, {"tier": "medium"}],
            "variants": ["low", "medium", "high"],
        }));
        let entry: ModelCatalogEntry = serde_json::from_value(wire.clone()).expect("decodes");
        assert_eq!(entry.default_reasoning_effort.as_deref(), Some("medium"));
        assert_eq!(entry.effort_tiers(), Some(vec!["low", "medium", "high"]));
        assert_eq!(entry.effort_description("low"), Some("Quick"));
        assert_eq!(entry.effort_description("high"), None);
        assert_eq!(serde_json::to_value(&entry).expect("encodes"), wire);
    }

    #[test]
    fn an_older_host_or_unknown_capability_offers_no_tiers() {
        let old: ModelCatalogEntry = serde_json::from_value(row(json!({}))).expect("decodes");
        assert_eq!(old.effort_tiers(), None);
        assert_eq!(old.default_reasoning_effort, None);
        let unknown: ModelCatalogEntry =
            serde_json::from_value(row(json!({"variants": "unknown"}))).expect("decodes");
        assert_eq!(unknown.effort_tiers(), None);
        // Described tiers alone still name the set.
        let described: ModelCatalogEntry = serde_json::from_value(row(json!({
            "reasoningEffortVariants": [{"tier": "high"}],
        })))
        .expect("decodes");
        assert_eq!(described.effort_tiers(), Some(vec!["high"]));
    }

    #[test]
    fn a_new_tier_or_a_bad_shape_never_fails_the_row() {
        let entry: ModelCatalogEntry = serde_json::from_value(row(json!({
            "defaultReasoningEffort": "hyper",
            "variants": ["low", "hyper"],
            "reasoningEffortVariants": 7,
        })))
        .expect("an unknown tier decodes");
        assert_eq!(entry.effort_tiers(), Some(vec!["low", "hyper"]));
        assert_eq!(entry.default_reasoning_effort.as_deref(), Some("hyper"));
        assert_eq!(entry.reasoning_effort_variants, None, "a bad shape reads as absent");
        let list: ModelListResult = serde_json::from_value(json!({
            "models": [row(json!({"variants": {"odd": true}, "defaultReasoningEffort": 3}))],
            "profileId": null, "providerId": "p", "source": "providerCatalog",
        }))
        .expect("the list decodes");
        assert_eq!(list.models[0].variants, None);
        assert_eq!(list.models[0].default_reasoning_effort, None);
    }
}
