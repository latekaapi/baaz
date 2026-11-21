//! Site permissions (task 4.5 split of `browser/mod.rs`; persisted in
//! docs/82 8.6): the shown tab's asks, the person's answers, and the
//! answers kept for good. "Always allow" and "Always block" store an
//! answer per (origin, kind) in `browser.db` (through [`super::data`]),
//! and every later ask of exactly those kinds on that origin — on any tab,
//! in any run — is answered from the store without asking. Settings ›
//! Browser › Site permissions lists and edits them.

use super::*;

/// One live permission ask as the router reads it: id, origin, kinds.
type LiveAsk = (u64, String, Vec<aui_webview::PermissionKind>);

/// How the person answered an ask on the strip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PermissionAnswer {
    /// Block, this time.
    Block,
    /// Block, and every later ask of these kinds on this site.
    AlwaysBlock,
    /// Allow, this time.
    AllowOnce,
    /// Allow, and every later ask of these kinds on this site.
    AlwaysAllow,
}

impl PermissionAnswer {
    fn allows(self) -> bool {
        matches!(self, PermissionAnswer::AllowOnce | PermissionAnswer::AlwaysAllow)
    }

    /// The answer kept for good, if this one is.
    fn kept(self) -> Option<baaz_browser_core::sites::Answer> {
        match self {
            PermissionAnswer::AlwaysAllow => Some(baaz_browser_core::sites::Answer::Allow),
            PermissionAnswer::AlwaysBlock => Some(baaz_browser_core::sites::Answer::Block),
            PermissionAnswer::Block | PermissionAnswer::AllowOnce => None,
        }
    }
}

impl Harness {

    /// Answer the shown tab's pending permission `id`. "Always" answers
    /// are kept for the (origin, kinds) asked — the origin as the grant
    /// table keys it (scheme + host + port; a host-only report borrows them
    /// from the tab's URL) — and answer the next ask themselves.
    pub(crate) fn answer_shown_permission(&mut self, id: u64, answer: PermissionAnswer, cx: &mut Context<Self>) {
        let tab_url = self.browser.read(cx).browse_centre_shown(cx).and_then(|id| self.browser.read(cx).tabs.get(id).map(|tab| tab.url.clone()));
        let ask = self.browse_centre_state(cx).and_then(|state| {
            state
                .read(cx)
                .permissions()
                .iter()
                .find(|request| request.id == id)
                .map(|request| (request.origin.clone(), request.kinds.clone()))
        });
        if let (Some(kept), Some((origin, kinds)), Some(tab_url)) = (answer.kept(), &ask, &tab_url) {
            let origin = chrome::normalize_origin(origin, tab_url);
            let data = self.browser.read(cx).data.clone();
            data.update(cx, |data, cx| {
                for kind in kinds {
                    data.set_site(&origin, &chrome::permission_kind_key(kind), Some(kept.as_str()), cx);
                }
            });
        }
        if let Some(state) = self.browse_centre_state(cx) {
            let decision = if answer.allows() { PermissionDecision::Allow } else { PermissionDecision::Deny };
            // The page drops the ask and redraws; the strip reads it.
            state.update(cx, |state, cx| state.answer_permission(id, decision, cx));
        }
    }
}

impl BrowserView {

    /// Route permission asks once per frame: an ask whose every kind has a
    /// kept answer on its origin is answered from the store on any tab
    /// (one kept Block blocks it); anything else still asks. Any other ask
    /// on a background tab marks that tab's row (the sidebar's "needs you"
    /// dot) until the tab is shown — in Browse only, where the rows live.
    /// The strip itself reads the shown tab's live asks, so a page
    /// dismissal drops it with no Baaz state to clear.
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
        let mut answered: HashSet<(TabId, u64)> = HashSet::new();
        for (id, asks) in &live {
            let tab_url = self.tabs.get(*id).map(|tab| tab.url.clone()).unwrap_or_default();
            for (ask, origin, kinds) in asks {
                let origin = chrome::normalize_origin(origin, &tab_url);
                let keys: Vec<String> = kinds.iter().map(chrome::permission_kind_key).collect();
                let kept = self.data.read(cx).sites().decide(&origin, &keys);
                if let Some(kept) = kept {
                    let decision = match kept {
                        baaz_browser_core::sites::Answer::Allow => PermissionDecision::Allow,
                        baaz_browser_core::sites::Answer::Block => PermissionDecision::Deny,
                    };
                    if let Some(state) = self.states.get(id).cloned() {
                        state.update(cx, |state, cx| state.answer_permission(*ask, decision, cx));
                    }
                    answered.insert((*id, *ask));
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
            let pending = live
                .iter()
                .find(|(live, _)| *live == id)
                .is_some_and(|(_, asks)| asks.iter().any(|(ask, ..)| !answered.contains(&(id, *ask))));
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
}
