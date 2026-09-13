//! Everything the lanes share: the envelope, errors, capabilities,
//! platform and server info, the background and command records, and the
//! published method and notification index.
//!
//! Part of [`crate::schema`]; see that module for the conventions every
//! type here follows.

use serde::{Deserialize, Serialize};

use super::*;

open_enum! {
    /// Who durably backgrounded a task (tdd SS4.5.5).
    BackgroundInitiator {
        User = "user",
        Timeout = "timeout",
    }
}

/// The latest branch observation in the snapshot (tdd SS4.6.4, SS4.9.1).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchState {
    /// The observed branch; `null` on a detached-HEAD observation. Required-nullable.
    #[serde(default)]
    pub branch: Option<String>,
    /// The detected version-control system; absent when none was detected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vcs: Option<Vcs>,
    /// The observed workspace root.
    pub workspace_root: String,
}

open_enum! {
    /// A grantable capability name (SS1.4.4).
    CapabilityName {
        UserShell = "userShell",
        SessionMcp = "sessionMcp",
    }
}

/// The client's requested capability posture (SS1.4.1). Every member defaults; an absent
/// `capabilities` object means all defaults.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientCapabilities {
    /// Opt into experimental methods, fields, and variants (default `false`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experimental_api: Option<bool>,
    /// Exact notification method names the client declines; no wildcards; unknown names accepted
    /// and ignored; the protected set (SS1.7) cannot be opted out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opt_out_notification_methods: Option<Vec<String>>,
    /// Names from the capability registry. Unknown entries are not an error and simply do not
    /// appear in `grantedCapabilities` — which is why this is a list of free strings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_capabilities: Option<Vec<String>>,
    /// Whether this client can render a `userInput/request` dialog and answer it. **Absent
    /// means capable**: a client that predates the member keeps today's contract, so no existing
    /// client changes its handshake. `false` makes the host withhold `userInput/request`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_input_dialogs: Option<bool>,
}

/// Client identification inside `initialize` params (SS1.4.1). Diagnostics and telemetry
/// attribution only; never an authority claim (SS1.10).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientInfo {
    /// Machine identifier, `[a-z0-9_]+`.
    pub name: String,
    /// Human display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Client version string.
    pub version: String,
}

/// The uniform SS3.1.2 command acknowledgement: admission only, never an outcome.
/// `session/compact` alone may answer `"noop"`; every other command answers `"accepted"`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandAcceptedResult {
    /// Echo of the client-minted UUIDv7 `commandId`.
    pub command_id: String,
    /// `"accepted"` for every admitted command.
    pub status: CommandAckStatus,
}

closed_enum! {
    /// The ack-status vocabulary: `"accepted"` for every admitted command; `session/compact` alone
    /// may answer `"noop"`. Closed — a third value would tell a client nothing it can act on.
    CommandAckStatus {
        Accepted = "accepted",
        Noop = "noop",
    }
}

open_enum! {
    /// The SS3.1.2 ack `status`: `accepted` for every admitted command.
    CommandStatus {
        Accepted = "accepted",
    }
}

open_enum! {
    /// `session/compact`'s ack status (tdd SS3.7): the one method that may answer `noop` in
    /// addition to `accepted`. A noop is a success, not an error.
    CompactStatus {
        Accepted = "accepted",
        Noop = "noop",
    }
}

open_enum! {
    /// Compaction outcome (tdd SS4.5.10).
    CompactionOutcome {
        Compacted = "compacted",
        Noop = "noop",
        Failed = "failed",
        Cancelled = "cancelled",
    }
}

open_enum! {
    /// What initiated a compaction (tdd SS4.5.10).
    CompactionTrigger {
        Manual = "manual",
        Auto = "auto",
    }
}

open_enum! {
    /// Context pressure level (tdd SS4.6.6): hard threshold first, both inclusive `>=`.
    ContextPressureLevel {
        Normal = "normal",
        Warning = "warning",
        Blocked = "blocked",
    }
}

/// The SS4.9.1 snapshot `contextUsage` block: the latest `(windowTokens, usedTokens, pressure)`
/// triple; the snapshot member is absent until the fold holds a tracked anchor and the current
/// basis is present (absent is never fabricated).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextUsage {
    /// Server-computed pressure level.
    pub pressure: ContextPressureLevel,
    /// Counted-once occupancy at the latest provider-reported fact.
    pub used_tokens: u64,
    /// The effective context-window size; absent when the basis has no limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_tokens: Option<u64>,
}

