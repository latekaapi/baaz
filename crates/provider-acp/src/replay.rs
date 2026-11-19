//! An ACP recording folded with no child: what `--replay` opens and what
//! the fixtures' own tests read (docs/82 5.7).
//!
//! A recording (`fixtures/acp/*.jsonl`) holds both directions of one
//! connection, one JSON-RPC message per line in an envelope that says which
//! way it went: `{"_dir": "client->agent" | "agent->client", "frame": …}`
//! (`#` lines are comments). Folding plays both halves the way a live lane
//! would: the client's prompt opens its turn, the agent's updates fill it,
//! its permission request raises the card and the client's answer settles
//! it, and the prompt's answer ends the turn — through the same
//! [`AcpFold`] the live pump drives.

use std::collections::{HashMap, HashSet};

use provider::{ProviderEvent, Replay};
use serde_json::Value;

use crate::fold::{AcpFold, Ending};
use crate::wire::{self, Frame, StopReason};

/// One recorded step.
#[derive(Clone, Debug)]
pub enum Recorded {
    /// A message, and the way it went.
    Message {
        /// The client sent it (else the agent did).
        from_client: bool,
        /// The message.
        frame: Frame,
    },
    /// The agent died here (`{"_dir": "agent->client", "exit": …,
    /// "stderr": […]}`), saying `stderr` last.
    Exit {
        /// Its last stderr lines.
        stderr: Vec<String>,
    },
}

/// A recording's messages, in order; `None` when `text` is not an ACP
/// recording (no envelope carries a session update or a prompt).
pub fn read(text: &str) -> Option<Vec<Recorded>> {
    let mut recorded = Vec::new();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')) {
        let Ok(envelope) = serde_json::from_str::<Value>(line) else { continue };
        let from_client = match envelope.get("_dir").and_then(Value::as_str) {
            Some("client->agent") => true,
            Some("agent->client") => false,
            _ => continue,
        };
        if !from_client && envelope.get("exit").is_some() {
            let stderr = envelope
                .get("stderr")
                .and_then(Value::as_array)
                .map(|lines| lines.iter().filter_map(Value::as_str).map(str::to_owned).collect())
                .unwrap_or_default();
            recorded.push(Recorded::Exit { stderr });
            continue;
        }
        let Some(frame) = envelope.get("frame").cloned().and_then(wire::frame_of) else { continue };
        recorded.push(Recorded::Message { from_client, frame });
    }
    let acp = recorded.iter().any(|step| {
        matches!(step, Recorded::Message { frame: Frame::Notification { method, .. } | Frame::Request { method, .. }, .. }
            if method == wire::SESSION_UPDATE || method == wire::SESSION_PROMPT)
    });
    acp.then_some(recorded)
}

