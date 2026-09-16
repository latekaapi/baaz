//! The sessions sidebar (spec §3.7).
//!
//! `session/list` filtered to the workspace gives identity and timestamps;
//! `~/.local/share/muse/session-index.db` gives the words a person can read.
//! This module joins the two into the library's [`SessionSummary`] rows and
//! groups them by calendar date, which is the grouping the sidebar's date view
//! is built for.
//!
//! Everything here is pure: a list in, a [`Grouping`] out. The application owns
//! the fetching.

use std::collections::{HashMap, HashSet};

use aui_tokens::AgentState;
pub use aui::nav::Grouping;

use aui::nav::{DateGroup, ProjectGroup, SessionSummary};
use chrono::{DateTime, Datelike, Local, TimeZone, Utc};

use crate::index::IndexEntry;
use crate::layout::Layout;
use crate::projects::Projects;
use crate::sessions::SessionMeta;

/// What a session with nothing to be called is called.
///
/// The last resort, and the one Phase 4's sidebar reached fourteen times in a
/// row (finding F10). Every step before it is a real fact about the session.
pub const UNNAMED: &str = "New session";

/// One row of the sidebar, joined from the wire, the local index and the
/// harness's own overrides.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionEntry {
    /// The Muse session id.
    pub id: String,
    /// What to call it: see [`SessionEntry::join`].
    pub label: String,
    /// Last activity, as a local time.
    pub updated: DateTime<Local>,
    /// Whether this host reports the session as running.
    pub running: bool,
    /// Completed turns, shown on the meta line.
    pub turns: u64,
    /// Hidden from this window's list (`/hide`).
    pub hidden: bool,
    /// Pinned to the top of the date view, in its own group.
    pub pinned: bool,
    /// Archived out of the list (shown only from the Sessions menu).
    pub archived: bool,
    /// The muted second line: the last summary when one exists, else the
    /// first prompt — but only when the row's label does not already say it
    /// (a user-given name, or a Muse title that is not that prompt retold).
    /// Otherwise the row carries the turns meta alone, never a repeated
    /// first line.
    pub description: String,
    /// A `--replay` capture, labelled by file rather than by the index. No
    /// store source speaks for its label, so a rejoin keeps it.
    pub replayed: bool,
    /// Named with `/name` or the row's pencil. A named session with no turns
    /// is somebody's draft, not noise, so the empty filter leaves it alone.
    pub named: bool,
    /// Whether anything but the fallback was found, which is what tells the
    /// application a `session/read` is worth making (finding F10).
    pub needs_title: bool,
    /// The row's own derived workspace branch (`Session.branch`, muse 1.2.1),
    /// shown on the meta line. The live-change signal stays
    /// `session/branchChanged`; this is the index's last derived value.
    pub branch: Option<String>,
    /// Placed locally at `session/start`: the wire lists a session only
    /// after its log flushes on `turn/completed`, so the window holds this
    /// row meanwhile. [`merge_session_list`] keeps it until the wire lists
    /// its id, then the joined wire row replaces it.
    pub local: bool,
    /// The row's `workspace_root`: canonicalized when the path exists,
    /// verbatim otherwise. What project resolution reads — never the current
    /// project, so a worktree session keeps its own folder.
    pub workspace: Option<String>,
    /// The resolved project id: the stored one when it still names a
    /// project, else the adoption whose root equals [`Self::workspace`].
    /// `None` is "Other workspaces".
    pub project: Option<String>,
}

impl SessionEntry {
    /// Join one `session/list` row with what the index and the store know.
    ///
    /// The title, best first (finding F10). Since muse 1.2.1 the row itself
    /// carries what only the index used to know, so the row's own copies come
    /// before the index's — and the index stays as the fallback for older
    /// rows whose list entries predate the derivation:
    ///
    /// 1. the name someone gave it with `/name` or the row's pencil;
    /// 2. the row's own `name`;
    /// 3. the row's own `title`;
    /// 4. the row's own `first_user_prompt`;
    /// 5. the index's `session_name`;
    /// 6. the index's generated `title`;
    /// 7. the index's `first_user_prompt`;
    /// 8. the transcript's first user prompt — else its first `userShell`
    ///    command when the session has no user text at all — cached in the
    ///    store by the application after a `session/read` or straight from
    ///    the open transcript;
    /// 9. [`UNNAMED`].
    ///
    /// The session id is never a title. "Session 01a081ef" tells a person
    /// nothing they can act on, and it reads like something went wrong.
    pub fn join(
        session: &muse_client::schema::Session,
        index: Option<&IndexEntry>,
        meta: Option<&SessionMeta>,
        projects: &crate::projects::Projects,
    ) -> Self {
        fn pick(value: Option<&str>) -> Option<&str> {
            value.map(str::trim).filter(|s| !s.is_empty())
        }
        let name = pick(meta.and_then(|m| m.name.as_deref()));
        let label = name
            .or_else(|| pick(session.name.as_deref()))
            .or_else(|| pick(session.title.as_deref()))
            .or_else(|| pick(session.first_user_prompt.as_deref()))
            .or_else(|| index.and_then(IndexEntry::label))
            .or_else(|| pick(meta.and_then(|m| m.derived_title.as_deref())));
        // A user-given name always earns the first prompt below it; any other
        // label earns it only when it does not already say it (see
        // `describe`): Muse writes whole first prompts into the index title,
        // and the harness's derived title is cut from the prompt the same
        // way, so comparing the fallthrough alone misses both.
        let user_named = name.is_some()
            || index
                .and_then(|i| i.session_name.as_deref())
                .map(str::trim)
                .is_some_and(|s| !s.is_empty());
        let text = label.unwrap_or(UNNAMED);
        let workspace = session.workspace_root.as_deref().map(crate::projects::canonical_str);
        // A missing root resolves nowhere: the row falls back to "Other
        // workspaces" while the adoption stays in the store.
        let project = projects
            .resolve_available(session.workspace_root.as_deref(), meta.and_then(|m| m.project.as_deref()))
            .map(|p| p.id.clone());
        Self {
            id: session.session_id.clone(),
            label: one_line(text),
            updated: parse_time(&session.updated_at),
            running: matches!(session.status, muse_client::schema::SessionStatus::Running),
            turns: session.turn_count,
            hidden: meta.is_some_and(|m| m.hidden),
            pinned: meta.is_some_and(|m| m.pinned),
            archived: meta.is_some_and(|m| m.archived),
            description: describe(meta, index, text, user_named),
            replayed: false,
            named: name.is_some(),
            needs_title: label.is_none(),
            local: false,
            workspace,
            project,
            branch: session.branch.clone(),
        }
    }

