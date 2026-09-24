//! Decoded frames into render-ready deltas: one primary lane plus a fallback,
//! and the reason.
//!
//! RENDERING LANES: **`item/completed` frames are the primary lane.**
//! `item/agentMessage/delta` frames are decoded and accumulated per
//! `(turn_id, item_id)` as a fallback lane; they emit no deltas on arrival.
//! When an `item/completed` closes an item, its buffered deltas are dropped
//! (the completed text renders whole). When a turn ends with buffered deltas
//! still open — the interrupted path, where `item/completed` never arrives
//! for the streamed message — `turn/completed` flushes them so the streamed
//! text survives. `item/started` is likewise carried, not rendered.
//!
//! Why `item/completed` stays primary:
//!
//! * `basic.jsonl` — a turn whose single `READY` arrives as one delta plus
//!   one completed item — renders its whole message through `item/completed`
//!   alone. The completed lane closes every message on the normal path; the
//!   interrupted path (`interrupt.jsonl`) never sends `item/completed` for
//!   the streamed message, so the delta fallback is what renders it there.
//! * `item/completed` carries whole `item.text`, so the fold needs no
//!   cross-frame assembly state on the normal path: no chunk bookkeeping,
//!   nothing to disagree about.
//! * The trap this task exists to avoid is folding both lanes and
//!   double-rendering every message. The fallback renders only text no
//!   completed item ever closed — rather than "deduplicating" two lanes with
//!   a heuristic — so there is no seam where the two paths can disagree on
//!   the normal path. The agreement test proves it: folding `basic.jsonl` or
//!   `approval.jsonl` with its `item/agentMessage/delta` lines stripped
//!   yields deltas identical to folding it whole, and a dedicated test proves
//!   the interrupted turn's streamed text still survives.
//!
//! What the fold emits, per frame:
//!
//! * `item/completed` with an `agentMessage` (non-empty text): one
//!   [`Delta::TurnStarted`] per unseen turn id, then one [`Delta::BlockAdded`]
//!   with a complete [`Block::Text`] (`streaming: false`).
//! * `item/completed` with a `userMessage`: one [`Delta::TurnStarted`] with a
//!   [`Turn::User`] per unseen item id.
//! * `item/completed` with `reasoning` (non-empty): a [`Block::Thinking`].
//! * `item/completed` with `commandExecution`: a [`Block::ToolCall`] shell
//!   card (`Success` on `completed` plus exit 0, `Error` on `completed`
//!   otherwise or on `failed`, `Cancelled` on `declined`, `Running` while
//!   `inProgress`; an unknown future status fails closed to `Error`, never
//!   back to a spinner).
//! * `item/completed` with anything else: a [`Block::Generic`] carrying the
//!   wire kind, status, and text — carried, never dropped.
//! * `turn/completed`: one [`Delta::TurnFinished`] per started turn, keyed on
//!   the wire turn id, with per-turn tokens from the `last` bucket (never the
//!   cumulative `total`) and `cost_usd: 0.0` — the neutral unknown default,
//!   never a measurement: the protocol reports tokens, never money, and there
//!   is deliberately no cost field on [`crate::frame::TokenUsage`].
//! * `thread/tokenUsage/updated`: recorded against `(thread_id, turn_id)`;
//!   no deltas. `account/rateLimits/updated`: the account snapshot; no
//!   deltas. Everything else: no deltas.

use std::collections::{HashMap, HashSet};

use aui_protocol::{Block, Delta, ThinkingState, ToolBody, ToolKind, ToolStatus, Turn, TurnMeta};
use provider::ProviderEvent;
use serde_json::Value;

use crate::frame::{Frame, Notification, TokenCounts};

/// The money guard's feed: the latest `account/rateLimits/updated` push, kept
/// beside the seam's account shape.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AccountSnapshot {
    /// Primary window usage percent, when reported.
    pub used_percent: Option<u64>,
    /// Plan name (`prolite`, …), when reported.
    pub plan: Option<String>,
    /// Unix time the window resets, when reported.
    pub resets_at: Option<u64>,
}

