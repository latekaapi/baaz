//! Muse's fold, mirrored into the neutral [`SessionStore`] a view renders.
//!
//! The fold is the MSP translator: it keeps its own copy of each session
//! because MSP addresses items by slots the deltas do not carry. What a view
//! draws is the store, so every fold step is mirrored: the deltas it
//! returned, the header fields it writes in place (model, mode, branch,
//! workspace), the turn clocks it refines without a delta, and its side
//! state translated to [`SessionSide`].

use provider::facts::{
    ContextPressure, ContextReading, ModelInEffect, PendingPrompt, PromptQuestion, QueuedTurn,
    RetryPlan, SessionSide, TokenTotals,
};
use session_store::SessionStore;

use aui_protocol::Delta;
use muse_client::schema::{ContextPressureLevel, UserInputSelectionMode};

use crate::{MuseFold, SideState};

/// A [`MuseFold`] side state, neutrally. Stored outputs are not here: they
/// live beside the side state in the fold (see [`Mirror::sync`]).
pub fn neutral_side(side: &SideState) -> SessionSide {
    SessionSide {
        context: side.context.as_ref().map(|usage| ContextReading {
            used_tokens: usage.used_tokens,
            window_tokens: usage.window_tokens,
            pressure: match usage.pressure {
                ContextPressureLevel::Warning => ContextPressure::Warning,
                ContextPressureLevel::Blocked => ContextPressure::Blocked,
                _ => ContextPressure::Normal,
            },
        }),
        totals: TokenTotals {
            prompt_tokens: side.cumulative.prompt_tokens,
            output_tokens: side.cumulative.output_tokens,
            total_tokens: side.cumulative.total_tokens,
        },
        queued: side
            .queued
            .iter()
            .map(|queued| QueuedTurn { turn_id: queued.turn_id.clone(), request_id: queued.command_id.clone() })
            .collect(),
        retry: side.retry.as_ref().map(|retry| RetryPlan {
            turn_id: retry.turn_id.clone(),
            attempt: retry.attempt,
            max_attempts: retry.max_attempts,
            delay_ms: retry.retry_delay_ms,
            reason: retry.reason.clone(),
        }),
        pending_approvals: side
            .pending_approvals
            .iter()
            .map(|(id, request)| (id.clone(), crate::stage_token(&request.current_requirement_id)))
            .collect(),
        pending_prompts: side
            .pending_inputs
            .iter()
            .map(|(id, request)| {
                let questions = request
                    .questions
                    .iter()
                    .map(|question| PromptQuestion {
                        id: question.id.clone(),
                        header: question.header.clone(),
                        multi: matches!(question.selection.mode, UserInputSelectionMode::Multiple),
                        labels: question.options.iter().map(|option| option.label.clone()).collect(),
                    })
                    .collect();
                (id.clone(), PendingPrompt { questions, auto_resolve_ms: request.auto_resolution_ms })
            })
            .collect(),
        model: side.model.as_ref().map(|model| ModelInEffect { id: model.model_id.clone() }),
        cursor: side.last_cursor.clone(),
        stored_outputs: Default::default(),
    }
}

/// Mirrors one session of a [`MuseFold`] into a [`SessionStore`].
#[derive(Debug, Default)]
pub struct Mirror {
    /// The fold's stored-output generation last copied, so the map is only
    /// rebuilt when it moved.
    stored_gen: Option<u64>,
}

impl Mirror {
    /// A mirror that has copied nothing yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Mirror a session the fold holds whole — a capture folded in one go —
    /// into `store`, replacing its transcript.
    pub fn sync_whole(&mut self, fold: &mut MuseFold, session_id: &str, store: &mut SessionStore) {
        let Some(session) = fold.session(session_id) else { return };
        store.rename(session_id);
        store.load(session.clone());
        let _ = fold.take_dirty(session_id);
        self.stored_gen = None;
        self.sync(fold, session_id, Vec::new(), store);
    }

    /// Mirror one fold step: `deltas` are what the step returned. Returns the
    /// deltas that landed in the store. Does nothing while the fold has no
    /// such session.
    pub fn sync(&mut self, fold: &mut MuseFold, session_id: &str, deltas: Vec<Delta>, store: &mut SessionStore) -> Vec<Delta> {
        let Some(session) = fold.session(session_id) else { return Vec::new() };
        store.sync_header(session);
        let landed = store.apply_deltas(deltas);
        // The fold refines a turn's clock in place (the earliest item stamp),
        // with no delta: copy it for every turn it touched.
        match fold.take_dirty(session_id) {
            Some(dirty) => {
                if let Some(session) = fold.session(session_id) {
                    store.sync_timestamps(session, dirty.iter().map(String::as_str));
                }
            }
            None => {
                if let Some(session) = fold.session(session_id) {
                    let ids: Vec<String> = session.turns.iter().map(|turn| turn.id().to_owned()).collect();
                    store.sync_timestamps(session, ids.iter().map(String::as_str));
                }
            }
        }
        if let Some(side) = fold.side(session_id) {
            let mut neutral = neutral_side(side);
            let previous = std::mem::take(&mut store.side_mut().stored_outputs);
            neutral.stored_outputs = match fold.stored_outputs(session_id) {
                Some((generation, refs)) if self.stored_gen != Some(generation) => {
                    self.stored_gen = Some(generation);
                    refs.map(|(item, reference)| (item.to_owned(), reference.to_owned())).collect()
                }
                _ => previous,
            };
            store.set_side(neutral);
        }
        landed
    }
}
