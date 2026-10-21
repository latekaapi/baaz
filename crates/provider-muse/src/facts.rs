//! What the Muse fold knows beyond deltas, as neutral provider events.
//!
//! After every folded event the adapter asks [`FactTracker::after`] what
//! moved: the session's side facts, its header, its approval mode, and the
//! MSP notifications whose meaning is not transcript — a turn starting or
//! settling, a prompt handed back, a row's status. Facts go out only when
//! they changed since the last ones sent for that session.

use std::collections::HashMap;

use aui_protocol::PermissionMode;
use muse_adapter::MuseFold;
use provider::facts::{SessionHeader, SessionSide, TurnOutcome};
use provider::ProviderEvent;
use serde_json::Value;

/// What was last sent for one session.
#[derive(Default)]
struct Sent {
    side: Option<SessionSide>,
    header: Option<SessionHeader>,
    mode: Option<PermissionMode>,
    stored_gen: Option<u64>,
}

/// Per-session memory of the facts already sent.
#[derive(Default)]
pub struct FactTracker {
    sent: HashMap<String, Sent>,
}

impl FactTracker {
    /// A tracker that has sent nothing.
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget a session: its next facts go out whole.
    pub fn forget(&mut self, session_id: &str) {
        self.sent.remove(session_id);
    }

    /// The events one folded notification implies, beyond its deltas:
    /// `method`/`params` are the notification's, already folded into `fold`.
    pub fn after(&mut self, fold: &mut MuseFold, session_id: &str, method: &str, params: &Value) -> Vec<ProviderEvent> {
        let mut out = Vec::new();
        let turn_id = || params.get("turnId").and_then(Value::as_str).map(str::to_owned);
        match method {
            "turn/started" => {
                if let Some(turn_id) = turn_id() {
                    out.push(ProviderEvent::TurnRunning { session_id: session_id.to_owned(), turn_id });
                }
            }
            "turn/completed" => {
                if let Some(turn_id) = turn_id() {
                    out.push(ProviderEvent::TurnSettled {
                        session_id: session_id.to_owned(),
                        turn_id,
                        outcome: outcome(params),
                    });
                }
            }
            "turn/retracted" | "turn/unqueued" => {
                if let Some(text) = fold.take_restored_prompt(session_id) {
                    out.push(ProviderEvent::PromptRestored {
                        session_id: session_id.to_owned(),
                        turn_id: if method == "turn/unqueued" { turn_id() } else { None },
                        text,
                    });
                }
            }
            "session/statusChanged" => {
                let running = params.get("status").and_then(Value::as_str) == Some("running");
                let attention = params
                    .get("attention")
                    .and_then(Value::as_array)
                    .map(|flags| flags.iter().filter_map(Value::as_str).map(str::to_owned).collect())
                    .unwrap_or_default();
                out.push(ProviderEvent::StatusChanged { session_id: session_id.to_owned(), running, attention });
            }
            _ => {}
        }
        out.extend(self.changed(fold, session_id));
        out
    }

    /// The side, header and mode of `session_id` where they differ from what
    /// was last sent.
    pub fn changed(&mut self, fold: &mut MuseFold, session_id: &str) -> Vec<ProviderEvent> {
        let mut out = Vec::new();
        let Some(session) = fold.session(session_id) else { return out };
        let header = SessionHeader { model: session.model.clone(), cwd: session.cwd.clone(), branch: session.branch.clone() };
        let mode = session.mode;
        let sent = self.sent.entry(session_id.to_owned()).or_default();
        if sent.header.as_ref() != Some(&header) {
            sent.header = Some(header.clone());
            out.push(ProviderEvent::HeaderChanged { session_id: session_id.to_owned(), header });
        }
        if sent.mode != Some(mode) {
            sent.mode = Some(mode);
            out.push(ProviderEvent::ApprovalModeChanged { session_id: session_id.to_owned(), mode });
        }
        let Some(side) = fold.side(session_id) else { return out };
        let mut side = muse_adapter::neutral_side(side);
        side.stored_outputs = match fold.stored_outputs(session_id) {
            Some((generation, refs)) if sent.stored_gen != Some(generation) => {
                sent.stored_gen = Some(generation);
                refs.map(|(item, reference)| (item.to_owned(), reference.to_owned())).collect()
            }
            _ => sent.side.as_ref().map(|side| side.stored_outputs.clone()).unwrap_or_default(),
        };
        if sent.side.as_ref() != Some(&side) {
            sent.side = Some(side.clone());
            out.push(ProviderEvent::SessionFacts { session_id: session_id.to_owned(), side });
        }
        out
    }
}

