//! Browser tabs: the pure tab model (B2a-H1). No gpui types here.
//!
//! A tab belongs to a **space** (a project id, `None` with no project) and
//! to an **owner**: the person, or one session. In Sessions mode the right
//! pane shows the active session's own tabs; Browse mode (next tasks) will
//! show the project's person tabs plus "From sessions".
//!
//! [`Tabs`] is ordered (the strip order), tracks each owner's active tab,
//! and keeps the last 20 closed tabs for reopen. It serialises to
//! `$BAAZ_STATE_DIR/Browser/tabs.json` through [`crate::store`]; on load,
//! [`Tabs::migrate_from_overrides`] gives every session with a stored
//! `RightState.browser_url` and no tabs yet one session tab with that URL,
//! so older builds' restore survives the upgrade — and the session's active
//! tab keeps writing `browser_url`, so older builds still restore too.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// A tab's identity: monotonic from [`Tabs::next_id`], never reused, stable
/// across save and load.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct TabId(pub u64);

/// Who a tab belongs to: the person, or one session by id.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) enum Owner {
    /// The person's own tabs, shown per project in Browse mode.
    Person,
    /// One agent/coding session's tabs, shown in Sessions mode.
    Session(String),
}

/// The map key for [`Tabs::active`]: `"person"` or `"session:<id>"`.
/// A plain string (not the enum) so the map serialises as JSON.
pub(crate) fn owner_key(owner: &Owner) -> String {
    match owner {
        Owner::Person => "person".to_owned(),
        Owner::Session(id) => format!("session:{id}"),
    }
}

/// One browser tab: where it lives, who owns it, and what it shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Tab {
    /// Stable identity, from [`Tabs::next_id`].
    pub id: TabId,
    /// The project this tab belongs to (`None` with no project open).
    pub space: Option<String>,
    /// The person, or one session.
    pub owner: Owner,
    /// The page's URL; `about:blank` for a fresh tab.
    pub url: String,
    /// The page's title; the URL until the page reports one.
    pub title: String,
    /// Pinned tabs sort first and are never auto-discarded.
    #[serde(default)]
    pub pinned: bool,
    /// When this tab was last the shown page, as ms since the epoch
    /// ([`now_ms`]); `None` for a tab never shown yet. Drives discard.
    #[serde(default)]
    pub last_shown: Option<u64>,
}

/// How many closed tabs [`Tabs::closed`] keeps for reopen.
pub(crate) const CLOSED_CAP: usize = 20;

/// One row of the pane's tab strip: the snapshot `right.rs` draws for the
/// shown owner's tabs. Pure data, so the pane never reads the model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StripTab {
    /// Which tab this row selects or closes.
    pub id: TabId,
    /// The row's label: [`tab_label`] of the tab's title and URL.
    pub title: String,
    /// The tile's letter: [`tab_initial`] of the tab's title and URL.
    pub initial: String,
    /// Whether this row is the owner's active tab.
    pub active: bool,
}

/// The whole tab model: ordered tabs, each owner's active tab, and the
/// recently closed for reopen. Serialisable as one JSON file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Tabs {
    /// Every open tab, in strip order.
    #[serde(default)]
    pub tabs: Vec<Tab>,
    /// Each owner's active tab, keyed by [`owner_key`].
    #[serde(default)]
    pub active: HashMap<String, TabId>,
    /// The next [`TabId`] to hand out; monotonic, never reused.
    #[serde(default)]
    pub next_id: u64,
    /// Recently closed tabs, oldest first, capped at [`CLOSED_CAP`].
    #[serde(default)]
    pub closed: Vec<Tab>,
}

impl Tabs {
    /// Open a tab for `owner` in `space` on `url`, selecting it. Inserted
    /// directly after `after` when that tab exists, else appended.
    pub(crate) fn open(
        &mut self,
        space: Option<String>,
        owner: Owner,
        url: &str,
        after: Option<TabId>,
    ) -> TabId {
        let id = TabId(self.next_id);
        self.next_id += 1;
        let tab =
            Tab { id, space, owner: owner.clone(), url: url.to_owned(), title: url.to_owned(), pinned: false, last_shown: None };
        let at = after
            .and_then(|after| self.tabs.iter().position(|tab| tab.id == after))
            .map(|index| index + 1)
            .unwrap_or(self.tabs.len());
        self.tabs.insert(at.min(self.tabs.len()), tab);
        self.active.insert(owner_key(&owner), id);
        id
    }

