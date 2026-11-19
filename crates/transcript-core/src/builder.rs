//! Block positions keyed by stable ids.
//!
//! Every block the builder adds may carry keys — `tool:<id>`,
//! `approval:<id>`, `question:<id>` or a fold's own (`plan:<turn>`) — and
//! every later change names a key, never an index. The builder finds the
//! index in its own transcript, which is built from exactly the deltas it
//! emits, so it is the index the app's copy has too: a block that never
//! landed (its turn is missing, or is a user turn) takes no index, and a
//! removed block moves every later one up. A block may carry several keys
//! (a group card renders several calls), and a key names one block at a
//! time (filing it again moves it).
//!
//! Blocks normally append. A fold whose wire can deliver a block out of its
//! log order (Muse's backfill) names the order with
//! [`TranscriptBuilder::set_order`]; the builder then inserts the block where
//! the log puts it, expressed as an append plus the updates that rotate the
//! tail, since `aui-protocol` has no insert.

use std::collections::{HashMap, HashSet};

use aui_protocol::{ApprovalState, Block, Delta, Provider, Session, Turn, TurnMeta};

use crate::tool::ToolEvent;

/// The key a tool call's card is filed under.
pub fn tool_key(id: &str) -> String {
    format!("tool:{id}")
}

/// The key an approval card is filed under.
pub fn approval_key(id: &str) -> String {
    format!("approval:{id}")
}

/// The key a question card is filed under.
pub fn question_key(id: &str) -> String {
    format!("question:{id}")
}

/// One block's entry in its turn's ledger, parallel to the turn's blocks.
#[derive(Clone, Debug, Default)]
struct Entry {
    /// The keys filed on this block.
    keys: Vec<String>,
    /// Its log order ([`TranscriptBuilder::set_order`]); `u64::MAX` for a
    /// block that named none, which belongs after everything filed.
    order: u64,
}

/// What a streamed chunk appends to ([`TranscriptBuilder::append`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    /// A text block's text.
    Text,
    /// A thinking block's text.
    Thinking,
    /// A shell card's output.
    Output,
}

/// Positional deltas out of id-keyed facts. See the crate docs.
#[derive(Clone, Debug)]
pub struct TranscriptBuilder {
    /// The transcript the emitted deltas build: positions are read here.
    session: Session,
    /// Turn id → one entry per block of that turn, in block order.
    ledger: HashMap<String, Vec<Entry>>,
    /// Key → the turn its block sits in.
    homes: HashMap<String, String>,
    /// Approval ids whose cards are raised and not yet resolved, oldest
    /// first: what [`TranscriptBuilder::abandon`] settles.
    open_approvals: Vec<String>,
    /// The log order the next added blocks take (see the module docs).
    order: Option<u64>,
    /// Whether a delta that did not land is still emitted (the default:
    /// the app's copy ignores it the same way) or dropped (a fold that
    /// returns only what landed).
    emit_unlanded: bool,
    /// The turns changed since the last [`TranscriptBuilder::take_dirty`],
    /// or `None` when that is unknown.
    dirty: Option<HashSet<String>>,
    /// Deltas emitted since the last [`TranscriptBuilder::take`].
    out: Vec<Delta>,
}

impl Default for TranscriptBuilder {
    /// An empty transcript whose header names `Provider::Claude`: a fold
    /// that never reads the header (only the turns) need not choose.
    fn default() -> Self {
        Self::new(Provider::Claude)
    }
}

impl TranscriptBuilder {
    /// An empty transcript for one session of `provider`.
    pub fn new(provider: Provider) -> Self {
        Self::with_session(Session::new("", provider, "", ""))
    }

    /// A builder over `session`, which holds no turns yet: its header (id,
    /// provider, model, cwd, mode) is the fold's to keep
    /// ([`TranscriptBuilder::header_mut`]).
    pub fn with_session(session: Session) -> Self {
        Self {
            session,
            ledger: HashMap::new(),
            homes: HashMap::new(),
            open_approvals: Vec::new(),
            order: None,
            emit_unlanded: true,
            dirty: None,
            out: Vec::new(),
        }
    }

