//! The `session/*` lane: a session's identity and metadata, the list, the
//! start/resume/read/fork calls, and every `session/*` notification.
//!
//! Part of [`crate::schema`]; see that module for the conventions every
//! type here follows.

use serde::{Deserialize, Serialize};

use super::*;

/// Where a fork cuts the source history (tdd SS2.5.3). Turn ids are used instead of counts because
/// ids stay stable across compaction and concurrent appends while indices do not.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkCutPoint {
    /// The last completed turn to copy, inclusive. Naming an in-progress or unknown turn fails
    /// `forkBoundaryInvalid`.
    pub last_turn_id: String,
}

/// Fork provenance folded from the durable `session.fork.created` record (tdd SS2.4).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkProvenance {
    /// The `commandId` of the `session/fork` that created it.
    pub command_id: String,
    /// An opaque provenance string preserved verbatim. Display-only — clients MUST NOT parse it and
    /// no method accepts it.
    pub cut_cursor: String,
    /// Whether the fork named an explicit cut point.
    pub cut_explicit: bool,
    /// The source session this fork was cut from.
    pub session_id: String,
}

/// The session object every lifecycle method returns or lists (tdd SS2.4). Additive-optional
/// evolution applies: clients must ignore unknown fields.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    /// The `turnId` of the current foreground turn when `status` is `running`, and `null` when the
    /// session is idle. **Required-nullable**: both production writers hard-code
    /// `"activeTurnId": null` and nothing strips it.
    #[serde(default)]
    pub active_turn_id: Option<String>,
    /// The folded effective approval mode. **Additive-optional**: a `Session` that omits it means
    /// the host has not folded a mode, which is why the index-derived `session/list` entry may
    /// legitimately omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_mode: Option<EffectiveApprovalModeState>,
    /// RFC3339. For a fork this is the fork session id's UUIDv7 mint instant.
    pub created_at: String,
    /// `null` for root sessions; fork provenance otherwise. Required-nullable.
    #[serde(default)]
    pub forked_from: Option<ForkProvenance>,
    /// The winning metadata fold's model; `null` when that record omits it. Required-nullable.
    #[serde(default)]
    pub model_id: Option<String>,
    /// Absolute path of the session's durable log; non-nullable. Under the ephemeral session
    /// profile it is the **empty string**, meaning "no durable log exists" — the one value a client
    /// must not hand to a filesystem call.
    pub path: String,
    /// The winning metadata fold's provider; `null` when that record omits it. Required-nullable.
    #[serde(default)]
    pub provider_id: Option<String>,
    /// The session identity.
    pub session_id: String,
    /// Load state as this host knows it; `session/list` reports `notLoaded` for sessions loaded by
    /// *other* hosts.
    pub status: SessionStatus,
    /// Completed-turn count from the session view fold.
    pub turn_count: u64,
    /// RFC3339; never precedes `createdAt`.
    pub updated_at: String,
    /// The winning metadata fold's workspace root; `null` when that record omits it — absent is
    /// never fabricated. Required-nullable.
    #[serde(default)]
    pub workspace_root: Option<String>,
}

/// `session/approvalModeChanged` params (tdd SS5.12): the fold of the durable approval-mode
/// reconfigure audit fact — every accepted change writes one.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionApprovalModeChangedParams {
    /// The requesting client's `clientInfo.name`, recorded on the audit fact.
    pub client_name: String,
    /// The command that changed it.
    pub command_id: String,
    /// The mode now in effect.
    pub mode: ApprovalMode,
    /// The owning session.
    pub session_id: String,
    /// How the mode took effect.
    pub source: ApprovalModeSource,
    /// The durable records this event folded from.
    pub source_range: SourceRange,
    /// Opaque, strictly monotonic view cursor.
    pub view_cursor: String,
}

