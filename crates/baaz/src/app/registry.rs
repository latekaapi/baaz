//! Every live session the window holds, in one place (docs/82 1.6 and 4.8,
//! audit A R1, B S-9).
//!
//! A session is a [`SessionRuntime`] — the lane, the store, the turn state —
//! and, while it is on screen or parked with its drawing kept, a
//! [`SessionView`] over it. A session lives in one of three places: the
//! centre ([`Harness::active`](super::Harness), a view, which stays a field
//! because almost every handler reads it), the parked MRU behind it, or the
//! browser chat on the agent line. Before this module each place was a
//! separate field and every question about "all of them" — find a session
//! by id, is any turn running, shut every lane down, subscribe to every
//! session — was answered by a hand-written loop that had to remember all
//! three. Several forgot the browser chat or the parked views (docs/81 A
//! F1, F5, F6, F7, F17).
//!
//! The registry owns the parked list (each entry a runtime with its view,
//! or with none once the view was evicted), the browser chat and the
//! subscriptions: one per runtime (what the session tells the root) and one
//! per view (what the person asked of the window). Everything that
//! enumerates sessions goes through it, and the centre view is passed in
//! where a question covers it, so no enumeration can leave a holder out.
//!
//! Views are drawing state and are cheap to build again over a runtime, so
//! the parked cap limits views; a runtime goes only when it is idle
//! ([`SessionRuntime::must_stay_alive`]). Eviction can never shut a turn
//! down mid-flight (docs/81 A F1): the runtime keeps running with no view,
//! settles its row and lands its ledger row, and the next open builds a
//! view over it.

use std::collections::HashSet;

use gpui::{App, Context, Entity, Subscription};

use super::Harness;
use crate::session::{SessionEvent, SessionRuntime, SessionView};

/// One parked session: its runtime, and its view while the cap keeps it.
struct Parked {
    /// The session id it is parked under.
    id: String,
    /// The session itself.
    runtime: Entity<SessionRuntime>,
    /// Its drawing, scroll position and draft; `None` once evicted.
    view: Option<Entity<SessionView>>,
}

/// The parked sessions and the browser chat, plus the subscriptions. See
/// the module doc.
#[derive(Default)]
pub(crate) struct SessionRegistry {
    /// Parked sessions, most recently used first, keyed by session id.
    parked: Vec<Parked>,
    /// The chat on the Browse agent line, when there is one.
    browser_chat: Option<Entity<SessionView>>,
    /// One subscription per live runtime and one per live view; rebuilt by
    /// [`Self::resubscribe`].
    subscriptions: Vec<Subscription>,
}

impl SessionRegistry {
    // ------------------------------------------------------------ parked

    /// Park `view` (and its runtime) as `id`, most recent first; an older
    /// slot for `id` goes.
    pub(crate) fn park(&mut self, id: String, view: Entity<SessionView>, cx: &App) {
        let runtime = view.read(cx).runtime().clone();
        self.parked.retain(|parked| parked.id != id);
        self.parked.insert(0, Parked { id, runtime, view: Some(view) });
    }

    /// Take the parked session for `id` out of the MRU: its runtime, and
    /// its view when the cap kept one.
    pub(crate) fn take(&mut self, id: &str) -> Option<(Entity<SessionRuntime>, Option<Entity<SessionView>>)> {
        let ix = self.parked.iter().position(|parked| parked.id == id)?;
        let parked = self.parked.remove(ix);
        Some((parked.runtime, parked.view))
    }

    /// The parked view for `id`, left where it is.
    pub(crate) fn parked(&self, id: &str) -> Option<Entity<SessionView>> {
        self.parked.iter().find(|parked| parked.id == id).and_then(|parked| parked.view.clone())
    }

    #[cfg(feature = "probe")]
    /// Whether `id` is parked, with or without its view.
    pub(crate) fn is_parked(&self, id: &str) -> bool {
        self.parked.iter().any(|parked| parked.id == id)
    }