/// `text`, folded: the events a live lane would have sent for it.
pub fn fold_recording(text: &str) -> Option<Replay> {
    let recorded = read(text)?;
    let mut fold = AcpFold::new("", "");
    let mut session: Option<String> = None;
    let mut cwd = String::new();
    let mut events: Vec<ProviderEvent> = Vec::new();
    let mut sent = Vec::new();
    // Requests in flight by id: what each one was.
    let mut asked: HashMap<String, String> = HashMap::new();
    let mut prompts: HashMap<String, String> = HashMap::new();
    let mut permissions: HashMap<String, String> = HashMap::new();
    let mut decided: HashSet<String> = HashSet::new();
    for step in recorded {
        let (from_client, frame) = match step {
            Recorded::Message { from_client, frame } => (from_client, frame),
            Recorded::Exit { stderr } => {
                let reason = match stderr.last() {
                    Some(line) => format!("{}: {}", crate::child::CHILD_EXITED_REASON, line.trim()),
                    None => crate::child::CHILD_EXITED_REASON.to_owned(),
                };
                fold.lost(&reason);
                flush(&mut fold, &mut events);
                events.push(ProviderEvent::ConnectionLost { reason });
                break;
            }
        };
        match (from_client, frame) {
            (true, Frame::Request { id, method, params }) => {
                let key = id.to_string();
                match method.as_str() {
                    wire::SESSION_NEW => cwd = params.get("cwd").and_then(Value::as_str).unwrap_or_default().to_owned(),
                    wire::SESSION_LOAD => {
                        cwd = params.get("cwd").and_then(Value::as_str).unwrap_or_default().to_owned();
                        let id = wire::session_of(&params).unwrap_or_default().to_owned();
                        fold = AcpFold::new(&id, &cwd);
                        fold.begin_history();
                        session.get_or_insert(id);
                    }
                    wire::SESSION_PROMPT => {
                        let turn = format!("turn-{}", sent.len() + 1);
                        let words = prompt_words(&params);
                        fold.submitted(&turn, &words, None);
                        flush(&mut fold, &mut events);
                        events.push(ProviderEvent::TurnRunning { session_id: fold.session_id().to_owned(), turn_id: turn.clone() });
                        prompts.insert(key.clone(), turn.clone());
                        sent.push((turn, words));
                    }
                    _ => {}
                }
                asked.insert(key, method);
            }
            // The client's answer to a permission request.
            (true, Frame::Response { id, result }) => {
                let Some(approval) = permissions.remove(&id.to_string()) else { continue };
                decided.insert(approval.clone());
                let outcome = result.get("outcome").cloned().unwrap_or(Value::Null);
                match outcome.get("optionId").and_then(Value::as_str) {
                    Some(option) if outcome.get("outcome").and_then(Value::as_str) == Some("selected") => {
                        if let Some(kind) = fold.option_kind(&approval, option) {
                            fold.decided(&approval, kind);
                        }
                    }
                    _ => fold.abandoned(&approval, "The turn was stopped before it was answered."),
                }
            }
            (false, Frame::Response { id, result }) => {
                let key = id.to_string();
                match asked.remove(&key).as_deref() {
                    Some(wire::SESSION_NEW) => {
                        let id = result.get("sessionId").and_then(Value::as_str).unwrap_or_default().to_owned();
                        fold = AcpFold::new(&id, &cwd);
                        session.get_or_insert(id);
                    }
                    Some(wire::SESSION_LOAD) => fold.end_history(),
                    Some(wire::SESSION_PROMPT) => {
                        prompts.remove(&key);
                        settle(&mut fold, &Ending::Stopped(StopReason::of(&result)), &mut events);
                    }
                    _ => {}
                }
            }
            (false, Frame::Error { id, code, message }) => {
                let key = id.to_string();
                if asked.remove(&key).as_deref() == Some(wire::SESSION_PROMPT) {
                    prompts.remove(&key);
                    settle(&mut fold, &Ending::Failed { code, message }, &mut events);
                }
            }
            (false, Frame::Request { id, method, params }) if method == wire::REQUEST_PERMISSION => {
                let approval = params
                    .get("toolCall")
                    .and_then(|call| call.get("toolCallId"))
                    .and_then(Value::as_str)
                    .map_or_else(|| format!("rpc-{id}"), str::to_owned);
                if let Some((headline, options)) = fold.permission(&approval, &params) {
                    flush(&mut fold, &mut events);
                    events.push(ProviderEvent::ApprovalRequested {
                        session_id: fold.session_id().to_owned(),
                        approval_id: approval.clone(),
                        headline,
                        options,
                    });
                    permissions.insert(id.to_string(), approval);
                }
            }
            (false, Frame::Notification { method, params }) if method == wire::SESSION_UPDATE => {
                fold.update(params.get("update").unwrap_or(&Value::Null), &mut events);
            }
            _ => {}
        }
        flush(&mut fold, &mut events);
    }
    // A tap whose answer the recording holds is not waiting on anyone.
    events.retain(|event| !matches!(event, ProviderEvent::ApprovalRequested { approval_id, .. } if decided.contains(approval_id)));
    Some(Replay { session_id: session, sent, events })
}

/// The words of a prompt's text blocks.
fn prompt_words(params: &Value) -> String {
    params
        .get("prompt")
        .and_then(Value::as_array)
        .map(|blocks| blocks.iter().filter_map(|block| block.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n"))
        .unwrap_or_default()
}

fn flush(fold: &mut AcpFold, events: &mut Vec<ProviderEvent>) {
    let deltas = fold.take();
    if !deltas.is_empty() {
        events.push(ProviderEvent::Deltas { session_id: Some(fold.session_id().to_owned()), deltas });
    }
}

fn settle(fold: &mut AcpFold, ending: &Ending, events: &mut Vec<ProviderEvent>) {
    let ended = fold.finish(ending);
    flush(fold, events);
    if let Some((turn_id, outcome)) = ended {
        events.push(ProviderEvent::TurnSettled { session_id: fold.session_id().to_owned(), turn_id, outcome });
    }
}