    /// Close `id`, keeping it for [`Self::reopen_last`]. Answers with the
    /// neighbour to activate: the next tab of the same owner, else the
    /// previous one, else nothing (the owner has no tabs left). The
    /// owner's active pointer follows the answer.
    pub(crate) fn close(&mut self, id: TabId) -> Option<TabId> {
        let index = self.tabs.iter().position(|tab| tab.id == id)?;
        let tab = self.tabs.remove(index);
        self.closed.push(tab.clone());
        while self.closed.len() > CLOSED_CAP {
            self.closed.remove(0);
        }
        // The neighbour that slid into (or already stood behind) the
        // removed tab's slot: the first sibling at or after the removal
        // index, else the last sibling before it.
        let next = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(_, other)| other.owner == tab.owner)
            .filter_map(|(i, other)| (i >= index).then_some(other.id))
            .next()
            .or_else(|| {
                self.tabs.iter().filter(|other| other.owner == tab.owner).map(|other| other.id).next_back()
            });
        let key = owner_key(&tab.owner);
        match next {
            Some(id) => {
                self.active.insert(key, id);
                Some(id)
            }
            None => {
                self.active.remove(&key);
                None
            }
        }
    }

    /// Reopen the most recently closed tab, selecting it for its owner.
    /// Keeps its identity, space and URL; appended at the strip's end.
    /// (Next tasks: the reopen control.)
    #[allow(dead_code)]
    pub(crate) fn reopen_last(&mut self) -> Option<TabId> {
        let tab = self.closed.pop()?;
        let id = tab.id;
        // A live tab can never carry the id: ids are never reused, and a
        // reopened tab left the live set when it closed.
        debug_assert!(!self.tabs.iter().any(|live| live.id == id));
        self.active.insert(owner_key(&tab.owner), id);
        self.tabs.push(tab);
        Some(id)
    }

    /// Select `id` for its owner. False for an unknown tab.
    pub(crate) fn activate(&mut self, id: TabId) -> bool {
        match self.tabs.iter().find(|tab| tab.id == id) {
            Some(tab) => {
                self.active.insert(owner_key(&tab.owner), id);
                true
            }
            None => false,
        }
    }

    /// The tab with `id`, when open.
    pub(crate) fn get(&self, id: TabId) -> Option<&Tab> {
        self.tabs.iter().find(|tab| tab.id == id)
    }

    /// The owner's active tab, when it has one still open.
    pub(crate) fn active_of(&self, owner: &Owner) -> Option<TabId> {
        let id = self.active.get(&owner_key(owner))?;
        self.tabs.iter().any(|tab| &tab.id == id).then_some(*id)
    }

    /// Every open tab of `owner`, in strip order.
    pub(crate) fn tabs_of(&self, owner: &Owner) -> Vec<&Tab> {
        self.tabs.iter().filter(|tab| &tab.owner == owner).collect()
    }

    /// The person's tabs in `space`, pinned first, in strip order.
    pub(crate) fn person_tabs(&self, space: Option<&str>) -> Vec<&Tab> {
        let mut tabs: Vec<&Tab> = self
            .tabs
            .iter()
            .filter(|tab| tab.owner == Owner::Person && tab.space.as_deref() == space)
            .collect();
        tabs.sort_by_key(|tab| (!tab.pinned, self.tabs.iter().position(|t| t.id == tab.id)));
        tabs
    }

    /// Every session's tabs in `space`, grouped by session id in first-seen
    /// order: Browse mode's "From sessions" (next tasks; the strip and the
    /// sync use the single-owner views above until then).
    #[allow(dead_code)]
    pub(crate) fn session_tabs(&self, space: Option<&str>) -> Vec<(String, Vec<&Tab>)> {
        let mut groups: Vec<(String, Vec<&Tab>)> = Vec::new();
        for tab in &self.tabs {
            let Owner::Session(session) = &tab.owner else { continue };
            if tab.space.as_deref() != space {
                continue;
            }
            match groups.iter_mut().find(|(id, _)| id == session) {
                Some((_, tabs)) => tabs.push(tab),
                None => groups.push((session.clone(), vec![tab])),
            }
        }
        groups
    }

    /// Hand `id` to another owner, keeping its space. The old owner's
    /// active pointer falls back to its newest remaining tab (or is
    /// cleared); the new owner's active pointer moves onto the tab.
    /// False for an unknown tab. (Next tasks: handing session tabs to the
    /// person in Browse mode.)
    #[allow(dead_code)]
    pub(crate) fn move_to(&mut self, id: TabId, owner: Owner) -> bool {
        let index = match self.tabs.iter().position(|tab| tab.id == id) {
            Some(index) => index,
            None => return false,
        };
        let old = self.tabs[index].owner.clone();
        if old == owner {
            return true;
        }
        self.tabs[index].owner = owner.clone();
        // The old owner falls back like a close; the new owner selects.
        let siblings: Vec<TabId> =
            self.tabs.iter().filter(|tab| tab.owner == old).map(|tab| tab.id).collect();
        let key = owner_key(&old);
        match siblings.last() {
            Some(id) => {
                self.active.insert(key, *id);
            }
            None => {
                self.active.remove(&key);
            }
        }
        self.active.insert(owner_key(&owner), id);
        true
    }

    /// Pin or unpin `id`. False for an unknown tab. (Next tasks: the pin
    /// control on the strip.)
    #[allow(dead_code)]
    pub(crate) fn set_pinned(&mut self, id: TabId, pinned: bool) -> bool {
        match self.tabs.iter_mut().find(|tab| tab.id == id) {
            Some(tab) => {
                tab.pinned = pinned;
                true
            }
            None => false,
        }
    }

    /// Move `id` to strip position `index` (clamped). False for unknown.
    /// (Next tasks: strip drag-reorder.)
    #[allow(dead_code)]
    pub(crate) fn reorder(&mut self, id: TabId, index: usize) -> bool {
        let from = match self.tabs.iter().position(|tab| tab.id == id) {
            Some(from) => from,
            None => return false,
        };
        let tab = self.tabs.remove(from);
        let at = index.min(self.tabs.len());
        self.tabs.insert(at, tab);
        true
    }

    /// Record a live page's URL and title on `id`. Answers whether either
    /// changed (the caller persists only on change).
    pub(crate) fn set_url_title(&mut self, id: TabId, url: &str, title: &str) -> bool {
        match self.tabs.iter_mut().find(|tab| tab.id == id) {
            Some(tab) => {
                let changed = tab.url != url || tab.title != title;
                tab.url = url.to_owned();
                tab.title = title.to_owned();
                changed
            }
            None => false,
        }
    }

    /// Write the store atomically. Best-effort: losing tabs is a nuisance,
    /// never a loss — the sessions and their URLs survive elsewhere.
    pub(crate) fn save(&self) {
        if let Ok(text) = serde_json::to_vec_pretty(self) {
            let _ = crate::store::write_atomic(&path(), &text);
        }
    }

    /// Stamp `id` as the just-shown page, at minute granularity so the
    /// per-frame sync does not persist every frame. Answers whether the
    /// stamp moved (the caller persists on true).
    pub(crate) fn mark_shown(&mut self, id: TabId, now: u64) -> bool {
        match self.tabs.iter_mut().find(|tab| tab.id == id) {
            Some(tab) => {
                let stale =
                    tab.last_shown.is_none_or(|last| now.saturating_sub(last) >= 60_000);
                if stale {
                    tab.last_shown = Some(now);
                }
                stale
            }
            None => false,
        }
    }

    /// Give every session with a stored `browser_url` and no tabs yet one
    /// session tab with that URL (space from the session's project).
    /// Answers whether any tab was created; the caller persists on true.
    pub(crate) fn migrate_from_overrides(&mut self, overrides: &crate::sessions::Overrides) -> bool {
        let mut made = false;
        let mut sessions: Vec<(String, Option<String>, String)> = overrides
            .iter()
            .filter_map(|(id, meta)| {
                meta.right
                    .as_ref()
                    .and_then(|right| right.browser_url.clone())
                    .map(|url| (id.clone(), meta.project.clone(), url))
            })
            .collect();
        sessions.sort();
        for (session, project, url) in sessions {
            let owner = Owner::Session(session);
            if self.tabs.iter().any(|tab| tab.owner == owner) {
                continue;
            }
            self.open(project, owner, &url, None);
            made = true;
        }
        made
    }
}