impl AccountSnapshot {
    /// Record a push. Unknown fields are ignored; a push that names no
    /// window at all still counts as "a reading was seen".
    pub fn observe(&mut self, params: &Value) {
        let limits = params.get("rateLimits");
        let primary = limits.and_then(|limits| limits.get("primary"));
        if let Some(used) =
            primary.and_then(|primary| primary.get("usedPercent")).and_then(Value::as_u64)
        {
            self.used_percent = Some(used);
        }
        if let Some(plan) = limits
            .and_then(|limits| limits.get("planType"))
            .and_then(Value::as_str)
        {
            self.plan = Some(plan.to_owned());
        }
        if let Some(resets) = primary
            .and_then(|primary| primary.get("resetsAt"))
            .and_then(Value::as_u64)
        {
            self.resets_at = Some(resets);
        }
    }

    /// Whether any push has been seen at all.
    pub fn has_reading(&self) -> bool {
        self.used_percent.is_some() || self.plan.is_some() || self.resets_at.is_some()
    }

    /// The display label for [`provider::Ack::Account`]. `None` until the
    /// first push arrives.
    pub fn label(&self) -> Option<String> {
        if !self.has_reading() {
            return None;
        }
        let plan = self.plan.as_deref().unwrap_or("codex");
        let mut label = match self.used_percent {
            Some(used) => format!("Codex {plan} {used}%"),
            None => format!("Codex {plan}"),
        };
        if let Some(resets) = self.resets_at {
            label.push_str(&format!(" (resets {resets})"));
        }
        Some(label)
    }
}

/// The fold: decoded frames in, [`Delta`]s out.
///
/// Stateful only where the wire is relational: turn id → started turn (many
/// items share one turn), `(thread_id, turn_id)` → per-turn tokens (usage
/// arrives on its own notification, after the text), and the server-minted
/// session mapping.
#[derive(Clone, Debug, Default)]
pub struct CodexFold {
    session_id: Option<String>,
    thread_id: Option<String>,
    model: Option<String>,
    assistant_started: HashSet<String>,
    /// The fallback lane: streamed `item/agentMessage/delta` text per
    /// `(turn_id, item_id)`, in first-seen order. Dropped when an
    /// `item/completed` closes the item; flushed at `turn/completed` when
    /// the turn was interrupted before any completed frame arrived.
    delta_text: HashMap<(String, String), String>,
    delta_order: Vec<(String, String)>,
    user_started: HashSet<String>,
    usage: HashMap<(String, String), TokenCounts>,
    account: AccountSnapshot,
    current_turn: Option<String>,
}

impl CodexFold {
    /// An empty fold.
    pub fn new() -> Self {
        Self::default()
    }

    /// The server-minted session id, once `thread/started` has been seen.
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// The latest turn id seen on `turn/started`, if any.
    pub fn current_turn(&self) -> Option<&str> {
        self.current_turn.as_deref()
    }

    /// Remember the effective model (from `model/list` or the turn params):
    /// it lands in the [`TurnMeta`] footer.
    pub fn set_model(&mut self, model: &str) {
        self.model = Some(model.to_owned());
    }

    /// The per-turn figure for `(thread_id, turn_id)`: the `last` bucket,
    /// never the cumulative `total`.
    pub fn usage_for(&self, thread_id: &str, turn_id: &str) -> Option<&TokenCounts> {
        self.usage.get(&(thread_id.to_owned(), turn_id.to_owned()))
    }

    /// The latest account reading (see [`AccountSnapshot`]).
    pub fn account(&self) -> &AccountSnapshot {
        &self.account
    }

    /// Fold one decoded frame into render-ready deltas. Only notifications
    /// render: requests and responses are the pump's routing business (see
    /// [`crate::child`]), and contribute nothing here.
    pub fn apply(&mut self, frame: &Frame) -> Vec<Delta> {
        match frame {
            Frame::Notification(notification) => self.apply_notification(notification),
            Frame::Request { .. } | Frame::Response { .. } | Frame::ResponseError { .. } => {
                Vec::new()
            }
        }
    }

