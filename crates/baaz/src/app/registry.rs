//! Every live session view the window holds, in one place (docs/82 1.6,
//! audit A R1).
//!
//! A [`SessionView`] lives in one of three places: the centre
//! ([`Harness::active`](super::Harness), which stays a field because almost
//! every handler reads it), the parked MRU behind it, or the browser chat on
//! the agent line. Before this module each place was a separate field and
//! every question about "all of them" — find a view by id, is any turn
//! running, shut every lane down, subscribe to every view — was answered by
//! a hand-written loop that had to remember all three. Several forgot the
//! browser chat or the parked views (docs/81 A F1, F5, F6, F7, F17).
//!
//! The registry owns the parked list, the browser chat and the one event
//! subscription per live view. Everything that enumerates views goes
//! through it, and the centre view is passed in where a question covers it,
//! so no enumeration can leave a holder out.

use std::collections::HashSet;

use gpui::{App, Context, Entity, Subscription};

use super::Harness;
use crate::session::SessionView;

/// The parked views and the browser chat, plus one subscription per live
/// view. See the module doc.
#[derive(Default)]
pub(crate) struct SessionRegistry {
    /// Parked views, most recently used first, keyed by session id.
    parked: Vec<(String, Entity<SessionView>)>,
    /// The chat on the Browse agent line, when there is one.
    browser_chat: Option<Entity<SessionView>>,
    /// One subscription per live view; rebuilt by [`Self::resubscribe`].
    subscriptions: Vec<Subscription>,
}

impl SessionRegistry {
    // ------------------------------------------------------------ parked

    /// Park `view` as `id`, most recent first; an older slot for `id` goes.
    pub(crate) fn park(&mut self, id: String, view: Entity<SessionView>) {
        self.parked.retain(|(parked, _)| *parked != id);
        self.parked.insert(0, (id, view));
    }

    /// Take the parked view for `id` out of the MRU.
    pub(crate) fn take(&mut self, id: &str) -> Option<Entity<SessionView>> {
        let ix = self.parked.iter().position(|(parked, _)| parked == id)?;
        Some(self.parked.remove(ix).1)
    }

    /// The parked view for `id`, left where it is.
    pub(crate) fn parked(&self, id: &str) -> Option<Entity<SessionView>> {
        self.parked.iter().find(|(parked, _)| parked == id).map(|(_, view)| view.clone())
    }

    /// Whether `id` has a parked view.
    pub(crate) fn is_parked(&self, id: &str) -> bool {
        self.parked.iter().any(|(parked, _)| parked == id)
    }

    /// Whether `view` is parked, under any id.
    #[cfg(test)]
    pub(crate) fn is_view_parked(&self, view: &Entity<SessionView>) -> bool {
        self.parked.iter().any(|(_, parked)| parked == view)
    }

    /// The parked ids, most recent first.
    pub(crate) fn parked_ids(&self) -> Vec<String> {
        self.parked.iter().map(|(id, _)| id.clone()).collect()
    }

    /// Drop every parked view whose id `drop` names.
    pub(crate) fn forget_where(&mut self, drop: impl Fn(&str) -> bool) {
        self.parked.retain(|(id, _)| !drop(id));
    }

    /// Drop `view` from the MRU, under whatever id it is parked.
    pub(crate) fn forget_view(&mut self, view: &Entity<SessionView>) {
        self.parked.retain(|(_, parked)| parked != view);
    }

    /// Move the parked slot of a provisional id onto the id its child
    /// minted (docs/81 A F4).
    pub(crate) fn rekey(&mut self, old: &str, new: &str) {
        for (id, _) in &mut self.parked {
            if id == old {
                *id = new.to_owned();
            }
        }
    }

    /// Cap the idle parked views at `limit`, most recent kept. A view named
    /// in `keep` (drafts) or with work in hand
    /// ([`SessionView::must_stay_alive`]) is never evicted and does not
    /// count toward the cap (docs/81 A F1). Returns what was evicted.
    pub(crate) fn evict_idle(
        &mut self,
        keep: &HashSet<String>,
        limit: usize,
        cx: &App,
    ) -> Vec<(String, Entity<SessionView>)> {
        let mut kept = keep.clone();
        kept.extend(
            self.parked.iter().filter(|(_, view)| view.read(cx).must_stay_alive()).map(|(id, _)| id.clone()),
        );
        let surviving = super::lifecycle::evict_parked(&self.parked_ids(), &kept, limit);
        let (stay, evicted): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.parked).into_iter().partition(|(id, _)| surviving.contains(id));
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

    // ------------------------------------------------------- every view

    /// Every live view once: `active` (the centre), every parked view, the
    /// browser chat.
    pub(crate) fn live(&self, active: Option<&Entity<SessionView>>) -> Vec<Entity<SessionView>> {
        let mut views: Vec<Entity<SessionView>> = Vec::new();
        let all = active.into_iter().chain(self.parked.iter().map(|(_, view)| view)).chain(self.browser_chat.iter());
        for view in all {
            if !views.contains(view) {
                views.push(view.clone());
            }
        }
        views
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
            .find(|view| view.read(cx).session_id == session_id)
            .cloned()
    }

    /// The live views with a turn in flight ([`SessionView::is_sending`]).
    pub(crate) fn iter_running(&self, active: Option<&Entity<SessionView>>, cx: &App) -> Vec<Entity<SessionView>> {
        self.live(active).into_iter().filter(|view| view.read(cx).is_sending()).collect()
    }

    /// One subscription per live view, `active` first; the old ones drop,
    /// so a view that left is never heard and none is heard twice.
    pub(crate) fn resubscribe(&mut self, active: Option<&Entity<SessionView>>, cx: &mut Context<Harness>) {
        self.subscriptions.clear();
        for view in self.live(active) {
            self.subscriptions.push(cx.subscribe(&view, |this, view, event, cx| this.on_session_event(view, event, cx)));
        }
    }

    /// Shut every live view's lane down (quit).
    pub(crate) fn shutdown_all(&self, active: Option<&Entity<SessionView>>, cx: &mut App) {
        for view in self.live(active) {
            view.update(cx, |view, _| view.shutdown_lane());
        }
    }

    /// How many view subscriptions are live.
    #[cfg(test)]
    pub(crate) fn subscription_count(&self) -> usize {
        self.subscriptions.len()
    }

    /// How many views are parked.
    #[cfg(test)]
    pub(crate) fn parked_len(&self) -> usize {
        self.parked.len()
    }

    /// Empty the MRU, handing the views back (tests that drop them).
    #[cfg(test)]
    pub(crate) fn drain_parked(&mut self) -> Vec<Entity<SessionView>> {
        self.parked.drain(..).map(|(_, view)| view).collect()
    }
}