    /// Whether `view` is parked, under any id.
    #[cfg(all(test, feature = "probe"))]
    pub(crate) fn is_view_parked(&self, view: &Entity<SessionView>) -> bool {
        self.parked.iter().any(|parked| parked.view.as_ref() == Some(view))
    }

    /// The parked ids, most recent first.
    pub(crate) fn parked_ids(&self) -> Vec<String> {
        self.parked.iter().map(|parked| parked.id.clone()).collect()
    }

    /// Drop every parked session whose id `drop` names.
    pub(crate) fn forget_where(&mut self, drop: impl Fn(&str) -> bool) {
        self.parked.retain(|parked| !drop(&parked.id));
    }

    /// Drop `view`'s session from the MRU, under whatever id it is parked.
    pub(crate) fn forget_view(&mut self, view: &Entity<SessionView>) {
        self.parked.retain(|parked| parked.view.as_ref() != Some(view));
    }

    /// Move the parked slot of a provisional id onto the id its child
    /// minted (docs/81 A F4).
    pub(crate) fn rekey(&mut self, old: &str, new: &str) {
        for parked in &mut self.parked {
            if parked.id == old {
                parked.id = new.to_owned();
            }
        }
    }

    /// Cap the parked views at `limit`, most recent kept. A session named
    /// in `keep` (drafts: the view holds the typed words) keeps its view
    /// and does not count toward the cap, and neither does one already
    /// without a view. Past the cap, a session with work in hand
    /// ([`SessionRuntime::must_stay_alive`]: a turn running, an approval or
    /// question waiting, a start on the wire) loses only its view — its
    /// runtime runs on — and an idle one goes whole (docs/81 A F1). Returns
    /// the runtimes that went.
    pub(crate) fn evict_idle(
        &mut self,
        keep: &HashSet<String>,
        limit: usize,
        cx: &App,
    ) -> Vec<(String, Entity<SessionRuntime>)> {
        // A runtime that already lost its view takes no view slot; it stays
        // only while it has work in hand, and goes whole once it is idle —
        // else a child that finished its turn past the cap would live on
        // until quit.
        let mut kept = keep.clone();
        kept.extend(self.parked.iter().filter(|parked| parked.view.is_none()).map(|parked| parked.id.clone()));
        let surviving = super::sessions::evict_parked(&self.parked_ids(), &kept, limit);
        let mut evicted = Vec::new();
        let mut stay = Vec::with_capacity(self.parked.len());
        for mut parked in std::mem::take(&mut self.parked) {
            let viewless_idle = parked.view.is_none()
                && !keep.contains(&parked.id)
                && !parked.runtime.read(cx).must_stay_alive();
            if viewless_idle {
                evicted.push((parked.id, parked.runtime));
            } else if surviving.contains(&parked.id) {
                stay.push(parked);
            } else if parked.runtime.read(cx).must_stay_alive() {
                parked.view = None;
                stay.push(parked);
            } else {
                evicted.push((parked.id, parked.runtime));
            }
        }
        self.parked = stay;
        evicted
    }

    // ------------------------------------------------------ browser chat

    /// The chat on the agent line.
    pub(crate) fn browser_chat(&self) -> Option<&Entity<SessionView>> {
        self.browser_chat.as_ref()
    }

    /// Put `view` on the agent line; returns the chat it replaced.
    pub(crate) fn replace_browser_chat(&mut self, view: Entity<SessionView>) -> Option<Entity<SessionView>> {
        self.browser_chat.replace(view)
    }

    // ---------------------------------------------------- every session

    /// Every live view once: `active` (the centre), every parked view, the
    /// browser chat.
    pub(crate) fn live(&self, active: Option<&Entity<SessionView>>) -> Vec<Entity<SessionView>> {
        let mut views: Vec<Entity<SessionView>> = Vec::new();
        let all = active
            .into_iter()
            .chain(self.parked.iter().filter_map(|parked| parked.view.as_ref()))
            .chain(self.browser_chat.iter());
        for view in all {
            if !views.contains(view) {
                views.push(view.clone());
            }
        }
        views
    }

