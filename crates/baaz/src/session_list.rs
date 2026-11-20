//! The session list as a model entity (task 4.7, audit R6).
//!
//! Every row the sidebar, the palettes, search and the agent tools list —
//! the joined `session/list` rows and the stores they are joined from (the
//! Muse index, Baaz's overrides, the provider-lane records, the adopted
//! projects and their branches) — with the one-per-change caches derived
//! from them: the sorted visible rows, their grouping and the handoff chain
//! index. The root ([`crate::app::Harness`]) still drives the list's
//! lifecycle (the fetch, the joins, the override edits) but holds none of
//! it: it reads through [`SessionList`]'s methods and writes inside
//! `session_list.update`.
//!
//! A change says so as a model event: [`SessionList::invalidate`] (the rows,
//! a store or a filter moved) and [`SessionList::invalidate_row`] (one row's
//! words or live state moved) emit [`SessionListEvent`] and notify this
//! entity, never the root. The sidebar pane observes it, so a list change
//! re-renders the column with no frame-time comparison and no root notify.
//!
//! What the list cannot know — which session is open, which click is
//! awaited, whose agent's terminal tasks run — arrives as [`ListFacts`] on every
//! read that depends on it; the cache keys on them, as it always did.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui::{App, Context, EventEmitter};

use crate::app::UndoBatch;
use crate::providers::ProviderId;
use crate::index::IndexEntry;
use crate::projects::Projects;
use crate::sidebar::{self, SessionEntry};
use crate::layout::LayoutGlobal as _;

/// What a list read needs from outside the list: the open session, the
/// awaited click, whether either is an unsent draft (an unsent session has
/// no row until its first send), and whose terminal tabs run a block (D52:
/// the row's hint). The root computes them; the sidebar pane holds the last
/// ones the root pushed ([`crate::sidebar_view::SidebarFacts`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ListFacts {
    /// The open session's id.
    pub(crate) active: Option<String>,
    /// Whether the open session is still an unsent draft.
    pub(crate) active_unsent: bool,
    /// The click's target while its open is in flight.
    pub(crate) pending: Option<String>,
    /// Whether that target is an unsent draft.
    pub(crate) pending_unsent: bool,
    /// Running tasks per session (docs/82 6.5): what each row's
    /// "N running" says.
    pub(crate) tasks: Vec<(String, u32)>,
}

/// What the list says changed. The sidebar pane re-renders on either; the
/// split is there for readers that only care about one row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SessionListEvent {
    /// Rows, a store the rows are joined from, or a filter moved: every
    /// derived view is stale.
    Changed,
    /// One row's words, live state or time moved and nothing it is
    /// filtered, grouped or collapsed on did (speed S4): the visible rows
    /// were patched in place and only the grouping rebuilds.
    RowChanged(String),
}

/// What `visible` was built for: the list epoch plus the open session's id
/// (the empty filter never hides the open session, so a switch changes the
/// rows) plus the click's target (the grouping rescues it past the fold, so
/// a click must rebuild) plus the grouping mode, the closed set and the
/// expanded set (a toggle regroups the same rows) plus the busy-terminal
/// counts per session (a block starting or ending restamps the hint).
type ListKey =
    (u64, Option<String>, Option<String>, crate::layout::GroupBy, Vec<String>, Vec<String>, Vec<(String, u32)>);

/// What the sidebar and the palette read out of the session list, held from
/// one change to the next instead of rebuilt per caller per frame
/// (findings `performance-5`, `support-2`, `performance-7`). Validity is
/// seven keys, not a timestamp (see [`ListKey`]), and the grouping carries
/// the minute it labelled its rows against.
#[derive(Default)]
struct ListCache {
    /// The [`ListKey`] `visible` was built for.
    key: Option<ListKey>,
    /// The sorted visible rows for that key.
    visible: Rc<Vec<SessionEntry>>,
    /// The grouping of those rows, and the minute its elapsed tags read.
    grouping: Option<(i64, Rc<sidebar::Grouping>, Rc<Vec<aui::nav::SessionSummary>>)>,
}

/// The `session/list` fetch's own state.
#[derive(Default)]
pub(crate) struct ListFetch {
    /// A `session/list` fetch is in flight: a second load meanwhile (at
    /// boot the boot session's `session/start` lands while the probe
    /// answer's fetch is still out) sets [`Self::stale`] instead of
    /// fetching — the reply issues the one follow-up. Two overlapping
    /// fetches used to apply the same reply twice, rejoining 337 rows and
    /// rebuilding the grouping and the search index for nothing.
    pub(crate) in_flight: bool,
    /// A refresh was asked for while [`Self::in_flight`]: the landing reply
    /// reloads once rather than dropping it.
    pub(crate) stale: bool,
    /// The last `session/list` this window holds ([`crate::list_cache`]):
    /// the next listing asks Muse only for rows newer than its newest.
    /// Loaded from disk by the first index read (or by the first listing,
    /// whichever runs first); `None` until then, and the listing reads the
    /// file itself.
    pub(crate) saved: Option<std::sync::Arc<crate::list_cache::ListCache<crate::list_cache::ListedRow>>>,
}

