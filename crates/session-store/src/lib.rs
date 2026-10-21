//! `session-store` — what one session view renders, for any provider.
//!
//! A [`SessionStore`] is a session's transcript folded from neutral
//! [`Delta`]s, the facts beside it ([`SessionSide`]), which turns changed
//! since the view last looked, the turns the client wrote itself (the plan
//! and handoff cards), and the prompts the client sent. Nothing here names a
//! provider or a wire: adapters translate their own events into deltas and
//! facts (`provider-muse` through its MSP fold, the Claude Code and Codex
//! adapters through theirs) and the store only folds.
//!
//! Pure — no I/O, no clock — so a test folds the same deltas into two stores
//! and compares.

#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

use std::collections::{BTreeMap, HashSet};

use aui_protocol::{Block, Delta, PermissionMode, Provider, Session, Turn, TurnMeta};
pub use provider::facts::SessionSide;

/// How many sent prompts a store remembers. A submission still queued keeps
/// its text past the cap: the queue strip renders it and Edit hands it back.
pub const COMMAND_TEXT_CAP: usize = 256;

/// One session as a view renders it.
#[derive(Clone, Debug)]
pub struct SessionStore {
    session: Session,
    /// Whether anything has spoken for the session yet: a provider opened
    /// it, or an adapter folded something into it. Until then
    /// [`SessionStore::session`] is `None`, the way an empty view has no
    /// session to draw.
    live: bool,
    side: SessionSide,
    /// Turn ids written since the last [`SessionStore::take_dirty`]; `None`
    /// means "unknown — treat every turn as changed".
    dirty: Option<HashSet<String>>,
    /// Request id → the text it was sent with.
    command_text: BTreeMap<String, String>,
    /// Request ids in the order they were remembered, for the cap.
    command_order: Vec<String>,
}

/// The turn a delta writes to.
fn delta_turn_id(delta: &Delta) -> Option<&str> {
    match delta {
        Delta::TurnStarted { turn } | Delta::TurnReplaced { turn } => Some(turn.id()),
        Delta::TextDelta { turn_id, .. }
        | Delta::BlockAdded { turn_id, .. }
        | Delta::BlockUpdated { turn_id, .. }
        | Delta::ThinkingDelta { turn_id, .. }
        | Delta::ToolOutputDelta { turn_id, .. }
        | Delta::TurnRemoved { turn_id }
        | Delta::BlockRemoved { turn_id, .. }
        | Delta::TurnFinished { turn_id, .. } => Some(turn_id),
        #[allow(unreachable_patterns)]
        _ => None,
    }
}

impl SessionStore {
    /// A store for `session_id`, driven by `agent`, in `cwd`. Not live: see
    /// [`SessionStore::ensure_live`].
    pub fn new(session_id: &str, agent: Provider, cwd: impl Into<String>) -> Self {
        let mut session = Session::new(session_id, agent, String::new(), cwd);
        // The wire default a session gets when its open names no mode.
        session.mode = PermissionMode::OnRequest;
        Self {
            session,
            live: false,
            side: SessionSide::default(),
            dirty: None,
            command_text: BTreeMap::new(),
            command_order: Vec::new(),
        }
    }

    /// The session, once anything has spoken for it.
    pub fn session(&self) -> Option<&Session> {
        self.live.then_some(&self.session)
    }

    /// Whether anything has spoken for the session yet.
    pub fn is_live(&self) -> bool {
        self.live
    }

    /// Mark the session live with who drives it and where: a provider lane
    /// that just opened. A no-op past the first call, like the lane that
    /// created it owning it.
    pub fn ensure_live(&mut self, agent: Provider, model: impl Into<String>, cwd: impl Into<String>) {
        if self.live {
            return;
        }
        self.live = true;
        self.session.agent = agent;
        self.session.model = model.into();
        self.session.cwd = cwd.into();
        self.dirty = None;
    }

    /// Take a whole session as it stands — a capture folded in one go —
    /// keeping the facts and remembered prompts. Every turn reads changed.
    pub fn load(&mut self, session: Session) {
        self.session = session;
        self.live = true;
        self.dirty = None;
    }

    /// The session's id.
    pub fn id(&self) -> &str {
        &self.session.id
    }

    /// Take a new id: a provisional open id renamed to the session's real
    /// one. Every turn stays.
    pub fn rename(&mut self, session_id: &str) {
        self.session.id = session_id.to_owned();
    }

    /// The facts beside the transcript.
    pub fn side(&self) -> &SessionSide {
        &self.side
    }

    /// Replace the facts wholesale: what an adapter reports as the
    /// session's side now.
    pub fn set_side(&mut self, side: SessionSide) {
        self.side = side;
    }

