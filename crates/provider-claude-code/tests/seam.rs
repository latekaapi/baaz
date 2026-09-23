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
    use provider_claude_code::history;
    // Plant basic.jsonl as the stored transcript for one session under a
    // fake home, at the resolved slug for a temp cwd — via the REAL slug
    // function, so this test fails if the slugging logic drifts from the
    // lookup path (a hand-rolled transform would pass with no slug logic
    // at all).
    let root = std::env::temp_dir().join("cc-seam-test-history");
    let _ = std::fs::remove_dir_all(&root);
    let cwd = root.join("work");
    let home = root.join("home");
    std::fs::create_dir_all(&cwd).expect("cwd");
    let resolved = history::resolve_cwd(&cwd).expect("resolves");
    let slug = history::slug_for_cwd(&resolved);
    let dir = home.join(".claude").join("projects").join(&slug);
    std::fs::create_dir_all(&dir).expect("slug dir");
    let fixture = std::fs::read_to_string(format!(
        "{}/../../fixtures/claude-code/basic.jsonl",
        env!("CARGO_MANIFEST_DIR")
    ))
    .expect("fixture reads");
    std::fs::write(dir.join("sess-stored.jsonl"), &fixture).expect("plant");
    // The resolver names exactly this file — the lookup below must agree.
    assert_eq!(
        history::stored_transcript_path(&cwd, "sess-stored", Some(&home)),
        Some(dir.join("sess-stored.jsonl"))
    );

    // Plant the SAME session id nowhere else, but a DIFFERENT session id
    // under a DIFFERENT workspace's slug directory: asking for it from
    // this workspace must answer unavailable, never that other file.
    let other_cwd = root.join("other-work");
    std::fs::create_dir_all(&other_cwd).expect("other cwd");
    let other_slug =
        history::slug_for_cwd(&history::resolve_cwd(&other_cwd).expect("other resolves"));
    assert_ne!(other_slug, slug, "slugs must differ for this test to mean anything");
    let other_dir = home.join(".claude").join("projects").join(&other_slug);
    std::fs::create_dir_all(&other_dir).expect("other slug dir");
    std::fs::write(other_dir.join("sess-decoy.jsonl"), &fixture).expect("plant decoy");

    let adapter =
        ClaudeCodeAdapter::new("claude").with_home(home).with_workspace(cwd);
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

    // THE POINT: this file exists, but under another workspace's slug.
    // A directory scan would serve it; the resolver must not.
    let error = adapter
        .dispatch(Command::ReadSession { session_id: "sess-decoy".into(), metadata_only: false })
        .expect_err("another workspace's file must not read here");
    assert!(matches!(error, ProviderError::Unavailable { .. }));
    let error = adapter
        .dispatch(Command::PageTranscript {
            session_id: "sess-decoy".into(),
            after: None,
            limit: 100,
            backward: false,
        })
        .expect_err("another workspace's file must not page here");
    assert!(matches!(error, ProviderError::Unavailable { .. }));

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn read_account_reports_the_live_meter_label() {
    let adapter = ClaudeCodeAdapter::new("claude");
    let ack = adapter.dispatch(Command::ReadAccount).expect("account reads");
    match ack {
        Ack::Account { signed_in, label } => {
            // No meter reading observed yet: no login claimed. `claude
            // --version` succeeds logged-out, so a fresh adapter must read
            // false — fail closed. (The true branch — a meter reading seen
            // from the live child — is covered by the unit tests in
            // src/lib.rs, which can reach the fold.)
            assert!(!signed_in, "no reading seen, no login claimed");
            assert!(label.is_none(), "no meter seen yet, no label invented");
        }
        other => panic!("expected account, got {other:?}"),
    }
}