/// `session/branchChanged` params (tdd SS4.6.4): a durable workspace-branch observation landed.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionBranchChangedParams {
    /// The observed branch; `null` on a detached-HEAD observation — a fact, not "unchanged".
    ///
    /// **Required-nullable**, against msp.d.ts's `branch?: string`: the key is present in every
    /// capture and the sibling [`BranchState::branch`] is declared `string | null`.
    #[serde(default)]
    pub branch: Option<String>,
    /// The owning session.
    pub session_id: String,
    /// The durable records this event folded from.
    pub source_range: SourceRange,
    /// The detected version-control system; absent when no repository was detected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vcs: Option<Vcs>,
    /// Opaque, strictly monotonic view cursor.
    pub view_cursor: String,
    /// The observed workspace root.
    pub workspace_root: String,
}

/// `session/compact` params (tdd SS3.7): manually compact the session's conversation context — the
/// `/compact` gesture. Compaction runs asynchronously; the ack is admission only.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCompactParams {
    /// The SS3.1.1 idempotency handle (UUIDv7).
    pub command_id: String,
    /// The target session.
    pub session_id: String,
    /// The run whose context to compact. Omit and the server resolves the session's current/latest
    /// run; a session with no resolvable run rejects with `commandRejected` reason `missing_run`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
}

/// `session/compact` result (tdd SS3.7). Failures *after* admission (`summarizer_failed`,
/// `install_rejected`, `cancelled`) are not wire errors — they arrive as the compaction's terminal
/// view event.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCompactResult {
    /// Echoes the client's id.
    pub command_id: String,
    /// Why the command was a noop, e.g. `no_compactable_history`. Present on a `noop` status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Admission status, or `noop`.
    pub status: CompactStatus,
}

/// `session/start`'s reserved `config` object (tdd SS2.5.1): **no members in v1**. It exists so a
/// future Configuration section can add per-session overrides additively.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionConfig {}

/// `session/contextUsage` params (tdd SS4.6.6): context-window pressure — the counted-once
/// occupancy at the latest provider-reported durable fact, joined with the host's pressure basis.
/// Replace wholesale; emitted only when the triple changes value.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionContextUsageParams {
    /// Server-computed pressure level over the basis thresholds.
    pub pressure: ContextPressureLevel,
    /// The owning session.
    pub session_id: String,
    /// The durable records this event folded from.
    pub source_range: SourceRange,
    /// Counted-once prompt tokens plus output tokens of the driving record, saturating — the honest
    /// occupancy floor.
    pub used_tokens: u64,
    /// Opaque, strictly monotonic view cursor.
    pub view_cursor: String,
    /// The effective context-window size from the host's pressure basis; absent when the basis has
    /// no limit — the limit part is omitted, never invented.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_tokens: Option<u64>,
}

open_enum! {
    /// Whether this host writes its sessions to disk (SS1.4.1, SS2.13). A property of the host
    /// process, fixed at construction — never requested, granted, or negotiated.
    SessionDurability {
        Durable = "durable",
        Ephemeral = "ephemeral",
    }
}

/// `session/fork` params (tdd SS2.5.3).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionForkParams {
    /// The SS2.5 idempotency handle (UUIDv7).
    pub command_id: String,
    /// Copy history through a completed turn, inclusive. Omitted means "all completed turns".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cut_point: Option<ForkCutPoint>,
    /// Skip inline history in the result and page it later — same as `session/resume`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude_items: Option<bool>,
    /// The source session.
    pub session_id: String,
}

/// `session/fork` result (tdd SS2.5.3): the `session/resume` envelope for the **new** session,
/// whose `session.forkedFrom` carries the provenance.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionForkResult {
    /// The served history.
    pub history: SessionHistory,
    /// The late-joiner pointer set.
    pub pending_requests: Vec<PendingRequestPointer>,
    /// The new fork session, carrying `forkedFrom` provenance.
    pub session: Session,
    /// The new session's view head.
    pub view_cursor: String,
}