/// How a `turn/completed` ended, from its `terminal` and `error`.
fn outcome(params: &Value) -> TurnOutcome {
    match params.get("terminal").and_then(Value::as_str) {
        Some("failed") => {
            let error = params.get("error");
            let kind = error.and_then(|e| e.get("kind")).and_then(Value::as_str);
            let message = error.and_then(|e| e.get("message")).and_then(Value::as_str).map(str::to_owned);
            let signed_out = crate::errors::looks_like_signed_out(kind, message.as_deref().unwrap_or_default());
            TurnOutcome::Failed { message, signed_out }
        }
        Some("interrupted" | "cancelled") => TurnOutcome::Interrupted,
        _ => TurnOutcome::Completed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use muse_client::MuseEvent;

    fn notify(fold: &mut MuseFold, method: &str, params: Value) -> (String, Value) {
        fold.apply(MuseEvent::Notification {
            method: method.to_owned(),
            params: params.clone(),
            cursor: Some(format!("c-{method}")),
            session_id: Some("s1".into()),
        });
        (method.to_owned(), params)
    }

    #[test]
    fn a_turn_runs_settles_and_its_failure_can_mean_signed_out() {
        let mut fold = MuseFold::new();
        let mut tracker = FactTracker::new();
        let (m, p) = notify(&mut fold, "turn/started", serde_json::json!({"sessionId": "s1", "turnId": "t1"}));
        let events = tracker.after(&mut fold, "s1", &m, &p);
        assert!(events.contains(&ProviderEvent::TurnRunning { session_id: "s1".into(), turn_id: "t1".into() }));
        let (m, p) = notify(
            &mut fold,
            "turn/completed",
            serde_json::json!({"sessionId": "s1", "turnId": "t1", "terminal": "failed",
                "error": {"kind": "modelError", "message": "meta is not authenticated"}}),
        );
        let events = tracker.after(&mut fold, "s1", &m, &p);
        assert!(events.iter().any(|event| matches!(event,
            ProviderEvent::TurnSettled { outcome: TurnOutcome::Failed { signed_out: true, .. }, .. })));
    }

    #[test]
    fn facts_go_out_once_until_they_move() {
        let mut fold = MuseFold::new();
        let mut tracker = FactTracker::new();
        let (m, p) = notify(&mut fold, "turn/started", serde_json::json!({"sessionId": "s1", "turnId": "t1"}));
        let first = tracker.after(&mut fold, "s1", &m, &p);
        assert!(first.iter().any(|event| matches!(event, ProviderEvent::SessionFacts { .. })));
        assert!(tracker.changed(&mut fold, "s1").is_empty(), "nothing moved since");
        let (m, p) = notify(
            &mut fold,
            "session/contextUsage",
            serde_json::json!({"sessionId": "s1", "pressure": "warning", "usedTokens": 900, "windowTokens": 1000}),
        );
        let moved = tracker.after(&mut fold, "s1", &m, &p);
        let side = moved.iter().find_map(|event| match event {
            ProviderEvent::SessionFacts { side, .. } => Some(side.clone()),
            _ => None,
        });
        assert_eq!(side.and_then(|side| side.context).map(|c| c.used_tokens), Some(900));
    }

    #[test]
    fn a_reclaimed_queue_row_hands_its_words_back_with_its_turn() {
        let mut fold = MuseFold::new();
        let mut tracker = FactTracker::new();
        fold.record_queued("s1", "t-q", "c-q", "later, please");
        let (m, p) = notify(
            &mut fold,
            "turn/unqueued",
            serde_json::json!({"sessionId": "s1", "turnId": "t-q", "commandId": "c-q"}),
        );
        let events = tracker.after(&mut fold, "s1", &m, &p);
        assert!(events.contains(&ProviderEvent::PromptRestored {
            session_id: "s1".into(),
            turn_id: Some("t-q".into()),
            text: "later, please".into(),
        }));
    }
}