/// The session list: the rows, the stores they are joined from, the list
/// filters, the title bookkeeping, and the caches derived from them.
pub(crate) struct SessionList {
    /// Rows from `session/list`, joined with the local index.
    pub(crate) sessions: Vec<SessionEntry>,
    /// Bumped by [`Self::invalidate`] whenever anything the sidebar and the
    /// palette read out of [`Self::sessions`] changed: the rows themselves,
    /// the overrides that relabel them, the index, or the
    /// show-hidden/empty/archived flags. It is the key the list cache is
    /// validated against (findings `performance-5`, `support-2`).
    pub(crate) epoch: u64,
    /// One sorted visible list and one grouping per change, not per frame.
    cache: RefCell<ListCache>,
    /// The handoff chains over full storage, built once per `epoch` and
    /// shared by the head, member and view lookups — never rebuilt per
    /// frame or per row (Y2a3). `epoch` is the validity key: every storage
    /// change invalidates the list, which retires the index with it.
    /// Interior mutability because the readers only hold `&self`.
    chain_index: RefCell<Option<(u64, sidebar::ChainIndex)>>,
    /// How many chain indexes this run built: the once-per-change proof
    /// the Y2a3 tests read.
    pub(crate) chain_index_builds: Cell<u64>,
    /// Muse's index, shared: the per-turn re-read snapshots it with a
    /// refcount instead of a 2,600-row clone on the UI thread (speed S4).
    pub(crate) index: std::sync::Arc<HashMap<String, IndexEntry>>,
    /// Baaz's own facts about each session: its name, whether it is
    /// hidden, and the title derived from its first shell command (spec §3.7).
    pub(crate) overrides: crate::sessions::Overrides,
    /// The local record of provider-lane sessions: which provider serves
    /// each, where it ran, and when it last moved. `session/list` never
    /// names these sessions, so without this the sidebar forgets them on
    /// every restart (W5).
    pub(crate) provider_sessions: crate::provider_sessions::ProviderSessionStore,
    /// The adopted workspaces (design `docs/12-projects.md` §4).
    pub(crate) projects: Projects,
    /// The branch behind every project group row, by project id, read on
    /// each `session/list` off the UI thread.
    pub(crate) branches: HashMap<String, String>,
    /// Whether the session list has landed at least once. The sidebar draws
    /// project groups once the index has: provisional rows debut with their
    /// groups, so the collapse measures content on its first frame instead
    /// of opening from an empty box it never re-measures.
    pub(crate) sessions_loaded: bool,
    /// Whether the session index has landed at least once: row descriptions
    /// (and with them, row visibility) come from it.
    pub(crate) index_loaded: bool,
    /// The `session/list` fetch in flight, the refresh owed after it, and
    /// the saved listing it is incremental against.
    pub(crate) fetch: ListFetch,
    /// Whether hidden sessions are listed anyway (the Sessions menu's toggle).
    pub(crate) show_hidden: bool,
    /// Whether sessions with no turns are listed anyway (the Sessions menu's toggle).
    pub(crate) show_empty: bool,
    /// Whether archived sessions are listed anyway (the Sessions menu's toggle).
    pub(crate) show_archived: bool,
    /// The B4M move plan, derived from the session records: the cache
    /// computed off the UI thread by `Harness::refresh_migration_cache` and
    /// read by every render, and whether a recompute is running.
    pub(crate) migration: crate::session_migration::MigrationPlan,
    /// Real session ids with a title generation in flight: their rows read
    /// the pending placeholder until the title lands or the attempt stands
    /// down (auto-titles).
    pub(crate) titles_pending: HashSet<String>,
    /// Every throwaway title/summary side session id this run minted, plus
    /// every `side_session` override the store held at boot. The explicit
    /// record behind the hide rule: `is_side_session` reads this and the
    /// persisted flag, never the id shape (muse 1.3.0 rejects any
    /// `session/start` id that is not its own uuid shape, so side ids carry
    /// no namespace to match on).
    pub(crate) side_sessions: HashSet<String>,
    /// Real session ids with a rewrite in flight: late answers for a
    /// stood-down rewrite are hidden and ignored, never landed.
    pub(crate) byline_live: HashSet<String>,
    /// Last rewrite start per real session: the 30 s debounce clock.
    pub(crate) byline_last_start: HashMap<String, std::time::Instant>,
    /// Sessions a `session/read` has already been spent on, so a title that
    /// genuinely is not there is not asked for once a frame (finding F10).
    pub(crate) titled: HashSet<String>,
    /// Batches taken out of the list in the last few seconds, newest last:
    /// the toast's Undo. One `/hide` is a batch of one; one "Clear empty" is
    /// a batch of everything it hid; one archive confirm is a batch of one
    /// archived session. One Undo restores the whole batch.
    pub(crate) undo_stack: Vec<UndoBatch>,
}

impl EventEmitter<SessionListEvent> for SessionList {}

impl SessionList {
    /// An empty list over the provider records on disk.
    pub(crate) fn new() -> Self {
        Self {
            sessions: Vec::new(),
            epoch: 0,
            cache: RefCell::new(ListCache::default()),
            chain_index: RefCell::new(None),
            chain_index_builds: Cell::new(0),
            index: std::sync::Arc::new(HashMap::new()),
            overrides: crate::sessions::Overrides::new(),
            provider_sessions: crate::provider_sessions::read(),
            projects: Projects::default(),
            branches: HashMap::new(),
            sessions_loaded: false,
            index_loaded: false,
            fetch: ListFetch::default(),
            show_hidden: false,
            show_empty: false,
            show_archived: false,
            migration: crate::session_migration::MigrationPlan::default(),
            titles_pending: HashSet::new(),
            side_sessions: HashSet::new(),
            byline_live: HashSet::new(),
            byline_last_start: HashMap::new(),
            titled: HashSet::new(),
            undo_stack: Vec::new(),
        }
    }

    /// Drop the cached visible list and grouping: something they are
    /// derived from changed. Says so ([`SessionListEvent::Changed`]) and
    /// notifies this entity — its observers (the sidebar pane) re-render;
    /// the root is not notified.
    pub(crate) fn invalidate(&mut self, cx: &mut Context<Self>) {
        self.epoch = self.epoch.wrapping_add(1);
        Self::say(SessionListEvent::Changed, cx);
    }

    /// Say what changed: the event for the list's subscribers (the sidebar
    /// pane), and a notify of this entity — never of the root.
    fn say(event: SessionListEvent, cx: &mut Context<Self>) {
        cx.emit(event);
        cx.notify();
    }

