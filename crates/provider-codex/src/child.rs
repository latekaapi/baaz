//! The child process lane: one long-lived `codex app-server` per session.
//!
//! The child speaks newline-delimited JSON-RPC 2.0 over stdio, and it is
//! bidirectional: besides answering our requests it sends requests to us
//! (approvals, questions, tool calls) that we must answer. The pump thread
//! routes each incoming line by shape: responses complete a pending
//! [`RunningChild::send_request`], server requests are surfaced as
//! [`provider::ProviderEvent`]s (or refused with a JSON-RPC error when Baaz
//! cannot serve them), and notifications run the shared per-line fold path
//! from [`crate::fold`] — the same function the fixture tests use, so the
//! live path and the tested path cannot drift apart.
//!
//! Nothing here runs in tests: spawning `codex` spends the owner's money, so
//! every test in this module stays offline against the checked-in fixtures
//! and the pure request builders below.
//!
//! The opening sequence, exactly as captured in
//! `fixtures/codex/basic.jsonl`, is built by [`initialize_request`],
//! [`initialized_notification`], [`thread_start_request`] and
//! [`turn_start_request`]: `thread/start` always carries an explicit `model`
//! (an account that rejects the owner's configured default fails the turn
//! otherwise), and the model comes from [`model_ids`] over the `model/list`
//! response — note the key is `data`, not `models`.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam_channel::{unbounded, Receiver, Sender};
use provider::ProviderEvent;
use serde_json::{json, Value};

use crate::fold::{step_line, CodexFold};
use crate::frame::{decode_line, Frame};

/// How long [`RunningChild::send_request`] waits for the server's answer
/// before giving up. The wire is local stdio, so two minutes is generous, not
/// tight: expiry means the child is wedged, and hanging the caller forever
/// would be worse.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// A server approval request waiting for a decision: the JSON-RPC id to
/// answer plus what the model said it wants, in its own words.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingApproval {
    /// The server request id, echoed in our answer.
    request_id: Value,
    /// The owning thread, for event attribution.
    thread_id: String,
    /// The wire item id (`exec-…`), which [`provider::Command::DecideApproval`]
    /// names.
    item_id: String,
    /// The model's human sentence justifying the request.
    headline: String,
}

/// One pending approval, pointed at — not the full card. Enough to find it
/// and decide it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingApprovalView {
    /// The wire item id.
    pub item_id: String,
    /// The owning thread.
    pub thread_id: String,
    /// The model's human sentence justifying the request.
    pub headline: String,
}

/// The answer lane for one in-flight request: the `result`, or the wire
/// error message.
type Answer = Result<Value, String>;

/// What the pump and the request lane share.
#[derive(Debug, Default)]
struct Shared {
    /// Response waiters by request id (`id.to_string()`).
    waiters: HashMap<String, Sender<Answer>>,
    /// Approval requests waiting for a decision, by wire item id.
    approvals: HashMap<String, PendingApproval>,
}

/// A request that never got its answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestError {
    /// What went wrong.
    pub reason: String,
}

impl std::fmt::Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "codex request failed: {}", self.reason)
    }
}

impl std::error::Error for RequestError {}

/// The client's `initialize` call. `id` is caller-chosen (1 in the fixture);
/// the server answers with its user agent, codex home, and platform.
pub fn initialize_request(id: u64, client_version: &str) -> Value {
    json!({
        "id": id,
        "method": "initialize",
        "params": {
            "clientInfo": {"name": "baaz", "title": "Baaz", "version": client_version},
            "capabilities": {"experimentalApi": true}
        }
    })
}

/// The `initialized` notification completing the handshake. No id: the
/// server sends nothing back.
pub fn initialized_notification() -> Value {
    json!({"method": "initialized", "params": {}})
}

/// The `thread/start` call. `model` is always explicit: an account that
/// rejects the owner's configured default fails the turn otherwise, so there
/// is no "server default" lane. Baaz cannot choose the thread id — the
/// server mints it and returns it (see [`thread_ids`]).
pub fn thread_start_request(id: u64, cwd: &str, model: &str) -> Value {
    json!({
        "id": id,
        "method": "thread/start",
        "params": {"cwd": cwd, "model": model}
    })
}

