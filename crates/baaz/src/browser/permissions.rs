//! Site permissions (task 4.5 split of `browser/mod.rs`): the shown tab's
//! asks, the answers and the "while visiting" grants.

use super::*;

/// One live permission ask as the router reads it: id, origin, kinds.
type LiveAsk = (u64, String, Vec<aui_webview::PermissionKind>);

impl Harness {

    /// Answer the shown tab's pending permission `id`: Block denies, Allow
    /// once and Allow while visiting grant — the latter remembers the
    /// (origin, kinds) for this run and auto-answers repeats of exactly
    /// those kinds. The grant keys on the full origin (scheme + host +
    /// port): a host-only report borrows them from the tab's URL.
    pub(crate) fn answer_shown_permission(&mut self, id: u64, allow: bool, remember_visit: bool, cx: &mut Context<Self>) {
        let tab_url = self.browser.read(cx).browse_centre_shown(cx).and_then(|id| self.browser.read(cx).tabs.get(id).map(|tab| tab.url.clone()));
        let ask = self.browse_centre_state(cx).and_then(|state| {
            state
                .read(cx)
                .permissions()
                .iter()
                .find(|request| request.id == id)
                .map(|request| (request.origin.clone(), request.kinds.clone()))
        });
        if remember_visit && allow {
            if let (Some((origin, kinds)), Some(tab_url)) = (&ask, &tab_url) {
                self.browser.update(cx, |browser, _| browser.visit_allows.remember(&chrome::normalize_origin(origin, tab_url), kinds));
            }
        }
        if let Some(state) = self.browse_centre_state(cx) {
            let decision = if allow { PermissionDecision::Allow } else { PermissionDecision::Deny };
            state.update(cx, |state, cx| state.answer_permission(id, decision, cx));
        }
        cx.notify();
    }
}

impl BrowserView {

    /// Route permission asks once per frame: origins trusted "while
    /// visiting" auto-answer on any tab, but only for the kinds granted —
    /// anything else still asks. Any other ask on a background tab marks
    /// that tab's row (the sidebar's "needs you" dot) until the tab is
    /// shown — in Browse only, where the rows live. The strip itself reads
    /// the shown tab's live asks, so a page dismissal drops it with no
    /// Baaz state to clear.
    pub(super) fn route_browse_permissions(&mut self, shown: Option<TabId>, mark_background: bool, cx: &mut gpui::Context<Self>) {
        let live: Vec<(TabId, Vec<LiveAsk>)> = self
            .states
            .iter()
            .map(|(id, state)| {
                let asks = state
                    .read(cx)
                    .permissions()
                    .iter()
                    .map(|request| (request.id, request.origin.clone(), request.kinds.clone()))
                    .collect();
                (*id, asks)
            })
            .collect();
        let mut dirty = false;
        for (id, asks) in &live {
            let tab_url = self.tabs.get(*id).map(|tab| tab.url.clone()).unwrap_or_default();
            for (ask, origin, kinds) in asks {
                let origin = chrome::normalize_origin(origin, &tab_url);
                if chrome::auto_answer(&origin, kinds, &self.visit_allows).is_some() {
                    if let Some(state) = self.states.get(id).cloned() {
                        state.update(cx, |state, cx| state.answer_permission(*ask, PermissionDecision::Allow, cx));
                    }
                } else if mark_background && Some(*id) != shown {
                    if let Some(tab) = self.tabs.tabs.iter_mut().find(|tab| tab.id == *id) {
                        if !tab.needs_attention {
                            tab.needs_attention = true;
                            dirty = true;
                        }
                    }
                }
            }
        }
        if let Some(id) = shown {
            let pending = live.iter().find(|(live, _)| *live == id).is_some_and(|(_, asks)| !asks.is_empty());
            if !pending {
                if let Some(tab) = self.tabs.tabs.iter_mut().find(|tab| tab.id == id) {
                    if tab.needs_attention {
                        tab.needs_attention = false;
                        dirty = true;
                    }
                }
            }
        }
        if dirty {
            self.tabs_changed(cx);
        }
    }


    /// Revoke "while visiting" grants the tabs left behind: a tab that
    /// navigated to another origin — or closed — drops that origin's
    /// grants, unless another live tab still shows it. Pure model work,
    /// once per frame.
    pub(super) fn revoke_stale_visit_grants(&mut self) {
        let live: HashMap<TabId, String> = self
            .tabs
            .tabs
            .iter()
            .map(|tab| (tab.id, chrome::normalize_origin(&tab.url, &tab.url)))
            .collect();
        let live_origins: HashSet<String> = live.values().cloned().collect();
        let mut stale = Vec::new();
        for (id, seen) in &self.permission_origins {
            if live.get(id) != Some(seen) {
                stale.push(seen.clone());
            }
        }
        for origin in stale {
            if !live_origins.contains(&origin) {
                self.visit_allows.revoke_origin(&origin);
            }
        }
        self.permission_origins = live;
    }
}
