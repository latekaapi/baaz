//! Muse approval modes and the permission profiles that go with them.
//!
//! A chip mode on Muse is two wire facts: the approval mode
//! (`session/setApprovalMode`) and the sandbox permission profile, which is
//! re-selected on a loaded session by a metadata-only `session/resume` (the
//! one call that admits it after start, mid-turn too — probed 2026-10-16).
//! A pick moves both, ordered so a half-applied pick never leaves the chip
//! reading less access than the session has.

use aui_protocol::PermissionMode;
use muse_client::schema::{ApprovalMode, Session, SessionResumeParams, SessionSetApprovalModeParams};
use muse_client::{new_command_id, MuseClient};
use provider::{ProviderError, Refusal, RefusalHint, Severity};

/// The wire's closed approval mode for a chip mode.
pub fn wire_mode(mode: PermissionMode) -> ApprovalMode {
    match mode {
        PermissionMode::AllowAll => ApprovalMode::AllowAll,
        PermissionMode::OnRequest => ApprovalMode::OnRequest,
        PermissionMode::PromptUnmatched => ApprovalMode::PromptUnmatched,
        PermissionMode::DenyUnmatched => ApprovalMode::DenyUnmatched,
    }
}

/// The metadata-only `session/resume` that re-selects `mode`'s profile on
/// the loaded `session_id`: the view already open replays nothing.
pub fn profile_resume_params(session_id: &str, mode: PermissionMode) -> SessionResumeParams {
    SessionResumeParams {
        command_id: new_command_id(),
        session_id: session_id.to_owned(),
        permission_profile: Some(muse_adapter::permission_profile(mode).to_owned()),
        exclude_items: Some(true),
        ..Default::default()
    }
}

/// The mode whose profile a resumed session should be under, when the
/// profile it reports is another one — or none. `None` when they agree or
/// the session reports no approval mode.
pub fn profile_mismatch(session: &Session) -> Option<PermissionMode> {
    let mode = muse_adapter::permission_mode(&session.approval_mode.as_ref()?.mode);
    let held = session.permission_state.as_ref().and_then(|state| state.profile_id.as_deref());
    (held != Some(muse_adapter::permission_profile(mode))).then_some(mode)
}

/// The chip mode a session reports, if any.
pub fn session_mode(session: &Session) -> Option<PermissionMode> {
    session.approval_mode.as_ref().map(|mode| muse_adapter::permission_mode(&mode.mode))
}

/// How much a mode lets through, for ordering a pick's two steps.
fn access_rank(mode: PermissionMode) -> u8 {
    match mode {
        PermissionMode::DenyUnmatched => 0,
        PermissionMode::PromptUnmatched => 1,
        PermissionMode::OnRequest => 2,
        PermissionMode::AllowAll => 3,
    }
}

/// Move `session_id` from `from` to `to`: mode and profile, in the order
/// that keeps the chip honest. `Ok` when both moved; a refusal otherwise,
/// whose words say what the session now runs under. Blocking.
pub fn pick(
    client: &MuseClient,
    request_id: Option<&str>,
    session_id: &str,
    from: PermissionMode,
    to: PermissionMode,
) -> Result<(), ProviderError> {
    let set_mode = |mode: PermissionMode| {
        client
            .session_set_approval_mode(&SessionSetApprovalModeParams {
                // The pick's own id on its mode step; a restore gets a fresh one.
                command_id: match request_id {
                    Some(id) if mode == to => id.to_owned(),
                    _ => new_command_id(),
                },
                session_id: session_id.to_owned(),
                mode: wire_mode(mode),
            })
            .map(|_| ())
    };
    // The raw call: the answer is not read, so a resume envelope that fails
    // to decode is never mistaken for a refused profile.
    let set_profile = || {
        let params = profile_resume_params(session_id, to);
        client.request("session/resume", serde_json::to_value(&params).ok()).map(|_| ())
    };
    let partial = |reason: String| {
        ProviderError::Refused(Refusal {
            hint: RefusalHint::None,
            title: "The mode did not fully change".into(),
            reason,
            severity: Severity::Banner,
            session_scoped: true,
        })
    };
    if access_rank(to) > access_rank(from) {
        set_mode(to).map_err(|error| crate::errors::refusal(&error))?;
        if let Err(error) = set_profile() {
            let restored = set_mode(from).is_ok();
            return Err(partial(if restored {
                format!(
                    "{} could not be applied: the sandbox profile was refused ({error}). The session stays in {}.",
                    to.label(),
                    from.label()
                )
            } else {
                format!(
                    "{} could not be applied: the sandbox profile was refused ({error}), and the mode could not be put back. The session still runs under the {} sandbox — pick a mode again.",
                    to.label(),
                    from.label()
                )
            }));
        }
    } else {
        set_profile().map_err(|error| crate::errors::refusal(&error))?;
        if let Err(error) = set_mode(to) {
            return Err(partial(format!(
                "The {} sandbox is on, but the approval mode could not change ({error}), so the chip still reads {}.",
                to.label(),
                from.label()
            )));
        }
    }
    Ok(())
}

/// A resumed session whose profile disagrees with its mode is put under the
/// mode's profile; if that is refused, under the read-only profile instead
/// (fail toward less access: the chip must never read less than the session
/// has). `Err` carries the banner words. Blocking.
pub fn reconcile(client: &MuseClient, session: &Session) -> Result<(), String> {
    let Some(mode) = profile_mismatch(session) else { return Ok(()) };
    let held = session
        .permission_state
        .as_ref()
        .and_then(|state| state.profile_id.clone())
        .unwrap_or_else(|| "the host's default".to_owned());
    let select = |mode: PermissionMode| {
        let params = profile_resume_params(&session.session_id, mode);
        client.request("session/resume", serde_json::to_value(&params).ok()).map(|_| ())
    };
    let Err(error) = select(mode) else { return Ok(()) };
    let read_only = mode != PermissionMode::DenyUnmatched && select(PermissionMode::DenyUnmatched).is_ok();
    Err(if read_only {
        format!(
            "This session's sandbox ({held}) could not be matched to {} ({error}), so it now runs read-only. Pick a mode to try again.",
            mode.label()
        )
    } else {
        format!(
            "This session's sandbox ({held}) could not be matched to {} ({error}). Pick a mode to try again.",
            mode.label()
        )
    })
}
