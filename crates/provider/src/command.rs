//! Everything the app asks a provider to do, neutrally named.
//!
//! The list is derived from what the app actually sends today (the `muse`
//! transport's request surface), but no variant, field, or capability name
//! may carry a wire spelling: adapters translate *into* this vocabulary.
//!
//! One flat enum, deliberately: a single dispatch point where an exhaustive
//! `match` forces every provider to answer every capability explicitly —
//! which is what makes [`crate::ProviderError::Unsupported`] honest instead
//! of a forgotten arm. Commands that the wire gives an idempotency handle
//! carry `request_id`; pure queries carry none.

use aui_protocol::PermissionMode;

/// Who the command is for. [`aui_protocol::Provider`] already names all
/// seven agents, so this is a reuse, not a second enum.
pub type ProviderId = aui_protocol::Provider;

/// One ordered piece of a submission. Text-only plus images: file mentions
/// stay inside text (`@relative/path`), and provider-specific invocations
/// (skill shortcuts, slash commands) have no neutral spelling and stay out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubmissionPart {
    /// Plain prompt text. Multiple parts join in order.
    Text(String),
    /// An image by value: base64 payload plus its media type. The one binary
    /// shape every agent transport accepts.
    Image {
        /// Base64-encoded bytes.
        base64_data: String,
        /// e.g. `"image/png"`.
        media_type: String,
    },
}

/// One answer inside an [`Command::AnswerQuestion`]: which question, the
/// pick (one label, several labels, or free text), plus an optional note.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuestionAnswer {
    /// The question being answered.
    pub question_id: String,
    /// The chosen option label (single-pick questions).
    pub selected_label: Option<String>,
    /// The chosen option labels (multi-pick questions).
    pub selected_labels: Option<Vec<String>>,
    /// Free-text answer (free-text questions).
    pub free_text: Option<String>,
    /// An optional note to the model alongside the answer.
    pub note: Option<String>,
}

