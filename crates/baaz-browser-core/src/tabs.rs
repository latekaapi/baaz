//! Browser tabs: the pure tab model (B2a-H1). No gpui types here.
//!
//! A tab belongs to a **space** (the browser's own: Personal, Work; older
//! project-filed tabs moved into folders, see [`Tabs::ensure_spaces`]) and
//! to an **owner**: the person, or one session. In Sessions mode the right
//! pane shows the active session's page; Browse mode shows the space's person
//! tabs plus one task folder per session that owns tabs.
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
pub struct TabId(pub u64);

/// Who a tab belongs to: the person, or one session by id.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Owner {
    /// The person's own tabs, shown per Space in Browse mode.
    Person,
    /// One agent/coding session's tabs, shown in Sessions mode.
    Session(String),
}

/// What a fresh webview opens on: the blank default, never persisted.
pub const BLANK: &str = "about:blank";

/// The map key for [`Tabs::active`]: `"person:<space>"` for the person's
/// tabs in one Space (`""` with none, an older file before
/// [`Tabs::ensure_spaces`]) or `"session:<id>"` for a session (its tabs
/// show in every Space, so the id suffices).
/// A plain string (not the enum) so the map serialises as JSON.
pub fn active_key(owner: &Owner, space: Option<&str>) -> String {
    match owner {
        Owner::Person => format!("person:{}", space.unwrap_or("")),
        Owner::Session(id) => format!("session:{id}"),
    }
}

/// One browser tab: where it lives, who owns it, and what it shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tab {
    /// Stable identity, from [`Tabs::next_id`].
    pub id: TabId,
    /// The Space this tab belongs to (a [`Space`] id). `None` only in an
    /// older file, before [`Tabs::ensure_spaces`] — which also moved the
    /// project ids older builds kept here into folders — or for a session's
    /// tab its migration opened with the session's project
    /// ([`Tabs::migrate_from_overrides`]); a session's tabs show in every
    /// Space, so nothing reads that value as a Space.
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
    /// The folder this tab sits in (one of the person's own folders in its
    /// space), or none (Today).
    #[serde(default)]
    pub folder: Option<String>,
    /// The page's icon URL as the engine last reported it, so a discarded
    /// or relaunched tab keeps its favicon ([`super::favicons`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub favicon: Option<String>,
    /// A background tab holds a permission ask: the sidebar draws its
    /// "needs you" dot until the tab is shown (B2a-H5). Live state, never
    /// persisted: a relaunch starts with no dots.
    #[serde(skip)]
    pub needs_attention: bool,
}

/// One browser space (docs/27 §4.2): the browser's own set of tabs with
/// its own pins and folders — Personal, Work, a trip. Projects are not
/// spaces; a session's tabs show in every space as a task folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Space {
    /// Stable id (`space-<n>`; the first one is [`PERSONAL`]).
    pub id: String,
    /// The chip's label.
    pub name: String,
    /// R4: the space keeps its own sign-ins (cookies, storage, cache) in
    /// `Browser/Chromium/Spaces/<id>`; shared (the default) uses the one
    /// profile every space shares.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub own_signins: bool,
}

/// One of the person's own folders, inside one space.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Folder {
    /// Stable id (`folder-<n>`), never the name: two folders may share one.
    pub id: String,
    /// The space it lives in.
    pub space: String,
    /// The visible name.
    pub name: String,
    /// Whether its tabs show.
    #[serde(default = "folder_open_default")]
    pub open: bool,
}

fn folder_open_default() -> bool {
    true
}

/// The first space's id: every person tab lands here on migration.
pub const PERSONAL: &str = "personal";

/// How many closed tabs [`Tabs::closed`] keeps for reopen.
pub const CLOSED_CAP: usize = 20;

/// Whether `url` is the fresh tab's page (`about:blank`, with or without the
/// trailing slash Chromium reports it with).
pub fn is_blank_url(url: &str) -> bool {
    url.trim_end_matches('/') == BLANK
}

/// The whole tab model: ordered tabs, each owner's active tab, and the
/// recently closed for reopen. Serialisable as one JSON file.
#[derive(Clone, Debug, Default, Eq, Serialize, Deserialize)]
pub struct Tabs {
    /// Every open tab, in strip order.
    #[serde(default)]
    pub tabs: Vec<Tab>,
    /// Each owner's active tab, keyed by [`active_key`].
    #[serde(default)]
    pub active: HashMap<String, TabId>,
    /// The next [`TabId`] to hand out; monotonic, never reused.
    #[serde(default)]
    pub next_id: u64,
    /// Recently closed tabs, oldest first, capped at [`CLOSED_CAP`].
    #[serde(default)]
    pub closed: Vec<Tab>,
    /// Session ids whose `browser_url` migration already ran: the
    /// migration runs only once per session, so a closed last tab never
    /// resurrects from a stale `browser_url`.
    #[serde(default)]
    pub migrated: Vec<String>,
    /// The browser's spaces, in chip order. Empty until
    /// [`Tabs::ensure_spaces`] migrates an older file.
    #[serde(default)]
    pub spaces: Vec<Space>,
    /// The space Browse shows.
    #[serde(default)]
    pub current_space: String,
    /// The person's folders, in every space, in list order.
    #[serde(default)]
    pub folders: Vec<Folder>,
    /// The next folder / space number to hand out.
    #[serde(default)]
    pub next_group: u64,
    /// The model generation: bumped on every mutation (open, close,
    /// activate, pin, reorder, retitle), so the cached sidebar keys the
    /// Browse fingerprints on this counter plus row state instead of
    /// rebuilding label strings per frame. Never serialised: a load
    /// starts a fresh run at zero.
    #[serde(skip, default)]
    pub generation: u64,
}

/// Content equality: the generation is an ephemeral repaint counter —
/// never serialised, never compared — so a loaded snapshot equals the
/// model that wrote it.
impl PartialEq for Tabs {
    fn eq(&self, other: &Self) -> bool {
        self.tabs == other.tabs
            && self.active == other.active
            && self.next_id == other.next_id
            && self.closed == other.closed
            && self.migrated == other.migrated
            && self.spaces == other.spaces
            && self.current_space == other.current_space
            && self.folders == other.folders
            && self.next_group == other.next_group
    }
}

