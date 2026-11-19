//! The replace state machine (Y2b): switching a draft's provider keeps
//! the old view until the new lane lands, and a pending switch can be
//! cancelled or aborted; `close_replaced` retires the old view.
//!
//! Part of [`Harness`]; see [`crate::app`] for what the entity owns.

use super::*;

impl Harness {
    /// Cancel a pending provider switch (Y2b2): anything the person does
    /// while a switch is pending — resume/click another session, New
    /// session, project switch, closing the view/window, starting a
    /// handoff — ends the switch instead of racing it. Both epochs move so
    /// the switch's in-flight open lands stale (its child is shut down,
    /// nothing is parked or persisted), the claim is dropped, the old view
    /// is unlocked with its chip and draft name restored, and waiting verbs
    /// are released. The caller's action then proceeds normally. Returns
    /// whether a switch was pending.
    ///
    /// Q4b: a pending provider *reopen* (a sidebar click whose child is
    /// still resuming — no replace marked, the old view still active)
    /// cancels the same way. The epoch moves so the late landing is
    /// discarded, the loading state drops, and the old view's pane is
    /// restored — so clicking the still-active session just stays there
    /// (the caller returns without reopening it) and clicking anywhere
    /// else proceeds with the stale open invalidated.
    pub(crate) fn cancel_pending_switch(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.open_state.is_replacing() {
            if self.pending_switch_dest(cx).is_none() {
                return false;
            }
            self.open_state.cancel_reopen();
            // The selection and the pane follow the restored view: the
            // click's destination never opened, so nothing may keep naming
            // it.
            if let Some(view) = self.active.clone() {
                let id = view.read(cx).rt(cx).session_id.clone();
                self.open_state.pending_id = Some(id.clone());
                self.restore_right_for_session(&id, cx);
                // Q4c: the click consumed the composer's focus arm — re-arm
                // it for the restored view, like `activate` arms it for a
                // landing view, so typing resumes where it was.
                self.arm_composer_focus(cx);
            } else {
                self.open_state.pending_id = None;
            }
            cx.notify();
            return true;
        }
        let replaced = self.open_state.cancel_switch();
        self.restore_replaced(replaced, cx);
        true
    }

    /// A provider open failed: release the verbs waiting on it and abort
    /// an in-place provider switch, if any (Y2b): keep the old view, unlock
    /// its composer, restore the chip and the drafts name
    /// ([`OpenState::abandon_switch`]). The caller still dialogs the reason.
    pub(super) fn abandon_switch(&mut self, cx: &mut Context<Self>) {
        let replaced = self.open_state.abandon_switch();
        self.restore_replaced(replaced, cx);
    }

    /// The window half of an aborted replace: the replaced view (`old_id`,
    /// from [`OpenState::abort_replace`]), when still active, gets its
    /// composer unlocked and its provider back as the new-session pick.
    fn restore_replaced(&mut self, old_id: Option<String>, cx: &mut Context<Self>) {
        let Some(old_id) = old_id else { return };
        if let Some(old) = self.active.clone().filter(|v| v.read(cx).rt(cx).session_id == old_id) {
            let provider = old.read(cx).rt(cx).provider_kind();
            self.provider_model.update(cx, |model, _| model.choose_new_provider(provider));
            old.update(cx, |v, cx| {
                v.runtime().clone().update(cx, |rt, cx| rt.set_input_locked(false, cx));
                v.runtime().clone().update(cx, |rt, cx| rt.set_switching_to(None, cx));
            });
        }
        cx.notify();
    }

    /// Close the view a landed switch replaced, in the same update that
    /// installs the replacement (Y2b): the old view never parks, and no
    /// frame renders without a session. Carries an unsent draft across.
    /// Returns true when the caller must skip `park_active` (the old view
    /// was taken instead of parked).
    pub(super) fn close_replaced(
        &mut self,
        old_id: &str,
        new_view: &Entity<SessionView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        // Same entity reopened (draft reuse): nothing to close.
        if self.active.as_ref().is_some_and(|v| *v == *new_view) {
            self.open_state.finish_replace();
            return false;
        }
        let old_view = self.active.take().filter(|v| v.read(cx).rt(cx).session_id == old_id);
        let Some(old_view) = old_view else {
            self.open_state.finish_replace();
            return false;
        };
        let new_id = new_view.read(cx).rt(cx).session_id.clone();
        // Z2: a replacement swaps session ids, so the pane state moves with
        // it — the new id inherits what the old one left, unless it already
        // holds its own. The restore at the end of `activate` applies it.
        if new_id != old_id {
            let carried = self.session_list.read(cx).overrides.get(old_id).and_then(|meta| meta.right.clone());
            if let Some(state) = carried {
                self.session_list.update(cx, |list, _| {
                    let entry = list.overrides.entry(new_id.clone()).or_default();
                    if entry.right.is_none() {
                        entry.right = Some(state);
                        crate::sessions::write(&list.overrides);
                    }
                });
            }
        }
        // Carry the unsent draft across when the replacement holds nothing.
        let moving = old_view.update(cx, |v, vc| {
            if v.draft_content_empty(vc) { None } else { Some(v.take_draft(window, vc)) }
        });
        if let Some(draft) = moving {
            new_view.update(cx, |v, vc| {
                if v.draft_content_empty(vc) {
                    v.put_draft(draft, window, vc);
                }
            });
        }
        old_view.read(cx).runtime().clone().update(cx, |rt, _| rt.shutdown_lane());
        if new_id != old_id && old_view.read(cx).rt(cx).is_opening_lane() {
            self.forget_provisional(old_id, cx);
        }
        // A stale parked copy under the same id never survives the swap —
        // unless it IS the replacement (shared scripted id in tests), which
        // is never parked, so this only drops the old duplicate.
        let keep = [new_view.read(cx).runtime().clone(), old_view.read(cx).runtime().clone()];
        if let Some((cached, _)) = self.registry.take(old_id) {
            if !keep.contains(&cached) {
                cached.update(cx, |rt, _| rt.shutdown_lane());
            }
        }
        // An unsent provider draft leaves no transcript anywhere, so its
        // record leaves with its view — except when the replacement shares
        // the id (the scripted double mints one fixed id), where the record
        // now belongs to the new view.
        if new_id != old_id {
            let unsent = self
                .session_list.read(cx).provider_sessions
                .get(old_id)
                .is_some_and(|record| record.turns == 0 && !self.first_prompt_sent(old_id, cx));
            if unsent {
                self.registry.forget_where(|id| id == old_id);
                self.session_list.update(cx, |list, _| list.sessions.retain(|entry| entry.id != old_id));
                self.session_list.update(cx, |list, _| list.overrides.remove(old_id));
                if self.session_list.update(cx, |list, _| crate::provider_sessions::remove(&mut list.provider_sessions, old_id)) {
                    self.persist_provider_sessions(cx);
                }
                crate::sessions::write(&self.session_list.read(cx).overrides);
                self.rejoin(cx);
                self.invalidate_list(cx);
            }
        }
        self.open_state.finish_replace();
        true
    }
}
