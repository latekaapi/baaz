//! Handoff between providers, as an honest lossy re-prompt (H2).
//!
//! A handoff moves work from one provider to another by starting a **fresh**
//! session on the destination seeded with a context pack — never pretended
//! continuity. The full contract lives in `docs/22-handoff.md`; this file
//! holds the state machine and the pack builder, deliberately free of gpui
//! so the transitions, refusal, fencing and budget are plain unit tests.
//!
//! The views and the application own the wiring around this: the source
//! view renders the [`aui_protocol::Block::Handoff`] card (appended with
//! `append_client_block`, refreshed with `replace_client_block`), and the
//! [`Harness`](crate::app::Harness) owns one [`HandoffRun`] per source
//! session plus the owner-epoch counter that fences stale acks.

#[cfg(test)]
use std::collections::HashMap;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use aui::transcript::{HandoffStep, HandoffStepState, default_handoff_steps};
use aui_protocol::{Block, HandoffItem, HandoffState, Session, TodoState, Turn};

use crate::providers::ProviderId;

/// Tokens are estimated at four characters each, the same rough measure the
/// composer meter uses. It is a budget guard, not a billing number.
pub fn estimate_tokens(text: &str) -> u64 {
    (text.len() as u64).div_ceil(4)
}

/// The context pack's token budget: recent turns are quoted verbatim only
/// while the whole pack stays under this.
pub const PACK_BUDGET_TOKENS: u64 = 8000;

/// Which summary the pack carries: the extractive fallback (the opening
/// lines of the earliest assistant replies, verbatim — what Z8 replaced
/// the "no model summary call runs" rule with), or a model-written one
/// from the checkpoint's cheap side session. The card and the confirm
/// dialog read [`SummaryKind::label`], so both always say which kind it
/// is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SummaryKind {
    /// Opening lines of the earliest replies, verbatim.
    Extractive,
    /// A 4–8 line summary the cheap side-session model wrote.
    Model,
}

impl SummaryKind {
    /// The card's `Carried` detail for the conversation summary.
    pub fn label(self) -> &'static str {
        match self {
            SummaryKind::Extractive => "extractive summary",
            SummaryKind::Model => "model summary",
        }
    }
}

/// The provider-neutral context pack: everything the destination's first
/// turn carries. See `docs/22-handoff.md` for what is never carried.
///
/// The conversation summary is extractive by default — the opening lines
/// of the earliest assistant replies, verbatim — and is upgraded to a
/// model-written one when the checkpoint's side session answers in time
/// (Z8; [`SummaryKind`] says which kind a pack carries).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextPack {
    /// The first user prompt, truncated: the original goal.
    pub goal: String,
    /// The compact conversation summary ([`SummaryKind`] says which kind).
    pub summary: String,
    /// Which summary [`Self::summary`] holds.
    pub summary_kind: SummaryKind,
    /// The last turns verbatim, oldest first: `(role, text)`.
    pub recent: Vec<(String, String)>,
    /// Open todo labels (pending or running, never done).
    pub todos: Vec<String>,
    /// Files touched, as `"verb target"` lines from tool cards.
    pub files: Vec<String>,
    /// The working directory the source session ran in.
    pub workspace: String,
    /// [`estimate_tokens`] over the whole pack text.
    pub tokens: u64,
    /// How many verbatim-eligible turns the source held when the pack was
    /// built (every non-empty user prompt and assistant reply). What
    /// [`pack_covers_every_turn`] compares [`ContextPack::recent`] against:
    /// a pack carrying all of them needs no model-written summary.
    pub source_turns: usize,
}

/// The header every pack is submitted under, naming the source provider.
/// The last line tells the destination to acknowledge rather than
/// re-execute: without it the destination re-ran the original
/// instruction instead of picking up the context.
pub fn pack_header(from: ProviderId) -> String {
    format!(
        "Continuing a session handed off from {}. Context follows.\nReply with a one-sentence acknowledgement of this context and wait for the user's next message; do not re-execute the instructions being handed off.",
        from.label()
    )
}

