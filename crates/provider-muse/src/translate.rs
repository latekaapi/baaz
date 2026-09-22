//! Neutral commands into MSP, view events out through the fold.
//!
//! This is where every wire spelling lives: the only crate in the seam
//! allowed to depend on `muse-client`. Each [`Command`] arm builds the
//! typed schema params, makes the one matching client call, and maps the
//! result back to a neutral [`Ack`]; transport failures map to
//! [`ProviderError`] with no wire type leaking across.

use muse_client::schema::{
    AccountLoginStartParams, AccountLoginType, AccountStateKind, ApprovalDecideParams,
    ApprovalMode, ApprovalRequirementRef, ApprovalSubject, ItemReadOutputParams,
    ModelSelection, SessionCompactParams, SessionForkParams, SessionListParams,
    SessionReadParams, SessionResumeParams, SessionSetApprovalModeParams, SessionSetModelParams,
    SessionStartParams, SessionUserShellParams, TurnInterruptParams, TurnStartParams,
    TurnSteerParams, TurnUnqueueParams, TurnCancelParams, UserInputAnswer, UserInputAnswerParams,
    UserInputCancelParams, UserInputClarification, UserInputClarifyParams, UserInputRequestParams,
    ApprovalRequestParams, ViewPageDirection, ViewPageParams, ViewSubscribeParams,
    ViewUnsubscribeParams,
};
use muse_client::{MuseClient, MuseError};
use provider::{Ack, Command, ModelSummary, PendingApproval, PendingQuestion, ProviderError,
    QuestionAnswer, SessionSummary, SubmissionPart};

/// The longest headline a pending-item summary may be. Headlines are taps
/// on the shoulder, not content — the full card arrives as a delta.
const HEADLINE_LIMIT: usize = 120;

/// Shorten `text` to one line of at most [`HEADLINE_LIMIT`] characters.
fn headline(mut text: String) -> String {
    text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() > HEADLINE_LIMIT {
        let cut: String = text.chars().take(HEADLINE_LIMIT - 1).collect();
        format!("{cut}…")
    } else {
        text
    }
}

/// The closed neutral approval mode, translated to the wire's closed mode.
fn approval_mode(mode: aui_protocol::PermissionMode) -> ApprovalMode {
    match mode {
        aui_protocol::PermissionMode::AllowAll => ApprovalMode::AllowAll,
        aui_protocol::PermissionMode::OnRequest => ApprovalMode::OnRequest,
        aui_protocol::PermissionMode::PromptUnmatched => ApprovalMode::PromptUnmatched,
        aui_protocol::PermissionMode::DenyUnmatched => ApprovalMode::DenyUnmatched,
    }
}

/// A neutral submission part, translated to the wire's content part.
fn input_part(part: SubmissionPart) -> muse_client::schema::TurnInputPart {
    use muse_client::schema::TurnInputPart as Wire;
    match part {
        SubmissionPart::Text(text) => Wire::text(text),
        SubmissionPart::Image { base64_data, media_type } => {
            Wire::image(base64_data, media_type)
        }
    }
}

/// A neutral answer, translated to the wire's answer.
fn answer(answer: QuestionAnswer) -> UserInputAnswer {
    UserInputAnswer {
        free_text: answer.free_text,
        note: answer.note,
        question_id: answer.question_id,
        selected_label: answer.selected_label,
        selected_labels: answer.selected_labels,
    }
}

/// One-line summary of what an approval gates, from its subject.
pub fn approval_headline(request: &ApprovalRequestParams) -> String {
    let subject: &ApprovalSubject = &request.subject;
    if let Some(command) = &subject.command {
        return headline(format!("Run `{command}`"));
    }
    if let Some(path) = &subject.path {
        match &subject.access {
            Some(access) => return headline(format!("{access} `{path}`")),
            None => return headline(format!("Access `{path}`")),
        }
    }
    if let Some(host) = &subject.host {
        return headline(format!("Connect to {host}"));
    }
    headline(format!("Approval ({})", subject.kind))
}

/// One-line summary of what a question asks: its first header, else its
/// first question text.
pub fn question_headline(request: &UserInputRequestParams) -> String {
    match request.questions.first() {
        Some(question) if !question.header.is_empty() => headline(question.header.clone()),
        Some(question) => headline(question.question.clone()),
        None => format!("Question {}", request.user_input_id),
    }
}

