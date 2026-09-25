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

/// Which approval lane a pending server request arrived on. The kind is
/// fixed when the request arrives (from its method) and decides which answer
/// shape is legal: an execpolicy amendment pairs with a command execution
/// and nothing else, and a permissions request takes no `decision` at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalKind {
    /// `item/commandExecution/requestApproval`.
    Command,
    /// `item/fileChange/requestApproval`.
    FileChange,
    /// `item/permissions/requestApproval`.
    Permissions,
    /// A future `*requestApproval` method this adapter does not know yet.
    /// Surfaced, never answered blind.
    Unknown,
}

impl ApprovalKind {
    /// Classify a server→client method: the three known approval requests,
    /// or `Unknown` for a future `*requestApproval` kind. `None` means the
    /// method is not an approval request at all.
    pub fn from_method(method: &str) -> Option<Self> {
        match method {
            "item/commandExecution/requestApproval" => Some(ApprovalKind::Command),
            "item/fileChange/requestApproval" => Some(ApprovalKind::FileChange),
            "item/permissions/requestApproval" => Some(ApprovalKind::Permissions),
            _ if method.contains("requestApproval") => Some(ApprovalKind::Unknown),
            _ => None,
        }
    }
}

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
    /// Which answer shape this request accepts, fixed at arrival.
    kind: ApprovalKind,
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

/// A server question request Baaz refused to settle blind: the JSON-RPC id
/// already answered with "cannot serve", kept so the UI can at least see it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingQuestion {
    /// The prompt's id (`itemId`, `callId`, or the request id fallback).
    question_id: String,
    /// The owning thread, for event attribution.
    thread_id: String,
    /// One-line human summary of what is being asked.
    headline: String,
}

/// One pending question, pointed at — not the full prompt. Enough to surface
/// it in [`RunningChild::pending_questions`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingQuestionView {
    /// The prompt's id.
    pub question_id: String,
    /// The owning thread.
    pub thread_id: String,
    /// One-line human summary of what is being asked.
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
    /// Question requests already refused with "cannot serve", by prompt id.
    /// Kept so [`RunningChild::pending_questions`] can surface them.
    questions: HashMap<String, PendingQuestion>,
}

/// Fail every in-flight request waiter: the child's stdout ended, so no
/// answer is coming. Without this a caller blocked in `recv_timeout` waits
/// the whole 120s and then reports a misleading timeout for a process that
/// already exited.
fn fail_waiters(shared: &Arc<Mutex<Shared>>, reason: &str) {
    let waiters: Vec<Sender<Answer>> = shared
        .lock()
        .map(|mut shared| shared.waiters.drain().map(|(_, waiter)| waiter).collect())
        .unwrap_or_default();
    for waiter in waiters {
        let _ = waiter.send(Err(reason.to_owned()));
    }
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

/// One row of the `model/list` catalog: the wire id plus the human
/// presentation the picker shows. The key is `data`, not `models`:
/// `result.data[]`, in provider order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelInfo {
    /// The catalog model id: what `thread/start` and `turn/start` carry.
    pub id: String,
    /// The human label (`displayName`), falling back to the id when the
    /// server sends none. The picker reads this; the wire never does.
    pub label: String,
    /// The server's one-line description, when it sends one.
    pub description: Option<String>,
}