/// Everything the app asks a provider to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// Open a new session in `workspace`.
    OpenSession {
        /// Client-minted idempotency handle: retrying with the same id
        /// returns the first result instead of opening twice.
        request_id: String,
        /// Working directory the session runs in.
        workspace: Option<String>,
        /// Initial model; provider default when absent.
        model: Option<String>,
        /// Upstream agent route; provider default when absent.
        provider: Option<String>,
    },
    /// Re-attach to a stored session. With `cursor` the provider serves only
    /// the suffix after it; with `metadata_only` it serves no history and
    /// the caller pages it later.
    ResumeSession {
        /// Client-minted idempotency handle.
        request_id: String,
        /// The session to load.
        session_id: String,
        /// A previously observed position; absent means from the start.
        cursor: Option<String>,
        /// Serve metadata only; page history separately.
        metadata_only: bool,
    },
    /// Branch a session into a new one, copying history through
    /// `through_turn` (inclusive) or all completed turns when absent.
    ForkSession {
        /// Client-minted idempotency handle.
        request_id: String,
        /// The source session.
        session_id: String,
        /// Copy history through this turn, inclusive.
        through_turn: Option<String>,
        /// Serve metadata only; page history separately.
        metadata_only: bool,
    },
    /// List stored sessions, newest activity first.
    ListSessions {
        /// Opaque page cursor from a prior result; absent means first page.
        cursor: Option<String>,
        /// Page size.
        limit: Option<u32>,
        /// Only sessions in this workspace.
        workspace: Option<String>,
    },
    /// Read a stored session without attaching to it.
    ReadSession {
        /// The session to read.
        session_id: String,
        /// Serve metadata only; page history separately.
        metadata_only: bool,
    },
    /// Compact a session's context through `through_turn`, or the latest
    /// run when absent.
    CompactSession {
        /// Client-minted idempotency handle.
        request_id: String,
        /// The session to compact.
        session_id: String,
        /// The run whose context to compact.
        through_turn: Option<String>,
    },
    /// Switch a session's model. Admission only: a running turn picks the
    /// new model up at its next model-call boundary.
    SelectModel {
        /// Client-minted idempotency handle.
        request_id: String,
        /// The target session.
        session_id: String,
        /// Catalog model id.
        model: String,
        /// Provider routing; absent means no change.
        provider: Option<String>,
    },
    /// Switch a session's approval enforcement mode. Applies forward: an
    /// in-flight approval is not decided retroactively.
    SelectApprovalMode {
        /// Client-minted idempotency handle.
        request_id: String,
        /// The target session.
        session_id: String,
        /// The mode to select. Closed set, never constructed.
        mode: PermissionMode,
    },
    /// Run a shell command inside the session, outside any turn. Still
    /// subject to the approval policy; output arrives as events.
    RunShell {
        /// Client-minted idempotency handle.
        request_id: String,
        /// The target session.
        session_id: String,
        /// The shell command to run.
        command: String,
    },
    /// Submit input to a session: start a turn, or queue behind the running
    /// one. The ack says which turn carries it — never derive that locally.
    SubmitInput {
        /// Client-minted idempotency handle; the fresh turn derives from it.
        request_id: String,
        /// The target session.
        session_id: String,
        /// Ordered content parts; required and non-empty.
        parts: Vec<SubmissionPart>,
        /// Presentation form of the prompt for transcripts. Durable, never
        /// model-visible.
        display_text: Option<String>,
    },
    /// Inject input into the running turn. `expected_turn` closes the race
    /// where the turn changes under the caller: input for turn A can never
    /// leak into turn B.
    SteerInput {
        /// Client-minted idempotency handle.
        request_id: String,
        /// The target session.
        session_id: String,
        /// The turn believed to be running.
        expected_turn: String,
        /// Ordered content parts; required and non-empty.
        parts: Vec<SubmissionPart>,
    },
    /// Stop the foreground turn (the stop button). With `retract`, an
    /// un-started turn is durably retracted so its prompt can be restored.
    InterruptTurn {
        /// Client-minted idempotency handle.
        request_id: String,
        /// The target session.
        session_id: String,
        /// The exact turn to stop; absent targets the foreground turn.
        turn: Option<String>,
        /// Retract the submission when nothing committed yet.
        retract: bool,
    },
    /// The non-urgent cancel, on the normal lane rather than the priority
    /// one [`Command::InterruptTurn`] takes.
    CancelTurn {
        /// Client-minted idempotency handle.
        request_id: String,
        /// The target session.
        session_id: String,
        /// The exact turn to cancel; absent targets the foreground turn.
        turn: Option<String>,
    },
    /// Reclaim a queued submission. Not a stop: a reclaim that arrives after
    /// its target launched is refused.
    ReclaimQueued {
        /// Client-minted idempotency handle.
        request_id: String,
        /// The target session.
        session_id: String,
        /// The queued turn to reclaim, exactly as the queueing ack minted it.
        turn: String,
    },
    /// List the model catalog. A query, not a command: no idempotency
    /// handle, no events. With `session` the row matching that session's
    /// effective model is flagged active.
    ListModels {
        /// Flag the row matching this session's effective model.
        session: Option<String>,
    },
    /// Decide a pending approval. `stage` must equal the approval's current
    /// stage — the race guard that keeps a stale decision from satisfying a
    /// newer stage. A non-terminal decision satisfies the stage but leaves
    /// the approval pending.
    DecideApproval {
        /// Client-minted idempotency handle.
        request_id: String,
        /// The target session.
        session_id: String,
        /// The approval being decided.
        approval: String,
        /// One of the approval's offered choices.
        choice: String,
        /// The stage this decision is aimed at.
        stage: u32,
        /// Free-text guidance delivered with the decision, on choices that
        /// accept it.
        feedback: Option<String>,
    },
    /// Pull the pending set for a session: approvals and questions, oldest
    /// first. Point-in-time — acting on it stays race-safe through the
    /// guards on [`Command::DecideApproval`] and
    /// [`Command::AnswerQuestion`].
    ListPending {
        /// The session to read.
        session_id: String,
    },
    /// Answer a raised question, one entry per question asked.
    AnswerQuestion {
        /// Client-minted idempotency handle.
        request_id: String,
        /// The target session.
        session_id: String,
        /// The prompt being answered.
        question: String,
        /// One entry per question.
        answers: Vec<QuestionAnswer>,
    },
    /// Decline to answer; the gated tool call resolves cancelled.
    DismissQuestion {
        /// Client-minted idempotency handle.
        request_id: String,
        /// The target session.
        session_id: String,
        /// The prompt being declined.
        question: String,
        /// Why it was declined.
        reason: Option<String>,
    },
    /// Answer with free-form clarification instead of the offered options —
    /// the "let me explain" path.
    ClarifyQuestion {
        /// Client-minted idempotency handle.
        request_id: String,
        /// The target session.
        session_id: String,
        /// The prompt being clarified.
        question: String,
        /// The clarification body.
        text: String,
    },
    /// Page back through a session's transcript. Pages are ascending,
    /// contiguous, and never replay streamed fragments.
    PageTranscript {
        /// The session to page.
        session_id: String,
        /// Exclusive anchor: forward, the page starts strictly after it;
        /// backward, it ends strictly before it. Absent means from the
        /// beginning (forward) or the head (backward).
        after: Option<String>,
        /// Maximum events to return.
        limit: u32,
        /// Page backward from the head instead of forward from the start.
        backward: bool,
    },
    /// Follow a session's live transcript. With `after`, the provider
    /// replays `(after, head]` before any live event — one gapless sequence.
    /// Absent means from now: no replay.
    FollowSession {
        /// The session to follow.
        session_id: String,
        /// Replay history after this position first.
        after: Option<String>,
    },
    /// Stop following a session. Idempotent; does not unload the session.
    UnfollowSession {
        /// The session to stop following.
        session_id: String,
    },
    /// Fetch a tool call's stored full output by byte range, for output the
    /// transcript truncated. Read-only; works on loaded and unloaded
    /// sessions.
    ReadStoredOutput {
        /// The session the item belongs to.
        session_id: String,
        /// The item whose stored output is fetched.
        item: String,
        /// The stored-output reference from the item.
        output: String,
        /// Stored-byte offset to start from.
        offset: u64,
        /// Stored bytes requested; provider maximum when absent.
        length: Option<u64>,
    },
    /// Which credential lane is in effect. The only probe: logged-out means
    /// the login screen, anything else means signed in.
    ReadAccount,
    /// Begin the login flow: with `api_key` store and validate a key,
    /// without it start the device-code flow the user completes in a
    /// browser. Key material travels here and must never reach a log.
    BeginLogin {
        /// The API key to store; absent starts the device-code flow.
        api_key: Option<String>,
    },
    /// Abandon the pending device-code flow.
    CancelLogin,
    /// Clear the stored credential.
    LogOut,
}

