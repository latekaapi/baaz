//! One-shot completions on Muse: the cheap side-session turns behind
//! generated titles, bylines, handoff summaries and drafts.
//!
//! Muse has no one-shot verb, so a completion is a throwaway session: the
//! caller mints its id (and records it, so the row stays hidden even after
//! a crash), [`MuseHub::complete`] starts it in the side workspace on the
//! cheapest model, sends one turn, waits for that turn to settle on a lane
//! of its own, and reads the reply back with a free `session/read`. No
//! other lane hears the side session; nothing routes it through the app.

use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, RecvTimeoutError};
use muse_client::schema::{
    ItemKind, ModelCatalogEntry, ModelListParams, SessionReadParams, SessionReadResult, SessionStartParams,
    TurnInputPart, TurnStartParams,
};
use provider::facts::TurnOutcome;
use provider::{ProviderError, ProviderEvent};

use crate::hub::MuseHub;
use crate::translate::transport_error;

/// The model side sessions are pinned to when the catalog lists it: the
/// non-contributor row. The wire reports `cost: null` for every model, so
/// this is a smallest-appropriate pick by proxy (newest generation) with a
/// privacy basis (the `-contributor` description flags transcript text "may
/// be used for product improvement"), not a priced one.
pub const UTILITY_MODEL_ID: &str = "muse-spark-1.3";

/// The `modelId` for a side session's start: [`UTILITY_MODEL_ID`] when the
/// catalog carries it, else `None` — omit the field and take the server
/// default rather than failing on a model id.
pub fn pick_model(models: &[ModelCatalogEntry]) -> Option<String> {
    models.iter().any(|m| m.model_id == UTILITY_MODEL_ID).then(|| UTILITY_MODEL_ID.to_owned())
}

/// The newest non-empty `agentMessage` text of a `session/read`: inline
/// items, then the snapshot. `None` is an empty reply.
pub fn last_agent_text(read: &SessionReadResult) -> Option<String> {
    let inline = read.history.items.iter().flatten();
    let snapshot = read.history.snapshot.iter().flat_map(|s| s.state.items.iter());
    inline
        .chain(snapshot)
        .filter(|item| item.kind == ItemKind::AgentMessage)
        .filter_map(|item| item.text.clone())
        .rfind(|text| !text.trim().is_empty())
}

impl MuseHub {
    /// Run `prompt` once in a new side session `side_id` (minted and
    /// recorded by the caller) rooted at `workspace`, and return the reply
    /// text. `label` is the turn's display text (`baaz auto-title`, …).
    /// Blocking, up to `deadline`: call it off the UI thread.
    pub fn complete(
        &self,
        side_id: &str,
        workspace: &str,
        prompt: &str,
        label: &str,
        deadline: Duration,
    ) -> Result<String, ProviderError> {
        let until = Instant::now() + deadline;
        let client = self.client()?;
        // Free pre-flight: the cheapest model when listed.
        let models = client.model_list(&ModelListParams { session_id: None }).map(|r| r.models).unwrap_or_default();
        // Bound before the start, so nothing the turn streams is missed.
        let (tx, rx) = unbounded();
        let _bound = self.bind_private(side_id, tx);
        client
            .session_start(&SessionStartParams {
                command_id: provider::new_request_id(),
                model_id: pick_model(&models),
                session_id: Some(side_id.to_owned()),
                workspace_root: Some(workspace.to_owned()),
                ..Default::default()
            })
            .map_err(transport_error)?;
        client
            .turn_start(&TurnStartParams {
                command_id: provider::new_request_id(),
                session_id: side_id.to_owned(),
                input: vec![TurnInputPart::text(prompt)],
                display_text: Some(label.to_owned()),
                ..Default::default()
            })
            .map_err(transport_error)?;
        loop {
            let left = until.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(ProviderEvent::TurnSettled { session_id, outcome, .. }) if session_id == side_id => match outcome {
                    TurnOutcome::Failed { message, .. } => {
                        return Err(ProviderError::Rejected { reason: message.unwrap_or_else(|| "the turn failed".into()) })
                    }
                    _ => break,
                },
                Ok(ProviderEvent::ConnectionLost { reason }) => return Err(ProviderError::Unavailable { reason }),
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => {
                    return Err(ProviderError::Unavailable { reason: format!("no answer in {} s", deadline.as_secs()) })
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(ProviderError::Unavailable { reason: "Muse went away".into() })
                }
            }
        }
        let read = client
            .session_read(&SessionReadParams { session_id: side_id.to_owned(), exclude_items: Some(false) })
            .map_err(transport_error)?;
        last_agent_text(&read).ok_or_else(|| ProviderError::Rejected { reason: "empty reply".into() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model_row(id: &str) -> ModelCatalogEntry {
        serde_json::from_value(serde_json::json!({
            "modelId": id,
            "displayLabel": id,
            "isActive": false,
            "isDefault": false,
            "providerId": "meta",
        }))
        .expect("a model row deserialises")
    }

    fn read_with(texts: impl IntoIterator<Item = &'static str>) -> SessionReadResult {
        let items: Vec<serde_json::Value> = texts
            .into_iter()
            .map(|text| {
                serde_json::json!({"itemId": "i", "kind": "agentMessage", "revision": 1, "status": "completed", "text": text})
            })
            .collect();
        serde_json::from_value(serde_json::json!({
            "history": { "items": items, "mode": "inline" },
            "pendingRequests": [],
            "session": {
                "sessionId": "side",
                "path": "",
                "createdAt": "2026-09-16T00:00:00Z",
                "updatedAt": "2026-09-16T00:00:00Z",
                "turnCount": 1,
                "status": "idle",
            },
            "viewCursor": "c",
        }))
        .expect("a read result deserialises")
    }

    #[test]
    fn the_pinned_model_wins_when_listed_and_yields_otherwise() {
        let listed = vec![
            model_row("muse-spark-1.2"),
            model_row(UTILITY_MODEL_ID),
            model_row("muse-spark-1.3-contributor"),
        ];
        assert_eq!(pick_model(&listed), Some(UTILITY_MODEL_ID.to_owned()));
        assert_eq!(pick_model(&[model_row("muse-spark-1.2")]), None);
        assert_eq!(pick_model(&[]), None);
    }

    #[test]
    fn the_reply_is_the_newest_non_blank_agent_text_verbatim() {
        let multi = "Did X.\nDecided Y.\nNow at Z.\nLeft: W.";
        assert_eq!(last_agent_text(&read_with(["queued reply", multi])), Some(multi.to_owned()));
        assert_eq!(last_agent_text(&read_with(["Tighten validation", "   "])), Some("Tighten validation".into()));
        assert_eq!(last_agent_text(&read_with([])), None);
    }
}