    /// Change the facts in place.
    pub fn side_mut(&mut self) -> &mut SessionSide {
        &mut self.side
    }

    /// Fold `deltas` in order and return the ones that landed. A delta for
    /// a turn or block that is gone is ignored, never an error; a store that
    /// is not live folds nothing.
    pub fn apply_deltas(&mut self, deltas: Vec<Delta>) -> Vec<Delta> {
        if !self.live {
            return Vec::new();
        }
        let mut landed = Vec::with_capacity(deltas.len());
        for delta in deltas {
            let turn = delta_turn_id(&delta).map(str::to_owned);
            if self.session.apply(delta.clone()) {
                self.mark_dirty(turn);
                landed.push(delta);
            }
        }
        landed
    }

    /// Copy what is not transcript from `from` — who drives it, the model,
    /// the mode, the plan overlay, where it runs — and make the store live.
    /// What an adapter that folds whole sessions reports as the header.
    pub fn sync_header(&mut self, from: &Session) {
        self.live = true;
        let session = &mut self.session;
        session.agent = from.agent;
        if session.model != from.model {
            session.model.clone_from(&from.model);
        }
        session.mode = from.mode;
        session.plan = from.plan;
        if session.cwd != from.cwd {
            session.cwd.clone_from(&from.cwd);
        }
        if session.branch != from.branch {
            session.branch.clone_from(&from.branch);
        }
        session.environment = from.environment.clone();
    }

    /// Take `from`'s timestamp for each named turn that differs: a clock an
    /// adapter refines on a turn after the turn's own delta.
    pub fn sync_timestamps<'a>(&mut self, from: &Session, turn_ids: impl IntoIterator<Item = &'a str>) {
        let theirs: std::collections::HashMap<&str, Option<u64>> =
            from.turns.iter().map(|turn| (turn.id(), turn_timestamp(turn))).collect();
        let wanted: HashSet<&str> = turn_ids.into_iter().collect();
        let mut changed = Vec::new();
        for turn in self.session.turns.iter_mut() {
            let id = turn.id();
            if !wanted.contains(id) {
                continue;
            }
            let Some(&their_stamp) = theirs.get(id) else { continue };
            let slot = match turn {
                Turn::User { timestamp, .. } | Turn::Assistant { timestamp, .. } => timestamp,
            };
            if *slot != their_stamp {
                *slot = their_stamp;
                changed.push(id_of(turn));
            }
        }
        for id in changed {
            self.mark_dirty(Some(id));
        }
    }

    /// Set the approval mode; `false` when it already held it.
    pub fn set_mode(&mut self, mode: PermissionMode) -> bool {
        if self.session.mode == mode {
            return false;
        }
        self.session.mode = mode;
        true
    }

    /// Set the plan overlay; `false` when it already held it.
    pub fn set_plan(&mut self, on: bool) -> bool {
        if self.session.plan == on {
            return false;
        }
        self.session.plan = on;
        true
    }

    /// The turn ids changed since the last call, and start counting afresh.
    /// `None` means "unknown — treat every turn as changed".
    pub fn take_dirty(&mut self) -> Option<HashSet<String>> {
        self.dirty.replace(HashSet::new())
    }

    fn mark_dirty(&mut self, turn: Option<String>) {
        match (turn, self.dirty.as_mut()) {
            (Some(turn), Some(dirty)) => {
                dirty.insert(turn);
            }
            (None, _) => self.dirty = None,
            (Some(_), None) => {}
        }
    }

    /// Remember the text a request was sent with: retry resends it, the
    /// first prompt titles the row, the queue strip shows it.
    pub fn record_command(&mut self, request_id: &str, text: &str) {
        if self.command_text.insert(request_id.to_owned(), text.to_owned()).is_none() {
            self.command_order.push(request_id.to_owned());
        }
        while self.command_order.len() > COMMAND_TEXT_CAP {
            let queued = &self.side.queued;
            let Some(at) = self.command_order.iter().position(|id| !queued.iter().any(|q| &q.request_id == id)) else {
                break;
            };
            let oldest = self.command_order.remove(at);
            self.command_text.remove(&oldest);
        }
    }

    /// Forget a request's text: its submission left the transcript.
    pub fn forget_command(&mut self, request_id: &str) {
        if self.command_text.remove(request_id).is_some() {
            self.command_order.retain(|held| held != request_id);
        }
    }

    /// The text a request was sent with, if this client sent it.
    pub fn command_text(&self, request_id: &str) -> Option<&str> {
        self.command_text.get(request_id).map(String::as_str)
    }

    /// The text of the greatest remembered request id: request ids are
    /// time-ordered (UUID v7), so this is the latest prompt sent.
    pub fn last_command_text(&self) -> Option<&str> {
        self.command_text.values().next_back().map(String::as_str)
    }

    /// Every remembered request id.
    pub fn command_ids(&self) -> impl Iterator<Item = &str> {
        self.command_text.keys().map(String::as_str)
    }

    /// Append a block the client authored, in a turn of its own (`id`): the
    /// plan card and the handoff card. A turn already under `id` takes the
    /// block at its end.
    pub fn append_client_block(&mut self, id: &str, block: Block) -> Vec<Delta> {
        let mut deltas = Vec::new();
        if !self.session.turns.iter().any(|turn| turn.id() == id) {
            deltas.push(Delta::TurnStarted {
                turn: Turn::Assistant { id: id.to_owned(), blocks: Vec::new(), meta: TurnMeta::default(), timestamp: None },
            });
        }
        deltas.push(Delta::BlockAdded { turn_id: id.to_owned(), block });
        self.apply_deltas(deltas)
    }

    /// Replace the first block of a client-authored turn.
    pub fn replace_client_block(&mut self, id: &str, block: Block) -> Vec<Delta> {
        self.apply_deltas(vec![Delta::BlockUpdated { turn_id: id.to_owned(), block_index: 0, block }])
    }
}