impl Tabs {
    /// Bump the model generation: every mutation calls this, so the
    /// cached sidebar's key moves on any change without comparing
    /// labels. Wraps on overflow: equality is all the key needs.
    pub fn bump(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    /// Open a tab for `owner` in `space` on `url`, selecting it. Inserted
    /// directly after `after` when that tab exists, else appended.
    pub fn open(
        &mut self,
        space: Option<String>,
        owner: Owner,
        url: &str,
        after: Option<TabId>,
    ) -> TabId {
        let id = TabId(self.next_id);
        self.next_id += 1;
        let key = active_key(&owner, space.as_deref());
        let tab = Tab {
            id,
            space,
            owner: owner.clone(),
            url: url.to_owned(),
            title: url.to_owned(),
            pinned: false,
            last_shown: None,
            folder: None,
            favicon: None,
            needs_attention: false,
        };
        let at = after
            .and_then(|after| self.tabs.iter().position(|tab| tab.id == after))
            .map(|index| index + 1)
            .unwrap_or(self.tabs.len());
        self.tabs.insert(at.min(self.tabs.len()), tab);
        self.active.insert(key, id);
        self.bump();
        id
    }

    /// Close `id`, keeping it for [`Self::reopen_last`]. Answers with the
    /// tab to show now, and only when the closed tab was its owner's
    /// active tab: the display-order neighbour after it, else the one
    /// before it, else nothing (the owner has no tabs left). Closing a
    /// background tab answers nothing and leaves the active pointer —
    /// and every registration — exactly alone, so the caller rebuilds
    /// nothing. The owner's active pointer follows the answer.
    pub fn close(&mut self, id: TabId) -> Option<TabId> {
        let index = self.tabs.iter().position(|tab| tab.id == id)?;
        // The display-order siblings around the closed tab: the one after
        // it in [`Self::ordered_tabs`], else the one before it. Owned ids
        // only — the removal below ends every borrow.
        let (owner, space) = {
            let tab = &self.tabs[index];
            (tab.owner.clone(), tab.space.clone())
        };
        let siblings: Vec<TabId> = self
            .ordered_tabs(&owner, space.as_deref())
            .iter()
            .map(|sibling| sibling.id)
            .collect();
        let at = siblings.iter().position(|sibling| *sibling == id);
        let tab = self.tabs.remove(index);
        // A blank tab has nothing to pick up again: it never joins the
        // closed list (owner 2026-10-13: five "New tab · closed" rows).
        if !is_blank_url(&tab.url) {
            self.closed.push(tab.clone());
            while self.closed.len() > CLOSED_CAP {
                self.closed.remove(0);
            }
        }
        self.bump();
        let key = active_key(&tab.owner, tab.space.as_deref());
        if self.active.get(&key) != Some(&id) {
            return None;
        }
        let next = at
            .and_then(|at| {
                siblings
                    .get(at + 1)
                    .or_else(|| (at > 0).then(|| &siblings[at - 1]))
            })
            .copied();
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

    /// Reopen the closed tab `id`, selecting it for its owner: the new-tab
    /// page's closed rows. Keeps its identity, space and URL; appended at
    /// the strip's end. `None` for an unknown (or never-closed) id.
    pub fn reopen(&mut self, id: TabId) -> Option<TabId> {
        let ix = self.closed.iter().position(|tab| tab.id == id)?;
        let tab = self.closed.remove(ix);
        // A live tab can never carry the id: ids are never reused, and a
        // reopened tab left the live set when it closed.
        debug_assert!(!self.tabs.iter().any(|live| live.id == id));
        self.active.insert(active_key(&tab.owner, tab.space.as_deref()), id);
        self.tabs.push(tab);
        Some(id)
    }

    /// Reopen the most recently closed tab, selecting it for its owner.
    /// Keeps its identity, space and URL; appended at the strip's end.
    /// (Next tasks: the reopen control.)
    pub fn reopen_last(&mut self) -> Option<TabId> {
        let tab = self.closed.pop()?;
        let id = tab.id;
        // A live tab can never carry the id: ids are never reused, and a
        // reopened tab left the live set when it closed.
        debug_assert!(!self.tabs.iter().any(|live| live.id == id));
        self.active
            .insert(active_key(&tab.owner, tab.space.as_deref()), id);
        self.tabs.push(tab);
        self.bump();
        Some(id)
    }

    /// Select `id` for its owner. False for an unknown tab.
    pub fn activate(&mut self, id: TabId) -> bool {
        match self.tabs.iter().find(|tab| tab.id == id) {
            Some(tab) => {
                let tab = tab.clone();
                self.active
                    .insert(active_key(&tab.owner, tab.space.as_deref()), id);
                self.bump();
                true
            }
            None => false,
        }
    }

    /// The tab with `id`, when open.
    pub fn get(&self, id: TabId) -> Option<&Tab> {
        self.tabs.iter().find(|tab| tab.id == id)
    }

    /// The owner's active tab, when it has one still open. Person tabs
    /// are per Space: `space` is the Space asking, and switching Space
    /// shows that Space's active person tab (or none). A session's tabs
    /// show in every Space, so `space` is ignored for them.
    pub fn active_of(&self, owner: &Owner, space: Option<&str>) -> Option<TabId> {
        let id = self.active.get(&active_key(owner, space))?;
        self.tabs.iter().any(|tab| &tab.id == id).then_some(*id)
    }

    /// Every open tab of `owner`, in strip order.
    pub fn tabs_of(&self, owner: &Owner) -> Vec<&Tab> {
        self.tabs.iter().filter(|tab| &tab.owner == owner).collect()
    }

    /// The ONE display ordering behind the strip, `strip_tab_at` (so
    /// `tab-select:n`/`tab-close:n` hit the n-th tab ON SCREEN), the
    /// close-neighbour choice, and future sidebars: pinned first, then
    /// model order. Person tabs are per Space; every session tab shows
    /// regardless of `space`.
    pub fn ordered_tabs(&self, owner: &Owner, space: Option<&str>) -> Vec<&Tab> {
        // One position walk, then a sort on cached keys: O(n log n), not
        // the O(n² log n) the per-comparison `position` scan cost.
        let positions: std::collections::HashMap<TabId, usize> =
            self.tabs.iter().enumerate().map(|(index, tab)| (tab.id, index)).collect();
        let mut tabs: Vec<&Tab> = self
            .tabs
            .iter()
            .filter(|tab| {
                tab.owner == *owner && (tab.owner != Owner::Person || tab.space.as_deref() == space)
            })
            .collect();
        tabs.sort_by_key(|tab| (!tab.pinned, positions.get(&tab.id).copied().unwrap_or(usize::MAX)));
        tabs
    }

    /// The person's tabs in `space`, pinned first, in strip order.
    pub fn person_tabs(&self, space: Option<&str>) -> Vec<&Tab> {
        self.ordered_tabs(&Owner::Person, space)
    }

    /// Every session's tabs in `space`, grouped by session id in first-seen
    /// order: Browse mode's task folders.
    pub fn session_tabs(&self, space: Option<&str>) -> Vec<(String, Vec<&Tab>)> {
        let mut groups: Vec<(String, Vec<&Tab>)> = Vec::new();
        for tab in &self.tabs {
            let Owner::Session(session) = &tab.owner else {
                continue;
            };
            // Task folders show in every Space (docs/27 §4.3); `space`
            // narrows only when a caller asks for one.
            if space.is_some_and(|space| tab.space.as_deref() != Some(space)) {
                continue;
            }
            match groups.iter_mut().find(|(id, _)| id == session) {
                Some((_, tabs)) => tabs.push(tab),
                None => groups.push((session.clone(), vec![tab])),
            }
        }
        groups
    }

    /// The one ownership move (docs/82 8.3): hand `id` to `owner` — a
    /// session's tab kept by the person ("Keep this tab"), or a tab handed
    /// to a session (a browser chat's hand-off, a provider's renamed id).
    /// The tab keeps its id, URL, title, pin and favicon, so its live page
    /// and its back/forward history (both keyed by id) stay with it; it
    /// leaves any folder (a person's folder means nothing to a session, and
    /// a task folder is the session itself). `space` places it: `Some` for
    /// the person's current space, `None` to keep the one it has. The old
    /// owner's active pointer falls back to its newest remaining tab (or is
    /// cleared); the new owner's moves onto the tab. False for an unknown
    /// tab; true and nothing changed when it already is `owner`'s there.
    pub fn move_to(&mut self, id: TabId, owner: Owner, space: Option<&str>) -> bool {
        let index = match self.tabs.iter().position(|tab| tab.id == id) {
            Some(index) => index,
            None => return false,
        };
        let old = self.tabs[index].owner.clone();
        let old_space = self.tabs[index].space.clone();
        let new_space = space.map(str::to_owned).or_else(|| old_space.clone());
        if old == owner && new_space == old_space {
            return true;
        }
        self.tabs[index].owner = owner.clone();
        self.tabs[index].space = new_space.clone();
        self.tabs[index].folder = None;
        // The old owner falls back like a close; the new owner selects.
        let siblings: Vec<TabId> = self
            .tabs
            .iter()
            .filter(|tab| tab.owner == old && tab.space == old_space)
            .map(|tab| tab.id)
            .collect();
        let key = active_key(&old, old_space.as_deref());
        match siblings.last() {
            Some(id) => {
                self.active.insert(key, *id);
            }
            None => {
                self.active.remove(&key);
            }
        }
        self.active.insert(active_key(&owner, new_space.as_deref()), id);
        self.bump();
        true
    }

    /// Pin or unpin `id`, moving it to the end of the pinned block (when
    /// pinning: behind the last pinned tab) or the start of the unpinned
    /// block (when unpinning: ahead of the first unpinned tab) — so the
    /// model order already reads like the strip. False for an unknown tab.
    /// (Next tasks: the pin control on the strip.)
    pub fn set_pinned(&mut self, id: TabId, pinned: bool) -> bool {
        let from = match self.tabs.iter().position(|tab| tab.id == id) {
            Some(from) => from,
            None => return false,
        };
        let mut tab = self.tabs.remove(from);
        tab.pinned = pinned;
        let at = if pinned {
            // Behind the last pinned tab (or the front when none).
            self.tabs
                .iter()
                .rposition(|tab| tab.pinned)
                .map(|last| last + 1)
                .unwrap_or(0)
        } else {
            // Ahead of the first unpinned tab (or the end when none).
            self.tabs
                .iter()
                .position(|tab| !tab.pinned)
                .unwrap_or(self.tabs.len())
        };
        self.tabs.insert(at, tab);
        self.bump();
        true
    }

    /// Move `id` to strip position `index` (clamped). False for unknown.
    /// (Next tasks: strip drag-reorder.)
    #[cfg(any(test, feature = "test-support"))]
    pub fn reorder(&mut self, id: TabId, index: usize) -> bool {
        let from = match self.tabs.iter().position(|tab| tab.id == id) {
            Some(from) => from,
            None => return false,
        };
        let tab = self.tabs.remove(from);
        let at = index.min(self.tabs.len());
        self.tabs.insert(at, tab);
        self.bump();
        true
    }


    /// Move the owner's active tab one display slot (`delta` of -1 or 1):
    /// the Browse sidebar's ⌥⌘↑/⌥⌘↓. Display order is pinned-first, so
    /// only a neighbour in the same pinned class swaps — crossing the
    /// pinned boundary would need a (un)pin, never a silent reorder.
    /// False with no active tab, at the edge, or across the boundary.
    pub fn move_active_by(&mut self, owner: &Owner, space: Option<&str>, delta: i32) -> bool {
        let order: Vec<(TabId, bool)> =
            self.ordered_tabs(owner, space).iter().map(|tab| (tab.id, tab.pinned)).collect();
        let active = self.active_of(owner, space);
        let at = active.and_then(|active| order.iter().position(|(id, _)| *id == active));
        let Some(at) = at else {
            return false;
        };
        let to = at as i32 + delta;
        if to < 0 || to >= order.len() as i32 {
            return false;
        }
        let to = to as usize;
        if order[at].1 != order[to].1 {
            return false;
        }
        let (first, second) = (order[at].0, order[to].0);
        let [left, right] = [first, second].map(|id| self.tabs.iter().position(|tab| tab.id == id));
        match (left, right) {
            (Some(left), Some(right)) => {
                self.tabs.swap(left, right);
                self.bump();
                true
            }
            _ => false,
        }
    }

    /// The display tab ⌘1–⌘8 (`n` = 1-based slot) or ⌘9 (`last`) selects:
    /// the n-th tab on screen, or the last one. `None` for an empty owner
    /// or an out-of-range slot.
    pub fn nth_tab(&self, owner: &Owner, space: Option<&str>, n: usize, last: bool) -> Option<TabId> {
        let order = self.ordered_tabs(owner, space);
        if order.is_empty() {
            return None;
        }
        if last {
            return order.last().map(|tab| tab.id);
        }
        if n == 0 {
            return None;
        }
        order.get(n - 1).map(|tab| tab.id)
    }

    /// Record a live page's URL and title on `id`. Answers whether either
    /// changed (the caller persists only on change). A fresh page's blank
    /// report never clobbers a real URL: when the tab already holds one,
    /// an empty or `about:blank` URL (and an empty title) is skipped, so
    /// a rebuild does not overwrite the saved page with blank.
    /// Record the page's icon URL for tab `id`; true when it changed.
    pub fn set_favicon(&mut self, id: TabId, favicon: &str) -> bool {
        match self.tabs.iter_mut().find(|tab| tab.id == id) {
            Some(tab) if tab.favicon.as_deref() != Some(favicon) && !favicon.is_empty() => {
                tab.favicon = Some(favicon.to_owned());
                true
            }
            _ => false,
        }
    }

    pub fn set_url_title(&mut self, id: TabId, url: &str, title: &str) -> bool {
        let changed = match self.tabs.iter_mut().find(|tab| tab.id == id) {
            Some(tab) => {
                let mut changed = false;
                if should_remember(url, Some(tab.url.as_str())) {
                    tab.url = url.to_owned();
                    // A navigation without a title yet reads as its URL,
                    // like [`Self::open`]: the strip never shows stale text.
                    tab.title = if title.is_empty() {
                        url.to_owned()
                    } else {
                        title.to_owned()
                    };
                    changed = true;
                } else if url == tab.url && !title.is_empty() && title != tab.title {
                    tab.title = title.to_owned();
                    changed = true;
                }
                changed
            }
            None => false,
        };
        if changed {
            self.bump();
        }
        changed
    }

    /// Stamp `id` as the just-shown page, in memory only. The stamp never
    /// makes the file dirty on its own: discard reads it from the live
    /// model, and the next real change's snapshot (or the quit flush)
    /// carries it to disk. No throttle: a stamp costs no write, so every
    /// shown frame measures the hiding from the true last show.
    pub fn mark_shown(&mut self, id: TabId, now: u64) {
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
            tab.last_shown = Some(now);
        }
    }

    /// Give every session with a stored `browser_url` and no tabs yet one
    /// session tab with that URL (its `space` is the session's project, as
    /// the older file had it; a session's tabs show in every Space).
    /// Give an older file its spaces (owner decision 2026-10-11): one space,
    /// "Personal", becomes current; every person tab moves into it, and the
    /// tabs that belonged to a project go into a folder named after that
    /// project (`project_name` resolves the id; unknown ids keep the id).
    /// Each project's remembered active tab carries over as Personal's when
    /// Personal has none yet. Answers whether anything changed; idempotent.
    pub fn ensure_spaces(&mut self, project_name: impl Fn(&str) -> Option<String>) -> bool {
        if !self.spaces.is_empty() {
            if !self.spaces.iter().any(|space| space.id == self.current_space) {
                self.current_space = self.spaces[0].id.clone();
                return true;
            }
            return false;
        }
        self.spaces.push(Space { id: PERSONAL.to_owned(), name: "Personal".to_owned(), own_signins: false });
        self.current_space = PERSONAL.to_owned();
        let mut folder_of: HashMap<String, String> = HashMap::new();
        let mut moved_active: Option<TabId> = None;
        let projects: Vec<Option<String>> =
            self.tabs.iter().filter(|tab| tab.owner == Owner::Person).map(|tab| tab.space.clone()).collect();
        for project in projects.iter().flatten() {
            if folder_of.contains_key(project) {
                continue;
            }
            let id = self.next_group_id("folder");
            let name = project_name(project).unwrap_or_else(|| project.clone());
            self.folders.push(Folder { id: id.clone(), space: PERSONAL.to_owned(), name, open: true });
            folder_of.insert(project.clone(), id);
        }
        for tab in self.tabs.iter_mut().filter(|tab| tab.owner == Owner::Person) {
            let old_key = active_key(&Owner::Person, tab.space.as_deref());
            if moved_active.is_none() && self.active.get(&old_key) == Some(&tab.id) {
                moved_active = Some(tab.id);
            }
            tab.folder = tab.space.as_ref().and_then(|project| folder_of.get(project).cloned());
            tab.space = Some(PERSONAL.to_owned());
        }
        self.active.retain(|key, _| !key.starts_with("person:"));
        if let Some(id) = moved_active {
            self.active.insert(active_key(&Owner::Person, Some(PERSONAL)), id);
        }
        self.bump();
        true
    }

    fn next_group_id(&mut self, kind: &str) -> String {
        self.next_group += 1;
        format!("{kind}-{}", self.next_group)
    }

    /// The person's folders in `space`, in list order.
    pub fn folders_in(&self, space: &str) -> Vec<&Folder> {
        self.folders.iter().filter(|folder| folder.space == space).collect()
    }

    /// A new folder in `space`, first in its list; answers its id.
    pub fn add_folder(&mut self, space: &str, name: &str) -> String {
        let id = self.next_group_id("folder");
        let at = self.folders.iter().position(|folder| folder.space == space).unwrap_or(self.folders.len());
        self.folders.insert(at, Folder { id: id.clone(), space: space.to_owned(), name: name.to_owned(), open: true });
        self.bump();
        id
    }

    /// Rename a folder; a blank name keeps the old one. False when unknown.
    pub fn rename_folder(&mut self, id: &str, name: &str) -> bool {
        let name = name.trim();
        match self.folders.iter_mut().find(|folder| folder.id == id) {
            Some(folder) if !name.is_empty() => {
                folder.name = name.to_owned();
                self.bump();
                true
            }
            _ => false,
        }
    }

    /// Delete a folder: its tabs move up into Today. False when unknown.
    pub fn delete_folder(&mut self, id: &str) -> bool {
        let Some(index) = self.folders.iter().position(|folder| folder.id == id) else {
            return false;
        };
        self.folders.remove(index);
        for tab in self.tabs.iter_mut().filter(|tab| tab.folder.as_deref() == Some(id)) {
            tab.folder = None;
        }
        self.bump();
        true
    }

    /// Open or close a folder. False when unknown.
    pub fn toggle_folder(&mut self, id: &str) -> bool {
        match self.folders.iter_mut().find(|folder| folder.id == id) {
            Some(folder) => {
                folder.open = !folder.open;
                self.bump();
                true
            }
            None => false,
        }
    }

    /// Put a tab into a folder (or back into Today with `None`). False when
    /// the tab or the folder is unknown.
    pub fn set_folder(&mut self, tab: TabId, folder: Option<&str>) -> bool {
        if folder.is_some_and(|id| !self.folders.iter().any(|f| f.id == id)) {
            return false;
        }
        match self.tabs.iter_mut().find(|t| t.id == tab) {
            Some(t) => {
                t.folder = folder.map(str::to_owned);
                self.bump();
                true
            }
            None => false,
        }
    }

    /// The person's tab ids in `space` inside `folder` (`None` = every one).
    pub fn person_ids_in(&self, space: &str, folder: Option<&str>) -> Vec<TabId> {
        self.ordered_tabs(&Owner::Person, Some(space))
            .iter()
            .filter(|tab| folder.is_none_or(|folder| tab.folder.as_deref() == Some(folder)))
            .map(|tab| tab.id)
            .collect()
    }

    /// A new space after the others, made current; answers its id. A blank
    /// name reads "Space N".
    pub fn add_space(&mut self, name: &str) -> String {
        let id = self.next_group_id("space");
        let name = if name.trim().is_empty() { format!("Space {}", self.spaces.len() + 1) } else { name.trim().to_owned() };
        self.spaces.push(Space { id: id.clone(), name, own_signins: false });
        self.current_space = id.clone();
        self.bump();
        id
    }

    /// Rename a space; a blank name keeps the old one.
    pub fn rename_space(&mut self, id: &str, name: &str) -> bool {
        let name = name.trim();
        match self.spaces.iter_mut().find(|space| space.id == id) {
            Some(space) if !name.is_empty() => {
                space.name = name.to_owned();
                self.bump();
                true
            }
            _ => false,
        }
    }

    /// Make `id` the current space. False when unknown or already current.
    pub fn switch_space(&mut self, id: &str) -> bool {
        if self.current_space == id || !self.spaces.iter().any(|space| space.id == id) {
            return false;
        }
        self.current_space = id.to_owned();
        self.bump();
        true
    }

    /// Delete a space with its folders, answering the person tabs it held
    /// (the caller closes their pages). The last space is never deleted;
    /// deleting the current one moves to the first remaining.
    pub fn delete_space(&mut self, id: &str) -> Option<Vec<TabId>> {
        if self.spaces.len() < 2 {
            return None;
        }
        let index = self.spaces.iter().position(|space| space.id == id)?;
        self.spaces.remove(index);
        self.folders.retain(|folder| folder.space != id);
        let held = self.person_ids_in(id, None);
        if self.current_space == id {
            self.current_space = self.spaces[0].id.clone();
        }
        self.bump();
        Some(held)
    }

    /// Answers whether any tab was created; the caller persists on true.
    /// Each session migrates only once ([`Tabs::migrated`]): a session
    /// whose last tab closed stays closed instead of resurrecting from a
    /// stale `browser_url`.
    pub fn migrate_from_overrides(
        &mut self,
        overrides: &crate::sessions::Overrides,
    ) -> bool {
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
            if self.migrated.contains(&session) {
                continue;
            }
            self.migrated.push(session.clone());
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
#[cfg(any(test, feature = "test-support"))]
pub fn path() -> std::path::PathBuf {
    STORE.path()
}

/// The tab model's one writer ([`crate::store::JsonStore`]). Under
/// `BAAZ_DETERMINISTIC=1` it writes nothing: a capture never touches the
/// owner's tabs.
static STORE: crate::store::JsonStore<Tabs> =
    crate::store::JsonStore::new("Browser/tabs.json").with_encoding(encode);

fn encode(tabs: &Tabs) -> Option<Vec<u8>> {
    if crate::runtime::browser_backend().in_memory() {
        return None;
    }
    serde_json::to_vec_pretty(tabs).ok()
}

/// Queue `tabs` to be written after the store's delay (750 ms): a burst of
/// changes coalesces, the newest snapshot wins, and a constant stream of
/// changes (a title that keeps changing) still lands within the delay of
/// the first.
pub fn save_soon(tabs: &Tabs) {
    STORE.save_soon(tabs.clone());
}

/// Write `tabs` now, superseding anything queued. Blocking: the quit and
/// window-close durability point, and tests.
pub fn write(tabs: &Tabs) {
    let _ = STORE.write_now(tabs);
}

/// Read the store like every store read — a missing file is an empty model
/// and migration refills it; an unparseable one is set aside and its `.bak`
/// read instead ([`crate::store::read_json`]). Under
/// `BAAZ_DETERMINISTIC=1` the store is never read: captures boot an empty
/// model. Repairs `next_id` past every id the file holds (live and
/// closed), so a hand-edited or older file can never hand out a duplicate.
/// Restored tabs read as just shown: `last_shown` lives in memory (a stamp
/// never dirties the file on its own), so a file stamp from a previous run
/// would wrongly read as long-hidden — nothing is due before its first
/// show in this run.
pub fn load() -> Tabs {
    if crate::runtime::browser_backend().in_memory() {
        return Tabs::default();
    }
    let mut tabs: Tabs = STORE.read();
    let max = tabs
        .tabs
        .iter()
        .map(|tab| tab.id.0)
        .chain(tabs.closed.iter().map(|tab| tab.id.0))
        .max();
    if let Some(max) = max {
        tabs.next_id = tabs.next_id.max(max + 1);
    }
    let now = now_ms();
    for tab in &mut tabs.tabs {
        tab.last_shown = Some(now);
    }
    forget_devtools_pages(&mut tabs);
    tabs
}

/// Tabs the DevTools front end took over before its browser was kept apart
/// (aui-cef `foreign`, owner screenshot 2026-10-11) stored `devtools://` as
/// their address: reopened, that is a disconnected DevTools page. They come
/// back as blank tabs (the page they held was overwritten), and closed ones
/// are forgotten.
fn forget_devtools_pages(tabs: &mut Tabs) {
    for tab in &mut tabs.tabs {
        if tab.url.starts_with("devtools:") {
            tab.url = BLANK.to_owned();
            tab.title = String::new();
            tab.favicon = None;
        }
    }
    tabs.closed.retain(|tab| !tab.url.starts_with("devtools:"));
}

/// Now, as ms since the epoch: the clock behind `last_shown`, download rates
/// and the discard timeout — always the wall clock (a rate read off a frozen
/// clock divides by nothing).
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

/// Now, for what the History page and the stored visits say: the wall clock,
/// frozen to a fixed day under a capture (`baaz_core::clock::now_ms`), so the
/// page's headings do not change with the day it is shot. Use this for visit
/// times, their pruning and the page's "today"; [`now_ms`] for everything
/// that measures elapsed time.
pub fn history_now_ms() -> u64 {
    baaz_core::clock::now_ms()
}

/// How long a hidden tab's page lives before discard, in seconds:
/// 30 minutes, or `BAAZ_TAB_DISCARD_SECS` when set (tests, live checks).
pub fn discard_timeout_secs() -> u64 {
    std::env::var("BAAZ_TAB_DISCARD_SECS")
        .ok()
        .and_then(|raw| raw.parse().ok())
        .filter(|secs| *secs > 0)
        .unwrap_or(30 * 60)
}

/// Whether `last_shown_ms` is idle past the timeout at `now_ms`. A tab
/// never shown (`None`) is never due: it may be brand new, and the frame
/// must not discard a page before its first show.
pub fn discard_due(now_ms: u64, last_shown_ms: Option<u64>) -> bool {
    match last_shown_ms {
        None => false,
        Some(last) => now_ms.saturating_sub(last) > discard_timeout_secs() * 1000,
    }
}

/// Whether a hidden tab's page should be discarded: idle past the timeout,
/// audibly silent, and not the agent's active tab. Pinned tabs are
/// spared by the caller, which reads the pin.
pub fn should_discard(
    now_ms: u64,
    last_shown_ms: Option<u64>,
    audible: bool,
    is_agent_active: bool,
) -> bool {
    !audible && !is_agent_active && discard_due(now_ms, last_shown_ms)
}

/// The strip row's label: `New tab` while the page is blank
/// (`about:blank`, whatever the backend reports), else the title, else the
/// URL's host (or the whole URL when it has no host) while the page has no
/// title. The aria label uses this same visible text; an empty title falls
/// back to the host.
pub fn tab_label(title: &str, url: &str) -> String {
    if url.is_empty() || url == BLANK {
        return "New tab".to_owned();
    }
    if !title.is_empty() {
        return title.to_owned();
    }
    host_of(url).unwrap_or_else(|| url.to_owned())
}

/// The tile's letter: the label's first alphanumeric, uppercased — `•`
/// when the label starts with nothing usable.
pub fn tab_initial(title: &str, url: &str) -> String {
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

/// Whether `current` is a real navigation worth persisting: non-empty, not
/// the blank default, and different from what is already stored. The poll
/// drains many times a second; only a change writes the session's row. Merely
/// showing the pane must leave the stored state untouched — `about:blank`
/// is where a fresh webview starts, not somewhere the person went.
pub fn should_remember(current: &str, stored: Option<&str>) -> bool {
    !current.is_empty() && current != BLANK && stored != Some(current)
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

    /// Owner screenshot 2026-10-11: a tab DevTools took over stored the
    /// front end's address; reopened it was a dead DevTools page. Loading
    /// turns it back into a blank tab and forgets closed ones.
    #[test]
    fn loading_forgets_devtools_pages() {
        let mut model = tabs();
        let hijacked = model.open(None, Owner::Person, "devtools://devtools/bundled/devtools_app.html", None);
        let page = model.open(None, Owner::Person, "https://a.example/", None);
        model.tabs.iter_mut().find(|tab| tab.id == hijacked).expect("tab").title = "DevTools - a.example/".to_owned();
        let mut closed = model.get(page).expect("tab").clone();
        closed.url = "devtools://devtools/x".to_owned();
        model.closed.push(closed);
        forget_devtools_pages(&mut model);
        let tab = model.get(hijacked).expect("the tab stays");
        assert_eq!((tab.url.as_str(), tab.title.as_str()), (BLANK, ""));
        assert_eq!(model.get(page).expect("tab").url, "https://a.example/", "real pages untouched");
        assert!(model.closed.is_empty(), "closed DevTools pages are forgotten");
    }

    #[test]
    fn opening_selects_and_appends_in_order() {
        let mut model = tabs();
        let first = model.open(None, session("s"), "https://a.example/", None);
        let second = model.open(None, session("s"), "https://b.example/", None);
        assert_eq!(model.active_of(&session("s"), None), Some(second));
        assert_eq!(
            model
                .tabs_of(&session("s"))
                .iter()
                .map(|tab| tab.id)
                .collect::<Vec<_>>(),
            vec![first, second]
        );
        assert!(model.activate(first));
        assert_eq!(model.active_of(&session("s"), None), Some(first));
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
        // Closing the active middle tab activates the next sibling.
        assert!(model.activate(second));
        assert_eq!(model.close(second), Some(third));
        assert_eq!(model.active_of(&session("s"), None), Some(third));
        // Closing the active last tab activates the previous sibling.
        assert_eq!(model.close(third), Some(first));
        assert_eq!(model.active_of(&session("s"), None), Some(first));
        // Closing the only tab clears the owner.
        assert_eq!(model.close(first), None);
        assert_eq!(model.active_of(&session("s"), None), None);
        // Unknown ids close nothing.
        assert_eq!(model.close(TabId(77)), None);
    }

    #[test]
    fn closing_a_background_tab_keeps_the_active_tab() {
        let mut model = tabs();
        let first = model.open(None, session("s"), "https://a.example/", None);
        let second = model.open(None, session("s"), "https://b.example/", None);
        let third = model.open(None, session("s"), "https://c.example/", None);
        assert_eq!(model.active_of(&session("s"), None), Some(third));
        // The closed background tab answers nothing: the active pointer
        // stays, so the caller rebuilds and re-registers nothing.
        assert_eq!(model.close(first), None);
        assert_eq!(model.active_of(&session("s"), None), Some(third));
        assert_eq!(model.close(second), None);
        assert_eq!(model.active_of(&session("s"), None), Some(third));
        // The neighbour rule still holds for the active tab itself.
        assert_eq!(model.close(third), None);
        assert_eq!(model.active_of(&session("s"), None), None);
    }

    #[test]
    fn person_active_tabs_are_per_project() {
        let mut model = tabs();
        let p1 = model.open(Some("p1".into()), Owner::Person, "https://a.example/", None);
        let p2 = model.open(Some("p2".into()), Owner::Person, "https://b.example/", None);
        // Each project shows its own active person tab: opening in p2 must
        // not steal p1's, and switching project flips between them.
        assert_eq!(model.active_of(&Owner::Person, Some("p1")), Some(p1));
        assert_eq!(model.active_of(&Owner::Person, Some("p2")), Some(p2));
        assert!(model.activate(p1));
        assert_eq!(model.active_of(&Owner::Person, Some("p1")), Some(p1));
        assert_eq!(model.active_of(&Owner::Person, Some("p2")), Some(p2));
        // A project with no person tabs shows none.
        assert_eq!(model.active_of(&Owner::Person, Some("p3")), None);
        assert_eq!(model.active_of(&Owner::Person, None), None);
        // Closing in one project leaves the other's active tab alone.
        assert_eq!(model.close(p1), None);
        assert_eq!(model.active_of(&Owner::Person, Some("p1")), None);
        assert_eq!(model.active_of(&Owner::Person, Some("p2")), Some(p2));
    }

    #[test]
    fn closing_keeps_owners_apart() {
        let mut model = tabs();
        let mine = model.open(None, session("s"), "https://a.example/", None);
        let other = model.open(None, session("t"), "https://b.example/", None);
        assert_eq!(model.close(mine), None);
        assert_eq!(model.active_of(&session("s"), None), None);
        assert_eq!(model.active_of(&session("t"), None), Some(other));
    }

    #[test]
    fn closed_tabs_reopen_newest_first_with_a_cap() {
        let mut model = tabs();
        let mut ids = Vec::new();
        for index in 0..(CLOSED_CAP + 5) {
            ids.push(model.open(
                None,
                session("s"),
                &format!("https://{index}.example/"),
                None,
            ));
        }
        for id in ids.iter() {
            model.close(*id);
        }
        assert_eq!(model.closed.len(), CLOSED_CAP, "reopen history is capped");
        let newest = model.reopen_last().expect("a closed tab");
        assert_eq!(
            model.get(newest).map(|tab| tab.url.as_str()),
            Some("https://24.example/")
        );
        assert_eq!(model.active_of(&session("s"), None), Some(newest));
    }

    #[test]
    fn reopen_by_id_restores_the_named_tab() {
        let mut model = tabs();
        let first = model.open(Some("p".to_owned()), Owner::Person, "https://a.example/", None);
        let second = model.open(Some("p".to_owned()), Owner::Person, "https://b.example/", None);
        model.close(first);
        model.close(second);
        assert_eq!(model.reopen(TabId(404)), None, "an unknown id reopens nothing");
        let back = model.reopen(first).expect("the named closed tab");
        assert_eq!(back, first, "identity survives the round trip");
        assert_eq!(model.get(back).map(|tab| tab.url.as_str()), Some("https://a.example/"));
        assert_eq!(model.active_of(&Owner::Person, Some("p")), Some(first));
        assert!(!model.closed.iter().any(|tab| tab.id == first), "reopened leaves the closed list");
    }

    #[test]
    fn moving_hands_the_tab_to_the_new_owner() {
        let mut model = tabs();
        let id = model.open(None, session("s"), "https://a.example/", None);
        assert!(model.move_to(id, Owner::Person, None));
        assert!(model.tabs_of(&session("s")).is_empty());
        assert_eq!(model.active_of(&session("s"), None), None);
        assert_eq!(model.active_of(&Owner::Person, None), Some(id));
        assert!(!model.move_to(TabId(404), Owner::Person, None));
    }

    /// Keep this tab (docs/82 8.3): a session's tab becomes the person's in
    /// the current space, out of the task folder, with its id, URL, title
    /// and pin — the session falls back to its next tab.
    #[test]
    fn keeping_a_session_tab_keeps_everything_but_the_owner() {
        let mut model = tabs();
        let older = model.open(Some("p".to_owned()), session("s"), "https://older.example/", None);
        let kept = model.open(Some("p".to_owned()), session("s"), "https://kept.example/", None);
        model.set_url_title(kept, "https://kept.example/page", "Kept page");
        model.set_pinned(kept, true);
        let before = model.get(kept).cloned().expect("the tab");
        assert!(model.move_to(kept, Owner::Person, Some("work")));
        let after = model.get(kept).cloned().expect("still there");
        assert_eq!(after.id, before.id, "same id: the live page and its history stay keyed to it");
        assert_eq!((after.url.as_str(), after.title.as_str(), after.pinned), ("https://kept.example/page", "Kept page", true));
        assert_eq!(after.owner, Owner::Person);
        assert_eq!(after.space.as_deref(), Some("work"), "into the person's current space");
        assert_eq!(after.folder, None);
        let task: Vec<TabId> = model.session_tabs(None).into_iter().flat_map(|(_, tabs)| tabs.into_iter().map(|t| t.id)).collect();
        assert!(!task.contains(&kept), "the task folder no longer lists it");
        assert_eq!(model.active_of(&session("s"), Some("p")), Some(older), "the session falls back to its next tab");
        assert_eq!(model.active_of(&Owner::Person, Some("work")), Some(kept), "the person's space shows it");
    }

    /// The reverse move (a hand-off): a person's tab goes to a session,
    /// leaves the person's folder, keeps id, URL and pin and its space.
    #[test]
    fn handing_a_person_tab_to_a_session_leaves_its_folder() {
        let mut model = tabs();
        let id = model.open(Some("p".to_owned()), Owner::Person, "https://a.example/", None);
        let folder = model.add_folder("p", "Reading");
        model.set_folder(id, Some(&folder));
        model.set_pinned(id, true);
        assert!(model.move_to(id, session("s"), None));
        let tab = model.get(id).expect("the tab");
        assert_eq!((tab.owner.clone(), tab.space.as_deref(), tab.folder.as_deref(), tab.pinned), (session("s"), Some("p"), None, true));
        assert_eq!(tab.url, "https://a.example/");
        assert!(model.person_ids_in("p", Some(&folder)).is_empty(), "its old folder no longer holds it");
        assert_eq!(model.active_of(&session("s"), Some("p")), Some(id));
        assert_eq!(model.active_of(&Owner::Person, Some("p")), None);
    }

    /// A kept tab survives the store: the move persists.
    #[test]
    fn a_kept_tab_round_trips_through_the_store() {
        let mut model = tabs();
        let id = model.open(Some("p".to_owned()), session("s"), "https://kept.example/", None);
        assert!(model.move_to(id, Owner::Person, Some("p")));
        let json = serde_json::to_string(&model).unwrap();
        let back: Tabs = serde_json::from_str(&json).unwrap();
        let tab = back.get(id).expect("the kept tab");
        assert_eq!((tab.owner.clone(), tab.url.as_str()), (Owner::Person, "https://kept.example/"));
        assert_eq!(back.active_of(&Owner::Person, Some("p")), Some(id));
        assert!(back.tabs_of(&session("s")).is_empty());
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
        assert!(
            !model.set_url_title(id, "https://b.example/", "B"),
            "an unchanged report is quiet"
        );
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
        let person: Vec<&str> = model
            .person_tabs(Some("p1"))
            .iter()
            .map(|tab| tab.url.as_str())
            .collect();
        assert_eq!(
            person,
            vec!["https://b.example/", "https://a.example/"],
            "pinned first"
        );
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
        std::env::remove_var("BAAZ_DETERMINISTIC");
        let before = now_ms();
        let mut model = tabs();
        let id = model.open(Some("p1".into()), session("s"), "https://a.example/", None);
        model.set_pinned(id, true);
        model.mark_shown(id, 12345);
        write(&model);
        let back = load();
        // Everything but the just-shown restamp survives the round trip:
        // a load reads restored tabs as just shown in this run.
        let mut expected = model.clone();
        for tab in &mut expected.tabs {
            tab.last_shown = back.get(tab.id).and_then(|tab| tab.last_shown);
        }
        assert_eq!(back, expected);
        let shown = back
            .get(id)
            .and_then(|tab| tab.last_shown)
            .expect("restored tabs read as shown");
        assert!(
            shown >= before,
            "the restamp is from this run, not the file's 12345"
        );
        assert_ne!(shown, 12345);
        match old {
            Some(value) => std::env::set_var("BAAZ_STATE_DIR", value),
            None => std::env::remove_var("BAAZ_STATE_DIR"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restored_tabs_read_as_just_shown() {
        let _guard = crate::store::test_env_lock();
        let old = std::env::var_os("BAAZ_STATE_DIR");
        let dir = std::env::temp_dir().join(format!("baaz-tabs-shown-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::env::set_var("BAAZ_STATE_DIR", &dir);
        std::env::remove_var("BAAZ_DETERMINISTIC");
        // A file stamp from a previous run (or a hand edit) must not read
        // as long-hidden: the load restamps to now.
        let mut model = tabs();
        let id = model.open(None, session("s"), "https://a.example/", None);
        model.mark_shown(id, 1);
        write(&model);
        let before = now_ms();
        let back = load();
        let shown = back
            .get(id)
            .and_then(|tab| tab.last_shown)
            .expect("restored tabs read as shown");
        assert!(
            shown >= before,
            "a stale file stamp becomes just shown, never discard-due"
        );
        assert!(!discard_due(
            before,
            back.get(id).and_then(|tab| tab.last_shown)
        ));
        match old {
            Some(value) => std::env::set_var("BAAZ_STATE_DIR", value),
            None => std::env::remove_var("BAAZ_STATE_DIR"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_counter_that_fell_behind_never_hands_out_a_duplicate() {
        let _guard = crate::store::test_env_lock();
        let old = std::env::var_os("BAAZ_STATE_DIR");
        let dir = std::env::temp_dir().join(format!("baaz-tabs-gen-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::env::set_var("BAAZ_STATE_DIR", &dir);
        std::env::remove_var("BAAZ_DETERMINISTIC");
        std::env::remove_var("BAAZ_DETERMINISTIC");
        let mut model = tabs();
        let live = model.open(None, session("s"), "https://a.example/", None);
        model.open(None, session("s"), "https://b.example/", None);
        model.close(live);
        // Simulate an older file whose counter fell behind its tabs.
        model.next_id = 1;
        write(&model);
        let back = load();
        assert_eq!(back.next_id, 2, "past every live and closed id");
        let fresh = {
            let mut back = back;
            back.open(None, session("s"), "https://c.example/", None)
        };
        assert_eq!(fresh, TabId(2), "the next id is never a duplicate");
        match old {
            Some(value) => std::env::set_var("BAAZ_STATE_DIR", value),
            None => std::env::remove_var("BAAZ_STATE_DIR"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn deterministic_runs_neither_read_nor_write_the_store() {
        let _guard = crate::store::test_env_lock();
        let old_dir = std::env::var_os("BAAZ_STATE_DIR");
        let old_det = std::env::var_os("BAAZ_DETERMINISTIC");
        let dir = std::env::temp_dir().join(format!("baaz-tabs-det-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::env::set_var("BAAZ_STATE_DIR", &dir);
        std::env::set_var("BAAZ_DETERMINISTIC", "1");
        // A file on disk is ignored …
        let mut seeded = tabs();
        seeded.open(None, session("s"), "https://owner.example/", None);
        std::env::remove_var("BAAZ_DETERMINISTIC");
        write(&seeded);
        std::env::set_var("BAAZ_DETERMINISTIC", "1");
        let back = load();
        assert!(
            back.tabs.is_empty(),
            "a capture never reads the owner's tabs"
        );
        // … and a save writes nothing.
        let _ = std::fs::remove_file(path());
        let mut model = tabs();
        model.open(None, session("s"), "https://capture.example/", None);
        write(&model);
        assert!(!path().exists(), "a capture never touches tabs.json");
        match old_dir {
            Some(value) => std::env::set_var("BAAZ_STATE_DIR", value),
            None => std::env::remove_var("BAAZ_STATE_DIR"),
        }
        match old_det {
            Some(value) => std::env::set_var("BAAZ_DETERMINISTIC", value),
            None => std::env::remove_var("BAAZ_DETERMINISTIC"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn migration_runs_only_once_per_session() {
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
        assert!(model.migrate_from_overrides(&overrides));
        assert_eq!(model.tabs_of(&session("s-1")).len(), 1);
        // The session's last tab closes …
        let id = model.tabs_of(&session("s-1"))[0].id;
        assert!(model.activate(id));
        assert_eq!(model.close(id), None);
        // … and the stale browser_url migrates nothing: the tab stays dead.
        assert!(!model.migrate_from_overrides(&overrides));
        assert!(model.tabs_of(&session("s-1")).is_empty());
    }
    fn names(model: &Tabs, space: &str) -> Vec<String> {
        model.folders_in(space).iter().map(|f| f.name.clone()).collect()
    }

    #[test]
    fn ensure_spaces_migrates_person_tabs_into_personal_and_is_idempotent() {
        let mut model = tabs();
        let a = model.open(Some("p1".into()), Owner::Person, "https://a.example/", None);
        let b = model.open(Some("p1".into()), Owner::Person, "https://b.example/", None);
        let c = model.open(Some("p2".into()), Owner::Person, "https://c.example/", None);
        let d = model.open(None, Owner::Person, "https://d.example/", None);
        let s = model.open(Some("p1".into()), session("s"), "https://s.example/", None);
        assert!(model.activate(a));
        let session_before = model.get(s).cloned().unwrap();
        assert!(model.ensure_spaces(|id| Some(format!("Name {id}"))));
        assert_eq!(model.spaces.len(), 1);
        assert_eq!(model.spaces[0].id, PERSONAL);
        assert_eq!(model.spaces[0].name, "Personal");
        assert_eq!(model.current_space, PERSONAL);
        assert_eq!(names(&model, PERSONAL), vec!["Name p1", "Name p2"]);
        let f1 = model.folders.iter().find(|f| f.name == "Name p1").unwrap().id.clone();
        let f2 = model.folders.iter().find(|f| f.name == "Name p2").unwrap().id.clone();
        assert_eq!(model.get(a).unwrap().folder.as_deref(), Some(f1.as_str()));
        assert_eq!(model.get(b).unwrap().folder.as_deref(), Some(f1.as_str()));
        assert_eq!(model.get(c).unwrap().folder.as_deref(), Some(f2.as_str()));
        assert_eq!(model.get(d).unwrap().folder, None);
        for id in [a, b, c, d] {
            assert_eq!(model.get(id).unwrap().space.as_deref(), Some("personal"));
        }
        assert_eq!(model.get(s).unwrap(), &session_before);
        assert_eq!(model.active_of(&Owner::Person, Some(PERSONAL)), Some(a));
        let snapshot = model.clone();
        assert!(!model.ensure_spaces(|id| Some(format!("Other {id}"))));
        assert_eq!(model, snapshot);
    }

    fn with_space() -> Tabs {
        let mut model = tabs();
        model.ensure_spaces(|_| None);
        model
    }

    #[test]
    fn folders_are_added_first_renamed_toggled_and_deleted() {
        let mut model = with_space();
        let a = model.add_folder(PERSONAL, "A");
        let b = model.add_folder(PERSONAL, "B");
        assert_ne!(a, b);
        assert_eq!(names(&model, PERSONAL), vec!["B", "A"]);
        assert!(model.rename_folder(&a, "  Alpha  "));
        assert!(!model.rename_folder(&a, "   "));
        assert!(!model.rename_folder("nope", "X"));
        assert_eq!(names(&model, PERSONAL), vec!["B", "Alpha"]);
        assert!(model.folders_in(PERSONAL)[0].open);
        assert!(model.toggle_folder(&b));
        assert!(!model.folders_in(PERSONAL)[0].open);
        assert!(!model.toggle_folder("nope"));
    }

    #[test]
    fn set_folder_and_delete_folder_move_tabs() {
        let mut model = with_space();
        let f = model.add_folder(PERSONAL, "F");
        let t1 = model.open(Some(PERSONAL.into()), Owner::Person, "https://a.example/", None);
        let t2 = model.open(Some(PERSONAL.into()), Owner::Person, "https://b.example/", None);
        assert!(!model.set_folder(t1, Some("nope")));
        assert!(!model.set_folder(TabId(999), Some(&f)));
        assert!(model.set_folder(t1, Some(&f)));
        assert_eq!(model.person_ids_in(PERSONAL, Some(&f)), vec![t1]);
        assert_eq!(model.person_ids_in(PERSONAL, None), vec![t1, t2]);
        assert!(model.set_folder(t1, None));
        assert_eq!(model.get(t1).unwrap().folder, None);
        assert!(model.set_folder(t1, Some(&f)));
        assert!(model.set_folder(t2, Some(&f)));
        assert!(model.delete_folder(&f));
        assert!(!model.delete_folder(&f));
        assert_eq!(model.get(t1).unwrap().folder, None);
        assert_eq!(model.get(t2).unwrap().folder, None);
        assert!(model.folders_in(PERSONAL).is_empty());
    }

    #[test]
    fn spaces_add_rename_switch_and_delete() {
        let mut model = with_space();
        let s2 = model.add_space("");
        assert_eq!(model.spaces[1].name, "Space 2");
        assert_eq!(model.current_space, s2);
        let s3 = model.add_space("  Trip ");
        assert_eq!(model.spaces[2].name, "Trip");
        assert_eq!(model.current_space, s3);
        assert!(model.rename_space(&s2, "Work"));
        assert!(!model.rename_space(&s2, " "));
        assert!(!model.rename_space("nope", "X"));
        assert_eq!(model.spaces[1].name, "Work");
        assert!(!model.switch_space(&s3), "already current");
        assert!(!model.switch_space("nope"));
        assert!(model.switch_space(PERSONAL));
        assert_eq!(model.current_space, PERSONAL);

        // Delete the current space: it moves to the first remaining.
        assert!(model.switch_space(&s2));
        let folder = model.add_folder(&s2, "Held");
        let kept = model.add_folder(&s3, "Kept");
        let t = model.open(Some(s2.clone()), Owner::Person, "https://a.example/", None);
        let held = model.delete_space(&s2).expect("deleted");
        assert_eq!(held, vec![t]);
        assert!(model.folders.iter().all(|f| f.id != folder));
        assert!(model.folders.iter().any(|f| f.id == kept));
        assert_eq!(model.current_space, PERSONAL);
        assert_eq!(model.delete_space("nope"), None);
        assert!(model.delete_space(&s3).is_some());
        assert_eq!(model.delete_space(PERSONAL), None, "the last space stays");
        assert_eq!(model.spaces.len(), 1);
    }

    #[test]
    fn session_tabs_without_a_space_span_every_space() {
        let mut model = tabs();
        let a = model.open(Some("x".into()), session("s1"), "https://a.example/", None);
        let b = model.open(Some("y".into()), session("s2"), "https://b.example/", None);
        model.open(Some("x".into()), Owner::Person, "https://p.example/", None);
        let all = model.session_tabs(None);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].0, "s1");
        assert_eq!(all[0].1[0].id, a);
        assert_eq!(all[1].0, "s2");
        assert_eq!(all[1].1[0].id, b);
        let only_x = model.session_tabs(Some("x"));
        assert_eq!(only_x.len(), 1);
        assert_eq!(only_x[0].0, "s1");
    }

    #[test]
    fn spaces_and_folders_round_trip_and_old_json_loads_empty() {
        let mut model = with_space();
        let f = model.add_folder(PERSONAL, "F");
        let t = model.open(Some(PERSONAL.into()), Owner::Person, "https://a.example/", None);
        model.set_folder(t, Some(&f));
        let json = serde_json::to_string(&model).unwrap();
        let back: Tabs = serde_json::from_str(&json).unwrap();
        assert_eq!(back, model);
        assert_eq!(back.get(t).unwrap().folder.as_deref(), Some(f.as_str()));

        let old = r#"{"tabs":[{"id":1,"space":"p1","owner":"Person","url":"https://a.example/","title":"A"}],"active":{},"next_id":2}"#;
        let old: Tabs = serde_json::from_str(old).unwrap();
        assert!(old.spaces.is_empty());
        assert!(old.folders.is_empty());
        assert_eq!(old.current_space, "");
        assert_eq!(old.tabs[0].folder, None);
    }


    #[test]
    fn elapsed_time_reads_the_wall_clock_while_history_may_be_frozen() {
        // Download rates and tab discards divide by `now_ms()` differences; the
        // History page's frozen capture clock must never leak into them (it
        // did, and the downloads popover lost "4.2 of 10.0 MB · 2 s left").
        let a = now_ms();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let b = now_ms();
        assert!(b - a >= 20, "wall time moved {} ms", b - a);
        assert!(history_now_ms() >= 1_700_000_000_000, "an epoch-ms clock either way");
    }
}