    fn apply_notification(&mut self, notification: &Notification) -> Vec<Delta> {
        match notification {
            Notification::ThreadStarted { .. } => {
                self.thread_id = notification.thread_id().map(str::to_owned);
                self.session_id = notification.session_id().map(str::to_owned);
                Vec::new()
            }
            Notification::TurnStarted { .. } => {
                self.current_turn = notification.turn_id().map(str::to_owned);
                Vec::new()
            }
            Notification::TokenUsage { .. } => {
                if let (Some(thread), Some(turn), Some(usage)) = (
                    notification.thread_id(),
                    notification.turn_id(),
                    notification.usage(),
                ) {
                    self.usage.insert(
                        (thread.to_owned(), turn.to_owned()),
                        usage.per_turn().clone(),
                    );
                }
                Vec::new()
            }
            Notification::RateLimitsUpdated { .. } => {
                if let Some(params) = notification.rate_limit_params() {
                    self.account.observe(params);
                }
                Vec::new()
            }
            Notification::ItemCompleted { .. } => self.apply_item(notification),
            Notification::AgentMessageDelta { turn_id, item_id, delta, .. } => {
                self.push_delta(turn_id, item_id, delta);
                Vec::new()
            }
            Notification::TurnCompleted { .. } => self.finish_turn(notification),
            // Status, MCP, error, warning, resolved, and unknown kinds move
            // no transcript.
            _ => Vec::new(),
        }
    }

    fn ensure_assistant(&mut self, turn_id: &str, deltas: &mut Vec<Delta>) {
        if self.assistant_started.insert(turn_id.to_owned()) {
            deltas.push(Delta::TurnStarted {
                turn: Turn::Assistant {
                    id: turn_id.to_owned(),
                    blocks: Vec::new(),
                    meta: TurnMeta::default(),
                    timestamp: None,
                },
            });
        }
    }

    /// Accumulate one streamed chunk on the fallback lane. Nothing renders
    /// here: the text only survives if `turn/completed` finds it still open.
    fn push_delta(&mut self, turn_id: &str, item_id: &str, delta: &str) {
        if delta.is_empty() {
            return;
        }
        let key = (turn_id.to_owned(), item_id.to_owned());
        if !self.delta_text.contains_key(&key) {
            self.delta_order.push(key.clone());
        }
        self.delta_text.entry(key).or_default().push_str(delta);
    }

    /// Forget one item's buffered deltas: an `item/completed` closed it, so
    /// the completed text renders whole and the fallback must not repeat it.
    fn drop_delta(&mut self, turn_id: &str, item_id: &str) {
        self.delta_text.remove(&(turn_id.to_owned(), item_id.to_owned()));
        self.delta_order
            .retain(|key| !(key.0 == turn_id && key.1 == item_id));
    }

    /// Drain every still-open buffer for `turn_id`, oldest item first. Items
    /// a completed frame already closed were dropped, so this is exactly the
    /// text no completed frame ever carried.
    fn take_pending_delta_text(&mut self, turn_id: &str) -> Vec<String> {
        let mut pending = Vec::new();
        let mut kept = Vec::new();
        for key in std::mem::take(&mut self.delta_order) {
            if key.0 == turn_id {
                if let Some(text) = self.delta_text.remove(&key) {
                    if !text.is_empty() {
                        pending.push(text);
                    }
                }
            } else {
                kept.push(key);
            }
        }
        self.delta_order = kept;
        pending
    }

