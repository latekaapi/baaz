//! Drive one matrix cell through a real [`provider::Provider`] the way
//! Baaz's provider lane does: `OpenSession` carrying the chip's mode,
//! `SubmitInput`, `SelectApprovalMode` for a mid-session switch, and
//! `DecideApproval` for every `ApprovalRequested` tap. The mode the chip
//! would show is the last `ApprovalModeChanged` for the session.
//!
//! Included by `#[path]` next to `live_modes.rs`; see that file for the
//! spend gate.

#![allow(dead_code)]

use std::time::{Duration, Instant};

use aui_protocol::{Block, Delta, PermissionMode, Turn};
use provider::{Ack, Command, Provider, ProviderEvent, SubmissionPart};

use super::live_modes::{probe_prompt, tail, Cell, Outcome, Paths, WARMUP};

/// How a provider spells the two answers the harness gives.
pub struct Choices {
    pub allow: &'static str,
    pub deny: &'static str,
}

/// Per-turn ceiling. A turn that outlives it is recorded, not retried.
const TURN_TIMEOUT: Duration = Duration::from_secs(240);

/// A v4-shaped UUID, as Baaz's `new_command_id` mints: Claude Code takes
/// the open's request id as `--session-id`, which must be a UUID.
fn id() -> String {
    use std::io::Read as _;
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes)).expect("urandom");
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

/// What one turn saw.
#[derive(Default)]
struct TurnSeen {
    asked: Vec<String>,
    answered: Vec<String>,
    text: String,
    finished: bool,
    /// Assistant turns started since the submit. The ack's turn id is not
    /// always the id the fold streams under (Claude Code folds under the
    /// API message id), so a turn that started after the submit and
    /// finished counts as this one.
    started: Vec<String>,
}

/// The session's reported mode, folded from every event drained.
struct Lane<'a> {
    provider: &'a Provider,
    events: crossbeam_channel::Receiver<ProviderEvent>,
    session_id: String,
    reported: Option<PermissionMode>,
    choices: &'a Choices,
    finished_ids: Vec<String>,
}

impl Lane<'_> {
    /// Fold one event; answer an approval with `answer` when it is ours.
    fn fold(&mut self, event: ProviderEvent, turn: &mut TurnSeen, turn_id: &str, deny_next: &mut bool) {
        match event {
            ProviderEvent::ApprovalModeChanged { session_id, mode } => {
                if session_id == self.session_id {
                    self.reported = Some(mode);
                }
            }
            ProviderEvent::ApprovalRequested { session_id, approval_id, headline } => {
                turn.asked.push(headline);
                let choice = if std::mem::take(deny_next) { self.choices.deny } else { self.choices.allow };
                let result = self.provider.send(Command::DecideApproval {
                    request_id: id(),
                    session_id,
                    approval: approval_id,
                    choice: choice.to_owned(),
                    stage_token: None,
                    feedback: None,
                });
                turn.answered.push(match result {
                    Ok(_) => choice.to_owned(),
                    Err(error) => format!("{choice}→ERR {error}"),
                });
            }
            ProviderEvent::Deltas { deltas, .. } => {
                for delta in deltas {
                    match delta {
                        Delta::TurnStarted { turn: started } if matches!(started, Turn::Assistant { .. }) => {
                            turn.started.push(started.id().to_owned());
                        }
                        Delta::TurnFinished { turn_id: finished, .. } => {
                            if !turn_id.is_empty() && (finished == turn_id || turn.started.contains(&finished)) {
                                turn.finished = true;
                            }
                            self.finished_ids.push(finished);
                        }
                        Delta::TextDelta { turn_id: owner, text, .. }
                            if owner == turn_id || turn.started.contains(&owner) =>
                        {
                            turn.text.push_str(&text);
                        }
                        Delta::TurnReplaced { turn: whole }
                            if whole.id() == turn_id || turn.started.iter().any(|id| id == whole.id()) =>
                        {
                            let text = text_of(&whole);
                            if !text.is_empty() {
                                turn.text = text;
                            }
                        }
                        _ => {}
                    }
                }
            }
            ProviderEvent::ConnectionLost { reason } => {
                turn.answered.push(format!("connection lost: {reason}"));
                turn.finished = true;
            }
            ProviderEvent::QuestionRaised { headline, .. } => {
                turn.asked.push(format!("QUESTION {headline}"));
            }
            // Facts beside the transcript: nothing a live mode run reads.
            ProviderEvent::SessionFacts { .. }
            | ProviderEvent::HeaderChanged { .. }
            | ProviderEvent::TurnRunning { .. }
            | ProviderEvent::TurnSettled { .. }
            | ProviderEvent::PromptRestored { .. }
            | ProviderEvent::StatusChanged { .. }
            | ProviderEvent::Notice { .. }
            | ProviderEvent::ConnectionRestored => {}
            // The acknowledged turn's streamed id (Claude Code): its
            // deltas count as this turn's from here on.
            ProviderEvent::TurnIdentified { accepted, turn_id: streamed, .. } => {
                if !turn_id.is_empty() && accepted == turn_id {
                    turn.started.push(streamed);
                }
            }
            // The effort chip's word; the mode matrix reads none of it.
            ProviderEvent::EffortInForce { .. } | ProviderEvent::ModelCatalogChanged { .. } => {}
        }
    }

    /// Drain whatever is queued right now, without waiting for a turn.
    fn settle(&mut self, for_ms: u64) {
        let deadline = Instant::now() + Duration::from_millis(for_ms);
        let mut scratch = TurnSeen::default();
        let mut no = false;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match self.events.recv_timeout(left) {
                Ok(event) => self.fold(event, &mut scratch, "", &mut no),
                Err(_) => break,
            }
        }
    }

    /// Submit `text` and wait for its turn to finish.
    fn turn(&mut self, text: &str, deny_first: bool) -> Result<TurnSeen, String> {
        let ack = self
            .provider
            .send(Command::SubmitInput {
                request_id: id(),
                session_id: self.session_id.clone(),
                parts: vec![SubmissionPart::Text(text.to_owned())],
                display_text: None,
                effort: None,
            })
            .map_err(|error| format!("SubmitInput: {error}"))?;
        let turn_id = match ack {
            Ack::TurnAccepted { turn_id, .. } => turn_id,
            other => return Err(format!("SubmitInput answered {other:?}")),
        };
        let mut seen = TurnSeen::default();
        let mut deny_next = deny_first;
        let deadline = Instant::now() + TURN_TIMEOUT;
        while !seen.finished {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return Err(format!(
                    "turn {turn_id} never finished in {TURN_TIMEOUT:?} (finished ids seen: {:?}); asked {:?}",
                    self.finished_ids, seen.asked
                ));
            };
            if let Ok(event) = self.events.recv_timeout(left.min(Duration::from_millis(500))) {
                self.fold(event, &mut seen, &turn_id, &mut deny_next);
            }
        }
        // Late mode restatements (Codex's settings echo) land after the
        // terminal; give them a moment.
        let mut no = false;
        let until = Instant::now() + Duration::from_millis(1500);
        while let Some(left) = until.checked_duration_since(Instant::now()) {
            match self.events.recv_timeout(left) {
                Ok(event) => self.fold(event, &mut seen, &turn_id, &mut no),
                Err(_) => break,
            }
        }
        Ok(seen)
    }
}

