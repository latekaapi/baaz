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
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex,
};

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

/// What a fresh webview opens on: the blank default, never persisted.
pub(crate) const BLANK: &str = "about:blank";

/// The map key for [`Tabs::active`]: `"person:<space>"` for the person's
/// per-project tabs (`""` with no project open) or `"session:<id>"` for a
/// session (a session already belongs to one project, so the id suffices).
/// A plain string (not the enum) so the map serialises as JSON.
pub(crate) fn active_key(owner: &Owner, space: Option<&str>) -> String {
    match owner {
        Owner::Person => format!("person:{}", space.unwrap_or("")),
        Owner::Session(id) => format!("session:{id}"),
    }
}

/// Whether the tab store must stay untouched: under
/// `BAAZ_DETERMINISTIC=1` (every capture) the tab model neither reads nor
/// writes `tabs.json` — exactly like `layout::read` returns defaults — so
/// captures are repeatable and never touch the owner's tabs. Read from the
/// environment on every call (not cached) so tests can flip it.
pub(crate) fn deterministic() -> bool {
    std::env::var("BAAZ_DETERMINISTIC").as_deref() == Ok("1")
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
    /// A background tab holds a permission ask: the sidebar draws its
    /// "needs you" dot until the tab is shown (B2a-H5).
    #[serde(default)]
    pub needs_attention: bool,
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
#[derive(Clone, Debug, Default, Eq, Serialize, Deserialize)]
pub(crate) struct Tabs {
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
    }
}

