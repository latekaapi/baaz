//! State the `Delta` enum cannot carry.

use std::collections::BTreeMap;

use muse_client::schema::{
    ApprovalMode, ApprovalRequestParams, ContextUsage, CumulativeTokenUsage, EffectiveModel, Goal,
    TurnRetryScheduledParams, UserInputRequestParams,
};
use serde::{Deserialize, Serialize};

/// One submission waiting behind the running turn.
///
/// The strip above the composer renders these. Ordering comes from the server:
/// rows move on `turn/unqueued` and `turn/started`, never optimistically.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QueuedTurn {
    /// The turn id the queueing ack minted. `turn/unqueue` needs it verbatim.
    pub turn_id: String,
    /// The `commandId` that queued it, which `turn/unqueued` echoes back.
    pub command_id: String,
    /// The composer text to restore on Edit or on an unqueue.
    pub text: String,
}

/// Everything about a Muse session that `aui_protocol::Session` has no slot for.
///
/// The fold keeps one of these per Muse session. Nothing here is rendered by
/// folding a [`aui_protocol::Delta`]; the app reads it directly for the context
/// meter, the queue strip, the model and mode chips and the retry countdown.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SideState {
    /// Latest `session/contextUsage`. Replaced wholesale; `window_tokens` is
    /// absent when the basis has no limit, so the meter must render "used, no
    /// denominator".
    pub context: Option<ContextUsage>,
    /// Session running totals from `session/tokenUsage.cumulative`. Never goes
    /// backward, and never includes subagent or workflow-child usage.
    pub cumulative: CumulativeTokenUsage,
    /// Latest `session/goalChanged`. An explicit `null` clears it.
    pub goal: Option<Goal>,
    /// Submissions queued behind the running turn, in server order.
    pub queued: Vec<QueuedTurn>,
    /// The live `turn/retryScheduled` fact, if the turn is between attempts.
    /// `retry_delay_ms` is a backoff, not a fire time — derive the countdown
    /// locally.
    pub retry: Option<TurnRetryScheduledParams>,
    /// Approvals awaiting a decision, keyed by `approvalId`, refreshed by
    /// `approval/updated` and removed by `approval/resolved`.
    pub pending_approvals: BTreeMap<String, ApprovalRequestParams>,
    /// Questions awaiting an answer, keyed by `userInputId`.
    pub pending_inputs: BTreeMap<String, UserInputRequestParams>,
    /// The effective model, from `session/modelChanged` or the `Session` object.
    pub model: Option<EffectiveModel>,
    /// The approval mode in effect.
    pub approval_mode: ApprovalMode,
    /// The last `viewCursor` this fold observed. It is what a reconnect passes
    /// to `session/resume`, which then serves `history.mode: "none"` and streams
    /// only the suffix. Opaque: compare, never parse.
    pub last_cursor: String,
    /// `commandId` → the composer text that command was sent with.
    ///
    /// An extension to the spec's list, and the reason it exists: both
    /// `turn/unqueued` and `turn/retracted` identify the submission by its
    /// `commandId` and by nothing else, so this map is what makes "restore the
    /// prompt" possible.
    pub command_text: BTreeMap<String, String>,
    /// The prompt a `turn/retracted` or `turn/unqueued` handed back, for the
    /// composer to pick up. The app clears it once it has.
    pub restored_prompt: Option<String>,
}

impl Default for SideState {
    /// `ApprovalMode` has no `Default` of its own on purpose — a client
    /// **selects** a preconfigured mode and can never construct one — so the
    /// starting value is written out here: `onRequest`, which is the wire
    /// default a session gets when `session/start` names no mode.
    fn default() -> Self {
        Self {
            context: None,
            cumulative: CumulativeTokenUsage::default(),
            goal: None,
            queued: Vec::new(),
            retry: None,
            pending_approvals: BTreeMap::new(),
            pending_inputs: BTreeMap::new(),
            model: None,
            approval_mode: ApprovalMode::OnRequest,
            last_cursor: String::new(),
            command_text: BTreeMap::new(),
            restored_prompt: None,
        }
    }
}

impl SideState {
    /// Remember what a command was sent with, so a later retraction can give it
    /// back.
    pub fn remember_command(&mut self, command_id: impl Into<String>, text: impl Into<String>) {
        self.command_text.insert(command_id.into(), text.into());
    }

    /// The queued row for a turn, if it is still queued.
    pub fn queued_turn(&self, turn_id: &str) -> Option<&QueuedTurn> {
        self.queued.iter().find(|q| q.turn_id == turn_id)
    }
}
