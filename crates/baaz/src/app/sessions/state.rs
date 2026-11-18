//! The session open/switch/replace state machine and the handoff runs, as
//! two plain structs [`Harness`] owns (docs/81 A R3). Before, these were
//! two dozen loose `Harness` fields whose invariants lived wherever a
//! caller happened to poke them (`new_session_in`, `activate`,
//! `close_view`, `cancel_pending_switch`, `finish_provider_open`); here the
//! generations, the claim and the replace marker are private and move only
//! through the methods below, so each transition is one call.
//!
//! Pure: no window, no entity. The window work a transition implies (park
//! a view, restore a chip, dialog) stays with the caller in the sibling
//! modules.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::app::LaunchFailure;
use crate::handoff::{HandoffConfirmState, HandoffRun, PendingHandoff};
use crate::providers::ProviderId;
use crate::session::Draft;

/// What a provider switch carries from the replaced draft to the draft that
/// replaces it: its approval mode and effort.
pub(crate) type SwitchCarry = (aui_protocol::PermissionMode, Option<aui_protocol::ReasoningEffort>);

/// The view a provider switch is replacing (Y2b): kept active and drawn —
/// composer locked, chip already on the pick — until the replacement
/// activates in the same update.
#[derive(Clone, Debug, PartialEq)]
struct Replacing {
    /// The old session id.
    old_id: String,
    /// The drafts-map project that named the replaced view, restored when
    /// the switch aborts.
    draft_project: Option<String>,
    /// The switch epoch stamped when the marker was set: a landing only
    /// closes the replaced view while this still matches the current
    /// switch, so a view that lands after a cancel or a superseding switch
    /// never closes a view it did not replace.
    epoch: u64,
    /// The replaced draft's mode and effort, taken by the replacement
    /// draft when it is drawn (once).
    carry: Option<SwitchCarry>,
}

