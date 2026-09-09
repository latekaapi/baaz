//! Round-trip every frame in the recorded MSP transcripts through the typed schema.
//!
//! For every line of every `fixtures/msp/*.jsonl` capture: parse the frame, deserialize its
//! `params` (or `result`) into the type the dispatch table names for that method, re-serialize, and
//! assert the result is `==` to the original JSON value. Any member the schema drops fails here.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;

use muse_client::schema::*;
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;

/// The recorded-transcript directory, resolved once.
const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/msp");

/// Methods present in the captures that deliberately have no typed params: `nope/nope` is a
/// probe for `methodNotFound`, `initialized` carries none.
/// Methods a capture may carry with no `params` at all.
///
/// `initialized` is the notification that takes none; `nope/nope` is the probe
/// that proves an unknown method is rejected. `model/list` takes none either —
/// it asks for the whole catalog — and `muse-client` sends it bare, which
/// `transcript-userinput-answer.jsonl` records.
const UNTYPED_METHODS: &[&str] = &["nope/nope", "initialized", "model/list"];

fn fixture_files() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(FIXTURE_DIR)
        .expect("fixture directory")
        .map(|e| e.expect("dir entry").path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no .jsonl fixtures found in {FIXTURE_DIR}");
    files
}

/// Deserialize `raw` into `T`, re-serialize, and require the JSON to be unchanged.
fn roundtrip<T: DeserializeOwned + Serialize>(what: &str, raw: &Value) {
    let typed: T = serde_json::from_value(raw.clone())
        .unwrap_or_else(|e| panic!("{what}: deserialize failed: {e}\n  json: {raw}"));
    let back = serde_json::to_value(&typed).unwrap_or_else(|e| panic!("{what}: serialize: {e}"));
    assert_eq!(
        &back, raw,
        "{what}: re-serialized JSON differs from the wire\n  wire: {raw}\n  ours: {back}"
    );
}

/// `params` of a client→server request, by method name.
fn roundtrip_request_params(method: &str, params: &Value) -> bool {
    let w = &format!("--> {method} params");
    match method {
        "initialize" => roundtrip::<InitializeParams>(w, params),
        "session/start" => roundtrip::<SessionStartParams>(w, params),
        "session/resume" => roundtrip::<SessionResumeParams>(w, params),
        "session/fork" => roundtrip::<SessionForkParams>(w, params),
        "session/list" => roundtrip::<SessionListParams>(w, params),
        "session/read" => roundtrip::<SessionReadParams>(w, params),
        "session/compact" => roundtrip::<SessionCompactParams>(w, params),
        "session/setModel" => roundtrip::<SessionSetModelParams>(w, params),
        "session/setApprovalMode" => roundtrip::<SessionSetApprovalModeParams>(w, params),
        "session/userShell" => roundtrip::<SessionUserShellParams>(w, params),
        "turn/start" => roundtrip::<TurnStartParams>(w, params),
        "turn/steer" => roundtrip::<TurnSteerParams>(w, params),
        "turn/interrupt" => roundtrip::<TurnInterruptParams>(w, params),
        "turn/cancel" => roundtrip::<TurnCancelParams>(w, params),
        "turn/unqueue" => roundtrip::<TurnUnqueueParams>(w, params),
        "model/list" => roundtrip::<ModelListParams>(w, params),
        "view/page" => roundtrip::<ViewPageParams>(w, params),
        "view/unsubscribe" => roundtrip::<ViewUnsubscribeParams>(w, params),
        "approval/decide" => roundtrip::<ApprovalDecideParams>(w, params),
        "approval/listPending" => roundtrip::<ApprovalListPendingParams>(w, params),
        "userInput/answer" => roundtrip::<UserInputAnswerParams>(w, params),
        "userInput/cancel" => roundtrip::<UserInputCancelParams>(w, params),
        "userInput/clarify" => roundtrip::<UserInputClarifyParams>(w, params),
        "subagent/sendMessage" | "subagent/followupTask" => {
            roundtrip::<SubagentInputParams>(w, params);
        }
        "subagent/interrupt" | "subagent/stop" | "subagent/close" => {
            roundtrip::<SubagentOwnerReasonParams>(w, params);
        }
        "subagent/resume" | "subagent/reopen" | "subagent/readResult" => {
            roundtrip::<SubagentTargetParams>(w, params);
        }
        _ => return false,
    }
    true
}

