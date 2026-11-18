//! The agent's `browser_tabs` tool on the window (task 4.5 split of
//! `browser/mod.rs`).

use super::*;

impl Harness {
    /// Answers the agent's `browser_tabs` (docs/27 §6.1): only ever this session's
    /// tabs. Opening a tab builds its page hidden, so a background session can
    /// work without moving anything the person sees.
    pub(crate) fn agent_tabs_call(&mut self, call: crate::terminal::service::TabsCall, cx: &mut Context<Self>) {
        use serde_json::{json, Value};
        self.ensure_tabs_loaded(cx);
        let session = call.session.clone();
        let owner = Owner::Session(session.clone());
        let tab_id = call.params.get("tab").and_then(Value::as_u64).map(TabId);
        let mine = |this: &Self, id: TabId| this.browser.read(cx).tabs.get(id).is_some_and(|tab| tab.owner == owner);
        let outcome: Result<Value, String> = match call.params.get("op").and_then(Value::as_str).unwrap_or("list") {
            "list" => {
                let current = self.browser.read(cx).tabs.active_of(&owner, None);
                let tabs: Vec<Value> = self
                    .browser
                    .read(cx)
                    .tabs
                    .tabs
                    .iter()
                    .filter(|tab| tab.owner == owner)
                    .map(|tab| json!({"tab": tab.id.0, "url": tab.url, "title": tab.title, "current": Some(tab.id) == current}))
                    .collect();
                Ok(json!({ "tabs": tabs }))
            }
            "open" => {
                let url = call.params.get("url").and_then(Value::as_str).unwrap_or(BLANK).to_owned();
                if !crate::terminal::service::browser_url_allowed(&url) {
                    Err(format!("refused URL: {url} (tabs open http, https, file, or about:blank)"))
                } else if let Some(main) = self.window_sync.handle {
                    let space = self.browser.read(cx).browse_space();
                    let id = self.browser.update(cx, |browser, _| browser.tabs.open(space, owner.clone(), &url, None));
                    self.browser.update(cx, |browser, cx| browser.tabs_changed(cx));
                    let this = cx.entity();
                    let key = session.clone();
                    cx.defer(move |cx| {
                        let _ = main.update(cx, move |_, window, cx| {
                            this.update(cx, |this, cx| {
                                let current = this.browser.read(cx).agent_tabs.get(&key).copied();
                                this.ensure_tab_page(&key, id, window, cx);
                                // A background tab: the session's current page stays
                                // what it was.
                                if let Some(state) = current.and_then(|tab| this.browser.read(cx).states.get(&tab).cloned()) {
                                    this.register_tab_page(&key, current.unwrap_or(id), &state, cx);
                                }
                                cx.notify();
                            });
                        });
                    });
                    Ok(json!({"tab": id.0, "url": url}))
                } else {
                    Err(String::from("the window is not ready"))
                }
            }
            "close" => match tab_id.filter(|id| mine(self, *id)) {
                Some(id) => {
                    self.terminal_service.unregister_tab_page(id.0);
                    if let Some(main) = self.window_sync.handle {
                        let this = cx.entity();
                        let key = session.clone();
                        cx.defer(move |cx| {
                            let _ = main.update(cx, move |_, window, cx| {
                                this.update(cx, |this, cx| this.close_tab(&key, id, window, cx));
                            });
                        });
                    }
                    Ok(json!({"closed": id.0}))
                }
                None => Err(String::from("no such tab in this session: browser_tabs lists them")),
            },
            "activate" => match tab_id.filter(|id| mine(self, *id)) {
                Some(id) => {
                    self.browser.update(cx, |browser, _| browser.tabs.activate(id));
                    if let Some(state) = self.browser.read(cx).states.get(&id).cloned() {
                        self.register_tab_page(&session, id, &state, cx);
                    }
                    self.browser.update(cx, |browser, cx| browser.tabs_changed(cx));
                    Ok(json!({"current": id.0}))
                }
                None => Err(String::from("no such tab in this session: browser_tabs lists them")),
            },
            "name" => match tab_id.filter(|id| mine(self, *id)) {
                Some(id) => {
                    let name = call.params.get("name").and_then(Value::as_str).unwrap_or("").to_owned();
                    let url = self.browser.read(cx).tabs.get(id).map(|tab| tab.url.clone()).unwrap_or_default();
                    self.browser.update(cx, |browser, _| browser.tabs.set_url_title(id, &url, &name));
                    self.browser.update(cx, |browser, cx| browser.tabs_changed(cx));
                    Ok(json!({"named": id.0}))
                }
                None => Err(String::from("no such tab in this session: browser_tabs lists them")),
            },
            other => Err(format!("unknown op {other}: list, open, close, activate or name")),
        };
        crate::terminal::service::answer_tabs(&call, outcome);
    }
}