/// Where the window is in opening, switching to and replacing sessions,
/// plus the drafts that have no row yet. Owned by [`Harness`] as
/// `open_state`.
///
/// Keyed by session id: `pending_id`, `drafts` (values), `unsent`,
/// `turns_admitted`, `starts_in_flight`, the claim and the replace marker.
/// A provider draft's provisional id is renamed in all of them by
/// [`OpenState::rekey`].
#[derive(Default)]
pub(crate) struct OpenState {
    /// The session the UI is pointed at: the sidebar click's target, set the
    /// moment `resume` runs. The centre swaps synchronously, so this usually
    /// names the active view — but the row highlights and the header label
    /// read it, never the view, so the click is acknowledged on its own frame.
    pub(crate) pending_id: Option<String>,
    /// One unsent draft session per project, by project id: what ⌘N returns
    /// to while nothing has been sent. A draft has no sidebar row; the entry
    /// leaves the map the moment its first turn is accepted (`turn/started`).
    pub(crate) drafts: HashMap<String, String>,
    /// New sessions with no project to name them in `drafts` (an unadopted
    /// root, a provider open with nowhere current): unsent all the same,
    /// so no row until their first turn starts, which forgets them here.
    pub(crate) unsent: HashSet<String>,
    /// Sessions whose turn this window admitted (a send landed as
    /// `ProviderTurnAccepted`) since launch. Only their settles are live
    /// activity that moves a row's time: every other settle is a reopen
    /// replaying history, and opening a session never moves its row.
    pub(crate) turns_admitted: HashSet<String>,
    /// A draft taken from another project's session, waiting for the picked
    /// project's draft session to exist so it can be moved in. Set by the
    /// project menu's retarget, consumed by `new_session_in`.
    pub(crate) pending_draft: Option<Draft>,
    /// A session is still opening — a `new:` step's start, a provider open,
    /// reopen or fork, or a provider switch: its round-trip lands after the
    /// following steps would run. Session verbs wait for it (bounded)
    /// instead of acting on the session that is still open; any activation
    /// clears it ([`Self::land`]).
    pub(crate) switch_pending: bool,
    /// New sessions whose view opened on the click while their open is
    /// still on the wire. Not a switch — nothing waits on them but the
    /// scripted session verbs (see [`Self::session_opening`]), so a second
    /// "New session" elsewhere is never swallowed behind a slow start.
    pub(crate) starts_in_flight: HashSet<String>,
    /// Z4: a provider open that failed after the click already moved. The
    /// window sits on the failed session with an inline failure instead of
    /// another session's transcript: `active` is None while this stands —
    /// the previous view parks in the registry like any switch away —
    /// and the next successful activation clears it. `session_id` is the
    /// clicked session for a reopen, `None` for a fresh open that never
    /// earned an id.
    pub(crate) provider_open_error: Option<LaunchFailure>,
    /// Set by quit (`Harness::shutdown_provider_views`): a provider draft's
    /// open still on the background executor hangs its child up the
    /// moment it answers, instead of handing it to a window that is going
    /// away.
    pub(crate) provider_opens_cancelled: Arc<AtomicBool>,
    /// A one-shot: the next `finish_provider_open` lands its view with the
    /// disabled-provider banner instead of a live child start having
    /// happened (set when opening on a disabled provider, which routes
    /// through the scripted lane).
    pub(crate) pending_disabled_notice: Option<ProviderId>,
    /// Whether the boot session has been attempted: `ensure_boot_session`
    /// opens `--session`/`--send`/`--steps`' first session without waiting
    /// for `session/list`, at most once — a failed attempt must not retry on
    /// every frame, and a later list reply must not open a second session
    /// beside it (see `open_boot_session`'s own guard).
    pub(crate) boot_session_attempted: bool,
    /// Which provider open the app asked for last: every provider open /
    /// reopen / fork bumps this and carries its value into the background
    /// work, and `finish_provider_open` lands only the current one — a
    /// late-finishing earlier open never steals focus or the send.
    /// Scripted (synchronous) opens bump it too, so the count also tells
    /// how many children one action spawned.
    open_epoch: u64,
    /// Switch generation (Y2b2): bumped on every `SwitchProvider` start
    /// and on every cancellation. The switch's open carries the value, and
    /// only an open whose epoch is still current may close what it
    /// replaced and activate as the switch result — anything the person
    /// does meanwhile cancels the switch (bumping this) and the stale open
    /// is discarded.
    switch_epoch: u64,
    /// The one `new_session` a provider switch is allowed to start while a
    /// switch is still pending: `SwitchProvider` closes the old view
    /// synchronously but starts its replacement on a task, so it claims the
    /// next start up front — any other `new` while a switch is in flight is
    /// a duplicate and starts nothing. The claim carries the switch epoch
    /// that stamped it, so a superseded switch's still-queued task cannot
    /// steal its replacement's claim.
    switch_claim: Option<(String, u64)>,
    /// The view a provider switch is replacing; `None` outside a switch.
    replacing: Option<Replacing>,
}

impl OpenState {
    // ------------------------------------------------------ provider opens

    /// A new provider open was asked for: the last ask wins, so this one's
    /// epoch supersedes every open still in flight. Returns the epoch the
    /// open carries into its background work.
    pub(crate) fn next_open(&mut self) -> u64 {
        self.open_epoch = self.open_epoch.wrapping_add(1);
        self.open_epoch
    }

    /// A provider open the session verbs must wait for (a round-trip open,
    /// a reopen, a fork): [`Self::next_open`] with the verbs held until it
    /// lands or fails.
    pub(crate) fn begin_open(&mut self) -> u64 {
        self.switch_pending = true;
        self.next_open()
    }

    /// The epoch of the newest provider open asked for.
    pub(crate) fn open_epoch(&self) -> u64 {
        self.open_epoch
    }

    /// Whether an open stamped `epoch` is still the one asked for last:
    /// anything else is stale and lands on nobody.
    pub(crate) fn is_current_open(&self, epoch: u64) -> bool {
        epoch == self.open_epoch
    }

    /// A session landed in the centre (`activate`): every provider open
    /// still in flight is retired (its late finish lands stale), the verbs
    /// waiting on a switch are released, and an inline open failure stands
    /// down — the landed view replaces it.
    pub(crate) fn land(&mut self) {
        self.next_open();
        self.switch_pending = false;
        self.provider_open_error = None;
    }

