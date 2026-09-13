//! The `view/*` lane (SS4.7) and the items it pages: the transcript's own
//! records, their bodies, and the unframed events a view emits.
//!
//! Part of [`crate::schema`]; see that module for the conventions every
//! type here follows.

use serde::{Deserialize, Serialize};

use super::*;

/// One transcript item at one revision (tdd SS4.4.1 common fields plus the per-kind fields of
/// SS4.5.2–4.5.10, all optional and owned by the kind their doc names). `item/started`,
/// `item/updated`, and `item/completed` carry the full object; `item/delta` appends to it by field
/// path. One flat struct, exactly as the schema declares it — never an enum.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    /// `subagent`: agent definition path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_path: Option<String>,
    /// `toolCall`: the approval that gated this call; join key for the approval view events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<String>,
    /// `toolCall`: the model-authored argument JSON, **verbatim**; clients parse. Verbatim keeps
    /// the fold byte-deterministic and survives model-emitted almost-JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<String>,
    /// `userMessage`: image attachment metadata only — base64 payloads are not echoed on the view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<MessageAttachment>>,
    /// `toolCall`: `true` once the task was durably backgrounded; delivered via `item/updated`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<bool>,
    /// `toolCall`: who backgrounded the task; absent on pre-split records — never inferred.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_initiator: Option<BackgroundInitiator>,
    /// `toolCall`: the provider call id (`call_...`), opaque.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    /// `subagent`/`reminderChild`: the child's own session id, readable via `session/read` /
    /// `view/page`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_session_id: Option<String>,
    /// `reminderChild`: parent-session-dir-relative child log path; absent when no filesystem log.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_session_log_path: Option<String>,
    /// `workflow`: folded per-child state, keyed by `(childId, attempt)`, re-emitted whole on every
    /// change — the item `revision` is the ordering guard.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<WorkflowChild>>,
    /// `userMessage`/`userShell`: the submitting command — multi-client UIs de-duplicate their
    /// local echo on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
    /// `userShell`: the command text as submitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_text: Option<String>,
    /// `subagent`: camelCased `SubagentControlStatus`; `status` stays the generic item vocabulary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_status: Option<SubagentControlStatus>,
    /// `subagent`: nesting depth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<u32>,
    /// `userMessage`: presentation form; absent when the client sent none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_text: Option<String>,
    /// `userShell`/`subagent`: observed wall-clock duration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// `workflow`: launched entry identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<String>,
    /// `userShell`: the process exit code, when it exited by code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// `userShell`: the terminating signal NUMBER, when signalled (e.g. 9) — the durable payload
    /// verbatim; nothing maps numbers to names, and the fold never invents one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_signal: Option<i32>,
    /// `toolCall`: machine-readable failure class (`TaskFailureKind`, snake_case verbatim).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_kind: Option<String>,
    /// `toolCall`/`subagent`: `Failed`/`Rejected`/`Cancelled` reason text, verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    /// Server-provided one-line human summary any kind MAY carry, for generic rendering of kinds a
    /// client does not recognize.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_text: Option<String>,
    /// `reminderChild`: the reminder generation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation_id: Option<u64>,
    /// Bare UUIDv7; the item's identity across its whole lifecycle. Opaque to clients.
    pub item_id: String,
    /// The item kind: clients MUST render unknown kinds generically — kind name plus `status` plus
    /// `fallbackText`.
    pub kind: ItemKind,
    /// `workflow`: the reconciled terminal message, set on completion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// `toolCall`: rich content the model saw beyond text; base64 is not inlined on the view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_visible_content: Option<Vec<ModelVisibleContent>>,
    /// `subagent`: objective as spawned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objective: Option<String>,
    /// `compaction`: terminal only — folds installed/fallback status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<CompactionOutcome>,
    /// `toolCall`/`userShell`: stored-output reference; fetch the full bytes via `item/readOutput`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_ref: Option<OutputRef>,
    /// `toolCall`: stored structured-patch reference (`kind: "tool_patch"`,
    /// `mediaType: "application/json"`); the body is fetched via `item/readOutput` with
    /// `patchRef.id`, and the ref survives the SS2.5.2 elided-snapshot rung exactly like
    /// `outputRef` without ever being an elision trigger.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch_ref: Option<OutputRef>,
    /// `toolCall`: server-authored edit-family diff summary — always beside `patchRef`;
    /// absent = no diff available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch_summary: Option<PatchSummary>,
    /// `reasoning`: provider reasoning item id (e.g. `rs_...`), for provider-side correlation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_item_id: Option<String>,
    /// `compaction`: noop/failure reason, verbatim (snake_case durable vocabulary).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// RFC3339 timestamp of the item's driving durable record. Absent on ephemeral-opened items
    /// until first durable re-emission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded_at: Option<String>,
    /// `reminderChild`: the reminder agent's id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reminder_agent_id: Option<String>,
    /// `subagent`: the result envelope from `ResultReady`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<SubagentResult>,
    /// `workflow`: set on resumed launches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_from_run_id: Option<String>,
    /// `userMessage`: `true` after an accepted retract; re-emitted via `item/updated`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retracted: Option<bool>,
    /// Integer >= 1, strictly monotonic per item; the apply rule is replace-iff-higher.
    /// `item/delta` never bumps it.
    pub revision: u32,
    /// `subagent`: role as spawned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// `workflow`: launched script identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script_id: Option<String>,
    /// Terminal = anything other than `"inProgress"`. Unknown values MUST be treated as
    /// terminal-unknown and rendered generically.
    pub status: ItemStatus,
    /// `userMessage`: `true` when injected mid-turn via `turn/steer` or `ifBusy: "steer"`; absent
    /// otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steered: Option<bool>,
    /// `compaction`: summarizer strategy (installed only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy_id: Option<String>,
    /// `subagent`: durable child identity (`subagent_id`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_id: Option<String>,
    /// `compaction`: opaque provenance string naming the compaction boundary; accepted as a read
    /// anchor by `session/resume` and `view/page`. Still opaque — relay it, never parse it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summarized_through: Option<String>,
    /// `reasoning`: one entry per summary part; part *n* streams via `item/delta` field
    /// `"summary.n"` (part boundary = index change).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<Vec<String>>,
    /// `reminderChild`: the linked task id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// `userMessage`: the prompt text as submitted. `agentMessage`: the accumulated reply, streamed
    /// via `item/delta` field `"text"`. `reasoning`: raw committed reasoning text where the
    /// provider exposes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// `compaction`: token budget snapshot after, when measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_after: Option<u64>,
    /// `compaction`: token budget snapshot before, when measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_before: Option<u64>,
    /// `toolCall`: tool name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// `compaction`: what initiated the compaction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<CompactionTrigger>,
    /// `workflow`: camelCased `WorkflowLaunchTriggerSource`, verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_source: Option<String>,
    /// `true` when the server's per-surface text budget saturated a streamed surface; the durable
    /// full text remains in the log.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
    /// The owning turn (== the submitting `commandId` for fresh turns). `null` only for
    /// `userShell` — the one kind outside a turn.
    ///
    /// **Required-nullable**, against msp.d.ts's `turnId?: string | null`: every item in every
    /// capture carries the key, explicitly `null` on `userShell`.
    #[serde(default)]
    pub turn_id: Option<String>,
    /// `subagent`: **transitive** observed usage — the child and its own descendants; updated in
    /// place, absent until first observation; never folded into `session/tokenUsage.cumulative`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
    /// `toolCall`/`userShell`: bounded transcript-visible result text; streams via `item/delta`
    /// field `"output"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible_output: Option<String>,
    /// `subagent`/`workflow`: the owning durable workflow run id (opaque string, not a UUID
    /// family).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_run_id: Option<String>,
}