/// Session running totals of counted-once usage (tdd SS4.6.5).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CumulativeTokenUsage {
    /// Output tokens, session total.
    pub output_tokens: u64,
    /// Counted-once prompt tokens, session total.
    pub prompt_tokens: u64,
    /// Counted-once totals, session total.
    pub total_tokens: u64,
}

/// The effective approval-mode projection (tdd SS5.12): the same object `session/setApprovalMode`
/// returns as `effectiveMode` and the [`Session`] object carries as `approvalMode`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveApprovalModeState {
    /// The command that last set it; `null` when no command did. Required-nullable.
    #[serde(default)]
    pub last_command_id: Option<String>,
    /// The mode in effect.
    pub mode: ApprovalMode,
    /// How the mode took effect.
    pub source: ApprovalModeSource,
}

/// The latest effective-model fact in the snapshot (tdd SS4.9.1).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveModel {
    /// The selected model.
    pub model_id: String,
    /// The selected provider; `null` when the selection names none. Required-nullable.
    #[serde(default)]
    pub provider_id: Option<String>,
    /// What drove the selection.
    pub source: ModelChangeSource,
}

/// The optional `error.data` object (SS1.6). All members additive-optional; `kind` is always
/// present when `data` is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorData {
    /// The next provable UTF-8 boundary on a misaligned text-media `item/readOutput` read; absent
    /// when no boundary is provable from the stored bytes ahead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aligned_next_offset: Option<u64>,
    /// The rejected anchor value on `notFound`/`missingAnchor` and the `-32042` boundary arms; on
    /// `-32042` it is served as an explicit `null` when the request named no concrete anchor —
    /// optional *and* nullable, so absent and `null` stay distinguishable.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub anchor: Option<Option<String>>,
    /// Approval identity on SS5 errors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<String>,
    /// The stored output's availability on `outputUnavailable` — the durable
    /// `ToolOutputAvailability` value, read-time-derived when the durable record still says
    /// `available`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub availability: Option<String>,
    /// The missing capability, on `capabilityRequired` errors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability: Option<CapabilityName>,
    /// The configured command-admission bound, on `backpressured` errors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity: Option<u64>,
    /// Rejected server-minted choice on `approvalChoiceInvalid`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choice_id: Option<String>,
    /// The rejected command's identifier, on `commandRejected` errors; paired with `reason` so a
    /// client can settle the exact command it sent without parsing the message string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
    /// Current stage token on `approvalRequirementStale`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_requirement_id: Option<ApprovalRequirementRef>,
    /// SS1.5.1 descriptor, on `experimentalRequired` errors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub descriptor: Option<String>,
    /// Free-form additive detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Map<String, Value>>,
    /// The oldest servable view position on `viewTruncated`; served as an explicit `null` when
    /// nothing before the head is servable — the null is present on the wire, never a dropped key.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub earliest_cursor: Option<Option<String>>,
    /// The item whose stored output was requested, on the `item/readOutput` arms (`notFound`,
    /// `outputUnavailable`, and the misaligned-offset `invalidParams`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_id: Option<String>,
    /// The stable camelCase category clients branch on.
    pub kind: ErrorKind,
    /// Rejected inclusive fork boundary on `forkBoundaryInvalid`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_turn_id: Option<String>,
    /// The latest installed boundary's view cursor on the `-32042` arms — always present in the
    /// `-32042` JSON and explicitly `null` when no boundary exists.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub latest_boundary_cursor: Option<Option<String>>,
    /// The exceeded byte limit, on `inputTooLarge` errors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_bytes: Option<u64>,
    /// The requested `outputRef.id`, on the `item/readOutput` arms (`notFound`,
    /// `outputUnavailable`, and the misaligned-offset `invalidParams`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_ref: Option<String>,
    /// Candidate retained paths on `sessionAmbiguous`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paths: Option<Vec<String>>,
    /// Failure detail vocabulary (e.g. `"missingAnchor"` on `notFound`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Winning resolution on `approvalAlreadyResolved`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<ApprovalResolutionSummary>,
    /// Whether retrying can succeed. When present, overrides the error table row's `retryable`
    /// default for this code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retryable: Option<bool>,
    /// Session identity on errors whose lookup is session-scoped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Winning settlement on `userInputAlreadySettled`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settlement: Option<UserInputSettlementSummary>,
    /// User-input prompt identity on SS5.10 errors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_input_id: Option<String>,
    /// The blocked view position on `pageEventTooLarge` errors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view_cursor: Option<String>,
}