    /// Emit only the deltas that landed (a fold that hands back exactly
    /// what changed its transcript).
    pub fn landed_only(mut self) -> Self {
        self.emit_unlanded = false;
        self
    }

    /// The deltas emitted since the last take, in order.
    pub fn take(&mut self) -> Vec<Delta> {
        std::mem::take(&mut self.out)
    }

    /// The transcript the emitted deltas build.
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// The transcript's header, for a fold that keeps one (model, cwd,
    /// mode, branch). Turns and blocks change only through deltas.
    pub fn header_mut(&mut self) -> &mut Session {
        &mut self.session
    }

    /// Restamp `turn_id`'s clock in place: a refinement the wire makes
    /// without a delta. Marks the turn changed when it moved.
    pub fn set_timestamp(&mut self, turn_id: &str, at: u64) {
        let Some(turn) = self.session.turn_mut(turn_id) else { return };
        let slot = match turn {
            Turn::User { timestamp, .. } | Turn::Assistant { timestamp, .. } => timestamp,
        };
        if *slot != Some(at) {
            *slot = Some(at);
            self.mark_dirty(turn_id);
        }
    }

    /// The turns changed since the last call, and start counting afresh.
    /// `None` means unknown: the first call.
    pub fn take_dirty(&mut self) -> Option<HashSet<String>> {
        self.dirty.replace(HashSet::new())
    }

    fn mark_dirty(&mut self, turn_id: &str) {
        if let Some(dirty) = self.dirty.as_mut() {
            dirty.insert(turn_id.to_owned());
        }
    }

    /// The log order the blocks added from now on take; `None` appends.
    pub fn set_order(&mut self, order: Option<u64>) {
        self.order = order;
    }

    /// The log order the blocks added now take, if one is set.
    pub fn order(&self) -> Option<u64> {
        self.order
    }

    /// The log order of `turn_id`'s last block, if it has any.
    pub fn last_order(&self, turn_id: &str) -> Option<u64> {
        self.ledger.get(turn_id)?.last().map(|entry| entry.order)
    }

    /// Whether a turn with this id is in the transcript.
    pub fn has_turn(&self, turn_id: &str) -> bool {
        self.session.turn(turn_id).is_some()
    }

    /// The block filed under `key`, if it is still in the transcript.
    pub fn block(&self, key: &str) -> Option<&Block> {
        let (turn, index) = self.position(key)?;
        self.session.turn(&turn)?.blocks().get(index)
    }

    /// The turn hosting the block filed under `key`.
    pub fn turn_of(&self, key: &str) -> Option<&str> {
        self.homes.get(key).map(String::as_str)
    }

    /// Whether the block filed under `key` is its turn's last.
    pub fn is_last(&self, key: &str) -> bool {
        self.position(key)
            .is_some_and(|(turn, index)| self.ledger.get(&turn).is_some_and(|held| index + 1 == held.len()))
    }

    /// `(turn id, block index)` of the block filed under `key`.
    fn position(&self, key: &str) -> Option<(String, usize)> {
        let turn = self.homes.get(key)?;
        let index = self.ledger.get(turn)?.iter().position(|entry| entry.keys.iter().any(|held| held == key))?;
        Some((turn.clone(), index))
    }