/// The `turn/start` call carrying one text input.
pub fn turn_start_request(id: u64, thread_id: &str, model: &str, text: &str) -> Value {
    json!({
        "id": id,
        "method": "turn/start",
        "params": {
            "threadId": thread_id,
            "model": model,
            "input": [{"type": "text", "text": text}]
        }
    })
}

/// The `turn/steer` call injecting input into the running turn.
/// `expected_turn_id` closes the race where the turn changes under us.
pub fn turn_steer_request(id: u64, thread_id: &str, expected_turn_id: &str, text: &str) -> Value {
    json!({
        "id": id,
        "method": "turn/steer",
        "params": {
            "threadId": thread_id,
            "expectedTurnId": expected_turn_id,
            "input": [{"type": "text", "text": text}]
        }
    })
}

/// The `turn/interrupt` call stopping the named turn.
pub fn turn_interrupt_request(id: u64, thread_id: &str, turn_id: &str) -> Value {
    json!({
        "id": id,
        "method": "turn/interrupt",
        "params": {"threadId": thread_id, "turnId": turn_id}
    })
}

/// The model catalog out of a `model/list` response. The key is `data`, not
/// `models`: `result.data[].id`, in provider order.
pub fn model_ids(result: &Value) -> Vec<String> {
    result
        .get("data")
        .and_then(Value::as_array)
        .map(|data| {
            data.iter()
                .filter_map(|row| row.get("id").and_then(Value::as_str).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// The default model out of a `model/list` response: the row flagged
/// `isDefault`, else the first row, else none.
pub fn default_model(result: &Value) -> Option<String> {
    let data = result.get("data").and_then(Value::as_array)?;
    data.iter()
        .find(|row| row.get("isDefault").and_then(Value::as_bool).unwrap_or(false))
        .or_else(|| data.first())
        .and_then(|row| row.get("id").and_then(Value::as_str).map(str::to_owned))
}

/// The minted `(thread_id, session_id)` out of a `thread/start` response.
/// Equal on a fresh thread; the session mapping stores both rather than
/// assuming either.
pub fn thread_ids(result: &Value) -> Option<(String, String)> {
    let thread = result.get("thread")?;
    Some((
        thread.get("id").and_then(Value::as_str)?.to_owned(),
        thread.get("sessionId").and_then(Value::as_str)?.to_owned(),
    ))
}

/// The started turn id out of a `turn/start` response.
pub fn turn_id_from(result: &Value) -> Option<String> {
    result.get("turn").and_then(|turn| turn.get("id")).and_then(Value::as_str).map(str::to_owned)
}

/// An approval decision, typed so the wire token can never be a string
/// literal. The token is `accept`: replying `approved` is silently treated
/// as a refusal (`status:"declined"` in the log, no schema error), which is
/// the safe direction but indistinguishable from a real denial.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalDecision {
    /// Run it, once.
    Accept,
    /// Run it and stop asking, session-scoped.
    AcceptForSession,
    /// Refuse; the turn continues.
    Decline,
    /// Refuse; the turn is interrupted.
    Cancel,
}

impl ApprovalDecision {
    /// The wire token.
    pub fn wire(&self) -> &'static str {
        match self {
            ApprovalDecision::Accept => "accept",
            ApprovalDecision::AcceptForSession => "acceptForSession",
            ApprovalDecision::Decline => "decline",
            ApprovalDecision::Cancel => "cancel",
        }
    }
}

/// The `result` answering an approval request: `{"decision": <token>}`.
pub fn decision_result(decision: ApprovalDecision) -> Value {
    json!({"decision": decision.wire()})
}

fn write_value(stdin: &Arc<Mutex<ChildStdin>>, value: &Value) -> std::io::Result<()> {
    let line = serde_json::to_string(value)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    let mut stdin = stdin.lock().map_err(|_| {
        std::io::Error::other("the session child's stdin lock is poisoned")
    })?;
    stdin.write_all(line.as_bytes())?;
    stdin.write_all(b"\n")?;
    stdin.flush()
}

/// A running session child: request/response calls in, a pump thread for
/// everything the server sends unprompted.
pub struct RunningChild {
    child: Child,
    stdin: Arc<Mutex<ChildStdin>>,
    next_id: Mutex<u64>,
    shared: Arc<Mutex<Shared>>,
    pump: Option<std::thread::JoinHandle<()>>,
}

impl RunningChild {
    /// Spawn `codex app-server` and pump its stdout: responses complete
    /// pending [`Self::send_request`] calls, server approval/question
    /// requests become [`ProviderEvent`]s, unservable server requests get an
    /// honest JSON-RPC error, and notifications run the shared fold path.
    /// The fold is shared with the adapter (for `ReadAccount`) and locked
    /// one line at a time, never for the whole stream.
    /// Blocking: run it on the background executor.
    pub fn spawn(
        program: &str,
        fold: Arc<Mutex<CodexFold>>,
        events: Sender<ProviderEvent>,
    ) -> std::io::Result<Self> {
        let mut child = Command::new(program)
            .arg("app-server")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdout = child.stdout.take().expect("stdout piped");
        let stdin_raw = child.stdin.take().expect("stdin piped");
        let stdin = Arc::new(Mutex::new(stdin_raw));
        let shared = Arc::new(Mutex::new(Shared::default()));
        let pump_stdin = Arc::clone(&stdin);
        let pump_shared = Arc::clone(&shared);
        let pump = std::thread::Builder::new()
            .name("provider-codex-pump".into())
            .spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    if line.trim().is_empty() {
                        continue;
                    }
                    let Ok(frame) = decode_line(&line) else { continue };
                    match frame {
                        Frame::Response { id, result } => {
                            let waiter = pump_shared
                                .lock()
                                .ok()
                                .and_then(|mut shared| {
                                    shared.waiters.remove(&id.to_string())
                                });
                            if let Some(waiter) = waiter {
                                let _ = waiter.send(Ok(result));
                            }
                        }
                        Frame::ResponseError { id, message, .. } => {
                            let waiter = pump_shared
                                .lock()
                                .ok()
                                .and_then(|mut shared| {
                                    shared.waiters.remove(&id.to_string())
                                });
                            if let Some(waiter) = waiter {
                                let _ = waiter.send(Err(message));
                            }
                        }
                        Frame::Request { id, method, params } => {
                            Self::serve_server_request(
                                &pump_stdin,
                                &pump_shared,
                                &events,
                                &id,
                                &method,
                                &params,
                            );
                        }
                        Frame::Notification(_) => {
                            let Ok(mut fold) = fold.lock() else { break };
                            step_line(&mut fold, &line, &mut |event| {
                                let _ = events.send(event);
                            });
                        }
                    }
                }
                let _ = events.send(ProviderEvent::ConnectionLost {
                    reason: "the agent process exited".into(),
                });
            })
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        Ok(Self { child, stdin, next_id: Mutex::new(1), shared, pump: Some(pump) })
    }

    /// Answer one server→client request. Approval requests are surfaced as
    /// [`ProviderEvent::ApprovalRequested`] and answered later by
    /// [`Self::answer_approval`]; question requests as
    /// [`ProviderEvent::QuestionRaised`]. Anything Baaz cannot serve —
    /// notably `item/tool/call`, which needs a registered client tool nobody
    /// has probed — gets a JSON-RPC "cannot serve" error, never silence
    /// (which would stall the server) and never a faked success.
    fn serve_server_request(
        stdin: &Arc<Mutex<ChildStdin>>,
        shared: &Arc<Mutex<Shared>>,
        events: &Sender<ProviderEvent>,
        id: &Value,
        method: &str,
        params: &Value,
    ) {
        let thread_id =
            params.get("threadId").and_then(Value::as_str).unwrap_or_default().to_owned();
        if method.contains("requestApproval") {
            let item_id =
                params.get("itemId").and_then(Value::as_str).unwrap_or_default().to_owned();
            let headline = params
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or(method)
                .to_owned();
            if let Ok(mut shared) = shared.lock() {
                shared.approvals.insert(item_id.clone(), PendingApproval {
                    request_id: id.clone(),
                    thread_id: thread_id.clone(),
                    item_id: item_id.clone(),
                    headline: headline.clone(),
                });
            }
            let _ = events.send(ProviderEvent::ApprovalRequested {
                session_id: thread_id,
                approval_id: item_id,
                headline,
            });
        } else if method.contains("requestUserInput") || method.contains("elicitation/request") {
            let question_id = params
                .get("itemId")
                .or_else(|| params.get("callId"))
                .and_then(Value::as_str)
                .unwrap_or(&id.to_string())
                .to_owned();
            let headline = params
                .get("question")
                .or_else(|| params.get("prompt"))
                .and_then(Value::as_str)
                .unwrap_or(method)
                .to_owned();
            let _ = events.send(ProviderEvent::QuestionRaised {
                session_id: thread_id,
                question_id,
                headline,
            });
        } else {
            let _ = write_value(stdin, &json!({
                "id": id,
                "error": {"code": -32601, "message": format!("baaz cannot serve {method}")}
            }));
        }
    }

    /// Mint a fresh request id (starting at 1, as in the fixtures).
    pub fn next_request_id(&self) -> u64 {
        let mut next = self.next_id.lock().expect("request counter mutex");
        let id = *next;
        *next += 1;
        id
    }

    /// Call one client→server method and wait for its `result`. The id is
    /// minted here; the pump completes the matching response.
    pub fn send_request(&self, method: &str, params: Value) -> Result<Value, RequestError> {
        let id = self.next_request_id();
        self.deliver(
            id.to_string(),
            method,
            json!({"id": id, "method": method, "params": params}),
        )
    }

    /// Send a prebuilt request frame (see the `*_request` builders above) and
    /// wait for its `result`. The frame's own `id` is the correlation key, so
    /// handshake frames built offline and ad-hoc [`Self::send_request`] calls
    /// share one id space via [`Self::next_request_id`].
    pub fn send_frame(&self, request: Value) -> Result<Value, RequestError> {
        let method =
            request.get("method").and_then(Value::as_str).unwrap_or("?").to_owned();
        let Some(id) = request.get("id") else {
            return Err(RequestError { reason: "a request frame needs an id".into() });
        };
        self.deliver(id.to_string(), &method, request)
    }

    fn deliver(&self, key: String, method: &str, wire: Value) -> Result<Value, RequestError> {
        let (tx, rx): (Sender<Answer>, Receiver<Answer>) = unbounded();
        {
            let mut shared = self.shared.lock().map_err(|_| RequestError {
                reason: "the session state lock is poisoned".into(),
            })?;
            shared.waiters.insert(key.clone(), tx);
        }
        if let Err(error) = write_value(&self.stdin, &wire) {
            let _ = self
                .shared
                .lock()
                .ok()
                .and_then(|mut shared| shared.waiters.remove(&key));
            return Err(RequestError { reason: format!("the session child is unreachable: {error}") });
        }
        match rx.recv_timeout(REQUEST_TIMEOUT) {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(message)) => {
                Err(RequestError { reason: format!("{method} errored: {message}") })
            }
            Err(_) => {
                let _ = self
                    .shared
                    .lock()
                    .ok()
                    .and_then(|mut shared| shared.waiters.remove(&key));
                Err(RequestError { reason: format!("{method} timed out") })
            }
        }
    }

    /// Send one client→server notification (no id, no answer).
    pub fn send_notification(&self, method: &str, params: Value) -> std::io::Result<()> {
        write_value(&self.stdin, &json!({"method": method, "params": params}))
    }

    /// Answer a pending approval request. True when the approval was known
    /// (and is now answered and forgotten); false when the id is unknown —
    /// never an error, a stale decision is refused, not misdelivered.
    pub fn answer_approval(
        &self,
        item_id: &str,
        decision: ApprovalDecision,
    ) -> std::io::Result<bool> {
        let pending = self
            .shared
            .lock()
            .map_err(|_| std::io::Error::other("the session state lock is poisoned"))?
            .approvals
            .remove(item_id);
        let Some(pending) = pending else { return Ok(false) };
        write_value(
            &self.stdin,
            &json!({"id": pending.request_id, "result": decision_result(decision)}),
        )?;
        Ok(true)
    }

    /// The approvals still waiting for a decision, oldest first is the
    /// caller's to sort — insertion order is not tracked, so this is
    /// unordered.
    pub fn pending_approvals(&self) -> Vec<PendingApprovalView> {
        self.shared
            .lock()
            .map(|shared| {
                shared
                    .approvals
                    .values()
                    .map(|pending| PendingApprovalView {
                        item_id: pending.item_id.clone(),
                        thread_id: pending.thread_id.clone(),
                        headline: pending.headline.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Hang up. Idempotent; also runs on drop.
    pub fn shutdown(&mut self) {
        // Detach, never join: the pump ends when stdout closes, which is
        // when the child exits after stdin closes.
        self.pump.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for RunningChild {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{decode_envelope, Direction};

    fn envelopes(name: &str) -> Vec<(Direction, Value)> {
        let path = format!("{}/../../fixtures/codex/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(path)
            .expect("fixture reads")
            .lines()
            .map(|line| {
                let value: Value = serde_json::from_str(line).expect("fixture is JSON");
                let dir = match value.get("_dir").and_then(Value::as_str) {
                    Some("client->server") => Direction::ClientToServer,
                    Some("server->client") => Direction::ServerToClient,
                    other => panic!("bad _dir: {other:?}"),
                };
                (dir, value.get("frame").cloned().expect("envelope has frame"))
            })
            .collect()
    }

    fn client_frame(frames: &[(Direction, Value)], method: &str) -> Value {
        frames
            .iter()
            .find(|(dir, frame)| {
                *dir == Direction::ClientToServer
                    && frame.get("method").and_then(Value::as_str) == Some(method)
            })
            .map(|(_, frame)| frame.clone())
            .unwrap_or_else(|| panic!("no client frame for {method}"))
    }

    #[test]
    fn handshake_matches_basic_fixture_with_explicit_models() {
        // No process is spawned here: the builders below produce the same
        // frames the live probe recorded, and the assertions pin §2 —
        // thread/start and turn/start carry an explicit model.
        let frames = envelopes("basic.jsonl");

        let init = client_frame(&frames, "initialize");
        assert_eq!(init.get("id"), Some(&json!(1)));
        let params = init.get("params").expect("initialize has params");
        assert_eq!(
            params.get("capabilities").and_then(|c| c.get("experimentalApi")),
            Some(&json!(true))
        );
        assert_eq!(
            params.get("clientInfo").and_then(|c| c.get("title")).and_then(Value::as_str),
            Some("Baaz")
        );
        let built = initialize_request(1, "0.1.0");
        assert_eq!(built.get("method"), init.get("method"));
        let built_params = built.get("params").expect("built params");
        assert_eq!(
            built_params.get("capabilities"),
            params.get("capabilities"),
            "experimentalApi handshake"
        );
        assert_eq!(
            built_params.get("clientInfo").and_then(|c| c.get("title")).and_then(Value::as_str),
            Some("Baaz")
        );

        assert!(
            frames.iter().any(|(dir, frame)| *dir == Direction::ClientToServer
                && frame.get("method").and_then(Value::as_str) == Some("initialized")
                && frame.get("id").is_none()),
            "the initialized notification is sent, with no id"
        );
        assert_eq!(
            initialized_notification(),
            json!({"method": "initialized", "params": {}})
        );

        let start = client_frame(&frames, "thread/start");
        assert_eq!(start.get("id"), Some(&json!(2)));
        let params = start.get("params").expect("thread/start has params");
        let model = params.get("model").and_then(Value::as_str).expect("explicit model");
        assert_eq!(model, "gpt-5.6-sol", "thread/start carries an explicit model");
        let cwd = params.get("cwd").and_then(Value::as_str).expect("cwd");
        assert_eq!(
            thread_start_request(2, cwd, model).get("params"),
            Some(&json!({"cwd": cwd, "model": model}))
        );

        let turn = client_frame(&frames, "turn/start");
        assert_eq!(turn.get("id"), Some(&json!(3)));
        let params = turn.get("params").expect("turn/start has params");
        assert_eq!(
            params.get("model").and_then(Value::as_str),
            Some("gpt-5.6-sol"),
            "turn/start carries an explicit model too"
        );
        let text = params
            .get("input")
            .and_then(Value::as_array)
            .and_then(|input| input.first())
            .and_then(|part| part.get("text"))
            .and_then(Value::as_str)
            .expect("text input");
        assert_eq!(text, "Reply with exactly the word READY. Do not use any tools.");
        let thread_id = params.get("threadId").and_then(Value::as_str).expect("threadId");
        assert_eq!(
            turn_start_request(3, thread_id, "gpt-5.6-sol", text).get("params"),
            Some(&json!({
                "threadId": thread_id,
                "model": "gpt-5.6-sol",
                "input": [{"type": "text", "text": text}]
            }))
        );
    }

    #[test]
    fn model_catalog_reads_data_not_models() {
        let frames = envelopes("basic.jsonl");
        let response = frames
            .iter()
            .find(|(dir, frame)| {
                *dir == Direction::ServerToClient && frame.get("id") == Some(&json!(10))
            })
            .map(|(_, frame)| frame.get("result").cloned().expect("result"))
            .expect("model/list response");
        assert!(response.get("data").and_then(Value::as_array).is_some(), "key is data");
        assert!(response.get("models").is_none(), "key is not models");
        let ids = model_ids(&response);
        assert!(ids.contains(&"gpt-5.6-sol".to_owned()), "catalog: {ids:?}");
        assert_eq!(default_model(&response), Some("gpt-5.6-sol".to_owned()));
    }

    #[test]
    fn the_server_mints_the_thread_id() {
        let frames = envelopes("basic.jsonl");
        let result = frames
            .iter()
            .find(|(dir, frame)| {
                *dir == Direction::ServerToClient && frame.get("id") == Some(&json!(2))
            })
            .map(|(_, frame)| frame.get("result").cloned().expect("result"))
            .expect("thread/start response");
        let (thread_id, session_id) = thread_ids(&result).expect("thread ids parse");
        assert_eq!(thread_id, session_id, "equal on a fresh thread — and still stored, not assumed");
        assert!(!thread_id.is_empty());
    }

    #[test]
    fn approval_decisions_are_typed_and_spell_accept() {
        // The recorded wrong guess: `approved` is silently a refusal.
        assert_eq!(decision_result(ApprovalDecision::Accept), json!({"decision": "accept"}));
        assert_ne!(decision_result(ApprovalDecision::Accept), json!({"decision": "approved"}));
        assert_eq!(
            decision_result(ApprovalDecision::AcceptForSession),
            json!({"decision": "acceptForSession"})
        );
        assert_eq!(decision_result(ApprovalDecision::Decline), json!({"decision": "decline"}));
        assert_eq!(decision_result(ApprovalDecision::Cancel), json!({"decision": "cancel"}));

        // The approval fixture's own round trip uses the typed token: the
        // server asked, the client answered `accept`, the command then ran.
        let frames = envelopes("approval.jsonl");
        let (dir, frame) = frames
            .iter()
            .find(|(_, frame)| {
                frame.get("method").and_then(Value::as_str)
                    == Some("item/commandExecution/requestApproval")
            })
            .expect("approval request");
        assert_eq!(*dir, Direction::ServerToClient);
        assert!(frame.get("id").is_some(), "a server request carries an id to answer");
        assert!(
            frame.get("params").and_then(|p| p.get("reason")).and_then(Value::as_str).is_some(),
            "the reason is the free human sentence"
        );
        let answer = frames
            .iter()
            .find(|(d, f)| *d == Direction::ClientToServer && f.get("id") == frame.get("id"))
            .map(|(_, f)| f.clone())
            .expect("client answer");
        assert_eq!(
            answer.get("result"),
            Some(&decision_result(ApprovalDecision::Accept)),
            "the recorded answer is exactly what the typed Accept builds"
        );
        // And the request itself decodes as a request (id plus method), not a
        // notification — the pump must see it to answer it.
        let envelope =
            serde_json::to_string(&json!({"_dir": "server->client", "frame": frame}))
                .expect("wraps");
        match decode_envelope(&envelope) {
            Ok((Direction::ServerToClient, frame @ Frame::Request { .. })) => {
                assert_eq!(
                    frame.method(),
                    Some("item/commandExecution/requestApproval")
                );
            }
            other => panic!("expected a server request, got {other:?}"),
        }
    }
}