open_enum! {
    /// A stable `error.data.kind` category (SS1.6): camelCase, the value clients branch on.
    ErrorKind {
        ParseError = "parseError",
        InvalidRequest = "invalidRequest",
        NotInitialized = "notInitialized",
        AlreadyInitialized = "alreadyInitialized",
        MethodNotFound = "methodNotFound",
        InvalidParams = "invalidParams",
        ExperimentalRequired = "experimentalRequired",
        Internal = "internal",
        PageEventTooLarge = "pageEventTooLarge",
        OutputResultTooLarge = "outputResultTooLarge",
        Overloaded = "overloaded",
        InputTooLarge = "inputTooLarge",
        CapabilityRequired = "capabilityRequired",
        NotFound = "notFound",
        Interrupted = "interrupted",
        Cancelled = "cancelled",
        SessionNotFound = "sessionNotFound",
        SessionInUse = "sessionInUse",
        SessionAmbiguous = "sessionAmbiguous",
        ForkBoundaryInvalid = "forkBoundaryInvalid",
        SessionNotLoaded = "sessionNotLoaded",
        SessionStreamMismatch = "sessionStreamMismatch",
        CommandRejected = "commandRejected",
        Backpressured = "backpressured",
        ViewTruncated = "viewTruncated",
        OutputUnavailable = "outputUnavailable",
        BoundaryPruned = "boundaryPruned",
        BoundaryUnusable = "boundaryUnusable",
        NoBoundary = "noBoundary",
        ApprovalNotFound = "approvalNotFound",
        ApprovalAlreadyResolved = "approvalAlreadyResolved",
        ApprovalChoiceInvalid = "approvalChoiceInvalid",
        ApprovalRequirementStale = "approvalRequirementStale",
        ApprovalReviewerUnavailable = "approvalReviewerUnavailable",
        UserInputNotFound = "userInputNotFound",
        UserInputAlreadySettled = "userInputAlreadySettled",
        UserInputAnswerInvalid = "userInputAnswerInvalid",
    }
}

/// The `error` member of an error response (SS1.2 §2.4).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorObject {
    /// Registry code (Appendix B). A plain integer on the wire.
    pub code: i32,
    /// Structured detail; carries `kind` when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<ErrorData>,
    /// Human-readable message. Never a branch point (SS1.6).
    pub message: String,
}

/// An error response frame (SS1.2 §2.4). Exactly one of `result` or `error` appears on a response;
/// this type is the `error` half.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorResponse {
    /// The error payload.
    pub error: ErrorObject,
    /// Echo of the request id; `null` **only** for an unrecoverable parse error whose offending id
    /// could not be recovered. Always serialized — required-nullable.
    #[serde(default)]
    pub id: Option<RequestId>,
    /// The literal `"2.0"`.
    pub jsonrpc: JsonRpcVersion,
}

/// The session goal block (tdd SS4.6.2). `status` and `percentComplete` are carried **verbatim** —
/// out-of-contract status strings and >100 percents pass through; display clamping is the
/// renderer's job.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Goal {
    /// Current work description, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_work: Option<String>,
    /// Next work description, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_work: Option<String>,
    /// The goal objective text.
    pub objective: String,
    /// Percent complete, verbatim (>100 passes through).
    pub percent_complete: f64,
    /// Goal status, verbatim from the durable goal state (free string by design).
    pub status: String,
}

open_enum! {
    /// What history a lifecycle result actually served (tdd SS2.5.2). Open on the client side:
    /// report-what-was-served means a client treats an unknown mode as "page it yourself".
    HistoryMode {
        AnchoredSnapshot = "anchoredSnapshot",
        Inline = "inline",
        Snapshot = "snapshot",
        None = "none",
    }
}

