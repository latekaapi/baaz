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
/// `transcript-userinput-answer.jsonl` records. `usage/read` (muse 1.3.0) is the
/// same shape: it asks for the last-observed usage snapshot and the published
/// schema gives it no params interface at all.
const UNTYPED_METHODS: &[&str] = &[
    "nope/nope",
    "initialized",
    "model/list",
    "account/read",
    "account/loginCancel",
    "account/logout",
    "usage/read",
];

/// The lane whose **results** msp.d.ts leaves shapeless.
///
/// The subagent calls (SS3.16) have typed params — [`SubagentInputParams`] and
/// friends — but the published declarations give their results no interface,
/// and the harness issues none of these calls, so there is nothing to model
/// and nothing to verify against. Typing them from guesswork would be worse
/// than saying so here.
const RESULTS_NOT_MODELLED: &[&str] = &[
    "subagent/sendMessage",
    "subagent/followupTask",
    "subagent/interrupt",
    "subagent/stop",
    "subagent/resume",
    "subagent/reopen",
    "subagent/close",
    "subagent/readResult",
];

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
///
/// `None` checks nothing and asserts nothing: it is the *inventory* call, which
/// only needs the dispatch arm below to exist and name a type. See
/// [`every_published_method_has_a_dispatch_arm`].
fn roundtrip<T: DeserializeOwned + Serialize>(what: &str, raw: Option<&Value>) {
    let Some(raw) = raw else { return };
    let typed: T = serde_json::from_value(raw.clone())
        .unwrap_or_else(|e| panic!("{what}: deserialize failed: {e}\n  json: {raw}"));
    let back = serde_json::to_value(&typed).unwrap_or_else(|e| panic!("{what}: serialize: {e}"));
    assert_eq!(
        &back, raw,
        "{what}: re-serialized JSON differs from the wire\n  wire: {raw}\n  ours: {back}"
    );
}