    /// Apply `delta` to the transcript, keep the ledger parallel to it, and
    /// emit it (unless it did not land and this builder emits only what
    /// lands).
    fn emit(&mut self, delta: Delta, key: Option<String>) -> bool {
        let turn_id = match &delta {
            Delta::TurnStarted { turn } | Delta::TurnReplaced { turn } => turn.id().to_owned(),
            Delta::TextDelta { turn_id, .. }
            | Delta::BlockAdded { turn_id, .. }
            | Delta::BlockUpdated { turn_id, .. }
            | Delta::ThinkingDelta { turn_id, .. }
            | Delta::ToolOutputDelta { turn_id, .. }
            | Delta::TurnRemoved { turn_id }
            | Delta::BlockRemoved { turn_id, .. }
            | Delta::TurnFinished { turn_id, .. } => turn_id.clone(),
        };
        let removed = match &delta {
            Delta::BlockRemoved { block_index, .. } => Some(*block_index),
            _ => None,
        };
        let (added, turn_removed) =
            (matches!(delta, Delta::BlockAdded { .. }), matches!(delta, Delta::TurnRemoved { .. }));
        let landed = self.session.apply(delta.clone());
        if landed {
            self.mark_dirty(&turn_id);
            if added {
                if let Some(key) = &key {
                    self.unfile(key);
                    self.homes.insert(key.clone(), turn_id.clone());
                }
                let order = self.order.unwrap_or(u64::MAX);
                self.ledger.entry(turn_id.clone()).or_default().push(Entry { keys: key.into_iter().collect(), order });
            } else if let Some(index) = removed {
                if let Some(held) = self.ledger.get_mut(&turn_id).filter(|held| index < held.len()) {
                    for key in held.remove(index).keys {
                        self.homes.remove(&key);
                    }
                }
            } else if turn_removed && !self.has_turn(&turn_id) {
                for key in self.ledger.remove(&turn_id).into_iter().flatten().flat_map(|entry| entry.keys) {
                    self.homes.remove(&key);
                }
            }
        }
        if landed || self.emit_unlanded {
            self.out.push(delta);
        }
        landed
    }

    /// Take `key` off whatever block it named before (a re-filed key names
    /// only its newest block).
    fn unfile(&mut self, key: &str) {
        let Some(turn) = self.homes.remove(key) else { return };
        if let Some(held) = self.ledger.get_mut(&turn) {
            for entry in held.iter_mut() {
                entry.keys.retain(|held| held != key);
            }
        }
    }

    /// File `key` on the block `with` is filed on (a call joining a group
    /// card). `false` when `with` names no block.
    pub fn file_with(&mut self, key: &str, with: &str) -> bool {
        let Some((turn, index)) = self.position(with) else { return false };
        self.file_at(key, &turn, index)
    }

    /// File `key` on block `index` of `turn_id` — for a card a fold found
    /// by reading the transcript. `false` when there is no such block.
    pub fn file_at(&mut self, key: &str, turn_id: &str, index: usize) -> bool {
        if self.ledger.get(turn_id).is_none_or(|held| index >= held.len()) {
            return false;
        }
        self.unfile(key);
        self.homes.insert(key.to_owned(), turn_id.to_owned());
        if let Some(entry) = self.ledger.get_mut(turn_id).and_then(|held| held.get_mut(index)) {
            entry.keys.push(key.to_owned());
        }
        true
    }

    /// Emit a delta that addresses no block by key (a user turn, a removal
    /// by turn id, a provider's own delta): applied and passed through.
    pub fn push(&mut self, delta: Delta) -> bool {
        self.emit(delta, None)
    }

    /// Open an assistant turn unless one with this id is already in the
    /// transcript. `true` when it was opened now.
    pub fn open_turn(&mut self, turn_id: &str, timestamp: Option<u64>) -> bool {
        if self.has_turn(turn_id) {
            return false;
        }
        self.emit(
            Delta::TurnStarted {
                turn: Turn::Assistant {
                    id: turn_id.to_owned(),
                    blocks: Vec::new(),
                    meta: TurnMeta::default(),
                    timestamp,
                },
            },
            None,
        );
        true
    }