/// `result` of a client→server request, by the method the request used.
fn roundtrip_result(method: &str, result: &Value) -> bool {
    let w = &format!("<-- {method} result");
    match method {
        "initialize" => roundtrip::<InitializeResult>(w, result),
        "session/start" => roundtrip::<SessionStartResult>(w, result),
        "session/resume" => roundtrip::<SessionResumeResult>(w, result),
        "session/fork" => roundtrip::<SessionForkResult>(w, result),
        "session/list" => roundtrip::<SessionListResult>(w, result),
        "session/read" => roundtrip::<SessionReadResult>(w, result),
        "session/compact" => roundtrip::<SessionCompactResult>(w, result),
        "session/setModel" => roundtrip::<SessionSetModelResult>(w, result),
        "session/setApprovalMode" => roundtrip::<SessionSetApprovalModeResult>(w, result),
        "session/userShell" => roundtrip::<SessionUserShellResult>(w, result),
        "turn/start" => roundtrip::<TurnStartResult>(w, result),
        "turn/steer" => roundtrip::<TurnSteerResult>(w, result),
        "turn/interrupt" => roundtrip::<TurnInterruptResult>(w, result),
        "turn/cancel" => roundtrip::<TurnCancelResult>(w, result),
        "turn/unqueue" => roundtrip::<TurnUnqueueResult>(w, result),
        "model/list" => roundtrip::<ModelListResult>(w, result),
        "view/page" => roundtrip::<ViewPageResult>(w, result),
        "view/unsubscribe" => roundtrip::<ViewUnsubscribeResult>(w, result),
        "approval/decide" => roundtrip::<ApprovalDecideResult>(w, result),
        "approval/listPending" => roundtrip::<ApprovalListPendingResult>(w, result),
        "userInput/answer" => roundtrip::<UserInputAnswerResult>(w, result),
        "userInput/cancel" => roundtrip::<UserInputCancelResult>(w, result),
        "userInput/clarify" => roundtrip::<UserInputClarifyResult>(w, result),
        _ => return false,
    }
    true
}

/// `params` of a server→client notification or request, by method name.
fn roundtrip_server_params(method: &str, params: &Value) -> bool {
    let w = &format!("<-- {method} params");
    match method {
        "session/started" => roundtrip::<SessionStartedParams>(w, params),
        "turn/started" => roundtrip::<TurnStartedParams>(w, params),
        "turn/completed" => roundtrip::<TurnCompletedParams>(w, params),
        "turn/retracted" => roundtrip::<TurnRetractedParams>(w, params),
        "turn/retryScheduled" => roundtrip::<TurnRetryScheduledParams>(w, params),
        "turn/unqueued" => roundtrip::<TurnUnqueuedParams>(w, params),
        "item/started" => roundtrip::<ItemStartedParams>(w, params),
        "item/updated" => roundtrip::<ItemUpdatedParams>(w, params),
        "item/delta" => roundtrip::<ItemDeltaParams>(w, params),
        "item/completed" => roundtrip::<ItemCompletedParams>(w, params),
        "view/gap" => roundtrip::<ViewGapParams>(w, params),
        "approval/request" | "approval/requested" => {
            roundtrip::<ApprovalRequestParams>(w, params);
        }
        "approval/updated" => roundtrip::<ApprovalUpdatedParams>(w, params),
        "approval/resolved" => roundtrip::<ApprovalResolvedParams>(w, params),
        "userInput/request" | "userInput/requested" => {
            roundtrip::<UserInputRequestParams>(w, params);
        }
        "userInput/settled" => roundtrip::<UserInputSettledParams>(w, params),
        "session/modelChanged" => roundtrip::<SessionModelChangedParams>(w, params),
        "session/goalChanged" => roundtrip::<SessionGoalChangedParams>(w, params),
        "session/todoListChanged" => roundtrip::<SessionTodoListChangedParams>(w, params),
        "session/branchChanged" => roundtrip::<SessionBranchChangedParams>(w, params),
        "session/tokenUsage" => roundtrip::<SessionTokenUsageParams>(w, params),
        "session/contextUsage" => roundtrip::<SessionContextUsageParams>(w, params),
        "session/approvalModeChanged" => roundtrip::<SessionApprovalModeChangedParams>(w, params),
        _ => return false,
    }
    true
}