/// `$BAAZ_STATE_DIR/Browser/tabs.json`: the tab model's home. The `Browser`
/// dir is created on write.
pub(crate) fn path() -> std::path::PathBuf {
    crate::store::support_dir().join("Browser").join("tabs.json")
}

/// Read the store; best-effort like every store read — a missing or
/// unparseable file is an empty model, and migration refills it.
pub(crate) fn load() -> Tabs {
    crate::store::read_json(&path())
}

/// Now, as ms since the epoch: the clock behind `last_shown`.
pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

/// How long a hidden tab's page lives before discard, in seconds:
/// 30 minutes, or `BAAZ_TAB_DISCARD_SECS` when set (tests, live checks).
pub(crate) fn discard_timeout_secs() -> u64 {
    std::env::var("BAAZ_TAB_DISCARD_SECS")
        .ok()
        .and_then(|raw| raw.parse().ok())
        .filter(|secs| *secs > 0)
        .unwrap_or(30 * 60)
}

/// Whether `last_shown_ms` is idle past the timeout at `now_ms`. A tab
/// never shown (`None`) is never due: it may be brand new, and the frame
/// must not discard a page before its first show.
pub(crate) fn discard_due(now_ms: u64, last_shown_ms: Option<u64>) -> bool {
    match last_shown_ms {
        None => false,
        Some(last) => now_ms.saturating_sub(last) > discard_timeout_secs() * 1000,
    }
}