open_enum! {
    /// Why a result served `history.mode: "none"`. An absent or unknown value decodes
    /// conservatively as an unknown reason, and `viewCursor` text never substitutes for it.
    HistoryNoneReason {
        Excluded = "excluded",
        CursorSuffix = "cursorSuffix",
        HistoryBudget = "historyBudget",
        ProjectionUnavailable = "projectionUnavailable",
        ProjectionReadLimit = "projectionReadLimit",
    }
}

closed_enum! {
    /// The `history` request preference of `session/resume` (tdd SS2.5.2). The forced values
    /// downgrade `anchored` → `inline` → `snapshot` → `none`.
    HistoryPreference {
        Auto = "auto",
        Inline = "inline",
        Snapshot = "snapshot",
        Anchored = "anchored",
    }
}

closed_enum! {
    /// Disposition when a turn is already running (tdd SS3.2). The wire default is `queue`: an SDK
    /// caller who has not looked at session state should not silently mutate an in-flight turn.
    IfBusy {
        Queue = "queue",
        Steer = "steer",
        Replace = "replace",
    }
}

/// `initialize` request params (SS1.4.1).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    /// Capability posture; absent means all defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<ClientCapabilities>,
    /// Client identification.
    pub client_info: ClientInfo,
}

impl InitializeParams {
    /// A default-posture `initialize` for a client with this machine `name` and `version`.
    #[must_use]
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            capabilities: None,
            client_info: ClientInfo {
                name: name.into(),
                title: None,
                version: version.into(),
            },
        }
    }
}

/// The `initialize` result (SS1.4.1).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    /// Echo of the effective negotiated experimental setting.
    pub experimental_api: bool,
    /// Subset of the requested capabilities, plus policy-implicit grants. Fixed for the connection
    /// lifetime.
    pub granted_capabilities: Vec<CapabilityName>,
    /// Absolute path of the muse home directory.
    pub muse_home: String,
    /// Server runtime platform family.
    pub platform_family: PlatformFamily,
    /// Server operating system.
    pub platform_os: PlatformOs,
    /// Envelope schema version and stable-surface fingerprint.
    pub schema: SchemaInfo,
    /// Server identification.
    pub server_info: ServerInfo,
    /// Whether this host persists its sessions. Optional so the addition is additive; a v1 host
    /// always sends it, and a client reads absent as `durable`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_durability: Option<SessionDurability>,
    /// Exact user-agent string presented to upstream model providers.
    pub user_agent: String,
}

closed_enum! {
    /// The literal `"2.0"` every frame carries (SS1.2).
    JsonRpcVersion {
        V2 = "2.0",
    }
}

// only value the enum has.
#[allow(clippy::derivable_impls)]
impl Default for JsonRpcVersion {
    fn default() -> Self {
        Self::V2
    }
}

/// A notification frame, either direction (SS1.2 §2.2). No `id`; a notification is never answered.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Notification {
    /// Emission time in Unix milliseconds, recorded once at emission; server→client notifications
    /// only. Optional on the wire so decoders tolerate older servers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emitted_at_ms: Option<u64>,
    /// The literal `"2.0"`.
    pub jsonrpc: JsonRpcVersion,
    /// Notification method name.
    pub method: String,
    /// Method-specific parameters; omitted entirely when empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Map<String, Value>>,
}

/// Stored-output reference (tdd SS4.5.5). The fetch path is `item/readOutput`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputRef {
    /// Whether the bytes are servable.
    pub availability: OutputRefAvailability,
    /// Stored byte length.
    pub byte_len: u64,
    /// Content digest (e.g. `"sha256:..."`), when recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    /// The stored output's durable id.
    pub id: String,
    /// The stored output's kind, verbatim durable vocabulary (e.g. `"tool_output"`; snake_case per
    /// the SS1.6 casing exemption).
    pub kind: String,
    /// Media type, when recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    /// Stored path, when recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The stored output's URI.
    pub uri: String,
}

open_enum! {
    /// Whether a stored output's bytes are servable (tdd SS4.5.5).
    OutputRefAvailability {
        Available = "available",
        Missing = "missing",
        Unsupported = "unsupported",
        AccessFailed = "accessFailed",
    }
}