/// Build the pack from the folded transcript. Provider-neutral: only the
/// goal, an extractive summary, verbatim recent turns within
/// [`PACK_BUDGET_TOKENS`], open todos, touched files and the workspace.
/// Tool call internals, pending approvals, provider memory, images and
/// reasoning traces never enter the pack.
pub fn build_pack(session: &Session, workspace: &str) -> ContextPack {
    let mut goal = String::new();
    let mut assistant_texts: Vec<&str> = Vec::new();
    let mut recent_all: Vec<(String, String)> = Vec::new();
    let mut todos: Vec<String> = Vec::new();
    let mut files: Vec<String> = Vec::new();

    for turn in &session.turns {
        match turn {
            Turn::User { text, .. } => {
                if goal.is_empty() {
                    goal = truncate_chars(text, 500);
                }
                if !text.trim().is_empty() {
                    recent_all.push(("user".to_owned(), text.clone()));
                }
            }
            Turn::Assistant { blocks, .. } => {
                let mut reply = String::new();
                for block in blocks {
                    match block {
                        Block::Text { text, .. } => {
                            if !reply.is_empty() {
                                reply.push('\n');
                            }
                            reply.push_str(text);
                        }
                        Block::Todo { items } => {
                            for item in items {
                                if !matches!(item.state, TodoState::Done) && !item.label.trim().is_empty() {
                                    push_unique(&mut todos, item.label.clone());
                                }
                            }
                        }
                        Block::ToolCall { .. } => {
                            if let Some(call) = block.as_tool_call() {
                                let target = call.target.trim();
                                if !target.is_empty() {
                                    push_unique(&mut files, format!("{} {target}", call.verb.trim()));
                                }
                            }
                        }
                        _ => {}
                    }
                }
                if !reply.trim().is_empty() {
                    assistant_texts.push(block_text_first(blocks));
                    recent_all.push(("assistant".to_owned(), reply));
                }
            }
        }
    }
    if files.len() > 30 {
        files.truncate(30);
    }

    // The extractive summary: the opening lines of the earliest replies.
    let mut summary = assistant_texts
        .iter()
        .take(3)
        .map(|t| truncate_chars(t, 300))
        .filter(|t| !t.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n---\n");
    if summary.is_empty() {
        summary = "(no assistant replies yet)".to_owned();
    }
    if goal.is_empty() {
        goal = "(no user prompt yet)".to_owned();
    }

    // Recent turns, newest first, while the running total stays in budget.
    // The goal, summary, todos, files and workspace reserve ~1500 tokens;
    // the rest is verbatim turns, always whole (never a cut turn).
    let reserve: u64 = 1500;
    let mut recent: Vec<(String, String)> = Vec::new();
    let mut used: u64 = 0;
    for (role, text) in recent_all.iter().rev() {
        let cost = estimate_tokens(text) + 8;
        if used + cost > PACK_BUDGET_TOKENS.saturating_sub(reserve) {
            break;
        }
        used += cost;
        recent.push((role.clone(), text.clone()));
    }
    recent.reverse();

    let mut pack = ContextPack {
        goal,
        summary,
        summary_kind: SummaryKind::Extractive,
        source_turns: recent_all.len(),
        recent,
        todos,
        files,
        workspace: workspace.to_owned(),
        tokens: 0,
    };
    pack.tokens = estimate_tokens(&pack_text(&pack, ProviderId::Muse));
    pack
}

/// The full first-turn text submitted on the destination.
pub fn pack_text(pack: &ContextPack, from: ProviderId) -> String {
    let mut out = String::new();
    out.push_str(&pack_header(from));
    out.push_str("\n\n## Original goal\n");
    out.push_str(&pack.goal);
    out.push_str("\n\n## Conversation summary\n");
    out.push_str(&pack.summary);
    if !pack.recent.is_empty() {
        out.push_str("\n\n## Recent turns\n");
        for (role, text) in &pack.recent {
            out.push_str(&format!("\n### {role}\n{text}\n"));
        }
    }
    if !pack.todos.is_empty() {
        out.push_str("\n## Open todos\n");
        for todo in &pack.todos {
            out.push_str(&format!("- [ ] {todo}\n"));
        }
    }
    if !pack.files.is_empty() {
        out.push_str("\n## Files touched\n");
        for file in &pack.files {
            out.push_str(&format!("- {file}\n"));
        }
    }
    out.push_str(&format!("\n## Working directory\n{}\n", pack.workspace));
    out
}

/// The short user-bubble text: the destination transcript shows this, never
/// the whole pack.
pub fn display_text(pack: &ContextPack, from: ProviderId) -> String {
    let turns = pack.recent.len();
    let turn_word = if turns == 1 { "turn" } else { "turns" };
    format!(
        "Handed off from {}: {} ({turns} recent {turn_word}, {} open todos, {} files touched)",
        from.label(),
        truncate_chars(&pack.goal, 120),
        pack.todos.len(),
        pack.files.len()
    )
}

/// How long the checkpoint waits for the model-written summary before
/// keeping the extractive one: 8 s. The side-session turn is already
/// paid for, so a reply that lands later is dropped, never retried. The
/// wait never delays the destination either way: it opens at once with
/// the extractive pack. Landing stands the wait down, so a summary that
/// arrives later never reaches the destination model — the submitted
/// pack is already the extractive one.
pub const SUMMARY_TIMEOUT_SECS: u64 = 8;

/// Below this many characters of summary input (the goal plus the pack's
/// recent turns, as [`summary_input`] renders them), a model-written
/// summary cannot add anything over the extractive pack the destination
/// already opens with — so no side session starts.
pub const SUMMARY_MIN_CHARS: usize = 2_000;

/// The prompt input budget: the goal plus the transcript excerpt the
/// summary side session reads, capped at ~12k characters — whole turns,
/// most recent kept.
pub const SUMMARY_INPUT_CHARS: usize = 12_000;

/// The first line of [`summary_prompt`], kept as its own constant so the
/// hide rule recognises a summary side session from the wire alone, like
/// a title one (see [`crate::titles::is_side_prompt`]).
pub const SUMMARY_PROMPT_PREFIX: &str =
    "Summarise a handed-off chat session for the provider picking it up:";

/// Whether the pack's recent turns already carry every turn of the
/// source session verbatim (nothing was cut for the token budget): then a
/// model-written summary cannot add anything, whatever the switch says.
pub fn pack_covers_every_turn(pack: &ContextPack) -> bool {
    pack.recent.len() >= pack.source_turns
}

/// Whether the checkpoint earns a model-written summary: the switch is on
/// AND the app is signed in to Muse (the side session is a muse turn)
/// AND the summary could add something — the pack's recent turns leave
/// out part of the source, and the excerpt is long enough to summarise.
/// Otherwise the pack keeps its extractive summary and no side session
/// starts; the destination opens at once either way.
pub fn should_model_summary(switch_on: bool, signed_in: bool, pack: &ContextPack) -> bool {
    if !(switch_on && signed_in) {
        return false;
    }
    if pack_covers_every_turn(pack) {
        return false;
    }
    summary_input(pack).chars().count() >= SUMMARY_MIN_CHARS
}

/// The summary side session's input: the goal plus the pack's recent
/// turns, oldest first, capped at [`SUMMARY_INPUT_CHARS`] — whole turns
/// only, most recent kept. The cap counts the rendered text; when even
/// the newest turn alone overflows, it is still kept whole rather than
/// cut (an empty input would buy nothing).
pub fn summary_input(pack: &ContextPack) -> String {
    let goal = format!("Goal: {}\n", pack.goal);
    let mut kept: Vec<String> = Vec::new();
    let mut used = goal.len();
    for (role, text) in pack.recent.iter().rev() {
        let rendered = format!("\n### {role}\n{text}\n");
        if used + rendered.len() > SUMMARY_INPUT_CHARS {
            break;
        }
        used += rendered.len();
        kept.push(rendered);
    }
    if kept.is_empty() {
        if let Some((role, text)) = pack.recent.last() {
            kept.push(format!("\n### {role}\n{text}\n"));
        }
    }
    kept.reverse();
    let mut out = goal;
    for turn in kept {
        out.push_str(&turn);
    }
    out
}

/// The one prompt the summary side session sends: the capped goal +
/// excerpt, asking for a 4–8 line summary of what was done, decisions
/// made, current state, and what is left — plain text, no preamble.
pub fn summary_prompt(input: &str) -> String {
    format!(
        "{SUMMARY_PROMPT_PREFIX}\n\n{input}\n\nWrite a 4-8 line summary of what was done, decisions made, current state, and what is left. Plain text, no preamble."
    )
}

/// What the card's `Carried` list shows for this pack. The conversation
/// summary's detail always says which kind it is ([`SummaryKind::label`]).
pub fn carried_items(pack: &ContextPack) -> Vec<HandoffItem> {
    vec![
        HandoffItem {
            label: "Conversation summary".to_owned(),
            detail: Some(pack.summary_kind.label().to_owned()),
        },
        HandoffItem {
            label: "Recent turns".to_owned(),
            detail: Some(format!("{} verbatim", pack.recent.len())),
        },
        HandoffItem {
            label: "Open todos".to_owned(),
            detail: Some(format!("{}", pack.todos.len())),
        },
        HandoffItem {
            label: "Files touched".to_owned(),
            detail: Some(format!("{}", pack.files.len())),
        },
        HandoffItem { label: "Working directory".to_owned(), detail: None },
    ]
}

/// What the card's `Not carried` list always shows. Never hidden, never
/// empty: the loss is the point of the card.
pub fn lost_items() -> Vec<HandoffItem> {
    vec![
        HandoffItem {
            label: "Tool call internals".to_owned(),
            detail: Some("outputs and arguments stay behind".to_owned()),
        },
        HandoffItem {
            label: "Pending approvals".to_owned(),
            detail: Some("decide them before handing off".to_owned()),
        },
        HandoffItem {
            label: "Provider memory".to_owned(),
            detail: Some("each session starts unbriefed".to_owned()),
        },
        HandoffItem {
            label: "Images".to_owned(),
            detail: Some("the pack is text-only".to_owned()),
        },
        HandoffItem {
            label: "Reasoning traces".to_owned(),
            detail: Some("conclusions only, not the trace".to_owned()),
        },
    ]
}

/// Whether a provider pick is a same-provider model change (never a
/// handoff) rather than a move.
pub fn is_same_provider(from: ProviderId, to: ProviderId) -> bool {
    from == to
}

/// Map Baaz's lane id onto the wire provider the [`Block::Handoff`] card
/// carries.
pub fn wire_provider(id: ProviderId) -> aui_protocol::Provider {
    match id {
        ProviderId::Muse => aui_protocol::Provider::Muse,
        ProviderId::ClaudeCode => aui_protocol::Provider::Claude,
        ProviderId::Codex => aui_protocol::Provider::Codex,
    }
}

/// Where a destination session came from: the quiet marker's facts and the
/// back-link's target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HandoffOrigin {
    /// The source session id, opened by the marker's back-link.
    pub source_session: String,
    /// The lane the session left.
    pub from: ProviderId,
    /// The model label on the source side.
    pub from_model: String,
}

/// A destination open in flight for a handoff run, held by the
/// application and consumed when the fresh session lands (provider lane
/// or muse) — then the pack submits onto it. The epoch is the run's
/// owner-epoch: a landing from a superseded request never matches. The
/// prefix is the source's visible turns at request time, applied on the
/// destination before its first render, so no frame shows the empty
/// destination waiting for the pack's acknowledgement.
#[derive(Clone, Debug)]
pub struct PendingHandoff {
    /// The session being left.
    pub source_session: String,
    /// The run's owner-epoch.
    pub epoch: u64,
    /// The source's visible turns (the handoff card left out), oldest
    /// first, to show above the divider from the very first frame.
    pub prefix: Vec<Turn>,
}

/// The confirm dialog's frozen facts: the source, the destination backend
/// and the pack preview the Carried / Not carried lists draw.
#[derive(Clone, Debug)]
pub struct HandoffConfirmState {
    /// The session being left.
    pub source_session: String,
    /// The lane starting fresh.
    pub to: ProviderId,
    /// The model label the destination starts with (empty until known).
    pub to_model: String,
    /// What the pack carries over.
    pub carried: Vec<HandoffItem>,
    /// What does not carry over; always shown, never hidden.
    pub lost: Vec<HandoffItem>,
    /// Size of the pack in tokens.
    pub pack_tokens: u64,
}

/// Why [`HandoffRun::request`] refused to start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HandoffRefusal {
    /// The pick names the session's own provider: a native model change,
    /// never a handoff.
    SameProvider,
    /// A question is pending on the source: answer it first.
    QuestionPending,
    /// An approval is pending on the source: decide it first.
    ApprovalPending,
    /// A turn is running that cannot be interrupted.
    TurnUninterruptible,
}

impl std::fmt::Display for HandoffRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HandoffRefusal::SameProvider => write!(f, "same provider: use the model picker, not a handoff"),
            HandoffRefusal::QuestionPending => write!(f, "a question is pending on this session — answer it first"),
            HandoffRefusal::ApprovalPending => {
                write!(f, "an approval is pending on this session — decide it first")
            }
            HandoffRefusal::TurnUninterruptible => {
                write!(f, "a turn is running that cannot be interrupted — wait for it to settle")
            }
        }
    }
}