    /// The one row a `--replay` window shows: the capture it is reading.
    ///
    /// It is labelled by the file rather than by the index, because a replayed
    /// session is not one this host ever ran and the index has nothing to say
    /// about it. The timestamp is the deterministic clock, so two runs label
    /// the row the same way (see [`grouping_now`]). Its workspace is the
    /// capture's own `workspace_root`, so it groups where a live session of
    /// that root would.
    pub fn replayed(session_id: &str, capture: &std::path::Path, projects: &crate::projects::Projects) -> Self {
        let label = capture.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "capture".to_owned());
        let workspace = replay_workspace(capture).map(|root| crate::projects::canonical_str(&root));
        let project = workspace
            .as_deref()
            .and_then(|root| projects.resolve_available(Some(root), None))
            .map(|p| p.id.clone());
        Self {
            id: session_id.to_owned(),
            label,
            updated: crate::clock::now_local(),
            running: false,
            turns: 0,
            hidden: false,
            pinned: false,
            archived: false,
            description: String::new(),
            replayed: true,
            named: false,
            needs_title: false,
            local: false,
            workspace,
            project,
            branch: None,
        }
    }

    /// Whether this row is noise: no turns yet, not running, and nobody named
    /// it. **A new session has no row at all until its first message is
    /// sent** — this holds even for the session that is currently open (owner
    /// round: v0.1 prep task 3). It used to exempt the open session so a
    /// fresh draft stayed visible while being typed into, which is what made
    /// this row noise-free under muse 1.2.1 (the wire never listed a
    /// zero-turn session at all, so the exemption never fired for anything
    /// the person had not already typed a name into). muse 1.3.0 lists a
    /// zero-turn session like any other, and `load_sessions` merges
    /// `session/list` unfiltered — so the exemption started firing on every
    /// brand-new session the moment it opened, putting an "untitled, 0
    /// turns" row in the sidebar before a single word was sent. The reveal on
    /// first send still works without the exemption: [`local_started_row`]
    /// marks its row `running: true`, which already fails this check on its
    /// own.
    pub fn is_empty(&self) -> bool {
        self.turns == 0 && !self.running && !self.named
    }

    /// The row's state dot: running sessions pulse, everything else is idle.
    fn state(&self) -> AgentState {
        if self.running {
            AgentState::Running
        } else {
            AgentState::Idle
        }
    }

    /// The library row for this session, labelled against `now`. No
    /// provider mark: the sidebar rows read title, preview and elapsed only
    /// (owner round 4, O4).
    fn summary(&self, now: DateTime<Local>) -> SessionSummary {
        let mut row = SessionSummary::new(self.id.clone(), self.label.clone(), self.state(), elapsed_at(self.updated, now));
        // The description first, so the second line reads what was done here
        // last; the turn count stays in the meta after it.
        if !self.description.is_empty() {
            row = row.meta(aui::nav::MetaItem::Text(self.description.clone().into()));
        }
        // The row's own derived branch (muse 1.2.1), ahead of the turn count:
        // which line of work this session is on.
        if let Some(branch) = self.branch.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            row = row.meta(aui::nav::MetaItem::Text(branch.to_owned().into()));
        }
        if self.turns > 0 {
            row = row.meta(aui::nav::MetaItem::Text(
                format!("{} turn{}", self.turns, if self.turns == 1 { "" } else { "s" }).into(),
            ));
        }
        if self.archived {
            row = row.meta(aui::nav::MetaItem::Tag("Archived".into()));
        }
        if self.pinned {
            row = row.pinned();
        }
        if self.running {
            row = row.pulse();
        }
        row
    }
}

/// The `workspace_root` a `--replay` capture ran in: the first `session/…`
/// line's, or `None` when it carries none.
///
/// Captures are JSON-RPC with a direction prefix (`-->` for what the client
/// sent, `<--` for what the server said). The workspace travels in the known
/// shapes — `session/start`'s params, `session/started`'s session object, a
/// `session/branchChanged` observation — and only the first `session/…` line
/// counts: a capture is one session's story, and its root is where that story
/// starts. Best-effort like every file read: an unreadable capture is a row
/// with no workspace, never an error.
pub fn replay_workspace(capture: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(capture).ok()?;
    for line in text.lines() {
        let line = line.trim().trim_start_matches("-->").trim_start_matches("<--").trim();
        let value: serde_json::Value = serde_json::from_str(line).ok()?;
        if value.get("method").and_then(|m| m.as_str()).is_none_or(|m| !m.starts_with("session/")) {
            continue;
        }
        let params = value.get("params")?;
        for shape in [
            params.pointer("/session/workspaceRoot"),
            params.pointer("/session/workspace_root"),
            params.get("workspaceRoot"),
            params.get("workspace_root"),
        ] {
            if let Some(root) = shape.and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty()) {
                return Some(root.to_owned());
            }
        }
        return None;
    }
    None
}

/// Merge a `session/list` reply with the rows the window placed locally.
///
/// A new session is listed only after its log flushes on `turn/completed`,
/// so the window holds a local row meanwhile: locals whose id the reply
/// does not contain survive (appended, still local), and a local whose id
/// is listed is dropped — the joined wire row already replaced it. The
/// visible list sorts newest-first downstream, so no order is promised here.
/// The row a `turn/started` inserts for a draft that never had one: the
/// first accepted turn makes the session real, titled from its prompt and
/// newest-dated, so the downstream newest-first sort puts it at the top of
/// its project group until the wire lists it.
pub fn local_started_row(
    session_id: &str,
    label: String,
    project: Option<String>,
    workspace: Option<String>,
    updated: DateTime<Local>,
) -> SessionEntry {
    SessionEntry {
        id: session_id.to_owned(),
        label,
        updated,
        running: true,
        turns: 0,
        hidden: false,
        pinned: false,
        archived: false,
        description: String::new(),
        replayed: false,
        named: false,
        needs_title: false,
        local: true,
        workspace,
        project,
        branch: None,
    }
}

/// What `turn/started` does to a session's row that already existed with no
/// turns — muse 1.3.0 lists a zero-turn session in `session/list` like any
/// other row (see [`SessionEntry::is_empty`]'s doc), so the draft a first
/// send makes real is, as of that muse, already a wire entry rather than the
/// rowless gap [`local_started_row`] used to fill; the same can happen to a
/// `local` placeholder row too, if one was already sitting in the list under
/// an older path. Either shape is invisible until this runs: mark it
/// running, title it from the prompt when it carries no name of its own yet,
/// and date it `now` so the newest-first sort puts it at the top of its
/// project group — the same three facts [`local_started_row`] starts a fresh
/// row with.
///
/// A session that already has turns is never a first send — it is a later
/// turn on a session whose row is already showing — so it is left untouched
/// and this returns `false`. The caller reads the return to decide whether
/// the list needs invalidating and the sidebar reveal re-arming.
pub fn first_send_update(entry: &mut SessionEntry, prompt: Option<&str>, now: DateTime<Local>) -> bool {
    if entry.turns > 0 {
        return false;
    }
    entry.running = true;
    if !entry.named {
        if let Some(prompt) = prompt.map(str::trim).filter(|p| !p.is_empty()) {
            entry.label = one_line(prompt);
        }
    }
    entry.updated = now;
    true
}

pub fn merge_session_list(wire: Vec<SessionEntry>, existing: &[SessionEntry]) -> Vec<SessionEntry> {
    let mut merged = wire;
    let listed: std::collections::HashSet<String> = merged.iter().map(|entry| entry.id.clone()).collect();
    merged.extend(existing.iter().filter(|entry| entry.local && !listed.contains(&entry.id)).cloned());
    merged
}

/// The single "now" a grouping is built against.
///
/// Under `HARNESS_DETERMINISTIC=1` it is the newest `updated` in the data, so
/// the newest row reads "now" however old the fixture is and two runs group
/// and label identically.
///
/// One clock per grouping, never one per row (findings `performance-6`,
/// `support-3`), and the window keys its grouping cache on the minute of it
/// (finding `support-2`), which is the finest thing an elapsed tag says.
pub fn grouping_now(entries: &[SessionEntry]) -> DateTime<Local> {
    if crate::clock::deterministic() {
        entries.iter().map(|e| e.updated).max().unwrap_or_else(crate::clock::now_local)
    } else {
        Local::now()
    }
}

/// Group the entries by calendar day, newest first, against an explicit
/// clock, so tests can pin it and the window's cache can hand back the clock
/// it keyed on. [`grouping_now`] is the clock a frame uses.
pub fn grouping_at(entries: &[SessionEntry], now: DateTime<Local>) -> Grouping {
    let mut sorted: Vec<&SessionEntry> = entries.iter().collect();
    sorted.sort_by_key(|e| std::cmp::Reverse(e.updated));
    let mut groups: Vec<DateGroup> = Vec::new();
    for entry in sorted {
        let label = bucket(entry.updated, now);
        match groups.last_mut() {
            Some(group) if group.label == label => group.sessions.push(entry.summary(now)),
            _ => groups.push(DateGroup::new(label, vec![entry.summary(now)])),
        }
    }
    Grouping::Date(groups)
}