    #[cfg(feature = "probe")]
    /// Whether a session the scripted verbs should wait for is still
    /// opening: a switch in flight, or a new session whose view is up while
    /// its open is still on the wire.
    pub(crate) fn session_opening(&self) -> bool {
        self.switch_pending || !self.starts_in_flight.is_empty()
    }

    // ------------------------------------------------- one session per new

    /// Whether a `new` may start a session: while a switch is pending it
    /// owns the next session, so a second `new` (boot racing the head's
    /// `new`, a double ⌘N, a scripted repeat) starts nothing — unless a
    /// provider switch claimed it, which this consumes.
    pub(crate) fn admit_new(&mut self) -> bool {
        !(self.switch_pending && self.switch_claim.take().is_none())
    }

    // ------------------------------------------------- the replace (Y2b)

    /// A draft's composer chip picked another provider: stamp a fresh
    /// switch generation, mark `old` as the view being replaced (with the
    /// drafts-map project that names it and the draft settings `carry`),
    /// hold the verbs, and claim the next start for the replacement.
    /// Returns the switch epoch the replacement's start carries.
    pub(crate) fn begin_switch(&mut self, old: String, carry: SwitchCarry) -> u64 {
        self.switch_epoch = self.switch_epoch.wrapping_add(1);
        let epoch = self.switch_epoch;
        let draft_project = self.draft_project(&old).map(str::to_owned);
        self.replacing = Some(Replacing { old_id: old.clone(), draft_project, epoch, carry: Some(carry) });
        self.switch_pending = true;
        self.switch_claim = Some((old, epoch));
        epoch
    }

    /// Whether a provider switch is replacing a view.
    pub(crate) fn is_replacing(&self) -> bool {
        self.replacing.is_some()
    }

    /// The session id the pending switch is replacing.
    pub(crate) fn replacing_id(&self) -> Option<&str> {
        self.replacing.as_ref().map(|replacing| replacing.old_id.as_str())
    }

    /// Whether the replace marker was stamped by switch `epoch`.
    pub(crate) fn replaces_under(&self, epoch: u64) -> bool {
        self.replacing.as_ref().is_some_and(|replacing| replacing.epoch == epoch)
    }

    /// Whether switch `epoch`'s queued start may still run: it is the
    /// current switch and the marker is its own. A superseding switch, or
    /// any navigation that cancelled this one, moved the epoch on.
    pub(crate) fn switch_is_current(&self, epoch: u64) -> bool {
        epoch == self.switch_epoch && self.replaces_under(epoch)
    }

    /// Take the claim for switch `epoch`'s start: true when the claim was
    /// stamped by that switch. The claim is consumed either way.
    pub(crate) fn take_claim_for(&mut self, epoch: u64) -> bool {
        self.switch_claim.take().is_some_and(|(_, stamped)| stamped == epoch)
    }

    /// The replaced draft's settings, taken by the replacement draft when it
    /// is drawn; `None` once taken or outside a switch.
    pub(crate) fn take_carry(&mut self) -> Option<SwitchCarry> {
        self.replacing.as_mut().and_then(|replacing| replacing.carry.take())
    }

    /// The view a landing may close as the switch's result: the replaced id
    /// while the marker is still the current switch's, else `None` (the
    /// landing parks the old view like any other).
    pub(crate) fn current_replace(&self) -> Option<String> {
        self.replacing.as_ref().filter(|replacing| replacing.epoch == self.switch_epoch).map(|replacing| replacing.old_id.clone())
    }

    /// The replace is over (the replacement landed, or the old view was
    /// already gone): drop the marker.
    pub(crate) fn finish_replace(&mut self) {
        self.replacing = None;
    }

    /// Abort the replace: drop the marker and give the replaced view its
    /// drafts-map name back (unless something else already names it).
    /// Returns the replaced id, whose view the caller unlocks.
    pub(crate) fn abort_replace(&mut self) -> Option<String> {
        let replacing = self.replacing.take()?;
        if let Some(project) = replacing.draft_project {
            if !self.names_draft(&replacing.old_id) {
                self.drafts.insert(project, replacing.old_id.clone());
            }
        }
        Some(replacing.old_id)
    }