#[test]
fn every_recorded_frame_round_trips() {
    let mut frames = 0usize;
    let mut typed = 0usize;

    for path in fixture_files() {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        // This test asserts that a frame the server actually sent survives a
        // trip through the typed schema **byte for byte**, which is what makes
        // it evidence about the wire. A `synthetic-*` capture is hand-written —
        // it exists to drive the fold through a state no recording covers — so
        // it is not evidence about anything and cannot meet a byte-exact bar: it
        // omits the nullable fields the server always spells out, and its
        // enum-shaped strings are ours rather than the server's. Those captures
        // are covered by `muse-adapter`'s fixture tests, which is where they
        // belong.
        if name.starts_with("synthetic-") {
            continue;
        }
        let text = fs::read_to_string(&path).expect("read fixture");
        // Requests keyed by direction: each side owns its own id space.
        let mut pending: HashMap<(bool, String), String> = HashMap::new();

        // Some captures are probes that deliberately send params the server must reject (an
        // out-of-vocabulary `reasoningEffort`, a session id that does not exist). A request whose
        // response was an error is not required to be representable by the typed params — the
        // closed enums exist precisely so those fail — so its params are exempt from the round
        // trip. Its frame envelope still is not.
        let rejected: HashSet<String> = text
            .lines()
            .filter_map(|l| l.strip_prefix("<-- "))
            .filter_map(|j| serde_json::from_str::<Value>(j).ok())
            .filter(|f| f.get("error").is_some())
            .filter_map(|f| f.get("id").map(Value::to_string))
            .collect();

        for (lineno, line) in text.lines().enumerate() {
            let line = line.trim();
            // A blank line, or the `#` header a hand-written `synthetic-*`
            // capture carries to say which of its lines came off the wire and
            // which did not. Neither is a frame.
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (dir, json) = line.split_once(' ').expect("`--> ` / `<-- ` prefix");
            let outbound = match dir {
                "-->" => true,
                "<--" => false,
                other => panic!("{name}:{lineno}: unknown direction {other}"),
            };
            let frame: Value = serde_json::from_str(json).expect("frame is JSON");
            frames += 1;
            let at = format!("{name}:{}", lineno + 1);

            if let Some(method) = frame.get("method").and_then(Value::as_str) {
                if let Some(id) = frame.get("id") {
                    // A request: remember it so its response can be typed.
                    pending.insert((outbound, id.to_string()), method.to_owned());
                    roundtrip::<Request>(&format!("{at} request frame"), &frame);
                } else {
                    roundtrip::<Notification>(&format!("{at} notification frame"), &frame);
                }

                if outbound
                    && frame
                        .get("id")
                        .is_some_and(|id| rejected.contains(&id.to_string()))
                {
                    continue;
                }

                let params = frame.get("params").cloned().unwrap_or(Value::Null);
                if params.is_null() {
                    assert!(
                        UNTYPED_METHODS.contains(&method),
                        "{at}: {method} arrived with no params"
                    );
                    continue;
                }
                let handled = if outbound {
                    roundtrip_request_params(method, &params)
                } else {
                    roundtrip_server_params(method, &params)
                };
                if handled {
                    typed += 1;
                } else {
                    assert!(
                        UNTYPED_METHODS.contains(&method),
                        "{at}: no dispatch entry for method {method}"
                    );
                }
            } else if let Some(result) = frame.get("result") {
                roundtrip::<SuccessResponse>(&format!("{at} success frame"), &frame);
                let id = frame.get("id").expect("response carries an id").to_string();
                let method = pending
                    .get(&(!outbound, id))
                    .unwrap_or_else(|| panic!("{at}: response with no matching request"));
                assert!(
                    roundtrip_result(method, result),
                    "{at}: no dispatch entry for {method} result"
                );
                typed += 1;
            } else if frame.get("error").is_some() {
                roundtrip::<ErrorResponse>(&format!("{at} error frame"), &frame);
                typed += 1;
            } else {
                panic!("{at}: frame is neither request, notification, result nor error");
            }
        }
    }

    assert!(frames > 100, "expected a substantial corpus, saw {frames}");
    assert!(typed > 100, "expected many typed payloads, saw {typed}");
}