    /// Add `block` to `turn_id`, filed under `key` when given (a key
    /// already filed moves to this block): appended, or — with a log order
    /// set — inserted after every block filed at or before that order.
    pub fn add(&mut self, turn_id: &str, key: Option<&str>, block: Block) -> bool {
        let order = self.order.unwrap_or(u64::MAX);
        // `<=` so two blocks of one log record keep the order they were
        // added in.
        let at = self.ledger.get(turn_id).map_or(0, |held| held.partition_point(|entry| entry.order <= order));
        let delta = Delta::BlockAdded { turn_id: turn_id.to_owned(), block: block.clone() };
        if !self.emit(delta, key.map(str::to_owned)) {
            return false;
        }
        let Some(held) = self.ledger.get_mut(turn_id) else { return true };
        let last = held.len() - 1;
        if at >= last {
            return true;
        }
        // Rotate `[at, last]` right by one: each old occupant moves down a
        // block and the new one takes `at`.
        held[at..].rotate_right(1);
        let mut moved: Vec<Block> = self.session.turn(turn_id).map_or(&[][..], Turn::blocks)[at..last].to_vec();
        moved.insert(0, block);
        for (offset, block) in moved.into_iter().enumerate() {
            let delta = Delta::BlockUpdated { turn_id: turn_id.to_owned(), block_index: at + offset, block };
            self.emit(delta, None);
        }
        true
    }

    /// Replace the block filed under `key`. `false` when no block is (or,
    /// for a builder that emits only what lands, when it did not land).
    pub fn update(&mut self, key: &str, block: Block) -> bool {
        let Some((turn_id, block_index)) = self.position(key) else { return false };
        self.emit(Delta::BlockUpdated { turn_id, block_index, block }, None)
    }

    /// Replace block `index` of `turn_id` — for a card a fold found by
    /// reading the transcript, not by a key.
    pub fn update_at(&mut self, turn_id: &str, index: usize, block: Block) -> bool {
        self.emit(Delta::BlockUpdated { turn_id: turn_id.to_owned(), block_index: index, block }, None)
    }

    /// Re-face the block filed under `key` in place: `change` edits a copy
    /// of it, and the copy replaces it. `false` when no block is filed.
    pub fn modify(&mut self, key: &str, change: impl FnOnce(&mut Block)) -> bool {
        let Some(mut block) = self.block(key).cloned() else { return false };
        change(&mut block);
        self.update(key, block)
    }

    /// Update the block filed under `key`, or append it to `turn_id` filed
    /// under `key` when none is.
    pub fn upsert(&mut self, turn_id: &str, key: &str, block: Block) {
        if !self.update(key, block.clone()) {
            self.add(turn_id, Some(key), block);
        }
    }

    /// Append a streamed chunk to the block filed under `key`.
    pub fn append(&mut self, key: &str, stream: Stream, text: &str) -> bool {
        let Some((turn_id, block_index)) = self.position(key) else { return false };
        let text = text.to_owned();
        let delta = match stream {
            Stream::Text => Delta::TextDelta { turn_id, block_index, text },
            Stream::Thinking => Delta::ThinkingDelta { turn_id, block_index, text },
            Stream::Output => Delta::ToolOutputDelta { turn_id, block_index, text },
        };
        self.emit(delta, None)
    }

    /// Remove the block filed under `key`.
    pub fn remove(&mut self, key: &str) -> bool {
        let Some((turn_id, block_index)) = self.position(key) else { return false };
        self.emit(Delta::BlockRemoved { turn_id, block_index }, None)
    }

    /// Take `turn_id` out of the transcript, with every key it hosted.
    pub fn remove_turn(&mut self, turn_id: &str) -> bool {
        self.has_turn(turn_id) && self.emit(Delta::TurnRemoved { turn_id: turn_id.to_owned() }, None)
    }

    /// Close `turn_id` with its footer.
    pub fn finish_turn(&mut self, turn_id: &str, meta: TurnMeta) {
        self.emit(Delta::TurnFinished { turn_id: turn_id.to_owned(), meta }, None);
    }

    // ---------------------------------------------------------------- tools

    /// A tool call opened: its card appended to `turn_id`, filed under the
    /// call's id.
    pub fn tool_started(&mut self, turn_id: &str, event: ToolEvent) {
        let key = tool_key(&event.id);
        self.add(turn_id, Some(&key), event.block());
    }

    /// A tool call finished: its open card replaced in place, or — a call
    /// whose start was never seen (history, a missed start) — appended to
    /// `turn_id` and filed, so a later frame of the same call finds it.
    pub fn tool_finished(&mut self, turn_id: &str, event: ToolEvent) {
        let key = tool_key(&event.id);
        self.upsert(turn_id, &key, event.block());
    }

