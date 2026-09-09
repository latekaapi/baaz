//! `MuseFold` — MSP view events in, `aui_protocol::Delta`s out.

use std::collections::{BTreeMap, HashMap};

use aui_protocol::{
    ApprovalBadges, ApprovalChoice, ApprovalScope, ApprovalStage, ApprovalState, Answer, Block,
    Delta, MarkerKind, PermissionMode, Provider, QuestionOption, QuestionPreview, ResolvedBy,
    SearchHit, Session, ThinkingState, ToolBody, ToolKind, ToolStatus, TodoItem, TodoState, Turn,
    TurnMeta,
};
use muse_client::schema::{self as msp, ApprovalMode};
use muse_client::MuseEvent;
use serde_json::Value;

use crate::side::{QueuedTurn, SideState};

/// Where a Muse item's rendering lives in the folded session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Slot {
    turn: usize,
    block: usize,
}

/// One Muse session, folded.
struct Folded {
    session: Session,
    side: SideState,
    /// `itemId` → the block it renders as. Absent for items that render as no
    /// block at all (a `userMessage`, which is a whole turn, and the
    /// `request_user_input` tool call, which renders as its question).
    items: HashMap<String, Slot>,
    /// `itemId` → highest revision applied. The apply rule is **replace iff
    /// higher**; `item/delta` never bumps a revision.
    revisions: HashMap<String, u32>,
    /// `approvalId` → its card.
    approvals: HashMap<String, Slot>,
    /// `userInputId` → its question cards, one per question in the request.
    inputs: HashMap<String, Vec<Slot>>,
    /// The session's single todo card, once one exists.
    todo: Option<Slot>,
    /// The session's single goal card, once one exists.
    goal: Option<Slot>,
    /// MSP `turnId` → the index of the assistant turn that hosts its blocks.
    assistant_turns: HashMap<String, usize>,
    /// The id of a `userMessage`'s `Turn::User` → the MSP turn it belongs to, so
    /// a retraction can take the right turn out.
    user_turns: HashMap<String, String>,
    /// Per-turn accumulated usage, summed over every `session/tokenUsage`.
    usage: HashMap<String, TurnUsage>,
    /// Server ids already seen, so a `…/request` and its `…/requested` twin
    /// fold once.
    seen_requests: HashMap<String, ()>,
    /// Counter for naming a marker turn raised by an event with no cursor.
    marker_seq: u64,
    /// Whether an approval mode has been observed for this session yet.
    ///
    /// A session emits `session/approvalModeChanged` at start-up, so the first
    /// observation is not a change; drawing "Approval mode · Auto" above the
    /// first user message would be a marker for something nobody did.
    mode_seen: bool,
    /// The log sequence of the event being folded right now, if it named one.
    ///
    /// Set once per notification and read by [`Folded::push_block`], so that a
    /// block lands where the **log** puts it rather than where the wire happened
    /// to deliver it.
    current_seq: Option<u64>,
    /// Turn id → the log sequence of each of its blocks, in block order.
    ///
    /// Parallel to the turn's `blocks`, and the reason a backfilled transcript
    /// reads the same as a live one (finding F3).
    block_order: HashMap<String, Vec<u64>>,
}

#[derive(Clone, Copy, Debug, Default)]
struct TurnUsage {
    prompt: u64,
    output: u64,
    reasoning: u64,
}

/// Folds a Muse session's view events into the transcript model the library
/// renders.
///
/// One fold serves a whole `muse serve` connection: it keeps one
/// [`aui_protocol::Session`] and one [`SideState`] **per Muse session**, because
/// a single connection multiplexes them (a `session/fork` alone puts two on the
/// wire). [`MuseFold::apply`] returns the deltas for the session the event
/// belonged to; [`MuseFold::last_touched`] names it.
///
/// The fold is pure: no I/O, no clock, no randomness. Replaying a capture always
/// produces the same session, which is what the fixture tests assert.
#[derive(Default)]
pub struct MuseFold {
    sessions: BTreeMap<String, Folded>,
    last_touched: Option<String>,
}

impl MuseFold {
    /// An empty fold.
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one event and return the deltas it produced.
    ///
    /// Every delta returned has already been applied to the fold's own session,
    /// so the caller can either re-apply them to its own copy or read
    /// [`MuseFold::session`]. Events for a session this fold has never seen
    /// create it, because `view/page` backfill and a resume can both deliver
    /// items before any `session/started`.
    pub fn apply(&mut self, event: MuseEvent) -> Vec<Delta> {
        match event {
            MuseEvent::Notification { method, params, cursor, session_id } => {
                self.notification(&method, &params, cursor, session_id)
            }
            MuseEvent::ServerRequest { method, params, .. } => {
                // A server request carries the same params as its sibling
                // `…/requested` notification and must fold exactly once.
                let session_id = params.get("sessionId").and_then(Value::as_str).map(str::to_owned);
                let requested = match method.as_str() {
                    "approval/request" => "approval/requested",
                    "userInput/request" => "userInput/requested",
                    other => other,
                };
                let cursor = params.get("viewCursor").and_then(Value::as_str).map(str::to_owned);
                self.notification(requested, &params, cursor, session_id)
            }
            MuseEvent::Closed(_) => Vec::new(),
        }
    }

    /// The folded session for a Muse session id.
    pub fn session(&self, session_id: &str) -> Option<&Session> {
        self.sessions.get(session_id).map(|s| &s.session)
    }

    /// The side state for a Muse session id.
    pub fn side(&self, session_id: &str) -> Option<&SideState> {
        self.sessions.get(session_id).map(|s| &s.side)
    }

    /// Every Muse session id this fold has seen, in id order.
    pub fn session_ids(&self) -> impl Iterator<Item = &str> {
        self.sessions.keys().map(String::as_str)
    }

    /// The session the last [`MuseFold::apply`] touched.
    pub fn last_touched(&self) -> Option<&str> {
        self.last_touched.as_deref()
    }

    /// Record what a command was submitted with, so a later `turn/retracted` or
    /// `turn/unqueued` can hand the prompt back to the composer.
    ///
    /// The wire cannot supply this: both events name the submission only by its
    /// `commandId`, and the text never comes back. The app calls this the moment
    /// it sends a `turn/start` or `turn/steer`.
    pub fn record_command(&mut self, session_id: &str, command_id: &str, text: &str) {
        self.folded(session_id).side.remember_command(command_id, text);
    }

    /// Record that a `turn/start` was **queued** rather than started.
    ///
    /// This comes from the ack (`disposition: "queued"`), not from a view event,
    /// so it is the app's job to hand it over. Removal is never optimistic: the
    /// row leaves the strip only when `turn/started` or `turn/unqueued` says so.
    pub fn record_queued(
        &mut self,
        session_id: &str,
        turn_id: &str,
        command_id: &str,
        text: &str,
    ) {
        let folded = self.folded(session_id);
        folded.side.remember_command(command_id, text);
        if folded.side.queued_turn(turn_id).is_none() {
            folded.side.queued.push(QueuedTurn {
                turn_id: turn_id.to_owned(),
                command_id: command_id.to_owned(),
                text: text.to_owned(),
            });
        }
    }

    /// Take the prompt a retraction or an unqueue handed back, clearing it.
    pub fn take_restored_prompt(&mut self, session_id: &str) -> Option<String> {
        self.sessions.get_mut(session_id)?.side.restored_prompt.take()
    }