/// `session/goalChanged` params (tdd SS4.6.2): the projected goal block's value changed — identical
/// adoptions emit nothing, and an explicit `null` clears. Replace wholesale; `null` never means
/// unchanged.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionGoalChangedParams {
    /// The full current goal block, or `null` to clear.
    ///
    /// **Required-nullable**, against msp.d.ts's `goal?: Goal`: the event's whole contract is that
    /// `null` is the clearing value, so the key is carried rather than dropped.
    #[serde(default)]
    pub goal: Option<Goal>,
    /// The owning session.
    pub session_id: String,
    /// The durable records this event folded from.
    pub source_range: SourceRange,
    /// Opaque, strictly monotonic view cursor.
    pub view_cursor: String,
}

/// The `history` envelope shared by `session/resume`, `session/fork`, and `session/read`
/// (tdd SS2.5.2).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionHistory {
    /// The full folded item array when `mode` is `inline`; `null` otherwise. Required-nullable.
    #[serde(default)]
    pub items: Option<Vec<Item>>,
    /// What was actually served — never what was asked for.
    pub mode: HistoryMode,
    /// Why no history was served — sent exactly when `mode` is `"none"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub none_reason: Option<HistoryNoneReason>,
    /// The folded view state when `mode` is `snapshot` or `anchoredSnapshot`; `null` otherwise.
    /// Required-nullable.
    #[serde(default)]
    pub snapshot: Option<ViewSnapshot>,
}

/// `session/list` params (tdd SS2.5.4). Read-only; never touches leases.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionListParams {
    /// Opaque page cursor from a prior result. Page cursors are a distinct opaque-string family
    /// from view cursors, valid only for re-issuing the same listing — never parse one. Omitted and
    /// explicit `null` both mean "first page".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// Page size; default 50, maximum 200.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Only sessions with log activity after this RFC3339 instant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_after: Option<String>,
    /// Only sessions whose metadata workspace root equals this path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_root: Option<String>,
}

/// `session/list` result (tdd SS2.5.4). Ordering is `updatedAt` descending.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionListResult {
    /// The next page's cursor; `null` on the last page. Required-nullable.
    #[serde(default)]
    pub next_cursor: Option<String>,
    /// The page of stored sessions.
    pub sessions: Vec<Session>,
}

/// `session/modelChanged` params (tdd SS4.6.1): a durable model-selection record landed.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionModelChangedParams {
    /// The selected model.
    pub model_id: String,
    /// The selected provider; `null` when the selection names none.
    ///
    /// **Required-nullable**, against msp.d.ts's `providerId?: string`, on its own doc comment and
    /// the sibling [`EffectiveModel::provider_id`].
    #[serde(default)]
    pub provider_id: Option<String>,
    /// The owning session.
    pub session_id: String,
    /// What drove the selection.
    pub source: ModelChangeSource,
    /// The durable records this event folded from.
    pub source_range: SourceRange,
    /// Opaque, strictly monotonic view cursor.
    pub view_cursor: String,
}

/// `session/modelRouteUnserved` params: an accepted `login.credential_update` installed a provider
/// that cannot serve the session's STANDING model route. Disclosure only — the standing selection
/// is unchanged (no `session/modelChanged` fires) and the repair path is a routable
/// `session/setModel`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionModelRouteUnservedParams {
    /// The command whose accepted swap installed the provider.
    pub command_id: String,
    /// The provider id the swap installed.
    pub installed_provider_id: String,
    /// The standing (still bound, now unroutable) model.
    pub model_id: String,
    /// The standing model's provider; `null` when the selection names none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    /// The owning session.
    pub session_id: String,
    /// The durable records this event folded from.
    pub source_range: SourceRange,
    /// Opaque, strictly monotonic view cursor.
    pub view_cursor: String,
}

/// `session/read` params (tdd SS2.5.5): read one stored session **without attaching** — no writer
/// lease, no load, no subscription, no `SessionResumed` record.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionReadParams {
    /// Metadata-only unless you ask: `false` carries the folded item history. Same name and
    /// polarity as `session/resume`/`session/fork`; only the default differs — here it is `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude_items: Option<bool>,
    /// The session to read.
    pub session_id: String,
}