/// Server-authored edit-family diff summary (tdd SS4.5.5): `files` counts the stored patch
/// document's file entries; `added` and `removed` are the total `+`/`-` prefixed LINE counts summed
/// over the stored patch's hunks across all files — line counts, never hunk or byte counts. Rides
/// the `toolCall` item beside `patchRef`; the body is fetched via `item/readOutput` with
/// `patchRef.id`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchSummary {
    /// Total `+`-prefixed line count across all files' hunks.
    pub added: u64,
    /// File entries in the stored patch document.
    pub files: u64,
    /// Total `-`-prefixed line count across all files' hunks.
    pub removed: u64,
}

/// `item/completed` params (tdd SS4.3): the item reached its terminal state — the authoritative
/// final object. Always cites a durable `sourceRange`. Clients MUST accept `item/completed` for an
/// `itemId` they never saw `item/started` for.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemCompletedParams {
    /// The full item at its terminal revision.
    pub item: Item,
    /// The owning session.
    pub session_id: String,
    /// The durable records this event folded from.
    pub source_range: SourceRange,
    /// Opaque, strictly monotonic view cursor.
    pub view_cursor: String,
}

/// `item/delta` params (tdd SS4.3.1): a UTF-8-safe streaming append to an open item's field.
/// Ephemeral-sourced by definition: this params object has **no** `sourceRange` member at all.
/// Deltas never bump `revision`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemDeltaParams {
    /// The appended text; concatenation of a field path's deltas in cursor order equals that
    /// field's value on `item/completed` unless the surface saturated.
    pub delta: String,
    /// Dotted field path the append targets (`"summary.0"`, `"output"`, …); **absent means
    /// `"text"`**.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// The open item being appended to.
    pub item_id: String,
    /// The owning session.
    pub session_id: String,
    /// Opaque, strictly monotonic view cursor.
    pub view_cursor: String,
}

