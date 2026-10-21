//! What comes back outside a command: the render stream plus the
//! out-of-band things a delta cannot carry.
//!
//! The render direction is already neutral, so this stays thin: a session
//! id with its [`aui_protocol::Delta`]s, and one variant per thing that is
//! not transcript — a connection ending, an approval raised, a question
//! asked. Small on purpose: anything a delta can say, the delta says.

use aui_protocol::{Delta, PermissionMode};

use crate::facts::{SessionHeader, SessionSide, TurnOutcome};

/// Something the provider did on its own — not the answer to a command.
#[derive(Clone, Debug, PartialEq)]
pub enum ProviderEvent {
    /// Transcript movement in `session_id`: fold these into the session in
    /// order. `session_id` is absent only for connection-level notices that
    /// belong to no session; a late delta for a gone turn is ignored by the
    /// fold, never an error.
    Deltas {
        /// The session that moved; absent when the notice is
        /// connection-level.
        session_id: Option<String>,
        /// Render-ready updates, in order.
        deltas: Vec<Delta>,
    },
    /// The provider raised an approval: decide it with
    /// [`crate::Command::DecideApproval`]. The full card also arrives as a
    /// delta; this is the tap on the shoulder, carrying only what a
    /// decision needs.
    ApprovalRequested {
        /// The owning session.
        session_id: String,
        /// The approval to decide.
        approval_id: String,
        /// One-line human summary of what is being approved.
        headline: String,
    },
    /// The provider asked a question: settle it with the question commands.
    /// The full prompt also arrives as a delta; this carries only what a
    /// reply needs.
    QuestionRaised {
        /// The owning session.
        session_id: String,
        /// The prompt to settle.
        question_id: String,
        /// One-line human summary of what is being asked.
        headline: String,
    },
    /// The session's approval mode as the provider now reports it: what
    /// the composer chip shows. Sent when a session starts in a mode and
    /// whenever the backend says it moved — a pick the backend took, or
    /// its own fallback — so the chip never shows a mode nobody set.
    ApprovalModeChanged {
        /// The owning session.
        session_id: String,
        /// The mode in effect.
        mode: PermissionMode,
    },
    /// The turn an earlier [`crate::Ack::TurnAccepted`] named streams
    /// under `turn_id`: from here on that turn is `turn_id` in every
    /// delta, and anything keyed by the ack's id maps across. Sent by an
    /// adapter whose backend names a turn only once it speaks (Claude
    /// Code: the turn is its first API message id, the one its stored
    /// history replays under), never by one whose ack already carries
    /// the streamed id.
    TurnIdentified {
        /// The owning session.
        session_id: String,
        /// The turn id the acknowledgement carried.
        accepted: String,
        /// The id the turn streams under.
        turn_id: String,
    },
    /// The reasoning effort a session actually runs at, when the adapter
    /// knows it differs from what the chip would otherwise read: a resumed
    /// thread still carrying an earlier level, or a Default pick the
    /// backend has no reset for. `None` is the model's default. Sent only
    /// by an adapter that can see the level in force (Codex: the
    /// `thread/resume` answer's `reasoningEffort`); the chip follows it
    /// without recording a new pick.
    EffortInForce {
        /// The owning session.
        session_id: String,
        /// The level in force, as the backend spells it.
        effort: Option<String>,
    },
    /// The session's model catalog changed after the open answered: ask
    /// again with [`crate::Command::ListModels`]. Claude Code acks the
    /// open before the child's `initialize` answer (which carries the
    /// catalog, effort levels included) lands, so a list asked for at
    /// attach can find nothing but the offline fallback (owner 2026-10-21:
    /// the effort menu offered only Default).
    ModelCatalogChanged {
        /// The owning session.
        session_id: String,
    },
    /// The session's facts beside its transcript changed: the whole side as
    /// it stands now. Sent by an adapter that tracks such facts (Muse:
    /// context, queue, retry, pending prompts, cursor), only when they moved.
    SessionFacts {
        /// The owning session.
        session_id: String,
        /// The facts now.
        side: SessionSide,
    },
    /// The session's header changed: model, working directory, branch.
    HeaderChanged {
        /// The owning session.
        session_id: String,
        /// The header now.
        header: SessionHeader,
    },
    /// A turn began running, before it says anything: what the stop button
    /// and the working row stand on. An adapter whose turns open with
    /// their first delta need not send it.
    TurnRunning {
        /// The owning session.
        session_id: String,
        /// The running turn.
        turn_id: String,
    },
    /// A turn ended, and how. The transcript's own `TurnFinished` carries
    /// the footer; this carries what the app acts on (a failure's
    /// sign-in hint, the row's outcome).
    TurnSettled {
        /// The owning session.
        session_id: String,
        /// The turn that ended.
        turn_id: String,
        /// How.
        outcome: TurnOutcome,
    },
    /// A submission left the transcript and its words come back to the
    /// composer: a retracted turn, or a queued one reclaimed.
    PromptRestored {
        /// The owning session.
        session_id: String,
        /// The queued turn that was reclaimed, when it was one.
        turn_id: Option<String>,
        /// The words it was sent with.
        text: String,
    },
    /// A session's live status as a row shows it — running, and what needs
    /// the person — for sessions no view follows.
    StatusChanged {
        /// The session.
        session_id: String,
        /// A turn is running.
        running: bool,
        /// What is waiting on the person.
        attention: Vec<crate::facts::Attention>,
    },
    /// The credential in effect changed (a sign-in finished elsewhere, a
    /// sign-out, a key appeared in the environment): connection-level.
    AccountChanged {
        /// The lane now.
        lane: crate::AccountLane,
        /// Its display label, when the provider sends one.
        label: Option<String>,
    },
    /// A sign-in flow this client started finished.
    LoginCompleted {
        /// How it ended.
        outcome: crate::LoginOutcome,
        /// The provider's display words for a refusal, when it sent any.
        /// Never key material.
        message: Option<String>,
    },
    /// A new usage reading landed: connection-level.
    UsageChanged {
        /// The reading.
        report: crate::UsageReport,
    },
    /// Words the provider wants on the session's banner: something it did
    /// on its own that the person should know (a sandbox it could not
    /// restore after a resume). Not an error of any command.
    Notice {
        /// The owning session.
        session_id: String,
        /// The banner's words.
        text: String,
    },
    /// A page asked for with [`crate::Command::PageTranscript`] has landed
    /// in this stream: its deltas came before this event. `next_cursor` is
    /// where the next page starts, `None` at the end. A backend whose pages
    /// travel the event stream sends it so the caller can tell a page that
    /// was answered from one that was drawn.
    TranscriptPaged {
        /// The paged session.
        session_id: String,
        /// Where the next page starts; `None` when this was the last.
        next_cursor: Option<String>,
    },
    /// The connection is back after a [`ProviderEvent::ConnectionLost`]:
    /// followed sessions were re-attached and resume from where they were.
    ConnectionRestored,
    /// The provider is gone: child exited, socket down, heartbeat lost.
    /// Reconnect, then resume from the last observed position.
    ConnectionLost {
        /// What went wrong.
        reason: String,
    },
}