    /// One row changed from `before` (speed S4): when nothing the list
    /// filters, groups or collapses on moved — only its words, live state
    /// or time — the cached visible list is patched in place (the row
    /// replaced, or moved to its new place when its time moved) and only
    /// the grouping is rebuilt. Anything else invalidates the whole list,
    /// as before. A finished turn used to rebuild 2,600 rows here.
    pub(crate) fn invalidate_row(&mut self, before: &SessionEntry, cx: &mut Context<Self>) {
        let Some(after) = self.sessions.iter().find(|entry| entry.id == before.id).cloned() else {
            self.invalidate(cx);
            return;
        };
        if after == *before {
            return;
        }
        let structural = before.hidden != after.hidden
            || before.archived != after.archived
            || before.pinned != after.pinned
            || before.is_empty()
            || after.is_empty()
            || before.turns_unknown != after.turns_unknown
            || before.provisional != after.provisional
            || before.side_marker != after.side_marker
            || before.project != after.project
            || before.project_name != after.project_name
            || before.workspace != after.workspace
            || before.branch != after.branch
            || sidebar::needs_collapse(&after.id, &self.overrides);
        if structural {
            self.invalidate(cx);
            return;
        }
        let id = after.id.clone();
        {
            let mut cache = self.cache.borrow_mut();
            // A stale cache rebuilds on its next read anyway.
            if cache.key.as_ref().is_some_and(|key| key.0 == self.epoch) {
                let visible = Rc::make_mut(&mut cache.visible);
                let Some(at) = visible.iter().position(|entry| entry.id == after.id) else {
                    drop(cache);
                    self.invalidate(cx);
                    return;
                };
                let mut row = after;
                row.tasks_running = visible[at].tasks_running;
                if row.updated == visible[at].updated {
                    visible[at] = row;
                } else {
                    // Newest first, after any row with the same time — the place
                    // the stable sort gives a row that was already there.
                    visible.remove(at);
                    let to = visible.partition_point(|entry| entry.updated >= row.updated);
                    visible.insert(to, row);
                }
                cache.grouping = None;
            }
        }
        Self::say(SessionListEvent::RowChanged(id), cx);
    }

    /// Which grouping the sidebar draws: the person's explicit choice, else
    /// Project (docs/83 §4, task 2.5: group by project by default). An
    /// install that never chose reads Project now even with one adopted
    /// project, where it used to read Date; a stored choice of Date stands.
    pub(crate) fn effective_group_by(cx: &App) -> crate::layout::GroupBy {
        crate::layout::Layout::get(cx).group_by.unwrap_or(crate::layout::GroupBy::Project)
    }

    /// The rows the sidebar should draw: hidden ones out unless asked for,
    /// archived ones out unless asked for, sessions with no turns out unless
    /// asked for. Text search lives in the search palette (⌘⇧F), never in a
    /// sidebar field, so no needle applies here. A still-empty open session
    /// is out too — its row arrives with its first send — unless it heads a
    /// real handoff chain.
    /// Cached: the clone and the sort run once per change, not once per
    /// caller per frame (finding `performance-5`). The sidebar pane, the
    /// sidebar's empty states, the Resume palette and the search rows all
    /// read the same `Rc`; [`Self::invalidate`] is what drops it.
    pub(crate) fn visible(&self, facts: &ListFacts, cx: &App) -> Rc<Vec<SessionEntry>> {
        let active = facts.active.clone();
        let group_by = Self::effective_group_by(cx);
        let closed = crate::layout::Layout::get(cx).closed_groups.clone();
        let expanded = crate::layout::Layout::get(cx).expanded_groups.clone();
        // The click's target rides the key: the grouping rescues it past the
        // fold, so a click (or an `open:` step) that names a held-back row
        // must rebuild the grouping on its own frame.
        let pending = facts.pending.clone();
        // Whose agent's terminal tasks run (docs/82 6.5): what stamps each
        // row's "N running" below. Part of the key, so a task starting or
        // ending rebuilds the rows without waiting for a list invalidation.
        let tasks = facts.tasks.clone();
        let mut cache = self.cache.borrow_mut();
        if cache.key.as_ref().is_some_and(
            |(epoch, id, awaited, cached_group, cached_closed, cached_expanded, cached_tasks)| {
                *epoch == self.epoch
                    && *id == active
                    && *awaited == pending
                    && *cached_group == group_by
                    && *cached_closed == closed
                    && *cached_expanded == expanded
                    && *cached_tasks == tasks
            },
        ) {
            return Rc::clone(&cache.visible);
        }
        // Y2a: the chain collapses at view time. Storage keeps every
        // member row; the collapsed head (one title, summed turns,
        // derived flags) is what the list filters, sorts and draws, so
        // the collapse is idempotent and order-independent. One index
        // build serves the collapse and both head lookups below (Y2a3).
        let index = self.chain_index();
        let collapsed = sidebar::collapse_with_index(
            self.sessions.clone(),
            &index,
            &self.overrides,
        );
        // One identity for selection: the open (or awaited) session's row
        // is never filtered as empty, so the selection always has a row
        // to stand on — a real handoff head (a chain with at least two
        // members) keeps the destination highlighted while the pack runs,
        // and a plain session keeps its row while a turn that never
        // settled leaves it at zero turns (live, 2026-10-16: the row
        // vanished mid-session when `running` read false). The one
        // exception is an unsent new session, whatever its project, which
        // has no row until its first send reveals it
        // (`SessionEntry::is_empty`): the facts' unsent flags name it, and
        // the root forgets the id the moment a turn starts.
        let exempt = |id: &str, unsent: bool| {
            let head = index.head(id);
            (index.members(&head).len() >= 2 || !unsent).then_some(head)
        };
        let active_head = active.as_deref().and_then(|id| exempt(id, facts.active_unsent));
        let pending_head = pending.as_deref().and_then(|id| exempt(id, facts.pending_unsent));
        let mut rows: Vec<SessionEntry> = collapsed
            .into_iter()
            // Show hidden reveals what the person hid, never a side session
            // or one another client made (`side_marker`): those are not
            // theirs, and there are thousands.
            .filter(|entry| (self.show_hidden && !entry.side_marker) || !entry.hidden)
            .filter(|entry| self.show_archived || !entry.archived)
            .filter(|entry| {
                // An archived row shown on request is explicitly asked for;
                // the empty filter must not swallow it back. An index-only
                // boot row's emptiness is unknown (the index knows no turn
                // count), so the empty filter leaves it alone — hidden and archived
                // still apply above, from the same overrides the real rows
                // get. Only labelled index entries ever become provisional,
                // which is what keeps the index's own zero-turn rows out.
                (self.show_archived && entry.archived)
                    || self.show_empty
                    || entry.turns_unknown
                    || !entry.is_empty()
                    || sidebar::sent_to(&entry.id, &self.overrides)
                    || active_head.as_deref() == Some(entry.id.as_str())
                    || pending_head.as_deref() == Some(entry.id.as_str())
            })
            .collect();
        // Newest first. The sidebar's grouping sorts for itself; the palette
        // takes the head of this list, so the order has to be right here.
        rows.sort_by_key(|entry| std::cmp::Reverse(entry.updated));
        // The running-task counts, stamped onto copies: the stored rows
        // stay clean of frame-rate liveness.
        if !tasks.is_empty() {
            for row in &mut rows {
                if let Some((_, count)) = tasks.iter().find(|(id, _)| *id == row.id) {
                    row.tasks_running = *count;
                }
            }
        }
        cache.key = Some((self.epoch, active, pending, group_by, closed, expanded, tasks));
        cache.visible = Rc::new(rows);
        cache.grouping = None;
        Rc::clone(&cache.visible)
    }