/// The "Other workspaces" group id: sessions whose workspace no adoption
/// holds. Always last, muted, and closed until the person opens it.
pub const OTHER_GROUP: &str = "other";

/// How many of a project's newest unpinned sessions a folded group shows.
///
/// Pinned rows always show and never count toward this; the open session is
/// appended past it when it would otherwise be cut (owner round 2, P3).
pub const VISIBLE_RECENT: usize = 5;

/// What the project grouping shows beyond the rows: which groups stand
/// closed, which folded groups stand expanded, which session is open, and
/// which session the UI is pointed at (neither is ever held back).
pub struct GroupView<'a> {
    /// Group ids standing closed (`"other"` starts closed).
    pub closed: &'a HashSet<String>,
    /// Project-group ids whose held-back rows stand shown.
    pub expanded: &'a HashSet<String>,
    /// The open session's id, if any.
    pub active: Option<&'a str>,
    /// The click's target: `resume` names it before any view exists (and on
    /// a client-less run no view ever comes), so without the rescue below
    /// the highlight it earned would sit behind "Show N more" unseen.
    pub pending: Option<&'a str>,
}

/// Group the entries by project, against an explicit clock.
///
/// One group per adoption in [`Projects::sorted`] order — pinned first, then
/// name, never recency (owner round 4, O1) — each a plain muted label with
/// the visible-session count (an empty project still gets its row, counting
/// `"0"`), a running dot when any of its sessions runs, and, only when the
/// layout flags ask, the collapse chevron, the current-project bar and the
/// trailing branch. No coloured mark anywhere. Sessions inside run
/// newest-first with pinned rows first, each drawn exactly as the date view
/// draws it. Entries that resolve to no project land in a last muted "Other
/// workspaces" group, each row tagged with its workspace's folder name. A
/// group stands open unless its id is in `closed` — except "Other
/// workspaces", which reads the same set inverted and starts closed, so one
/// flip rule serves both.
///
/// `entries` arrive already filtered: the empty/hidden/archived filters hide
/// rows, never groups.
///
/// A project group shows its pinned rows, then the [`VISIBLE_RECENT`] most
/// recent others; the rest are held back and the group is
/// [folded](aui::nav::ProjectGroup::folded) until its id lands in `expanded`.
/// The open session (`active`) — and the click's target (`pending`) — are
/// always among the visible ones even when older than the fifth: they append
/// past the five rather than displacing a newer row. "Other workspaces"
/// never folds: it is closed until opened and usually short.
pub fn grouping_by_project(
    entries: &[SessionEntry],
    projects: &Projects,
    branches: &HashMap<String, String>,
    view: &GroupView<'_>,
    layout: &Layout,
    now: DateTime<Local>,
) -> Grouping {
    let mut by_project: HashMap<&str, Vec<&SessionEntry>> = HashMap::new();
    let mut other: Vec<&SessionEntry> = Vec::new();
    for entry in entries {
        // A missing root is no project: the row lands in "Other workspaces"
        // even when its stored id still names the adoption.
        match entry.project.as_deref().and_then(|id| projects.find_available(id)) {
            Some(project) => {
                by_project.entry(project.id.as_str()).or_default().push(entry);
            }
            None => other.push(entry),
        }
    }
    // D4: the project the open session belongs to wears the accent bar. With
    // no session open the store's current project wears it instead — that is
    // the project the header crumb names and the one a new session would land
    // in, so the bar keeps pointing at the same place either way. Either
    // falls back past a missing root, exactly as the crumb does.
    let current_project: Option<&str> = view
        .active
        .and_then(|open| entries.iter().find(|e| e.id == open))
        .and_then(|e| e.project.as_deref())
        .and_then(|id| projects.find_available(id))
        .map(|p| p.id.as_str())
        .or_else(|| projects.effective_current().map(|p| p.id.as_str()));
    let mut groups = Vec::new();
    // Missing roots get no group at all — but stay adopted, so they come
    // back when the path does.
    for project in projects.sorted_available() {
        let mut rows = by_project.remove(project.id.as_str()).unwrap_or_default();
        rows.sort_by_key(|e| (!e.pinned, std::cmp::Reverse(e.updated)));
        // Pinned rows always show and never count toward the five; the open
        // session appends past the cut when it would otherwise be held back.
        // The walk is in group order, so the rescued row keeps its place.
        let mut visible: Vec<&SessionEntry> = Vec::with_capacity(rows.len().min(VISIBLE_RECENT + 1));
        let mut recent = 0usize;
        for entry in &rows {
            if entry.pinned {
                visible.push(entry);
            } else if recent < VISIBLE_RECENT {
                visible.push(entry);
                recent += 1;
            }
        }
        if let Some(open) = view.active {
            if !visible.iter().any(|e| e.id == open) {
                if let Some(entry) = rows.iter().find(|e| e.id == open) {
                    visible.push(entry);
                }
            }
        }
        // The click's target is rescued exactly like the open session: on a
        // client-less run it never becomes the open session, but its row
        // still has to be on screen for the highlight — and the reveal — to
        // mean anything.
        if let Some(pending) = view.pending {
            if !visible.iter().any(|e| e.id == pending) {
                if let Some(entry) = rows.iter().find(|e| e.id == pending) {
                    visible.push(entry);
                }
            }
        }
        let held = rows.len().saturating_sub(visible.len());
        let is_expanded = view.expanded.contains(&project.id);
        let shown: Vec<SessionSummary> = if is_expanded {
            rows.iter().map(|e| e.summary(now)).collect()
        } else {
            visible.into_iter().map(|e| e.summary(now)).collect()
        };
        // A count says how much is inside; at zero it says only that the row
        // is empty, which the absent rows already say, and it puts a
        // meaningless digit on the same baseline as the meaningful ones
        // (audit 2026-09-13).
        let count = if rows.is_empty() { String::new() } else { rows.len().to_string() };
        // The plain default group row (owner round 4, O4): no mark, the
        // chevron, the current bar and the trailing branch only when the
        // layout flags ask. The count stays.
        let mut group = ProjectGroup::new(project.id.clone(), project.name.clone(), count)
            .chevron(layout.group_chevron)
            .current_bar(layout.group_bar);
        if layout.group_branch {
            if let Some(branch) = branches.get(&project.id) {
                group = group.trailing(branch.clone());
            }
        }
        if rows.iter().any(|e| e.running) {
            group = group.state(AgentState::Running);
        }
        if !view.closed.contains(&project.id) {
            group = group.open(shown);
        }
        if held > 0 {
            group = group.folded(held, is_expanded);
        }
        if current_project == Some(project.id.as_str()) {
            group = group.current(true);
        }
        groups.push(group);
    }
    if !other.is_empty() {
        other.sort_by_key(|e| (!e.pinned, std::cmp::Reverse(e.updated)));
        let sessions: Vec<SessionSummary> = other
            .iter()
            .map(|e| {
                let row = e.summary(now);
                match e.workspace.as_deref().map(workspace_folder) {
                    Some(folder) => row.repo(folder),
                    None => row,
                }
            })
            .collect();
        let count = sessions.len().to_string();
        let mut group = ProjectGroup::new(OTHER_GROUP, "Other workspaces", count).muted();
        if view.closed.contains(OTHER_GROUP) {
            group = group.open(sessions);
        }
        groups.push(group);
    }
    Grouping::Project(groups)
}

/// The last path component of a workspace root, for the "Other workspaces"
/// rows' repo tag — and the search palette's badge for sessions no project
/// holds. A root with no final component names itself whole rather than
/// tagging nothing.
pub(crate) fn workspace_folder(workspace: &str) -> String {
    std::path::Path::new(workspace)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| workspace.to_owned())
}