open_enum! {
    /// The nine v1 item kinds (tdd SS4.5.2–4.5.10). A new kind is additive evolution, and clients
    /// MUST render unknown kinds generically.
    ItemKind {
        UserMessage = "userMessage",
        AgentMessage = "agentMessage",
        Reasoning = "reasoning",
        ToolCall = "toolCall",
        UserShell = "userShell",
        Subagent = "subagent",
        Workflow = "workflow",
        ReminderChild = "reminderChild",
        Compaction = "compaction",
    }
}

/// `item/started` params (tdd SS4.3): a new item opened on the transcript. `sourceRange` is absent
/// when the item opens on an ephemeral record (the delta-streamed kinds); the item's authoritative
/// open is re-stated by its durable-sourced `item/completed`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemStartedParams {
    /// The full item at revision 1.
    pub item: Item,
    /// The owning session.
    pub session_id: String,
    /// The durable records this event folded from; absent on an ephemeral-sourced open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_range: Option<SourceRange>,
    /// Opaque, strictly monotonic view cursor.
    pub view_cursor: String,
}

open_enum! {
    /// Item status (tdd SS4.4.1). Terminal = anything other than `"inProgress"`, and unknown values
    /// are terminal-unknown, rendered generically.
    ItemStatus {
        InProgress = "inProgress",
        Completed = "completed",
        Failed = "failed",
        Cancelled = "cancelled",
        Rejected = "rejected",
        TimedOut = "timedOut",
    }
}

/// `item/updated` params (tdd SS4.4.2): an open item changed non-terminally in a way deltas cannot
/// express — the full item re-emitted at a higher revision. **Apply it iff the revision is
/// higher.**
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemUpdatedParams {
    /// The full item at a higher revision.
    pub item: Item,
    /// The owning session.
    pub session_id: String,
    /// The durable records this event folded from.
    pub source_range: SourceRange,
    /// Opaque, strictly monotonic view cursor.
    pub view_cursor: String,
}

closed_enum! {
    /// The `item/readOutput` content encoding (tdd SS4.7.4). Closed: text media is ALWAYS `utf8`
    /// and binary media `base64`; a third value would change the client's decode contract.
    ItemReadOutputEncoding {
        Utf8 = "utf8",
        Base64 = "base64",
    }
}