/// `params` of a client→server request, by method name.
fn roundtrip_request_params(method: &str, params: Option<&Value>) -> bool {
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
        "session/rename" => roundtrip::<SessionRenameParams>(w, params),
        "session/setReasoningEffort" => roundtrip::<SessionSetReasoningEffortParams>(w, params),
        "session/setApprovalMode" => roundtrip::<SessionSetApprovalModeParams>(w, params),
        "session/userShell" => roundtrip::<SessionUserShellParams>(w, params),
        "turn/start" => roundtrip::<TurnStartParams>(w, params),
        "turn/steer" => roundtrip::<TurnSteerParams>(w, params),
        "turn/interrupt" => roundtrip::<TurnInterruptParams>(w, params),
        "turn/cancel" => roundtrip::<TurnCancelParams>(w, params),
        "turn/unqueue" => roundtrip::<TurnUnqueueParams>(w, params),
        "model/list" => roundtrip::<ModelListParams>(w, params),
        "skill/list" => roundtrip::<SkillListParams>(w, params),
        "task/background" => roundtrip::<TaskBackgroundParams>(w, params),
        "task/stop" => roundtrip::<TaskStopParams>(w, params),
        "task/stopAll" => roundtrip::<TaskStopAllParams>(w, params),
        "goal/set" => roundtrip::<GoalSetParams>(w, params),
        "goal/edit" => roundtrip::<GoalEditParams>(w, params),
        "goal/clear" => roundtrip::<GoalClearParams>(w, params),
        "goal/pause" => roundtrip::<GoalPauseParams>(w, params),
        "goal/resume" => roundtrip::<GoalResumeParams>(w, params),
        "workflow/cancel" => roundtrip::<WorkflowCancelParams>(w, params),
        "workflow/childControl" => roundtrip::<WorkflowChildControlParams>(w, params),
        "view/page" => roundtrip::<ViewPageParams>(w, params),
        "view/subscribe" => roundtrip::<ViewSubscribeParams>(w, params),
        "view/unsubscribe" => roundtrip::<ViewUnsubscribeParams>(w, params),
        "item/readOutput" => roundtrip::<ItemReadOutputParams>(w, params),
        "approval/decide" => roundtrip::<ApprovalDecideParams>(w, params),
        "approval/listPending" => roundtrip::<ApprovalListPendingParams>(w, params),
        "userInput/answer" => roundtrip::<UserInputAnswerParams>(w, params),
        "userInput/cancel" => roundtrip::<UserInputCancelParams>(w, params),
        "userInput/clarify" => roundtrip::<UserInputClarifyParams>(w, params),
        "account/loginStart" => roundtrip::<AccountLoginStartParams>(w, params),
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
fn roundtrip_result(method: &str, result: Option<&Value>) -> bool {
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
        "session/rename" => roundtrip::<SessionRenameResult>(w, result),
        "session/setReasoningEffort" => roundtrip::<SessionSetReasoningEffortResult>(w, result),
        "session/setApprovalMode" => roundtrip::<SessionSetApprovalModeResult>(w, result),
        "session/userShell" => roundtrip::<SessionUserShellResult>(w, result),
        "turn/start" => roundtrip::<TurnStartResult>(w, result),
        "turn/steer" => roundtrip::<TurnSteerResult>(w, result),
        "turn/interrupt" => roundtrip::<TurnInterruptResult>(w, result),
        "turn/cancel" => roundtrip::<TurnCancelResult>(w, result),
        "turn/unqueue" => roundtrip::<TurnUnqueueResult>(w, result),
        "model/list" => roundtrip::<ModelListResult>(w, result),
        "skill/list" => roundtrip::<SkillListResult>(w, result),
        "task/background" | "task/stop" => roundtrip::<TaskCommandResult>(w, result),
        "task/stopAll" => roundtrip::<TaskStopAllResult>(w, result),
        "goal/set" | "goal/edit" | "goal/pause" | "goal/resume" | "goal/clear" => {
            roundtrip::<GoalCommandResult>(w, result);
        }
        "workflow/cancel" => roundtrip::<WorkflowControlResult>(w, result),
        "workflow/childControl" => roundtrip::<WorkflowControlResult>(w, result),
        "usage/read" => roundtrip::<UsageReadResult>(w, result),
        "view/page" => roundtrip::<ViewPageResult>(w, result),
        "view/subscribe" => roundtrip::<ViewSubscribeResult>(w, result),
        "view/unsubscribe" => roundtrip::<ViewUnsubscribeResult>(w, result),
        "item/readOutput" => roundtrip::<ItemReadOutputResult>(w, result),
        "approval/decide" => roundtrip::<ApprovalDecideResult>(w, result),
        "approval/listPending" => roundtrip::<ApprovalListPendingResult>(w, result),
        "userInput/answer" => roundtrip::<UserInputAnswerResult>(w, result),
        "userInput/cancel" => roundtrip::<UserInputCancelResult>(w, result),
        "userInput/clarify" => roundtrip::<UserInputClarifyResult>(w, result),
        "account/read" | "account/logout" => roundtrip::<AccountState>(w, result),
        "account/loginStart" => roundtrip::<AccountLoginStartResult>(w, result),
        "account/loginCancel" => roundtrip::<AccountLoginCancelResult>(w, result),
        _ => return false,
    }
    true
}