fn id_of(turn: &Turn) -> String {
    turn.id().to_owned()
}

fn turn_timestamp(turn: &Turn) -> Option<u64> {
    match turn {
        Turn::User { timestamp, .. } | Turn::Assistant { timestamp, .. } => *timestamp,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live() -> SessionStore {
        let mut store = SessionStore::new("s1", Provider::Claude, "/w");
        store.ensure_live(Provider::Claude, "m", "/w");
        store
    }

    fn user(id: &str, text: &str) -> Delta {
        Delta::TurnStarted {
            turn: Turn::User { id: id.into(), text: text.into(), attachments: Vec::new(), mentions: Vec::new(), timestamp: None },
        }
    }

    #[test]
    fn a_store_that_is_not_live_folds_nothing() {
        let mut store = SessionStore::new("s1", Provider::Muse, "/w");
        assert!(store.session().is_none());
        assert!(store.apply_deltas(vec![user("u1", "hi")]).is_empty());
        store.ensure_live(Provider::Muse, "", "/w");
        assert_eq!(store.apply_deltas(vec![user("u1", "hi")]).len(), 1);
        assert_eq!(store.session().unwrap().turns.len(), 1);
    }

    #[test]
    fn dirty_names_the_turns_written_since_the_last_look() {
        let mut store = live();
        assert_eq!(store.take_dirty(), None, "the first look is unknown");
        store.apply_deltas(vec![user("u1", "hi"), user("u2", "there")]);
        let dirty = store.take_dirty().unwrap();
        assert!(dirty.contains("u1") && dirty.contains("u2"));
        assert_eq!(store.take_dirty(), Some(HashSet::new()));
    }

    #[test]
    fn a_delta_for_a_gone_turn_does_not_land() {
        let mut store = live();
        let landed = store.apply_deltas(vec![Delta::TextDelta { turn_id: "nope".into(), block_index: 0, text: "x".into() }]);
        assert!(landed.is_empty());
    }

    #[test]
    fn client_blocks_get_their_own_turn_and_replace_in_place() {
        let mut store = live();
        let plan = Block::Marker { kind: aui_protocol::MarkerKind::TurnRetracted, text: "one".into() };
        assert_eq!(store.append_client_block("plan-1", plan).len(), 2);
        let again = Block::Marker { kind: aui_protocol::MarkerKind::TurnRetracted, text: "two".into() };
        assert_eq!(store.replace_client_block("plan-1", again.clone()).len(), 1);
        let turn = &store.session().unwrap().turns[0];
        assert_eq!(turn.id(), "plan-1");
        assert_eq!(turn.blocks(), &[again]);
    }

    #[test]
    fn a_queued_prompt_keeps_its_text_past_the_cap() {
        let mut store = live();
        store.side_mut().queued.push(provider::facts::QueuedTurn { turn_id: "t0".into(), request_id: "r0".into() });
        for n in 0..=COMMAND_TEXT_CAP + 3 {
            store.record_command(&format!("r{n}"), "text");
        }
        assert_eq!(store.command_text("r0"), Some("text"), "still queued");
        assert_eq!(store.command_text("r1"), None, "evicted");
        assert_eq!(store.command_ids().count(), COMMAND_TEXT_CAP);
    }

    #[test]
    fn rename_keeps_the_transcript() {
        let mut store = live();
        store.apply_deltas(vec![user("u1", "hi")]);
        store.rename("real");
        assert_eq!(store.id(), "real");
        assert_eq!(store.session().unwrap().turns.len(), 1);
    }
}