    /// The sidebar's grouping, built once per change and once per minute
    /// (findings `performance-5`, `performance-6`, `support-2`).
    ///
    /// Per minute because that is the finest thing a row's elapsed tag says —
    /// `now`, `14m`, `2h`, `3d` — so a cached grouping can only go stale when
    /// the minute turns. Under `BAAZ_DETERMINISTIC=1` "now" comes from the
    /// data rather than the clock, so the key is constant and a capture is
    /// identical run to run.
    pub(crate) fn grouping(&self, facts: &ListFacts, cx: &App) -> Rc<sidebar::Grouping> {
        let visible = self.drawn(facts, cx);
        let now = sidebar::grouping_now(&visible);
        let minute = now.timestamp().div_euclid(60);
        let mut cache = self.cache.borrow_mut();
        if let Some((cached, grouping, _)) = cache.grouping.as_ref() {
            if *cached == minute {
                return Rc::clone(grouping);
            }
        }
        let at = std::time::Instant::now();
        let was_boot = !self.sessions_loaded || !self.index_loaded;
        let grouping = match Self::effective_group_by(cx) {
            crate::layout::GroupBy::Date => sidebar::grouping_at(&visible, now),
            crate::layout::GroupBy::Project if self.index_loaded => {
                let closed: HashSet<String> = crate::layout::Layout::get(cx).closed_groups.iter().cloned().collect();
                let expanded: HashSet<String> =
                    crate::layout::Layout::get(cx).expanded_groups.iter().cloned().collect();
                let active = facts.active.as_deref();
                // A brand-new session has no row yet: resolve its project
                // past the rows so its group still counts as current.
                let active_project = active.and_then(|open| {
                    sidebar::rowless_session_project(open, visible.as_slice(), &self.overrides)
                });
                let view = sidebar::GroupView {
                    closed: &closed,
                    expanded: &expanded,
                    active,
                    active_project: active_project.as_deref(),
                    pending: facts.pending.as_deref(),
                };
                sidebar::grouping_by_project(
                    &visible,
                    &self.projects,
                    &self.branches,
                    &view,
                    crate::layout::Layout::get(cx),
                    now,
                )
            }
            // The index hasn't landed yet: a flat date view, exactly what
            // the window showed while loading before projects existed.
            // Once the index lands the rows are provisional but real, so
            // project groups debut with them — the collapse measures
            // content on its first frame instead of opening from an empty
            // box it never re-measures, and the grouping already has its
            // final shape when the wire rows replace them.
            crate::layout::GroupBy::Project => sidebar::grouping_at(&visible, now),
        };
        // The v2 sidebar lists pinned rows in their own section (D3).
        let (grouping, pinned) = sidebar::lift_pinned(grouping);
        let grouping = Rc::new(grouping);
        crate::log::boot_mark(&format!(
            "grouping-built rows={} boot={} in={}ms",
            visible.len(),
            was_boot as u8,
            at.elapsed().as_millis()
        ));
        cache.grouping = Some((minute, Rc::clone(&grouping), Rc::new(pinned)));
        grouping
    }

    /// The rows the sidebar draws: the visible rows, narrowed by the
    /// "Needs you" filter (task 3.4) when it is on — never what the palettes
    /// list. Toggling the filter invalidates the list, so the cached
    /// grouping rebuilds.
    fn drawn(&self, facts: &ListFacts, cx: &App) -> Rc<Vec<SessionEntry>> {
        let visible = self.visible(facts, cx);
        if crate::attention::Notifier::filter_on(cx) {
            Rc::new(crate::attention::waiting(visible.iter()).into_iter().cloned().collect::<Vec<_>>())
        } else {
            visible
        }
    }

    /// The minute the grouping's elapsed tags read against: what turns the
    /// cached grouping over. The sidebar pane's facts carry it, so the
    /// rollover re-renders the column.
    pub(crate) fn grouping_minute(&self, facts: &ListFacts, cx: &App) -> i64 {
        sidebar::grouping_now(&self.drawn(facts, cx)).timestamp().div_euclid(60)
    }

    /// The pinned sessions lifted out of [`Self::grouping`], from the same
    /// cache entry: the v2 sidebar's Pinned section.
    pub(crate) fn pinned(&self, facts: &ListFacts, cx: &App) -> Rc<Vec<aui::nav::SessionSummary>> {
        let _ = self.grouping(facts, cx);
        self.cache.borrow().grouping.as_ref().map(|(_, _, pinned)| Rc::clone(pinned)).unwrap_or_default()
    }