    /// Append a block the **client** authored, in a turn of its own.
    ///
    /// The one block the harness writes itself is the plan card: MSP has no plan
    /// mode, so the proposal is derived from the reply's text (spec §3.1) and
    /// has to enter the transcript from this side. `id` is the turn's id, so a
    /// later [`MuseFold::replace_client_block`] can find it again.
    pub fn append_client_block(&mut self, session_id: &str, id: &str, block: Block) -> Vec<Delta> {
        let folded = self.folded(session_id);
        let mut deltas = Vec::new();
        let turn = folded.standalone_turn(id, &mut deltas);
        let (added, _) = folded.push_block(turn, block);
        deltas.extend(added);
        deltas
    }

    /// Replace the first block of a client-authored turn.
    pub fn replace_client_block(&mut self, session_id: &str, id: &str, block: Block) -> Vec<Delta> {
        let folded = self.folded(session_id);
        let Some(turn) = folded.session.turns.iter().position(|t| t.id() == id) else {
            return Vec::new();
        };
        folded.update_block(Slot { turn, block: 0 }, block)
    }

    /// Settle an approval card from a **losing** `approval/decide`.
    ///
    /// `approvalAlreadyResolved` carries the winning resolution in its error
    /// data, and `approvalNotFound` carries nothing at all. Both mean the card
    /// on screen is a lie, and neither will be followed by an
    /// `approval/resolved` the client has not already missed — so this is the
    /// one path where a resolution enters the fold from the command plane
    /// rather than from the view stream.
    pub fn resolve_approval(
        &mut self,
        session_id: &str,
        approval_id: &str,
        resolution: Option<msp::ApprovalResolutionSummary>,
    ) -> Vec<Delta> {
        let folded = self.folded(session_id);
        let request = folded.side.pending_approvals.remove(approval_id);
        let Some(slot) = folded.approvals.get(approval_id).copied() else { return Vec::new() };
        let Some(request) = request else { return Vec::new() };
        let (state, by) = match &resolution {
            Some(resolution) => {
                let by = match resolution.resolved_by.as_str() {
                    "user" => Some(ResolvedBy::User),
                    "policy" => Some(ResolvedBy::Policy),
                    "llmJudge" => Some(ResolvedBy::LlmJudge),
                    _ => None,
                };
                let allowed = resolution.decision.starts_with("approved");
                let rule = request.subject.command.clone().unwrap_or_else(|| resolution.decision.clone());
                let state = match (by, allowed) {
                    (Some(ResolvedBy::User), true) => ApprovalState::Approving,
                    (Some(ResolvedBy::User), false) => ApprovalState::Denied,
                    (_, true) => ApprovalState::AutoAllowed { rule },
                    (_, false) => ApprovalState::AutoDenied { rule },
                };
                (state, by)
            }
            // `approvalNotFound`: the server has forgotten it and will never say
            // how it ended. "Denied" would be a guess; the honest card is the
            // quiet resolved one with nobody's name on it.
            None => (ApprovalState::Denied, None),
        };
        let block = approval_block(&request, state, by, None);
        folded.update_block(slot, block)
    }

    fn folded(&mut self, session_id: &str) -> &mut Folded {
        self.last_touched = Some(session_id.to_owned());
        self.sessions
            .entry(session_id.to_owned())
            .or_insert_with(|| Folded::new(session_id))
    }

    fn notification(
        &mut self,
        method: &str,
        params: &Value,
        cursor: Option<String>,
        session_id: Option<String>,
    ) -> Vec<Delta> {
        // `session/started` is the one notification whose session id lives
        // inside the `session` object rather than at the top level.
        let session_id = session_id.or_else(|| {
            params.get("session").and_then(|s| s.get("sessionId")).and_then(Value::as_str).map(str::to_owned)
        });
        let Some(session_id) = session_id else {
            return Vec::new();
        };
        self.last_touched = Some(session_id.clone());
        let folded = self.sessions.entry(session_id.clone()).or_insert_with(|| Folded::new(&session_id));
        if let Some(cursor) = cursor {
            folded.side.last_cursor = cursor;
        }
        folded.notification(method, params)
    }
}

impl Folded {
    fn new(session_id: &str) -> Self {
        let mut session = Session::new(session_id, Provider::Muse, String::new(), String::new());
        session.mode = PermissionMode::OnRequest;
        Self {
            session,
            side: SideState::default(),
            items: HashMap::new(),
            revisions: HashMap::new(),
            approvals: HashMap::new(),
            inputs: HashMap::new(),
            todo: None,
            goal: None,
            assistant_turns: HashMap::new(),
            user_turns: HashMap::new(),
            usage: HashMap::new(),
            seen_requests: HashMap::new(),
            marker_seq: 0,
            mode_seen: false,
            current_seq: None,
            block_order: HashMap::new(),
        }
    }

    fn notification(&mut self, method: &str, params: &Value) -> Vec<Delta> {
        // Where this event sits in the session's own log. It is what orders the
        // blocks the event adds, rather than the order the events arrived in —
        // see [`Folded::push_block`] and finding F3.
        self.current_seq = params
            .get("sourceRange")
            .and_then(|range| range.get("first"))
            .and_then(|first| first.get("sequence"))
            .and_then(Value::as_u64);
        match method {
            "session/started" => self.session_started(params),
            "session/branchChanged" => self.branch_changed(params),
            "session/approvalModeChanged" => self.approval_mode_changed(params),
            "session/modelChanged" => self.model_changed(params),
            "session/contextUsage" => self.context_usage(params),
            "session/tokenUsage" => self.token_usage(params),
            "session/todoListChanged" => self.todo_changed(params),
            "session/goalChanged" => self.goal_changed(params),
            "turn/started" => self.turn_started(params),
            "turn/completed" => self.turn_completed(params),
            "turn/retracted" => self.turn_retracted(params),
            "turn/unqueued" => self.turn_unqueued(params),
            "turn/retryScheduled" => self.turn_retry_scheduled(params),
            "item/started" | "item/updated" | "item/completed" => {
                self.item(params, method == "item/completed")
            }
            "item/delta" => self.item_delta(params),
            "approval/requested" => self.approval_requested(params),
            "approval/updated" => self.approval_updated(params),
            "approval/resolved" => self.approval_resolved(params),
            "userInput/requested" => self.user_input_requested(params),
            "userInput/settled" => self.user_input_settled(params),
            "view/gap" => self.view_gap(params),
            _ => Vec::new(),
        }
    }

    // ------------------------------------------------------------ session facts

    fn session_started(&mut self, params: &Value) -> Vec<Delta> {
        let Some(session) = params.get("session") else { return Vec::new() };
        let Ok(session) = serde_json::from_value::<msp::Session>(session.clone()) else {
            return Vec::new();
        };
        self.session.model = session.model_id.clone().unwrap_or_default();
        self.session.cwd = session.workspace_root.clone().unwrap_or_default();
        if let Some(mode) = &session.approval_mode {
            self.side.approval_mode = mode.mode.clone();
            self.session.mode = permission_mode(&mode.mode);
            self.mode_seen = true;
        }
        let mut deltas = Vec::new();
        if let Some(fork) = &session.forked_from {
            deltas.extend(self.marker_turn(
                MarkerKind::ForkedFrom,
                format!("Forked from {}", fork.session_id),
            ));
        }
        deltas
    }