/// Which date header a moment belongs under, on the calendar and not on a
/// rolling 24 hours: something from 00:30 this morning is "Today" at 23:00.
fn bucket(at: DateTime<Local>, now: DateTime<Local>) -> &'static str {
    let days = now.date_naive().num_days_from_ce() - at.date_naive().num_days_from_ce();
    match days {
        d if d <= 0 => "Today",
        1 => "Yesterday",
        2..=6 => "This week",
        7..=30 => "This month",
        _ => "Earlier",
    }
}

/// `now`, `14m`, `2h`, `3d` — the elapsed tag at the end of a session row,
/// against an explicit clock so one frame reads it once.
fn elapsed_at(at: DateTime<Local>, now: DateTime<Local>) -> String {
    let seconds = (now - at).num_seconds().max(0);
    match seconds {
        s if s < 60 => "now".into(),
        s if s < 3_600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3_600),
        s => format!("{}d", s / 86_400),
    }
}

/// What the row's muted second line says: the summary the last completed
/// turn left behind; without one, the index's first prompt — but only when
/// the row's label is a user-given name (`user_named`) or a Muse-provided
/// title that is not a prefix or an elision of that prompt (see
/// `echoes_prompt`). Otherwise there is no second line at all: the turns
/// meta speaks for the row. The row's own cap bounds whatever is shown.
pub fn describe(
    meta: Option<&SessionMeta>,
    index: Option<&IndexEntry>,
    label: &str,
    user_named: bool,
) -> String {
    let summary = meta.and_then(|m| m.last_summary.as_deref()).map(str::trim).filter(|s| !s.is_empty());
    if let Some(summary) = summary {
        return one_line(summary);
    }
    let prompt = index.and_then(|i| i.first_user_prompt.as_deref()).map(str::trim).filter(|s| !s.is_empty());
    let Some(prompt) = prompt else { return String::new() };
    if user_named || !echoes_prompt(label, prompt) {
        return one_line(prompt);
    }
    String::new()
}

/// Whether a row label already says the first prompt: the label, normalised
/// (lowercased, whitespace collapsed, a trailing elision trimmed), matches
/// the prompt's normalised first 40 chars in full. Catches the prompt itself,
/// a Muse index title that is the whole prompt, and a derived title elided
/// from it — all three read as the same words twice when the prompt follows.
fn echoes_prompt(label: &str, prompt: &str) -> bool {
    fn norm(text: &str) -> String {
        text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
    }
    let label: String = norm(label)
        .trim_end_matches(['\u{2026}', '.', ' '])
        .chars()
        .take(40)
        .collect();
    if label.is_empty() {
        return false;
    }
    let prompt: String = norm(prompt).chars().take(40).collect();
    prompt.starts_with(&label)
}

/// An RFC3339 instant as a local time; anything unparseable is the epoch, which
/// sorts to the bottom rather than pretending to be now.
fn parse_time(rfc3339: &str) -> DateTime<Local> {
    DateTime::parse_from_rfc3339(rfc3339)
        .map(|t| t.with_timezone(&Local))
        .unwrap_or_else(|_| Local.from_utc_datetime(&DateTime::<Utc>::UNIX_EPOCH.naive_utc()))
}