#[test]
fn open_enums_tolerate_unknown_server_values() {
    let kind: ItemKind = serde_json::from_value(Value::from("someFutureKind")).unwrap();
    assert_eq!(kind, ItemKind::Unknown);
    assert_eq!(kind.as_wire(), None);

    let status: ItemStatus = serde_json::from_value(Value::from("halfway")).unwrap();
    assert_eq!(status, ItemStatus::Unknown);

    let terminal: TurnTerminal = serde_json::from_value(Value::from("exploded")).unwrap();
    assert_eq!(terminal, TurnTerminal::Unknown);

    // A known value still decodes to its own variant and reports its wire string.
    let known: ItemKind = serde_json::from_value(Value::from("toolCall")).unwrap();
    assert_eq!(known, ItemKind::ToolCall);
    assert_eq!(known.as_wire(), Some("toolCall"));
    assert_eq!(serde_json::to_value(&known).unwrap(), Value::from("toolCall"));
}

#[test]
fn closed_enums_reject_unknown_values() {
    assert!(serde_json::from_value::<ApprovalMode>(Value::from("yolo")).is_err());
    assert!(serde_json::from_value::<ReasoningEffort>(Value::from("turbo")).is_err());
    // `max` joined the closed tier vocabulary in muse 1.1.1, between `xhigh` and `ultra`.
    assert_eq!(
        serde_json::from_value::<ReasoningEffort>(Value::from("max")).unwrap(),
        ReasoningEffort::Max
    );
    assert!(serde_json::from_value::<IfBusy>(Value::from("later")).is_err());
    assert!(serde_json::from_value::<UserInputSelectionMode>(Value::from("many")).is_err());
    assert!(serde_json::from_value::<ViewPageDirection>(Value::from("sideways")).is_err());
    assert!(serde_json::from_value::<JsonRpcVersion>(Value::from("1.0")).is_err());

    // …and still accept the values the schema names.
    assert_eq!(
        serde_json::from_value::<ApprovalMode>(Value::from("promptUnmatched")).unwrap(),
        ApprovalMode::PromptUnmatched
    );
}

#[test]
fn required_nullable_members_serialize_as_null() {
    let params: SessionStartedParams = serde_json::from_str(
        r#"{"session":{"sessionId":"s","path":"","createdAt":"t","updatedAt":"t",
             "status":"idle","turnCount":0,"activeTurnId":null,"forkedFrom":null,
             "modelId":null,"providerId":null,"workspaceRoot":null}}"#,
    )
    .unwrap();
    let back = serde_json::to_value(&params).unwrap();
    for key in [
        "activeTurnId",
        "forkedFrom",
        "modelId",
        "providerId",
        "workspaceRoot",
    ] {
        assert_eq!(back["session"][key], Value::Null, "{key} must stay on the wire");
    }
    // …while an absent optional member stays absent.
    assert!(back["session"].get("approvalMode").is_none());
}

#[test]
fn optional_and_nullable_error_data_keeps_the_distinction() {
    let absent: ErrorData = serde_json::from_str(r#"{"kind":"notFound"}"#).unwrap();
    assert_eq!(absent.anchor, None);
    assert!(serde_json::to_value(&absent).unwrap().get("anchor").is_none());

    let explicit_null: ErrorData =
        serde_json::from_str(r#"{"kind":"noBoundary","anchor":null}"#).unwrap();
    assert_eq!(explicit_null.anchor, Some(None));
    assert_eq!(
        serde_json::to_value(&explicit_null).unwrap()["anchor"],
        Value::Null
    );
}

#[test]
fn constructors_produce_the_documented_shapes() {
    let init = InitializeParams::new("harness", "0.1.0");
    assert_eq!(
        serde_json::to_value(&init).unwrap(),
        serde_json::json!({"clientInfo": {"name": "harness", "version": "0.1.0"}})
    );

    assert_eq!(
        serde_json::to_value(TurnInputPart::text("hi")).unwrap(),
        serde_json::json!({"text": "hi", "type": "text"})
    );
    assert_eq!(
        serde_json::to_value(TurnInputPart::image("QUJD", "image/png")).unwrap(),
        serde_json::json!({"base64Data": "QUJD", "mediaType": "image/png", "type": "image"})
    );
}

#[test]
fn index_constants_match_the_published_schema() {
    assert_eq!(MSP_METHODS.len(), 33);
    assert_eq!(MSP_NOTIFICATIONS.len(), 24);
    assert_eq!(MSP_ERROR_DATA_KINDS.len(), 37);
    assert!(SCHEMA_FINGERPRINT.starts_with("sha256:"));
    // `session/started` is emitted by the binary but absent from the published index.
    assert!(!MSP_NOTIFICATIONS.contains(&"session/started"));
}