    /// A provider open failed: release the waiting verbs and abort any
    /// replace ([`Self::abort_replace`]).
    pub(crate) fn abandon_switch(&mut self) -> Option<String> {
        self.switch_pending = false;
        self.abort_replace()
    }

    /// Cancel a pending provider switch (Y2b2): both epochs move, so its
    /// in-flight open lands stale and its queued start runs nothing, the
    /// claim is dropped, the verbs are released and the replace aborts.
    /// Returns the replaced id, as [`Self::abort_replace`].
    pub(crate) fn cancel_switch(&mut self) -> Option<String> {
        self.switch_epoch = self.switch_epoch.wrapping_add(1);
        self.next_open();
        self.switch_claim = None;
        self.switch_pending = false;
        self.abort_replace()
    }

    /// Cancel a pending provider reopen (Q4b: no replace, the old view still
    /// active): its open lands stale and the verbs are released.
    pub(crate) fn cancel_reopen(&mut self) {
        self.next_open();
        self.switch_pending = false;
    }

    // ------------------------------------------------------------ drafts

    /// Name an unsent session: the project's draft when it has a project,
    /// else one of the project-less unsent sessions.
    pub(crate) fn name_draft(&mut self, project: Option<String>, session_id: &str) {
        match project {
            Some(project) => {
                self.drafts.insert(project, session_id.to_owned());
            }
            None => {
                self.unsent.insert(session_id.to_owned());
            }
        }
    }

    /// Whether some project's draft is `session_id`.
    pub(crate) fn names_draft(&self, session_id: &str) -> bool {
        self.drafts.values().any(|named| named == session_id)
    }

    /// The project whose draft is `session_id`.
    pub(crate) fn draft_project(&self, session_id: &str) -> Option<&str> {
        self.drafts.iter().find(|(_, named)| *named == session_id).map(|(project, _)| project.as_str())
    }

    /// Whether `session_id` is unsent: a project's draft or a project-less
    /// unsent session. Neither has a row yet.
    pub(crate) fn is_unsent(&self, session_id: &str) -> bool {
        self.unsent.contains(session_id) || self.names_draft(session_id)
    }

    /// Forget `session_id` as any project's draft.
    pub(crate) fn forget_draft(&mut self, session_id: &str) {
        self.drafts.retain(|_, named| named != session_id);
    }

    /// A turn started on `session_id`: the session is made real — no draft
    /// and no unsent session any more — and its settles are live activity.
    pub(crate) fn admit_turn(&mut self, session_id: &str) {
        self.forget_draft(session_id);
        self.unsent.remove(session_id);
        self.turns_admitted.insert(session_id.to_owned());
    }

    /// A draft's open ended without a session (it failed, or the draft was
    /// closed): it is no longer opening, nor anyone's draft, nor unsent.
    pub(crate) fn forget_opening(&mut self, session_id: &str) {
        self.starts_in_flight.remove(session_id);
        self.forget_draft(session_id);
        self.unsent.remove(session_id);
    }

    /// A provider draft's provisional id became the id its child minted:
    /// rename it everywhere this state holds it — the drafts and unsent
    /// names, the selection, the replace marker and the switch claim (a
    /// switch away from this draft, made while its child was still
    /// answering, replaces it under its real id).
    pub(crate) fn rekey(&mut self, old: &str, new: &str) {
        for named in self.drafts.values_mut() {
            if named == old {
                *named = new.to_owned();
            }
        }
        if self.unsent.remove(old) {
            self.unsent.insert(new.to_owned());
        }
        if self.pending_id.as_deref() == Some(old) {
            self.pending_id = Some(new.to_owned());
        }
        if let Some(replacing) = self.replacing.as_mut().filter(|replacing| replacing.old_id == old) {
            replacing.old_id = new.to_owned();
        }
        if let Some((id, _)) = self.switch_claim.as_mut().filter(|(id, _)| id == old) {
            *id = new.to_owned();
        }
    }
}