/// A pending approval in the snapshot: the SS2.5.2 pointer shape plus the gated `itemId` when one
/// exists (tdd SS4.9.1).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingApprovalPointer {
    /// The pending approval's id.
    pub approval_id: String,
    /// The parked item, when one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_id: Option<String>,
    /// The cursor the approval opened at.
    pub view_cursor: String,
}

open_enum! {
    /// The `pendingRequests` discriminator (tdd SS2.5.2), v1 values `approval` and `userInput`.
    PendingRequestKind {
        Approval = "approval",
        UserInput = "userInput",
    }
}

/// A late-joiner pointer at an unsettled server-initiated request (tdd SS2.5.2, SS2.2.1). The full
/// payloads arrive as re-issued server→client requests right after a `session/resume` response;
/// `session/read` never re-issues them. One flat object with both ids optional — the published
/// schema names no object-variant union, so `{"kind":"approval","userInputId":…}` is accepted.
///
/// Note the shape the binary actually serves in `pendingRequests` is usually
/// not this but the full payload — see [`PendingRequestEntry`].
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingRequestPointer {
    /// Present on an `approval` entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<String>,
    /// The discriminator; v1 values `approval` and `userInput`.
    pub kind: PendingRequestKind,
    /// Present on a `userInput` entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_input_id: Option<String>,
    /// The cursor the request opened at.
    pub view_cursor: String,
}

/// One entry of the `pendingRequests` array on `session/resume`, `session/fork`
/// and `session/read` (tdd SS2.5.2).
///
/// The published schema names only the `{kind, approvalId?, userInputId?,
/// viewCursor}` pointer — but the muse 1.2.1 binary serves the **full**
/// server-initiated request payloads there instead: an approval entry is the
/// whole [`ApprovalRequestParams`], a user-input entry the whole
/// [`UserInputRequestParams`], and neither carries a top-level `kind`
/// (observed 2026-09-13: `session/read` for a title failed with
/// `missing field 'kind'`). The capture wins over the schema
/// (`docs/01-transport.md` §4), so this is an untagged union that accepts
/// both: full payloads first, the documented pointer last. Anything else is
/// a hard error, not a silent drop — an undecodable pending set must not
/// lose an approval.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PendingRequestEntry {
    /// A pending approval, served inline with its full payload.
    Approval(Box<ApprovalRequestParams>),
    /// A pending user-input prompt, served inline with its full payload.
    UserInput(Box<UserInputRequestParams>),
    /// The documented pointer shape (`{kind, …, viewCursor}`).
    Pointer(PendingRequestPointer),
}

/// A pending user-input prompt in the snapshot (tdd SS4.9.1, SS5.10).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingUserInputPointer {
    /// The parked item, when one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_id: Option<String>,
    /// The pending prompt's id.
    pub user_input_id: String,
    /// The cursor the prompt opened at.
    pub view_cursor: String,
}

closed_enum! {
    /// The server runtime's platform family (SS1.4.1); may differ from the client's. Closed: SS1.4.1
    /// fixes the value set, so widening it is a deliberate, gate-visible protocol change.
    PlatformFamily {
        Unix = "unix",
        Windows = "windows",
    }
}

closed_enum! {
    /// The server's operating system (SS1.4.1). Closed, for the same reason as [`PlatformFamily`].
    PlatformOs {
        Macos = "macos",
        Linux = "linux",
        Windows = "windows",
    }
}

/// One end of a [`SourceRange`]: a record's event id and its sequence number within the stream
/// (tdd SS4.2).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordPosition {
    /// The record's event id.
    pub id: String,
    /// The record's sequence number within its stream.
    pub sequence: u64,
}

/// A request frame, either direction (SS1.2 §2.1).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    /// Client-chosen id, unique among that client's in-flight requests.
    pub id: RequestId,
    /// The literal `"2.0"`.
    pub jsonrpc: JsonRpcVersion,
    /// Slash-namespaced camelCase method name.
    pub method: String,
    /// Method-specific parameters; omitted entirely when empty, never `null`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Map<String, Value>>,
    /// W3C trace passthrough (SS1.8); requests only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace: Option<TraceContext>,
}

/// A request id (SS1.3): client-chosen string or integer. `1` and `"1"` do not compare equal; each
/// direction owns its own id space.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    /// An integer id.
    Number(i64),
    /// A string id.
    String(String),
}