    /// How many listed sessions are hidden: the view menu's Show hidden count.
    pub(crate) fn hidden_count(&self) -> usize {
        self.sessions.iter().filter(|entry| entry.hidden && !entry.side_marker && !entry.archived).count()
    }

    /// Whether the sidebar still draws its skeleton rows: until the index
    /// has landed, and after that only while there is nothing to paint —
    /// the wire has not answered and no boot row exists. The boot rows
    /// (`install_provisional_rows`) exist to be painted before
    /// `session/list` lands (baa4338: rows at ~162 ms instead of ~1.2 s);
    /// the D3 screen's `loading` waited for the list again, which is a
    /// skeleton for as long as Muse's first listing takes — 1.4 s typically,
    /// 20 s once on 2026-10-21.
    pub(crate) fn loading(&self) -> bool {
        !self.index_loaded || (!self.sessions_loaded && !self.sessions.iter().any(|e| e.provisional))
    }

    /// Whether `session_id` has a stored row.
    pub(crate) fn has_row(&self, session_id: &str) -> bool {
        self.sessions.iter().any(|entry| entry.id == session_id)
    }

    /// The handoff chains over full storage, built once per storage change
    /// and shared by every lookup on this frame. The build is keyed on
    /// `epoch`: each rebuild, rejoin or override edit invalidates the
    /// list, which retires the index with it — so repeated frames and
    /// repeated rows never rebuild (Y2a3).
    /// [`Self::chain_index`] borrowed rather than cloned: a reader that
    /// asks about a few ids pays no copy of every row's head (speed S4).
    pub(crate) fn with_chain_index<R>(&self, f: impl FnOnce(&sidebar::ChainIndex) -> R) -> R {
        {
            let cache = self.chain_index.borrow();
            if let Some((epoch, index)) = cache.as_ref() {
                if *epoch == self.epoch {
                    return f(index);
                }
            }
        }
        let _ = self.chain_index();
        let cache = self.chain_index.borrow();
        f(&cache.as_ref().expect("built just above").1)
    }

    /// The chain index for this epoch, built on first read.
    pub(crate) fn chain_index(&self) -> sidebar::ChainIndex {
        let mut cache = self.chain_index.borrow_mut();
        if let Some((epoch, index)) = cache.as_ref() {
            if *epoch == self.epoch {
                return index.clone();
            }
        }
        let index = sidebar::ChainIndex::build(&self.provider_sessions, &self.overrides, &self.sessions);
        self.chain_index_builds.set(self.chain_index_builds.get() + 1);
        cache.replace((self.epoch, index.clone()));
        index
    }

    /// Follow `handoff_to` links across the session rows to the chain head
    /// (`docs/22-handoff.md` §8): any member id opens the head instead. A
    /// dangling destination resolves to the id itself, so its source stays
    /// listed and openable. Reads the cached per-change index, never a
    /// fresh walk (Y2a3).
    pub(crate) fn chain_head(&self, session_id: &str) -> String {
        self.chain_index().head(session_id)
    }

    /// Every member of `session_id`'s chain, sorted: what archiving the head
    /// covers so no member resurfaces. Reads the cached per-change index,
    /// never a fresh walk (Y2a3).
    pub(crate) fn chain_members(&self, session_id: &str) -> Vec<String> {
        self.chain_index().members(session_id)
    }
}

/// The joins: how the rows are (re)built from the stores they are joined
/// from. The root drives when (an index read, a list reply, an override
/// edit, a settled turn); the list does the work and says what changed.
impl SessionList {
    /// Re-apply the titler's ephemeral flags after a list rebuild: pending
    /// rows read pending, side sessions read hidden — even before their
    /// override writes land. A free function of the two sets (rather than
    /// `&self`) so [`Self::rejoin`] can call it mid-iteration over its own
    /// rows; the persisted `hidden` override is what carries a restart.
    pub(crate) fn apply_title_flags(pending: &HashSet<String>, sides: &HashSet<String>, entry: &mut SessionEntry) {
        if pending.contains(&entry.id) {
            entry.title_pending = true;
        }
        if sides.contains(&entry.id) {
            entry.hidden = true;
        }
    }

