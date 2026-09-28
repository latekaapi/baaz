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
/// keeping the extractive one: 20 s. The side-session turn is already
/// paid for, so a reply that lands later is dropped, never retried.
pub const SUMMARY_TIMEOUT_SECS: u64 = 20;

/// The prompt input budget: the goal plus the transcript excerpt the
/// summary side session reads, capped at ~12k characters — whole turns,
/// most recent kept.
pub const SUMMARY_INPUT_CHARS: usize = 12_000;

/// The first line of [`summary_prompt`], kept as its own constant so the
/// hide rule recognises a summary side session from the wire alone, like
/// a title one (see [`crate::titles::is_side_prompt`]).
pub const SUMMARY_PROMPT_PREFIX: &str =
    "Summarise a handed-off chat session for the provider picking it up:";

/// Whether the checkpoint earns a model-written summary: the switch is on
/// AND the app is signed in to Muse (the side session is a muse turn).
/// Otherwise the pack keeps its extractive summary and no side session
/// starts.
pub fn should_model_summary(switch_on: bool, signed_in: bool) -> bool {
    switch_on && signed_in
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
/// owner-epoch: a landing from a superseded request never matches.
#[derive(Clone, Debug)]
pub struct PendingHandoff {
    /// The session being left.
    pub source_session: String,
    /// The run's owner-epoch.
    pub epoch: u64,
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
    /// The card's block id in the source transcript.
    pub card_id: String,
    /// Where the move is.
    pub state: HandoffState,
    /// The pack, from Checkpointed on.
    pub pack: Option<ContextPack>,
    /// A model-written summary is in flight for the checkpointed pack:
    /// the run stays Checkpointed and the card's summary line reads
    /// "Summarising…" until the side session answers or the watchdog
    /// keeps the extractive text. Cancel works throughout (the hidden
    /// side session is simply abandoned; the destination never opens).
    pub summarising: bool,
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
            card_id: format!("handoff-{}", nanoid()),
            state: HandoffState::Requested,
            source_session,
            epoch,
            from,
            to,
            from_model,
            to_model,
            pack: None,
            summarising: false,
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
            card_id: format!("handoff-{}", nanoid()),
            state: HandoffState::Refused { reason: refusal.to_string() },
            source_session,
            epoch,
            from,
            to,
            from_model,
            to_model: String::new(),
            pack: None,
            summarising: false,
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
    /// stays Checkpointed, and the card reads "Summarising…" until the
    /// summary resolves.
    pub fn note_summary_pending(&mut self) {
        if matches!(self.state, HandoffState::Checkpointed) {
            self.summarising = true;
        }
    }

    /// The side session answered: the harvested text replaces the pack's
    /// extractive summary, the kind flips to model, and the wait ends. A
    /// no-op unless the run is still waiting (a cancelled or superseded
    /// run keeps whatever it holds).
    pub fn apply_model_summary(&mut self, summary: String) {
        if !matches!(self.state, HandoffState::Checkpointed) || !self.summarising {
            return;
        }
        if let Some(pack) = self.pack.as_mut() {
            pack.summary = summary;
            pack.summary_kind = SummaryKind::Model;
            pack.tokens = estimate_tokens(&pack_text(pack, self.from));
        }
        self.summarising = false;
    }

    /// The summary will not arrive (timeout, wire error, empty reply):
    /// the pack keeps its extractive summary and the wait ends, so the
    /// destination can open.
    pub fn note_summary_fallback(&mut self) {
        if matches!(self.state, HandoffState::Checkpointed) {
            self.summarising = false;
        }
    }

    /// The destination session exists and the pack is submitted.
    pub fn note_prepared(&mut self, destination_session: String) {
        if matches!(self.state, HandoffState::Checkpointed) {
            self.destination_session = Some(destination_session);
            self.state = HandoffState::Prepared;
        }
    }

    /// The destination's first `TurnStarted` (or submit ack) under `epoch`.
    /// A stale epoch is ignored — returns `false` and changes nothing.
    pub fn acknowledge(&mut self, epoch: u64) -> bool {
        if epoch != self.epoch {
            return false;
        }
        if matches!(self.state, HandoffState::Prepared) {
            self.state = HandoffState::Acknowledged;
            return true;
        }
        false
    }

    /// The fresh session runs on the destination; the source retires.
    pub fn activate(&mut self) {
        if matches!(self.state, HandoffState::Acknowledged) {
            self.state = HandoffState::Activated;
        }
    }

    /// A step failed: the source stays usable, the card names the reason.
    pub fn fail(&mut self, reason: String) {
        if !matches!(self.state, HandoffState::Activated | HandoffState::Cancelled) {
            self.reason = Some(reason.clone());
            self.state = HandoffState::Failed { reason };
        }
    }

    /// Cancel before Acknowledged aborts cleanly. Returns whether a
    /// destination was already opened, so the caller can shut it down.
    /// After Acknowledged the move is done and cancel is a no-op `false`.
    /// Cancelling during a summary wait also stands the wait down, so the
    /// card stops reading "Summarising…" and a late side-session answer
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

    /// The card block for the current state. While a model-written
    /// summary is in flight the summary line reads "Summarising…";
    /// otherwise it names the kind the pack carries.
    pub fn card(&self) -> Block {
        let (mut carried, lost, tokens) = match &self.pack {
            Some(pack) => (carried_items(pack), lost_items(), Some(pack.tokens)),
            None => (Vec::new(), lost_items(), None),
        };
        if self.summarising {
            if let Some(first) = carried.first_mut() {
                first.detail = Some("Summarising…".to_owned());
            }
        }
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
    fn a_timeout_keeps_the_extractive_summary_and_says_so() {
        let mut run = run();
        run.note_quiescing();
        run.note_checkpointed(build_pack(&session_with_turns(), "/tmp/proj"));
        run.note_summary_pending();
        let waiting = run.card();
        if let Block::Handoff { carried, .. } = waiting {
            assert_eq!(carried[0].detail.as_deref(), Some("Summarising…"));
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
        assert!(should_model_summary(true, true));
        assert!(!should_model_summary(false, true), "switch off starts no side session");
        assert!(!should_model_summary(true, false), "signed out starts no side session");
        assert!(!should_model_summary(false, false));
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
}