/// Sidebar rows are one line: a prompt's newlines become spaces and a very long
/// one is cut where the row would truncate it anyway. The one truncation
/// convention: derived titles reuse it rather than cutting their own way.
pub(crate) fn one_line(text: &str) -> String {
    let flattened: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flattened.chars().count() <= 80 {
        return flattened;
    }
    flattened.chars().take(79).collect::<String>() + "\u{2026}"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(days_ago: i64) -> DateTime<Local> {
        Local::now() - chrono::Duration::days(days_ago)
    }

    #[test]
    fn buckets_are_calendar_days_not_rolling_hours() {
        let now = Local::now();
        assert_eq!(bucket(now, now), "Today");
        assert_eq!(bucket(at(1), now), "Yesterday");
        assert_eq!(bucket(at(3), now), "This week");
        assert_eq!(bucket(at(400), now), "Earlier");
    }

    #[test]
    fn a_prompt_becomes_a_single_line_row() {
        assert_eq!(one_line("why does\n  this  panic"), "why does this panic");
        assert_eq!(one_line(&"x".repeat(200)).chars().count(), 80);
    }

    #[test]
    fn an_unparseable_timestamp_sorts_last_rather_than_first() {
        assert!(parse_time("not a date") < Local::now() - chrono::Duration::days(365));
    }

    #[test]
    fn elapsed_labels_are_quantised_against_the_given_clock() {
        let now = Local::now();
        assert_eq!(elapsed_at(now, now), "now");
        assert_eq!(elapsed_at(now - chrono::Duration::seconds(90), now), "1m");
        assert_eq!(elapsed_at(now - chrono::Duration::hours(2), now), "2h");
        assert_eq!(elapsed_at(now - chrono::Duration::days(3), now), "3d");
    }

    #[test]
    fn grouping_against_a_fixed_clock_is_stable_run_to_run() {
        let now = Local::now();
        let mut fresh = entry("a");
        fresh.updated = now;
        let mut old = entry("b");
        old.updated = now - chrono::Duration::days(2);
        let entries = vec![old, fresh];
        // Twice against the same clock: identical grouping, newest first,
        // newest reading "now".
        let first = grouping_at(&entries, now);
        let second = grouping_at(&entries, now);
        assert_eq!(format!("{first:?}"), format!("{second:?}"));
    }

    /// What a sidebar frame used to cost at five hundred sessions, and what
    /// it costs now (findings `performance-5`, `support-2`).
    ///
    /// `render_sidebar` itself needs a window, so this times the two pure
    /// halves it is made of — the visible filter-and-sort and the grouping
    /// that builds one `SessionSummary` per row — against the cached frame,
    /// which is two `Rc` hand-backs and nothing else now that `sidebar_view`
    /// takes the grouping behind an `Rc` too (finding `performance-13`).
    /// Numbers with `--nocapture`; the assertion is only the ordering, so the
    /// test is not a timing flake.
    #[test]
    fn five_hundred_sidebar_rows_cost_less_from_the_cache() {
        const N: usize = 500;
        const FRAMES: usize = 20;
        let now = Local::now();
        let entries: Vec<SessionEntry> = (0..N)
            .map(|i| {
                let mut e = entry(&format!("s{i}"));
                e.label = format!("session number {i}");
                e.description = format!("did something to file {i}");
                e.turns = (i % 7) as u64;
                e.updated = now - chrono::Duration::minutes(i as i64 * 7);
                e
            })
            .collect();
        // Cold: what every frame did before — clone-and-sort the visible
        // list, then group it, building every row.
        let cold = std::time::Instant::now();
        for _ in 0..FRAMES {
            let mut visible: Vec<SessionEntry> = entries.iter().filter(|e| !e.hidden).cloned().collect();
            visible.sort_by_key(|e| std::cmp::Reverse(e.updated));
            std::hint::black_box(grouping_at(&visible, now));
        }
        let cold = cold.elapsed() / FRAMES as u32;
        // Warm: what a frame does now — the cached rows and the cached
        // grouping, both handed out behind `Rc` and neither of them cloned.
        let mut visible: Vec<SessionEntry> = entries.iter().filter(|e| !e.hidden).cloned().collect();
        visible.sort_by_key(|e| std::cmp::Reverse(e.updated));
        let visible = std::rc::Rc::new(visible);
        let grouping = std::rc::Rc::new(grouping_at(&visible, now));
        let warm = std::time::Instant::now();
        for _ in 0..FRAMES {
            std::hint::black_box(std::rc::Rc::clone(&visible));
            std::hint::black_box(std::rc::Rc::clone(&grouping));
        }
        let warm = warm.elapsed() / FRAMES as u32;
        eprintln!("sidebar-rows n={N} cold={cold:?}/frame warm={warm:?}/frame");
        assert!(warm < cold, "cached frame ({warm:?}) must cost less than the rebuild ({cold:?})");
    }

    /// A `session/list` row as muse 1.2.1 serves it: the index-era members
    /// plus the new derivations, each settable per test.
    fn wire_session() -> muse_client::schema::Session {
        muse_client::schema::Session {
            active_turn_id: None,
            approval_mode: None,
            attention: None,
            branch: None,
            created_at: "2026-09-13T10:00:00Z".into(),
            first_user_prompt: None,
            forked_from: None,
            last_activity_at: None,
            model_id: None,
            name: None,
            path: String::new(),
            provider_id: None,
            session_id: "s1".into(),
            status: muse_client::schema::SessionStatus::Idle,
            title: None,
            turn_count: 0,
            updated_at: "2026-09-13T10:00:00Z".into(),
            workspace_root: None,
        }
    }

    fn old_index() -> IndexEntry {
        IndexEntry {
            session_name: Some("Old Name".into()),
            title: "Old Title".into(),
            first_user_prompt: Some("Old prompt".into()),
            search_text: String::new(),
            updated_at_us: None,
            workspace_root: None,
        }
    }

    /// Owner round 2 S1: the row's own `name` / `title` / `firstUserPrompt`
    /// come before the index's copies, and the index stays the fallback for
    /// older rows that predate the derivation.
    #[test]
    fn the_rows_own_words_come_before_the_index_copies() {
        let projects = crate::projects::Projects::default();
        let index = old_index();
        // An old row: nothing of its own, the index names it.
        let old = SessionEntry::join(&wire_session(), Some(&index), None, &projects);
        assert_eq!(old.label, "Old Name");
        assert!(!old.needs_title, "the index named it");
        // A 1.2.1 row: its own name wins over the index's.
        let mut named = wire_session();
        named.name = Some("Wire Name".into());
        let entry = SessionEntry::join(&named, Some(&index), None, &projects);
        assert_eq!(entry.label, "Wire Name");
        // …its own title wins when it has no name…
        let mut titled = wire_session();
        titled.title = Some("Wire Title".into());
        let entry = SessionEntry::join(&titled, Some(&index), None, &projects);
        assert_eq!(entry.label, "Wire Title");
        // …and its own first prompt when it has neither.
        let mut prompted = wire_session();
        prompted.first_user_prompt = Some("Wire prompt".into());
        let entry = SessionEntry::join(&prompted, Some(&index), None, &projects);
        assert_eq!(entry.label, "Wire prompt");
        // The branch rides along for the row meta.
        let mut branched = wire_session();
        branched.branch = Some("feature-x".into());
        let entry = SessionEntry::join(&branched, None, None, &projects);
        assert_eq!(entry.branch.as_deref(), Some("feature-x"));
        assert_eq!(entry.label, UNNAMED);
        assert!(entry.needs_title);
    }

    fn entry(id: &str) -> SessionEntry {
        SessionEntry {
            id: id.to_owned(),
            label: "x".into(),
            updated: Local::now(),
            running: false,
            turns: 0,
            hidden: false,
            pinned: false,
            archived: false,
            description: String::new(),
            replayed: false,
            named: false,
            needs_title: false,
            local: false,
            workspace: None,
            project: None,
            branch: None,
        }
    }

    #[test]
    fn an_empty_row_is_noise_even_while_it_is_open() {
        // v0.1 prep task 3: a zero-turn unnamed session has no row, full
        // stop — being the currently open session no longer exempts it
        // (that exemption is what put a row in the sidebar for a brand-new
        // session the instant muse 1.3.0 started listing it).
        let row = entry("s");
        assert!(row.is_empty());
        let mut named = entry("n");
        named.named = true;
        assert!(!named.is_empty());
        let mut turned = entry("t");
        turned.turns = 1;
        assert!(!turned.is_empty());
        let mut running = entry("r");
        running.running = true;
        assert!(!running.is_empty());
    }

    #[test]
    fn a_first_send_inserts_a_titled_local_row() {
        let now = Local::now();
        let row = local_started_row("s-new", "Fix the header".into(), Some("p".into()), Some("/w".into()), now);
        // Titled from the prompt, local so the wire replaces it on listing,
        // carrying the draft's project, and dated now for the top of the
        // group — but never named, so the empty filter still applies
        // anywhere but the open session.
        assert_eq!(row.label, "Fix the header");
        assert!(row.local);
        assert!(!row.named);
        assert_eq!(row.project.as_deref(), Some("p"));
        assert_eq!(row.updated, now);
        assert!(!row.is_empty());
        // The wire's later listing supersedes it, as before.
        let wire = vec![entry("s-new")];
        let merged = merge_session_list(wire, &[row]);
        assert_eq!(merged.len(), 1);
        assert!(!merged[0].local);
    }

    /// muse 1.3.0's own case: `session/list` already listed the draft as a
    /// zero-turn wire row (`local: false`) before its first send, so
    /// `turn/started` finds it via `find(...)` rather than needing to insert
    /// one. It is noise (`is_empty`) until `first_send_update` runs.
    #[test]
    fn first_send_update_reveals_a_wire_zero_turn_entry() {
        let now = Local::now();
        let mut row = entry("s-wire");
        assert!(row.is_empty());
        assert!(!row.local);
        let changed = first_send_update(&mut row, Some("Fix the header"), now);
        assert!(changed);
        assert!(row.running);
        assert_eq!(row.label, "Fix the header");
        assert_eq!(row.updated, now);
        assert!(!row.is_empty());
        // Titling is not naming: the empty filter's `named` exemption still
        // reads this as a first send, not a person's own choice.
        assert!(!row.named);
    }

    /// The pre-1.3.0 shape still works the same way: a `local` placeholder
    /// row already in the list (rather than newly inserted) is revealed
    /// exactly like a wire one.
    #[test]
    fn first_send_update_reveals_a_local_entry() {
        let now = Local::now();
        let mut row = local_started_row("s-local", UNNAMED.to_owned(), None, None, now);
        row.running = false; // as it would be once parked back into `self.sessions`
        let changed = first_send_update(&mut row, Some("Fix the header"), now);
        assert!(changed);
        assert!(row.running);
        assert_eq!(row.label, "Fix the header");
        assert!(row.local);
    }

    /// A later turn on a session that already has one is never a first
    /// send: the row is left exactly as it was.
    #[test]
    fn first_send_update_leaves_a_turned_entry_untouched() {
        let now = Local::now();
        let mut row = entry("s-turned");
        row.turns = 3;
        row.label = "Existing title".into();
        let before = row.clone();
        let changed = first_send_update(&mut row, Some("Ignored prompt"), now);
        assert!(!changed);
        assert_eq!(row, before);
    }

    /// A user-given name outranks the prompt, exactly as [`SessionEntry::join`]
    /// ranks it: a first send titles an unnamed row, never renames a named one.
    #[test]
    fn first_send_update_never_overwrites_a_user_given_name() {
        let now = Local::now();
        let mut row = entry("s-named");
        row.named = true;
        row.label = "My name".into();
        first_send_update(&mut row, Some("Ignored prompt"), now);
        assert_eq!(row.label, "My name");
    }

    /// The grouping tests' roots on disk: availability hides a missing root,
    /// so fake `/work` paths would group everything into "Other workspaces".
    /// One shared base per test-binary run; `create_dir_all` is idempotent
    /// across the parallel tests that share it.
    fn roots_base() -> std::path::PathBuf {
        let base = std::env::temp_dir().join(format!("harness-sidebar-{}", std::process::id()));
        std::fs::create_dir_all(&base).expect("temp roots");
        base
    }

    fn project(id: &str, name: &str, colour: u8, pinned: bool) -> crate::projects::Project {
        let root = roots_base().join(id);
        std::fs::create_dir_all(&root).expect("temp root");
        crate::projects::Project {
            id: id.to_owned(),
            root,
            name: name.to_owned(),
            colour,
            pinned,
            added_at: "2026-09-13T10:00:00Z".into(),
            last_opened_at: "2026-09-13T10:00:00Z".into(),
            defaults: crate::projects::ProjectDefaults::default(),
        }
    }

    fn grouped_projects() -> crate::projects::Projects {
        let mut store = crate::projects::Projects::default();
        store.projects = vec![
            project("p-harness", "harness", 1, false),
            project("p-agentic", "agentic-ui", 2, false),
            project("p-empty", "empty", 3, false),
        ];
        store
    }

    fn grouped_entry(id: &str, project: Option<&str>, workspace: Option<&str>, days_ago: i64) -> SessionEntry {
        let mut e = entry(id);
        e.label = format!("session {id}");
        e.project = project.map(str::to_owned);
        e.workspace = workspace.map(str::to_owned);
        e.updated = Local::now() - chrono::Duration::days(days_ago);
        e.turns = 1;
        e
    }

    fn by_project(entries: &[SessionEntry], projects: &crate::projects::Projects) -> Grouping {
        by_project_view(entries, projects, &HashSet::new(), &HashSet::new(), None)
    }

    fn by_project_view(
        entries: &[SessionEntry],
        projects: &crate::projects::Projects,
        closed: &HashSet<String>,
        expanded: &HashSet<String>,
        active: Option<&str>,
    ) -> Grouping {
        by_project_layout(entries, projects, closed, expanded, active, &crate::layout::Layout::default())
    }

    fn by_project_layout(
        entries: &[SessionEntry],
        projects: &crate::projects::Projects,
        closed: &HashSet<String>,
        expanded: &HashSet<String>,
        active: Option<&str>,
        layout: &crate::layout::Layout,
    ) -> Grouping {
        let view = GroupView { closed, expanded, active, pending: None };
        grouping_by_project(entries, projects, &HashMap::new(), &view, layout, Local::now())
    }

    /// Nine sessions in one project, newest first: `s0` is today, `s8` eight
    /// days ago.
    fn nine_sessions(project: &str) -> Vec<SessionEntry> {
        (0..9)
            .map(|i| grouped_entry(&format!("s{i}"), Some(project), Some("/work/p-big"), i as i64))
            .collect()
    }

    fn folded_group<'a>(groups: &'a [ProjectGroup], id: &str) -> &'a ProjectGroup {
        groups.iter().find(|g| g.id.as_ref() == id).expect("the group")
    }

    #[test]
    fn a_missing_root_gets_no_group_and_its_sessions_land_in_other() {
        let mut projects = grouped_projects();
        // Adopted, then deleted: never created on disk.
        let gone = crate::projects::Project {
            id: "p-gone".to_owned(),
            root: roots_base().join("p-gone"),
            name: "gone".to_owned(),
            colour: 4,
            pinned: false,
            added_at: "2026-09-13T10:00:00Z".into(),
            last_opened_at: "2026-09-13T10:00:00Z".into(),
            defaults: crate::projects::ProjectDefaults::default(),
        };
        assert!(!gone.root.is_dir(), "the test root must stay missing");
        projects.projects.push(gone);
        let entries = vec![
            grouped_entry("s1", Some("p-harness"), Some("ws"), 0),
            grouped_entry("s2", Some("p-gone"), Some("ws"), 0),
        ];
        let Grouping::Project(groups) = by_project(&entries, &projects) else {
            panic!("project grouping must yield project groups");
        };
        // No group for the missing root — but it stays adopted.
        assert!(groups.iter().all(|g| g.id.as_ref() != "p-gone"));
        assert!(projects.find("p-gone").is_some());
        // Its session is not lost: it falls back to "Other workspaces" with
        // the harness session.
        let harness = folded_group(&groups, "p-harness");
        assert_eq!(harness.sessions.len(), 1);
        let other = groups.last().expect("other group");
        assert_eq!(other.id.as_ref(), OTHER_GROUP);
        assert_eq!(other.count.as_ref(), "1");
    }

    #[test]
    fn three_projects_and_two_strays_group_into_four() {
        let projects = grouped_projects();
        let entries = vec![
            grouped_entry("s1", Some("p-harness"), Some("/work/p-harness"), 0),
            grouped_entry("s2", Some("p-harness"), Some("/work/p-harness"), 2),
            grouped_entry("s3", Some("p-agentic"), Some("/work/p-agentic"), 1),
            grouped_entry("s4", None, Some("/tmp/stray-one"), 0),
            grouped_entry("s5", None, Some("/tmp/stray-two"), 3),
        ];
        let Grouping::Project(groups) = by_project(&entries, &projects) else {
            panic!("project grouping must yield project groups");
        };
        // Name order, never recency (owner round 4, O1): agentic-ui, empty,
        // harness — even though the harness rows are the newest — then Other
        // workspaces last.
        assert_eq!(groups.len(), 4);
        assert_eq!(groups[0].id.as_ref(), "p-agentic");
        assert_eq!(groups[1].id.as_ref(), "p-empty");
        assert_eq!(groups[2].id.as_ref(), "p-harness");
        assert_eq!(groups[2].count.as_ref(), "2");
        // No count at all rather than "0": the pill says how much is inside,
        // and at zero the absent rows already say it (audit 2026-09-13).
        assert_eq!(groups[1].count.as_ref(), "", "an empty project carries no count");
        assert!(groups[1].open, "an empty project still gets an open group row");
        let other = &groups[3];
        assert_eq!(other.id.as_ref(), OTHER_GROUP);
        assert_eq!(other.name.as_ref(), "Other workspaces");
        assert!(other.muted);
        assert!(!other.open, "Other workspaces starts closed");
        assert_eq!(other.count.as_ref(), "2");
        assert!(other.sessions.is_empty(), "a closed group counts its rows without carrying them");
        // No coloured marks anywhere: every group row is the plain default.
        assert!(groups.iter().all(|g| g.mark.is_none()), "no group row carries a mark");
        // Newest first inside the group.
        let harness = &groups[2];
        assert_eq!(harness.sessions[0].id.as_ref(), "s1");
        assert_eq!(harness.sessions[1].id.as_ref(), "s2");
    }

    /// Sessions that resolve to no project land after every adoption: "Other
    /// workspaces" is always last, whatever the names say.
    #[test]
    fn other_workspaces_is_last() {
        let projects = grouped_projects();
        let entries = vec![
            grouped_entry("s1", Some("p-harness"), Some("/work/p-harness"), 0),
            grouped_entry("s4", None, Some("/tmp/z-stray"), 0),
        ];
        let Grouping::Project(groups) = by_project(&entries, &projects) else {
            panic!("project grouping must yield project groups");
        };
        let last = groups.last().expect("groups");
        assert_eq!(last.id.as_ref(), OTHER_GROUP);
        assert_eq!(last.count.as_ref(), "1");
    }

    #[test]
    fn a_pinned_project_leads_whatever_its_activity() {
        let mut projects = grouped_projects();
        // Pin the project that sorts last by name: pinned still leads.
        projects.projects.iter_mut().find(|p| p.id == "p-harness").expect("harness").pinned = true;
        let entries = vec![
            grouped_entry("s1", Some("p-harness"), Some("/work/p-harness"), 5),
            grouped_entry("s3", Some("p-agentic"), Some("/work/p-agentic"), 0),
        ];
        let Grouping::Project(groups) = by_project(&entries, &projects) else {
            panic!("project grouping must yield project groups");
        };
        assert_eq!(groups[0].id.as_ref(), "p-harness");
        assert_eq!(groups[1].id.as_ref(), "p-agentic");
    }

    #[test]
    fn a_running_session_marks_its_group() {
        let projects = grouped_projects();
        let mut entries = vec![grouped_entry("s1", Some("p-harness"), Some("/work/p-harness"), 0)];
        entries[0].running = true;
        let Grouping::Project(groups) = by_project(&entries, &projects) else {
            panic!("project grouping must yield project groups");
        };
        let group = folded_group(&groups, "p-harness");
        assert_eq!(group.state, Some(AgentState::Running));
        // …while the branch stays off the row until its flag is on.
        assert_eq!(group.trailing.as_deref(), None);
        assert!(!group.chevron);
        assert!(!group.current_bar);
    }

    /// The three flags are state only: the grouping passes each one through
    /// to the row, and the branch trails only when its flag is on.
    #[test]
    fn group_chrome_follows_the_layout_flags() {
        let projects = grouped_projects();
        let entries = vec![grouped_entry("s1", Some("p-harness"), Some("/work/p-harness"), 0)];
        let mut branches = HashMap::new();
        branches.insert("p-harness".to_owned(), "main".to_owned());
        let layout = crate::layout::Layout { group_chevron: true, group_bar: true, group_branch: true, ..Default::default() };
        let empty = HashSet::new();
        let view = GroupView { closed: &empty, expanded: &empty, active: None, pending: None };
        let Grouping::Project(groups) =
            grouping_by_project(&entries, &projects, &branches, &view, &layout, Local::now())
        else {
            panic!("project grouping must yield project groups");
        };
        let group = folded_group(&groups, "p-harness");
        assert!(group.chevron);
        assert!(group.current_bar);
        assert_eq!(group.trailing.as_deref(), Some("main"));
    }

    #[test]
    fn a_closed_group_keeps_its_count_but_hides_its_rows() {
        let projects = grouped_projects();
        let entries = vec![grouped_entry("s1", Some("p-harness"), Some("/work/p-harness"), 0)];
        let mut closed = HashSet::new();
        closed.insert("p-harness".to_owned());
        let Grouping::Project(groups) = by_project_view(&entries, &projects, &closed, &HashSet::new(), None)
        else {
            panic!("project grouping must yield project groups");
        };
        let group = folded_group(&groups, "p-harness");
        assert!(!group.open);
        assert_eq!(group.count.as_ref(), "1");
        assert!(group.sessions.is_empty());
        // …while opening Other workspaces is the same set read inverted.
        let mut closed = HashSet::new();
        closed.insert(OTHER_GROUP.to_owned());
        let entries = vec![
            grouped_entry("s4", None, Some("/tmp/stray"), 0),
            grouped_entry("s5", None, Some("/tmp/stray-two"), 3),
        ];
        let Grouping::Project(groups) = by_project_view(&entries, &projects, &closed, &HashSet::new(), None)
        else {
            panic!("project grouping must yield project groups");
        };
        let other = groups.last().expect("other group");
        assert!(other.open);
        // Stray rows carry their workspace's folder name, newest first.
        assert_eq!(other.sessions.len(), 2);
        assert_eq!(other.sessions[0].id.as_ref(), "s4");
        assert_eq!(other.sessions[0].repo.as_deref(), Some("stray"));
        assert_eq!(other.sessions[1].repo.as_deref(), Some("stray-two"));
    }

    #[test]
    fn pinned_rows_lead_their_group_newest_first() {
        let projects = grouped_projects();
        let mut entries = vec![
            grouped_entry("s1", Some("p-harness"), Some("/work/p-harness"), 0),
            grouped_entry("s2", Some("p-harness"), Some("/work/p-harness"), 1),
        ];
        entries[1].pinned = true;
        let Grouping::Project(groups) = by_project(&entries, &projects) else {
            panic!("project grouping must yield project groups");
        };
        let group = folded_group(&groups, "p-harness");
        let ids: Vec<&str> = group.sessions.iter().map(|s| s.id.as_ref()).collect();
        assert_eq!(ids, vec!["s2", "s1"]);
        // The pin flag reaches the library row, which is what draws the
        // meta-line glyph and flips the tray button to `PinOff` (P5).
        assert!(group.sessions[0].pinned);
        assert!(!group.sessions[1].pinned);
    }

    /// Owner round 2, P3: nine sessions fold to the five newest, holding
    /// four back. The count still names all nine.
    #[test]
    fn nine_sessions_fold_to_five_plus_four_hidden() {
        let projects = grouped_projects();
        let entries = nine_sessions("p-harness");
        let Grouping::Project(groups) = by_project(&entries, &projects) else {
            panic!("project grouping must yield project groups");
        };
        let group = folded_group(&groups, "p-harness");
        assert_eq!(group.count.as_ref(), "9");
        let ids: Vec<&str> = group.sessions.iter().map(|s| s.id.as_ref()).collect();
        assert_eq!(ids, vec!["s0", "s1", "s2", "s3", "s4"]);
        assert_eq!(group.fold, Some((4, false)));
    }

    /// …and an expanded group shows all nine, with the fold row reading
    /// "Show less".
    #[test]
    fn an_expanded_group_shows_all_nine() {
        let projects = grouped_projects();
        let entries = nine_sessions("p-harness");
        let mut expanded = HashSet::new();
        expanded.insert("p-harness".to_owned());
        let Grouping::Project(groups) = by_project_view(&entries, &projects, &HashSet::new(), &expanded, None)
        else {
            panic!("project grouping must yield project groups");
        };
        let group = folded_group(&groups, "p-harness");
        assert_eq!(group.sessions.len(), 9);
        assert_eq!(group.sessions[8].id.as_ref(), "s8");
        assert_eq!(group.fold, Some((4, true)));
    }

    /// …while the open session survives the cut even when it is the oldest
    /// of the nine: it appends past the five rather than displacing a newer
    /// row.
    #[test]
    fn the_open_session_survives_the_cut() {
        let projects = grouped_projects();
        let entries = nine_sessions("p-harness");
        let Grouping::Project(groups) =
            by_project_view(&entries, &projects, &HashSet::new(), &HashSet::new(), Some("s8"))
        else {
            panic!("project grouping must yield project groups");
        };
        let group = folded_group(&groups, "p-harness");
        let ids: Vec<&str> = group.sessions.iter().map(|s| s.id.as_ref()).collect();
        assert_eq!(ids, vec!["s0", "s1", "s2", "s3", "s4", "s8"]);
        assert_eq!(group.fold, Some((3, false)));
    }

    /// The click's target survives the cut like the open session: on a
    /// client-less run it never becomes open, but its row still has to show
    /// for the highlight — and the reveal — to mean anything.
    #[test]
    fn the_click_target_survives_the_cut() {
        let projects = grouped_projects();
        let entries = nine_sessions("p-harness");
        let empty = HashSet::new();
        let view = GroupView { closed: &empty, expanded: &empty, active: None, pending: Some("s8") };
        let layout = crate::layout::Layout::default();
        let Grouping::Project(groups) =
            grouping_by_project(&entries, &projects, &HashMap::new(), &view, &layout, Local::now())
        else {
            panic!("project grouping must yield project groups");
        };
        let group = folded_group(&groups, "p-harness");
        let ids: Vec<&str> = group.sessions.iter().map(|s| s.id.as_ref()).collect();
        assert_eq!(ids, vec!["s0", "s1", "s2", "s3", "s4", "s8"]);
        assert_eq!(group.fold, Some((3, false)));
    }

    /// Pinned rows always show and never count toward the five: two pinned
    /// ancients plus the five newest make seven visible, two held back.
    #[test]
    fn pinned_rows_never_count_toward_the_five() {
        let projects = grouped_projects();
        let mut entries = nine_sessions("p-harness");
        entries[7].pinned = true;
        entries[8].pinned = true;
        let Grouping::Project(groups) = by_project(&entries, &projects) else {
            panic!("project grouping must yield project groups");
        };
        let group = folded_group(&groups, "p-harness");
        let ids: Vec<&str> = group.sessions.iter().map(|s| s.id.as_ref()).collect();
        assert_eq!(ids, vec!["s7", "s8", "s0", "s1", "s2", "s3", "s4"]);
        assert_eq!(group.fold, Some((2, false)));
    }

    /// The project-grouping twin of
    /// [`five_hundred_sidebar_rows_cost_less_from_the_cache`]: the pure half
    /// `render_sidebar` builds in project mode, timed against the cached
    /// frame, which hands the same `Rc` back. The assertion is only the
    /// ordering, so the test is not a timing flake.
    #[test]
    fn five_hundred_project_rows_cost_less_from_the_cache() {
        const N: usize = 500;
        const FRAMES: usize = 20;
        let now = Local::now();
        let projects = grouped_projects();
        let ids = ["p-harness", "p-agentic", "p-empty"];
        let entries: Vec<SessionEntry> = (0..N)
            .map(|i| {
                let mut e = entry(&format!("s{i}"));
                e.label = format!("session number {i}");
                e.description = format!("did something to file {i}");
                e.turns = (i % 7) as u64;
                e.updated = now - chrono::Duration::minutes(i as i64 * 7);
                e.project = Some(ids[i % ids.len()].to_owned());
                e.workspace = Some(format!("/work/{}", ids[i % ids.len()]));
                e
            })
            .collect();
        let empty = HashSet::new();
        let view = GroupView { closed: &empty, expanded: &empty, active: None, pending: None };
        let layout = crate::layout::Layout::default();
        let cold = std::time::Instant::now();
        for _ in 0..FRAMES {
            std::hint::black_box(grouping_by_project(&entries, &projects, &HashMap::new(), &view, &layout, now));
        }
        let cold = cold.elapsed() / FRAMES as u32;
        let grouping =
            std::rc::Rc::new(grouping_by_project(&entries, &projects, &HashMap::new(), &view, &layout, now));
        let warm = std::time::Instant::now();
        for _ in 0..FRAMES {
            std::hint::black_box(std::rc::Rc::clone(&grouping));
        }
        let warm = warm.elapsed() / FRAMES as u32;
        eprintln!("project-rows n={N} cold={cold:?}/frame warm={warm:?}/frame");
        assert!(warm < cold, "cached frame ({warm:?}) must cost less than the rebuild ({cold:?})");
    }

    #[test]
    fn a_replay_takes_its_workspace_from_the_capture() {
        let dir = std::env::temp_dir().join(format!("harness-replay-ws-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let capture = dir.join("capture.jsonl");
        std::fs::write(
            &capture,
            "--> {\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"session/start\",\"params\":{\"workspaceRoot\":\"/work/a\"}}\n<-- {\"jsonrpc\":\"2.0\",\"method\":\"session/started\",\"params\":{\"session\":{\"workspaceRoot\":\"/work/a\"}}}\n",
        )
        .expect("capture");
        assert_eq!(replay_workspace(&capture).as_deref(), Some("/work/a"));
        // The first `session/…` line decides: when it carries no root the
        // row has no workspace even if a later line does.
        std::fs::write(
            &capture,
            "<-- {\"jsonrpc\":\"2.0\",\"method\":\"session/started\",\"params\":{\"session\":{}}}\n<-- {\"jsonrpc\":\"2.0\",\"method\":\"session/branchChanged\",\"params\":{\"workspaceRoot\":\"/work/b\"}}\n",
        )
        .expect("capture");
        assert_eq!(replay_workspace(&capture), None);
        assert_eq!(replay_workspace(&dir.join("missing.jsonl")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_local_row_survives_until_the_wire_lists_it() {
        let mut local = entry("new");
        local.local = true;
        local.label = "Fix the parser".to_owned();
        // Not listed yet: the local row survives, still local.
        let merged = merge_session_list(Vec::new(), std::slice::from_ref(&local));
        assert_eq!(merged, vec![local.clone()]);
        // Listed now: the joined wire row replaces it, no longer local.
        let mut wire = entry("new");
        wire.label = "Fix the parser".to_owned();
        let merged = merge_session_list(vec![wire.clone()], std::slice::from_ref(&local));
        assert_eq!(merged, vec![wire]);
        // A non-local row is never kept past the reply.
        let gone = entry("old");
        let merged = merge_session_list(Vec::new(), std::slice::from_ref(&gone));
        assert!(merged.is_empty());
    }

    #[test]
    fn a_session_with_no_turns_is_empty() {
        assert!(entry("a").is_empty());
    }

    /// v0.1 prep task 3: being the open session no longer exempts a
    /// zero-turn, unnamed row — see [`SessionEntry::is_empty`]'s doc for why
    /// (`load_sessions` merges muse 1.3.0's now-unfiltered `session/list`,
    /// and the old exemption fired on every brand-new session as a result).
    #[test]
    fn the_open_session_is_empty_too_until_it_has_something_to_show() {
        assert!(entry("a").is_empty());
    }

    #[test]
    fn a_running_session_is_never_empty() {
        let mut running = entry("a");
        running.running = true;
        assert!(!running.is_empty());
    }

    #[test]
    fn a_named_session_is_never_empty() {
        let mut named = entry("a");
        named.named = true;
        assert!(!named.is_empty());
    }

    #[test]
    fn a_session_with_turns_is_never_empty() {
        let mut turned = entry("a");
        turned.turns = 1;
        assert!(!turned.is_empty());
    }

    fn meta_with(summary: Option<&str>, derived: Option<&str>) -> SessionMeta {
        SessionMeta {
            last_summary: summary.map(str::to_owned),
            derived_title: derived.map(str::to_owned),
            ..SessionMeta::default()
        }
    }

    fn indexed(prompt: Option<&str>, title: &str, name: Option<&str>) -> IndexEntry {
        IndexEntry {
            session_name: name.map(str::to_owned),
            title: title.to_owned(),
            first_user_prompt: prompt.map(str::to_owned),
            ..IndexEntry::default()
        }
    }

    #[test]
    fn the_description_prefers_the_last_summary_then_the_prompt() {
        let prompt = "Run the shell command `ls` in the workspace, then use your question tool";
        // A summary wins even when the label already is the prompt.
        let meta = meta_with(Some("Fixed the parser panic"), None);
        let index = indexed(Some(prompt), prompt, None);
        assert_eq!(describe(Some(&meta), Some(&index), prompt, false), "Fixed the parser panic");
        // A Muse title that is the whole first prompt must not repeat below itself.
        assert_eq!(describe(None, Some(&index), prompt, false), "");
        // A derived title elided from the prompt is the same words twice.
        assert_eq!(describe(None, Some(&index), "Run the shell command…", false), "");
        // A user-given name earns the prompt below it.
        let named = indexed(Some(prompt), "", Some("ls run"));
        assert_eq!(describe(None, Some(&named), "ls run", true), prompt);
        // A Muse title of its own earns the prompt too.
        let titled = indexed(Some("why does this panic"), "Parser panic", None);
        assert_eq!(describe(None, Some(&titled), "Parser panic", false), "why does this panic");
        // Without a summary or a showable prompt the row carries the turns
        // meta alone; a stale derived title is not a description line.
        assert_eq!(describe(Some(&meta_with(None, Some("cargo test"))), None, "cargo test", false), "");
        assert_eq!(describe(None, None, "New session", false), "");
    }

    #[test]
    fn a_label_cut_from_the_prompt_echoes_it() {
        let prompt = "Run the shell command `ls` in the workspace, then ask";
        assert!(echoes_prompt(prompt, prompt));
        assert!(echoes_prompt("Run the shell command…", prompt));
        assert!(echoes_prompt("RUN THE SHELL   COMMAND", prompt));
        assert!(!echoes_prompt("Parser panic", "why does this panic"));
        assert!(!echoes_prompt("", prompt));
    }

    #[test]
    fn a_long_summary_is_cut_where_the_row_would_truncate_it() {
        let meta = meta_with(Some(&"w".repeat(200)), None);
        assert_eq!(describe(Some(&meta), None, "x", false).chars().count(), 80);
    }

}