    fn apply_item(&mut self, notification: &Notification) -> Vec<Delta> {
        let mut deltas = Vec::new();
        let (Some(turn_id), Some(item)) = (notification.turn_id(), notification.item()) else {
            return deltas;
        };
        match item.kind() {
            "agentMessage" => {
                self.drop_delta(turn_id, item.id());
                if item.text().is_empty() {
                    return deltas;
                }
                self.ensure_assistant(turn_id, &mut deltas);
                deltas.push(Delta::BlockAdded {
                    turn_id: turn_id.to_owned(),
                    block: Block::Text { text: item.text().to_owned(), streaming: false },
                });
            }
            "userMessage" => {
                if self.user_started.insert(item.id().to_owned()) {
                    deltas.push(Delta::TurnStarted {
                        turn: Turn::User {
                            id: item.id().to_owned(),
                            text: item.text().to_owned(),
                            attachments: Vec::new(),
                            mentions: Vec::new(),
                            timestamp: None,
                        },
                    });
                }
            }
            "reasoning" => {
                if item.text().is_empty() {
                    return deltas;
                }
                self.ensure_assistant(turn_id, &mut deltas);
                deltas.push(Delta::BlockAdded {
                    turn_id: turn_id.to_owned(),
                    block: Block::Thinking {
                        text: item.text().to_owned(),
                        elapsed_ms: 0,
                        summary: None,
                        state: ThinkingState::Done,
                    },
                });
            }
            "commandExecution" => {
                self.ensure_assistant(turn_id, &mut deltas);
                // Every `CommandExecutionStatus` in the schema, named: the
                // wire status is a plain string, so a future CLI version can
                // send a fifth value no arm names — that unknown fails closed
                // to `Error`, never back to a spinner that never stops.
                let status = match (item.status(), item.exit_code()) {
                    ("inProgress", _) => ToolStatus::Running,
                    ("completed", Some(0)) => ToolStatus::Success,
                    ("completed", _) => ToolStatus::Error,
                    ("failed", _) => ToolStatus::Error,
                    ("declined", _) => ToolStatus::Cancelled,
                    (_unknown, _) => ToolStatus::Error,
                };
                let exit_code = item.exit_code().and_then(|code| i32::try_from(code).ok());
                deltas.push(Delta::BlockAdded {
                    turn_id: turn_id.to_owned(),
                    block: Block::ToolCall {
                        id: item.id().to_owned(),
                        kind: ToolKind::Shell,
                        verb: "Ran".into(),
                        target: item.command().to_owned(),
                        status,
                        duration_ms: None,
                        body: ToolBody::Shell {
                            output_lines: Vec::new(),
                            exit_code,
                            live: false,
                        },
                        diff_stat: None,
                    },
                });
            }
            _ => {
                self.ensure_assistant(turn_id, &mut deltas);
                deltas.push(Delta::BlockAdded {
                    turn_id: turn_id.to_owned(),
                    block: Block::Generic {
                        kind: item.kind().to_owned(),
                        status: item.status().to_owned(),
                        text: item.text().to_owned(),
                    },
                });
            }
        }
        deltas
    }

    fn finish_turn(&mut self, notification: &Notification) -> Vec<Delta> {
        let Some(turn_id) = notification.turn_id() else { return Vec::new() };
        let mut deltas = Vec::new();
        // The interrupted path never sends `item/completed` for the streamed
        // message, so flush whatever the delta lane still holds: without this
        // the whole turn folds to zero deltas — not even a `TurnFinished`.
        // Items a completed frame already closed were dropped from the
        // buffer, so this cannot double-render the normal path.
        for text in self.take_pending_delta_text(turn_id) {
            self.ensure_assistant(turn_id, &mut deltas);
            deltas.push(Delta::BlockAdded {
                turn_id: turn_id.to_owned(),
                block: Block::Text { text, streaming: false },
            });
        }
        if !self.assistant_started.contains(turn_id) {
            // No content lane ever opened this turn and no deltas survived
            // it: finishing it would mint an empty turn the transcript never
            // had.
            return deltas;
        }
        let thread_id = notification.thread_id().unwrap_or_default();
        let last = notification
            .turn_id()
            .and_then(|turn| self.usage_for(thread_id, turn))
            .cloned()
            .unwrap_or_default();
        let meta = TurnMeta {
            model: self.model.clone().unwrap_or_default(),
            duration_ms: notification.duration_ms().unwrap_or(0),
            tokens_in: last.input_tokens,
            tokens_out: last.output_tokens,
            reasoning_tokens: last.reasoning_output_tokens,
            // No cost field exists anywhere in this protocol: 0.0 is the
            // neutral unknown default, never a measurement.
            cost_usd: 0.0,
            cache_read_tokens: None,
            cache_write_tokens: None,
            cached_tokens: last.cached_input_tokens,
        };
        deltas.push(Delta::TurnFinished { turn_id: turn_id.to_owned(), meta });
        deltas
    }
}