/// `item/readOutput` params (tdd SS4.7.4): byte-ranged fetch of stored full output that the view
/// truncated. Read-only; works on loaded and unloaded sessions; no lease.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemReadOutputParams {
    /// The item whose stored output is fetched.
    pub item_id: String,
    /// STORED bytes requested; default and max 6 MiB. A value above the max is served at the max.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length_bytes: Option<u64>,
    /// STORED-byte offset to start from; 0 when omitted. STORED bytes are a property of the
    /// output, not of a transport.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset_bytes: Option<u64>,
    /// The `outputRef.id` from the item (never the `uri`; the uri is display/provenance).
    pub output_ref: String,
    /// The session the item belongs to.
    pub session_id: String,
}

/// `item/readOutput` result (tdd SS4.7.4): one stored-byte page.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemReadOutputResult {
    /// STORED bytes served in THIS response (not the output's total; that is `outputRef.byteLen`
    /// on the item). The next page's `offsetBytes` is `offsetBytes + byteLen`.
    pub byte_len: u64,
    /// The served range, decoded per `encoding`.
    pub content: String,
    /// How `content` encodes the stored bytes.
    pub encoding: ItemReadOutputEncoding,
    /// `true` when this range reached the end of the stored output.
    pub eof: bool,
    /// The stored output's media type (e.g. `text/plain`).
    pub media_type: String,
    /// The STORED-byte offset this response actually served from.
    pub offset_bytes: u64,
}

/// One element of a `view/page` result's `events` array: an **unframed view notification** exactly
/// as tdd SS4.2.1 defines it — the nested pair `{"method", "params"}`, which is the live
/// notification minus `jsonrpc` and `emittedAtMs`, with nothing lifted out of `params` and nothing
/// spliced into it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnframedViewNotification {
    /// The event type — the live notification's method name, retained because the type lives only
    /// in the notification name and would otherwise be lost.
    pub method: String,
    /// The notification's params object, verbatim.
    pub params: UnframedViewNotificationParams,
}

/// The `params` of an unframed view notification (tdd SS4.2.1, SS4.2). Declares the SS4.2 base
/// members every view notification carries and stays **open** for the event-specific fields
/// SS4.5/SS4.6/SS5 add per event type — those are preserved verbatim in [`Self::extra`], because the
/// v1 schema model names no method-correlated union and a published schema that rejected a real
/// page element would be the defect this type exists to avoid.
///
/// `sourceRange` is required here and not optional: `view/page` serves durable-sourced events only,
/// and durable-sourced is *defined* as carrying a `sourceRange` — the ephemeral exemption applies
/// to `item/delta`, which `view/page` never replays.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnframedViewNotificationParams {
    /// The owning session.
    pub session_id: String,
    /// The durable records this event folded from.
    pub source_range: SourceRange,
    /// The event's opaque, strictly monotonic view cursor. It stays **inside** `params`, where
    /// every live notification already carries it.
    pub view_cursor: String,
    /// Every event-specific member, verbatim — select the arm with the sibling `method` and
    /// re-parse this map into the matching `*Params` type.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `view/gap` params (spec 208 SS4.8): push delivery dropped events, and this names the hole. On
/// receipt a client may take either sanctioned recovery: splice-fill — buffer live events at
/// cursors >= `next`, page `(after, next)` forward, discard the overlap, splice — or re-anchor
/// through the anchored read surface.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewGapParams {
    /// The last cursor delivered before the hole — the exclusive lower bound of the undelivered
    /// range. An **opaque** view cursor: relay it, never parse it.
    pub after: String,
    /// The first cursor delivered after the hole — the exclusive upper bound, and the position
    /// delivery continues from. Also opaque.
    pub next: String,
    /// The session whose subscription dropped events.
    pub session_id: String,
}

open_enum! {
    /// The `view/page` request-mode anchor (tdd SS4.7.3), v1 value `latestCompaction`.
    ViewPageAnchor {
        LatestCompaction = "latestCompaction",
    }
}

closed_enum! {
    /// The `view/page` paging direction (tdd SS4.7.3). Closed: a third value would change the
    /// paging contract.
    ViewPageDirection {
        Forward = "forward",
        Backward = "backward",
    }
}

