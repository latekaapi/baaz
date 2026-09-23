//! The second implementation, exercised — and it is not optional.
//!
//! These tests drive the shared [`provider::scripted::ScriptedProvider`]
//! stand-in for "a provider that is not muse" end to end through the trait.

use std::time::Duration;

use aui_protocol::{Block, Session, Turn, Provider as Backend};
use provider::{
    Ack, Command, ConnectInfo, Provider, ProviderError, ProviderEvent, QuestionAnswer,
    SubmissionPart,
};
use provider::scripted::ScriptedProvider;

/// Drain every event currently queued, without waiting for more.
fn drain(provider: &Provider) -> Vec<ProviderEvent> {
    let rx = provider.events();
    let mut out = Vec::new();
    while let Ok(event) = rx.try_recv() {
        out.push(event);
    }
    // One short wait covers a send that raced the first poll: the scripted
    // provider emits synchronously inside `send`, so anything still missing
    // after this window is genuinely absent.
    std::thread::sleep(Duration::from_millis(20));
    while let Ok(event) = rx.try_recv() {
        out.push(event);
    }
    out
}

#[test]
fn scripted_provider_drives_a_session_end_to_end_through_the_trait() {
    let mut provider = Provider::new(ScriptedProvider::new());
    assert_eq!(provider.id(), Backend::Codex);

    let handshake = provider.connect(&ConnectInfo::new("baaz", "0.1.0")).expect("connect");
    assert_eq!(handshake.provider, Backend::Codex);

    let Ack::Session { session_id, .. } = provider
        .send(Command::OpenSession {
            request_id: "r-1".into(),
            workspace: None,
            model: None,
            model_provider: None,
        })
        .expect("open")
    else {
        panic!("open-session must ack a session");
    };

    let Ack::TurnAccepted { .. } = provider
        .send(Command::SubmitInput {
            request_id: "r-2".into(),
            session_id: session_id.clone(),
            parts: vec![SubmissionPart::Text("hello".into())],
            display_text: None,
        })
        .expect("submit")
    else {
        panic!("submit-input must ack a turn");
    };

    let mut session = Session::new(&session_id, Backend::Codex, "scripted", "/tmp");
    let mut applied = 0;
    for event in drain(&provider) {
        let ProviderEvent::Deltas { deltas, .. } = event else {
            panic!("scripted providers emit only deltas");
        };
        for delta in deltas {
            assert!(session.apply(delta), "every canned delta must land");
            applied += 1;
        }
    }
    assert_eq!(applied, 4, "the whole script must arrive");

    let user = session
        .turns
        .iter()
        .find(|turn| matches!(turn, Turn::User { .. }))
        .expect("the person's message became a user turn");
    let Turn::User { text, .. } = user else { unreachable!() };
    assert_eq!(text, "hello");

    let assistant = session
        .turns
        .iter()
        .find(|turn| matches!(turn, Turn::Assistant { .. }))
        .expect("the reply became an assistant turn");
    let body = assistant
        .blocks()
        .iter()
        .filter_map(|block| match block {
            Block::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect::<String>();
    assert_eq!(body, "echo: hello");
}

/// Every command the scripted provider refuses, each paired with the
/// capability name its refusal must carry.
///
/// The list is built through the `classify` helper's exhaustive `match`
/// with no catch-all arm: adding a `Command` variant breaks compilation there until
/// the new variant is classified — implemented (a `None` arm, handled in
/// `send`) or refused (a `Some` arm plus a constructor below). A bare
/// `commands.len()` count could never do that: it fires only when someone
/// edits the list without fixing the count, not when a new variant is never
/// listed at all.
///
/// The expected names are string literals, deliberately *not*
/// `command.capability()`: the production `send` arm calls that same method,
/// so comparing against it could not catch a bug in `capability()` itself.
fn refused_commands() -> Vec<(Command, &'static str)> {
    /// Implemented commands pass through as `None`; every refused one comes
    /// back as `Some` with its capability name. Exhaustive — no wildcard.
    ///
    /// Implemented commands pass through as `None` and are handled in
    /// `dispatch`; refused ones never reach it — the [`Provider`] gate
    /// refuses them first.
    fn classify(command: Command) -> Option<(Command, &'static str)> {
        match command {
            Command::OpenSession { .. }
            | Command::SubmitInput { .. }
            | Command::FollowSession { .. } => None,
            command @ Command::ResumeSession { .. } => Some((command, "resume-session")),
            command @ Command::ForkSession { .. } => Some((command, "fork-session")),
            command @ Command::ListSessions { .. } => Some((command, "list-sessions")),
            command @ Command::ReadSession { .. } => Some((command, "read-session")),
            command @ Command::CompactSession { .. } => Some((command, "compact-session")),
            command @ Command::SelectModel { .. } => Some((command, "select-model")),
            command @ Command::SelectApprovalMode { .. } => {
                Some((command, "select-approval-mode"))
            }
            command @ Command::RunShell { .. } => Some((command, "run-shell")),
            command @ Command::SteerInput { .. } => Some((command, "steer-input")),
            command @ Command::InterruptTurn { .. } => Some((command, "interrupt-turn")),
            command @ Command::CancelTurn { .. } => Some((command, "cancel-turn")),
            command @ Command::ReclaimQueued { .. } => Some((command, "reclaim-queued")),
            command @ Command::ListModels { .. } => Some((command, "list-models")),
            command @ Command::DecideApproval { .. } => Some((command, "decide-approval")),
            command @ Command::ListPending { .. } => Some((command, "list-pending")),
            command @ Command::AnswerQuestion { .. } => Some((command, "answer-question")),
            command @ Command::DismissQuestion { .. } => Some((command, "dismiss-question")),
            command @ Command::ClarifyQuestion { .. } => Some((command, "clarify-question")),
            command @ Command::PageTranscript { .. } => Some((command, "page-transcript")),
            command @ Command::UnfollowSession { .. } => Some((command, "unfollow-session")),
            command @ Command::ReadStoredOutput { .. } => Some((command, "read-stored-output")),
            command @ Command::ReadAccount => Some((command, "read-account")),
            command @ Command::BeginLogin { .. } => Some((command, "begin-login")),
            command @ Command::CancelLogin => Some((command, "cancel-login")),
            command @ Command::LogOut => Some((command, "log-out")),
        }
    }

    // OpenSession, SubmitInput and FollowSession are the three the double
    // implements; everything else must refuse.
    let candidates: Vec<Command> = vec![
        Command::ResumeSession {
            request_id: "r".into(),
            session_id: "s".into(),
            cursor: None,
            metadata_only: false,
        },
        Command::ForkSession {
            request_id: "r".into(),
            session_id: "s".into(),
            through_turn: None,
            metadata_only: false,
        },
        Command::ListSessions { cursor: None, limit: None, workspace: None },
        Command::ReadSession { session_id: "s".into(), metadata_only: false },
        Command::CompactSession { request_id: "r".into(), session_id: "s".into(), through_turn: None },
        Command::SelectModel {
            request_id: "r".into(),
            session_id: "s".into(),
            model: "m".into(),
            model_provider: None,
        },
        Command::SelectApprovalMode {
            request_id: "r".into(),
            session_id: "s".into(),
            mode: aui_protocol::PermissionMode::OnRequest,
        },
        Command::RunShell { request_id: "r".into(), session_id: "s".into(), command: "ls".into() },
        Command::SteerInput {
            request_id: "r".into(),
            session_id: "s".into(),
            expected_turn: "t".into(),
            parts: vec![SubmissionPart::Text("x".into())],
        },
        Command::InterruptTurn {
            request_id: "r".into(),
            session_id: "s".into(),
            turn: None,
            retract: false,
        },
        Command::CancelTurn { request_id: "r".into(), session_id: "s".into(), turn: None },
        Command::ReclaimQueued { request_id: "r".into(), session_id: "s".into(), turn: "t".into() },
        Command::ListModels { session: None },
        Command::DecideApproval {
            request_id: "r".into(),
            session_id: "s".into(),
            approval: "a".into(),
            choice: "c".into(),
            stage_token: None,
            feedback: None,
        },
        Command::ListPending { session_id: "s".into() },
        Command::AnswerQuestion {
            request_id: "r".into(),
            session_id: "s".into(),
            question: "q".into(),
            answers: vec![QuestionAnswer {
                question_id: "q0".into(),
                selected_label: Some("yes".into()),
                selected_labels: None,
                free_text: None,
                note: None,
            }],
        },
        Command::DismissQuestion {
            request_id: "r".into(),
            session_id: "s".into(),
            question: "q".into(),
            reason: None,
        },
        Command::ClarifyQuestion {
            request_id: "r".into(),
            session_id: "s".into(),
            question: "q".into(),
            text: "x".into(),
        },
        Command::PageTranscript { session_id: "s".into(), after: None, limit: 10, backward: false },
        Command::UnfollowSession { session_id: "s".into() },
        Command::ReadStoredOutput {
            session_id: "s".into(),
            item: "i".into(),
            output: "o".into(),
            offset: 0,
            length: None,
        },
        Command::ReadAccount,
        Command::BeginLogin { api_key: None },
        Command::CancelLogin,
        Command::LogOut,
    ];

    let mut refused = Vec::with_capacity(candidates.len());
    for command in candidates {
        match classify(command) {
            Some(pair) => refused.push(pair),
            None => panic!("the refusal list names an implemented command"),
        }
    }
    // Backstop for edits to the list itself (duplicates, dropped entries):
    // new variants are caught by `classify`, not by this count.
    assert_eq!(refused.len(), 25, "the refusal list drifted from the 25 unimplemented commands");
    refused
}

/// Every command the scripted provider does not implement answers
/// `Unsupported` naming the capability — never `Ok`.
#[test]
fn unimplemented_commands_are_typed_refusals_not_success() {
    let mut provider = Provider::new(ScriptedProvider::new());
    provider.connect(&ConnectInfo::new("baaz", "0.1.0")).expect("connect");

    for (command, expected) in &refused_commands() {
        match provider.send(command.clone()) {
            Err(ProviderError::Unsupported { capability, .. }) => {
                assert_eq!(capability, *expected, "the refusal must name the capability");
            }
            other => panic!("{expected} must refuse, not answer {other:?}"),
        }
    }
}