/// The model catalog out of a `model/list` response: ids, human labels
/// and descriptions, in provider order. See [`ModelInfo`] for the key.
pub fn model_catalog(result: &Value) -> Vec<ModelInfo> {
    result
        .get("data")
        .and_then(Value::as_array)
        .map(|data| {
            data.iter()
                .filter_map(|row| {
                    let id = row.get("id").and_then(Value::as_str)?;
                    let label = row
                        .get("displayName")
                        .and_then(Value::as_str)
                        .filter(|label| !label.is_empty())
                        .unwrap_or(id);
                    Some(ModelInfo {
                        id: id.to_owned(),
                        label: label.to_owned(),
                        description: row
                            .get("description")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The model ids out of a `model/list` response, in provider order: the
/// [`model_catalog`] ids without their labels. One parse, so the `data`
/// (not `models`) key lives in exactly one place.
pub fn model_ids(result: &Value) -> Vec<String> {
    model_catalog(result).into_iter().map(|row| row.id).collect()
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

/// A command-execution approval decision, typed per approval kind so the
/// wire token can never be a string literal — and an execpolicy or network
/// amendment can never be offered to a file-change request: that variant
/// simply does not exist on [`FileChangeApprovalDecision`], so the pairing
/// fails to compile instead of failing closed at runtime.
///
/// The token is `accept`: replying `approved` is silently treated as a
/// refusal (`status:"declined"` in the log, no schema error), which is the
/// safe direction but indistinguishable from a real denial. Per
/// `CommandExecutionRequestApprovalResponse.json`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandApprovalDecision {
    /// Run it, once (`accept`).
    Accept,
    /// Run it and stop asking, session-scoped (`acceptForSession`).
    AcceptForSession,
    /// Run it and persist the proposed execpolicy amendment, so future
    /// matching commands run without prompting.
    AcceptWithExecpolicyAmendment {
        /// The amended execpolicy rules, echoed from the request's
        /// `proposedExecpolicyAmendment`.
        execpolicy_amendment: Vec<String>,
    },
    /// Persist a network policy rule for the host (the persistent
    /// counterpart to `acceptForSession` for network access).
    ApplyNetworkPolicyAmendment {
        /// The host the rule covers.
        host: String,
        /// Whether the host is allowed or denied.
        action: NetworkPolicyAction,
    },
    /// Refuse; the turn continues (`decline`).
    Decline,
    /// Refuse; the turn is interrupted (`cancel`).
    Cancel,
}

/// The persistent network rule action in
/// `applyNetworkPolicyAmendment.network_policy_amendment`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetworkPolicyAction {
    /// Allow the host (`allow`).
    Allow,
    /// Deny the host (`deny`).
    Deny,
}

impl NetworkPolicyAction {
    /// The wire token.
    pub fn wire(&self) -> &'static str {
        match self {
            NetworkPolicyAction::Allow => "allow",
            NetworkPolicyAction::Deny => "deny",
        }
    }

    /// Parse the wire token; anything else is `None`, never a default.
    pub fn parse(token: &str) -> Option<Self> {
        match token {
            "allow" => Some(NetworkPolicyAction::Allow),
            "deny" => Some(NetworkPolicyAction::Deny),
            _ => None,
        }
    }
}

/// A file-change approval decision. Deliberately narrower than
/// [`CommandApprovalDecision`]: `FileChangeRequestApprovalResponse.json`
/// admits exactly these four string tokens and no amendment object, so an
/// amendment sent here would be a runtime rejection — the missing variants
/// make it a compile error instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileChangeApprovalDecision {
    /// Approve the file changes, once (`accept`).
    Accept,
    /// Approve and stop asking for the same files, session-scoped
    /// (`acceptForSession`).
    AcceptForSession,
    /// Refuse; the turn continues (`decline`).
    Decline,
    /// Refuse; the turn is interrupted (`cancel`).
    Cancel,
}

/// How long a permissions grant lasts. Per
/// `PermissionsRequestApprovalResponse.json`; defaults to the turn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PermissionGrantScope {
    /// The grant lasts for this turn (the schema default).
    #[default]
    Turn,
    /// The grant lasts for the session.
    Session,
}

impl PermissionGrantScope {
    /// The wire token.
    pub fn wire(&self) -> &'static str {
        match self {
            PermissionGrantScope::Turn => "turn",
            PermissionGrantScope::Session => "session",
        }
    }

    /// Parse the wire token; anything else is `None`, never a default.
    pub fn parse(token: &str) -> Option<Self> {
        match token {
            "turn" => Some(PermissionGrantScope::Turn),
            "session" => Some(PermissionGrantScope::Session),
            _ => None,
        }
    }
}

/// The answer to a `permissions/requestApproval`: a completely different
/// shape from the other two kinds — no `decision` field at all, just the
/// granted profile plus scope. Per
/// `PermissionsRequestApprovalResponse.json`.
#[derive(Clone, Debug, PartialEq)]
pub struct PermissionsApprovalAnswer {
    /// The granted permission profile (`GrantedPermissionProfile` shape).
    /// Carried as raw JSON: the profile is a wide overlay the adapter never
    /// interprets, only echoes.
    pub permissions: Value,
    /// How long the grant lasts.
    pub scope: PermissionGrantScope,
    /// When set, every subsequent command in this turn is reviewed before
    /// normal sandboxed execution. `None` omits the key (schema null).
    pub strict_auto_review: Option<bool>,
}

/// Any approval answer, tagged by the kind it may satisfy. Constructing the
/// wrong pairing (an amendment for a file change, a `decision` for a
/// permissions request) is impossible at this layer: the per-kind answer
/// fns take only their own decision type.
#[derive(Clone, Debug, PartialEq)]
pub enum ApprovalAnswer {
    /// Answer a command-execution approval.
    Command(CommandApprovalDecision),
    /// Answer a file-change approval.
    FileChange(FileChangeApprovalDecision),
    /// Answer a permissions approval.
    Permissions(PermissionsApprovalAnswer),
}

/// The `result` answering a command-execution approval:
/// `{"decision": <token or amendment object>}`.
pub fn command_decision_result(decision: &CommandApprovalDecision) -> Value {
    let decision_value = match decision {
        CommandApprovalDecision::Accept => json!("accept"),
        CommandApprovalDecision::AcceptForSession => json!("acceptForSession"),
        CommandApprovalDecision::AcceptWithExecpolicyAmendment { execpolicy_amendment } => {
            json!({"acceptWithExecpolicyAmendment": {"execpolicy_amendment": execpolicy_amendment}})
        }
        CommandApprovalDecision::ApplyNetworkPolicyAmendment { host, action } => {
            json!({"applyNetworkPolicyAmendment":
                {"network_policy_amendment": {"host": host, "action": action.wire()}}})
        }
        CommandApprovalDecision::Decline => json!("decline"),
        CommandApprovalDecision::Cancel => json!("cancel"),
    };
    json!({"decision": decision_value})
}

/// The `result` answering a file-change approval:
/// `{"decision": <one of the four tokens>}`.
pub fn file_change_decision_result(decision: FileChangeApprovalDecision) -> Value {
    let token = match decision {
        FileChangeApprovalDecision::Accept => "accept",
        FileChangeApprovalDecision::AcceptForSession => "acceptForSession",
        FileChangeApprovalDecision::Decline => "decline",
        FileChangeApprovalDecision::Cancel => "cancel",
    };
    json!({"decision": token})
}

/// The `result` answering a permissions approval:
/// `{permissions, scope, strictAutoReview}` — no `decision` field.
pub fn permissions_answer_result(answer: &PermissionsApprovalAnswer) -> Value {
    let mut result = serde_json::Map::with_capacity(3);
    result.insert("permissions".to_owned(), answer.permissions.clone());
    result.insert("scope".to_owned(), Value::String(answer.scope.wire().to_owned()));
    if let Some(strict) = answer.strict_auto_review {
        result.insert("strictAutoReview".to_owned(), Value::Bool(strict));
    }
    Value::Object(result)
}

/// The `result` for any approval answer, routed by kind.
pub fn approval_answer_result(answer: &ApprovalAnswer) -> Value {
    match answer {
        ApprovalAnswer::Command(decision) => command_decision_result(decision),
        ApprovalAnswer::FileChange(decision) => {
            file_change_decision_result(*decision)
        }
        ApprovalAnswer::Permissions(answer) => permissions_answer_result(answer),
    }
}

fn required_str(params: &Value, key: &str) -> Option<String> {
    params.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn optional_str(params: &Value, key: &str) -> Option<String> {
    params.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// A decoded `item/fileChange/requestApproval` params. The shape comes from
/// `FileChangeRequestApprovalParams.json`: no live capture of this request
/// exists, so this decoder is schema-read, never fixture-proven.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileChangeApprovalParams {
    /// The wire item id being approved.
    pub item_id: String,
    /// The owning thread.
    pub thread_id: String,
    /// The owning turn.
    pub turn_id: String,
    /// When the approval request started (unix millis).
    pub started_at_ms: i64,
    /// Unstable session write root the agent asks for, when present.
    pub grant_root: Option<String>,
    /// The model's human sentence, when present.
    pub reason: Option<String>,
}

impl FileChangeApprovalParams {
    /// Decode the request `params`. `None` when a required schema field
    /// (`itemId`, `threadId`, `turnId`, `startedAtMs`) is missing — never a
    /// defaulted guess.
    pub fn decode(params: &Value) -> Option<Self> {
        Some(Self {
            item_id: required_str(params, "itemId")?,
            thread_id: required_str(params, "threadId")?,
            turn_id: required_str(params, "turnId")?,
            started_at_ms: params.get("startedAtMs").and_then(Value::as_i64)?,
            grant_root: optional_str(params, "grantRoot"),
            reason: optional_str(params, "reason"),
        })
    }
}

/// A decoded `item/permissions/requestApproval` params. The shape comes
/// from `PermissionsRequestApprovalParams.json`: no live capture of this
/// request exists, so this decoder is schema-read, never fixture-proven.
#[derive(Clone, Debug, PartialEq)]
pub struct PermissionsApprovalParams {
    /// The wire item id being approved.
    pub item_id: String,
    /// The owning thread.
    pub thread_id: String,
    /// The owning turn.
    pub turn_id: String,
    /// When the approval request started (unix millis).
    pub started_at_ms: i64,
    /// The working directory the grant applies to.
    pub cwd: String,
    /// The requested permission profile (`RequestPermissionProfile` shape).
    /// Carried raw: the adapter never interprets it, only surfaces it.
    pub permissions: Value,
    /// The environment the grant applies to, when present.
    pub environment_id: Option<String>,
    /// The model's human sentence, when present.
    pub reason: Option<String>,
}

impl PermissionsApprovalParams {
    /// Decode the request `params`. `None` when a required schema field
    /// (`itemId`, `threadId`, `turnId`, `startedAtMs`, `cwd`,
    /// `permissions`) is missing — never a defaulted guess.
    pub fn decode(params: &Value) -> Option<Self> {
        Some(Self {
            item_id: required_str(params, "itemId")?,
            thread_id: required_str(params, "threadId")?,
            turn_id: required_str(params, "turnId")?,
            started_at_ms: params.get("startedAtMs").and_then(Value::as_i64)?,
            cwd: required_str(params, "cwd")?,
            permissions: params.get("permissions")?.clone(),
            environment_id: optional_str(params, "environmentId"),
            reason: optional_str(params, "reason"),
        })
    }
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
                            let write = |value: Value| {
                                let _ = write_value(&pump_stdin, &value);
                            };
                            Self::serve_server_request(
                                &write,
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
                // The child's stdout ended: wake every in-flight caller with an
                // error naming the exit, then report the loss. Waiters left
                // alone would sit out the full 120s `recv_timeout` and fail
                // with a misleading "{method} timed out".
                fail_waiters(&pump_shared, "the agent process exited before answering");
                let _ = events.send(ProviderEvent::ConnectionLost {
                    reason: "the agent process exited".into(),
                });
            })
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        Ok(Self { child, stdin, next_id: Mutex::new(1), shared, pump: Some(pump) })
    }

    /// The JSON-RPC refusal for a server request Baaz cannot serve: an
    /// answer, not silence (silence would stall the server, which has no
    /// timeout on these) and never a faked success.
    fn cannot_serve_error(id: &Value, method: &str) -> Value {
        json!({
            "id": id,
            "error": {"code": -32601, "message": format!("baaz cannot serve {method}")}
        })
    }

    /// Answer one server→client request. Approval requests (all three
    /// `*requestApproval` kinds) are surfaced as
    /// [`ProviderEvent::ApprovalRequested`] and answered later by the
    /// per-kind answer fns; question requests (`item/tool/requestUserInput`,
    /// `mcpServer/elicitation/request`) are surfaced as
    /// [`ProviderEvent::QuestionRaised`], recorded for
    /// [`Self::pending_questions`], and refused with the same "cannot serve"
    /// error — answering their shapes blind would be guessing, but hanging
    /// the turn forever is worse. Anything else Baaz cannot serve —
    /// notably `item/tool/call`, which needs a registered client tool nobody
    /// has probed — gets the same JSON-RPC error, never silence and never a
    /// faked success.
    fn serve_server_request(
        write: &impl Fn(Value),
        shared: &Arc<Mutex<Shared>>,
        events: &Sender<ProviderEvent>,
        id: &Value,
        method: &str,
        params: &Value,
    ) {
        let thread_id =
            params.get("threadId").and_then(Value::as_str).unwrap_or_default().to_owned();
        if let Some(kind) = ApprovalKind::from_method(method) {
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
                    kind,
                });
            }
            let _ = events.send(ProviderEvent::ApprovalRequested {
                session_id: thread_id,
                approval_id: item_id,
                headline,
            });
        } else if method.contains("requestUserInput") || method.contains("elicitation/request") {
            let id_string = id.to_string();
            let question_id = params
                .get("itemId")
                .or_else(|| params.get("callId"))
                .and_then(Value::as_str)
                .unwrap_or(&id_string)
                .to_owned();
            let headline = params
                .get("question")
                .or_else(|| params.get("prompt"))
                .or_else(|| params.get("message"))
                .and_then(Value::as_str)
                .unwrap_or(method)
                .to_owned();
            if let Ok(mut shared) = shared.lock() {
                shared.questions.insert(question_id.clone(), PendingQuestion {
                    question_id: question_id.clone(),
                    thread_id: thread_id.clone(),
                    headline: headline.clone(),
                });
            }
            let _ = events.send(ProviderEvent::QuestionRaised {
                session_id: thread_id,
                question_id,
                headline,
            });
            // Refused, not hung: the server has no timeout on these, so a
            // turn with no answer here blocks permanently.
            write(Self::cannot_serve_error(id, method));
        } else {
            write(Self::cannot_serve_error(id, method));
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

    /// Answer a pending command-execution approval. True when the approval
    /// was known (and is now answered and forgotten); false when the id is
    /// unknown — never an error, a stale decision is refused, not
    /// misdelivered. Taking only [`CommandApprovalDecision`] is what makes
    /// an amendment-to-file-change pairing a compile error.
    pub fn answer_command_approval(
        &self,
        item_id: &str,
        decision: &CommandApprovalDecision,
    ) -> std::io::Result<bool> {
        self.answer_result(item_id, &command_decision_result(decision))
    }

    /// Answer a pending file-change approval. The decision type admits
    /// exactly the four schema tokens: there is no amendment variant to
    /// send here, by construction.
    pub fn answer_file_change_approval(
        &self,
        item_id: &str,
        decision: FileChangeApprovalDecision,
    ) -> std::io::Result<bool> {
        self.answer_result(item_id, &file_change_decision_result(decision))
    }

    /// Answer a pending permissions approval with its real
    /// `{permissions, scope, strictAutoReview}` shape — no `decision`
    /// field.
    pub fn answer_permissions_approval(
        &self,
        item_id: &str,
        answer: &PermissionsApprovalAnswer,
    ) -> std::io::Result<bool> {
        self.answer_result(item_id, &permissions_answer_result(answer))
    }

    fn answer_result(&self, item_id: &str, result: &Value) -> std::io::Result<bool> {
        let pending = self
            .shared
            .lock()
            .map_err(|_| std::io::Error::other("the session state lock is poisoned"))?
            .approvals
            .remove(item_id);
        let Some(pending) = pending else { return Ok(false) };
        write_value(&self.stdin, &json!({"id": pending.request_id, "result": result}))?;
        Ok(true)
    }

    /// Answer a pending approval request with a kind-tagged answer. True
    /// when the approval was known and the answer's kind matches the lane
    /// the request arrived on; false when the id is unknown. A kind
    /// mismatch (a command answer for a file-change request) is an error,
    /// never a misdelivery: the per-kind fns above make that pairing a
    /// compile error, and this router refuses it at runtime too.
    pub fn answer_approval(
        &self,
        item_id: &str,
        answer: &ApprovalAnswer,
    ) -> std::io::Result<bool> {
        let expected = match answer {
            ApprovalAnswer::Command(_) => ApprovalKind::Command,
            ApprovalAnswer::FileChange(_) => ApprovalKind::FileChange,
            ApprovalAnswer::Permissions(_) => ApprovalKind::Permissions,
        };
        let kind = self.approval_kind(item_id);
        match kind {
            Some(found) if found == expected => {
                self.answer_result(item_id, &approval_answer_result(answer))
            }
            Some(_) => Err(std::io::Error::other(format!(
                "approval {item_id:?} arrived as {kind:?} and cannot take a {expected:?} answer"
            ))),
            None => Ok(false),
        }
    }

    /// Which lane a pending approval arrived on, if it is still pending.
    pub fn approval_kind(&self, item_id: &str) -> Option<ApprovalKind> {
        self.shared
            .lock()
            .ok()
            .and_then(|shared| shared.approvals.get(item_id).map(|pending| pending.kind))
    }

    /// The questions already refused with "cannot serve", so the UI can at
    /// least see what the server asked. Unordered, like [`Self::pending_approvals`].
    pub fn pending_questions(&self) -> Vec<PendingQuestionView> {
        self.shared
            .lock()
            .map(|shared| {
                shared
                    .questions
                    .values()
                    .map(|pending| PendingQuestionView {
                        question_id: pending.question_id.clone(),
                        thread_id: pending.thread_id.clone(),
                        headline: pending.headline.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
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
    fn model_catalog_carries_display_names_in_provider_order() {
        // The picker reads `label`, never the raw slug: `basic.jsonl`
        // ships a `displayName` per row and the catalog keeps it, with the
        // id kept alongside for the wire. A parser that reads
        // `result.models` instead of `result.data` returns nothing here —
        // that mutation must fail this test, not slide through.
        let frames = envelopes("basic.jsonl");
        let response = frames
            .iter()
            .find(|(dir, frame)| {
                *dir == Direction::ServerToClient && frame.get("id") == Some(&json!(10))
            })
            .map(|(_, frame)| frame.get("result").cloned().expect("result"))
            .expect("model/list response");
        let rows = model_catalog(&response);
        let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(ids, ["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna", "gpt-5.5"]);
        let labels: Vec<&str> = rows.iter().map(|row| row.label.as_str()).collect();
        assert_eq!(labels, ["GPT-5.6-Sol", "GPT-5.6-Terra", "GPT-5.6-Luna", "GPT-5.5"]);
        assert!(
            rows.iter().all(|row| row.description.as_ref().is_some_and(|d| !d.is_empty())),
            "every fixture row describes itself: {rows:?}"
        );
        assert!(
            rows.iter().all(|row| row.label != row.id),
            "no raw slug where a display name exists: {rows:?}"
        );
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

    /// Pin one command decision's exact wire bytes against the schema's
    /// literal: not a round trip through our own encoder and decoder
    /// (which agrees with itself whatever it emits), but a comparison to
    /// the string the schema names.
    fn pin_decision(decision: &CommandApprovalDecision, wire: &str) {
        let result = command_decision_result(decision);
        assert_eq!(
            serde_json::to_string(&result).expect("serializes"),
            format!("{{\"decision\":{wire}}}"),
            "command decision pins its schema literal byte-for-byte"
        );
    }

    fn pin_file_change(decision: FileChangeApprovalDecision, token: &str) {
        let result = file_change_decision_result(decision);
        assert_eq!(
            serde_json::to_string(&result).expect("serializes"),
            format!("{{\"decision\":\"{token}\"}}"),
            "file-change decision pins its schema literal byte-for-byte"
        );
    }

    #[test]
    fn command_accept_pins_its_schema_literal() {
        use CommandApprovalDecision as D;
        pin_decision(&D::Accept, r#""accept""#);
        // The recorded wrong guess: `approved` is silently a refusal.
        assert_ne!(
            serde_json::to_string(&command_decision_result(&D::Accept)).expect("serializes"),
            r#"{"decision":"approved"}"#
        );
    }

    #[test]
    fn command_accept_for_session_pins_its_schema_literal() {
        pin_decision(&CommandApprovalDecision::AcceptForSession, r#""acceptForSession""#);
    }

    #[test]
    fn command_execpolicy_amendment_pins_its_schema_literal() {
        // Per `CommandExecutionRequestApprovalResponse.json`: the decision
        // is an object keyed `acceptWithExecpolicyAmendment`, carrying the
        // amended rules under `execpolicy_amendment`.
        pin_decision(
            &CommandApprovalDecision::AcceptWithExecpolicyAmendment {
                execpolicy_amendment: vec!["/bin/zsh".to_owned(), "-lc".to_owned()],
            },
            r#"{"acceptWithExecpolicyAmendment":{"execpolicy_amendment":["/bin/zsh","-lc"]}}"#,
        );
    }

    #[test]
    fn command_network_policy_amendment_pins_its_schema_literal() {
        // Per `CommandExecutionRequestApprovalResponse.json`: the decision
        // is an object keyed `applyNetworkPolicyAmendment`, carrying the
        // host rule under `network_policy_amendment`.
        for (action, token) in
            [(NetworkPolicyAction::Allow, "allow"), (NetworkPolicyAction::Deny, "deny")]
        {
            pin_decision(
                &CommandApprovalDecision::ApplyNetworkPolicyAmendment {
                    host: "example.com".to_owned(),
                    action,
                },
                &format!(
                    r#"{{"applyNetworkPolicyAmendment":{{"network_policy_amendment":{{"host":"example.com","action":"{token}"}}}}}}"#
                ),
            );
        }
    }

    #[test]
    fn command_decline_pins_its_schema_literal() {
        pin_decision(&CommandApprovalDecision::Decline, r#""decline""#);
    }

    #[test]
    fn command_cancel_pins_its_schema_literal() {
        pin_decision(&CommandApprovalDecision::Cancel, r#""cancel""#);
    }

    #[test]
    fn file_change_decisions_pin_their_schema_literals() {
        // Per `FileChangeRequestApprovalResponse.json`: exactly the four
        // string tokens, no amendment objects.
        use FileChangeApprovalDecision as D;
        pin_file_change(D::Accept, "accept");
        pin_file_change(D::AcceptForSession, "acceptForSession");
        pin_file_change(D::Decline, "decline");
        pin_file_change(D::Cancel, "cancel");
    }

    #[test]
    fn permissions_answer_pins_its_schema_shape() {
        // Per `PermissionsRequestApprovalResponse.json`: no `decision`
        // field at all — `{permissions, scope, strictAutoReview}`.
        let answer = PermissionsApprovalAnswer {
            permissions: json!({"network": {"enabled": true}}),
            scope: PermissionGrantScope::Session,
            strict_auto_review: Some(true),
        };
        let result = permissions_answer_result(&answer);
        assert!(result.get("decision").is_none(), "no decision field on this lane");
        assert_eq!(
            serde_json::to_string(&result).expect("serializes"),
            r#"{"permissions":{"network":{"enabled":true}},"scope":"session","strictAutoReview":true}"#,
            "permissions answer pins its schema shape byte-for-byte"
        );
        // Without the optional review flag the key is omitted (schema
        // null), and the scope defaults to the turn.
        let minimal = PermissionsApprovalAnswer {
            permissions: json!({"fileSystem": null, "network": null}),
            scope: PermissionGrantScope::Turn,
            strict_auto_review: None,
        };
        assert_eq!(
            serde_json::to_string(&permissions_answer_result(&minimal)).expect("serializes"),
            r#"{"permissions":{"fileSystem":null,"network":null},"scope":"turn"}"#
        );
    }

    #[test]
    fn captured_accept_answer_pins_byte_for_byte() {
        // `fixtures/codex/approval.jsonl` holds the one real answer ever
        // captured: `{"id":0,"result":{"decision":"accept"}}`. Pin it
        // byte-for-byte — the full frame, not just the decision.
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
            serde_json::to_string(&answer).expect("serializes"),
            r#"{"id":0,"result":{"decision":"accept"}}"#,
            "the captured answer, byte-for-byte"
        );
        assert_eq!(
            answer.get("result"),
            Some(&command_decision_result(&CommandApprovalDecision::Accept)),
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

    #[test]
    fn approval_methods_classify_to_their_lane() {
        use ApprovalKind as K;
        assert_eq!(
            ApprovalKind::from_method("item/commandExecution/requestApproval"),
            Some(K::Command)
        );
        assert_eq!(
            ApprovalKind::from_method("item/fileChange/requestApproval"),
            Some(K::FileChange)
        );
        assert_eq!(
            ApprovalKind::from_method("item/permissions/requestApproval"),
            Some(K::Permissions)
        );
        // A future approval kind still surfaces (the generic
        // `requestApproval` catch), but on the lane that is never answered
        // blind.
        assert_eq!(ApprovalKind::from_method("item/future/requestApproval"), Some(K::Unknown));
        assert_eq!(ApprovalKind::from_method("turn/start"), None);
        assert_eq!(ApprovalKind::from_method("item/tool/requestUserInput"), None);
    }

    #[test]
    fn file_change_params_decode_from_their_schema_shape() {
        // Schema-read, never captured: this JSON is shaped from
        // `FileChangeRequestApprovalParams.json`, not from a fixture.
        let params = json!({
            "itemId": "fc-1",
            "threadId": "t",
            "turnId": "u",
            "startedAtMs": 1790250968177_i64,
            "grantRoot": null,
            "reason": "Allow writing the edited files?",
        });
        assert_eq!(
            FileChangeApprovalParams::decode(&params),
            Some(FileChangeApprovalParams {
                item_id: "fc-1".to_owned(),
                thread_id: "t".to_owned(),
                turn_id: "u".to_owned(),
                started_at_ms: 1790250968177,
                grant_root: None,
                reason: Some("Allow writing the edited files?".to_owned()),
            })
        );
        // A missing required field decodes to nothing, never a default.
        assert_eq!(FileChangeApprovalParams::decode(&json!({"itemId": "fc-1"})), None);
    }

    #[test]
    fn permissions_params_decode_from_their_schema_shape() {
        // Schema-read, never captured: this JSON is shaped from
        // `PermissionsRequestApprovalParams.json`, not from a fixture.
        let params = json!({
            "itemId": "perm-1",
            "threadId": "t",
            "turnId": "u",
            "startedAtMs": 1790250968177_i64,
            "cwd": "/Users/owner/Projects/harness",
            "environmentId": null,
            "permissions": {"network": {"enabled": true}},
            "reason": "Allow network access for this turn?",
        });
        assert_eq!(
            PermissionsApprovalParams::decode(&params),
            Some(PermissionsApprovalParams {
                item_id: "perm-1".to_owned(),
                thread_id: "t".to_owned(),
                turn_id: "u".to_owned(),
                started_at_ms: 1790250968177,
                cwd: "/Users/owner/Projects/harness".to_owned(),
                permissions: json!({"network": {"enabled": true}}),
                environment_id: None,
                reason: Some("Allow network access for this turn?".to_owned()),
            })
        );
        // A missing required field decodes to nothing, never a default.
        assert_eq!(PermissionsApprovalParams::decode(&json!({"itemId": "perm-1"})), None);
    }

    /// Feed one server question request through `serve_server_request` with a
    /// capturing writer (no process is spawned): the branch must write a
    /// response line, raise the event, and record the question.
    fn serve_question(method: &str, params: Value) -> (Vec<Value>, Vec<ProviderEvent>, Shared) {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (events, events_rx) = unbounded();
        let written = Arc::new(Mutex::new(Vec::new()));
        let written_in = Arc::clone(&written);
        RunningChild::serve_server_request(
            &|value: Value| {
                written_in.lock().expect("written mutex").push(value);
            },
            &shared,
            &events,
            &json!(7),
            method,
            &params,
        );
        drop(events);
        drop(written_in);
        let seen: Vec<ProviderEvent> = events_rx.try_iter().collect();
        let shared = Arc::try_unwrap(shared).expect("no other owner").into_inner().expect("mutex");
        let written = Arc::try_unwrap(written).expect("no other owner").into_inner().expect("mutex");
        (written, seen, shared)
    }

    #[test]
    fn request_user_input_gets_a_response_line() {
        // `item/tool/requestUserInput` is a real server request (see
        // `schemas/codex/ServerRequest.json`): leaving its JSON-RPC id
        // unanswered would stall the turn permanently.
        let (written, seen, shared) = serve_question(
            "item/tool/requestUserInput",
            json!({"threadId": "t", "turnId": "u", "itemId": "q-1", "question": "Pick one"}),
        );
        assert_eq!(written.len(), 1, "a response line is written, never silence");
        assert_eq!(written[0].get("id"), Some(&json!(7)), "the answer echoes the request id");
        assert_eq!(
            written[0].get("error").and_then(|error| error.get("code")),
            Some(&json!(-32601)),
            "refused with cannot-serve, never a faked success"
        );
        assert!(
            matches!(
                seen.as_slice(),
                [ProviderEvent::QuestionRaised { question_id, headline, .. }]
                if question_id == "q-1" && headline == "Pick one"
            ),
            "the question is still raised: {seen:?}"
        );
        assert!(
            shared.questions.contains_key("q-1"),
            "the refused question is recorded for ListPending"
        );
    }

    #[test]
    fn elicitation_request_gets_a_response_line() {
        let (written, seen, shared) = serve_question(
            "mcpServer/elicitation/request",
            json!({"threadId": "t", "serverName": "s", "message": "Fill the form", "mode": "form", "requestedSchema": {}}),
        );
        assert_eq!(written.len(), 1, "a response line is written, never silence");
        assert_eq!(written[0].get("id"), Some(&json!(7)), "the answer echoes the request id");
        assert_eq!(
            written[0].get("error").and_then(|error| error.get("code")),
            Some(&json!(-32601)),
            "refused with cannot-serve, never a faked success"
        );
        assert!(
            matches!(seen.as_slice(), [ProviderEvent::QuestionRaised { .. }]),
            "the elicitation is still raised: {seen:?}"
        );
        assert_eq!(shared.questions.len(), 1, "the refused elicitation is recorded");
    }

    #[test]
    fn exited_child_fails_waiters_with_the_exit_not_a_timeout() {
        // No process is spawned: a waiter is registered directly, then the
        // exit path fails it — the same call the pump makes when stdout ends.
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (tx, rx) = unbounded();
        shared.lock().expect("mutex").waiters.insert("3".into(), tx);
        fail_waiters(&shared, "the agent process exited before answering");
        match rx.try_recv() {
            Ok(Err(message)) => assert!(
                message.contains("exited"),
                "the error names the exit, not a timeout: {message}"
            ),
            other => panic!("expected an exit error for the waiter, got {other:?}"),
        }
        assert!(
            shared.lock().expect("mutex").waiters.is_empty(),
            "failed waiters are forgotten, never re-failed"
        );
    }
}