/// Fold one raw live line: skip blanks and torn JSON, decode, fold, emit.
///
/// Only notifications reach the fold here — the pump routes requests and
/// responses before this runs. The per-line unit of the live pump, factored
/// out so the fixture tests exercise the same decode-and-fold.
pub fn step_line(fold: &mut CodexFold, line: &str, emit: &mut impl FnMut(ProviderEvent)) {
    if line.trim().is_empty() {
        return;
    }
    let Ok(frame) = crate::frame::decode_line(line) else { return };
    if !matches!(frame, Frame::Notification(_)) {
        return;
    }
    let session_id = fold.session_id().map(str::to_owned).or_else(|| {
        if let Frame::Notification(notification) = &frame {
            notification.thread_id().map(str::to_owned)
        } else {
            None
        }
    });
    let deltas = fold.apply(&frame);
    if !deltas.is_empty() {
        emit(ProviderEvent::Deltas { session_id, deltas });
    }
}

/// Whether a recorded envelope line is an `item/agentMessage/delta` frame:
/// the ignored lane the agreement test strips.
#[cfg(test)]
fn is_agent_delta(line: &str) -> bool {
    serde_json::from_str::<Value>(line)
        .ok()
        .and_then(|envelope| envelope.get("frame").cloned())
        .and_then(|frame| frame.get("method").and_then(Value::as_str).map(str::to_owned))
        .as_deref()
        == Some("item/agentMessage/delta")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::decode_envelope;

    fn fixture_lines(name: &str) -> Vec<String> {
        let path = format!("{}/../../fixtures/codex/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(path).expect("fixture reads").lines().map(str::to_owned).collect()
    }

    /// Replay every frame in `name` — both directions, in order — and return
    /// the folded deltas. Client frames contribute nothing; the assertion is
    /// that the whole file replays without loss and folds to its transcript.
    fn replay(name: &str) -> (CodexFold, Vec<Delta>) {
        let mut fold = CodexFold::new();
        let mut deltas = Vec::new();
        for line in fixture_lines(name) {
            let (_, frame) = decode_envelope(&line).expect("fixture decodes");
            deltas.extend(fold.apply(&frame));
        }
        (fold, deltas)
    }

    fn texts(deltas: &[Delta]) -> Vec<&str> {
        deltas
            .iter()
            .filter_map(|delta| match delta {
                Delta::BlockAdded { block: Block::Text { text, .. }, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn basic_replays_end_to_end() {
        let (fold, deltas) = replay("basic.jsonl");
        assert_eq!(
            fold.session_id(),
            Some("01a0d344-59b2-7513-a297-f3015556bce1"),
            "the server-minted session id is stored, never assumed"
        );
        assert_eq!(texts(&deltas), ["READY"], "the turn renders its whole message once");
        for delta in &deltas {
            if let Delta::BlockAdded { block: Block::Text { streaming, .. }, .. } = delta {
                assert!(!streaming, "completed-lane blocks are whole, never streaming");
            }
        }
        let started = deltas.iter().filter(|d| matches!(d, Delta::TurnStarted { .. })).count();
        assert_eq!(started, 2, "one user turn plus one assistant turn");
        let finished: Vec<&TurnMeta> = deltas
            .iter()
            .filter_map(|delta| match delta {
                Delta::TurnFinished { meta, .. } => Some(meta),
                _ => None,
            })
            .collect();
        assert_eq!(finished.len(), 1, "the completed turn finishes once");
        let meta = finished[0];
        assert_eq!(meta.tokens_in, 14918, "per-turn input comes from last, not total");
        assert_eq!(meta.tokens_out, 5);
        assert_eq!(meta.reasoning_tokens, 0);
        assert_eq!(meta.cached_tokens, 10624);
        assert_eq!(meta.duration_ms, 6048);
    }

    /// THE AGREEMENT TEST. The normal-path fixtures carry both lanes
    /// (incremental deltas plus completed repeats); folding one with its
    /// `item/agentMessage/delta` lines stripped must yield deltas identical
    /// to folding it whole — the fallback lane changes nothing there. That is
    /// "agree modulo streaming granularity": the two lanes describe the same
    /// content, and the fold renders it once.
    ///
    /// `interrupt.jsonl` is deliberately NOT in this loop: its streamed text
    /// exists ONLY on the delta lane (no `item/completed` ever closes the
    /// message), so stripping the deltas provably changes its transcript.
    /// The next test pins that survival instead.
    #[test]
    fn delta_and_completed_lanes_agree() {
        for name in ["basic.jsonl", "approval.jsonl"] {
            let lines = fixture_lines(name);
            let (_, whole) = replay(name);
            let stripped: Vec<String> =
                lines.iter().filter(|line| !is_agent_delta(line)).cloned().collect();
            assert!(
                stripped.len() < lines.len(),
                "{name} must actually contain delta lines"
            );
            let mut fold = CodexFold::new();
            let mut without_deltas = Vec::new();
            for line in &stripped {
                let (_, frame) = decode_envelope(line).expect("fixture decodes");
                without_deltas.extend(fold.apply(&frame));
            }
            assert_eq!(
                whole, without_deltas,
                "{name}: the delta lane must not change the transcript"
            );
        }
    }

    /// NO DOUBLE RENDER. The completed text appears exactly once although the
    /// delta lane describes the same content a second time.
    #[test]
    fn each_completed_message_renders_exactly_once() {
        let (_, deltas) = replay("basic.jsonl");
        let readies = texts(&deltas).iter().filter(|text| **text == "READY").count();
        assert_eq!(readies, 1, "READY renders exactly once");
    }

    /// THE INTERRUPTION TEST. `interrupt.jsonl` streams 374 delta chunks of
    /// real text and then ends at `turn/completed` with `status:"interrupted"`
    /// — no `item/completed` ever closes the assistant message. The turn must
    /// still render what it produced: without the fallback lane this replay
    /// folds to a lone user turn, not even a `TurnFinished`.
    #[test]
    fn interrupted_turn_renders_streamed_text() {
        let (_, deltas) = replay("interrupt.jsonl");
        let rendered: String = texts(&deltas).join("");
        assert!(
            rendered.contains("1 — Starting gently."),
            "the first streamed chunk survives: {rendered:.120}…"
        );
        assert!(
            rendered.contains("69 — One"),
            "the last streamed chunk survives: …{tail}",
            tail = rendered.chars().rev().take(120).collect::<String>()
        );
        let finished =
            deltas.iter().filter(|d| matches!(d, Delta::TurnFinished { .. })).count();
        assert_eq!(finished, 1, "the interrupted turn still finishes once");
        let started =
            deltas.iter().filter(|d| matches!(d, Delta::TurnStarted { .. })).count();
        assert_eq!(started, 2, "one user turn plus one assistant turn");
    }

    /// Stripping the delta lane from the interrupted turn provably loses its
    /// only content — the mirror of the agreement test above.
    #[test]
    fn interrupted_turn_without_deltas_renders_nothing() {
        let lines = fixture_lines("interrupt.jsonl");
        let stripped: Vec<String> =
            lines.iter().filter(|line| !is_agent_delta(line)).cloned().collect();
        assert!(stripped.len() < lines.len(), "the fixture must carry delta lines");
        let mut fold = CodexFold::new();
        let mut without_deltas = Vec::new();
        for line in &stripped {
            let (_, frame) = decode_envelope(line).expect("fixture decodes");
            without_deltas.extend(fold.apply(&frame));
        }
        assert!(
            texts(&without_deltas).is_empty(),
            "no completed lane, no content — the delta lane was the only copy"
        );
    }

    #[test]
    fn per_turn_tokens_use_last_not_total() {
        // approval.jsonl updates usage twice for one turn. The first update
        // has total == last; the second splits: total 28645, last 14352. A
        // fold that summed `total` would bill 42938 for one turn.
        let lines = fixture_lines("approval.jsonl");
        let mut fold = CodexFold::new();
        let mut seen = Vec::new();
        for line in &lines {
            let (_, frame) = decode_envelope(line).expect("fixture decodes");
            if let Frame::Notification(notification) = &frame {
                if notification.usage().is_some() {
                    seen.push((
                        notification.thread_id().unwrap_or_default().to_owned(),
                        notification.turn_id().unwrap_or_default().to_owned(),
                    ));
                }
            }
            fold.apply(&frame);
        }
        assert_eq!(seen.len(), 2, "two usage updates pin the split");
        let (thread_id, turn_id) = &seen[1];
        let last = fold.usage_for(thread_id, turn_id).expect("usage recorded");
        assert_eq!(last.total_tokens, 14352, "the per-turn figure is last");
        // And the raw notification proves total differs: the trap is real.
        let raw_total = lines
            .iter()
            .filter_map(|line| decode_envelope(line).ok())
            .filter_map(|(_, frame)| match frame {
                Frame::Notification(notification) => {
                    notification.usage().map(|usage| usage.total.total_tokens)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(raw_total, [14293, 28645], "cumulative totals, never summed");
        assert_ne!(raw_total[1], last.total_tokens, "total != last on the second update");
    }

    #[test]
    fn rate_limit_pushes_feed_the_account_snapshot() {
        // Through the real fold: `CodexFold::apply_notification`'s
        // `RateLimitsUpdated` arm is what feeds the snapshot. Calling
        // `AccountSnapshot::observe` directly would pass even with that arm
        // deleted.
        assert!(
            CodexFold::new().account().label().is_none(),
            "no reading seen, no login claimed"
        );
        let (fold, _) = replay("basic.jsonl");
        let label = fold.account().label().expect("a push was seen");
        assert!(label.contains("19%"), "live meter label: {label}");
    }

    /// Every `CommandExecutionStatus` in the schema maps off the spinner:
    /// `failed` and `declined` must never render as still-executing.
    #[test]
    fn failed_and_declined_commands_do_not_spin() {
        fn fold_status(status: &str) -> ToolStatus {
            let line = format!(
                r#"{{"method":"item/completed","params":{{"threadId":"t","turnId":"u","item":{{"type":"commandExecution","id":"exec-1","command":"ls","status":"{status}","exitCode":1}}}}}}"#
            );
            let frame = crate::frame::decode_line(&line).expect("synthetic line decodes");
            let mut fold = CodexFold::new();
            let deltas = fold.apply(&frame);
            deltas
                .iter()
                .find_map(|delta| match delta {
                    Delta::BlockAdded { block: Block::ToolCall { status, .. }, .. } => {
                        Some(*status)
                    }
                    _ => None,
                })
                .expect("a command execution folds to a shell card")
        }
        assert_eq!(fold_status("failed"), ToolStatus::Error);
        assert_eq!(fold_status("declined"), ToolStatus::Cancelled);
        assert_eq!(fold_status("inProgress"), ToolStatus::Running);
        assert_eq!(fold_status("completed"), ToolStatus::Error, "exit 1 is not success");
    }

    /// Reasoning items in the schema's object shape fold to thinking blocks:
    /// with the extractor fixed, the reasoning arm is reachable again.
    #[test]
    fn reasoning_items_fold_to_thinking_blocks() {
        let line = r#"{"method":"item/completed","params":{"threadId":"t","turnId":"u","item":{"type":"reasoning","id":"rs_1","summary":[{"type":"summary_text","text":"planned"}],"content":[{"type":"reasoning_text","text":"trace"}]}}}"#;
        let frame = crate::frame::decode_line(line).expect("synthetic line decodes");
        let mut fold = CodexFold::new();
        let deltas = fold.apply(&frame);
        let thinking: Vec<&str> = deltas
            .iter()
            .filter_map(|delta| match delta {
                Delta::BlockAdded { block: Block::Thinking { text, .. }, .. } => {
                    Some(text.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(thinking, ["plannedtrace"], "summary plus content, joined");
    }

    /// Approval fixture command executions fold to one shell card, completed
    /// with exit 0.
    #[test]
    fn command_executions_fold_to_shell_cards() {
        let (_, deltas) = replay("approval.jsonl");
        let shells: Vec<(&ToolStatus, _)> = deltas
            .iter()
            .filter_map(|delta| match delta {
                Delta::BlockAdded {
                    block:
                        Block::ToolCall {
                            kind: ToolKind::Shell,
                            status,
                            target,
                            body: ToolBody::Shell { exit_code, .. },
                            ..
                        },
                    ..
                } => Some((status, (target.clone(), *exit_code))),
                _ => None,
            })
            .collect();
        assert_eq!(shells.len(), 1, "one executed command, one card");
        assert_eq!(shells[0].0, &ToolStatus::Success);
        assert_eq!(shells[0].1.1, Some(0));
        assert!(shells[0].1.0.contains("baaz_probe_write.txt"), "target: {}", shells[0].1.0);
    }
}
