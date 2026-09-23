//! Seam-level tests: the enforced gate and the stored-history commands.
//!
//! All offline: no `claude` process is ever spawned. Stored history is
//! exercised by planting a fixture copy under a fake `$HOME`.

use provider::{
    Ack, Command, Provider, ProviderAdapter, ProviderError, QuestionAnswer,
};
use provider_claude_code::ClaudeCodeAdapter;

fn answer_command() -> Command {
    Command::AnswerQuestion {
        request_id: "q-1".into(),
        session_id: "s-1".into(),
        question: "prompt-1".into(),
        answers: vec![QuestionAnswer {
            question_id: "prompt-1".into(),
            selected_label: Some("yes".into()),
            selected_labels: None,
            free_text: None,
            note: None,
        }],
    }
}

#[test]
fn questions_are_refused_by_the_gate_never_spelled_ok() {
    // Questions is Unavailable: Provider::send refuses before dispatch runs.
    let adapter = ClaudeCodeAdapter::new("claude");
    let provider = Provider::new(adapter);
    let error = provider.send(answer_command()).expect_err("questions must not succeed");
    assert!(error.is_unsupported());
    assert_eq!(error.capability(), Some("answer-question"));
}

#[test]
fn unsupported_is_never_spelled_as_success() {
    let adapter = ClaudeCodeAdapter::new("claude");
    // Login has no CLI surface: refused, not Ok.
    let error = adapter
        .dispatch(Command::BeginLogin { api_key: None })
        .expect_err("begin-login must not succeed");
    assert!(matches!(error, ProviderError::Unsupported { .. }));
    // Interrupt was never probed: rejected with a reason, not Ok.
    let error = adapter
        .dispatch(Command::InterruptTurn {
            request_id: "i-1".into(),
            session_id: "s-1".into(),
            turn: None,
            retract: false,
        })
        .expect_err("interrupt must not succeed");
    assert!(matches!(error, ProviderError::Rejected { .. }));
    // Submit with no child: unavailable, not Ok.
    let error = adapter
        .dispatch(Command::SubmitInput {
            request_id: "t-1".into(),
            session_id: "s-1".into(),
            parts: vec![provider::SubmissionPart::Text("hi".into())],
            display_text: None,
        })
        .expect_err("submit with no child must not succeed");
    assert!(matches!(error, ProviderError::Unavailable { .. }));
}

#[test]
fn stored_history_serves_read_and_page_but_never_guesses() {
    // Plant basic.jsonl as the stored transcript for one session under a
    // fake home, at the resolved slug for a temp cwd.
    let root = std::env::temp_dir().join("cc-seam-test-history");
    let _ = std::fs::remove_dir_all(&root);
    let cwd = root.join("work");
    let home = root.join("home");
    std::fs::create_dir_all(&cwd).expect("cwd");
    let resolved = std::fs::canonicalize(&cwd).expect("resolves");
    let slug: String = resolved.to_string_lossy().replace('/', "-");
    let dir = home.join(".claude").join("projects").join(slug);
    std::fs::create_dir_all(&dir).expect("slug dir");
    let fixture = std::fs::read_to_string(format!(
        "{}/../../fixtures/claude-code/basic.jsonl",
        env!("CARGO_MANIFEST_DIR")
    ))
    .expect("fixture reads");
    std::fs::write(dir.join("sess-stored.jsonl"), &fixture).expect("plant");

    let adapter = ClaudeCodeAdapter::new("claude").with_home(home);
    let ack = adapter
        .dispatch(Command::ReadSession { session_id: "sess-stored".into(), metadata_only: false })
        .expect("stored session reads");
    assert!(matches!(ack, Ack::Session { session_id, .. } if session_id == "sess-stored"));

    let ack = adapter
        .dispatch(Command::PageTranscript {
            session_id: "sess-stored".into(),
            after: None,
            limit: 100,
            backward: false,
        })
        .expect("stored transcript pages");
    let (deltas, next) = match ack {
        Ack::TranscriptPage { deltas, next_cursor } => (deltas, next_cursor),
        other => panic!("expected a page, got {other:?}"),
    };
    assert!(!deltas.is_empty(), "basic.jsonl pages to real deltas");
    assert!(next.is_none(), "one page holds the whole turn");

    // Unknown session: honest unavailable, never a neighboring file.
    let error = adapter
        .dispatch(Command::ReadSession { session_id: "sess-absent".into(), metadata_only: false })
        .expect_err("absent session must not read");
    assert!(matches!(error, ProviderError::Unavailable { .. }));

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn read_account_reports_the_live_meter_label() {
    let adapter = ClaudeCodeAdapter::new("claude");
    let ack = adapter.dispatch(Command::ReadAccount).expect("account reads");
    match ack {
        Ack::Account { signed_in, label } => {
            assert!(signed_in);
            assert!(label.is_none(), "no meter seen yet, no label invented");
        }
        other => panic!("expected account, got {other:?}"),
    }
}