    /// The ids of `turn_id`'s tool cards still reading running.
    pub fn running_tools(&self, turn_id: &str) -> Vec<String> {
        let Some(turn) = self.session.turn(turn_id) else { return Vec::new() };
        let held = self.ledger.get(turn_id).map(Vec::as_slice).unwrap_or_default();
        turn.blocks()
            .iter()
            .zip(held)
            .filter(|(block, _)| matches!(block, Block::ToolCall { status: aui_protocol::ToolStatus::Running, .. }))
            .flat_map(|(_, entry)| entry.keys.iter().filter_map(|key| key.strip_prefix("tool:")))
            .map(str::to_owned)
            .collect()
    }

    // ------------------------------------------------------------ approvals

    /// An approval card raised on `turn_id` (`card` is a
    /// [`Block::Approval`]; its id files it). A card already open for that
    /// id raises nothing twice: it moves only when it is resolved.
    pub fn approval_raised(&mut self, turn_id: &str, card: Block) -> bool {
        let Block::Approval { id, .. } = &card else { return false };
        let id = id.clone();
        if id.is_empty() || self.open_approvals.contains(&id) {
            return false;
        }
        self.add(turn_id, Some(&approval_key(&id)), card);
        self.open_approvals.push(id);
        true
    }

    /// Whether an approval card for `id` is raised and not yet resolved.
    pub fn approval_open(&self, id: &str) -> bool {
        self.open_approvals.iter().any(|open| open == id)
    }

    /// The open approvals, oldest first.
    pub fn open_approvals(&self) -> &[String] {
        &self.open_approvals
    }

    /// Re-face an open approval to `state` without settling it: a later
    /// resolution can still correct it.
    pub fn approval_restated(&mut self, id: &str, state: ApprovalState) -> bool {
        if !self.approval_open(id) {
            return false;
        }
        self.modify(&approval_key(id), |card| set_state(card, state))
    }

    /// Settle an open approval to `state`: the only thing that moves the
    /// card after the press. A repeat resolution is a no-op.
    pub fn approval_resolved(&mut self, id: &str, state: ApprovalState) -> bool {
        let Some(at) = self.open_approvals.iter().position(|open| open == id) else { return false };
        self.open_approvals.remove(at);
        self.modify(&approval_key(id), |card| set_state(card, state))
    }

    /// Settle an approval with a card rebuilt by the fold (a face that
    /// changed with the resolution): the card is replaced whether or not
    /// it was still open — a late word from the wire is still the word —
    /// and it is open no more.
    pub fn approval_resolved_as(&mut self, id: &str, card: Block) -> bool {
        self.open_approvals.retain(|open| open != id);
        self.update(&approval_key(id), card)
    }

    // ------------------------------------------------------------ questions

    /// A question card raised on `turn_id`, filed under its block id.
    pub fn question_raised(&mut self, turn_id: &str, id: &str, card: Block) {
        self.add(turn_id, Some(&question_key(id)), card);
    }

    /// A question card answered (or declined): its new face in place.
    pub fn question_answered(&mut self, id: &str, card: Block) -> bool {
        self.update(&question_key(id), card)
    }

    // ----------------------------------------------------------- child loss

    /// The child is gone (docs/71 §2): every open approval settles to
    /// `state(id)` — nothing will ever answer it — and, when `turn_id`
    /// names the turn it was running, that turn carries one error card of
    /// `card_kind` naming `reason`. Closing the turn stays with the fold,
    /// which knows its footer.
    pub fn abandon(
        &mut self,
        turn_id: Option<&str>,
        card_kind: &str,
        reason: &str,
        state: impl Fn(&str) -> ApprovalState,
    ) {
        for id in self.open_approvals.clone() {
            self.approval_resolved(&id, state(&id));
        }
        if let Some(turn_id) = turn_id {
            self.add(
                turn_id,
                None,
                Block::Generic { kind: card_kind.to_owned(), status: "error".into(), text: reason.to_owned() },
            );
        }
    }
}