    fn branch_changed(&mut self, params: &Value) -> Vec<Delta> {
        // `branch: null` is a detached-HEAD observation — a fact, not "unchanged".
        self.session.branch =
            params.get("branch").and_then(Value::as_str).map(str::to_owned);
        if let Some(root) = params.get("workspaceRoot").and_then(Value::as_str) {
            if self.session.cwd.is_empty() {
                self.session.cwd = root.to_owned();
            }
        }
        Vec::new()
    }

    fn approval_mode_changed(&mut self, params: &Value) -> Vec<Delta> {
        let Some(mode) = params.get("mode") else { return Vec::new() };
        let Ok(mode) = serde_json::from_value::<ApprovalMode>(mode.clone()) else {
            return Vec::new();
        };
        self.side.approval_mode = mode.clone();
        let mode = permission_mode(&mode);
        let first = !std::mem::replace(&mut self.mode_seen, true);
        let unchanged = mode == self.session.mode;
        self.session.mode = mode;
        // A mode change that changed nothing is not a change; and the first
        // observation is the session announcing what it already is, which is
        // only worth a row when it is *not* the default — then it is a fact
        // about this session rather than about every session.
        if unchanged || (first && mode == PermissionMode::default()) {
            return Vec::new();
        }
        self.marker_turn(
            MarkerKind::PermissionModeChanged { mode },
            format!("Approval mode · {}", mode.label()),
        )
    }

    fn model_changed(&mut self, params: &Value) -> Vec<Delta> {
        if let Some(model_id) = params.get("modelId").and_then(Value::as_str) {
            self.session.model = model_id.to_owned();
        }
        self.side.model = serde_json::from_value(params.clone()).ok();
        Vec::new()
    }

    fn context_usage(&mut self, params: &Value) -> Vec<Delta> {
        self.side.context = serde_json::from_value(params.clone()).ok();
        Vec::new()
    }