fn text_of(turn: &Turn) -> String {
    turn.blocks()
        .iter()
        .filter_map(|block| match block {
            Block::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Run `cell` on a freshly made, connected provider.
pub fn run_cell(
    provider: &mut Provider,
    cell: &Cell,
    paths: &Paths,
    model: Option<&str>,
    choices: &Choices,
) -> Outcome {
    let mut outcome = Outcome::default();
    let opening = cell.from.unwrap_or(cell.mode);
    let events = provider.events();
    let ack = provider.send(Command::OpenSession {
        request_id: id(),
        workspace: Some(paths.workspace.to_string_lossy().into_owned()),
        model: model.map(str::to_owned),
        model_provider: None,
        mode: Some(opening),
    });
    let session_id = match ack {
        Ok(Ack::Session { session_id, .. }) => session_id,
        Ok(other) => {
            outcome.error = Some(format!("OpenSession answered {other:?}"));
            return outcome;
        }
        Err(error) => {
            outcome.error = Some(format!("OpenSession: {error}"));
            return outcome;
        }
    };
    let mut lane = Lane { provider, events, session_id: session_id.clone(), reported: None, choices, finished_ids: Vec::new() };
    lane.settle(500);
    if let Some(from) = cell.from {
        if lane.reported != Some(from) {
            eprintln!("[{}] opened in {from:?}, reported {:?} before the switch", cell.name, lane.reported);
        }
        match lane.turn(WARMUP, false) {
            Ok(seen) => {
                outcome.turns += 1;
                outcome.warmup_asked = seen.asked.len();
            }
            Err(error) => {
                outcome.turns += 1;
                outcome.error = Some(format!("warm-up: {error}"));
                outcome.reported = lane.reported;
                return outcome;
            }
        }
        if let Err(error) = lane.provider.send(Command::SelectApprovalMode {
            request_id: id(),
            session_id: session_id.clone(),
            mode: cell.mode,
        }) {
            outcome.error = Some(format!("SelectApprovalMode: {error}"));
        }
        lane.settle(1500);
    }
    match lane.turn(&probe_prompt(paths), cell.deny) {
        Ok(seen) => {
            outcome.turns += 1;
            outcome.asked = seen.asked;
            outcome.answered = seen.answered;
            outcome.reply = tail(&seen.text, 160);
        }
        Err(error) => {
            outcome.turns += 1;
            outcome.error = Some(error);
        }
    }
    outcome.reported = lane.reported;
    outcome.inside = paths.inside.exists();
    outcome.outside = paths.outside.exists();
    provider.shutdown();
    outcome
}

/// Open a Read-only session and print the model catalog — no turn, no
/// spend — so the cheapest row can be picked for `BAAZ_LIVE_MODES_MODEL`.
pub fn print_catalog(provider: &mut Provider, workspace: &std::path::Path) {
    let ack = provider.send(Command::OpenSession {
        request_id: id(),
        workspace: Some(workspace.to_string_lossy().into_owned()),
        model: None,
        model_provider: None,
        mode: Some(PermissionMode::DenyUnmatched),
    });
    let session = match ack {
        Ok(Ack::Session { session_id, .. }) => session_id,
        other => panic!("open for the catalog: {other:?}"),
    };
    match provider.send(Command::ListModels { session: Some(session) }) {
        Ok(Ack::ModelCatalog { models, provider: vendor }) => {
            println!("catalog ({vendor}):");
            for row in models {
                println!(
                    "  {} | {} | default={} hidden={} efforts={:?} | {}",
                    row.id,
                    row.label,
                    row.is_default,
                    row.hidden,
                    row.efforts,
                    row.description.unwrap_or_default()
                );
            }
        }
        other => println!("ListModels answered {other:?}"),
    }
    provider.shutdown();
}
