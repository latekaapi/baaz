//! Seam-level tests: the enforced gate and the stored-history commands.
//!
//! All offline: no `claude` process is ever spawned. Stored history is
//! exercised by planting a fixture copy under a fake `$HOME`.

use provider::{
    Ack, Command, Provider, ProviderAdapter, ProviderError, ProviderEvent, QuestionAnswer,
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
            effort: None,
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

/// Resume replays the stored transcript exactly once and names the model.
///
/// Plants the live-captured `stored-history.jsonl` (one "Say R1" turn, no
/// `result` frame in the file) as the stored transcript, then:
/// * `PageTranscript` folds it to one user bubble, one assistant answer,
///   and one finish — the finish synthesised from the stored usage, so a
///   reopened view settles instead of hanging on "working";
/// * the finish carries the stored model (`message.model`);
/// * `ResumeSession` (over a stub child — `/usr/bin/true` exits at once,
///   so no `claude` ever runs) emits the same deltas as a `Deltas` event
///   before any live delta;
/// * a NEW stream echo (fresh uuid) still bubbles afterwards — the W4d
///   seeding swallows only the stored uuid, never the next turn.
#[test]
fn resume_replays_stored_history_once_with_its_model() {
    use provider_claude_code::{fold::ClaudeFold, frame::decode_line, history};

    let root = std::env::temp_dir().join("cc-seam-test-resume");
    let _ = std::fs::remove_dir_all(&root);
    let cwd = root.join("work");
    let home = root.join("home");
    std::fs::create_dir_all(&cwd).expect("cwd");
    let resolved = history::resolve_cwd(&cwd).expect("resolves");
    let slug = history::slug_for_cwd(&resolved);
    let dir = home.join(".claude").join("projects").join(&slug);
    std::fs::create_dir_all(&dir).expect("slug dir");
    let session_id = "af18b5bb-1ff5-4ba2-825c-94804b78834e";
    let fixture = std::fs::read_to_string(format!(
        "{}/../../fixtures/claude-code/stored-history.jsonl",
        env!("CARGO_MANIFEST_DIR")
    ))
    .expect("fixture reads");
    assert!(
        fixture.contains(session_id),
        "the fixture must carry the session the test reopens"
    );
    std::fs::write(dir.join(format!("{session_id}.jsonl")), &fixture).expect("plant");

    let adapter =
        ClaudeCodeAdapter::new("claude").with_home(home.clone()).with_workspace(cwd.clone());

    // The page a reopen shows: bubble, answer, finish — each exactly once.
    let ack = adapter
        .dispatch(Command::PageTranscript {
            session_id: session_id.into(),
            after: None,
            limit: 100,
            backward: false,
        })
        .expect("stored transcript pages");
    let (deltas, next) = match ack {
        Ack::TranscriptPage { deltas, next_cursor } => (deltas, next_cursor),
        other => panic!("expected a page, got {other:?}"),
    };
    assert!(next.is_none(), "one page holds the whole turn");
    let bubbles: Vec<&str> = deltas
        .iter()
        .filter_map(|delta| match delta {
            aui_protocol::Delta::TurnStarted {
                turn: aui_protocol::Turn::User { text, .. },
            } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(bubbles, ["Say R1 and nothing else"], "the prior prompt renders once: {bubbles:?}");
    let answers: Vec<&str> = deltas
        .iter()
        .filter_map(|delta| match delta {
            aui_protocol::Delta::BlockAdded {
                block: aui_protocol::Block::Text { text, .. },
                ..
            } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(answers, ["R1"], "the prior answer renders once: {answers:?}");
    let finishes: Vec<&aui_protocol::TurnMeta> = deltas
        .iter()
        .filter_map(|delta| match delta {
            aui_protocol::Delta::TurnFinished { meta, .. } => Some(meta),
            _ => None,
        })
        .collect();
    assert_eq!(finishes.len(), 1, "the stored turn closes once, or the view hangs working");
    assert_eq!(finishes[0].model, "claude-opus-5", "the chip's model comes from history");
    assert!(finishes[0].tokens_in > 0, "the footer keeps the stored usage: {:?}", finishes[0]);

    // Resume over a stub child replays the same history as an event
    // (`/usr/bin/true` exits at once with no stdout: the pump ends, the
    // replay below is the only history source).
    let resumed = ClaudeCodeAdapter::new("/usr/bin/true")
        .with_home(home)
        .with_workspace(cwd);
    let ack = resumed
        .dispatch(Command::ResumeSession {
            request_id: "r-1".into(),
            session_id: session_id.into(),
            cursor: None,
            metadata_only: false,
        })
        .expect("resume lands over the stub");
    assert!(
        matches!(ack, Ack::Session { session_id: ref held, .. } if held == session_id),
        "resume keeps the id: {ack:?}"
    );
    let replayed = resumed
        .events()
        .try_iter()
        .filter_map(|event| match event {
            ProviderEvent::Deltas { deltas, .. } => Some(deltas),
            _ => None,
        })
        .flatten()
        .collect::<Vec<_>>();
    assert_eq!(replayed, deltas, "resume replays what the page serves — one copy");

    // The next turn still bubbles: seeding swallowed the stored uuid only.
    let mut fold = ClaudeFold::new();
    for line in fixture.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(frame) = decode_line(line) else { continue };
        if let provider_claude_code::frame::Frame::UserText { uuid, .. } = &frame {
            fold.mark_user_echo_seen(uuid);
        }
    }
    let echo = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Say R2"}]},"session_id":"SES","uuid":"u-fresh-2"}"#
        .replace("SES", session_id);
    let frame = decode_line(&echo).expect("stream echo decodes");
    let next: Vec<_> = {
        let mut live = fold.apply(&frame).into_iter().collect::<Vec<_>>();
        // A second arrival of the STORED echo stays silent.
        let stored_echo = fixture
            .lines()
            .find_map(|line| {
                decode_line(line).ok().filter(|frame| {
                    matches!(
                        frame,
                        provider_claude_code::frame::Frame::UserText { uuid, .. }
                        if uuid == "6aeb89c4-f5f1-422e-98fc-fd78222e37e3"
                    )
                })
            })
            .expect("the fixture carries its user echo");
        live.extend(fold.apply(&stored_echo));
        live
    };
    assert_eq!(
        next.iter()
            .filter_map(|delta| match delta {
                aui_protocol::Delta::TurnStarted {
                    turn: aui_protocol::Turn::User { text, .. },
                } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>(),
        ["Say R2"],
        "the new turn bubbles; the stored echo does not repeat"
    );

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