/// One handoff's lifecycle, owned by the application and keyed by the
/// source session id. States follow `docs/22-handoff.md`:
/// `requested → quiescing → checkpointed → prepared → acknowledged →
/// activated`, with `refused`/`failed`/`cancelled` off-ramps. The epoch is
/// the owner-epoch the request ran under; any ack from a superseded epoch
/// is ignored, never applied.
#[derive(Clone, Debug)]
pub struct HandoffRun {
    /// The session being left.
    pub source_session: String,
    /// The owner-epoch the request ran under.
    pub epoch: u64,
    /// The lane being left.
    pub from: ProviderId,
    /// The lane starting fresh.
    pub to: ProviderId,
    /// Model label on the source side.
    pub from_model: String,
    /// Model label the destination starts with.
    pub to_model: String,
    /// The card's block id in the source transcript. Mints through
    /// [`handoff_card_id`], so it stamps its own creation wall-ms — the
    /// run-age basis for the current step's elapsed counter, read back
    /// with [`handoff_card_started_ms`] (the protocol card carries no
    /// timestamps). Opaque everywhere else: cancel matches it by
    /// equality, and snapshots strip the card.
    pub card_id: String,
    /// When the request ran: the run-age basis for [`HandoffRun::steps`].
    /// The rendered card derives its age from the card id instead, so only
    /// the run-model tests read this.
    #[cfg_attr(not(test), allow(dead_code))]
    pub started_at: Instant,
    /// Where the move is.
    pub state: HandoffState,
    /// The pack, from Checkpointed on.
    pub pack: Option<ContextPack>,
    /// A model-written summary is in flight for the checkpointed pack:
    /// the summary step reads Current (with its elapsed counter) until
    /// the side session answers, the watchdog keeps the extractive
    /// text, or the destination lands (which stands the wait down: the
    /// submitted pack is already the extractive one). Every transition
    /// out of the wait clears this. The carried row always names the
    /// kind the pack carries — never the wait.
    pub summarising: bool,
    /// The destination's pack turn has reached its terminal state and been
    /// judged, once. Until then a failed pack turn fails the move in any
    /// live state — the ack arrives when the turn STARTS, so a pack that
    /// fails at its model ("model … does not exist") lands after
    /// activation. After it, a later failed turn is the destination's own.
    pub pack_settled: bool,
    /// The fresh destination session, once it exists.
    pub destination_session: Option<String>,
    /// The failure or refusal reason, on Failed/Refused.
    pub reason: Option<String>,
}

impl HandoffRun {
    /// Open a run. Refuses immediately (no card, no side effects) when a
    /// question or approval is pending, the turn cannot be interrupted, or
    /// the pick is the session's own provider.
    #[allow(clippy::too_many_arguments)]
    pub fn request(
        source_session: String,
        epoch: u64,
        from: ProviderId,
        to: ProviderId,
        from_model: String,
        to_model: String,
        question_pending: bool,
        approval_pending: bool,
        turn_running_uninterruptible: bool,
    ) -> Result<Self, HandoffRefusal> {
        if is_same_provider(from, to) {
            return Err(HandoffRefusal::SameProvider);
        }
        if question_pending {
            return Err(HandoffRefusal::QuestionPending);
        }
        if approval_pending {
            return Err(HandoffRefusal::ApprovalPending);
        }
        if turn_running_uninterruptible {
            return Err(HandoffRefusal::TurnUninterruptible);
        }
        Ok(Self {
            card_id: handoff_card_id(),
            started_at: Instant::now(),
            state: HandoffState::Requested,
            source_session,
            epoch,
            from,
            to,
            from_model,
            to_model,
            pack: None,
            summarising: false,
            pack_settled: false,
            destination_session: None,
            reason: None,
        })
    }

    /// A request that never started: the refusal as a visible card state,
    /// so the person reads why rather than watching nothing happen.
    pub fn refused(
        source_session: String,
        epoch: u64,
        from: ProviderId,
        to: ProviderId,
        from_model: String,
        refusal: HandoffRefusal,
    ) -> Self {
        Self {
            card_id: handoff_card_id(),
            started_at: Instant::now(),
            state: HandoffState::Refused { reason: refusal.to_string() },
            source_session,
            epoch,
            from,
            to,
            from_model,
            to_model: String::new(),
            pack: None,
            summarising: false,
            pack_settled: false,
            destination_session: None,
            reason: Some(refusal.to_string()),
        }
    }

    /// The source goes quiet: a running turn is interrupted (or settles),
    /// no new sends accepted.
    pub fn note_quiescing(&mut self) {
        if matches!(self.state, HandoffState::Requested) {
            self.state = HandoffState::Quiescing;
        }
    }

    /// The source stopped and its pack was captured.
    pub fn note_checkpointed(&mut self, pack: ContextPack) {
        if matches!(self.state, HandoffState::Requested | HandoffState::Quiescing) {
            self.pack = Some(pack);
            self.state = HandoffState::Checkpointed;
        }
    }

    /// A model-written summary started for the checkpointed pack: the run
    /// stays Checkpointed, and the summary step reads Current (with its
    /// elapsed counter) until the summary resolves.
    pub fn note_summary_pending(&mut self) {
        if matches!(self.state, HandoffState::Checkpointed) {
            self.summarising = true;
        }
    }

    /// The side session answered. The wait always ends, in every state —
    /// a late answer after acknowledge, activation, failure or cancel
    /// stands the card down without touching the pack. Only a run still
    /// Checkpointed takes the upgrade: the harvested text replaces the
    /// pack's extractive summary and the kind flips to model. Past
    /// landing the pack is already submitted, so a later summary is
    /// extractive by record and the destination model never sees it.
    pub fn apply_model_summary(&mut self, summary: String) {
        if !self.summarising {
            return;
        }
        if matches!(self.state, HandoffState::Checkpointed) {
            if let Some(pack) = self.pack.as_mut() {
                pack.summary = summary;
                pack.summary_kind = SummaryKind::Model;
                pack.tokens = estimate_tokens(&pack_text(pack, self.from));
            }
        }
        self.summarising = false;
    }

    /// The summary will not arrive (timeout, wire error, empty reply):
    /// the pack keeps its extractive summary and the wait ends. Clears
    /// in every state, so a watchdog firing after acknowledge still
    /// stands a stuck card down.
    pub fn note_summary_fallback(&mut self) {
        self.summarising = false;
    }

    /// The destination session exists and the pack is submitted — with
    /// the extractive pack, whatever the summary side session is doing.
    /// Landing stands a still-waiting summary down: the submitted pack
    /// is already fixed, so a later answer is dropped, never applied.
    pub fn note_prepared(&mut self, destination_session: String) {
        if matches!(self.state, HandoffState::Checkpointed) {
            self.destination_session = Some(destination_session);
            self.state = HandoffState::Prepared;
            self.summarising = false;
        }
    }

    /// The destination's first `TurnStarted` (or submit ack) under `epoch`.
    /// A stale epoch is ignored — returns `false` and changes nothing.
    /// Acknowledging also stands a stuck summary wait down, so the
    /// summary step never stays Current past this point.
    pub fn acknowledge(&mut self, epoch: u64) -> bool {
        if epoch != self.epoch {
            return false;
        }
        if matches!(self.state, HandoffState::Prepared) {
            self.summarising = false;
            self.state = HandoffState::Acknowledged;
            return true;
        }
        false
    }

    /// The fresh session runs on the destination; the source retires.
    /// Stands a stuck summary wait down, like every transition out of
    /// the waiting states. Settled cards hide the step list again.
    pub fn activate(&mut self) {
        if matches!(self.state, HandoffState::Acknowledged) {
            self.summarising = false;
            self.state = HandoffState::Activated;
        }
    }

    /// A step failed: the source stays usable, the card names the reason.
    /// Stands a stuck summary wait down. The failed step carries the
    /// reason (see [`handoff_steps`]).
    pub fn fail(&mut self, reason: String) {
        if !matches!(self.state, HandoffState::Activated | HandoffState::Cancelled) {
            self.summarising = false;
            self.reason = Some(reason.clone());
            self.state = HandoffState::Failed { reason };
        }
    }

    /// The destination's pack turn reached its terminal failed state: the
    /// handoff fails with the turn's reason instead of going silently
    /// Active. The pack is acknowledged when its turn starts, so this
    /// applies from Prepared through Activated — exactly once, while the
    /// pack turn is unjudged ([`Self::pack_settled`]). Cancelled, refused
    /// and already-failed runs keep their state and first reason.
    pub fn fail_pack_turn(&mut self, reason: String) {
        if self.pack_settled || !self.pack_turn_can_fail() {
            return;
        }
        self.pack_settled = true;
        self.summarising = false;
        self.reason = Some(reason.clone());
        self.state = HandoffState::Failed { reason };
    }

