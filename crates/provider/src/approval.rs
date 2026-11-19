//! One approval vocabulary for every provider (docs/82 5.5; docs/81 B S-7,
//! F D3).
//!
//! Before this module each adapter put its own words on its card — Claude
//! Code "Allow / Deny", Codex "Allow once / Allow for this session / Deny /
//! Deny and stop", Muse whatever its server sent — and the app read the
//! kind of an approval off its id. Now there are four answers a person can
//! give, whoever asks: [`ApprovalAnswer`]. Each adapter maps its native
//! choices onto them, offers only the ones its wire can carry (a scope a
//! provider cannot do is absent, never disabled), and says which it offers
//! on [`crate::ProviderEvent::ApprovalRequested`] as typed
//! [`ApprovalOption`]s.

use aui_protocol::{ApprovalChoice, ApprovalDecision, ApprovalScope};

/// The four answers an approval can take, in card order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ApprovalAnswer {
    /// Allow this one action.
    Once,
    /// Allow it, and its like, for the rest of this session.
    ThisSession,
    /// Allow it, and its like, from now on in this folder: a rule the
    /// provider keeps.
    AlwaysHere,
    /// Refuse it; the agent goes on another way.
    Deny,
}

impl ApprovalAnswer {
    /// Every answer, in card order.
    pub const ALL: [ApprovalAnswer; 4] =
        [ApprovalAnswer::Once, ApprovalAnswer::ThisSession, ApprovalAnswer::AlwaysHere, ApprovalAnswer::Deny];

    /// The button's words: the same on every provider.
    pub fn label(self) -> &'static str {
        match self {
            ApprovalAnswer::Once => "Once",
            ApprovalAnswer::ThisSession => "This session",
            ApprovalAnswer::AlwaysHere => "Always here",
            ApprovalAnswer::Deny => "Deny",
        }
    }

    /// What choosing it means, in the transcript's terms.
    pub fn decision(self) -> ApprovalDecision {
        match self {
            ApprovalAnswer::Once => ApprovalDecision::Once,
            ApprovalAnswer::ThisSession => ApprovalDecision::ApprovedForSession,
            ApprovalAnswer::AlwaysHere => ApprovalDecision::PolicyAmendment,
            ApprovalAnswer::Deny => ApprovalDecision::Deny,
        }
    }

    /// How far it reaches.
    pub fn scope(self) -> ApprovalScope {
        match self {
            ApprovalAnswer::Once | ApprovalAnswer::Deny => ApprovalScope::ThisCommand,
            ApprovalAnswer::ThisSession => ApprovalScope::ThisSession,
            ApprovalAnswer::AlwaysHere => ApprovalScope::ThisWorktree,
        }
    }

    /// The card's button for this answer, carrying the adapter's own
    /// choice id (`native`), which rides
    /// [`crate::Command::DecideApproval`] back verbatim.
    pub fn choice(self, native: impl Into<String>) -> ApprovalChoice {
        ApprovalChoice {
            id: native.into(),
            label: self.label().to_owned(),
            decision: self.decision(),
            scope: self.scope(),
            rule_preview: None,
            accepts_feedback: false,
        }
    }

    /// Which answer a native `(decision, scope)` pair is, when it is one
    /// of the four: a refusal written into a rule, or an allow reaching
    /// past this folder, has no place in the vocabulary and is not offered.
    pub fn of(decision: ApprovalDecision, scope: ApprovalScope) -> Option<ApprovalAnswer> {
        match (decision, scope) {
            (ApprovalDecision::Once, ApprovalScope::ThisCommand) => Some(ApprovalAnswer::Once),
            (ApprovalDecision::ApprovedForSession, _) | (ApprovalDecision::Once, ApprovalScope::ThisSession) => {
                Some(ApprovalAnswer::ThisSession)
            }
            (ApprovalDecision::Always | ApprovalDecision::PolicyAmendment, ApprovalScope::ThisWorktree) => {
                Some(ApprovalAnswer::AlwaysHere)
            }
            (ApprovalDecision::Always | ApprovalDecision::PolicyAmendment, ApprovalScope::ThisSession) => {
                Some(ApprovalAnswer::ThisSession)
            }
            (ApprovalDecision::Deny, ApprovalScope::ThisCommand) => Some(ApprovalAnswer::Deny),
            // A backend whose only refusal also stops the turn (Muse's
            // "Reject") still offers Deny; where a plain refusal exists
            // beside it, [`Self::restate`] keeps the plain one.
            (ApprovalDecision::Abort, ApprovalScope::ThisCommand) => Some(ApprovalAnswer::Deny),
            _ => None,
        }
    }

    /// A card's choices restated in the vocabulary: each native choice that
    /// is one of the four answers, relabelled, the first of each answer
    /// kept, in card order. What an adapter whose backend supplies its own
    /// choices (Muse) shows.
    pub fn restate(choices: &[ApprovalChoice]) -> Vec<ApprovalChoice> {
        let mut kept: Vec<ApprovalChoice> = Vec::new();
        for answer in ApprovalAnswer::ALL {
            let is = |choice: &&ApprovalChoice| ApprovalAnswer::of(choice.decision, choice.scope) == Some(answer);
            // A plain refusal wins over one that also stops the turn.
            let found = choices
                .iter()
                .filter(is)
                .find(|choice| choice.decision != ApprovalDecision::Abort)
                .or_else(|| choices.iter().find(is));
            if let Some(native) = found {
                let mut choice = answer.choice(native.id.clone());
                choice.rule_preview = native.rule_preview.clone();
                choice.accepts_feedback = native.accepts_feedback;
                kept.push(choice);
            }
        }
        kept
    }
}