/// `session/read` result (tdd SS2.5.5). The `viewCursor` is the fold head at read time — a
/// point-in-time read, immediately stale if a foreign host is appending.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionReadResult {
    /// The served history.
    pub history: SessionHistory,
    /// The same pointer shape as `session/resume`. Because `session/read` never subscribes this is
    /// a point-in-time log read only: no requests are re-issued after it.
    pub pending_requests: Vec<PendingRequestPointer>,
    /// The session as folded point-in-time.
    pub session: Session,
    /// The fold head at read time.
    pub view_cursor: String,
}

/// `session/resume` params (tdd SS2.5.2).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionResumeParams {
    /// The SS2.5 idempotency handle (UUIDv7). A resume that loads a session writes a durable
    /// `SessionResumed` record.
    pub command_id: String,
    /// A view cursor previously observed by this client — or an observed `summarizedThrough`
    /// compaction anchor. When present the server returns only the suffix and `history.mode` is
    /// `none`. Omitted and explicit `null` both mean "no cursor".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// Return only session metadata and live resume state; page history separately with
    /// `view/page`. Default `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude_items: Option<bool>,
    /// History mode preference, default `auto`. A PREFERENCE, not a budget override: when the
    /// requested mode does not fit the history budget the server downgrades it and `history.mode`
    /// reports what was actually served. Ignored when `cursor` or `excludeItems` makes it moot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history: Option<HistoryPreference>,
    /// The session to load.
    pub session_id: String,
}

/// `session/resume` result (tdd SS2.5.2).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionResumeResult {
    /// The served history.
    pub history: SessionHistory,
    /// The late-joiner pointer set; empty when nothing is pending.
    pub pending_requests: Vec<PendingRequestPointer>,
    /// The loaded session.
    pub session: Session,
    /// The session view head; the connection is subscribed after it.
    pub view_cursor: String,
}

/// `session/setApprovalMode` params (tdd SS5.12): switch the session's approval enforcement mode
/// mid-session. **Select, never create**: a client may only *select* a mode the host's
/// configuration already defines, which is why [`ApprovalMode`] is closed.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSetApprovalModeParams {
    /// The SS3.1.1 idempotency handle (UUIDv7).
    pub command_id: String,
    /// The preconfigured mode to select. The requesting actor is recorded from the connection's
    /// `clientInfo.name`.
    pub mode: ApprovalMode,
    /// The target session.
    pub session_id: String,
}

/// `session/setApprovalMode` result (tdd SS5.12). Applies next-action — an in-flight tool action's
/// pending approval is not retroactively decided by a mode change.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSetApprovalModeResult {
    /// Whether the change did anything.
    pub apply_outcome: ApprovalModeApplyOutcome,
    /// Echoes the client's id.
    pub command_id: String,
    /// The folded effective-mode projection — the same object the `Session` carries as
    /// `approvalMode`.
    pub effective_mode: EffectiveApprovalModeState,
    /// Admission status.
    pub status: CommandStatus,
}

/// `session/setModel` params (tdd SS3.8): the model-picker gesture. The selection is durable and
/// applies to subsequent model calls.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSetModelParams {
    /// The SS3.1.1 idempotency handle (UUIDv7).
    pub command_id: String,
    /// The selection.
    pub model: ModelSelection,
    /// The target session.
    pub session_id: String,
}

/// `session/setModel` result (tdd SS3.8). If a turn is running the selection is admitted now and
/// applied at the next model-call boundary; the ack does not wait for that boundary.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSetModelResult {
    /// Echoes the client's id.
    pub command_id: String,
    /// Admission status.
    pub status: CommandStatus,
}

/// `session/start` params (tdd SS2.5.1).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStartParams {
    /// The session's starting approval mode; server default when omitted or explicit `null`.
    /// Select, never create. This is the only surface that declares a non-interactive run's policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_mode: Option<ApprovalMode>,
    /// The SS2.5 idempotency handle (UUIDv7). Required; the server never mints one.
    pub command_id: String,
    /// Reserved for per-session overrides owned by a future Configuration section, which is why
    /// [`SessionConfig`] declares no members.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<SessionConfig>,
    /// Initial model; server default when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    /// Initial provider routing; server default when omitted or explicit `null`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    /// Exact identity for this new root session. When omitted the server mints a UUIDv7. This field
    /// never selects an existing session: a retained or reserved id is rejected `commandRejected`
    /// with reason `session_id_conflict`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Absolute path, folded into the first metadata record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_root: Option<String>,
}