/// A transport failure, translated to the neutral error. The wire detail
/// stays behind the seam; only the human reason crosses it.
pub fn transport_error(error: MuseError) -> ProviderError {
    match error {
        MuseError::Closed => {
            ProviderError::Unavailable { reason: "the agent process exited".into() }
        }
        MuseError::Timeout(method) => {
            ProviderError::Unavailable { reason: format!("the agent never answered {method}") }
        }
        MuseError::Io(error) => {
            ProviderError::Unavailable { reason: format!("agent I/O failed: {error}") }
        }
        MuseError::Rpc(error) => {
            ProviderError::Rejected { reason: format!("refused ({}): {}", error.code, error.message) }
        }
        MuseError::Json(error) => {
            ProviderError::Rejected { reason: format!("unusable answer: {error}") }
        }
        MuseError::Protocol(line) => {
            ProviderError::Rejected { reason: format!("unframable reply: {line}") }
        }
    }
}

/// Run one neutral command against the client. One arm per [`Command`]
/// variant; each builds the typed params, makes the one matching call, and
/// maps the result to a neutral [`Ack`]. Paging also takes the fold: it is
/// the one command whose ack carries transcript, so its page is folded to
/// deltas before it crosses the seam.
pub fn dispatch(
    client: &MuseClient,
    fold: &std::sync::Mutex<muse_adapter::MuseFold>,
    command: Command,
) -> Result<Ack, ProviderError> {
    match command {
        Command::OpenSession { request_id, workspace, model, provider } => {
            let result = client
                .session_start(&SessionStartParams {
                    approval_mode: None,
                    command_id: request_id,
                    config: None,
                    model_id: model,
                    provider_id: provider,
                    session_id: None,
                    workspace_root: workspace,
                })
                .map_err(transport_error)?;
            Ok(Ack::Session {
                session_id: result.session.session_id,
                title: result.session.title,
            })
        }
        Command::ResumeSession { request_id, session_id, cursor, metadata_only } => {
            let result = client
                .session_resume(&SessionResumeParams {
                    command_id: request_id,
                    config: None,
                    cursor,
                    exclude_items: metadata_only.then_some(true),
                    history: None,
                    session_id,
                })
                .map_err(transport_error)?;
            Ok(Ack::Session {
                session_id: result.session.session_id,
                title: result.session.title,
            })
        }
        Command::ForkSession { request_id, session_id, through_turn, metadata_only } => {
            let result = client
                .session_fork(&SessionForkParams {
                    command_id: request_id,
                    cut_point: through_turn.map(|last_turn_id| {
                        muse_client::schema::ForkCutPoint { last_turn_id }
                    }),
                    exclude_items: metadata_only.then_some(true),
                    session_id,
                })
                .map_err(transport_error)?;
            Ok(Ack::Session {
                session_id: result.session.session_id,
                title: result.session.title,
            })
        }
        Command::ListSessions { cursor, limit, workspace } => {
            let result = client
                .session_list(&SessionListParams {
                    cursor,
                    limit,
                    updated_after: None,
                    workspace_root: workspace,
                })
                .map_err(transport_error)?;
            Ok(Ack::SessionIndex {
                sessions: result
                    .sessions
                    .into_iter()
                    .map(|session| SessionSummary {
                        session_id: session.session_id,
                        title: session.title,
                    })
                    .collect(),
                next_cursor: result.next_cursor,
            })
        }
        Command::ReadSession { session_id, metadata_only } => {
            let result = client
                .session_read(&SessionReadParams {
                    exclude_items: metadata_only.then_some(true),
                    session_id,
                })
                .map_err(transport_error)?;
            Ok(Ack::Session {
                session_id: result.session.session_id,
                title: result.session.title,
            })
        }
        Command::CompactSession { request_id, session_id, through_turn } => {
            client
                .session_compact(&SessionCompactParams {
                    command_id: request_id,
                    session_id,
                    turn_id: through_turn,
                })
                .map_err(transport_error)?;
            Ok(Ack::Accepted)
        }
        Command::SelectModel { request_id, session_id, model, provider } => {
            client
                .session_set_model(&SessionSetModelParams {
                    command_id: request_id,
                    model: ModelSelection {
                        display_label: None,
                        model_id: model,
                        profile_id: None,
                        provider_id: provider,
                    },
                    session_id,
                })
                .map_err(transport_error)?;
            Ok(Ack::Accepted)
        }
        Command::SelectApprovalMode { request_id, session_id, mode } => {
            client
                .session_set_approval_mode(&SessionSetApprovalModeParams {
                    command_id: request_id,
                    mode: approval_mode(mode),
                    session_id,
                })
                .map_err(transport_error)?;
            Ok(Ack::Accepted)
        }
        Command::RunShell { request_id, session_id, command } => {
            client
                .session_user_shell(&SessionUserShellParams {
                    command_id: request_id,
                    command_text: command,
                    session_id,
                })
                .map_err(transport_error)?;
            Ok(Ack::Accepted)
        }
        Command::SubmitInput { request_id, session_id, parts, display_text } => {
            let result = client
                .turn_start(&TurnStartParams {
                    command_id: request_id,
                    display_text,
                    if_busy: None,
                    input: parts.into_iter().map(input_part).collect(),
                    reasoning_effort: None,
                    session_id,
                })
                .map_err(transport_error)?;
            Ok(Ack::TurnAccepted { turn_id: result.turn_id })
        }
        Command::SteerInput { request_id, session_id, expected_turn, parts } => {
            client
                .turn_steer(&TurnSteerParams {
                    command_id: request_id,
                    expected_turn_id: expected_turn,
                    input: parts.into_iter().map(input_part).collect(),
                    reasoning_effort: None,
                    session_id,
                })
                .map_err(transport_error)?;
            Ok(Ack::Accepted)
        }
        Command::InterruptTurn { request_id, session_id, turn, retract } => {
            client
                .turn_interrupt(&TurnInterruptParams {
                    command_id: request_id,
                    retract: Some(retract),
                    session_id,
                    turn_id: turn,
                })
                .map_err(transport_error)?;
            Ok(Ack::Accepted)
        }
        Command::CancelTurn { request_id, session_id, turn } => {
            client
                .turn_cancel(&TurnCancelParams {
                    command_id: request_id,
                    session_id,
                    turn_id: turn,
                })
                .map_err(transport_error)?;
            Ok(Ack::Accepted)
        }
        Command::ReclaimQueued { request_id, session_id, turn } => {
            client
                .turn_unqueue(&TurnUnqueueParams {
                    command_id: request_id,
                    session_id,
                    turn_id: turn,
                })
                .map_err(transport_error)?;
            Ok(Ack::Accepted)
        }
        Command::ListModels { session } => {
            let result = client
                .model_list(&muse_client::schema::ModelListParams { session_id: session })
                .map_err(transport_error)?;
            Ok(Ack::ModelCatalog {
                models: result
                    .models
                    .into_iter()
                    .map(|model| ModelSummary {
                        id: model.model_id,
                        label: model.display_label,
                        active: model.is_active,
                    })
                    .collect(),
                provider: result.provider_id,
            })
        }
        Command::DecideApproval { request_id, session_id, approval, choice, stage, feedback } => {
            client
                .approval_decide(&ApprovalDecideParams {
                    approval_id: approval.clone(),
                    choice_id: choice,
                    command_id: request_id,
                    feedback,
                    requirement_id: ApprovalRequirementRef {
                        approval_id: approval,
                        source_index: stage,
                    },
                    session_id,
                })
                .map_err(transport_error)?;
            Ok(Ack::Accepted)
        }
        Command::ListPending { session_id } => {
            let result = client
                .approval_list_pending(&muse_client::schema::ApprovalListPendingParams {
                    session_id,
                })
                .map_err(transport_error)?;
            Ok(Ack::PendingWork {
                approvals: result
                    .approvals
                    .iter()
                    .map(|approval| PendingApproval {
                        id: approval.approval_id.clone(),
                        session_id: approval.session_id.clone(),
                        headline: approval_headline(approval),
                    })
                    .collect(),
                questions: result
                    .user_inputs
                    .iter()
                    .map(|prompt| PendingQuestion {
                        id: prompt.user_input_id.clone(),
                        session_id: prompt.session_id.clone(),
                        headline: question_headline(prompt),
                    })
                    .collect(),
            })
        }
        Command::AnswerQuestion { request_id, session_id, question, answers } => {
            client
                .user_input_answer(&UserInputAnswerParams {
                    answers: answers.into_iter().map(answer).collect(),
                    command_id: request_id,
                    session_id,
                    user_input_id: question,
                })
                .map_err(transport_error)?;
            Ok(Ack::Accepted)
        }
        Command::DismissQuestion { request_id, session_id, question, reason } => {
            client
                .user_input_cancel(&UserInputCancelParams {
                    command_id: request_id,
                    reason,
                    session_id,
                    user_input_id: question,
                })
                .map_err(transport_error)?;
            Ok(Ack::Accepted)
        }
        Command::ClarifyQuestion { request_id, session_id, question, text } => {
            client
                .user_input_clarify(&UserInputClarifyParams {
                    clarification: UserInputClarification {
                        content: text,
                        format: "text".into(),
                    },
                    command_id: request_id,
                    session_id,
                    user_input_id: question,
                })
                .map_err(transport_error)?;
            Ok(Ack::Accepted)
        }
        Command::PageTranscript { session_id, after, limit, backward } => {
            let page = client
                .view_page(&ViewPageParams {
                    anchor: None,
                    cursor: after,
                    direction: page_direction(backward),
                    limit,
                    session_id,
                })
                .map_err(transport_error)?;
            let mut fold = fold.lock().expect("fold mutex");
            let (deltas, next_cursor) = fold_page(&mut fold, page)?;
            Ok(Ack::TranscriptPage { deltas, next_cursor })
        }
        Command::FollowSession { session_id, after } => {
            client
                .view_subscribe(&ViewSubscribeParams { after, session_id })
                .map_err(transport_error)?;
            Ok(Ack::Accepted)
        }
        Command::UnfollowSession { session_id } => {
            client
                .view_unsubscribe(&ViewUnsubscribeParams { session_id })
                .map_err(transport_error)?;
            Ok(Ack::Accepted)
        }
        Command::ReadStoredOutput { session_id, item, output, offset, length } => {
            let result = client
                .item_read_output(&ItemReadOutputParams {
                    item_id: item,
                    length_bytes: length,
                    offset_bytes: Some(offset),
                    output_ref: output,
                    session_id,
                })
                .map_err(transport_error)?;
            Ok(Ack::StoredOutput { content: result.content, complete: result.eof })
        }
        Command::ReadAccount => {
            let state = client.account_read().map_err(transport_error)?;
            Ok(Ack::Account {
                signed_in: state.state != AccountStateKind::LoggedOut,
                label: state.label,
            })
        }
        Command::BeginLogin { api_key } => {
            let result = client
                .account_login_start(&AccountLoginStartParams {
                    api_key: api_key.clone(),
                    r#type: if api_key.is_some() {
                        AccountLoginType::ApiKey
                    } else {
                        AccountLoginType::DeviceCode
                    },
                })
                .map_err(transport_error)?;
            Ok(Ack::LoginChallenge {
                verification_url: result.verification_url,
                user_code: result.user_code,
            })
        }
        Command::CancelLogin => {
            let result = client.account_login_cancel().map_err(transport_error)?;
            Ok(Ack::LoginCancelled { cancelled: result.cancelled })
        }
        Command::LogOut => {
            let state = client.account_logout().map_err(transport_error)?;
            Ok(Ack::Account {
                signed_in: state.state != AccountStateKind::LoggedOut,
                label: state.label,
            })
        }
    }
}