/// The `schema` pair in [`InitializeResult`] (SS1.4.1, SS1.5.3). `version` is the wire **envelope**
/// schema version — distinct from the session-view `schemaVersion` and the raw-log
/// `schema_version`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaInfo {
    /// `sha256:<64 lower hex>` content hash of this server binary's stable-surface bundle. A
    /// mismatch against [`SCHEMA_FINGERPRINT`] is a **warning** condition, never an error.
    pub fingerprint: String,
    /// Envelope schema version; `1` in v1, expected to stay `1` for a long time.
    pub version: u32,
}

/// Server identification inside [`InitializeResult`].
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    /// Server name.
    pub name: String,
    /// Server version string.
    pub version: String,
}

/// The compaction boundary an anchored snapshot is anchored at (tdd SS2.5.2).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotAnchor {
    /// The view cursor of the boundary's `compaction` event.
    pub boundary_cursor: String,
    /// The boundary's opaque compaction anchor.
    pub summarized_through: String,
}

/// The complete folded view at the snapshot cursor (tdd SS4.9.1). Additive-optional evolution
/// applies; unknown members MUST be ignored.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotState {
    /// The running foreground turn; `null` when none is running. Required-nullable.
    #[serde(default)]
    pub active_turn: Option<TurnRef>,
    /// The effective approval mode at this cursor — the same object the `Session` carries.
    pub approval_mode: EffectiveApprovalModeState,
    /// Latest branch observation; `null` when no fact has landed. Required-nullable.
    #[serde(default)]
    pub branch: Option<BranchState>,
    /// The latest `(windowTokens, usedTokens, pressure)` triple. SS4.9.2's worked example omits the
    /// member and the binary omits it too — this is the absent arm, while the siblings are
    /// present-null in the same frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_usage: Option<ContextUsage>,
    /// Latest effective model; `null` when no fact has landed. Required-nullable.
    #[serde(default)]
    pub effective_model: Option<EffectiveModel>,
    /// Latest goal block; `null` when no fact has landed. Required-nullable.
    #[serde(default)]
    pub goal: Option<Goal>,
    /// Every item, in first-opened order, each at its latest revision at the snapshot cursor — the
    /// same item schema the notifications carry, which is what makes snapshot+suffix a pure splice.
    pub items: Vec<Item>,
    /// The settled canonical session name (tdd SS4.9.1 / SS2.14). **Additive-optional**, absent
    /// until the session is named. The schema's own doc says "`null` when the session was never
    /// named" while typing the member as a bare `string`, so the two arms are indistinguishable
    /// here: both read as `None` and both write back as absent. No capture has served either arm
    /// yet; revisit when one does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The pending-approval half of the pending set.
    pub pending_approvals: Vec<PendingApprovalPointer>,
    /// The pending-user-input half of the pending set.
    pub pending_user_inputs: Vec<PendingUserInputPointer>,
    /// Admitted-but-not-launched submits, in launch order.
    pub queued_turns: Vec<TurnRef>,
    /// The latest session-default reasoning effort with its source (tdd SS4.9.1, ADR 31255 D1).
    /// **Additive-optional, absent arm**: missing until a set lands, never present-`null`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffortState>,
    /// Latest todo list; `null` when no fact has landed. Required-nullable.
    #[serde(default)]
    pub todo_list: Option<TodoListState>,
    /// The `cumulative` block of the last `session/tokenUsage` event; zeroes when none.
    pub token_usage: CumulativeTokenUsage,
    /// Completed-turn count at this cursor.
    pub turn_count: u64,
}

/// The inclusive raw-record range a durable-sourced view event folded from (tdd SS4.2) — the
/// reconciliation token. In v1 it is an **opaque provenance token**: no wire method reads what it
/// points at. Ephemeral-sourced events (`item/delta`, and an `item/started` that opens on an
/// ephemeral record) carry no `sourceRange`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRange {
    /// The first record of the inclusive range.
    pub first: RecordPosition,
    /// The last record of the inclusive range; equal to `first` for an event folded from one
    /// record.
    pub last: RecordPosition,
    /// The stream the folded records live on.
    pub stream: StreamRef,
}