    /// Paint the sidebar from the local index while `session/list` is still
    /// in flight: one provisional row per labelled index entry, through
    /// [`SessionEntry::provisional`]. Only when the wire has not answered
    /// yet — the list reply replaces these wholesale
    /// ([`sidebar::merge_session_list`] keeps local rows only, and
    /// provisional rows are never local), so the end state is exactly what
    /// the wire joined. Rows already present (a local row placed at
    /// `session/start` before anything landed) keep their seat: an index
    /// entry for an id already in the list builds nothing.
    pub(crate) fn install_provisional_rows(&mut self, cx: &mut Context<Self>) {
        self.invalidate(cx);
        let mut canon = crate::projects::CanonicalCache::default();
        let projects = self.projects.clone();
        let pending = self.titles_pending.clone();
        let sides = self.side_sessions.clone();
        let mut rows = Vec::new();
        // Only local rows survive the swap below (`retain(local)`), so only
        // they keep their seat. Skipping every id already shown dropped the
        // whole sidebar on a second install — the index reconcile landing
        // before the list reply — because the first install's rows were
        // skipped here and then removed by the retain.
        let present: std::collections::HashSet<String> =
            self.sessions.iter().filter(|e| e.local).map(|e| e.id.clone()).collect();
        // The saved `session/list` first ([`crate::list_cache`]): each row
        // joins exactly as the list reply will join it, so its label — and
        // the search row built from it — does not change when the reply
        // lands. Before this, ~60 rows whose wire title differs from the
        // index label rewrote their search row on every launch. Only what
        // may have moved since the copy was saved stays at its zero value,
        // as for the index rows below: idle, no attention, no title read.
        let mut joined: std::collections::HashSet<String> = std::collections::HashSet::new();
        if let Some(listed) = self.fetch.saved.clone() {
            for session in &listed.sessions {
                if present.contains(&session.session_id) {
                    continue;
                }
                let mut entry = sidebar::SessionEntry::join_cached(
                    session,
                    self.index.get(&session.session_id),
                    self.overrides.get(&session.session_id),
                    &projects,
                    &mut canon,
                );
                entry.provisional = true;
                entry.running = false;
                entry.attention.clear();
                entry.needs_title = false;
                Self::apply_title_flags(&pending, &sides, &mut entry);
                joined.insert(entry.id.clone());
                rows.push(entry);
            }
        }
        for (session_id, index) in self.index.iter() {
            if present.contains(session_id) || joined.contains(session_id) {
                continue;
            }
            let Some(mut entry) = sidebar::SessionEntry::provisional(
                session_id,
                index,
                self.overrides.get(session_id),
                &projects,
                &mut canon,
            ) else {
                continue;
            };
            // Pending rows read pending; side sessions read hidden — the
            // same flags the joined wire rows get, before their overrides
            // land.
            Self::apply_title_flags(&pending, &sides, &mut entry);
            rows.push(entry);
        }
        // Newest first, like [`Self::visible_sessions`]: the grouping reads
        // this order, and the palette takes its head from it.
        rows.sort_by_key(|entry| std::cmp::Reverse(entry.updated));
        self.sessions.retain(|entry| entry.local);
        self.sessions.extend(rows);
        // A restart lands here before `session/list`: the provider rows
        // install from the local record now, so the sidebar shows them
        // with no wire round-trip.
        self.merge_provider_rows(cx);
    }

    /// Refresh one provider row from its record and the overrides,
    /// keeping its live facts. [`Self::rejoin`] refreshes every such row
    /// through here; the turn handlers use the one-row version directly.
    pub(crate) fn rejoin_provider_row(&mut self, session_id: &str, cx: &mut Context<Self>) {
        let Some(record) = self.provider_sessions.get(session_id).cloned() else { return };
        let Some(ix) = self.sessions.iter().position(|entry| entry.id == session_id) else { return };
        // A row its provider lists (Muse's) is the list's, labelled by the
        // list ladder in `rejoin`: the record only moves its counters — the
        // settled turn and its time — in place of the relist a settle used
        // to cost (2,661 rows joined, 64–144 ms).
        let before = self.sessions[ix].clone();
        if self.sessions[ix].provider.has(provider::ProviderTrait::NativeSessionList) {
            let entry = &mut self.sessions[ix];
            let updated = chrono::DateTime::from_timestamp_millis(record.updated_ms)
                .map(|at| at.with_timezone(&chrono::Local));
            let mut changed = false;
            if record.turns > entry.turns {
                entry.turns = record.turns;
                entry.turns_unknown = false;
                changed = true;
            }
            if let Some(updated) = updated.filter(|at| *at > entry.updated) {
                entry.updated = updated;
                changed = true;
            }
            if changed {
                self.invalidate_row(&before, cx);
            }
            return;
        }
        let Some(mut row) = sidebar::SessionEntry::provider_row(
            &record,
            self.overrides.get(session_id),
            &self.projects,
        ) else {
            return;
        };
        // The row's live facts survive the rejoin, like in
        // [`Self::merge_provider_rows`].
        row.running = self.sessions[ix].running;
        row.turn_started = self.sessions[ix].turn_started;
        row.approval_command = self.sessions[ix].approval_command.clone();
        row.pending_question = self.sessions[ix].pending_question.clone();
        row.attention = self.sessions[ix].attention.clone();
        Self::apply_title_flags(&self.titles_pending, &self.side_sessions, &mut row);
        if self.sessions[ix] != row {
            self.sessions[ix] = row;
            self.invalidate_row(&before, cx);
        }
        // A settled turn on a linked session re-derives its chain's head:
        // the one-row refresh above rebuilt member facts, and the view
        // model re-sums the chain (title, turns, flags) from them. An
        // unlinked settle stops at the O(1) refresh above (X3d).
        if sidebar::needs_collapse(session_id, &self.overrides) {
            self.collapse_handoff_chains(cx);
        }
    }

    /// Re-label the rows after the index arrives (it usually beats the wire,
    /// but the order is not guaranteed) or after an override changed.
    ///
    /// The same precedence [`SessionEntry::join`] documents, in one place.
    pub(crate) fn rejoin(&mut self, cx: &mut Context<Self>) {
        self.rejoin_scoped(None, cx);
    }

