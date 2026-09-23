//! Every [`Command`] arm against the recording puppet.
//!
//! The puppet (`tests/fixtures/model_list_puppet.py`) is still a real child
//! process spoken to over real stdin/stdout JSON-RPC framing, so
//! `muse-client`'s writer and reader threads, its request/response matching
//! and its typed deserialization all genuinely run. It now records every
//! `{method, params}` it receives (appended to `--record PATH` and flushed
//! before the response, so a record is durable as soon as `send` returns)
//! and answers every method with a minimally valid result.
//!
//! The table below builds each [`Command`] variant, drives it through
//! [`MuseAdapter::send`], and asserts on what the puppet recorded — BOTH
//! the method name and the full params object. Asserting params alone would
//! miss an arm that builds the right object and calls the wrong client
//! method; asserting the method alone would miss the reviewer's mutation
//! (emptying `SubmitInput`'s `input`). Where the result maps back into an
//! [`Ack`], the ack is asserted too. All 28 variants are covered; none is
//! skipped.
//!
//! These tests prove the adapter sends the method and params it intends to.
//! They do NOT prove a real muse accepts them — nothing here talks to one.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use aui_protocol::PermissionMode;
use muse_client::schema::{
    AccountLoginStartParams, AccountLoginType, ApprovalDecideParams, ApprovalListPendingParams,
    ApprovalMode, ApprovalRequirementRef, ForkCutPoint, ItemReadOutputParams, ModelListParams,
    ModelSelection, SessionCompactParams, SessionForkParams, SessionListParams, SessionReadParams,
    SessionResumeParams, SessionSetApprovalModeParams, SessionSetModelParams, SessionStartParams,
    SessionUserShellParams, TurnCancelParams, TurnInterruptParams, TurnStartParams, TurnSteerParams,
    TurnUnqueueParams, UserInputAnswer, UserInputAnswerParams, UserInputCancelParams,
    UserInputClarification, UserInputClarifyParams, ViewPageDirection, ViewPageParams,
    ViewSubscribeParams, ViewUnsubscribeParams,
};
use muse_client::{MuseClient, MuseConfig};
use provider::{Ack, Command, ConnectInfo, Provider, QuestionAnswer, SubmissionPart};
use provider_muse::MuseAdapter;
use serde_json::Value;

fn puppet() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/model_list_puppet.py")
}

static RECORD_SEQ: AtomicU64 = AtomicU64::new(0);

fn record_path() -> PathBuf {
    let seq = RECORD_SEQ.fetch_add(1, Ordering::SeqCst);
    std::env::temp_dir()
        .join(format!("provider-muse-arms-{}-{seq}.jsonl", std::process::id()))
}

/// One recorded client→server request line.
struct Record {
    method: String,
    params: Value,
}

/// Block until the puppet has recorded `connect`'s whole handshake — the
/// `initialize` request plus its trailing `initialized` notification — and
/// return the record count.
///
/// Without this, the baseline below races the handshake's tail:
/// `MuseClient::initialize` only queues the `initialized` notification to
/// its writer thread, so the puppet can record it after `connect` has
/// already returned, and the late arrival is then misattributed to the
/// first command (`open-session` sees 3 records vs `seen + 1`). Stdin
/// order means everything `connect` sent is recorded once `initialized`
/// is, so waiting for that one line settles the whole baseline.
fn wait_for_handshake_records(path: &PathBuf) -> usize {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let all = read_records(path);
        if all.iter().any(|record| record.method == "initialized") {
            return all.len();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "connect's `initialized` notification never reached the puppet"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn read_records(path: &PathBuf) -> Vec<Record> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let value: Value = serde_json::from_str(line).expect("record line is JSON");
            Record {
                method: value
                    .get("method")
                    .and_then(Value::as_str)
                    .expect("record has a method")
                    .to_owned(),
                params: value.get("params").cloned().unwrap_or(Value::Null),
            }
        })
        .collect()
}

macro_rules! params_of {
    ($params:expr) => {
        serde_json::to_value(&$params).expect("params serialize")
    };
}

/// A [`Command`] ready to send, except [`Prepared::DecideApproval`], which
/// still needs the stage token [`Command::ListPending`] minted — the table
/// always runs that row first, so the token is on hand by then.
enum Prepared {
    Ready(Command),
    DecideApproval {
        request_id: String,
        session_id: String,
        approval: String,
        choice: String,
        feedback: Option<String>,
    },
}