    /// The pack turn finished without failing: judge it once, so no later
    /// turn on the destination can fail the move.
    pub fn note_pack_completed(&mut self) {
        self.pack_settled = true;
    }

    /// Whether a pack-turn failure can still fail this move: it has a
    /// destination, its pack is unjudged, and it is live (Prepared,
    /// Acknowledged or Activated).
    pub fn pack_turn_can_fail(&self) -> bool {
        !self.pack_settled
            && self.destination_session.is_some()
            && matches!(
                self.state,
                HandoffState::Prepared | HandoffState::Acknowledged | HandoffState::Activated
            )
    }

    /// Cancel before Acknowledged aborts cleanly. Returns whether a
    /// destination was already opened, so the caller can shut it down.
    /// After Acknowledged the move is done and cancel is a no-op `false`.
    /// Cancelling during a summary wait also stands the wait down, so the
    /// summary step stops reading Current and a late side-session answer
    /// lands on nothing.
    pub fn cancel(&mut self) -> bool {
        if self.cancellable() {
            let opened = self.destination_session.is_some();
            self.summarising = false;
            self.state = HandoffState::Cancelled;
            return opened;
        }
        false
    }

    /// Whether the person can still stop the move: Requested through
    /// Prepared, mirroring the card's own `cancellable`.
    pub fn cancellable(&self) -> bool {
        matches!(
            self.state,
            HandoffState::Requested
                | HandoffState::Quiescing
                | HandoffState::Checkpointed
                | HandoffState::Prepared
        )
    }

    /// Whether the move is still in flight: Requested through
    /// Acknowledged. Settled runs (Activated, Cancelled, Refused,
    /// Failed) own no per-second re-render — see [`handoff_tick_wanted`].
    pub fn is_live(&self) -> bool {
        matches!(
            self.state,
            HandoffState::Requested
                | HandoffState::Quiescing
                | HandoffState::Checkpointed
                | HandoffState::Prepared
                | HandoffState::Acknowledged
        )
    }

    /// The card's progress steps: the library's four defaults with this
    /// run's states marked (see [`handoff_steps`], the mapping the
    /// transcript shares from the block alone). The current step carries
    /// the run's age (`"n s"`); a refused run stays all pending —
    /// nothing started, so the state line and the pill carry the reason.
    #[cfg(test)]
    pub fn steps(&self) -> Vec<HandoffStep> {
        handoff_steps(
            &self.state,
            wire_provider(self.to),
            self.pack.as_ref().map(|pack| pack.summary_kind),
            self.summarising,
            self.destination_session.is_some(),
            self.pack.is_some(),
            Some(self.started_at.elapsed().as_secs()),
        )
    }

    /// The card block for the current state. The progress steps ride the
    /// view card, not this block (the protocol has no step list): the
    /// transcript re-derives them from the block's own fields with
    /// [`handoff_steps`]. The first carried row always names the kind
    /// the pack carries — never the wait: while a model-written summary
    /// is in flight the summary step reads Current, not the carried text.
    pub fn card(&self) -> Block {
        let (carried, lost, tokens) = match &self.pack {
            Some(pack) => (carried_items(pack), lost_items(), Some(pack.tokens)),
            None => (Vec::new(), lost_items(), None),
        };
        Block::Handoff {
            id: self.card_id.clone(),
            from: wire_provider(self.from),
            to: wire_provider(self.to),
            from_model: self.from_model.clone(),
            to_model: self.to_model.clone(),
            state: self.state.clone(),
            carried,
            lost,
            pack_tokens: tokens,
            destination_session: self.destination_session.clone(),
        }
    }
}

/// Whether any run is still in flight: the per-second card refresh
/// (which is what ticks the current step's elapsed counter) runs only
/// while this holds, and no timer is armed otherwise.
#[cfg(test)]
pub fn handoff_tick_wanted(handoffs: &HashMap<String, HandoffRun>) -> bool {
    handoffs.values().any(HandoffRun::is_live)
}

/// Mark the library's four default steps from a run's state — the one
/// mapping both [`HandoffRun::steps`] and the transcript's card build
/// share (the protocol card carries no step list, so the transcript
/// re-derives the same steps from the block's own fields).
///
/// `summary_kind` is the pack's kind once checkpointed (`None` before);
/// `waiting_summary` is the summary side session still being awaited;
/// `elapsed_secs` details the one current step (`"n s"`), when the
/// caller knows a basis for it. The failed step carries the reason;
/// refused stays all pending (nothing started).
#[allow(clippy::too_many_arguments)]
pub fn handoff_steps(
    state: &HandoffState,
    to: aui_protocol::Provider,
    summary_kind: Option<SummaryKind>,
    waiting_summary: bool,
    has_destination: bool,
    has_pack: bool,
    elapsed_secs: Option<u64>,
) -> Vec<HandoffStep> {
    let mut steps = default_handoff_steps(to);
    let current = elapsed_secs.map(|secs| format!("{secs} s"));
    let summary_settled = |steps: &mut Vec<HandoffStep>| {
        steps[1].state = match summary_kind {
            Some(SummaryKind::Model) => HandoffStepState::Done,
            _ => HandoffStepState::Skipped,
        };
    };
    match state {
        HandoffState::Requested | HandoffState::Quiescing => {
            steps[0].state = HandoffStepState::Current;
            steps[0].detail = current;
        }
        HandoffState::Checkpointed => {
            steps[0].state = HandoffStepState::Done;
            match summary_kind {
                Some(SummaryKind::Model) => steps[1].state = HandoffStepState::Done,
                _ if waiting_summary => {
                    steps[1].state = HandoffStepState::Current;
                    steps[1].detail = current;
                }
                Some(SummaryKind::Extractive) => steps[1].state = HandoffStepState::Skipped,
                None => steps[1].state = HandoffStepState::Pending,
            }
        }
        HandoffState::Prepared | HandoffState::Acknowledged => {
            // Landing stands the summary wait down: the submitted pack is
            // fixed, so extractive reads skipped from here on.
            steps[0].state = HandoffStepState::Done;
            summary_settled(&mut steps);
            steps[2].state = HandoffStepState::Done;
            steps[3].state = HandoffStepState::Current;
            steps[3].detail = current;
        }
        HandoffState::Activated => {
            // Settled cards hide the list again (the library's call), but
            // the mapping stays total either way.
            steps[0].state = HandoffStepState::Done;
            summary_settled(&mut steps);
            steps[2].state = HandoffStepState::Done;
            steps[3].state = HandoffStepState::Done;
        }
        HandoffState::Cancelled => {
            // A cancelled move never reads as complete: what ran stays
            // done, what never happened reads skipped.
            steps[0].state = HandoffStepState::Done;
            summary_settled(&mut steps);
            steps[2].state = if has_destination { HandoffStepState::Done } else { HandoffStepState::Skipped };
            steps[3].state = HandoffStepState::Skipped;
        }
        HandoffState::Failed { reason } => {
            summary_settled(&mut steps);
            if has_destination {
                // The pack turn failed after the open: <To> never confirmed.
                steps[0].state = HandoffStepState::Done;
                steps[2].state = HandoffStepState::Done;
                steps[3].state = HandoffStepState::Failed;
                steps[3].detail = Some(reason.clone());
            } else if has_pack {
                // The destination never opened.
                steps[0].state = HandoffStepState::Done;
                steps[2].state = HandoffStepState::Failed;
                steps[2].detail = Some(reason.clone());
            } else {
                // Nothing to hand off, or the request never started it.
                steps[0].state = HandoffStepState::Failed;
                steps[0].detail = Some(reason.clone());
            }
        }
        HandoffState::Refused { .. } => {}
    }
    steps
}

/// A card id that stamps its own creation wall-ms: `handoff-<rand>-<ms>`.
/// The [`Block::Handoff`] card carries no timestamps, so the elapsed
/// counter on its current step reads this back at render time with
/// [`handoff_card_started_ms`] against the frame clock.
fn handoff_card_id() -> String {
    let ms =
        SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    format!("handoff-{}-{ms}", nanoid())
}

/// The run-age basis a card id stamps: `Some(ms)` for ids minted by
/// [`handoff_card_id`], `None` for the older stamp-less shape (replays
/// and snapshots from before it) — those cards show no counter.
pub fn handoff_card_started_ms(card_id: &str) -> Option<u64> {
    let rest = card_id.strip_prefix("handoff-")?;
    let (_, ms) = rest.rsplit_once('-')?;
    if ms.is_empty() || !ms.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    ms.parse().ok()
}

fn push_unique(out: &mut Vec<String>, item: String) {
    if !out.iter().any(|existing| existing == &item) {
        out.push(item);
    }
}