/// `params` of a server→client notification or request, by method name.
fn roundtrip_server_params(method: &str, params: Option<&Value>) -> bool {
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
        "account/changed" => roundtrip::<AccountState>(w, params),
        "account/loginCompleted" => roundtrip::<AccountLoginCompletedParams>(w, params),
        "session/modelChanged" => roundtrip::<SessionModelChangedParams>(w, params),
        "session/nameChanged" => roundtrip::<SessionNameChangedParams>(w, params),
        "session/reasoningEffortChanged" => roundtrip::<SessionReasoningEffortChangedParams>(w, params),
        "session/modelRouteUnserved" => roundtrip::<SessionModelRouteUnservedParams>(w, params),
        "session/goalChanged" => roundtrip::<SessionGoalChangedParams>(w, params),
        "session/todoListChanged" => roundtrip::<SessionTodoListChangedParams>(w, params),
        "session/branchChanged" => roundtrip::<SessionBranchChangedParams>(w, params),
        "session/tokenUsage" => roundtrip::<SessionTokenUsageParams>(w, params),
        "session/contextUsage" => roundtrip::<SessionContextUsageParams>(w, params),
        "session/approvalModeChanged" => roundtrip::<SessionApprovalModeChangedParams>(w, params),
        "session/statusChanged" => roundtrip::<SessionStatusChangedParams>(w, params),
        "session/viewHealthChanged" => roundtrip::<SessionViewHealthChangedParams>(w, params),
        "skill/changed" => roundtrip::<SkillChangedParams>(w, params),
        "usage/changed" => roundtrip::<SubscriptionUsage>(w, params),
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
                    roundtrip::<Request>(&format!("{at} request frame"), Some(&frame));
                } else {
                    roundtrip::<Notification>(&format!("{at} notification frame"), Some(&frame));
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
                    roundtrip_request_params(method, Some(&params))
                } else {
                    roundtrip_server_params(method, Some(&params))
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
                roundtrip::<SuccessResponse>(&format!("{at} success frame"), Some(&frame));
                let id = frame.get("id").expect("response carries an id").to_string();
                let method = pending
                    .get(&(!outbound, id))
                    .unwrap_or_else(|| panic!("{at}: response with no matching request"));
                assert!(
                    roundtrip_result(method, Some(result)),
                    "{at}: no dispatch entry for {method} result"
                );
                typed += 1;
            } else if frame.get("error").is_some() {
                roundtrip::<ErrorResponse>(&format!("{at} error frame"), Some(&frame));
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
    assert_eq!(kind, ItemKind::Unknown("someFutureKind".to_owned()));
    assert_eq!(kind.as_wire(), None);

    let status: ItemStatus = serde_json::from_value(Value::from("halfway")).unwrap();
    assert_eq!(status, ItemStatus::Unknown("halfway".to_owned()));

    let terminal: TurnTerminal = serde_json::from_value(Value::from("exploded")).unwrap();
    assert_eq!(terminal, TurnTerminal::Unknown("exploded".to_owned()));

    // A known value still decodes to its own variant and reports its wire string.
    let known: ItemKind = serde_json::from_value(Value::from("toolCall")).unwrap();
    assert_eq!(known, ItemKind::ToolCall);
    assert_eq!(known.as_wire(), Some("toolCall"));
    assert_eq!(serde_json::to_value(&known).unwrap(), Value::from("toolCall"));
}