    /// [`Self::rejoin`] over the rows `only` names (`None`: every row).
    /// A scoped rejoin invalidates the visible list only when a row it
    /// touched actually changed, and resolves chains only when one of its
    /// rows is linked (speed S4: a finished turn rejoins its own row).
    pub(crate) fn rejoin_scoped(&mut self, only: Option<&std::collections::HashSet<String>>, cx: &mut Context<Self>) {
        let wanted = |id: &str| only.is_none_or(|ids| ids.contains(id));
        let before: Vec<sidebar::SessionEntry> = match only {
            None => {
                self.invalidate(cx);
                Vec::new()
            }
            Some(_) => self.sessions.iter().filter(|entry| wanted(&entry.id)).cloned().collect(),
        };
        // Provider-lane rows rejoin from their record, not from the wire
        // or the index: the generic ladder below knows neither.
        let provider_ids: Vec<String> = self
            .sessions
            .iter()
            .filter(|entry| !entry.provider.has(provider::ProviderTrait::NativeSessionList) && wanted(&entry.id))
            .map(|entry| entry.id.clone())
            .collect();
        for id in provider_ids {
            self.rejoin_provider_row(&id, cx);
        }
        // One canonicalize per distinct root for the whole pass, not one
        // per row per project: this runs on the UI thread on every index
        // load, list reply and override edit (perf audit 2026-10-16).
        let mut canon = crate::projects::CanonicalCache::default();
        // The list rows by id, for the label ladder the list join reads
        // ([`sidebar::label_ladder`]): without them a rejoin fell back to the
        // index label, uncut, and the next list reply flipped it back.
        let listed = self.fetch.saved.clone();
        let wire: HashMap<&str, &crate::list_cache::ListedRow> = listed
            .as_deref()
            .map(|l| {
                l.sessions.iter().filter(|s| wanted(&s.session_id)).map(|s| (s.session_id.as_str(), s)).collect()
            })
            .unwrap_or_default();
        for entry in &mut self.sessions {
            if !entry.provider.has(provider::ProviderTrait::NativeSessionList) || !wanted(&entry.id) {
                continue;
            }
            // The adoption may have changed under the row: re-resolve every
            // entry, including local and replayed ones, whose labels keep
            // their own rules below.
            let meta = self.overrides.get(&entry.id);
            // A missing root resolves nowhere: the row falls back to "Other
            // workspaces" while the adoption stays in the store.
            let resolved = self.projects.resolve_available_cached(
                entry.workspace.as_deref(),
                meta.and_then(|m| m.project.as_deref()),
                &mut canon,
            );
            entry.project = resolved.as_ref().map(|p| p.id.clone());
            entry.project_name = resolved.as_ref().map(|p| p.name.clone());
            if entry.local {
                // A local row predates the wire list: no index or store
                // source speaks for it, so its label ("New session", or the
                // first prompt set on `turn/started`) stands until the wire
                // lists it. Only an explicit rename or a generated title
                // overrides it — the two store sources ranked above the
                // prompt, so the row and the header that falls back to it
                // read the title the moment it lands; the flags still
                // follow the store, so pin and hide work meanwhile.
                let meta = self.overrides.get(&entry.id);
                entry.hidden = meta.is_some_and(|m| m.hidden);
                entry.pinned = meta.is_some_and(|m| m.pinned);
                entry.archived = meta.is_some_and(|m| m.archived);
                let pick = |value: Option<&str>| value.map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
                if let Some(name) = pick(meta.and_then(|m| m.name.as_deref())) {
                    entry.label = name;
                    entry.named = true;
                } else if let Some(generated) = pick(meta.and_then(|m| m.generated_title.as_deref())) {
                    entry.label = generated;
                }
                continue;
            }
            let meta = self.overrides.get(&entry.id);
            let index = self.index.get(&entry.id);
            let name = meta.and_then(|m| m.name.as_deref()).map(str::trim).filter(|s| !s.is_empty());
            let label = sidebar::label_ladder(meta, wire.get(entry.id.as_str()).copied(), index);
            // Same rule as [`SessionEntry::join`]: a user-given name always
            // earns the first prompt below it, any other label only when it
            // does not already say it.
            let user_named = name.is_some();
            let text = label.unwrap_or(crate::sidebar::UNNAMED);
            entry.needs_title = label.is_none();
            // A replayed capture names its own row by file, and no source
            // speaks for it: keep that label rather than blanking it to the
            // fallback on every override write.
            match label {
                Some(label) => entry.label = sidebar::one_line(label),
                None if !entry.replayed => entry.label = crate::sidebar::UNNAMED.to_owned(),
                None => {}
            }
            // The wire-alone mark survives the rejoin: a side started by any
            // other state dir carries no override here, so its own row flag
            // is what keeps it hidden.
            // The index may land after the row joined: a session another
            // client made is marked here too (`made_elsewhere`).
            let root = wire.get(entry.id.as_str()).and_then(|w| w.workspace_root.as_deref());
            entry.side_marker = entry.side_marker || sidebar::made_elsewhere(index, root, meta);
            entry.hidden = meta.is_some_and(|m| m.hidden) || entry.side_marker;
            entry.pinned = meta.is_some_and(|m| m.pinned);
            entry.archived = meta.is_some_and(|m| m.archived);
            // A generation in flight still reads pending after the rejoin,
            // and a side session still reads hidden — even before its
            // override write lands (the persisted `hidden` flag carries a
            // restart).
            Self::apply_title_flags(&self.titles_pending, &self.side_sessions, entry);
            // A fixture or replayed row names its own preview the way it
            // names its label: no store or index source speaks for a
            // scripted id, so a rejoin must not blank it either.
            if !entry.replayed {
                entry.description = sidebar::describe(meta, index, text, user_named);
                entry.last_ask = meta
                    .and_then(|m| m.last_ask.as_deref())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned);
                entry.last_error = meta
                    .and_then(|m| m.last_error.as_deref())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned);
                entry.unread = meta.is_some_and(|m| m.unread);
            }
            entry.named = name.is_some();
        }
        // A handoff chain is one row at view time: member rows stay in
        // storage, so a rejoin never resurfaces one — but the derived
        // head still re-reads, so invalidate when a link is in play.
        // Link-free rejoins keep the O(1) row refresh (X3d).
        let touched_link = self
            .sessions
            .iter()
            .filter(|entry| wanted(&entry.id))
            .any(|entry| sidebar::needs_collapse(&entry.id, &self.overrides));
        if touched_link {
            self.collapse_handoff_chains(cx);
        }
        if only.is_some() {
            if touched_link {
                self.invalidate(cx);
            } else {
                for row in &before {
                    self.invalidate_row(row, cx);
                }
            }
        }
    }

    /// Storage keeps every chain member row; the one-row-per-chain view
    /// is derived in [`Self::visible_sessions`](crate::app::Harness::visible_sessions)
    /// (see [`sidebar::collapse_handoff_chains`]). Kept as a named step so
    /// call sites read the same — it only invalidates when a link is in
    /// play, never removes rows. Returns whether the list changed.
    pub(crate) fn collapse_handoff_chains(&mut self, cx: &mut Context<Self>) -> bool {
        let touched = self
            .sessions
            .iter()
            .any(|entry| sidebar::needs_collapse(&entry.id, &self.overrides));
        if touched {
            self.invalidate(cx);
        }
        touched
    }

    /// Rebuild the sidebar rows the provider record owns: one row per
    /// stored session, joined with the overrides, preserving each row's
    /// live facts (a running turn, pending approvals) across the rebuild.
    /// Rows whose record is gone (deleted) leave with it. Chain members
    /// stay as rows; the one-row-per-chain view derives in
    /// `visible_sessions` (Y2a). Returns whether the list changed.
    pub(crate) fn merge_provider_rows(&mut self, cx: &mut Context<Self>) -> bool {
        // A deleted record that anchored a chain still needs the collapse:
        // its row leaves, the chain re-sums without it.
        let removed_linked = self.sessions.iter().any(|entry| {
            !entry.provider.has(provider::ProviderTrait::NativeSessionList)
                && !self.provider_sessions.contains_key(&entry.id)
                && sidebar::needs_collapse(&entry.id, &self.overrides)
        });
        let records = &self.provider_sessions;
        self.sessions.retain(|entry| entry.provider.has(provider::ProviderTrait::NativeSessionList) || records.contains_key(&entry.id));
        if self.provider_sessions.is_empty() {
            return false;
        }
        let mut changed = false;
        let mut touched_link = removed_linked;
        // A provider that lists its own sessions (Muse) owns their rows:
        // its records route reopens and hold Baaz's facts, never a row.
        let mut records: Vec<crate::provider_sessions::ProviderSessionRecord> = self
            .provider_sessions
            .values()
            .filter(|record| !ProviderId::parse_known(&record.provider).is_some_and(|id| id.has(provider::ProviderTrait::NativeSessionList)))
            .cloned()
            .collect();
        records.sort_by(|a, b| a.session_id.cmp(&b.session_id));
        for record in &records {
            touched_link |= sidebar::needs_collapse(&record.session_id, &self.overrides);
            let Some(mut row) = sidebar::SessionEntry::provider_row(
                record,
                self.overrides.get(&record.session_id),
                &self.projects,
            ) else {
                continue;
            };
            // The row's live facts survive the rebuild: a running turn,
            // pending words and the title-pending flag belong to the view
            // and the titler, not the store.
            if let Some(existing) = self.sessions.iter().find(|entry| entry.id == record.session_id) {
                row.running = existing.running;
                row.turn_started = existing.turn_started;
                row.approval_command = existing.approval_command.clone();
                row.pending_question = existing.pending_question.clone();
                row.attention = existing.attention.clone();
            }
            Self::apply_title_flags(&self.titles_pending, &self.side_sessions, &mut row);
            match self.sessions.iter().position(|entry| entry.id == record.session_id) {
                Some(ix) if self.sessions[ix] == row => {}
                Some(ix) => {
                    self.sessions[ix] = row;
                    changed = true;
                }
                None => {
                    self.sessions.push(row);
                    changed = true;
                }
            }
        }
        // Restart before the wire: a muse-lane destination exists only as
        // an override yet — synthesise its row from the local stores so
        // the chain shows with the right title before `session/list`
        // arrives (Y2a). Named by the chain title, so the empty filter
        // keeps it. Only an id whose own override carries the destination
        // half (`handoff_from`) is one: an id merely *named* by another
        // session's link is a Claude Code / Codex member whose record is
        // its row — with the record missing (a state dir copied without
        // `provider-sessions.json`) it was drawn as a "New session · now"
        // ghost once any record existed (live check of 7ff7f92).
        {
            let missing: Vec<String> = self
                .overrides
                .iter()
                .filter(|(id, meta)| {
                    meta.handoff_from.as_deref().map(str::trim).is_some_and(|from| !from.is_empty())
                        && !self.sessions.iter().any(|e| e.id == **id)
                        && sidebar::needs_collapse(id, &self.overrides)
                })
                .map(|(id, _)| id.clone())
                .collect();
            // These rows are the session-listing provider's: a member of
            // any other provider has a record, which is its row.
            let listing = ProviderId::with(provider::ProviderTrait::NativeSessionList);
            if let (false, Some(listing)) = (missing.is_empty(), listing) {
                let now = crate::clock::now_local();
                for id in missing {
                    let meta = self.overrides.get(&id);
                    let title = meta
                        .and_then(|m| m.handoff_title.clone())
                        .or_else(|| meta.and_then(|m| m.name.clone()))
                        .unwrap_or_else(|| crate::sidebar::UNNAMED.to_owned());
                    let named = meta.and_then(|m| m.handoff_title.clone()).is_some();
                    self.sessions.push(sidebar::SessionEntry {
                        id: id.clone(),
                        label: sidebar::one_line(&title),
                        updated: now,
                        created: now,
                        running: false,
                        turns: 0,
                        hidden: meta.is_some_and(|m| m.hidden),
                        pinned: meta.is_some_and(|m| m.pinned),
                        archived: meta.is_some_and(|m| m.archived),
                        description: String::new(),
                        replayed: false,
                        provider: listing,
                        named,
                        needs_title: false,
                        side_marker: false,
                        title_pending: false,
                        last_ask: None,
                        local: false,
                        provisional: false,
                        turns_unknown: false,
                        workspace: None,
                        project: meta.and_then(|m| m.project.clone()),
                        project_name: None,
                        attention: Vec::new(),
                        approval_command: None,
                        pending_question: None,
                        turn_started: None,
                        last_error: None,
                        unread: false,
                        branch: None,
                        tasks_running: 0,
                    });
                    changed = true;
                }
            }
        }
        if changed {
            self.invalidate(cx);
        }
        // A handoff chain is one row at view time: invalidate so the
        // derived head re-reads the rebuilt rows — but only when a merged
        // or removed record is link-adjacent, so link-free merges stop at
        // the row refresh (X3d).
        let collapsed = if touched_link { self.collapse_handoff_chains(cx) } else { false };
        changed | collapsed
    }
}