    /// Every live runtime once: the centre's, every parked one (with a view
    /// or without), the browser chat's.
    pub(crate) fn runtimes(&self, active: Option<&Entity<SessionView>>, cx: &App) -> Vec<Entity<SessionRuntime>> {
        let mut runtimes: Vec<Entity<SessionRuntime>> = Vec::new();
        let all = active
            .map(|view| view.read(cx).runtime().clone())
            .into_iter()
            .chain(self.parked.iter().map(|parked| parked.runtime.clone()))
            .chain(self.browser_chat.iter().map(|view| view.read(cx).runtime().clone()));
        for runtime in all {
            if !runtimes.contains(&runtime) {
                runtimes.push(runtime);
            }
        }
        runtimes
    }

    /// The live view for `session_id`, wherever it is held.
    pub(crate) fn find(
        &self,
        active: Option<&Entity<SessionView>>,
        session_id: &str,
        cx: &App,
    ) -> Option<Entity<SessionView>> {
        if let Some(view) = self.parked(session_id) {
            return Some(view);
        }
        active
            .into_iter()
            .chain(self.browser_chat.iter())
            .find(|view| view.read(cx).rt(cx).session_id == session_id)
            .cloned()
    }

    /// The live runtime for `session_id`, wherever it is held — a parked
    /// one with no view included.
    pub(crate) fn find_runtime(
        &self,
        active: Option<&Entity<SessionView>>,
        session_id: &str,
        cx: &App,
    ) -> Option<Entity<SessionRuntime>> {
        self.runtimes(active, cx).into_iter().find(|runtime| runtime.read(cx).session_id == session_id)
    }

    /// The live runtimes with a turn in flight
    /// ([`SessionRuntime::is_sending`]), drawn or not.
    pub(crate) fn iter_running(&self, active: Option<&Entity<SessionView>>, cx: &App) -> Vec<Entity<SessionRuntime>> {
        self.runtimes(active, cx).into_iter().filter(|runtime| runtime.read(cx).is_sending()).collect()
    }

    /// One subscription per live runtime and one per live view, `active`
    /// first; the old ones drop, so a session that left is never heard and
    /// none is heard twice. A runtime with no view is heard all the same:
    /// its turn settles its row (docs/81 A F1).
    pub(crate) fn resubscribe(&mut self, active: Option<&Entity<SessionView>>, cx: &mut Context<Harness>) {
        self.subscriptions.clear();
        for runtime in self.runtimes(active, cx) {
            self.subscriptions.push(cx.subscribe(&runtime, |this, runtime, event: &SessionEvent, cx| {
                this.on_session_event(runtime, event, cx);
                this.check_attention(cx);
            }));
        }
        for view in self.live(active) {
            self.subscriptions.push(cx.subscribe(&view, |this, view, event: &SessionEvent, cx| {
                let runtime = view.read(cx).runtime().clone();
                this.on_session_event(runtime, event, cx);
                this.check_attention(cx);
            }));
        }
    }

    /// Shut every live runtime's lane down (quit), drawn or not, waiting
    /// for each child to hang up.
    pub(crate) fn shutdown_all(&self, active: Option<&Entity<SessionView>>, cx: &mut App) {
        for runtime in self.runtimes(active, cx) {
            runtime.update(cx, |runtime, _| runtime.shutdown_lane_and_wait());
        }
    }

    /// How many subscriptions are live (a runtime's and a view's each).
    #[cfg(all(test, feature = "probe"))]
    pub(crate) fn subscription_count(&self) -> usize {
        self.subscriptions.len()
    }

    /// How many sessions are parked.
    #[cfg(all(test, feature = "probe"))]
    pub(crate) fn parked_len(&self) -> usize {
        self.parked.len()
    }

    /// Empty the MRU, handing the views back (tests that drop them).
    #[cfg(all(test, feature = "probe"))]
    pub(crate) fn drain_parked(&mut self) -> Vec<Entity<SessionView>> {
        self.parked.drain(..).filter_map(|parked| parked.view).collect()
    }
}