/// Fold one page of transcript notifications to deltas. Paging is the one
/// command whose ack carries transcript, so it needs the fold — it lives on
/// the adapter, next to the pump, rather than in [`dispatch`].
pub fn fold_page(
    fold: &mut muse_adapter::MuseFold,
    page: muse_client::schema::ViewPageResult,
) -> Result<(Vec<provider::Delta>, Option<String>), ProviderError> {
    let mut deltas = Vec::new();
    for event in page.events {
        let params = serde_json::to_value(&event.params).map_err(|error| {
            ProviderError::Rejected { reason: format!("unusable page event: {error}") }
        })?;
        let (cursor, session_id) = match &params {
            serde_json::Value::Object(map) => (
                map.get("viewCursor").and_then(|cursor| cursor.as_str()).map(str::to_owned),
                map.get("sessionId").and_then(|id| id.as_str()).map(str::to_owned),
            ),
            _ => (None, None),
        };
        deltas.extend(fold.apply(muse_client::MuseEvent::Notification {
            method: event.method,
            params,
            cursor,
            session_id,
        }));
    }
    Ok((deltas, page.next_cursor))
}

/// Page direction for [`Command::PageTranscript`]: forward from the start
/// unless `backward` asks for the head end first.
pub fn page_direction(backward: bool) -> Option<ViewPageDirection> {
    backward.then_some(ViewPageDirection::Backward)
}