/// One raw stream named by a [`SourceRange`] (tdd SS4.2).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamRef {
    /// The stream id.
    pub id: String,
    /// The stream kind, verbatim raw-log vocabulary (e.g. `"run"`, `"session"`). A free string, not
    /// an enum: `sourceRange` is an opaque provenance token in v1.
    pub kind: String,
}

open_enum! {
    /// Subagent control status (tdd SS4.5.7). The generic item `status` is the terminal authority.
    SubagentControlStatus {
        Accepted = "accepted",
        Starting = "starting",
        Running = "running",
        ResultReady = "resultReady",
        Closing = "closing",
        Closed = "closed",
        RecoveryPending = "recoveryPending",
        ManualReconciliation = "manualReconciliation",
    }
}

/// Params for `subagent/sendMessage` and `subagent/followupTask` (SS3.16): a child target plus the
/// input body.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentInputParams {
    /// The note/task text: trimmed with Unicode-whitespace semantics, rejected when empty after
    /// trimming, delivered trimmed with multibyte content byte-for-byte intact.
    pub body: String,
    /// Client-minted UUIDv7 command id.
    pub command_id: String,
    /// Target session.
    pub session_id: String,
    /// Durable child id (the SS4.5.7 item's `subagentId`; opaque string).
    pub subagent_id: String,
}

/// Params for `subagent/interrupt`, `subagent/stop`, and `subagent/close` (SS3.16): a child target
/// plus an optional human-readable reason preserved on the durable effect record.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentOwnerReasonParams {
    /// Client-minted UUIDv7 command id.
    pub command_id: String,
    /// Optional reason, preserved on the durable owner-command record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Target session.
    pub session_id: String,
    /// Durable child id.
    pub subagent_id: String,
}

/// A subagent's result envelope (tdd SS4.5.7).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentResult {
    /// Artifact references, verbatim.
    pub artifact_refs: Vec<String>,
    /// Error kind, verbatim durable vocabulary, when the child failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<String>,
    /// Evidence references, verbatim.
    pub evidence_refs: Vec<String>,
    /// Structured result data, verbatim, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_data: Option<Map<String, Value>>,
    /// Bounded result summary (<=512 chars, runtime-enforced).
    pub summary: String,
    /// Result text (<=32 KiB), when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// Params for `subagent/resume`, `subagent/reopen`, and `subagent/readResult` (SS3.16): the bare
/// child target.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentTargetParams {
    /// Client-minted UUIDv7 command id.
    pub command_id: String,
    /// Target session.
    pub session_id: String,
    /// Durable child id.
    pub subagent_id: String,
}

/// A success response frame (SS1.2 §2.3).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuccessResponse {
    /// Echo of the request id.
    pub id: RequestId,
    /// The literal `"2.0"`.
    pub jsonrpc: JsonRpcVersion,
    /// Always a JSON object, possibly `{}`, never a bare scalar — so every result can grow
    /// additive-optional members.
    pub result: Map<String, Value>,
}

open_enum! {
    /// Version-control system of a branch observation (tdd SS4.6.4).
    Vcs {
        Git = "git",
        Sapling = "sapling",
    }
}

/// The `sha256:` stable-surface fingerprint of the muse 1.2.1 schema bundle these types were
/// generated from (`fixtures/msp/msp/manifest.json`).
///
/// Compare it against `InitializeResult.schema.fingerprint`. **A mismatch is a WARNING, never an
/// error**: the server is free to ship a different stable surface, and additive evolution keeps
/// these types working (research §1.2). Log it, do not refuse the connection.
pub const SCHEMA_FINGERPRINT: &str =
    "sha256:c7ff6c5d1e89cd42f803aea1f05b8e72082f2099685802473eb726903484713b";