fn set_state(card: &mut Block, state: ApprovalState) {
    if let Block::Approval { state: slot, .. } = card {
        *slot = state;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aui_protocol::{ApprovalState, ToolBody, ToolKind, ToolStatus};

    fn text(text: &str) -> Block {
        Block::Text { text: text.into(), streaming: false }
    }

    fn shell(id: &str, status: ToolStatus) -> ToolEvent {
        ToolEvent::new(id, ToolKind::Shell, "ls", status)
            .body(ToolBody::Shell { output_lines: Vec::new(), exit_code: None, live: false })
    }

    /// The app's copy, folded from the same deltas, holds the same blocks.
    fn replayed(builder: &mut TranscriptBuilder) -> Session {
        let mut session = Session::new("", Provider::Codex, "", "");
        for delta in builder.take() {
            session.apply(delta);
        }
        session
    }

    #[test]
    fn a_key_finds_its_block_after_a_removal_moved_it() {
        let mut builder = TranscriptBuilder::new(Provider::Codex);
        builder.open_turn("t", None);
        builder.add("t", Some("a"), text("a"));
        builder.tool_started("t", shell("x", ToolStatus::Running));
        builder.add("t", Some("b"), text("b"));
        assert!(builder.remove("a"));
        builder.tool_finished("t", shell("x", ToolStatus::Success));
        let session = replayed(&mut builder);
        let blocks = session.turn("t").unwrap().blocks();
        assert!(matches!(&blocks[0], Block::ToolCall { status: ToolStatus::Success, verb, .. } if verb == "Ran"));
        assert_eq!(blocks[1], text("b"));
        assert_eq!(builder.session(), &session);
    }

    #[test]
    fn a_block_that_never_lands_takes_no_index() {
        let mut builder = TranscriptBuilder::new(Provider::Codex);
        builder.add("missing", Some("lost"), text("lost"));
        builder.open_turn("t", None);
        builder.add("t", Some("a"), text("a"));
        assert!(builder.update("a", text("A")));
        assert!(builder.block("lost").is_none());
        assert_eq!(replayed(&mut builder).turn("t").unwrap().blocks(), &[text("A")]);
    }

    #[test]
    fn approvals_raise_once_settle_once_and_abandon_settles_the_rest() {
        let mut builder = TranscriptBuilder::new(Provider::Codex);
        builder.open_turn("t", None);
        let card = |id: &str| crate::approval::card(id, "Bash", "ls", "why", "", Default::default(), Vec::new());
        assert!(builder.approval_raised("t", card("a")));
        assert!(!builder.approval_raised("t", card("a")), "an open card raises nothing twice");
        assert!(builder.approval_raised("t", card("b")));
        assert!(builder.approval_resolved("a", ApprovalState::Denied));
        assert!(!builder.approval_resolved("a", ApprovalState::Denied), "settled once");
        builder.abandon(Some("t"), "child-exited", "gone", |_| ApprovalState::Cancelled { reason: None });
        let session = replayed(&mut builder);
        let blocks = session.turn("t").unwrap().blocks();
        assert!(matches!(&blocks[0], Block::Approval { state: ApprovalState::Denied, .. }));
        assert!(matches!(&blocks[1], Block::Approval { state: ApprovalState::Cancelled { .. }, .. }));
        assert!(matches!(&blocks[2], Block::Generic { kind, .. } if kind == "child-exited"));
        assert!(builder.open_approvals().is_empty());
    }

    #[test]
    fn running_tools_name_only_open_keyed_cards() {
        let mut builder = TranscriptBuilder::new(Provider::Codex);
        builder.open_turn("t", None);
        builder.tool_started("t", shell("a", ToolStatus::Running));
        builder.tool_started("t", shell("b", ToolStatus::Running));
        builder.add("t", None, shell("c", ToolStatus::Running).block());
        builder.tool_finished("t", shell("a", ToolStatus::Success));
        assert_eq!(builder.running_tools("t"), vec!["b".to_owned()]);
    }
}