impl Command {
    /// The capability this command needs, in kebab-case — the same name a
    /// [`crate::ProviderError::Unsupported`] refusal carries, so a test can
    /// assert the refusal names exactly the thing that was asked for.
    pub fn capability(&self) -> &'static str {
        match self {
            Self::OpenSession { .. } => "open-session",
            Self::ResumeSession { .. } => "resume-session",
            Self::ForkSession { .. } => "fork-session",
            Self::ListSessions { .. } => "list-sessions",
            Self::ReadSession { .. } => "read-session",
            Self::CompactSession { .. } => "compact-session",
            Self::SelectModel { .. } => "select-model",
            Self::SelectApprovalMode { .. } => "select-approval-mode",
            Self::RunShell { .. } => "run-shell",
            Self::SubmitInput { .. } => "submit-input",
            Self::SteerInput { .. } => "steer-input",
            Self::InterruptTurn { .. } => "interrupt-turn",
            Self::CancelTurn { .. } => "cancel-turn",
            Self::ReclaimQueued { .. } => "reclaim-queued",
            Self::ListModels { .. } => "list-models",
            Self::DecideApproval { .. } => "decide-approval",
            Self::ListPending { .. } => "list-pending",
            Self::AnswerQuestion { .. } => "answer-question",
            Self::DismissQuestion { .. } => "dismiss-question",
            Self::ClarifyQuestion { .. } => "clarify-question",
            Self::PageTranscript { .. } => "page-transcript",
            Self::FollowSession { .. } => "follow-session",
            Self::UnfollowSession { .. } => "unfollow-session",
            Self::ReadStoredOutput { .. } => "read-stored-output",
            Self::ReadAccount => "read-account",
            Self::BeginLogin { .. } => "begin-login",
            Self::CancelLogin { .. } => "cancel-login",
            Self::LogOut => "log-out",
        }
    }
}