/// **client-adapter-15 / A-MECH-10.** `Unknown` used to discard the
/// original string entirely — it deserialized fine but re-serialized as the
/// literal `"Unknown"`, breaking round-trip for exactly the values the open
/// enum exists to tolerate. Retaining the string fixes that.
#[test]
fn an_unknown_open_enum_value_round_trips_byte_exact() {
    let kind: ItemKind = serde_json::from_value(Value::from("someFutureKind")).unwrap();
    assert_eq!(serde_json::to_value(&kind).unwrap(), Value::from("someFutureKind"));
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
fn login_start_params_debug_redacts_the_key() {
    let params = AccountLoginStartParams {
        api_key: Some("test-key-abcdef-1234".to_owned()),
        r#type: AccountLoginType::ApiKey,
    };
    let debug = format!("{params:?}");
    assert!(debug.contains("apiKey"), "the field is named, {debug}");
    assert!(debug.contains("<redacted>"), "the key is redacted, {debug}");
    assert!(!debug.contains("test-key-abcdef-1234"), "the raw key leaked: {debug}");
    // …and the wire shape still carries the real key: redaction is
    // `Debug`-only, not serialization.
    let wire = serde_json::to_value(&params).unwrap();
    assert_eq!(
        wire,
        serde_json::json!({"type": "apiKey", "apiKey": "test-key-abcdef-1234"})
    );
}

/// **client-adapter-12 / A-MECH-4.** No capture in `fixtures/msp/` carries
/// `view/subscribe`, `item/readOutput` or `session/modelRouteUnserved` —
/// `every_recorded_frame_round_trips` skips `synthetic-*` captures on
/// purpose (see its doc comment), so those three typed surfaces had no wire
/// coverage at all even after their dispatch arms were added above. These
/// are direct schema round trips instead, to the shapes in
/// `fixtures/msp/msp-ts/msp.d.ts`.
#[test]
fn view_subscribe_and_item_read_output_and_model_route_unserved_round_trip() {
    roundtrip::<ViewSubscribeParams>(
        "view/subscribe params",
        Some(&serde_json::json!({"sessionId": "s1", "after": "v:s1:9"})),
    );
    roundtrip::<ViewSubscribeResult>(
        "view/subscribe result",
        Some(&serde_json::json!({"viewCursor": "v:s1:9"})),
    );
    roundtrip::<ItemReadOutputParams>(
        "item/readOutput params",
        Some(&serde_json::json!({
            "sessionId": "s1",
            "itemId": "i-1",
            "outputRef": "out-1",
            "offsetBytes": 0,
            "lengthBytes": 4096,
        })),
    );
    roundtrip::<ItemReadOutputResult>(
        "item/readOutput result",
        Some(&serde_json::json!({
            "content": "hello",
            "byteLen": 5,
            "offsetBytes": 0,
            "eof": true,
            "encoding": "utf8",
            "mediaType": "text/plain",
        })),
    );
    roundtrip::<SessionModelRouteUnservedParams>(
        "session/modelRouteUnserved params",
        Some(&serde_json::json!({
            "sessionId": "s1",
            "commandId": "c1",
            "installedProviderId": "meta",
            "modelId": "muse-spark-1.3",
            "providerId": "openai",
            "sourceRange": {
                "stream": {"kind": "session", "id": "s1"},
                "first": {"id": "e1", "sequence": 1},
                "last": {"id": "e1", "sequence": 1},
            },
            "viewCursor": "v:s1:1",
        })),
    );
}

#[test]
fn index_constants_match_the_published_schema() {
    assert_eq!(MSP_METHODS.len(), 47);
    assert_eq!(MSP_NOTIFICATIONS.len(), 30);
    assert_eq!(MSP_SERVER_REQUESTS, &["approval/request", "userInput/request"]);
    assert_eq!(MSP_ERROR_DATA_KINDS.len(), 38);
    assert!(SCHEMA_FINGERPRINT.starts_with("sha256:"));
    // `session/started` is emitted by the binary but absent from the published index.
    assert!(!MSP_NOTIFICATIONS.contains(&"session/started"));
}

/// One inventory, three tables: every published method and notification must
/// be named by the dispatch arms above (finding `client-adapter-16`).
///
/// Adding a wire method used to mean remembering four places — the type,
/// [`MSP_METHODS`], and the three matches here — and nothing said when one was
/// forgotten. [`MSP_METHODS`] and [`MSP_NOTIFICATIONS`] are the index, so this
/// walks them and asks each table whether it has an arm; the arm is reached
/// with no payload, so it proves only that the method is *named* and typed,
/// which is exactly what the captures cannot prove for a method they do not
/// happen to contain.
///
/// [`UNTYPED_METHODS`] and [`RESULTS_NOT_MODELLED`] are the two deliberate
/// exception lists, and the three server→client frames are named here because
/// the published index does not carry them (see [`muse_client::schema`]).
#[test]
fn every_published_method_has_a_dispatch_arm() {
    for method in MSP_METHODS {
        if !UNTYPED_METHODS.contains(method) {
            assert!(
                roundtrip_request_params(method, None),
                "{method}: no params arm in roundtrip_request_params"
            );
        }
        if !RESULTS_NOT_MODELLED.contains(method) {
            assert!(roundtrip_result(method, None), "{method}: no result arm in roundtrip_result");
        }
    }
    for method in MSP_NOTIFICATIONS {
        if UNTYPED_METHODS.contains(method) {
            continue;
        }
        assert!(
            roundtrip_server_params(method, None),
            "{method}: no params arm in roundtrip_server_params"
        );
    }
    // Emitted by the binary, absent from the published index.
    for method in ["session/started", "approval/request", "userInput/request"] {
        assert!(
            roundtrip_server_params(method, None),
            "{method}: no params arm in roundtrip_server_params"
        );
    }
}

/// **muse 1.2.1 / owner-round-2 S1.** The 1.2.1 binary serves the *full*
/// server-initiated request payloads in the `pendingRequests` of
/// `session/read` (and `resume`/`fork`) — no top-level `kind`, so the
/// documented `PendingRequestPointer` fails with `missing field 'kind'`.
///
/// The entry below is verbatim what `session/read` served on 2026-09-13 for a
/// session with a pending shell approval (ids are real; the command is
/// `echo hi && ls` in a throwaway workspace — nothing sensitive). It must
/// decode as [`PendingRequestEntry::Approval`] and re-serialize byte-exact,
/// while the documented pointer shape still decodes as
/// [`PendingRequestEntry::Pointer`].
#[test]
fn pending_requests_serve_full_payloads_without_a_kind() {
    let observed: Value = serde_json::from_str(r#"{"approvalId":"88c9949e-d154-5a28-87e6-7e9bdcf1186b","availableChoices":[{"choiceId":"allow_once","decision":"approved","label":"Allow once","scope":"once"},{"choiceId":"allow_local_prefix","decision":"approvedPolicyAmendment","label":"Always allow in this workspace: echo ...","rulePreview":"Always allow in this workspace: echo ...","scope":"localPersistent"},{"acceptsFeedback":true,"choiceId":"abort","decision":"abort","label":"Reject","scope":"once"}],"currentRequirementId":{"approvalId":"88c9949e-d154-5a28-87e6-7e9bdcf1186b","sourceIndex":0},"itemId":"57c88dbf-7fc7-4140-85bc-2b3529488d78","judgeEscalated":false,"protectedWrite":false,"rawArgs":"{\"command\":\"echo hi && ls\"}","sessionId":"01a085ab-5461-7b10-a6ab-805e21f7a245","sourceRange":{"first":{"id":"7d16cd47-d4ba-4bc8-a4e9-1356aa45c641","sequence":17},"last":{"id":"7d16cd47-d4ba-4bc8-a4e9-1356aa45c641","sequence":17},"stream":{"id":"01a085ab-5461-7b10-a6ab-805e21f7a245","kind":"session"}},"subject":{"command":"echo hi && ls","kind":"shell","stages":[{"argv":["echo","hi"],"argvComplete":true,"position":1,"requirementId":{"approvalId":"88c9949e-d154-5a28-87e6-7e9bdcf1186b","sourceIndex":0},"resolution":{"kind":"unresolved"},"suggestedPrefix":{"argvPrefix":["echo"],"label":"Always allow in this workspace: echo ..."},"totalStages":2},{"argv":["ls"],"argvComplete":true,"position":2,"requirementId":{"approvalId":"88c9949e-d154-5a28-87e6-7e9bdcf1186b","sourceIndex":1},"resolution":{"kind":"unresolved"},"suggestedPrefix":{"argvPrefix":["ls"],"label":"Always allow in this workspace: ls ..."},"totalStages":2}],"workspaceRoot":"/private/tmp/harness-ws"},"taskId":"57c88dbf-7fc7-4140-85bc-2b3529488d78","toolCallId":"user_shell_01a085ab-54a7-7411-80ca-08842679168d","toolName":"shell","turnId":"01a085ab-54a7-7411-80ca-08842679168d","viewCursor":"v:01a085ab-5461-7b10-a6ab-805e21f7a245:7"}"#)
        .expect("observed entry is JSON");
    // The old shape (`Vec<PendingRequestPointer>`) rejected this with
    // `missing field 'kind'` — the exact failure the owner saw.
    assert!(serde_json::from_value::<PendingRequestPointer>(observed.clone()).is_err());
    let entry: PendingRequestEntry =
        serde_json::from_value(observed.clone()).expect("full approval payload decodes");
    assert!(matches!(entry, PendingRequestEntry::Approval(_)));
    assert_eq!(serde_json::to_value(&entry).expect("serialize"), observed);

    // …and the documented pointer still decodes, as the Pointer arm.
    let pointer: PendingRequestEntry = serde_json::from_value(serde_json::json!({
        "kind": "approval",
        "approvalId": "a1",
        "viewCursor": "v:s:3",
    }))
    .expect("pointer decodes");
    assert!(matches!(pointer, PendingRequestEntry::Pointer(_)));
}

/// **muse 1.2.1 / owner-round-2 S1.** The additive-optional `Session` members,
/// the snapshot's new arms, the `RecoveryPending` rename result without a
/// name, and the closed MCP union (which must reject an unknown transport).
#[test]
fn new_1_2_1_shapes_round_trip() {
    roundtrip::<Session>(
        "session with 1.2.1 members",
        Some(&serde_json::json!({
            "sessionId": "s1", "path": "/tmp/x", "createdAt": "t", "updatedAt": "t",
            "status": "idle", "turnCount": 1, "activeTurnId": null, "forkedFrom": null,
            "modelId": null, "providerId": null, "workspaceRoot": null,
            "branch": "main", "title": "Do the thing", "name": "tidy-otter",
            "firstUserPrompt": "Do the thing",
        })),
    );
    // Old rows omit them; the index stays the fallback for those.
    roundtrip::<Session>(
        "session without 1.2.1 members",
        Some(&serde_json::json!({
            "sessionId": "s1", "path": "", "createdAt": "t", "updatedAt": "t",
            "status": "idle", "turnCount": 0, "activeTurnId": null, "forkedFrom": null,
            "modelId": null, "providerId": null, "workspaceRoot": null,
        })),
    );
    roundtrip::<SessionRenameResult>(
        "rename RecoveryPending arm",
        Some(&serde_json::json!({"commandId": "c1", "status": "accepted"})),
    );
    roundtrip::<SessionRenameResult>(
        "rename settled arm",
        Some(&serde_json::json!({"commandId": "c1", "status": "accepted", "name": "tidy-otter"})),
    );
    roundtrip::<SessionSetReasoningEffortParams>(
        "setReasoningEffort params",
        Some(&serde_json::json!({"commandId": "c1", "reasoningEffort": "high", "sessionId": "s1"})),
    );
    roundtrip::<SessionReasoningEffortChangedParams>(
        "reasoningEffortChanged",
        Some(&serde_json::json!({
            "reasoningEffort": "high", "sessionId": "s1", "source": "user",
            "sourceRange": {"stream": {"kind": "session", "id": "s1"},
                            "first": {"id": "e1", "sequence": 1},
                            "last": {"id": "e1", "sequence": 1}},
            "viewCursor": "v:s1:1",
        })),
    );
    roundtrip::<SessionMcpServerConfig>(
        "stdio mcp server",
        Some(&serde_json::json!({"transport": "stdio", "command": "uvx", "mode": "optional"})),
    );
    // Closed union: an undeclared transport is a real error, never a guess.
    assert!(serde_json::from_value::<SessionMcpServerConfig>(
        serde_json::json!({"transport": "pigeon", "command": "coo"})
    )
    .is_err());
    roundtrip::<Item>(
        "toolCall with patchRef and patchSummary",
        Some(&serde_json::json!({
            "itemId": "i1", "kind": "toolCall", "revision": 2, "status": "completed",
            "turnId": "t1", "tool": "edit",
            "patchRef": {"availability": "available", "byteLen": 12, "id": "p1",
                         "kind": "tool_patch", "mediaType": "application/json", "uri": "msp:p1"},
            "patchSummary": {"added": 10, "files": 2, "removed": 3},
        })),
    );
}