impl Tabs {
    /// Bump the model generation: every mutation calls this, so the
    /// cached sidebar's key moves on any change without comparing
    /// labels. Wraps on overflow: equality is all the key needs.
    pub(crate) fn bump(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

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
        let key = active_key(&owner, space.as_deref());
        let tab = Tab {
            id,
            space,
            owner: owner.clone(),
            url: url.to_owned(),
            title: url.to_owned(),
            pinned: false,
            last_shown: None,
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
    pub(crate) fn close(&mut self, id: TabId) -> Option<TabId> {
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
        self.closed.push(tab.clone());
        while self.closed.len() > CLOSED_CAP {
            self.closed.remove(0);
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
    pub(crate) fn reopen(&mut self, id: TabId) -> Option<TabId> {
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
    #[allow(dead_code)]
    pub(crate) fn reopen_last(&mut self) -> Option<TabId> {
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
    pub(crate) fn activate(&mut self, id: TabId) -> bool {
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
    pub(crate) fn get(&self, id: TabId) -> Option<&Tab> {
        self.tabs.iter().find(|tab| tab.id == id)
    }

    /// The owner's active tab, when it has one still open. Person tabs
    /// are per project: `space` is the project asking, and switching
    /// project shows that project's active person tab (or none).
    /// Sessions already belong to one project, so `space` is ignored
    /// for them.
    pub(crate) fn active_of(&self, owner: &Owner, space: Option<&str>) -> Option<TabId> {
        let id = self.active.get(&active_key(owner, space))?;
        self.tabs.iter().any(|tab| &tab.id == id).then_some(*id)
    }

    /// Every open tab of `owner`, in strip order.
    #[allow(dead_code)]
    pub(crate) fn tabs_of(&self, owner: &Owner) -> Vec<&Tab> {
        self.tabs.iter().filter(|tab| &tab.owner == owner).collect()
    }

    /// The ONE display ordering behind the strip, `strip_tab_at` (so
    /// `tab-select:n`/`tab-close:n` hit the n-th tab ON SCREEN), the
    /// close-neighbour choice, and future sidebars: pinned first, then
    /// model order. Person tabs are per project; sessions already belong
    /// to one project, so every session tab shows regardless of `space`.
    pub(crate) fn ordered_tabs(&self, owner: &Owner, space: Option<&str>) -> Vec<&Tab> {
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
    #[allow(dead_code)]
    pub(crate) fn person_tabs(&self, space: Option<&str>) -> Vec<&Tab> {
        self.ordered_tabs(&Owner::Person, space)
    }

    /// Every session's tabs in `space`, grouped by session id in first-seen
    /// order: Browse mode's "From sessions" (next tasks; the strip and the
    /// sync use the single-owner views above until then).
    #[allow(dead_code)]
    pub(crate) fn session_tabs(&self, space: Option<&str>) -> Vec<(String, Vec<&Tab>)> {
        let mut groups: Vec<(String, Vec<&Tab>)> = Vec::new();
        for tab in &self.tabs {
            let Owner::Session(session) = &tab.owner else {
                continue;
            };
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
        let space = self.tabs[index].space.clone();
        let siblings: Vec<TabId> = self
            .tabs
            .iter()
            .filter(|tab| tab.owner == old)
            .map(|tab| tab.id)
            .collect();
        let key = active_key(&old, space.as_deref());
        match siblings.last() {
            Some(id) => {
                self.active.insert(key, *id);
            }
            None => {
                self.active.remove(&key);
            }
        }
        self.active.insert(active_key(&owner, space.as_deref()), id);
        self.bump();
        true
    }

    /// Pin or unpin `id`, moving it to the end of the pinned block (when
    /// pinning: behind the last pinned tab) or the start of the unpinned
    /// block (when unpinning: ahead of the first unpinned tab) — so the
    /// model order already reads like the strip. False for an unknown tab.
    /// (Next tasks: the pin control on the strip.)
    #[allow(dead_code)]
    pub(crate) fn set_pinned(&mut self, id: TabId, pinned: bool) -> bool {
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
    #[allow(dead_code)]
    pub(crate) fn reorder(&mut self, id: TabId, index: usize) -> bool {
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

    /// Move `id` directly before `before` in model order, or to the end
    /// when `before` is `None`: the Browse sidebar's drag-reorder. False
    /// for an unknown `id`; an unknown `before` also lands at the end.
    /// Moving before itself is a no-op that still answers true.
    pub(crate) fn move_before(&mut self, id: TabId, before: Option<TabId>) -> bool {
        let from = match self.tabs.iter().position(|tab| tab.id == id) {
            Some(from) => from,
            None => return false,
        };
        if before == Some(id) {
            return true;
        }
        let tab = self.tabs.remove(from);
        let at = before
            .and_then(|before| self.tabs.iter().position(|tab| tab.id == before))
            .unwrap_or(self.tabs.len());
        self.tabs.insert(at, tab);
        self.bump();
        true
    }

    /// Move the owner's active tab one display slot (`delta` of -1 or 1):
    /// the Browse sidebar's ⌥⌘↑/⌥⌘↓. Display order is pinned-first, so
    /// only a neighbour in the same pinned class swaps — crossing the
    /// pinned boundary would need a (un)pin, never a silent reorder.
    /// False with no active tab, at the edge, or across the boundary.
    pub(crate) fn move_active_by(&mut self, owner: &Owner, space: Option<&str>, delta: i32) -> bool {
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
    pub(crate) fn nth_tab(&self, owner: &Owner, space: Option<&str>, n: usize, last: bool) -> Option<TabId> {
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

    /// Duplicate `id`: a copy of its URL and title, unpinned, inserted
    /// directly after it and selected. `None` for an unknown tab.
    pub(crate) fn duplicate(&mut self, id: TabId) -> Option<TabId> {
        let tab = self.tabs.iter().find(|tab| tab.id == id)?.clone();
        let fresh = TabId(self.next_id);
        self.next_id += 1;
        let at = self.tabs.iter().position(|tab| tab.id == id).map(|index| index + 1).unwrap_or(self.tabs.len());
        self.tabs.insert(
            at.min(self.tabs.len()),
            Tab {
                id: fresh,
                space: tab.space.clone(),
                owner: tab.owner.clone(),
                url: tab.url.clone(),
                title: tab.title.clone(),
                pinned: false,
                last_shown: None,
                needs_attention: false,
            },
        );
        self.active.insert(active_key(&tab.owner, tab.space.as_deref()), fresh);
        self.bump();
        Some(fresh)
    }

    /// Close every unpinned tab of `id`'s owner in `id`'s space except
    /// `id` itself: the tab menu's "Close other tabs". Pinned tabs are
    /// spared, the browser convention — a pinned tile never dies from
    /// another tab's menu. Answers how many closed, and selects `id`.
    pub(crate) fn close_others(&mut self, id: TabId) -> usize {
        let Some(tab) = self.tabs.iter().find(|tab| tab.id == id).cloned() else {
            return 0;
        };
        let others: Vec<TabId> = self
            .tabs
            .iter()
            .filter(|other| {
                other.id != id
                    && other.owner == tab.owner
                    && other.space == tab.space
                    && !other.pinned
            })
            .map(|other| other.id)
            .collect();
        let count = others.len();
        for other in others {
            self.close(other);
        }
        self.active.insert(active_key(&tab.owner, tab.space.as_deref()), id);
        self.bump();
        count
    }

    /// Record a live page's URL and title on `id`. Answers whether either
    /// changed (the caller persists only on change). A fresh page's blank
    /// report never clobbers a real URL: when the tab already holds one,
    /// an empty or `about:blank` URL (and an empty title) is skipped, so
    /// a rebuild does not overwrite the saved page with blank.
    pub(crate) fn set_url_title(&mut self, id: TabId, url: &str, title: &str) -> bool {
        let changed = match self.tabs.iter_mut().find(|tab| tab.id == id) {
            Some(tab) => {
                let mut changed = false;
                if super::should_remember(url, Some(tab.url.as_str())) {
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
    pub(crate) fn mark_shown(&mut self, id: TabId, now: u64) {
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
            tab.last_shown = Some(now);
        }
    }

    /// Give every session with a stored `browser_url` and no tabs yet one
    /// session tab with that URL (space from the session's project).
    /// Answers whether any tab was created; the caller persists on true.
    /// Each session migrates only once ([`Tabs::migrated`]): a session
    /// whose last tab closed stays closed instead of resurrecting from a
    /// stale `browser_url`.
    pub(crate) fn migrate_from_overrides(
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
pub(crate) fn path() -> std::path::PathBuf {
    crate::store::support_dir()
        .join("Browser")
        .join("tabs.json")
}

/// The next `tabs.json` snapshot generation: process-wide and monotonic,
/// so debounced saves and the quit flush order against each other even
/// across registry rebuilds (and across tests sharing the process).
static TABS_SAVE_GENERATION: AtomicU64 = AtomicU64::new(1);

/// The newest snapshot generation already on disk. The writer holds this
/// lock across the stale check and the rename, so two background saves
/// can never finish out of order with the older snapshot winning.
static TABS_LAST_WRITTEN: Mutex<u64> = Mutex::new(0);

/// Hand out the next snapshot generation for a `tabs.json` write.
pub(crate) fn next_tabs_generation() -> u64 {
    TABS_SAVE_GENERATION.fetch_add(1, Ordering::SeqCst)
}

/// Write `snapshot` as `generation`, skipping it when a newer snapshot
/// already landed: the loser of a racing pair writes nothing. Answers
/// whether the file was replaced. A no-op under `BAAZ_DETERMINISTIC=1`.
pub(crate) fn write_tabs_snapshot_generation(generation: u64, snapshot: &Tabs) -> bool {
    if deterministic() {
        return false;
    }
    let Ok(bytes) = serde_json::to_vec_pretty(snapshot) else {
        return false;
    };
    write_tabs_bytes_generation(generation, &bytes)
}

/// The byte-level ordered write behind [`write_tabs_snapshot_generation`]:
/// under one lock, skip a snapshot older than (or even with) the last
/// written, else replace the file and record the generation. Pure over
/// the bytes, so tests drive the ordering without a tab model.
pub(crate) fn write_tabs_bytes_generation(generation: u64, bytes: &[u8]) -> bool {
    if deterministic() {
        return false;
    }
    let mut last = TABS_LAST_WRITTEN
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if generation <= *last {
        return false;
    }
    if crate::store::write_atomic(&path(), bytes).is_ok() {
        *last = generation;
        true
    } else {
        false
    }
}

/// Read the store; best-effort like every store read — a missing or
/// unparseable file is an empty model, and migration refills it. Under
/// `BAAZ_DETERMINISTIC=1` the store is never read: captures boot an empty
/// model. Repairs `next_id` past every id the file holds (live and
/// closed), so a hand-edited or older file can never hand out a duplicate.
/// Restored tabs read as just shown: `last_shown` lives in memory (a stamp
/// never dirties the file on its own), so a file stamp from a previous run
/// would wrongly read as long-hidden — nothing is due before its first
/// show in this run.
pub(crate) fn load() -> Tabs {
    if deterministic() {
        return Tabs::default();
    }
    let mut tabs: Tabs = crate::store::read_json(&path());
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
    tabs
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

/// The strip row's label: `New tab` while the page is blank
/// (`about:blank`, whatever the backend reports), else the title, else the
/// URL's host (or the whole URL when it has no host) while the page has no
/// title. The aria label uses this same visible text; an empty title falls
/// back to the host.
pub(crate) fn tab_label(title: &str, url: &str) -> String {
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
        assert!(model.move_to(id, Owner::Person));
        assert!(model.tabs_of(&session("s")).is_empty());
        assert_eq!(model.active_of(&session("s"), None), None);
        assert_eq!(model.active_of(&Owner::Person, None), Some(id));
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
        write_tabs_snapshot_generation(next_tabs_generation(), &model);
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
        write_tabs_snapshot_generation(next_tabs_generation(), &model);
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
    fn an_older_snapshot_never_wins_the_file() {
        let _guard = crate::store::test_env_lock();
        let old = std::env::var_os("BAAZ_STATE_DIR");
        let old_det = std::env::var_os("BAAZ_DETERMINISTIC");
        let dir = std::env::temp_dir().join(format!("baaz-tabs-gen-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::env::set_var("BAAZ_STATE_DIR", &dir);
        std::env::remove_var("BAAZ_DETERMINISTIC");
        // Two saves racing: the newer generation lands first, the older
        // one finishing later must not clobber it.
        let older = next_tabs_generation();
        let newer = next_tabs_generation();
        assert!(older < newer);
        assert!(write_tabs_bytes_generation(
            newer,
            b"{\"tabs\":[],\"new\":true}"
        ));
        assert!(
            !write_tabs_bytes_generation(older, b"{\"tabs\":[],\"new\":false}"),
            "a stale snapshot writes nothing"
        );
        assert!(
            !write_tabs_bytes_generation(newer, b"{\"tabs\":[],\"new\":false}"),
            "even a replay writes nothing twice"
        );
        let body = std::fs::read_to_string(path()).expect("the newer snapshot landed");
        assert!(
            body.contains("\"new\":true"),
            "the older snapshot never won: {body}"
        );
        match old {
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
    fn discard_fires_only_when_idle_silent_and_unregistered() {
        let _guard = crate::store::test_env_lock();
        let old = std::env::var_os("BAAZ_TAB_DISCARD_SECS");
        std::env::set_var("BAAZ_TAB_DISCARD_SECS", "60");
        let now = 10_000_000;
        assert!(
            should_discard(now, Some(now - 61_000), false, false),
            "idle past the timeout goes"
        );
        assert!(
            !should_discard(now, Some(now - 59_000), false, false),
            "within the timeout stays"
        );
        assert!(
            !should_discard(now, None, false, false),
            "never shown stays"
        );
        assert!(
            !should_discard(now, Some(now - 61_000), true, false),
            "audible stays"
        );
        assert!(
            !should_discard(now, Some(now - 61_000), false, true),
            "the agent's tab stays"
        );
        assert!(
            !should_discard(now - 5_000, Some(now), false, false),
            "clock skew never discards"
        );
        match old {
            Some(value) => std::env::set_var("BAAZ_TAB_DISCARD_SECS", value),
            None => std::env::remove_var("BAAZ_TAB_DISCARD_SECS"),
        }
    }

    #[test]
    fn strip_labels_fall_back_to_hosts_and_initials() {
        assert_eq!(tab_label("Ledger", "https://localhost:5173/"), "Ledger");
        assert_eq!(
            tab_label("", "https://www.wikipedia.org/wiki/X"),
            "wikipedia.org"
        );
        assert_eq!(tab_label("", "https://docs.rs/"), "docs.rs");
        assert_eq!(tab_initial("", "https://docs.rs/"), "D");
    }

    #[test]
    fn a_blank_tab_reads_new_tab_whatever_the_backend_reports() {
        // The strip and the aria label share this visible text.
        assert_eq!(tab_label("", "about:blank"), "New tab");
        assert_eq!(tab_label("about:blank", "about:blank"), "New tab");
        assert_eq!(tab_label("Something", "about:blank"), "New tab");
        assert_eq!(tab_label("", ""), "New tab");
        assert_eq!(tab_initial("", "about:blank"), "N");
        // An empty title still falls back to the host on a real page.
        assert_eq!(
            tab_label("", "https://www.wikipedia.org/wiki/X"),
            "wikipedia.org"
        );
    }

    #[test]
    fn blank_reports_never_clobber_a_real_url() {
        let mut model = tabs();
        let id = model.open(None, session("s"), "https://a.example/", None);
        assert!(model.set_url_title(id, "https://a.example/", "A"));
        // A fresh page's blank report (rebuild, first paint) is skipped:
        // the saved URL and title survive, and nothing looks changed.
        assert!(!model.set_url_title(id, "about:blank", ""));
        assert!(!model.set_url_title(id, "", ""));
        let tab = model.get(id).expect("the tab");
        assert_eq!(tab.url, "https://a.example/");
        assert_eq!(tab.title, "A");
        // An empty title alone never clears a real one either.
        assert!(!model.set_url_title(id, "https://a.example/", ""));
        assert_eq!(model.get(id).expect("the tab").title, "A");
        // A real navigation still lands — and an untitled one reads as
        // its URL, like a fresh open.
        assert!(model.set_url_title(id, "https://b.example/", ""));
        let tab = model.get(id).expect("the tab");
        assert_eq!(tab.url, "https://b.example/");
        assert_eq!(tab.title, "https://b.example/");
        // A blank tab still adopts its first real page.
        let blank = model.open(None, session("s"), "about:blank", None);
        assert!(model.set_url_title(blank, "https://c.example/", "C"));
        assert_eq!(model.get(blank).expect("the tab").url, "https://c.example/");
    }

    #[test]
    fn the_display_order_is_pinned_first_then_model_order() {
        let mut model = tabs();
        let first = model.open(None, session("s"), "https://a.example/", None);
        let second = model.open(None, session("s"), "https://b.example/", None);
        let third = model.open(None, session("s"), "https://c.example/", None);
        let shown: Vec<TabId> = model
            .ordered_tabs(&session("s"), None)
            .iter()
            .map(|tab| tab.id)
            .collect();
        assert_eq!(shown, vec![first, second, third]);
        // Pinning moves the tab to the end of the pinned block …
        assert!(model.set_pinned(third, true));
        let shown: Vec<TabId> = model
            .ordered_tabs(&session("s"), None)
            .iter()
            .map(|tab| tab.id)
            .collect();
        assert_eq!(shown, vec![third, first, second]);
        assert_eq!(
            model.tabs.iter().map(|tab| tab.id).collect::<Vec<_>>(),
            vec![third, first, second],
            "the model order already reads like the strip"
        );
        assert!(model.set_pinned(first, true));
        let shown: Vec<TabId> = model
            .ordered_tabs(&session("s"), None)
            .iter()
            .map(|tab| tab.id)
            .collect();
        assert_eq!(shown, vec![third, first, second]);
        // … and unpinning moves it to the start of the unpinned block.
        assert!(model.set_pinned(third, false));
        let shown: Vec<TabId> = model
            .ordered_tabs(&session("s"), None)
            .iter()
            .map(|tab| tab.id)
            .collect();
        assert_eq!(shown, vec![first, third, second]);
        // The close neighbour follows the same on-screen order: with the
        // second tab active, closing it lands on the third.
        assert!(model.activate(second));
        assert_eq!(model.close(second), Some(third));
    }

    #[test]
    fn drag_reorder_moves_before_the_drop_row() {
        let mut model = tabs();
        let first = model.open(None, session("s"), "https://a.example/", None);
        let second = model.open(None, session("s"), "https://b.example/", None);
        let third = model.open(None, session("s"), "https://c.example/", None);
        assert!(model.move_before(third, Some(first)));
        let shown: Vec<TabId> = model
            .ordered_tabs(&session("s"), None)
            .iter()
            .map(|tab| tab.id)
            .collect();
        assert_eq!(shown, vec![third, first, second]);
        // Dropping past the end (or onto an unknown row) appends.
        assert!(model.move_before(first, None));
        let shown: Vec<TabId> = model
            .ordered_tabs(&session("s"), None)
            .iter()
            .map(|tab| tab.id)
            .collect();
        assert_eq!(shown, vec![third, second, first]);
        // Unknown drags and self-drops change nothing.
        assert!(!model.move_before(TabId(77), Some(second)));
        assert!(model.move_before(second, Some(second)));
        let shown: Vec<TabId> = model
            .ordered_tabs(&session("s"), None)
            .iter()
            .map(|tab| tab.id)
            .collect();
        assert_eq!(shown, vec![third, second, first]);
    }

    #[test]
    fn option_command_arrows_move_the_active_tab() {
        let mut model = tabs();
        let first = model.open(None, session("s"), "https://a.example/", None);
        let second = model.open(None, session("s"), "https://b.example/", None);
        let third = model.open(None, session("s"), "https://c.example/", None);
        assert!(model.activate(second));
        assert!(model.move_active_by(&session("s"), None, 1));
        let shown: Vec<TabId> = model
            .ordered_tabs(&session("s"), None)
            .iter()
            .map(|tab| tab.id)
            .collect();
        assert_eq!(shown, vec![first, third, second]);
        assert!(model.move_active_by(&session("s"), None, -1));
        let shown: Vec<TabId> = model
            .ordered_tabs(&session("s"), None)
            .iter()
            .map(|tab| tab.id)
            .collect();
        assert_eq!(shown, vec![first, second, third]);
        // At the edge there is nowhere to go.
        assert!(model.activate(first));
        assert!(!model.move_active_by(&session("s"), None, -1));
        assert!(model.activate(third));
        assert!(!model.move_active_by(&session("s"), None, 1));
        // Crossing the pinned boundary needs a (un)pin, never a move.
        assert!(model.set_pinned(first, true));
        assert!(model.activate(second));
        assert!(!model.move_active_by(&session("s"), None, -1));
    }

    #[test]
    fn command_digits_map_to_display_slots_and_nine_is_last() {
        let mut model = tabs();
        let first = model.open(None, session("s"), "https://a.example/", None);
        let second = model.open(None, session("s"), "https://b.example/", None);
        let third = model.open(None, session("s"), "https://c.example/", None);
        assert_eq!(model.nth_tab(&session("s"), None, 1, false), Some(first));
        assert_eq!(model.nth_tab(&session("s"), None, 3, false), Some(third));
        assert_eq!(model.nth_tab(&session("s"), None, 4, false), None);
        assert_eq!(model.nth_tab(&session("s"), None, 0, false), None);
        assert_eq!(model.nth_tab(&session("s"), None, 9, false), None);
        // ⌘9 is the last tab even with fewer than nine open.
        assert_eq!(model.nth_tab(&session("s"), None, 9, true), Some(third));
        assert_eq!(model.nth_tab(&session("s"), None, 1, true), Some(third));
        // Pinned tabs lead the digit order, like the strip.
        assert!(model.set_pinned(third, true));
        assert_eq!(model.nth_tab(&session("s"), None, 1, false), Some(third));
        assert_eq!(model.nth_tab(&session("s"), None, 2, false), Some(first));
        assert_eq!(model.nth_tab(&session("t"), None, 1, false), None);
        assert_eq!(model.nth_tab(&session("t"), None, 1, true), None);
        let _ = second;
    }

    #[test]
    fn duplicate_copies_and_selects_behind_the_source() {
        let mut model = tabs();
        let first = model.open(None, session("s"), "https://a.example/", None);
        assert!(model.set_url_title(first, "https://a.example/", "A"));
        let copy = model.duplicate(first).expect("a copy");
        assert_ne!(copy, first);
        let tab = model.get(copy).expect("the copy");
        assert_eq!(tab.url, "https://a.example/");
        assert_eq!(tab.title, "A");
        assert!(!tab.pinned);
        assert_eq!(model.active_of(&session("s"), None), Some(copy));
        assert!(model.duplicate(TabId(77)).is_none());
    }

    #[test]
    fn close_others_keeps_only_the_kept_tab_selected() {
        let mut model = tabs();
        let first = model.open(None, session("s"), "https://a.example/", None);
        let second = model.open(None, session("s"), "https://b.example/", None);
        let third = model.open(None, session("s"), "https://c.example/", None);
        let other = model.open(None, session("t"), "https://d.example/", None);
        assert_eq!(model.close_others(second), 2);
        assert_eq!(model.active_of(&session("s"), None), Some(second));
        assert_eq!(model.active_of(&session("t"), None), Some(other));
        // Reopen still knows the closed ones, newest first.
        assert_eq!(model.reopen_last(), Some(third));
        assert_eq!(model.close_others(TabId(77)), 0);
        let _ = first;
    }

    #[test]
    fn close_others_spares_pinned_tabs() {
        let mut model = tabs();
        let pinned = model.open(None, session("s"), "https://a.example/", None);
        let kept = model.open(None, session("s"), "https://b.example/", None);
        let doomed = model.open(None, session("s"), "https://c.example/", None);
        assert!(model.set_pinned(pinned, true));
        assert_eq!(model.close_others(kept), 1, "only the unpinned other closes");
        assert!(model.get(pinned).is_some(), "the pinned tab survives");
        assert!(model.get(doomed).is_none(), "the unpinned other closes");
        assert!(model.get(kept).is_some(), "the kept tab survives");
        assert_eq!(model.active_of(&session("s"), None), Some(kept));
    }

    #[test]
    fn mutations_bump_the_model_generation() {
        let mut model = tabs();
        assert_eq!(model.generation, 0, "a fresh model starts at zero");
        let first = model.open(None, session("s"), "https://a.example/", None);
        let second = model.open(None, session("s"), "https://b.example/", None);
        let opened = model.generation;
        assert!(opened > 0, "opening bumps the generation");
        assert!(model.activate(first));
        assert!(model.generation > opened, "selecting bumps the generation");
        let selected = model.generation;
        assert!(model.set_pinned(first, true));
        assert!(model.generation > selected, "pinning bumps the generation");
        let pinned_gen = model.generation;
        assert!(model.set_url_title(first, "https://a.example/", "A"));
        assert!(model.generation > pinned_gen, "a retitle bumps the generation");
        let retitled = model.generation;
        assert_eq!(
            model.close(second),
            None,
            "closing a background tab answers nothing — and still bumps"
        );
        assert!(model.generation > retitled, "closing bumps the generation");
        assert!(model.duplicate(first).is_some());
        assert!(model.move_before(first, None));
        assert!(model.reorder(first, 0));
        let before = model.generation;
        assert!(!model.set_url_title(first, "https://a.example/", "A"), "an unchanged report is not a change");
        assert_eq!(model.generation, before, "an unchanged report bumps nothing");
        let _ = selected;
    }

    #[test]
    fn shown_stamps_stay_in_memory_and_never_dirty() {
        // The stamp is in-memory only: it always lands, and its return
        // carries no dirty bit — the per-frame sync schedules no write
        // for a merely visible page.
        let mut model = tabs();
        let id = model.open(None, session("s"), "https://a.example/", None);
        model.mark_shown(id, 10_000);
        assert_eq!(model.get(id).and_then(|tab| tab.last_shown), Some(10_000));
        model.mark_shown(id, 10_500);
        assert_eq!(
            model.get(id).and_then(|tab| tab.last_shown),
            Some(10_500),
            "no throttle: a stamp costs no write"
        );
        // An unknown tab stamps nothing and panics nothing.
        model.mark_shown(TabId(77), 12_000);
        assert!(model.get(TabId(77)).is_none());
    }

    #[test]
    fn load_repairs_next_id_past_every_stored_tab() {
        let _guard = crate::store::test_env_lock();
        let old = std::env::var_os("BAAZ_STATE_DIR");
        let dir = std::env::temp_dir().join(format!("baaz-tabs-nextid-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::env::set_var("BAAZ_STATE_DIR", &dir);
        std::env::remove_var("BAAZ_DETERMINISTIC");
        let mut model = tabs();
        let live = model.open(None, session("s"), "https://a.example/", None);
        model.open(None, session("s"), "https://b.example/", None);
        model.close(live);
        // Simulate an older file whose counter fell behind its tabs.
        model.next_id = 1;
        write_tabs_snapshot_generation(next_tabs_generation(), &model);
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
        write_tabs_snapshot_generation(next_tabs_generation(), &seeded);
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
        write_tabs_snapshot_generation(next_tabs_generation(), &model);
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
}