/// Every wire **method** in the SS1.9 published index (`MspMethod`).
///
/// The server→client requests (`approval/request`, `userInput/request`) are **not** methods: they
/// live in the schema's own `requests` index, mirrored here as [`MSP_SERVER_REQUESTS`].
pub const MSP_METHODS: &[&str] = &[
    "initialize",
    "subagent/sendMessage",
    "subagent/followupTask",
    "subagent/interrupt",
    "subagent/stop",
    "subagent/resume",
    "subagent/reopen",
    "subagent/close",
    "subagent/readResult",
    "session/start",
    "session/resume",
    "session/fork",
    "session/list",
    "session/read",
    "turn/start",
    "turn/steer",
    "turn/interrupt",
    "turn/cancel",
    "turn/unqueue",
    "session/compact",
    "session/setModel",
    "session/rename",
    "session/setReasoningEffort",
    "session/userShell",
    "model/list",
    "view/subscribe",
    "view/unsubscribe",
    "view/page",
    "item/readOutput",
    "approval/decide",
    "approval/listPending",
    "session/setApprovalMode",
    "userInput/answer",
    "userInput/cancel",
    "userInput/clarify",
];

/// Every wire **notification** in the SS1.9 published index (`MspNotification`).
///
/// The binary additionally emits `session/started` ([`SessionStartedParams`]), which this index
/// omits — a client must accept it.
pub const MSP_NOTIFICATIONS: &[&str] = &[
    "initialized",
    "turn/started",
    "turn/completed",
    "turn/retracted",
    "turn/retryScheduled",
    "turn/unqueued",
    "item/started",
    "item/updated",
    "item/delta",
    "item/completed",
    "view/gap",
    "approval/requested",
    "approval/updated",
    "approval/resolved",
    "userInput/requested",
    "userInput/settled",
    "session/modelChanged",
    "session/nameChanged",
    "session/reasoningEffortChanged",
    "session/goalChanged",
    "session/todoListChanged",
    "session/branchChanged",
    "session/tokenUsage",
    "session/contextUsage",
    "session/approvalModeChanged",
    "session/modelRouteUnserved",
];

/// Every server-initiated wire request in the schema's `requests` index
/// (`MspServerRequest`, SS5.3/SS5.10.1).
///
/// These are server→client and therefore absent from [`MSP_METHODS`]: the client never sends them,
/// it answers each with a [`RequestReceipt`] while the decision/answer travels as a command
/// (`approval/decide`, `userInput/answer`).
pub const MSP_SERVER_REQUESTS: &[&str] = &["approval/request", "userInput/request"];

open_enum! {
    /// Every server-initiated wire request in the schema's `requests` index
    /// (`MspServerRequest`, SS5.3/SS5.10.1).
    MspServerRequest {
        ApprovalRequest = "approval/request",
        UserInputRequest = "userInput/request",
    }
}

/// The SS5.3.3 presentation receipt — the client's response to a server-initiated request
/// (`approval/request`, `userInput/request`).
///
/// The response acknowledges presentation only ("a surface showed or will show this"): it changes
/// no state, the server uses it for diagnostics alone, and the decision/answer travels as a
/// command (`approval/decide`, `userInput/answer`). An error response or a dropped connection means
/// this connection could not present the request; the approval or prompt stays pending and the
/// request is re-issued on the next subscribe. There is no dismiss-without-deciding on the wire.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestReceipt {}

/// Every `error.data.kind` in the SS1.6 error table (`MspErrorDataKind`) — each code's primary kind
/// plus its override kinds. Rendered as strings rather than an enum because [`ErrorKind`] already
/// carries the same domain as a type; this list is the index, not a second type.
pub const MSP_ERROR_DATA_KINDS: &[&str] = &[
    "parseError",
    "invalidRequest",
    "notInitialized",
    "alreadyInitialized",
    "methodNotFound",
    "experimentalRequired",
    "invalidParams",
    "internal",
    "pageEventTooLarge",
    "outputResultTooLarge",
    "overloaded",
    "inputTooLarge",
    "capabilityRequired",
    "notFound",
    "interrupted",
    "cancelled",
    "sessionNotFound",
    "sessionInUse",
    "sessionAmbiguous",
    "forkBoundaryInvalid",
    "sessionNotLoaded",
    "sessionStreamMismatch",
    "commandRejected",
    "backpressured",
    "viewTruncated",
    "outputUnavailable",
    "boundaryPruned",
    "boundaryUnusable",
    "noBoundary",
    "approvalNotFound",
    "approvalAlreadyResolved",
    "approvalChoiceInvalid",
    "approvalRequirementStale",
    "approvalReviewerUnavailable",
    "userInputNotFound",
    "userInputAlreadySettled",
    "userInputAnswerInvalid",
];