/// `view/page` params (tdd SS4.7.3): cursor-paged reads of the session view.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewPageParams {
    /// The cold-client request mode, resolved at call time to the latest installed compaction
    /// boundary. The one named exception to the SS4.1 observed-only rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<ViewPageAnchor>,
    /// The exclusive anchor. Forward: the page starts strictly after it; omitted means from the
    /// beginning of the view. Backward: the page ends strictly before it; omitted means from the
    /// head. Mutually exclusive with `anchor`. Opaque — never parse it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// Paging direction; `forward` when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<ViewPageDirection>,
    /// Maximum events to return, 1–1000 inclusive. The server MAY return fewer.
    pub limit: u32,
    /// The session to page.
    pub session_id: String,
}

/// The `resolvedAnchor` echo (tdd SS4.7.3): subsequent pages pass ordinary cursors, so the anchor
/// never needs re-resolving.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewPageResolvedAnchor {
    /// The view cursor of the resolved boundary's `compaction` event.
    pub boundary_cursor: String,
}

/// `view/page` result (tdd SS4.7.3).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewPageResult {
    /// The page's events, always ascending by `viewCursor` in both directions. Durable-sourced
    /// only; pages are contiguous and can never skip a cursor.
    pub events: Vec<UnframedViewNotification>,
    /// The anchor for the next page in the same direction: forward, the last event's cursor;
    /// backward, the first's. `null` at the end of the view in that direction — never omitted.
    /// Required-nullable.
    #[serde(default)]
    pub next_cursor: Option<String>,
    /// The resolution echo, present exactly when the request used `anchor`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_anchor: Option<ViewPageResolvedAnchor>,
}

/// The `snapshot` object returned by `session/resume` / `session/read` (tdd SS4.9).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewSnapshot {
    /// Present exactly when `history.mode` is `anchoredSnapshot`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<SnapshotAnchor>,
    /// Snapshot schema version. A client whose generated types are older than this must fall back
    /// to inline/paged history.
    pub schema_version: u32,
    /// The complete folded view at `viewCursor`.
    pub state: SnapshotState,
    /// The cursor the state is folded at; every event after it streams as the suffix.
    pub view_cursor: String,
}

/// `view/subscribe` params (tdd SS4.7.1): attach this connection's live view subscription for
/// a session at an explicit cursor — the fine-grained counterpart of the session auto-subscribe,
/// and the gap-free re-attachment path after `view/unsubscribe`. It never loads a session, takes
/// no lease, writes nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewSubscribeParams {
    /// Resume cursor. When present, the server replays `(after, head]` as ordinary view
    /// notifications before any live event — one gapless cursor sequence. Omitted means "from
    /// now": no historical replay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    /// The session to follow.
    pub session_id: String,
}

/// `view/subscribe` result (tdd SS4.7.1).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewSubscribeResult {
    /// The view head at subscribe time; the client knows it is live once cursors pass it.
    pub view_cursor: String,
}

/// `view/unsubscribe` params (tdd SS4.7.2): remove this connection from the session's view
/// subscription set. It does not unload the session; unloading is the idle policy.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewUnsubscribeParams {
    /// The session to stop following.
    pub session_id: String,
}

/// `view/unsubscribe` result (tdd SS4.7.2): the empty object. Idempotent — unsubscribing while not
/// subscribed returns `{}`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewUnsubscribeResult {}

/// One workflow child's folded state (tdd SS4.5.8), keyed by `(childId, attempt)`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowChild {
    /// The attempt number.
    pub attempt: u32,
    /// The child's id within the run.
    pub child_id: String,
    /// The child's observed duration, when recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// The child's display label, when recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The child's phase, when recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    /// The child's recorded result reference, verbatim, when present — an opaque string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_ref: Option<String>,
    /// camelCased `WorkflowChildLifecycleStatus`, verbatim (durable runtime vocabulary).
    pub status: String,
    /// The child's terminal, turn vocabulary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<TurnTerminal>,
    /// The child's observed usage, when recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
}