/// One answer an approval offers, and the provider's own choice id that
/// carries it: what [`crate::ProviderEvent::ApprovalRequested`] lists, so
/// the app decides an approval without reading anything off its id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalOption {
    /// The answer, in the one vocabulary.
    pub answer: ApprovalAnswer,
    /// The adapter's choice id for it, echoed in
    /// [`crate::Command::DecideApproval`].
    pub choice: String,
}

impl ApprovalOption {
    /// The options a card's choices offer, in card order: every choice
    /// that is one of the four answers.
    pub fn of_choices(choices: &[ApprovalChoice]) -> Vec<ApprovalOption> {
        choices
            .iter()
            .filter_map(|choice| {
                ApprovalAnswer::of(choice.decision, choice.scope)
                    .map(|answer| ApprovalOption { answer, choice: choice.id.clone() })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each answer's button is its own label, and restates as itself.
    #[test]
    fn every_answer_round_trips_through_its_choice() {
        for answer in ApprovalAnswer::ALL {
            let choice = answer.choice(format!("native-{answer:?}"));
            assert_eq!(choice.label, answer.label());
            assert_eq!(ApprovalAnswer::of(choice.decision, choice.scope), Some(answer));
            assert_eq!(
                ApprovalOption::of_choices(std::slice::from_ref(&choice)),
                [ApprovalOption { answer, choice: format!("native-{answer:?}") }]
            );
        }
        let labels: Vec<&str> = ApprovalAnswer::ALL.iter().map(|answer| answer.label()).collect();
        assert_eq!(labels, ["Once", "This session", "Always here", "Deny"]);
    }

    /// Native choices outside the vocabulary are not offered; the first of
    /// each answer is kept, with its rule preview and feedback field.
    #[test]
    fn restating_keeps_one_choice_per_answer_and_drops_the_rest() {
        let native = |id: &str, decision, scope| ApprovalChoice {
            id: id.into(),
            label: format!("native {id}"),
            decision,
            scope,
            rule_preview: Some(format!("rule {id}")),
            accepts_feedback: id == "deny",
        };
        let choices = vec![
            native("allow", ApprovalDecision::Once, ApprovalScope::ThisCommand),
            native("prefix", ApprovalDecision::PolicyAmendment, ApprovalScope::ThisWorktree),
            native("exact", ApprovalDecision::PolicyAmendment, ApprovalScope::ThisWorktree),
            native("global", ApprovalDecision::PolicyAmendment, ApprovalScope::Global),
            native("deny", ApprovalDecision::Deny, ApprovalScope::ThisCommand),
            native("stop", ApprovalDecision::Abort, ApprovalScope::ThisCommand),
        ];
        let restated = ApprovalAnswer::restate(&choices);
        let ids: Vec<&str> = restated.iter().map(|choice| choice.id.as_str()).collect();
        assert_eq!(ids, ["allow", "prefix", "deny"], "a plain refusal wins over deny-and-stop");
        // A backend whose only refusal stops the turn still offers Deny.
        let reject_only = vec![
            native("allow", ApprovalDecision::Once, ApprovalScope::ThisCommand),
            native("abort", ApprovalDecision::Abort, ApprovalScope::ThisCommand),
        ];
        let restated_reject = ApprovalAnswer::restate(&reject_only);
        assert_eq!(
            restated_reject.iter().map(|choice| (choice.id.as_str(), choice.label.as_str())).collect::<Vec<_>>(),
            [("allow", "Once"), ("abort", "Deny")]
        );
        let labels: Vec<&str> = restated.iter().map(|choice| choice.label.as_str()).collect();
        assert_eq!(labels, ["Once", "Always here", "Deny"], "This session is absent, not disabled");
        assert_eq!(restated[1].rule_preview.as_deref(), Some("rule prefix"));
        assert!(restated[2].accepts_feedback);
    }
}