/// Handoff between providers (H2), the application's half of the state:
/// the runs, their owner epoch, the destination open in flight and the
/// confirm dialog. Owned by [`Harness`] as `handoff_state`.
#[derive(Default)]
pub(crate) struct HandoffState {
    /// One handoff run per source session: the machine in
    /// [`crate::handoff`]. The source view mirrors the run's card; this
    /// map is the authority the ack and cancel paths advance.
    pub(crate) runs: HashMap<String, HandoffRun>,
    /// The destination open in flight for a handoff: consumed when the
    /// fresh session lands, then the pack submits onto it.
    pub(crate) pending: Option<PendingHandoff>,
    /// The confirm dialog's frozen facts, held while it is open.
    pub(crate) confirm: Option<HandoffConfirmState>,
    /// Owner-epoch counter for handoffs: every request bumps it and
    /// carries the value, so an event from a superseded epoch is ignored
    /// rather than applied to a newer run.
    epoch: u64,
}

impl HandoffState {
    /// A new handoff request: bump the owner epoch and return it.
    pub(crate) fn next_epoch(&mut self) -> u64 {
        self.epoch = self.epoch.wrapping_add(1);
        self.epoch
    }

    /// Drop the destination open in flight when it belongs to `source`.
    pub(crate) fn drop_pending_for(&mut self, source: &str) {
        if self.pending.as_ref().is_some_and(|pending| pending.source_session == source) {
            self.pending = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn carry() -> SwitchCarry {
        (aui_protocol::PermissionMode::default(), None)
    }

    /// The last ask wins: every open moves the epoch, and only the newest
    /// is current.
    #[test]
    fn the_newest_open_is_the_only_current_one() {
        let mut state = OpenState::default();
        let first = state.begin_open();
        assert!(state.switch_pending, "a round-trip open holds the verbs");
        let second = state.next_open();
        assert!(!state.is_current_open(first), "the earlier open is stale");
        assert!(state.is_current_open(second));
        assert_eq!(state.open_epoch(), second);
    }

    /// A landing retires every open in flight, releases the verbs and stands
    /// the inline failure down.
    #[test]
    fn a_landing_retires_opens_and_releases_the_verbs() {
        let mut state = OpenState::default();
        let epoch = state.begin_open();
        state.starts_in_flight.insert("s-draft".into());
        state.land();
        assert!(!state.is_current_open(epoch), "the open in flight lands stale");
        assert!(!state.switch_pending);
        assert!(state.provider_open_error.is_none());
        assert!(state.session_opening(), "a draft still on the wire is still opening");
        state.starts_in_flight.clear();
        assert!(!state.session_opening());
    }

    /// One session per `new`: a pending switch swallows a second `new`
    /// unless the switch claimed it, and the claim is single-use.
    #[test]
    fn only_a_claimed_new_passes_a_pending_switch() {
        let mut state = OpenState::default();
        assert!(state.admit_new(), "nothing pending: a new starts");
        state.begin_open();
        assert!(!state.admit_new(), "an open in flight owns the next session");
        state.begin_switch("s-old".into(), carry());
        assert!(state.admit_new(), "the switch's claim lets its own start through");
        assert!(!state.admit_new(), "a claim is used once");
    }

    /// A switch marks the replaced view, remembers its draft name and
    /// carry, and only its own epoch may start or close.
    #[test]
    fn a_switch_owns_its_marker_until_superseded() {
        let mut state = OpenState::default();
        state.drafts.insert("p".into(), "s-old".into());
        let epoch = state.begin_switch("s-old".into(), carry());
        assert!(state.is_replacing());
        assert_eq!(state.replacing_id(), Some("s-old"));
        assert!(state.switch_is_current(epoch));
        assert!(state.replaces_under(epoch));
        assert_eq!(state.current_replace().as_deref(), Some("s-old"));
        assert_eq!(state.take_carry(), Some(carry()));
        assert_eq!(state.take_carry(), None, "the carry is taken once");
        // A second switch supersedes the first: the first's queued start and
        // claim are dead.
        let second = state.begin_switch("s-old".into(), carry());
        assert!(!state.switch_is_current(epoch));
        assert!(!state.take_claim_for(epoch), "a superseded claim never passes");
        assert!(state.switch_is_current(second));
        state.finish_replace();
        assert!(!state.is_replacing());
        assert_eq!(state.current_replace(), None);
    }

    /// Cancelling a switch moves both epochs, drops the claim, releases the
    /// verbs and gives the old view its draft name back.
    #[test]
    fn cancelling_a_switch_restores_the_draft_name() {
        let mut state = OpenState::default();
        state.drafts.insert("p".into(), "s-old".into());
        let epoch = state.begin_switch("s-old".into(), carry());
        let open = state.next_open();
        // The switch's open drew the replacement, which `activate` would
        // have named; the old draft's name went with the swap.
        state.forget_draft("s-old");
        assert_eq!(state.cancel_switch().as_deref(), Some("s-old"));
        assert!(!state.is_current_open(open), "the switch's open lands stale");
        assert!(!state.switch_is_current(epoch));
        assert!(!state.switch_pending);
        assert!(!state.take_claim_for(epoch), "the claim is gone");
        assert_eq!(state.drafts.get("p").map(String::as_str), Some("s-old"));
        assert_eq!(state.cancel_switch(), None, "nothing left to cancel");
    }

    /// An abort never names a draft twice.
    #[test]
    fn an_abort_keeps_an_existing_draft_name() {
        let mut state = OpenState::default();
        state.drafts.insert("p".into(), "s-old".into());
        state.begin_switch("s-old".into(), carry());
        assert_eq!(state.abandon_switch().as_deref(), Some("s-old"));
        assert_eq!(state.drafts.len(), 1);
        assert!(!state.switch_pending);
    }

    /// Cancelling a reopen retires its open and releases the verbs.
    #[test]
    fn cancelling_a_reopen_retires_its_open() {
        let mut state = OpenState::default();
        let epoch = state.begin_open();
        state.cancel_reopen();
        assert!(!state.is_current_open(epoch));
        assert!(!state.switch_pending);
    }

    /// Drafts: named per project or project-less, made real by a turn,
    /// forgotten by a failed open.
    #[test]
    fn a_turn_makes_a_draft_real_and_a_failed_open_forgets_it() {
        let mut state = OpenState::default();
        state.name_draft(Some("p".into()), "s-a");
        state.name_draft(None, "s-b");
        assert!(state.is_unsent("s-a") && state.is_unsent("s-b"));
        assert_eq!(state.draft_project("s-a"), Some("p"));
        state.admit_turn("s-a");
        assert!(!state.is_unsent("s-a"));
        assert!(state.turns_admitted.contains("s-a"));
        state.starts_in_flight.insert("s-b".into());
        state.forget_opening("s-b");
        assert!(!state.is_unsent("s-b"));
        assert!(state.starts_in_flight.is_empty());
    }

    /// A provisional id renamed by its landing moves in every holder.
    #[test]
    fn a_rekey_moves_every_holder() {
        let mut state = OpenState::default();
        state.name_draft(Some("p".into()), "req-1");
        state.name_draft(None, "req-2");
        state.pending_id = Some("req-1".into());
        let epoch = state.begin_switch("req-1".into(), carry());
        state.rekey("req-1", "thread-1");
        state.rekey("req-2", "thread-2");
        assert_eq!(state.drafts.get("p").map(String::as_str), Some("thread-1"));
        assert!(state.unsent.contains("thread-2") && !state.unsent.contains("req-2"));
        assert_eq!(state.pending_id.as_deref(), Some("thread-1"));
        assert_eq!(state.replacing_id(), Some("thread-1"));
        assert_eq!(state.switch_claim, Some(("thread-1".to_owned(), epoch)));
    }

    /// Handoff epochs move per request; a pending destination drops only for
    /// its own source.
    #[test]
    fn a_pending_handoff_drops_only_for_its_source() {
        let mut state = HandoffState::default();
        let first = state.next_epoch();
        assert_eq!(state.next_epoch(), first.wrapping_add(1));
        state.pending = Some(PendingHandoff { source_session: "src".into(), epoch: first, prefix: Vec::new() });
        state.drop_pending_for("other");
        assert!(state.pending.is_some());
        state.drop_pending_for("src");
        assert!(state.pending.is_none());
    }
}