fn truncate_chars(text: &str, max: usize) -> String {
    let mut out = String::with_capacity(text.len().min(max + 4));
    for (i, c) in text.chars().enumerate() {
        if i >= max {
            out.push('…');
            return out;
        }
        out.push(c);
    }
    out
}

fn block_text_first(blocks: &[Block]) -> &str {
    for block in blocks {
        if let Block::Text { text, .. } = block {
            let line = text.lines().next().unwrap_or("").trim();
            if !line.is_empty() {
                return line;
            }
        }
    }
    ""
}

fn nanoid() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    format!("{:08x}", nanos ^ (std::process::id().wrapping_mul(0x9E37)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run() -> HandoffRun {
        HandoffRun::request(
            "src".to_owned(),
            7,
            ProviderId::Muse,
            ProviderId::Codex,
            "muse".to_owned(),
            "gpt-5".to_owned(),
            false,
            false,
            false,
        )
        .expect("fresh request starts")
    }

    fn session_with_turns() -> Session {
        Session::new("s", aui_protocol::Provider::Muse, "m", "/tmp/proj")
    }

    #[test]
    fn request_starts_in_requested() {
        let run = run();
        assert!(matches!(run.state, HandoffState::Requested));
        assert_eq!(run.epoch, 7);
    }

    #[test]
    fn refusal_with_pending_question() {
        let err = HandoffRun::request(
            "s".to_owned(),
            1,
            ProviderId::Muse,
            ProviderId::Codex,
            String::new(),
            String::new(),
            true,
            false,
            false,
        )
        .expect_err("question pending refuses");
        assert_eq!(err, HandoffRefusal::QuestionPending);
    }

    #[test]
    fn refusal_with_pending_approval() {
        let err = HandoffRun::request(
            "s".to_owned(),
            1,
            ProviderId::Muse,
            ProviderId::ClaudeCode,
            String::new(),
            String::new(),
            false,
            true,
            false,
        )
        .expect_err("approval pending refuses");
        assert_eq!(err, HandoffRefusal::ApprovalPending);
    }

    #[test]
    fn refusal_with_uninterruptible_turn() {
        let err = HandoffRun::request(
            "s".to_owned(),
            1,
            ProviderId::Codex,
            ProviderId::ClaudeCode,
            String::new(),
            String::new(),
            false,
            false,
            true,
        )
        .expect_err("uninterruptible turn refuses");
        assert_eq!(err, HandoffRefusal::TurnUninterruptible);
    }

    #[test]
    fn same_provider_is_a_model_change_not_a_handoff() {
        let err = HandoffRun::request(
            "s".to_owned(),
            1,
            ProviderId::Codex,
            ProviderId::Codex,
            String::new(),
            String::new(),
            false,
            false,
            false,
        )
        .expect_err("same provider is never a handoff");
        assert_eq!(err, HandoffRefusal::SameProvider);
        assert!(is_same_provider(ProviderId::Muse, ProviderId::Muse));
        assert!(!is_same_provider(ProviderId::Muse, ProviderId::Codex));
    }

    #[test]
    fn each_transition_advances_in_order() {
        let mut run = run();
        run.note_quiescing();
        assert!(matches!(run.state, HandoffState::Quiescing));
        let pack = build_pack(&session_with_turns(), "/tmp/proj");
        run.note_checkpointed(pack);
        assert!(matches!(run.state, HandoffState::Checkpointed));
        run.note_prepared("dst".to_owned());
        assert!(matches!(run.state, HandoffState::Prepared));
        assert!(run.acknowledge(7));
        assert!(matches!(run.state, HandoffState::Acknowledged));
        run.activate();
        assert!(matches!(run.state, HandoffState::Activated));
        assert!(!run.cancellable());
    }

    #[test]
    fn epoch_fencing_ignores_a_stale_ack() {
        let mut run = run();
        run.note_quiescing();
        run.note_checkpointed(build_pack(&session_with_turns(), "/tmp/proj"));
        run.note_prepared("dst".to_owned());
        assert!(!run.acknowledge(6), "a superseded epoch is ignored");
        assert!(matches!(run.state, HandoffState::Prepared));
        assert!(run.acknowledge(7));
    }

    #[test]
    fn cancel_before_ack_shuts_the_destination() {
        let mut run = run();
        run.note_quiescing();
        assert!(run.cancellable());
        // No destination yet: nothing to shut down.
        assert!(!run.cancel());
        assert!(matches!(run.state, HandoffState::Cancelled));
    }

    #[test]
    fn cancel_after_prepare_reports_the_open_destination() {
        let mut run = run();
        run.note_quiescing();
        run.note_checkpointed(build_pack(&session_with_turns(), "/tmp/proj"));
        run.note_prepared("dst".to_owned());
        assert!(run.cancel(), "caller must shut the opened destination down");
        assert!(matches!(run.state, HandoffState::Cancelled));
    }

    #[test]
    fn cancel_after_acknowledge_is_a_noop() {
        let mut run = run();
        run.note_quiescing();
        run.note_checkpointed(build_pack(&session_with_turns(), "/tmp/proj"));
        run.note_prepared("dst".to_owned());
        assert!(run.acknowledge(7));
        assert!(!run.cancel());
        assert!(matches!(run.state, HandoffState::Acknowledged));
    }

    #[test]
    fn failure_names_the_reason_and_keeps_the_source_usable() {
        let mut run = run();
        run.note_quiescing();
        run.fail("the child would not start".to_owned());
        assert!(matches!(run.state, HandoffState::Failed { .. }));
        assert_eq!(run.reason.as_deref(), Some("the child would not start"));
    }

    #[test]
    fn pack_holds_goal_todos_files_and_workspace() {
        use aui_protocol::{Block as B, Turn as T};
        let mut session = Session::new("s", aui_protocol::Provider::Muse, "m", "/tmp/proj");
        session.turns.push(T::User {
            id: "u1".to_owned(),
            text: "Fix the login redirect".to_owned(),
            attachments: vec![],
            mentions: vec![],
            timestamp: None,
        });
        session.turns.push(T::Assistant {
            id: "a1".to_owned(),
            blocks: vec![
                B::Text { text: "On it.".to_owned(), streaming: false },
                B::Todo {
                    items: vec![
                        aui_protocol::TodoItem {
                            label: "Repro the redirect".to_owned(),
                            state: TodoState::Pending,
                            elapsed_ms: None,
                        },
                        aui_protocol::TodoItem {
                            label: "Done already".to_owned(),
                            state: TodoState::Done,
                            elapsed_ms: None,
                        },
                    ],
                },
            ],
            meta: Default::default(),
            timestamp: None,
        });
        let pack = build_pack(&session, "/tmp/proj");
        assert_eq!(pack.goal, "Fix the login redirect");
        assert!(pack.summary.contains("On it"), "extractive summary quotes the reply");
        assert_eq!(pack.todos, vec!["Repro the redirect".to_owned()]);
        assert_eq!(pack.workspace, "/tmp/proj");
        assert!(!pack.recent.is_empty());
        let text = pack_text(&pack, ProviderId::Muse);
        assert!(text.starts_with("Continuing a session handed off from Muse. Context follows."));
        assert!(
            text.contains("one-sentence acknowledgement"),
            "the pack tells the destination to acknowledge and wait, not re-execute: {text}"
        );
        let bubble = display_text(&pack, ProviderId::Muse);
        assert!(bubble.len() < text.len());
        assert!(bubble.contains("Handed off from Muse"));
    }

    #[test]
    fn pack_stays_within_its_token_budget() {
        use aui_protocol::{Block as B, Turn as T};
        let mut session = Session::new("s", aui_protocol::Provider::Muse, "m", "/tmp/proj");
        for i in 0..60 {
            session.turns.push(T::User {
                id: format!("u{i}"),
                text: format!("prompt {i} {}", "word ".repeat(200)),
                attachments: vec![],
                mentions: vec![],
                timestamp: None,
            });
            session.turns.push(T::Assistant {
                id: format!("a{i}"),
                blocks: vec![B::Text { text: format!("reply {i} {}", "word ".repeat(200)), streaming: false }],
                meta: Default::default(),
                timestamp: None,
            });
        }
        let pack = build_pack(&session, "/tmp/proj");
        assert!(pack.tokens <= PACK_BUDGET_TOKENS + 1500, "pack tokens {} over budget", pack.tokens);
        // Whole turns only: the first kept turn is a complete quote.
        assert!(!pack.recent.is_empty());
        assert!(pack.recent.len() < 120);
    }

    #[test]
    fn lost_list_is_never_empty() {
        let lost = lost_items();
        assert!(lost.len() >= 5);
        assert!(lost.iter().any(|item| item.label == "Pending approvals"));
    }

    #[test]
    fn card_mirrors_run_state() {
        let mut run = run();
        let card = run.card();
        assert!(matches!(card, Block::Handoff { .. }));
        run.note_quiescing();
        run.note_checkpointed(build_pack(&session_with_turns(), "/tmp/proj"));
        let card = run.card();
        if let Block::Handoff { carried, lost, pack_tokens, .. } = card {
            assert!(!carried.is_empty());
            assert!(!lost.is_empty());
            assert!(pack_tokens.is_some());
        } else {
            panic!("not a handoff card");
        }
    }

    #[test]
    fn steps_follow_the_run_from_request_to_activation() {
        let mut run = run();
        // The labels are the library's four defaults, naming the destination.
        let expected = default_handoff_steps(wire_provider(ProviderId::Codex));
        let steps = run.steps();
        let labels: Vec<&str> = steps.iter().map(|step| step.label.as_str()).collect();
        assert_eq!(labels[0], "Pack the context");
        assert_eq!(labels[1], "Write a summary");
        assert_eq!(labels[2], expected[2].label.as_str());
        assert_eq!(labels[3], expected[3].label.as_str());
        // Requested: packing now, with the run's age on it.
        let states = |run: &HandoffRun| run.steps().iter().map(|step| step.state).collect::<Vec<_>>();
        assert_eq!(
            states(&run),
            vec![
                HandoffStepState::Current,
                HandoffStepState::Pending,
                HandoffStepState::Pending,
                HandoffStepState::Pending,
            ]
        );
        assert_eq!(run.steps()[0].detail.as_deref(), Some("0 s"));
        run.note_quiescing();
        assert_eq!(
            states(&run),
            vec![
                HandoffStepState::Current,
                HandoffStepState::Pending,
                HandoffStepState::Pending,
                HandoffStepState::Pending,
            ]
        );
        // Checkpointed with no summary wait: the model summary is skipped.
        run.note_checkpointed(build_pack(&session_with_turns(), "/tmp/proj"));
        assert_eq!(
            states(&run),
            vec![
                HandoffStepState::Done,
                HandoffStepState::Skipped,
                HandoffStepState::Pending,
                HandoffStepState::Pending,
            ]
        );
        // Prepared: the destination is open, waiting on its confirm.
        run.note_prepared("dst".to_owned());
        assert_eq!(
            states(&run),
            vec![
                HandoffStepState::Done,
                HandoffStepState::Skipped,
                HandoffStepState::Done,
                HandoffStepState::Current,
            ]
        );
        assert_eq!(run.steps()[3].detail.as_deref(), Some("0 s"));
        // Acknowledged, then activated: the confirm lands, then all done.
        assert!(run.acknowledge(7));
        assert_eq!(
            states(&run),
            vec![
                HandoffStepState::Done,
                HandoffStepState::Skipped,
                HandoffStepState::Done,
                HandoffStepState::Current,
            ]
        );
        run.activate();
        assert_eq!(
            states(&run),
            vec![
                HandoffStepState::Done,
                HandoffStepState::Skipped,
                HandoffStepState::Done,
                HandoffStepState::Done,
            ]
        );
        assert!(run.steps().iter().all(|step| step.detail.is_none()), "settled steps carry no counter");
    }

    #[test]
    fn steps_fail_at_start_names_the_pack_step() {
        let mut run = run();
        run.note_quiescing();
        run.fail("nothing to hand off — the session has no turns".to_owned());
        let steps = run.steps();
        assert!(matches!(steps[0].state, HandoffStepState::Failed));
        assert_eq!(
            steps[0].detail.as_deref(),
            Some("nothing to hand off — the session has no turns"),
            "the failed step carries the reason"
        );
        assert!(
            !steps.iter().any(|step| matches!(step.state, HandoffStepState::Current)),
            "nothing is left Current on a dead card"
        );
    }

    #[test]
    fn steps_fail_at_the_pack_turn_names_the_confirm_step() {
        let mut run = run();
        run.note_quiescing();
        run.note_checkpointed(build_pack(&session_with_turns(), "/tmp/proj"));
        run.note_prepared("dst".to_owned());
        assert!(run.acknowledge(7));
        run.activate();
        run.fail_pack_turn("model `gpt-6-astra` does not exist or you lack access".to_owned());
        assert!(matches!(run.state, HandoffState::Failed { .. }));
        let steps = run.steps();
        assert_eq!(
            (steps[0].state, steps[1].state, steps[2].state),
            (HandoffStepState::Done, HandoffStepState::Skipped, HandoffStepState::Done)
        );
        assert!(matches!(steps[3].state, HandoffStepState::Failed));
        assert_eq!(
            steps[3].detail.as_deref(),
            Some("model `gpt-6-astra` does not exist or you lack access"),
            "the failed step carries the reason"
        );
    }

    #[test]
    fn steps_show_the_summary_waited_or_skipped() {
        // Skipped: checkpointed with no wait — no model summary is used.
        let mut skipped = run();
        skipped.note_quiescing();
        skipped.note_checkpointed(build_pack(&session_with_turns(), "/tmp/proj"));
        assert!(matches!(skipped.steps()[1].state, HandoffStepState::Skipped));
        assert_eq!(skipped.steps()[1].detail, None);
        // Waited: the summary step is Current with its elapsed counter…
        let mut waited = run();
        waited.note_quiescing();
        waited.note_checkpointed(build_pack(&session_with_turns(), "/tmp/proj"));
        waited.note_summary_pending();
        assert!(matches!(waited.steps()[1].state, HandoffStepState::Current));
        assert_eq!(waited.steps()[1].detail.as_deref(), Some("0 s"));
        // …while the carried row still names the kind, never the wait.
        if let Block::Handoff { carried, .. } = waited.card() {
            assert_eq!(carried[0].detail.as_deref(), Some("extractive summary"));
        } else {
            panic!("not a handoff card");
        }
        // Harvested before landing: the step reads done.
        waited.apply_model_summary("Did X. Decided Y.".to_owned());
        assert!(matches!(waited.steps()[1].state, HandoffStepState::Done));
        // Landed without a summary: skipped from here on.
        let mut landed = run();
        landed.note_quiescing();
        landed.note_checkpointed(build_pack(&session_with_turns(), "/tmp/proj"));
        landed.note_prepared("dst".to_owned());
        assert!(matches!(landed.steps()[1].state, HandoffStepState::Skipped));
    }

    #[test]
    fn no_tick_is_wanted_when_no_run_is_in_flight() {
        let empty: HashMap<String, HandoffRun> = HashMap::new();
        assert!(!handoff_tick_wanted(&empty), "no runs, no timer");
        let mut settled: HashMap<String, HandoffRun> = HashMap::new();
        let mut done = run();
        done.note_quiescing();
        done.note_checkpointed(build_pack(&session_with_turns(), "/tmp/proj"));
        done.note_prepared("dst".to_owned());
        assert!(done.acknowledge(7));
        done.activate();
        settled.insert("a".to_owned(), done);
        let mut cancelled = run();
        cancelled.note_quiescing();
        assert!(!cancelled.cancel());
        settled.insert("b".to_owned(), cancelled);
        assert!(!handoff_tick_wanted(&settled), "settled runs own no timer");
        let mut live: HashMap<String, HandoffRun> = HashMap::new();
        live.insert("c".to_owned(), run());
        assert!(handoff_tick_wanted(&live), "a requested run ticks");
    }

    #[test]
    fn card_ids_carry_their_creation_time() {
        let id = run().card_id;
        let started = handoff_card_started_ms(&id).expect("fresh ids stamp their creation");
        let now =
            SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
        assert!(started <= now && now - started < 60_000, "the stamp reads wall-ms");
        assert_eq!(
            handoff_card_started_ms("handoff-deadbeef"),
            None,
            "stamp-less ids show no counter"
        );
    }

    #[test]
    fn a_harvested_summary_replaces_the_extractive_one() {
        let mut run = run();
        run.note_quiescing();
        run.note_checkpointed(build_pack(&session_with_turns(), "/tmp/proj"));
        run.note_summary_pending();
        run.apply_model_summary("Did X. Decided Y. Now at Z. Left: W.".to_owned());
        let pack = run.pack.as_ref().expect("checkpointed pack");
        assert_eq!(pack.summary_kind, SummaryKind::Model);
        assert!(pack.summary.contains("Decided Y"));
        let card = run.card();
        if let Block::Handoff { carried, .. } = card {
            assert_eq!(carried[0].detail.as_deref(), Some("model summary"));
        } else {
            panic!("not a handoff card");
        }
    }

    #[test]
    fn a_late_summary_never_lands_on_a_cancelled_run() {
        let mut run = run();
        run.note_quiescing();
        run.note_checkpointed(build_pack(&session_with_turns(), "/tmp/proj"));
        run.note_summary_pending();
        // Cancel during summarising: clean abort, and no destination was
        // ever opened, so there is nothing to shut down.
        assert!(!run.cancel());
        assert!(matches!(run.state, HandoffState::Cancelled));
        assert!(!run.summarising, "cancel stands the summary wait down");
        run.apply_model_summary("late model text".to_owned());
        let pack = run.pack.as_ref().expect("checkpointed pack");
        assert_eq!(pack.summary_kind, SummaryKind::Extractive, "abandoned side sessions change nothing");
    }

    #[test]
    fn a_timeout_keeps_the_extractive_summary() {
        let mut run = run();
        run.note_quiescing();
        run.note_checkpointed(build_pack(&session_with_turns(), "/tmp/proj"));
        run.note_summary_pending();
        let waiting = run.card();
        if let Block::Handoff { carried, .. } = waiting {
            // The wait lives on the summary step now, never the carried row.
            assert_eq!(carried[0].detail.as_deref(), Some("extractive summary"));
        } else {
            panic!("not a handoff card");
        }
        run.note_summary_fallback();
        assert!(!run.summarising);
        let pack = run.pack.as_ref().expect("checkpointed pack");
        assert_eq!(pack.summary_kind, SummaryKind::Extractive);
        let card = run.card();
        if let Block::Handoff { carried, .. } = card {
            assert_eq!(carried[0].detail.as_deref(), Some("extractive summary"));
        } else {
            panic!("not a handoff card");
        }
        // Still checkpointed: the destination opens next.
        assert!(matches!(run.state, HandoffState::Checkpointed));
        assert!(run.cancellable());
    }

    #[test]
    fn the_model_summary_needs_the_switch_and_a_sign_in() {
        // A long session whose pack leaves turns out: worth summarising.
        let pack = big_pack();
        assert!(should_model_summary(true, true, &pack));
        assert!(!should_model_summary(false, true, &pack), "switch off starts no side session");
        assert!(!should_model_summary(true, false, &pack), "signed out starts no side session");
        assert!(!should_model_summary(false, false, &pack));
    }

    /// A session long enough that the pack budget cuts turns: the recent
    /// list is shorter than the source, and the excerpt tops 2k chars.
    fn big_pack() -> ContextPack {
        use aui_protocol::{Block as B, Turn as T};
        let mut session = Session::new("s", aui_protocol::Provider::Muse, "m", "/tmp/proj");
        for i in 0..60 {
            session.turns.push(T::User {
                id: format!("u{i}"),
                text: format!("prompt {i} {}", "word ".repeat(200)),
                attachments: vec![],
                mentions: vec![],
                timestamp: None,
            });
            session.turns.push(T::Assistant {
                id: format!("a{i}"),
                blocks: vec![B::Text { text: format!("reply {i} {}", "word ".repeat(200)), streaming: false }],
                meta: Default::default(),
                timestamp: None,
            });
        }
        let pack = build_pack(&session, "/tmp/proj");
        assert!(
            !pack_covers_every_turn(&pack),
            "the fixture really does cut turns ({} of {})",
            pack.recent.len(),
            pack.source_turns
        );
        pack
    }

    #[test]
    fn a_pack_that_carries_every_turn_skips_the_model_summary() {
        use aui_protocol::{Block as B, Turn as T};
        let mut session = Session::new("s", aui_protocol::Provider::Muse, "m", "/tmp/proj");
        session.turns.push(T::User {
            id: "u1".to_owned(),
            text: "Fix the login redirect".to_owned(),
            attachments: vec![],
            mentions: vec![],
            timestamp: None,
        });
        session.turns.push(T::Assistant {
            id: "a1".to_owned(),
            blocks: vec![B::Text { text: "On it.".to_owned(), streaming: false }],
            meta: Default::default(),
            timestamp: None,
        });
        let pack = build_pack(&session, "/tmp/proj");
        assert!(pack_covers_every_turn(&pack));
        assert!(
            !should_model_summary(true, true, &pack),
            "every turn already rides verbatim: no side session, even with the switch on"
        );
    }

    #[test]
    fn a_short_excerpt_skips_the_model_summary() {
        // The pack leaves most of a long source out — but the whole
        // excerpt is a few words, so a model summary still adds nothing
        // over the extractive pack the destination opens with.
        let pack = ContextPack {
            goal: "Fix it".to_owned(),
            summary: "Did it.".to_owned(),
            summary_kind: SummaryKind::Extractive,
            recent: vec![("user".to_owned(), "hi".to_owned())],
            todos: Vec::new(),
            files: Vec::new(),
            workspace: "/tmp/proj".to_owned(),
            tokens: 10,
            source_turns: 50,
        };
        assert!(!pack_covers_every_turn(&pack));
        assert!(summary_input(&pack).len() < SUMMARY_MIN_CHARS);
        assert!(!should_model_summary(true, true, &pack));
        // …while a long excerpt from the same cut-down pack earns one.
        let mut long = pack.clone();
        long.recent = vec![("user".to_owned(), "word ".repeat(600))];
        assert!(summary_input(&long).len() >= SUMMARY_MIN_CHARS);
        assert!(should_model_summary(true, true, &long));
    }

    #[test]
    fn handoff_summary_never_delays_the_destination_open() {
        const { assert!(SUMMARY_TIMEOUT_SECS <= 8, "the watchdog is a backstop, not the open gate") };
        // The machine reaches Prepared while a summary is still in flight:
        // nothing in the run gates the destination open on the summary —
        // and landing stands the wait down, since the submitted pack is
        // already the extractive one.
        let mut landed = run();
        landed.note_quiescing();
        landed.note_checkpointed(build_pack(&session_with_turns(), "/tmp/proj"));
        landed.note_summary_pending();
        landed.note_prepared("dst".to_owned());
        assert!(
            matches!(landed.state, HandoffState::Prepared),
            "landing marks Prepared, state is {:?}",
            landed.state
        );
        assert!(!landed.summarising, "landing stands the summary wait down");
        // A summary arriving after landing never upgrades the pack the
        // destination already submitted: the kind stays extractive.
        landed.apply_model_summary("Did X. Decided Y.".to_owned());
        assert_eq!(landed.pack.as_ref().map(|pack| pack.summary_kind), Some(SummaryKind::Extractive));
        assert!(!landed.summarising);
        // …while one that beats the landing still upgrades it.
        let mut early = run();
        early.note_quiescing();
        early.note_checkpointed(build_pack(&session_with_turns(), "/tmp/proj"));
        early.note_summary_pending();
        early.apply_model_summary("Did X. Decided Y.".to_owned());
        assert_eq!(
            early.pack.as_ref().map(|pack| pack.summary_kind),
            Some(SummaryKind::Model)
        );
        assert!(!early.summarising);
    }

    #[test]
    fn a_pack_turn_that_fails_fails_the_handoff() {
        let mut run = run();
        run.note_quiescing();
        run.note_checkpointed(build_pack(&session_with_turns(), "/tmp/proj"));
        run.note_prepared("dst".to_owned());
        // turn/started, then turn/completed{status: failed} on the
        // destination before the ack: the pack never reached any model.
        run.fail_pack_turn("model `gpt-6-astra` does not exist or you lack access".to_owned());
        assert!(
            matches!(run.state, HandoffState::Failed { .. }),
            "a failed pack turn fails the handoff, state is {:?}",
            run.state
        );
        assert_eq!(
            run.reason.as_deref(),
            Some("model `gpt-6-astra` does not exist or you lack access")
        );
        let card = run.card();
        if let Block::Handoff { state: HandoffState::Failed { reason }, .. } = card {
            assert!(reason.contains("gpt-6-astra"), "the card names the reason: {reason}");
        } else {
            panic!("the card reads Failed, got {:?}", card);
        }
    }

    #[test]
    fn a_pack_turn_that_fails_after_activation_fails_the_handoff_once() {
        let mut run = run();
        run.note_quiescing();
        run.note_checkpointed(build_pack(&session_with_turns(), "/tmp/proj"));
        run.note_prepared("dst".to_owned());
        assert!(run.acknowledge(7));
        run.activate();
        assert!(matches!(run.state, HandoffState::Activated));
        // The ack came when the pack turn STARTED; the owner's real case
        // (a model that does not exist) fails after it. That still fails
        // the move, with the turn's reason.
        run.fail_pack_turn("model `gpt-6-astra` does not exist or you lack access".to_owned());
        assert!(matches!(run.state, HandoffState::Failed { .. }), "state is {:?}", run.state);
        assert_eq!(run.reason.as_deref(), Some("model `gpt-6-astra` does not exist or you lack access"));
        assert!(!run.pack_turn_can_fail(), "judged once");
    }

    #[test]
    fn a_later_failed_turn_never_fails_a_handoff_whose_pack_completed() {
        let mut run = run();
        run.note_quiescing();
        run.note_checkpointed(build_pack(&session_with_turns(), "/tmp/proj"));
        run.note_prepared("dst".to_owned());
        assert!(run.acknowledge(7));
        run.activate();
        run.note_pack_completed();
        run.fail_pack_turn("a later turn failed".to_owned());
        assert!(matches!(run.state, HandoffState::Activated), "state is {:?}", run.state);
        assert_eq!(run.reason, None);
    }

    #[test]
    fn a_failed_pack_turn_never_revives_the_run() {
        let mut run = run();
        run.note_quiescing();
        run.note_checkpointed(build_pack(&session_with_turns(), "/tmp/proj"));
        run.note_prepared("dst".to_owned());
        assert!(run.cancel());
        run.fail_pack_turn("late failure".to_owned());
        assert!(
            matches!(run.state, HandoffState::Cancelled),
            "a cancelled run stays cancelled, state is {:?}",
            run.state
        );
    }

    #[test]
    fn the_summary_prompt_asks_for_four_to_eight_lines() {
        let pack = build_pack(&session_with_turns(), "/tmp/proj");
        let prompt = summary_prompt(&summary_input(&pack));
        assert!(prompt.starts_with(SUMMARY_PROMPT_PREFIX));
        assert!(prompt.contains("4-8 line"));
        assert!(prompt.contains("no preamble"));
    }

    #[test]
    fn the_summary_input_is_capped_and_keeps_the_most_recent_whole_turns() {
        use aui_protocol::{Block as B, Turn as T};
        let mut session = Session::new("s", aui_protocol::Provider::Muse, "m", "/tmp/proj");
        for i in 0..40 {
            session.turns.push(T::User {
                id: format!("u{i}"),
                text: format!("prompt-marker-{i} {}", "word ".repeat(100)),
                attachments: vec![],
                mentions: vec![],
                timestamp: None,
            });
            session.turns.push(T::Assistant {
                id: format!("a{i}"),
                blocks: vec![B::Text { text: format!("reply-marker-{i} {}", "word ".repeat(100)), streaming: false }],
                meta: Default::default(),
                timestamp: None,
            });
        }
        let pack = build_pack(&session, "/tmp/proj");
        let input = summary_input(&pack);
        assert!(input.len() <= SUMMARY_INPUT_CHARS, "input {} chars over cap", input.len());
        assert!(input.contains("reply-marker-39"), "the newest turn is kept");
        assert!(!input.contains("reply-marker-0"), "the oldest turn falls off");
        // The first user prompt still arrives as the goal line (the prompt
        // is goal + excerpt by construction) — but its turn is gone from
        // the excerpt itself, whole rather than cut mid-turn.
        assert!(input.starts_with("Goal: prompt-marker-0"));
        assert!(!input.contains("### user\nprompt-marker-0"));
    }

    #[test]
    fn every_transition_out_of_the_wait_stands_the_card_down() {
        // Acknowledge clears a stuck wait.
        let mut settled = run();
        settled.note_quiescing();
        settled.note_checkpointed(build_pack(&session_with_turns(), "/tmp/proj"));
        settled.note_summary_pending();
        settled.note_prepared("dst".to_owned());
        // Landing itself already stood the wait down; re-arm the flag the
        // way a missed stand-down would leave it, then acknowledge.
        settled.summarising = true;
        assert!(settled.acknowledge(7));
        assert!(!settled.summarising, "acknowledge clears the summary wait");
        // Activate clears a stuck wait.
        settled.summarising = true;
        settled.activate();
        assert!(!settled.summarising, "activate clears the summary wait");
        assert!(matches!(settled.state, HandoffState::Activated));
        let card = settled.card();
        if let Block::Handoff { carried, .. } = card {
            assert_ne!(
                carried[0].detail.as_deref(),
                Some("Summarising…"),
                "a settled card never reads Summarising…"
            );
        } else {
            panic!("not a handoff card");
        }
        // Fail clears a stuck wait.
        let mut failed = run();
        failed.note_quiescing();
        failed.note_checkpointed(build_pack(&session_with_turns(), "/tmp/proj"));
        failed.note_summary_pending();
        failed.summarising = true;
        failed.fail("the child would not start".to_owned());
        assert!(!failed.summarising, "fail clears the summary wait");
        let card = failed.card();
        if let Block::Handoff { carried, .. } = card {
            assert_ne!(
                carried[0].detail.as_deref(),
                Some("Summarising…"),
                "a failed card never reads Summarising…"
            );
        } else {
            panic!("not a handoff card");
        }
    }

    #[test]
    fn a_late_harvest_after_ack_keeps_the_extractive_pack() {
        let mut run = run();
        run.note_quiescing();
        run.note_checkpointed(build_pack(&session_with_turns(), "/tmp/proj"));
        run.note_summary_pending();
        run.note_prepared("dst".to_owned());
        assert!(run.acknowledge(7));
        run.activate();
        // The wait was stood down at landing; a side-session answer
        // landing now must not upgrade the submitted pack.
        run.summarising = true;
        run.apply_model_summary("late model text".to_owned());
        assert!(!run.summarising, "a late harvest still stands the wait down");
        let pack = run.pack.as_ref().expect("checkpointed pack");
        assert_eq!(pack.summary_kind, SummaryKind::Extractive, "a late answer changes nothing");
        let card = run.card();
        if let Block::Handoff { carried, .. } = card {
            assert_eq!(carried[0].detail.as_deref(), Some("extractive summary"));
        } else {
            panic!("not a handoff card");
        }
    }

    #[test]
    fn a_watchdog_after_activation_stands_a_stuck_wait_down() {
        let mut run = run();
        run.note_quiescing();
        run.note_checkpointed(build_pack(&session_with_turns(), "/tmp/proj"));
        run.note_summary_pending();
        run.note_prepared("dst".to_owned());
        assert!(run.acknowledge(7));
        run.activate();
        run.summarising = true;
        run.note_summary_fallback();
        assert!(!run.summarising, "a late watchdog still stands the wait down");
        let pack = run.pack.as_ref().expect("checkpointed pack");
        assert_eq!(pack.summary_kind, SummaryKind::Extractive);
    }

    #[test]
    fn the_summary_threshold_counts_characters_not_bytes() {
        // "é" is two bytes but one character: 1_500 of them are 3_000
        // bytes but only ~1_500 characters — under the 2_000-character
        // threshold, so no side session starts.
        let pack = ContextPack {
            goal: "Fix it".to_owned(),
            summary: "Did it.".to_owned(),
            summary_kind: SummaryKind::Extractive,
            recent: vec![("user".to_owned(), "é".repeat(1500))],
            todos: Vec::new(),
            files: Vec::new(),
            workspace: "/tmp/proj".to_owned(),
            tokens: 10,
            source_turns: 50,
        };
        assert!(!pack_covers_every_turn(&pack));
        let input = summary_input(&pack);
        assert!(input.len() >= SUMMARY_MIN_CHARS, "bytes alone would qualify: {}", input.len());
        assert!(
            input.chars().count() < SUMMARY_MIN_CHARS,
            "characters do not: {}",
            input.chars().count()
        );
        assert!(
            !should_model_summary(true, true, &pack),
            "the threshold counts characters, not bytes"
        );
    }

    #[test]
    fn cancel_before_ack_with_scripted_destination() {
        // The destination the run opened is a real (scripted) provider the
        // caller shuts down on cancel: the shutdown is total — a send past
        // it is refused rather than answered.
        let adapter = provider::scripted::ScriptedProvider::new();
        let mut dest = provider::Provider::new(adapter);
        dest.connect(&provider::ConnectInfo::new("baaz", "test")).expect("scripted connects");
        let mut run = run();
        run.note_quiescing();
        run.note_checkpointed(build_pack(&session_with_turns(), "/tmp/proj"));
        run.note_prepared("scripted-dst".to_owned());
        assert!(run.cancel());
        dest.shutdown();
        let refused = dest.send(provider::Command::SubmitInput {
            request_id: "r".to_owned(),
            session_id: "scripted-dst".to_owned(),
            parts: vec![provider::SubmissionPart::Text("late".to_owned())],
            display_text: None,
            effort: None,
        });
        assert!(refused.is_err(), "a shut destination answers nothing");
    }

#[cfg(test)]
mod cancelled_steps_tests {
    use super::*;

    #[test]
    fn a_cancelled_move_never_reads_complete() {
        let steps = handoff_steps(&HandoffState::Cancelled, aui_protocol::Provider::Codex, Some(SummaryKind::Extractive), false, false, true, None);
        assert_eq!(steps[3].state, HandoffStepState::Skipped);
        assert_eq!(steps[2].state, HandoffStepState::Skipped);
    }
}

}
