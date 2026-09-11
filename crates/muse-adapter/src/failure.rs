//! Turning a `turn/completed` failure into words a person would use.
//!
//! MSP settles a failed turn with `{kind, message, retryable}` and, separately,
//! a free-text `reason` on the terminal. Both are written for a log:
//! `modelError` is not a sentence, and `resume_reconcile:orphaned_by_process_loss`
//! is not an explanation. This module is the one place that translates them, so
//! the transcript, the dialog and any future toast all say the same thing.
//!
//! Two rules it keeps:
//!
//! * **The provider's own message is never rewritten.** It becomes the detail
//!   line verbatim, because it is the only part of the failure that knows
//!   anything specific.
//! * **The raw reason code is never thrown away.** A sentence the table knows
//!   is drawn *above* the code, not instead of it — a code you can paste into a
//!   bug report is worth more than a tidy card.

use muse_client::schema::TurnErrorKind;

/// The `TurnErrorKind` for a wire string, or [`TurnErrorKind::Unknown`].
fn parse_kind(kind: &str) -> TurnErrorKind {
    serde_json::from_value(serde_json::Value::String(kind.to_owned()))
        .unwrap_or_else(|_| TurnErrorKind::Unknown(kind.to_owned()))
}

/// The headline and the body of a failed turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    /// The card's title, e.g. `"Model error"`.
    pub title: String,
    /// The card's detail: the provider's message, then the humanized reason and
    /// its raw code where there is one.
    pub detail: String,
}

/// The title for a failure class.
///
/// An unknown kind is shown **verbatim**: MSP's error kinds are an open enum,
/// and a class this build has never heard of is still more informative as its
/// own wire name than as "Error".
pub fn title(kind: &str) -> String {
    match parse_kind(kind) {
        TurnErrorKind::ModelError => "Model error".to_owned(),
        TurnErrorKind::ConfigError => "Configuration error".to_owned(),
        TurnErrorKind::StepLimit => "Step limit reached".to_owned(),
        TurnErrorKind::EnvironmentError => "Environment error".to_owned(),
        TurnErrorKind::LaunchError => "Launch error".to_owned(),
        TurnErrorKind::ProjectionError => "Projection error".to_owned(),
        TurnErrorKind::LogError => "Log error".to_owned(),
        TurnErrorKind::WorkflowLaunchError => "Workflow launch error".to_owned(),
        TurnErrorKind::AuthRequired => "Authentication required".to_owned(),
        // The open enum drops the string it could not name, so the raw one is
        // what the card shows: a class this build has never heard of is still
        // more informative as its own wire name than as "Error".
        TurnErrorKind::Unknown(_) => kind.to_owned(),
    }
}

/// The sentence a known `reason` code stands for, or `None`.
///
/// The codes are `namespace:detail`. The namespace says which part of the
/// runtime gave up; the detail says what it gave up on. Only codes actually
/// observed on the wire (or documented in the tdd) are listed — a guess here
/// would be a lie in the transcript.
pub fn reason_sentence(reason: &str) -> Option<&'static str> {
    Some(match reason {
        "resume_reconcile:orphaned_by_process_loss" => {
            "The turn was orphaned when the session's process was lost."
        }
        "resume_reconcile:orphaned_by_restart" => {
            "The turn was orphaned when the session was restarted."
        }
        "resume_reconcile:unknown_terminal" => {
            "The turn had already ended, and the runtime could not tell how."
        }
        "interrupt:user" => "You interrupted the turn.",
        "interrupt:shutdown" => "The turn stopped because Muse was shutting down.",
        "cancel:user" => "You cancelled the turn.",
        "cancel:superseded" => "A newer submission replaced this turn.",
        "step_limit:exceeded" => "The turn used its whole step budget.",
        "provider:overloaded" => "The provider was overloaded and stopped the turn.",
        "provider:context_exhausted" => "The turn ran out of context window.",
        "workflow:launch_failed" => "The workflow this turn would have launched could not start.",
        _ => return None,
    })
}

/// The whole card: a title from the kind, the provider's message, and the
/// reason spelled out with its code kept.
///
/// `reason` is the terminal's free-text reason, which is present on cancelled
/// turns as often as on failed ones and is `None` when the runtime measured
/// nothing worth saying.
pub fn humanize(kind: &str, message: &str, reason: Option<&str>) -> Failure {
    let mut detail = String::new();
    if !message.trim().is_empty() {
        detail.push_str(message.trim());
    }
    if let Some(reason) = reason.map(str::trim).filter(|r| !r.is_empty()) {
        if !detail.is_empty() {
            detail.push('\n');
        }
        match reason_sentence(reason) {
            // The sentence first, then the code on its own line: a person reads
            // the first, a bug report carries the second.
            Some(sentence) => {
                detail.push_str(sentence);
                detail.push('\n');
                detail.push_str(reason);
            }
            None => detail.push_str(reason),
        }
    }
    if detail.is_empty() {
        detail.push_str("Muse gave no detail.");
    }
    Failure { title: title(kind), detail }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_known_kind_has_a_sentence_of_a_title() {
        let kinds = [
            ("modelError", "Model error"),
            ("configError", "Configuration error"),
            ("stepLimit", "Step limit reached"),
            ("environmentError", "Environment error"),
            ("launchError", "Launch error"),
            ("projectionError", "Projection error"),
            ("logError", "Log error"),
            ("workflowLaunchError", "Workflow launch error"),
            ("authRequired", "Authentication required"),
        ];
        for (kind, expected) in kinds {
            assert_eq!(title(kind), expected);
        }
    }

    #[test]
    fn an_unknown_kind_keeps_its_wire_name() {
        assert_eq!(title("quantumError"), "quantumError");
    }

    #[test]
    fn every_known_reason_becomes_a_sentence() {
        // One assertion per known code, which is what makes adding a code to
        // the table a deliberate act rather than a drive-by.
        let codes = [
            "resume_reconcile:orphaned_by_process_loss",
            "resume_reconcile:orphaned_by_restart",
            "resume_reconcile:unknown_terminal",
            "interrupt:user",
            "interrupt:shutdown",
            "cancel:user",
            "cancel:superseded",
            "step_limit:exceeded",
            "provider:overloaded",
            "provider:context_exhausted",
            "workflow:launch_failed",
        ];
        for code in codes {
            let sentence = reason_sentence(code).unwrap_or_else(|| panic!("{code} has no sentence"));
            assert!(sentence.ends_with('.'), "{code}: a sentence ends in a full stop");
            assert!(
                sentence.chars().next().is_some_and(char::is_uppercase),
                "{code}: a sentence starts with a capital"
            );
        }
    }

    #[test]
    fn a_known_reason_keeps_its_raw_code_underneath() {
        let failure = humanize(
            "modelError",
            "the provider closed the stream",
            Some("resume_reconcile:orphaned_by_process_loss"),
        );
        assert_eq!(failure.title, "Model error");
        assert_eq!(
            failure.detail,
            "the provider closed the stream\nThe turn was orphaned when the session's process was lost.\nresume_reconcile:orphaned_by_process_loss"
        );
    }

    #[test]
    fn an_unknown_reason_is_shown_as_it_came() {
        let failure = humanize("stepLimit", "", Some("something:new"));
        assert_eq!(failure.detail, "something:new");
    }

    #[test]
    fn a_failure_with_nothing_to_say_still_says_so() {
        let failure = humanize("logError", "  ", None);
        assert_eq!(failure.detail, "Muse gave no detail.");
    }
}