struct Case {
    /// Table name, used in every assertion message.
    name: &'static str,
    /// The exact wire method the arm must call.
    method: &'static str,
    command: Prepared,
    /// The exact params object the arm must send.
    expected_params: Value,
    /// The ack the puppet's canned result must map back to.
    check_ack: Box<dyn Fn(Ack)>,
}

fn accepted(ack: Ack) {
    assert_eq!(ack, Ack::Accepted, "admission-only arms ack Accepted");
}

fn session_ack(session_id: &'static str, title: Option<&'static str>) -> Box<dyn Fn(Ack)> {
    let title = title.map(str::to_owned);
    Box::new(move |ack| match ack {
        Ack::Session { session_id: got, title: got_title } => {
            assert_eq!(got, session_id);
            assert_eq!(got_title, title);
        }
        other => panic!("expected a session ack, got {other:?}"),
    })
}

#[test]
fn every_command_arm_sends_its_method_params_and_ack() {
    let script = puppet();
    assert!(script.exists(), "fake server script missing: {}", script.display());
    let records = record_path();

    let client = MuseClient::spawn(&MuseConfig {
        program: script,
        trust_workspace: false,
        no_session_log: false,
        extra_args: vec![
            "--record".to_owned(),
            records.to_string_lossy().into_owned(),
        ],
    })
    .expect("fake server spawns — is python3 on PATH?");
    let mut adapter = Provider::new(MuseAdapter::new(client));
    let handshake = adapter.connect(&ConnectInfo::new("baaz", "0.1.0")).expect("handshake");
    assert_eq!(handshake.agent_name, "fake-muse");

    // Whatever `connect` itself said on the wire is not under test — once
    // all of it has arrived (see `wait_for_handshake_records`: the
    // `initialized` tail is fire-and-forget and must be awaited here, not
    // charged to the first command).
    let mut seen = wait_for_handshake_records(&records);

    // The stage token only exists once `list-pending` has run; the table
    // order below guarantees that row comes before `decide-approval`.
    let mut stage_token: Option<String> = None;

    let cases: Vec<Case> = vec![
        Case {
            name: "open-session",
            method: "session/start",
            command: Prepared::Ready(Command::OpenSession {
                request_id: "cmd-open".into(),
                workspace: Some("/tmp/w".into()),
                model: Some("m-1".into()),
                model_provider: Some("p-1".into()),
            }),
            expected_params: params_of!(SessionStartParams {
                approval_mode: None,
                command_id: "cmd-open".into(),
                config: None,
                model_id: Some("m-1".into()),
                provider_id: Some("p-1".into()),
                session_id: None,
                workspace_root: Some("/tmp/w".into()),
            }),
            check_ack: session_ack("s-1", Some("Fake Session")),
        },
        Case {
            name: "resume-session",
            method: "session/resume",
            command: Prepared::Ready(Command::ResumeSession {
                request_id: "cmd-resume".into(),
                session_id: "s-1".into(),
                cursor: Some("c-1".into()),
                metadata_only: true,
            }),
            expected_params: params_of!(SessionResumeParams {
                command_id: "cmd-resume".into(),
                config: None,
                cursor: Some("c-1".into()),
                exclude_items: Some(true),
                history: None,
                session_id: "s-1".into(),
            }),
            check_ack: session_ack("s-1", Some("Fake Session")),
        },
        Case {
            name: "fork-session",
            method: "session/fork",
            command: Prepared::Ready(Command::ForkSession {
                request_id: "cmd-fork".into(),
                session_id: "s-1".into(),
                through_turn: Some("t-9".into()),
                metadata_only: false,
            }),
            expected_params: params_of!(SessionForkParams {
                command_id: "cmd-fork".into(),
                cut_point: Some(ForkCutPoint { last_turn_id: "t-9".into() }),
                exclude_items: None,
                session_id: "s-1".into(),
            }),
            check_ack: session_ack("s-1", Some("Fake Session")),
        },
        Case {
            name: "list-sessions",
            method: "session/list",
            command: Prepared::Ready(Command::ListSessions {
                cursor: Some("c-1".into()),
                limit: Some(25),
                workspace: Some("/tmp/w".into()),
            }),
            expected_params: params_of!(SessionListParams {
                cursor: Some("c-1".into()),
                limit: Some(25),
                updated_after: None,
                workspace_root: Some("/tmp/w".into()),
            }),
            check_ack: Box::new(|ack| match ack {
                Ack::SessionIndex { sessions, next_cursor } => {
                    assert_eq!(sessions.len(), 1);
                    assert_eq!(sessions[0].session_id, "s-1");
                    assert_eq!(sessions[0].title.as_deref(), Some("Fake Session"));
                    assert_eq!(next_cursor, None);
                }
                other => panic!("expected a session index, got {other:?}"),
            }),
        },
        Case {
            name: "read-session",
            method: "session/read",
            command: Prepared::Ready(Command::ReadSession {
                session_id: "s-1".into(),
                metadata_only: false,
            }),
            expected_params: params_of!(SessionReadParams {
                exclude_items: None,
                session_id: "s-1".into(),
            }),
            check_ack: session_ack("s-1", Some("Fake Session")),
        },
        Case {
            name: "compact-session",
            method: "session/compact",
            command: Prepared::Ready(Command::CompactSession {
                request_id: "cmd-compact".into(),
                session_id: "s-1".into(),
                through_turn: Some("t-9".into()),
            }),
            expected_params: params_of!(SessionCompactParams {
                command_id: "cmd-compact".into(),
                session_id: "s-1".into(),
                turn_id: Some("t-9".into()),
            }),
            check_ack: Box::new(accepted),
        },
        Case {
            name: "select-model",
            method: "session/setModel",
            command: Prepared::Ready(Command::SelectModel {
                request_id: "cmd-model".into(),
                session_id: "s-1".into(),
                model: "m-2".into(),
                model_provider: Some("p-9".into()),
            }),
            expected_params: params_of!(SessionSetModelParams {
                command_id: "cmd-model".into(),
                model: ModelSelection {
                    display_label: None,
                    model_id: "m-2".into(),
                    profile_id: None,
                    provider_id: Some("p-9".into()),
                },
                session_id: "s-1".into(),
            }),
            check_ack: Box::new(accepted),
        },
        Case {
            name: "select-approval-mode",
            method: "session/setApprovalMode",
            command: Prepared::Ready(Command::SelectApprovalMode {
                request_id: "cmd-mode".into(),
                session_id: "s-1".into(),
                mode: PermissionMode::DenyUnmatched,
            }),
            expected_params: params_of!(SessionSetApprovalModeParams {
                command_id: "cmd-mode".into(),
                mode: ApprovalMode::DenyUnmatched,
                session_id: "s-1".into(),
            }),
            check_ack: Box::new(accepted),
        },
        Case {
            name: "run-shell",
            method: "session/userShell",
            command: Prepared::Ready(Command::RunShell {
                request_id: "cmd-shell".into(),
                session_id: "s-1".into(),
                command: "echo hi".into(),
            }),
            expected_params: params_of!(SessionUserShellParams {
                command_id: "cmd-shell".into(),
                command_text: "echo hi".into(),
                session_id: "s-1".into(),
            }),
            check_ack: Box::new(accepted),
        },
        Case {
            name: "submit-input",
            method: "turn/start",
            command: Prepared::Ready(Command::SubmitInput {
                request_id: "cmd-submit".into(),
                session_id: "s-1".into(),
                parts: vec![
                    SubmissionPart::Text("hello".into()),
                    SubmissionPart::Image {
                        base64_data: "aGVsbG8=".into(),
                        media_type: "image/png".into(),
                    },
                ],
                display_text: Some("hello display".into()),
            }),
            expected_params: params_of!(TurnStartParams {
                command_id: "cmd-submit".into(),
                display_text: Some("hello display".into()),
                if_busy: None,
                input: vec![
                    muse_client::schema::TurnInputPart::text("hello"),
                    muse_client::schema::TurnInputPart::image("aGVsbG8=", "image/png"),
                ],
                reasoning_effort: None,
                session_id: "s-1".into(),
            }),
            check_ack: Box::new(|ack| match ack {
                Ack::TurnAccepted { turn_id } => assert_eq!(turn_id, "t-1"),
                other => panic!("expected a turn ack, got {other:?}"),
            }),
        },
        Case {
            name: "steer-input",
            method: "turn/steer",
            command: Prepared::Ready(Command::SteerInput {
                request_id: "cmd-steer".into(),
                session_id: "s-1".into(),
                expected_turn: "t-1".into(),
                parts: vec![SubmissionPart::Text("steer this".into())],
            }),
            expected_params: params_of!(TurnSteerParams {
                command_id: "cmd-steer".into(),
                expected_turn_id: "t-1".into(),
                input: vec![muse_client::schema::TurnInputPart::text("steer this")],
                reasoning_effort: None,
                session_id: "s-1".into(),
            }),
            check_ack: Box::new(accepted),
        },
        Case {
            name: "interrupt-turn",
            method: "turn/interrupt",
            command: Prepared::Ready(Command::InterruptTurn {
                request_id: "cmd-interrupt".into(),
                session_id: "s-1".into(),
                turn: Some("t-3".into()),
                retract: true,
            }),
            expected_params: params_of!(TurnInterruptParams {
                command_id: "cmd-interrupt".into(),
                retract: Some(true),
                session_id: "s-1".into(),
                turn_id: Some("t-3".into()),
            }),
            check_ack: Box::new(accepted),
        },
        Case {
            name: "cancel-turn",
            method: "turn/cancel",
            command: Prepared::Ready(Command::CancelTurn {
                request_id: "cmd-cancel".into(),
                session_id: "s-1".into(),
                turn: None,
            }),
            expected_params: params_of!(TurnCancelParams {
                command_id: "cmd-cancel".into(),
                session_id: "s-1".into(),
                turn_id: None,
            }),
            check_ack: Box::new(accepted),
        },
        Case {
            name: "reclaim-queued",
            method: "turn/unqueue",
            command: Prepared::Ready(Command::ReclaimQueued {
                request_id: "cmd-reclaim".into(),
                session_id: "s-1".into(),
                turn: "t-7".into(),
            }),
            expected_params: params_of!(TurnUnqueueParams {
                command_id: "cmd-reclaim".into(),
                session_id: "s-1".into(),
                turn_id: "t-7".into(),
            }),
            check_ack: Box::new(accepted),
        },
        Case {
            name: "list-models",
            method: "model/list",
            command: Prepared::Ready(Command::ListModels { session: Some("s-1".into()) }),
            expected_params: params_of!(ModelListParams { session_id: Some("s-1".into()) }),
            check_ack: Box::new(|ack| match ack {
                Ack::ModelCatalog { models, provider } => {
                    assert_eq!(provider, "fake");
                    assert_eq!(models.len(), 1);
                    assert_eq!(models[0].id, "fake-pro");
                    assert_eq!(models[0].label, "Fake Pro");
                    assert!(!models[0].active);
                }
                other => panic!("expected a model catalog, got {other:?}"),
            }),
        },
        Case {
            name: "list-pending",
            method: "approval/listPending",
            command: Prepared::Ready(Command::ListPending { session_id: "s-1".into() }),
            expected_params: params_of!(ApprovalListPendingParams { session_id: "s-1".into() }),
            check_ack: Box::new(|ack| match ack {
                Ack::PendingWork { approvals, questions } => {
                    assert_eq!(approvals.len(), 1);
                    assert_eq!(approvals[0].id, "a-1");
                    assert_eq!(approvals[0].session_id, "s-1");
                    assert_eq!(approvals[0].headline, "Run `rm -rf /tmp/x`");
                    assert!(
                        approvals[0].stage_token.is_some(),
                        "the staged approval must mint a token to decide with"
                    );
                    assert!(questions.is_empty());
                }
                other => panic!("expected pending work, got {other:?}"),
            }),
        },
        Case {
            name: "decide-approval",
            method: "approval/decide",
            command: Prepared::DecideApproval {
                request_id: "cmd-decide".into(),
                session_id: "s-1".into(),
                approval: "a-1".into(),
                choice: "c-yes".into(),
                feedback: Some("looks good".into()),
            },
            // The guard the decision must echo: the puppet's canned
            // approval carries `currentRequirementId`
            // `{approvalId: "a-1", sourceIndex: 3}`.
            expected_params: params_of!(ApprovalDecideParams {
                approval_id: "a-1".into(),
                choice_id: "c-yes".into(),
                command_id: "cmd-decide".into(),
                feedback: Some("looks good".into()),
                requirement_id: ApprovalRequirementRef {
                    approval_id: "a-1".into(),
                    source_index: 3,
                },
                session_id: "s-1".into(),
            }),
            check_ack: Box::new(accepted),
        },
        Case {
            name: "answer-question",
            method: "userInput/answer",
            command: Prepared::Ready(Command::AnswerQuestion {
                request_id: "cmd-answer".into(),
                session_id: "s-1".into(),
                question: "q-1".into(),
                answers: vec![QuestionAnswer {
                    question_id: "q-1".into(),
                    selected_label: Some("Yes".into()),
                    selected_labels: Some(vec!["A".into(), "B".into()]),
                    free_text: Some("free".into()),
                    note: Some("note".into()),
                }],
            }),
            expected_params: params_of!(UserInputAnswerParams {
                answers: vec![UserInputAnswer {
                    free_text: Some("free".into()),
                    note: Some("note".into()),
                    question_id: "q-1".into(),
                    selected_label: Some("Yes".into()),
                    selected_labels: Some(vec!["A".into(), "B".into()]),
                }],
                command_id: "cmd-answer".into(),
                session_id: "s-1".into(),
                user_input_id: "q-1".into(),
            }),
            check_ack: Box::new(accepted),
        },
        Case {
            name: "dismiss-question",
            method: "userInput/cancel",
            command: Prepared::Ready(Command::DismissQuestion {
                request_id: "cmd-dismiss".into(),
                session_id: "s-1".into(),
                question: "q-1".into(),
                reason: Some("nope".into()),
            }),
            expected_params: params_of!(UserInputCancelParams {
                command_id: "cmd-dismiss".into(),
                reason: Some("nope".into()),
                session_id: "s-1".into(),
                user_input_id: "q-1".into(),
            }),
            check_ack: Box::new(accepted),
        },
        Case {
            name: "clarify-question",
            method: "userInput/clarify",
            command: Prepared::Ready(Command::ClarifyQuestion {
                request_id: "cmd-clarify".into(),
                session_id: "s-1".into(),
                question: "q-1".into(),
                text: "explain please".into(),
            }),
            expected_params: params_of!(UserInputClarifyParams {
                clarification: UserInputClarification {
                    content: "explain please".into(),
                    format: "text".into(),
                },
                command_id: "cmd-clarify".into(),
                session_id: "s-1".into(),
                user_input_id: "q-1".into(),
            }),
            check_ack: Box::new(accepted),
        },
        Case {
            name: "page-transcript",
            method: "view/page",
            command: Prepared::Ready(Command::PageTranscript {
                session_id: "s-1".into(),
                after: Some("v:1".into()),
                limit: 50,
                backward: true,
            }),
            expected_params: params_of!(ViewPageParams {
                anchor: None,
                cursor: Some("v:1".into()),
                direction: Some(ViewPageDirection::Backward),
                limit: 50,
                session_id: "s-1".into(),
            }),
            check_ack: Box::new(|ack| match ack {
                Ack::TranscriptPage { deltas, next_cursor } => {
                    assert!(deltas.is_empty(), "the empty page folds to no deltas");
                    assert_eq!(next_cursor.as_deref(), Some("v:2"));
                }
                other => panic!("expected a transcript page, got {other:?}"),
            }),
        },
        Case {
            name: "follow-session",
            method: "view/subscribe",
            command: Prepared::Ready(Command::FollowSession {
                session_id: "s-1".into(),
                after: Some("v:1".into()),
            }),
            expected_params: params_of!(ViewSubscribeParams {
                after: Some("v:1".into()),
                session_id: "s-1".into(),
            }),
            check_ack: Box::new(accepted),
        },
        Case {
            name: "unfollow-session",
            method: "view/unsubscribe",
            command: Prepared::Ready(Command::UnfollowSession { session_id: "s-1".into() }),
            expected_params: params_of!(ViewUnsubscribeParams { session_id: "s-1".into() }),
            check_ack: Box::new(accepted),
        },
        Case {
            name: "read-stored-output",
            method: "item/readOutput",
            command: Prepared::Ready(Command::ReadStoredOutput {
                session_id: "s-1".into(),
                item: "i-1".into(),
                output: "o-1".into(),
                offset: 10,
                length: Some(100),
            }),
            expected_params: params_of!(ItemReadOutputParams {
                item_id: "i-1".into(),
                length_bytes: Some(100),
                offset_bytes: Some(10),
                output_ref: "o-1".into(),
                session_id: "s-1".into(),
            }),
            check_ack: Box::new(|ack| match ack {
                Ack::StoredOutput { content, complete } => {
                    assert_eq!(content, "hello");
                    assert!(complete);
                }
                other => panic!("expected stored output, got {other:?}"),
            }),
        },
        Case {
            name: "read-account",
            method: "account/read",
            command: Prepared::Ready(Command::ReadAccount),
            // A no-params call: the client omits `params` entirely.
            expected_params: Value::Null,
            check_ack: Box::new(|ack| match ack {
                Ack::Account { signed_in, label } => {
                    assert!(signed_in);
                    assert_eq!(label.as_deref(), Some("fake@example.com"));
                }
                other => panic!("expected account state, got {other:?}"),
            }),
        },
        Case {
            name: "begin-login",
            method: "account/loginStart",
            command: Prepared::Ready(Command::BeginLogin { api_key: None }),
            expected_params: params_of!(AccountLoginStartParams {
                api_key: None,
                r#type: AccountLoginType::DeviceCode,
            }),
            check_ack: Box::new(|ack| match ack {
                Ack::LoginChallenge { verification_url, user_code } => {
                    assert_eq!(
                        verification_url.as_deref(),
                        Some("https://example.test/verify")
                    );
                    assert_eq!(user_code.as_deref(), Some("CODE-1"));
                }
                other => panic!("expected a login challenge, got {other:?}"),
            }),
        },
        Case {
            name: "cancel-login",
            method: "account/loginCancel",
            command: Prepared::Ready(Command::CancelLogin),
            expected_params: Value::Null,
            check_ack: Box::new(|ack| match ack {
                Ack::LoginCancelled { cancelled } => assert!(cancelled),
                other => panic!("expected a cancelled login, got {other:?}"),
            }),
        },
        Case {
            name: "log-out",
            method: "account/logout",
            command: Prepared::Ready(Command::LogOut),
            expected_params: Value::Null,
            check_ack: Box::new(|ack| match ack {
                Ack::Account { signed_in, label } => {
                    assert!(!signed_in);
                    assert_eq!(label, None);
                }
                other => panic!("expected account state, got {other:?}"),
            }),
        },
    ];

    assert_eq!(cases.len(), 28, "one row per Command variant");

    for Case { name, method, command, expected_params, check_ack } in cases {
        let command = match command {
            Prepared::Ready(command) => command,
            Prepared::DecideApproval { request_id, session_id, approval, choice, feedback } => {
                let token = stage_token.clone().expect(
                    "list-pending must run before decide-approval so the token is minted",
                );
                Command::DecideApproval {
                    request_id,
                    session_id,
                    approval,
                    choice,
                    stage_token: Some(token),
                    feedback,
                }
            }
        };
        let ack = adapter
            .send(command)
            .unwrap_or_else(|error| panic!("{name}: send failed: {error:?}"));

        // Exactly one new wire request per command: the record is flushed
        // before the puppet answers, so it is on disk by now.
        let all = read_records(&records);
        assert_eq!(
            all.len(),
            seen + 1,
            "{name}: expected exactly one new recorded request"
        );
        seen = all.len();
        let record = &all[all.len() - 1];
        assert_eq!(record.method, method, "{name}: wire method");
        assert_eq!(record.params, expected_params, "{name}: wire params");

        check_ack(ack.clone());
        if name == "list-pending" {
            match ack {
                Ack::PendingWork { approvals, .. } => {
                    stage_token = approvals.into_iter().next().and_then(|a| a.stage_token);
                    assert!(stage_token.is_some(), "list-pending must mint the token");
                }
                other => panic!("expected pending work, got {other:?}"),
            }
        }
    }

    adapter.shutdown();
    let _ = std::fs::remove_file(&records);
    // The pump reports the child's exit as one connection notice, then
    // stops — the same contract `roundtrip.rs` pins down.
    let rx = adapter.events();
    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(provider::ProviderEvent::ConnectionLost { .. }) => {}
        other => panic!("expected the single exit notice, got {other:?}"),
    }
    assert!(
        rx.recv_timeout(Duration::from_secs(1)).is_err(),
        "the exit notice must arrive exactly once"
    );
}