/// Whether a hidden tab's page should be discarded: idle past the timeout,
/// audibly silent, and not the agent's active tab. Pinned tabs are
/// spared by the caller, which reads the pin.
pub(crate) fn should_discard(
    now_ms: u64,
    last_shown_ms: Option<u64>,
    audible: bool,
    is_agent_active: bool,
) -> bool {
    !audible && !is_agent_active && discard_due(now_ms, last_shown_ms)
}

/// The strip row's label: the title, or the URL's host (or the whole URL
/// when it has no host, e.g. `about:blank`) while the page has no title.
pub(crate) fn tab_label(title: &str, url: &str) -> String {
    if !title.is_empty() {
        return title.to_owned();
    }
    host_of(url).unwrap_or_else(|| url.to_owned())
}

/// The tile's letter: the label's first alphanumeric, uppercased — `•`
/// when the label starts with nothing usable.
pub(crate) fn tab_initial(title: &str, url: &str) -> String {
    tab_label(title, url)
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_else(|| "•".to_owned())
}

/// The URL's host without a leading `www.`, when it parses as hierarchical.
fn host_of(url: &str) -> Option<String> {
    let after = url.split("://").nth(1)?;
    let host = after.split(['/']).next()?.split('@').next_back()?;
    let host = host.split(':').next()?;
    if host.is_empty() {
        return None;
    }
    Some(host.strip_prefix("www.").unwrap_or(host).to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tabs() -> Tabs {
        Tabs::default()
    }

    fn session(id: &str) -> Owner {
        Owner::Session(id.to_owned())
    }

    #[test]
    fn opening_selects_and_appends_in_order() {
        let mut model = tabs();
        let first = model.open(None, session("s"), "https://a.example/", None);
        let second = model.open(None, session("s"), "https://b.example/", None);
        assert_eq!(model.active_of(&session("s")), Some(second));
        assert_eq!(model.tabs_of(&session("s")).iter().map(|tab| tab.id).collect::<Vec<_>>(), vec![first, second]);
        assert!(model.activate(first));
        assert_eq!(model.active_of(&session("s")), Some(first));
        assert!(!model.activate(TabId(99)));
    }

    #[test]
    fn opening_after_inserts_behind_the_anchor() {
        let mut model = tabs();
        let first = model.open(None, session("s"), "https://a.example/", None);
        let second = model.open(None, session("s"), "https://b.example/", Some(first));
        let third = model.open(None, session("s"), "https://c.example/", Some(TabId(4242)));
        let order: Vec<TabId> = model.tabs.iter().map(|tab| tab.id).collect();
        assert_eq!(order, vec![first, second, third]);
    }

    #[test]
    fn closing_activates_the_neighbour() {
        let mut model = tabs();
        let first = model.open(None, session("s"), "https://a.example/", None);
        let second = model.open(None, session("s"), "https://b.example/", None);
        let third = model.open(None, session("s"), "https://c.example/", None);
        // Closing the middle activates the next sibling.
        assert_eq!(model.close(second), Some(third));
        assert_eq!(model.active_of(&session("s")), Some(third));
        // Closing the last activates the previous sibling.
        assert_eq!(model.close(third), Some(first));
        assert_eq!(model.active_of(&session("s")), Some(first));
        // Closing the only tab clears the owner.
        assert_eq!(model.close(first), None);
        assert_eq!(model.active_of(&session("s")), None);
        // Unknown ids close nothing.
        assert_eq!(model.close(TabId(77)), None);
    }

    #[test]
    fn closing_keeps_owners_apart() {
        let mut model = tabs();
        let mine = model.open(None, session("s"), "https://a.example/", None);
        let other = model.open(None, session("t"), "https://b.example/", None);
        assert_eq!(model.close(mine), None);
        assert_eq!(model.active_of(&session("s")), None);
        assert_eq!(model.active_of(&session("t")), Some(other));
    }

    #[test]
    fn closed_tabs_reopen_newest_first_with_a_cap() {
        let mut model = tabs();
        let mut ids = Vec::new();
        for index in 0..(CLOSED_CAP + 5) {
            ids.push(model.open(None, session("s"), &format!("https://{index}.example/"), None));
        }
        for id in ids.iter() {
            model.close(*id);
        }
        assert_eq!(model.closed.len(), CLOSED_CAP, "reopen history is capped");
        let newest = model.reopen_last().expect("a closed tab");
        assert_eq!(model.get(newest).map(|tab| tab.url.as_str()), Some("https://24.example/"));
        assert_eq!(model.active_of(&session("s")), Some(newest));
    }

    #[test]
    fn moving_hands_the_tab_to_the_new_owner() {
        let mut model = tabs();
        let id = model.open(None, session("s"), "https://a.example/", None);
        assert!(model.move_to(id, Owner::Person));
        assert!(model.tabs_of(&session("s")).is_empty());
        assert_eq!(model.active_of(&session("s")), None);
        assert_eq!(model.active_of(&Owner::Person), Some(id));
        assert!(!model.move_to(TabId(404), Owner::Person));
    }

    #[test]
    fn reordering_moves_within_the_strip() {
        let mut model = tabs();
        let first = model.open(None, session("s"), "https://a.example/", None);
        let second = model.open(None, session("s"), "https://b.example/", None);
        let third = model.open(None, session("s"), "https://c.example/", None);
        assert!(model.reorder(first, 5));
        let order: Vec<TabId> = model.tabs.iter().map(|tab| tab.id).collect();
        assert_eq!(order, vec![second, third, first]);
        assert!(!model.reorder(TabId(99), 0));
    }

    #[test]
    fn pins_and_urls_update() {
        let mut model = tabs();
        let id = model.open(None, session("s"), "https://a.example/", None);
        assert!(model.set_pinned(id, true));
        assert!(model.get(id).is_some_and(|tab| tab.pinned));
        assert!(!model.set_pinned(TabId(99), true));
        assert!(model.set_url_title(id, "https://b.example/", "B"));
        assert!(!model.set_url_title(id, "https://b.example/", "B"), "an unchanged report is quiet");
        assert_eq!(model.get(id).map(|tab| tab.title.as_str()), Some("B"));
    }

    #[test]
    fn person_and_session_views_split_by_space() {
        let mut model = tabs();
        model.open(Some("p1".into()), Owner::Person, "https://a.example/", None);
        let pinned = model.open(Some("p1".into()), Owner::Person, "https://b.example/", None);
        model.set_pinned(pinned, true);
        model.open(Some("p2".into()), Owner::Person, "https://c.example/", None);
        model.open(Some("p1".into()), session("s"), "https://d.example/", None);
        model.open(Some("p1".into()), session("t"), "https://e.example/", None);
        let person: Vec<&str> =
            model.person_tabs(Some("p1")).iter().map(|tab| tab.url.as_str()).collect();
        assert_eq!(person, vec!["https://b.example/", "https://a.example/"], "pinned first");
        let groups = model.session_tabs(Some("p1"));
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].0, "s");
        assert_eq!(groups[1].0, "t");
        assert!(model.session_tabs(Some("p2")).is_empty());
    }

    #[test]
    fn migration_gives_each_stored_url_one_session_tab() {
        let _guard = crate::store::test_env_lock();
        let mut model = tabs();
        let mut overrides = crate::sessions::Overrides::new();
        overrides.insert(
            "s-1".into(),
            crate::sessions::SessionMeta {
                project: Some("p1".into()),
                right: Some(crate::sessions::RightState {
                    browser_url: Some("https://a.example/".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        overrides.insert("s-2".into(), crate::sessions::SessionMeta::default());
        assert!(model.migrate_from_overrides(&overrides));
        let owned = model.tabs_of(&session("s-1"));
        assert_eq!(owned.len(), 1);
        assert_eq!(owned[0].url, "https://a.example/");
        assert_eq!(owned[0].space.as_deref(), Some("p1"));
        // A second load migrates nothing: the session has a tab now.
        assert!(!model.migrate_from_overrides(&overrides));
        assert_eq!(model.tabs_of(&session("s-1")).len(), 1);
    }

    #[test]
    fn tabs_round_trip_through_json() {
        let _guard = crate::store::test_env_lock();
        let old = std::env::var_os("BAAZ_STATE_DIR");
        let dir = std::env::temp_dir().join(format!("baaz-tabs-rt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::env::set_var("BAAZ_STATE_DIR", &dir);
        let mut model = tabs();
        let id = model.open(Some("p1".into()), session("s"), "https://a.example/", None);
        model.set_pinned(id, true);
        model.mark_shown(id, 12345);
        model.save();
        let back = load();
        assert_eq!(back, model);
        assert_eq!(back.get(id).and_then(|tab| tab.last_shown), Some(12345));
        match old {
            Some(value) => std::env::set_var("BAAZ_STATE_DIR", value),
            None => std::env::remove_var("BAAZ_STATE_DIR"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discard_fires_only_when_idle_silent_and_unregistered() {
        let _guard = crate::store::test_env_lock();
        let old = std::env::var_os("BAAZ_TAB_DISCARD_SECS");
        std::env::set_var("BAAZ_TAB_DISCARD_SECS", "60");
        let now = 10_000_000;
        assert!(should_discard(now, Some(now - 61_000), false, false), "idle past the timeout goes");
        assert!(!should_discard(now, Some(now - 59_000), false, false), "within the timeout stays");
        assert!(!should_discard(now, None, false, false), "never shown stays");
        assert!(!should_discard(now, Some(now - 61_000), true, false), "audible stays");
        assert!(!should_discard(now, Some(now - 61_000), false, true), "the agent's tab stays");
        assert!(!should_discard(now - 5_000, Some(now), false, false), "clock skew never discards");
        match old {
            Some(value) => std::env::set_var("BAAZ_TAB_DISCARD_SECS", value),
            None => std::env::remove_var("BAAZ_TAB_DISCARD_SECS"),
        }
    }

    #[test]
    fn strip_labels_fall_back_to_hosts_and_initials() {
        assert_eq!(tab_label("Ledger", "https://localhost:5173/"), "Ledger");
        assert_eq!(tab_label("", "https://www.wikipedia.org/wiki/X"), "wikipedia.org");
        assert_eq!(tab_label("", "about:blank"), "about:blank");
        assert_eq!(tab_initial("", "https://docs.rs/"), "D");
        assert_eq!(tab_initial("", "about:blank"), "A");
    }
}