    fn token_usage(&mut self, params: &Value) -> Vec<Delta> {
        if let Some(cumulative) = params.get("cumulative") {
            if let Ok(cumulative) = serde_json::from_value(cumulative.clone()) {
                self.side.cumulative = cumulative;
            }
        }
        let Some(turn_id) = params.get("turnId").and_then(Value::as_str) else {
            return Vec::new();
        };
        let entry = self.usage.entry(turn_id.to_owned()).or_default();
        entry.prompt += params.get("promptTokens").and_then(Value::as_u64).unwrap_or(0);
        let usage = params.get("usage");
        entry.output += usage
            .and_then(|u| u.get("outputTokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        entry.reasoning += usage
            .and_then(|u| u.get("reasoningTokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if let Some(model_id) = params.get("modelId").and_then(Value::as_str) {
            if self.session.model.is_empty() {
                self.session.model = model_id.to_owned();
            }
        }
        Vec::new()
    }

    fn todo_changed(&mut self, params: &Value) -> Vec<Delta> {
        // Replace the whole list every event; an empty `items` is a cleared
        // list, not a no-op.
        let items: Vec<msp::TodoItem> = params
            .get("items")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        // An empty list is the agent saying it has no tasks any more, so the
        // card goes: a todo card with nothing in it is not a todo card.
        if items.is_empty() {
            return match self.todo.take() {
                Some(slot) => self.remove_block(slot),
                None => Vec::new(),
            };
        }
        let block = Block::Todo {
            items: items
                .iter()
                .map(|item| TodoItem {
                    label: item.text.clone(),
                    state: todo_state(&item.status),
                    elapsed_ms: None,
                })
                .collect(),
        };
        match self.todo {
            Some(slot) => self.update_block(slot, block),
            None => {
                let mut deltas = Vec::new();
                let id = format!("todo:{}", self.session.id);
                let turn = self.standalone_turn(&id, &mut deltas);
                let (added, slot) = self.push_block(turn, block);
                deltas.extend(added);
                self.todo = Some(slot);
                deltas
            }
        }
    }

    fn goal_changed(&mut self, params: &Value) -> Vec<Delta> {
        let goal: Option<msp::Goal> =
            params.get("goal").and_then(|v| serde_json::from_value(v.clone()).ok());
        self.side.goal = goal.clone();
        let Some(goal) = goal else {
            // An explicit `null` clears the goal; `null` never means unchanged.
            return match self.goal.take() {
                Some(slot) => self.remove_block(slot),
                None => Vec::new(),
            };
        };
        let block = Block::Goal {
            objective: goal.objective.clone(),
            status: goal.status.clone(),
            percent_complete: Some(goal.percent_complete as f32),
            current_work: goal.current_work.clone(),
            next_work: goal.next_work.clone(),
        };
        match self.goal {
            Some(slot) => self.update_block(slot, block),
            None => {
                let mut deltas = Vec::new();
                let id = format!("goal:{}", self.session.id);
                let turn = self.standalone_turn(&id, &mut deltas);
                let (added, slot) = self.push_block(turn, block);
                deltas.extend(added);
                self.goal = Some(slot);
                deltas
            }
        }
    }

    // -------------------------------------------------------------------- turns

    fn turn_started(&mut self, params: &Value) -> Vec<Delta> {
        // A queued turn leaves the strip at its launch boundary, and this is
        // that boundary. `turn/started` never fires for a steer.
        if let Some(turn_id) = params.get("turnId").and_then(Value::as_str) {
            self.side.queued.retain(|queued| queued.turn_id != turn_id);
        }
        // The assistant turn itself is created lazily by its first block: the
        // `userMessage` item — which is its own turn in `aui_protocol` — arrives
        // *after* `turn/started`, and the transcript must read in that order.
        Vec::new()
    }

    fn turn_completed(&mut self, params: &Value) -> Vec<Delta> {
        let Some(turn_id) = params.get("turnId").and_then(Value::as_str).map(str::to_owned) else {
            return Vec::new();
        };
        let terminal = params.get("terminal").and_then(Value::as_str).unwrap_or("completed");
        let mut deltas = Vec::new();

        if terminal == "failed" {
            let error = params.get("error");
            // The wire's `kind` is a log token and its `reason` is a code; both
            // go through one table (`failure.rs`) so the card reads as a
            // sentence and still carries what a bug report needs.
            let kind =
                error.and_then(|e| e.get("kind")).and_then(Value::as_str).unwrap_or("modelError");
            let message =
                error.and_then(|e| e.get("message")).and_then(Value::as_str).unwrap_or_default();
            let reason = params.get("reason").and_then(Value::as_str);
            let crate::failure::Failure { title, detail } =
                crate::failure::humanize(kind, message, reason);
            let retryable =
                error.and_then(|e| e.get("retryable")).and_then(Value::as_bool).unwrap_or(false);
            let turn = self.ensure_assistant_turn(&turn_id, &mut deltas);
            let (added, _) = self.push_block(turn, Block::Error { title, detail, retryable });
            deltas.extend(added);
        } else if terminal == "cancelled" {
            let reason = params.get("reason").and_then(Value::as_str).unwrap_or("Turn cancelled");
            let turn = self.ensure_assistant_turn(&turn_id, &mut deltas);
            let (added, _) = self.push_block(
                turn,
                Block::Marker { kind: MarkerKind::TurnCancelled, text: reason.to_owned() },
            );
            deltas.extend(added);
        }

        // The retry schedule, if any, is over once the turn reaches a terminal.
        self.side.retry = None;

        let Some(&turn) = self.assistant_turns.get(&turn_id) else {
            return deltas;
        };
        let usage = self.usage.get(&turn_id).copied().unwrap_or_default();
        let meta = TurnMeta {
            model: self.session.model.clone(),
            duration_ms: params.get("durationMs").and_then(Value::as_u64).unwrap_or(0),
            tokens_in: usage.prompt,
            tokens_out: usage.output,
            reasoning_tokens: usage.reasoning,
            // Every live catalog row reported `cost: null`, so cost stays
            // client-side view math and is 0.0 until a catalog supplies one.
            cost_usd: 0.0,
        };
        let delta = Delta::TurnFinished { turn_id: self.session.turns[turn].id().to_owned(), meta };
        self.session.apply(delta.clone());
        deltas.push(delta);
        deltas
    }

    fn turn_retracted(&mut self, params: &Value) -> Vec<Delta> {
        let mut deltas = self.restore_and_remove(params);
        deltas.extend(self.marker_turn(MarkerKind::TurnRetracted, "Prompt retracted".to_owned()));
        deltas
    }

    fn turn_unqueued(&mut self, params: &Value) -> Vec<Delta> {
        if let Some(turn_id) = params.get("turnId").and_then(Value::as_str) {
            self.side.queued.retain(|q| q.turn_id != turn_id);
        }
        self.restore_and_remove(params)
    }

    /// Both `turn/retracted` and `turn/unqueued` identify the submission by its
    /// `commandId`, hand the prompt back, and take the turn out of the
    /// transcript.
    fn restore_and_remove(&mut self, params: &Value) -> Vec<Delta> {
        if let Some(command_id) = params.get("commandId").and_then(Value::as_str) {
            if let Some(text) = self.side.command_text.get(command_id) {
                self.side.restored_prompt = Some(text.clone());
            }
        }
        let Some(turn_id) = params.get("turnId").and_then(Value::as_str).map(str::to_owned) else {
            return Vec::new();
        };
        let mut deltas = Vec::new();
        // The user turn is keyed on the `userMessage` item id, so it is found
        // through the MSP turn it belongs to.
        let user_turns: Vec<String> = self
            .user_turns
            .iter()
            .filter(|(_, owner)| owner.as_str() == turn_id)
            .map(|(id, _)| id.clone())
            .collect();
        for id in user_turns {
            self.user_turns.remove(&id);
            deltas.extend(self.remove_turn(&id));
        }
        if self.assistant_turns.contains_key(&turn_id) {
            deltas.extend(self.remove_turn(&turn_id));
        }
        deltas
    }

    fn turn_retry_scheduled(&mut self, params: &Value) -> Vec<Delta> {
        let retry: Option<msp::TurnRetryScheduledParams> =
            serde_json::from_value(params.clone()).ok();
        let text = match &retry {
            Some(retry) => format!(
                "attempt {}/{} · retrying in {}s · {}",
                retry.attempt,
                retry.max_attempts,
                retry.retry_delay_ms / 1000,
                retry.reason
            ),
            None => "retrying".to_owned(),
        };
        let _ = text;
        self.side.retry = retry;
        // A scheduled retry is a **live** fact, not a transcript row: it is the
        // countdown above the composer, and the turn's own terminal replaces it.
        // Drawing a marker as well would leave a permanent "retrying in 4s" in
        // the history for something that finished a minute ago.
        Vec::new()
    }

    fn view_gap(&mut self, params: &Value) -> Vec<Delta> {
        let next = params.get("next").and_then(Value::as_str).unwrap_or_default();
        self.marker_turn(
            MarkerKind::ViewGap,
            format!("Some events were missed while disconnected; backfilling to {next}"),
        )
    }

    // -------------------------------------------------------------------- items

    fn item(&mut self, params: &Value, terminal: bool) -> Vec<Delta> {
        let Some(item) = params.get("item") else { return Vec::new() };
        let Ok(item) = serde_json::from_value::<msp::Item>(item.clone()) else {
            return Vec::new();
        };
        // Apply rule: replace iff the revision is higher.
        let previous = self.revisions.get(&item.item_id).copied();
        if let Some(previous) = previous {
            if item.revision <= previous {
                return Vec::new();
            }
        }
        self.revisions.insert(item.item_id.clone(), item.revision);

        match item.kind {
            msp::ItemKind::UserMessage => self.user_message(&item, previous.is_some()),
            _ => self.assistant_item(&item, terminal),
        }
    }

    fn user_message(&mut self, item: &msp::Item, seen: bool) -> Vec<Delta> {
        if seen {
            // A `userMessage` is a whole `Turn::User`, and `Delta` has no
            // variant that edits one. The only revision the wire produces is the
            // `retracted: true` flag, and `turn/retracted` removes the turn
            // anyway, so there is nothing to do here.
            return Vec::new();
        }
        if let (Some(command_id), text) = (item.command_id.as_ref(), item.text.clone()) {
            self.side.remember_command(command_id.clone(), text.unwrap_or_default());
        }
        let text = item
            .display_text
            .clone()
            .or_else(|| item.text.clone())
            .unwrap_or_default();
        // `attachments` is metadata only — the base64 is never echoed back — so
        // the chip carries the media type and nothing else.
        let attachments = item
            .attachments
            .iter()
            .flatten()
            .map(|attachment| aui_protocol::Attachment {
                name: attachment.media_type.clone(),
                kind: aui_protocol::AttachmentKind::Image,
                size_bytes: None,
                meta: None,
                state: aui_protocol::UploadState::Ready,
            })
            .collect();
        let turn = Turn::User {
            id: item.item_id.clone(),
            text,
            attachments,
            mentions: Vec::new(),
        };
        let delta = Delta::TurnStarted { turn };
        self.session.apply(delta.clone());
        if let Some(turn_id) = &item.turn_id {
            self.user_turns.insert(item.item_id.clone(), turn_id.clone());
        }
        vec![delta]
    }

    fn assistant_item(&mut self, item: &msp::Item, terminal: bool) -> Vec<Delta> {
        // A `request_user_input` tool call renders as its question card, not as
        // a tool card; the question arrives on `userInput/requested` under the
        // same id.
        if matches!(item.kind, msp::ItemKind::ToolCall)
            && item.tool.as_deref() == Some("request_user_input")
        {
            return Vec::new();
        }
        let Some(block) = self.block_for(item, terminal) else { return Vec::new() };
        match self.items.get(&item.item_id).copied() {
            Some(slot) => self.update_block(slot, block),
            None => {
                let mut deltas = Vec::new();
                let turn = self.item_host_turn(item, &mut deltas);
                let (added, slot) = self.push_block(turn, block);
                deltas.extend(added);
                self.items.insert(item.item_id.clone(), slot);
                deltas
            }
        }
    }

    fn block_for(&self, item: &msp::Item, terminal: bool) -> Option<Block> {
        let block = match item.kind {
            msp::ItemKind::AgentMessage => Block::Text {
                text: item.text.clone().unwrap_or_default(),
                streaming: !terminal,
            },
            msp::ItemKind::Reasoning => Block::Thinking {
                // One entry per summary part; part boundaries are blank lines.
                text: item.summary.clone().unwrap_or_default().join("\n\n"),
                elapsed_ms: 0,
                summary: item.summary.as_ref().and_then(|parts| parts.first().cloned()),
                state: if terminal { ThinkingState::Done } else { ThinkingState::Thinking },
            },
            msp::ItemKind::ToolCall => {
                let tool = item.tool.clone().unwrap_or_default();
                let (kind, verb, target) = tool_shape(&tool, item.args.as_deref());
                let body = tool_body(&kind, item.visible_output.as_deref().unwrap_or(""), None, !terminal);
                Block::ToolCall { id: item.item_id.clone(), kind, verb, target, status: tool_status(&item.status), duration_ms: item.duration_ms, body }
            }
            msp::ItemKind::UserShell => Block::ToolCall {
                id: item.item_id.clone(),
                kind: ToolKind::Shell,
                verb: "$".to_owned(),
                target: item.command_text.clone().unwrap_or_default(),
                status: tool_status(&item.status),
                duration_ms: item.duration_ms,
                body: ToolBody::Shell {
                    output_lines: split_lines(item.visible_output.as_deref().unwrap_or("")),
                    exit_code: item.exit_code,
                    live: !terminal,
                },
            },
            msp::ItemKind::Subagent => Block::ToolCall {
                id: item.item_id.clone(),
                kind: ToolKind::SubAgent,
                verb: "Delegated".to_owned(),
                target: item
                    .objective
                    .clone()
                    .or_else(|| item.role.clone())
                    .unwrap_or_else(|| "subagent".to_owned()),
                status: tool_status(&item.status),
                duration_ms: item.duration_ms,
                body: ToolBody::SubAgent { turns: Vec::new() },
            },
            msp::ItemKind::Compaction => Block::Marker {
                kind: MarkerKind::ContextCompacted,
                text: compaction_text(item),
            },
            // `workflow` and `reminderChild` have no home in the library yet, so
            // they take the mandated generic rendering: kind + status +
            // `fallbackText`.
            _ => Block::Generic {
                kind: item.kind.as_wire().unwrap_or("unknown").to_owned(),
                status: item.status.as_wire().unwrap_or("unknown").to_owned(),
                text: item.fallback_text.clone().unwrap_or_default(),
            },
        };
        Some(block)
    }

    fn item_delta(&mut self, params: &Value) -> Vec<Delta> {
        let Some(item_id) = params.get("itemId").and_then(Value::as_str) else {
            return Vec::new();
        };
        let Some(text) = params.get("delta").and_then(Value::as_str).map(str::to_owned) else {
            return Vec::new();
        };
        let Some(slot) = self.items.get(item_id).copied() else { return Vec::new() };
        let Some(turn) = self.session.turns.get(slot.turn) else { return Vec::new() };
        let turn_id = turn.id().to_owned();
        // `field` is a dotted path and **absent means `"text"`**.
        let field = params.get("field").and_then(Value::as_str).unwrap_or("text");
        let delta = match (field, turn.blocks().get(slot.block)) {
            ("output", _) => Delta::ToolOutputDelta { turn_id, block_index: slot.block, text },
            (_, Some(Block::Thinking { .. })) => {
                Delta::ThinkingDelta { turn_id, block_index: slot.block, text }
            }
            (_, Some(Block::ToolCall { .. })) => {
                Delta::ToolOutputDelta { turn_id, block_index: slot.block, text }
            }
            _ => Delta::TextDelta { turn_id, block_index: slot.block, text },
        };
        if self.session.apply(delta.clone()) {
            vec![delta]
        } else {
            Vec::new()
        }
    }

    // ---------------------------------------------------------------- approvals

    fn approval_requested(&mut self, params: &Value) -> Vec<Delta> {
        let Ok(request) = serde_json::from_value::<msp::ApprovalRequestParams>(params.clone())
        else {
            return Vec::new();
        };
        if self.seen_requests.insert(request.approval_id.clone(), ()).is_some() {
            return Vec::new();
        }
        self.side.pending_approvals.insert(request.approval_id.clone(), request.clone());
        let block = approval_block(&request, ApprovalState::Pending, None, None);
        let mut deltas = Vec::new();
        // The approval's `turnId` is the gated command's turn — for a
        // `session/userShell` that is the shell item's `commandId`, which is
        // exactly the synthetic turn the shell item was filed under.
        let turn = self.ensure_assistant_turn(&request.turn_id, &mut deltas);
        let (added, slot) = self.push_block(turn, block);
        deltas.extend(added);
        self.approvals.insert(request.approval_id.clone(), slot);
        deltas
    }

    fn approval_updated(&mut self, params: &Value) -> Vec<Delta> {
        let Some(approval_id) = params.get("approvalId").and_then(Value::as_str) else {
            return Vec::new();
        };
        let Some(slot) = self.approvals.get(approval_id).copied() else { return Vec::new() };
        // The choices change between stages — re-render from the update, never
        // from a cached copy.
        let Some(request) = self.side.pending_approvals.get_mut(approval_id) else {
            return Vec::new();
        };
        if let Some(subject) = params.get("subject") {
            if let Ok(subject) = serde_json::from_value(subject.clone()) {
                request.subject = subject;
            }
        }
        if let Some(choices) = params.get("availableChoices") {
            if let Ok(choices) = serde_json::from_value(choices.clone()) {
                request.available_choices = choices;
            }
        }
        if let Some(current) = params.get("currentRequirementId") {
            if let Ok(current) = serde_json::from_value(current.clone()) {
                request.current_requirement_id = current;
            }
        }
        let request = request.clone();
        let block = approval_block(&request, ApprovalState::Pending, None, None);
        self.update_block(slot, block)
    }

    fn approval_resolved(&mut self, params: &Value) -> Vec<Delta> {
        let Ok(resolved) = serde_json::from_value::<msp::ApprovalResolvedParams>(params.clone())
        else {
            return Vec::new();
        };
        let request = self.side.pending_approvals.remove(&resolved.approval_id);
        let Some(slot) = self.approvals.get(&resolved.approval_id).copied() else {
            return Vec::new();
        };
        let rule = request
            .as_ref()
            .and_then(|r| r.subject.command.clone())
            .unwrap_or_else(|| resolved.decision.as_wire().unwrap_or("policy").to_owned());
        let allowed = matches!(resolved.policy_result, msp::ApprovalPolicyResult::Allow);
        let by = resolved_by(&resolved.resolved_by);
        let state = match (by, allowed) {
            // A policy or judge resolution can arrive with no user interaction
            // at all — the card opened and closed in the same breath and was
            // never actionable.
            (Some(ResolvedBy::User), true) => ApprovalState::Approving,
            (Some(ResolvedBy::User), false) => ApprovalState::Denied,
            (_, true) => ApprovalState::AutoAllowed { rule },
            (_, false) => ApprovalState::AutoDenied { rule },
        };
        let Some(request) = request else { return Vec::new() };
        let block = approval_block(&request, state, by, None);
        self.update_block(slot, block)
    }

    // --------------------------------------------------------------- user input

    fn user_input_requested(&mut self, params: &Value) -> Vec<Delta> {
        let Ok(request) = serde_json::from_value::<msp::UserInputRequestParams>(params.clone())
        else {
            return Vec::new();
        };
        if self.seen_requests.insert(request.user_input_id.clone(), ()).is_some() {
            return Vec::new();
        }
        self.side.pending_inputs.insert(request.user_input_id.clone(), request.clone());
        let mut deltas = Vec::new();
        let turn = self.ensure_assistant_turn(&request.turn_id, &mut deltas);
        let mut slots = Vec::new();
        // One MSP request may carry N questions; each becomes its own card and
        // they are answered together.
        for question in &request.questions {
            let block = question_block(&request, question, None);
            let (added, slot) = self.push_block(turn, block);
            deltas.extend(added);
            slots.push(slot);
        }
        self.inputs.insert(request.user_input_id.clone(), slots);
        deltas
    }

    fn user_input_settled(&mut self, params: &Value) -> Vec<Delta> {
        let Ok(settled) = serde_json::from_value::<msp::UserInputSettledParams>(params.clone())
        else {
            return Vec::new();
        };
        let Some(request) = self.side.pending_inputs.remove(&settled.user_input_id) else {
            return Vec::new();
        };
        let Some(slots) = self.inputs.get(&settled.user_input_id).cloned() else {
            return Vec::new();
        };
        let mut deltas = Vec::new();
        for (question, slot) in request.questions.iter().zip(slots) {
            let answer = answer_for(question, &settled);
            let block = question_block(&request, question, Some(answer));
            deltas.extend(self.update_block(slot, block));
        }
        deltas
    }

    // ------------------------------------------------------------------ plumbing

    /// The assistant turn that hosts `turn_id`'s blocks, appending it if it does
    /// not exist yet.
    fn ensure_assistant_turn(&mut self, turn_id: &str, deltas: &mut Vec<Delta>) -> usize {
        if let Some(&index) = self.assistant_turns.get(turn_id) {
            return index;
        }
        let delta = Delta::TurnStarted {
            turn: Turn::Assistant {
                id: turn_id.to_owned(),
                blocks: Vec::new(),
                meta: TurnMeta::default(),
            },
        };
        self.session.apply(delta.clone());
        deltas.push(delta);
        let index = self.session.turns.len() - 1;
        self.assistant_turns.insert(turn_id.to_owned(), index);
        index
    }

    /// Where an item's block goes.
    ///
    /// A `userShell` item is the one kind outside a turn (`turnId: null`); it is
    /// filed under its own `commandId`, which is also what the approval it
    /// raises reports as its `turnId`.
    fn item_host_turn(&mut self, item: &msp::Item, deltas: &mut Vec<Delta>) -> usize {
        let turn_id = item
            .turn_id
            .clone()
            .or_else(|| item.command_id.clone())
            .unwrap_or_else(|| item.item_id.clone());
        self.ensure_assistant_turn(&turn_id, deltas)
    }

    /// A turn of its own for a session-level card.
    ///
    /// A [`Block::Marker`], a [`Block::Todo`] and a [`Block::Goal`] are blocks,
    /// not turns, so a fact that belongs to the session rather than to a reply
    /// still needs a host turn. Giving each one its own turn keeps the
    /// transcript in wire order: a marker lands where it happened instead of
    /// being appended to whatever reply happened to be last, which for a reply
    /// that already finished would put it after that turn's footer.
    fn standalone_turn(&mut self, id: &str, deltas: &mut Vec<Delta>) -> usize {
        self.ensure_assistant_turn(id, deltas)
    }

    /// A marker row, in its own turn, keyed on the cursor of the event that
    /// caused it — cursors are unique per event, so no two markers collide.
    fn marker_turn(&mut self, kind: MarkerKind, text: String) -> Vec<Delta> {
        let mut deltas = Vec::new();
        self.marker_seq += 1;
        let id = if self.side.last_cursor.is_empty() {
            format!("marker:{}:{}", self.session.id, self.marker_seq)
        } else {
            format!("marker:{}", self.side.last_cursor)
        };
        let turn = self.standalone_turn(&id, &mut deltas);
        let (added, _) = self.push_block(turn, Block::Marker { kind, text });
        deltas.extend(added);
        deltas
    }

    /// Add a block to a turn, in **log** order.
    ///
    /// # Why this is not a plain append (finding F3)
    ///
    /// The same turn arrives two ways. Live, an item announces itself with
    /// `item/started` the moment it begins, so a shell tool call that then
    /// raises an approval is already in the transcript when the approval lands:
    /// tool card, then approval card. Backfilled, `view/page` serves no
    /// `item/started` at all — the tool call only appears at its
    /// `item/completed`, which the log records *after* the approval it was
    /// waiting on. Appending in arrival order therefore reverses the two, and a
    /// session read a second time no longer says what it said the first time.
    ///
    /// Both events carry the item's own `sourceRange.first.sequence` — the same
    /// number on `item/started` and on `item/completed` — so the log's order is
    /// knowable from either path, and it is the order used here.
    ///
    /// gpui's transcript is an append-only list of [`Delta`]s with no insert, so
    /// an out-of-order arrival is expressed as an append plus the
    /// [`Delta::BlockUpdated`]s that rotate the tail. Every cached [`Slot`] past
    /// the insertion point shifts with it.
    fn push_block(&mut self, turn: usize, block: Block) -> (Vec<Delta>, Slot) {
        let turn_id = self.session.turns[turn].id().to_owned();
        // An event with no sequence (a synthesised marker, a `session/*` fact)
        // belongs after everything already filed, which is what `u64::MAX` says.
        let order = self.current_seq.unwrap_or(u64::MAX);
        let keys = self.block_order.entry(turn_id.clone()).or_default();
        // `<=` so that two blocks from the same log record keep the order they
        // were folded in, which is the order the fold created them.
        let at = keys.partition_point(|&key| key <= order);
        keys.insert(at, order);

        let mut deltas = vec![Delta::BlockAdded { turn_id: turn_id.clone(), block: block.clone() }];
        self.session.apply(deltas[0].clone());
        let last = self.session.turns[turn].blocks().len() - 1;
        if at == last {
            return (deltas, Slot { turn, block: at });
        }

        // Rotate `[at, last]` right by one: each old occupant moves down a slot
        // and the new block takes `at`.
        let mut moved: Vec<Block> = self.session.turns[turn].blocks()[at..last].to_vec();
        moved.insert(0, block);
        for (offset, block) in moved.into_iter().enumerate() {
            let delta = Delta::BlockUpdated {
                turn_id: turn_id.clone(),
                block_index: at + offset,
                block,
            };
            if self.session.apply(delta.clone()) {
                deltas.push(delta);
            }
        }
        self.shift_slots(turn, at);
        (deltas, Slot { turn, block: at })
    }

    /// Every cached slot at or past `at` in `turn` moved down one.
    fn shift_slots(&mut self, turn: usize, at: usize) {
        let bump = |slot: &mut Slot| {
            if slot.turn == turn && slot.block >= at {
                slot.block += 1;
            }
        };
        self.items.values_mut().for_each(bump);
        self.approvals.values_mut().for_each(bump);
        self.inputs.values_mut().flatten().for_each(bump);
        self.todo.iter_mut().for_each(bump);
        self.goal.iter_mut().for_each(bump);
    }

    fn update_block(&mut self, slot: Slot, block: Block) -> Vec<Delta> {
        let Some(turn) = self.session.turns.get(slot.turn) else { return Vec::new() };
        let delta = Delta::BlockUpdated {
            turn_id: turn.id().to_owned(),
            block_index: slot.block,
            block,
        };
        if self.session.apply(delta.clone()) {
            vec![delta]
        } else {
            Vec::new()
        }
    }

    fn remove_block(&mut self, slot: Slot) -> Vec<Delta> {
        let Some(turn) = self.session.turns.get(slot.turn) else { return Vec::new() };
        let turn_id = turn.id().to_owned();
        let delta = Delta::BlockRemoved { turn_id: turn_id.clone(), block_index: slot.block };
        if !self.session.apply(delta.clone()) {
            return Vec::new();
        }
        // The order keys are parallel to the blocks, and every slot past the
        // hole moved up one.
        if let Some(keys) = self.block_order.get_mut(&turn_id) {
            if slot.block < keys.len() {
                keys.remove(slot.block);
            }
        }
        let unshift = |s: &mut Slot| {
            if s.turn == slot.turn && s.block > slot.block {
                s.block -= 1;
            }
        };
        self.items.values_mut().for_each(unshift);
        self.approvals.values_mut().for_each(unshift);
        self.inputs.values_mut().flatten().for_each(unshift);
        self.todo.iter_mut().for_each(unshift);
        self.goal.iter_mut().for_each(unshift);
        vec![delta]
    }

    fn remove_turn(&mut self, turn_id: &str) -> Vec<Delta> {
        let delta = Delta::TurnRemoved { turn_id: turn_id.to_owned() };
        if !self.session.apply(delta.clone()) {
            return Vec::new();
        }
        self.reindex();
        vec![delta]
    }

    /// Turn indices shift when a turn is removed, so every cached slot is
    /// rebuilt from the transcript itself.
    fn reindex(&mut self) {
        let positions: HashMap<String, usize> = self
            .session
            .turns
            .iter()
            .enumerate()
            .map(|(index, turn)| (turn.id().to_owned(), index))
            .collect();
        self.assistant_turns.retain(|id, index| match positions.get(id) {
            Some(&position) => {
                *index = position;
                true
            }
            None => false,
        });
        let live: std::collections::HashSet<usize> = self.assistant_turns.values().copied().collect();
        self.items.retain(|_, slot| live.contains(&slot.turn));
        self.approvals.retain(|_, slot| live.contains(&slot.turn));
        self.inputs.retain(|_, slots| slots.iter().all(|slot| live.contains(&slot.turn)));
        if let Some(slot) = self.todo {
            if !live.contains(&slot.turn) {
                self.todo = None;
            }
        }
        if let Some(slot) = self.goal {
            if !live.contains(&slot.turn) {
                self.goal = None;
            }
        }
        // A turn that is gone takes its block ordering with it, or a turn id
        // reused after a retraction would inherit the old turn's keys.
        self.block_order.retain(|id, _| positions.contains_key(id));
    }
}

fn permission_mode(mode: &ApprovalMode) -> PermissionMode {
    match mode {
        ApprovalMode::AllowAll => PermissionMode::AllowAll,
        ApprovalMode::PromptUnmatched => PermissionMode::PromptUnmatched,
        ApprovalMode::OnRequest => PermissionMode::OnRequest,
        ApprovalMode::DenyUnmatched => PermissionMode::DenyUnmatched,
    }
}

fn todo_state(status: &msp::TodoStatus) -> TodoState {
    match status {
        msp::TodoStatus::Pending => TodoState::Pending,
        msp::TodoStatus::InProgress => TodoState::Running,
        msp::TodoStatus::Completed => TodoState::Done,
        // A cancelled todo has no mark of its own; it reads as finished.
        _ => TodoState::Done,
    }
}

fn tool_status(status: &msp::ItemStatus) -> ToolStatus {
    match status {
        msp::ItemStatus::InProgress => ToolStatus::Running,
        msp::ItemStatus::Completed => ToolStatus::Success,
        msp::ItemStatus::Cancelled => ToolStatus::Cancelled,
        // `failed`, `rejected`, `timedOut` and anything this build has never
        // heard of are all "it did not work".
        _ => ToolStatus::Error,
    }
}

fn resolved_by(by: &msp::ApprovalResolvedBy) -> Option<ResolvedBy> {
    match by {
        msp::ApprovalResolvedBy::User => Some(ResolvedBy::User),
        msp::ApprovalResolvedBy::Policy => Some(ResolvedBy::Policy),
        msp::ApprovalResolvedBy::LlmJudge => Some(ResolvedBy::LlmJudge),
        _ => None,
    }
}

/// `args` is model-authored JSON **as a verbatim string**, so it is parsed
/// per-tool and falls back to raw display when it is not valid JSON.
///
/// There is no tool *kind* taxonomy on the wire — `Item.tool` is a bare name —
/// so the mapping is by name and by which `rawArgs` field is present. The
/// families are the ones Muse actually ships (`read_file`, `read_skill`,
/// `bash`, the write/edit family, the search family and the web family); a name
/// this table has never seen is still presented honestly as a Muse-provided
/// tool rather than guessed at.
fn tool_shape(tool: &str, args: Option<&str>) -> (ToolKind, String, String) {
    let parsed: Option<Value> = args.and_then(|args| serde_json::from_str(args).ok());
    let field = |name: &str| {
        parsed.as_ref().and_then(|v| v.get(name)).and_then(Value::as_str).map(str::to_owned)
    };
    let raw = || args.unwrap_or_default().to_owned();
    let path = || field("path").or_else(|| field("file_path")).or_else(|| field("filename"));
    match tool {
        "bash" | "shell" => (ToolKind::Shell, "Ran".to_owned(), field("command").unwrap_or_else(raw)),
        "read" | "read_file" | "view" | "cat" => (ToolKind::Read, "Read".to_owned(), path().unwrap_or_else(raw)),
        "read_skill" => (
            ToolKind::Read,
            "Read skill".to_owned(),
            field("name").or_else(path).unwrap_or_else(raw),
        ),
        "write" | "write_file" | "create" | "create_file" => {
            (ToolKind::Write, "Wrote".to_owned(), path().unwrap_or_else(raw))
        }
        "edit" | "edit_file" | "str_replace" | "apply_patch" => {
            (ToolKind::Edit, "Edited".to_owned(), path().unwrap_or_else(raw))
        }
        "grep" | "glob" | "search" | "search_files" | "ripgrep" => (
            ToolKind::Search,
            "Searched".to_owned(),
            field("pattern").or_else(|| field("query")).or_else(path).unwrap_or_else(raw),
        ),
        "fetch" | "web_fetch" | "web_search" | "web_read" => (
            ToolKind::Web,
            if tool == "web_search" { "Searched the web".to_owned() } else { "Fetched".to_owned() },
            field("url").or_else(|| field("query")).unwrap_or_else(raw),
        ),
        _ => (
            // There is no tool *kind* taxonomy on the wire, so anything the app
            // does not recognise is presented as a Muse-provided tool.
            ToolKind::Mcp { server: "muse".to_owned(), tool: tool.to_owned() },
            "Ran".to_owned(),
            field("command").or_else(path).unwrap_or_else(raw),
        ),
    }
}

/// The body that goes with a tool's kind.
///
/// The rule is "never draw a body the wire did not send". MSP carries one
/// rendering surface per tool call — `visibleOutput`, a plain string — and no
/// diffs, no structured hits and no result lists, so:
///
/// * a **read** renders as its header plus the line count, which is the
///   library's own rendering for a read and what the Phase 2 review asked for;
/// * a **search** promotes `path:line:text` output to real hits when every
///   line parses, and otherwise keeps the raw output;
/// * everything else keeps the raw output, because a card with no body would
///   hide what the tool actually said.
fn tool_body(kind: &ToolKind, visible_output: &str, exit_code: Option<i32>, live: bool) -> ToolBody {
    let lines = split_lines(visible_output);
    match kind {
        ToolKind::Read => ToolBody::Read { lines: lines.len() },
        ToolKind::Search => match search_hits(&lines) {
            Some(hits) => ToolBody::Search { hits },
            None => ToolBody::Shell { output_lines: lines, exit_code, live },
        },
        _ => ToolBody::Shell { output_lines: lines, exit_code, live },
    }
}

/// `path:line:text` on every non-empty line, or nothing.
fn search_hits(lines: &[String]) -> Option<Vec<SearchHit>> {
    let mut hits = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let (path, rest) = line.split_once(':')?;
        let (number, snippet) = rest.split_once(':')?;
        let line_no: u32 = number.trim().parse().ok()?;
        hits.push(SearchHit { path: path.to_owned(), line: line_no, snippet: snippet.trim().to_owned() });
    }
    (!hits.is_empty()).then_some(hits)
}

fn compaction_text(item: &msp::Item) -> String {
    let before = item.tokens_before.unwrap_or(0);
    let after = item.tokens_after.unwrap_or(0);
    let trigger = item
        .trigger
        .as_ref()
        .and_then(|t| t.as_wire())
        .unwrap_or("auto");
    format!("Context compacted · {before} → {after} tokens · {trigger}")
}

fn split_lines(text: &str) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    text.trim_end_matches('\n').split('\n').map(str::to_owned).collect()
}

fn approval_scope(scope: &msp::ApprovalChoiceScope) -> ApprovalScope {
    match scope {
        msp::ApprovalChoiceScope::Once => ApprovalScope::ThisCommand,
        msp::ApprovalChoiceScope::Session => ApprovalScope::ThisSession,
        msp::ApprovalChoiceScope::LocalPersistent => ApprovalScope::ThisWorktree,
        _ => ApprovalScope::ThisCommand,
    }
}

fn approval_decision(decision: &msp::ApprovalDecision) -> aui_protocol::ApprovalDecision {
    use aui_protocol::ApprovalDecision as Ui;
    match decision {
        msp::ApprovalDecision::Approved => Ui::Once,
        msp::ApprovalDecision::ApprovedForSession => Ui::ApprovedForSession,
        msp::ApprovalDecision::ApprovedPolicyAmendment => Ui::PolicyAmendment,
        msp::ApprovalDecision::Denied => Ui::Deny,
        msp::ApprovalDecision::DeniedPolicyAmendment => Ui::DeniedPolicyAmendment,
        msp::ApprovalDecision::TimedOut => Ui::TimedOut,
        _ => Ui::Abort,
    }
}

fn approval_block(
    request: &msp::ApprovalRequestParams,
    state: ApprovalState,
    resolved_by: Option<ResolvedBy>,
    feedback: Option<String>,
) -> Block {
    let subject_stages = request.subject.stages.clone().unwrap_or_default();
    let stages: Vec<ApprovalStage> = subject_stages
        .iter()
        .map(|stage| ApprovalStage {
            position: stage.position,
            total: stage.total_stages,
            argv: stage.argv.clone(),
            argv_complete: stage.argv_complete,
            resolved: stage.resolution.kind != "unresolved",
            suggested_rule: stage.suggested_prefix.as_ref().map(|p| p.label.clone()),
        })
        .collect();
    let current_stage = Some(request.current_requirement_id.source_index as usize)
        .filter(|index| *index < stages.len());
    let choices: Vec<ApprovalChoice> = request
        .available_choices
        .iter()
        .map(|choice| ApprovalChoice {
            id: choice.choice_id.clone(),
            label: choice.label.clone(),
            decision: approval_decision(&choice.decision),
            scope: approval_scope(&choice.scope),
            rule_preview: choice.rule_preview.clone(),
            accepts_feedback: choice.accepts_feedback.unwrap_or(false),
        })
        .collect();
    let scope = choices
        .iter()
        .find(|choice| choice.scope != ApprovalScope::ThisCommand)
        .map(|choice| choice.scope)
        .unwrap_or(ApprovalScope::ThisCommand);
    let rule = choices.iter().find_map(|choice| choice.rule_preview.clone());
    Block::Approval {
        id: request.approval_id.clone(),
        tool: request.tool_name.clone(),
        command: request
            .subject
            .command
            .clone()
            .or_else(|| request.subject.path.clone())
            .or_else(|| request.subject.target.clone())
            .unwrap_or_default(),
        reason: String::new(),
        cwd: request.subject.workspace_root.clone().unwrap_or_default(),
        capabilities: Vec::new(),
        scope,
        state,
        rule,
        choices,
        stages,
        current_stage,
        badges: ApprovalBadges {
            protected_write: request.protected_write,
            judge_escalated: request.judge_escalated,
        },
        feedback,
        resolved_by,
    }
}

fn question_block(
    request: &msp::UserInputRequestParams,
    question: &msp::UserInputQuestion,
    answer: Option<Answer>,
) -> Block {
    Block::Question {
        // One request may carry several questions, so the block id names both.
        id: format!("{}:{}", request.user_input_id, question.id),
        prompt: question.question.clone(),
        subtitle: String::new(),
        header: question.header.clone(),
        options: question
            .options
            .iter()
            .enumerate()
            .map(|(index, option)| QuestionOption {
                label: option.label.clone(),
                description: option.description.clone().unwrap_or_default(),
                key: (index + 1).to_string(),
                preview: option.preview.as_ref().map(|preview| QuestionPreview {
                    content: preview.content.clone(),
                    format: preview.format.clone(),
                }),
            })
            .collect(),
        multi: matches!(question.selection.mode, msp::UserInputSelectionMode::Multiple),
        // MSP has no "Other" row: free text is `freeText` on an answer, and the
        // "let me explain instead" path is `userInput/clarify`.
        allow_other: false,
        answer,
        timeout_ms: request.auto_resolution_ms,
    }
}

/// Turn a settlement into the answer the card renders. Answers key on the option
/// **label**, not an index, so the labels are resolved back to indices here.
fn answer_for(question: &msp::UserInputQuestion, settled: &msp::UserInputSettledParams) -> Answer {
    let index_of = |label: &str| question.options.iter().position(|o| o.label == label);
    let mut answer = Answer::default();
    for given in &settled.answers {
        if given.question_id != question.id {
            continue;
        }
        if let Some(label) = &given.selected_label {
            answer.selected.extend(index_of(label));
        }
        for label in given.selected_labels.iter().flatten() {
            answer.selected.extend(index_of(label));
        }
        if let Some(text) = &given.free_text {
            answer.other = Some(text.clone());
        }
    }
    if let Some(clarification) = &settled.clarification {
        answer.other = Some(clarification.content.clone());
    }
    answer
}