/// `session/start` result (tdd SS2.5.1).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStartResult {
    /// The new session.
    pub session: Session,
    /// The session view head after the start fold; the connection is subscribed and receives every
    /// view event after this cursor. A deduplicated retry returns this same result.
    pub view_cursor: String,
}

/// `session/started` params — **not in the SS1.9 published index**, but emitted by the muse 1.1.1
/// binary right after a `session/start` result. The payload is the same [`Session`] object.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStartedParams {
    /// The session that just started.
    pub session: Session,
}

open_enum! {
    /// A session's load state (tdd SS2.4).
    SessionStatus {
        NotLoaded = "notLoaded",
        Idle = "idle",
        Running = "running",
    }
}

/// `session/todoListChanged` params (tdd SS4.6.3): a `TodoSnapshotUpdated` record landed. Replace
/// the whole list on every event; an empty `items` array is a cleared list, not a no-op.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTodoListChangedParams {
    /// The full todo list, replaced wholesale.
    pub items: Vec<TodoItem>,
    /// The snapshot revision. Diagnostics only, never an ordering guard — `viewCursor` orders.
    pub revision: u32,
    /// The owning session.
    pub session_id: String,
    /// The durable records this event folded from.
    pub source_range: SourceRange,
    /// The tool that produced the snapshot, verbatim.
    pub source_tool: String,
    /// Opaque, strictly monotonic view cursor.
    pub view_cursor: String,
}

/// `session/tokenUsage` params (tdd SS4.6.5): one per model completion that reports usage. Carries
/// the raw counters verbatim plus the server-derived counted-once `promptTokens`/`totalTokens` and
/// the session `cumulative` block. Accumulate-only: `cumulative` never goes backward.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTokenUsageParams {
    /// Session running totals at this cursor; the fold owns the accumulation. Subagent/workflow
    /// child usage is never folded in — it rides the owning items.
    pub cumulative: CumulativeTokenUsage,
    /// Model-call wall time, when measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Provider finish reason when reported (open vocabulary, verbatim).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    /// The effective model that produced this usage; `null` on pre-schema records — never
    /// back-filled, an unpriced leg.
    ///
    /// **Required-nullable**, against msp.d.ts's `modelId?: string`: the key is present in every
    /// capture and is explicitly `null` on unpriced legs.
    #[serde(default)]
    pub model_id: Option<String>,
    /// Prompt tokens counted exactly once under the provider's cache convention. Server-derived and
    /// deterministic — never re-derive the provider's cache convention client-side.
    pub prompt_tokens: u64,
    /// The owning session.
    pub session_id: String,
    /// The durable records this event folded from.
    pub source_range: SourceRange,
    /// `promptTokens + outputTokens` — the honest per-completion total.
    pub total_tokens: u64,
    /// The turn whose model call completed.
    pub turn_id: String,
    /// Raw counters verbatim from the durable record.
    pub usage: TokenUsage,
    /// Opaque, strictly monotonic view cursor.
    pub view_cursor: String,
}

/// `session/userShell` params (tdd SS3.9): run a user-initiated shell command in the session's
/// workspace — the TUI's `!` escape hatch. **Capability-gated**: the connection must negotiate
/// `userShell` at `initialize`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionUserShellParams {
    /// The SS3.1.1 idempotency handle (UUIDv7).
    pub command_id: String,
    /// The shell command to run.
    pub command_text: String,
    /// The target session.
    pub session_id: String,
}

/// `session/userShell` result (tdd SS3.9): immediate. The shell runs off the command worker so a
/// long command cannot stall the command plane; the output arrives as the `userShell` item's
/// terminal view event.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionUserShellResult {
    /// Echoes the client's id.
    pub command_id: String,
    /// Admission status.
    pub status: CommandStatus,
}
