//! The sessions sidebar's library rows (spec §3.7): [`baaz_core::sidebar`]'s
//! [`SessionEntry`] rows projected onto the aui component library's
//! [`SessionSummary`] rows and [`Grouping`]s — by calendar date, or by
//! project — and the hover card's detail.
//!
//! Everything here is pure: rows in, a [`Grouping`] out. The model (state,
//! words, order, chains, buckets) lives in the core crate and is re-exported
//! here, so `crate::sidebar::…` paths name both halves.

use std::collections::HashMap;

use aui_tokens::AgentState;
pub use aui::nav::Grouping;

use aui::nav::{Byline, DateGroup, ProjectGroup, RowStatus, RowStatusKind, SessionSummary};
use chrono::{DateTime, Local};

pub use baaz_core::sidebar::*;

use crate::layout::Layout;
use crate::projects::Projects;
use crate::providers::ProviderMark as _;

/// The library's mark for a [`SessionState`].
pub trait StateMark {
    /// The library's mark for this state; `DoneUnread` is an idle mark with
    /// the unread flag beside it.
    fn agent_state(self) -> AgentState;
}

impl StateMark for SessionState {
    fn agent_state(self) -> AgentState {
        match self {
            SessionState::Working => AgentState::Running,
            SessionState::NeedsYou(_) => AgentState::Waiting,
            SessionState::Failed => AgentState::Failed,
            SessionState::DoneUnread | SessionState::Idle => AgentState::Idle,
        }
    }
}

/// A [`SessionEntry`] as the library draws it: the trailing mark, the
/// status words, the second line, the hover detail and the row itself.
pub trait SessionRow {
    /// The library's state for the row's trailing slot.
    fn state(&self) -> AgentState;
    /// The row's state verdict, against `now`.
    fn row_status(&self, now: DateTime<Local>) -> RowStatus;
    /// The row's second line, when one wins the slot.
    fn second_line(&self) -> Option<Byline>;
    /// The hover detail's content.
    fn detail_data(&self, now: DateTime<Local>) -> aui::nav::SessionDetailData;
    /// The library row for this session, labelled against `now`.
    fn summary(&self, now: DateTime<Local>) -> SessionSummary;
}

impl SessionRow for SessionEntry {
    /// The library's state for the row's trailing slot, from
    /// [`Self::session_state`]: spinner, amber dot, danger dot, the unread
    /// dot (`Idle` + [`SessionSummary::unread`]) or nothing. The one-line
    /// row (docs/83 §4) has no status line and no leading glyph, so this
    /// slot is the only place the state reads.
    fn state(&self) -> AgentState {
        self.session_state().agent_state()
    }

    /// The row's state verdict, against `now`: which sentence names it and
    /// in which colour. The one-line row (docs/83 §4) draws no status line
    /// of its own — the state rides the trailing slot (and the hover card),
    /// and the sentence is the summary line's when Settings turns it on.
    /// The words for [`Self::session_state`], so they always agree with the
    /// mark: `Needs approval`, the question, `Working` with its own
    /// elapsed, `Failed` with the session's elapsed (`Failed · 1h` — the
    /// message itself lives in the hover detail, where it fits), else
    /// `Settled` with elapsed and turn count, or never replied.
    fn row_status(&self, now: DateTime<Local>) -> RowStatus {
        match self.session_state() {
            SessionState::NeedsYou(NeedKind::Approval) => RowStatus::new(RowStatusKind::NeedsApproval, ""),
            SessionState::NeedsYou(NeedKind::Question) => {
                RowStatus::new(RowStatusKind::Asked, self.question_text().unwrap_or_default())
            }
            SessionState::Working => {
                let start = self.turn_started.unwrap_or(self.updated);
                RowStatus::new(RowStatusKind::Working, elapsed_at(start, now))
            }
            SessionState::Failed => RowStatus::new(RowStatusKind::Failed, elapsed_at(self.updated, now)),
            SessionState::DoneUnread | SessionState::Idle if self.turns > 0 => {
                let elapsed = elapsed_at(self.updated, now);
                let detail = format!("{elapsed} · {} turn{}", self.turns, if self.turns == 1 { "" } else { "s" });
                RowStatus::new(RowStatusKind::Settled, detail)
            }
            SessionState::DoneUnread | SessionState::Idle => RowStatus::new(RowStatusKind::NoReply, ""),
        }
    }

    /// The row's second line, option B's context: the approval command or
    /// pending question when one exists, else the ask/result byline, else a
    /// one-line preview, else nothing. The `Working…` and `No reply yet`
    /// placeholders retired with the status line, which owns those words
    /// now; `Naming this session…` stays, for a session with nothing else
    /// to say while its generated title is in flight.
    ///
    /// `Some` is an explicit [`Byline`] that wins the slot; `None` falls
    /// through to `project · branch` (via `repo`/`branch`), the Archived
    /// tag, or the library's empty-space line — which keeps its height as
    /// blank space, so a brand-new session is exactly as tall as its
    /// neighbours.
    fn second_line(&self) -> Option<Byline> {
        if self.title_pending {
            return Some(Byline::Placeholder(PLACEHOLDER_NAMING.into()));
        }
        if let Some(ask) = self.last_ask.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            return Some(Byline::TwoLines { ask: ask.to_owned().into(), result: self.description.clone().into() });
        }
        if !self.description.trim().is_empty() {
            return Some(Byline::Preview(self.description.clone().into()));
        }
        None
    }

    /// The hover detail's content: the whole picture the row truncates —
    /// full title (capped at [`hover_title`]'s 160-char budget), ask and
    /// latest reply, the status with its detail (the
    /// pending question, the approval command, the terminal error),
    /// project, branch, turn count, last change, workspace path and the
    /// open session's pending words. Only what the app knows: empty words
    /// stay unset and never draw. The `Failed` status carries the terminal
    /// error where it fits; every other state reuses the row's own status
    /// words.
    fn detail_data(&self, now: DateTime<Local>) -> aui::nav::SessionDetailData {
        let status = self.row_status(now);
        let status = if status.kind == RowStatusKind::Failed {
            RowStatus::new(
                RowStatusKind::Failed,
                self.last_error.as_deref().map(str::trim).filter(|s| !s.is_empty()).unwrap_or_default(),
            )
        } else if status.kind == RowStatusKind::NeedsApproval {
            RowStatus::new(RowStatusKind::NeedsApproval, self.approval_text().unwrap_or_default())
        } else {
            status
        };
        let elapsed = elapsed_at(self.updated, now);
        aui::nav::SessionDetailData {
            title: Some(hover_title(&self.label).into()),
            ask: self.last_ask.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(|s| s.to_owned().into()),
            reply: (!self.description.trim().is_empty()).then(|| self.description.clone().into()),
            status: Some(status),
            project: self.project_name.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(|s| s.to_owned().into()),
            branch: branch_with_pr(self).map(Into::into),
            turns: Some(usize::try_from(self.turns).unwrap_or(usize::MAX)),
            updated: Some(if elapsed == "now" { "now".into() } else { format!("{elapsed} ago").into() }),
            workspace: self.workspace.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(|s| s.to_owned().into()),
            pending_question: self.question_text().map(|s| s.to_owned().into()),
            pending_approval: self.approval_text().map(|s| s.to_owned().into()),
        }
    }

    /// The library row for this session, labelled against `now`. Every
    /// row's data carries its provider's mark (docs/82 5.5); the flat
    /// sidebar row (docs/83 §4) does not draw it — only the library's
    /// compact row shows marks, in its meta line.
    fn summary(&self, now: DateTime<Local>) -> SessionSummary {
        let branch = branch_with_pr(self);
        let mut row = SessionSummary::new(self.id.clone(), self.label.clone(), self.state(), elapsed_at(self.updated, now))
            .provider(self.provider.row_mark());
        // Option B's context priority: the live approval/question override
        // first, then the ladder below.
        if let Some(command) = self.approval_text() {
            row = row.attention(command.to_owned());
        } else if let Some(question) = self.question_text() {
            row = row.attention(question.to_owned());
        } else {
            match self.second_line() {
                Some(Byline::Placeholder(text)) => {
                    row = row.placeholder(text);
                }
                Some(Byline::Preview(text)) => {
                    row = row.preview(text);
                }
                Some(Byline::TwoLines { ask, result }) => {
                    row = row.byline(ask, result);
                }
                // No byline and no preview: `project · branch` where either
                // half exists, the Archived tag on a bare archived row, else
                // the library's empty space — one line in every case (the
                // empty line keeps its height as blank space, measured
                // pixel-identical against text lines in the row-B captures).
                None => {
                    if let Some(project) = self.project_name.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                        row = row.repo(project.to_owned());
                    }
                    if let Some(branch) = branch.as_deref() {
                        row = row.branch(branch.to_owned());
                    }
                    if self.archived {
                        row = row.meta(aui::nav::MetaItem::Tag("Archived".into()));
                    }
                }
            }
        }
        // A worktree session carries its branch whatever the second line
        // says: the summary line falls back to it when the session has said
        // nothing newer, and the hover card always shows it (docs/82 6.2).
        if let Some(branch) = branch.as_deref().filter(|_| worktree_branch(self).is_some()) {
            row = row.branch(branch.to_owned());
        }
        // Commands the session's agent started that still run (docs/82
        // 6.5): the row's trailing slot says "N running".
        row = row.running_tasks(self.tasks_running);
        // The pages the session owns (docs/82 8.4): "N pages" in an empty
        // slot, and beside the `⋯` on hover.
        row = row.pages(self.pages);
        // A provisional row carries no turn count, no running bit and no
        // status verdict — the index knows none of them — so no status is
        // set: the row says no verdict (never a false `Settled · 0
        // turns`), and the real row fills the verdict in when
        // it lands. No layout jump either way: a row is one height
        // in every state.
        if !self.provisional {
            let status = self.row_status(now);
            row = row.status(status.kind, status.detail);
        }
        if self.pinned {
            row = row.pinned();
        }
        if self.running {
            row = row.pulse();
        }
        if self.session_state() == SessionState::DoneUnread {
            row = row.unread();
        }
        row
    }
}

/// The branch of the worktree `entry` runs in, when it runs in one of
/// Baaz's (docs/82 6.2), from the process cache.
fn worktree_branch(entry: &SessionEntry) -> Option<String> {
    entry.workspace.as_deref().and_then(|folder| baaz_core::worktrees::cached_branch_for_folder(std::path::Path::new(folder)))
}

/// The branch a row names: its worktree's, else the index's derived one.
pub(crate) fn shown_branch(entry: &SessionEntry) -> Option<String> {
    worktree_branch(entry).or_else(|| entry.branch.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned))
}

/// The branch as a row draws it: led by its pull request
/// (`PR #12 open · fix-checkout`, docs/82 7.3; the PR first so a narrow row
/// truncates the branch, never the state) wherever the row names its
/// branch — the hover card's meta row and the summary line's `project ·
/// branch`. The flat row (docs/83 §4) has no other place for it: no
/// leading glyph, and the trailing slot holds the row's one state.
fn branch_with_pr(entry: &SessionEntry) -> Option<String> {
    let branch = shown_branch(entry)?;
    Some(match &entry.pull_request {
        Some(pr) => format!("{} · {branch}", pr.label()),
        None => branch,
    })
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

/// Group the entries by project, against an explicit clock.
///
/// One group per adoption in [`Projects::sorted`] order — pinned first, then
/// name, never recency — each a plain muted label with
/// the visible-session count (an empty project still gets its row, counting
/// `"0"`), a running dot when any of its sessions runs, the current-project
/// accent mark on the open session's project (from `current(true)` alone —
/// no layout flag gates it), and, only when its layout flag asks, the
/// trailing branch. Sessions inside run
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
/// past the five rather than displacing a newer row. Unfiled
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
        // A missing root is no project: the row lands in Unfiled
        // even when its stored id still names the adoption.
        match entry.project.as_deref().and_then(|id| projects.find_available(id)) {
            Some(project) => {
                by_project.entry(project.id.as_str()).or_default().push(entry);
            }
            None => other.push(entry),
        }
    }
    // D4: the project the open session belongs to wears the accent mark,
    // drawn from `current(true)` alone (agentic-ui v0.3.15 — the `group_bar`
    // flag no longer gates it). With no session open the store's current
    // project wears it instead — that is the project the header crumb names
    // and the one a new session would land in, so the mark keeps pointing
    // at the same place either way. A brand-new session has no row yet, so
    // its project arrives through `view.active_project` (the caller resolves
    // it past the rows); a session with a row reads its project from the
    // row. Either falls back past a missing root, exactly as the crumb does.
    let current_project: Option<&str> = view
        .active
        .and_then(|open| entries.iter().find(|e| e.id == open))
        .and_then(|e| e.project.as_deref())
        .or(view.active_project)
        .and_then(|id| projects.find_available(id))
        .map(|p| p.id.as_str())
        .or_else(|| projects.effective_current().map(|p| p.id.as_str()));
    let mut groups = Vec::new();
    // Missing roots get no group at all — but stay adopted, so they come
    // back when the path does.
    for project in projects.sorted_available() {
        let mut rows = by_project.remove(project.id.as_str()).unwrap_or_default();
        sort_rows(&mut rows, layout.sort_by);
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
        // meaningless digit on the same baseline as the meaningful ones.
        let count = if rows.is_empty() { String::new() } else { rows.len().to_string() };
        // The plain default group row: no mark; the trailing branch only
        // when its layout flag asks. The label draws its own chevron on
        // hover or while closed (docs/83 §4), with no switch. The current
        // mark draws from `current(true)` below alone; the retired
        // `group_bar` flag no longer draws a second bar. The count stays.
        let mut group = ProjectGroup::new(project.id.clone(), project.name.clone(), count);
        if layout.group_branch {
            if let Some(branch) = branches.get(&project.id) {
                group = group.trailing(branch.clone());
            }
        }
        // The strongest state of the project's sessions (task 3.2): what
        // its header shows while the group is closed.
        match strongest_state(rows.iter().copied()) {
            Some(SessionState::DoneUnread) => group = group.state(AgentState::Idle).unread(true),
            Some(state) => group = group.state(state.agent_state()),
            None => {}
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
        sort_rows(&mut other, layout.sort_by);
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
        let mut group = ProjectGroup::new(OTHER_GROUP, UNFILED_LABEL, count).muted();
        // Open unless the person closed it, exactly like a project group.
        // It used to be the one row that read the set inverted, because it
        // held other people's workspaces and started shut; it now holds the
        // default workspace too, so a session started before any project was
        // adopted would otherwise land out of sight.
        if !view.closed.contains(OTHER_GROUP) {
            group = group.open(sessions);
        }
        groups.push(group);
    }
    Grouping::Project(groups)
}

/// Lift the pinned sessions out of a project or date grouping: the v2
/// sidebar draws them in its own Pinned section above the list, so the
/// grouping must not list them a second time (a date grouping left with
/// pinned rows would flatten a second Pinned header). A date bucket left
/// empty by the lift is dropped. Status groupings are untouched. The
/// project's count keeps counting them: the head says how many sessions
/// the project has, not how many rows sit under it.
/// docs/85 14.9: each worker's row moves under its lead's row (`leads`:
/// worker → lead) when the lead is listed; a worker whose lead is not
/// listed stays where it is. One level: a worker's own workers nest under
/// the same lead.
pub fn nest_workers(grouping: Grouping, leads: &HashMap<String, String>) -> Grouping {
    if leads.is_empty() {
        return grouping;
    }
    fn lists(grouping: &mut Grouping) -> Vec<&mut Vec<SessionSummary>> {
        match grouping {
            Grouping::Project(groups) => groups.iter_mut().map(|g| &mut g.sessions).collect(),
            Grouping::Date(groups) => groups.iter_mut().map(|g| &mut g.sessions).collect(),
            Grouping::Status(groups) => groups.iter_mut().map(|g| &mut g.sessions).collect(),
        }
    }
    let mut grouping = grouping;
    let listed: std::collections::HashSet<String> = lists(&mut grouping).iter().flat_map(|l| l.iter().map(|s| s.id.to_string())).collect();
    // The top lead of a chain (a worker's worker nests under the first lead).
    let top = |id: &str| {
        let mut at = id.to_owned();
        for _ in 0..4 {
            match leads.get(&at) {
                Some(lead) if listed.contains(lead) => at = lead.clone(),
                _ => break,
            }
        }
        at
    };
    let mut moved: HashMap<String, Vec<SessionSummary>> = HashMap::new();
    for list in lists(&mut grouping) {
        let mut kept = Vec::with_capacity(list.len());
        for row in list.drain(..) {
            let lead = top(&row.id);
            if lead != row.id.as_ref() {
                moved.entry(lead).or_default().push(row);
            } else {
                kept.push(row);
            }
        }
        *list = kept;
    }
    for list in lists(&mut grouping) {
        for row in list.iter_mut() {
            if let Some(children) = moved.remove(row.id.as_ref()) {
                row.children.extend(children);
            }
        }
    }
    grouping
}

pub fn lift_pinned(grouping: Grouping) -> (Grouping, Vec<aui::nav::SessionSummary>) {
    match grouping {
        Grouping::Project(mut groups) => {
            let mut pinned = Vec::new();
            for group in &mut groups {
                let (lifted, kept): (Vec<_>, Vec<_>) =
                    std::mem::take(&mut group.sessions).into_iter().partition(|s| s.pinned);
                group.sessions = kept;
                pinned.extend(lifted);
            }
            (Grouping::Project(groups), pinned)
        }
        Grouping::Date(buckets) => {
            let mut pinned = Vec::new();
            let mut kept_buckets = Vec::with_capacity(buckets.len());
            for mut bucket in buckets {
                let (lifted, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut bucket.sessions).into_iter().partition(|s| s.pinned);
                pinned.extend(lifted);
                if !kept.is_empty() {
                    bucket.sessions = kept;
                    kept_buckets.push(bucket);
                }
            }
            (Grouping::Date(kept_buckets), pinned)
        }
        other => (other, Vec::new()),
    }
}

/// The wire id's sidebar mark: the registered provider's own badge;
/// `None` for an id no registered provider answers to.
pub fn provider_mark(id: &str) -> Option<aui_icons::Provider> {
    crate::providers::ProviderId::parse_known(id).map(|provider| provider.row_mark())
}

#[cfg(feature = "probe")]
#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};
    use chrono::Local;
    use crate::index::IndexEntry;
    use crate::list_cache::ListedRow;
    use crate::providers::ProviderId;
    use provider::facts::Attention;
    use crate::projects::Projects;

    /// docs/85 14.9: workers nest under their lead; a worker of a worker
    /// under the first lead; a worker whose lead is not listed stays put.
    #[test]
    fn workers_nest_under_their_lead() {
        let row = |id: &str| SessionSummary::new(id, id, AgentState::Idle, "1m");
        let grouping = Grouping::Date(vec![DateGroup::new("Today", vec![row("lead"), row("w1"), row("w2"), row("w3"), row("orphan")])]);
        let leads: HashMap<String, String> = [("w1", "lead"), ("w2", "lead"), ("w3", "w2"), ("orphan", "gone")].into_iter().map(|(a, b)| (a.to_owned(), b.to_owned())).collect();
        let Grouping::Date(groups) = nest_workers(grouping, &leads) else { panic!() };
        let ids: Vec<&str> = groups[0].sessions.iter().map(|s| s.id.as_ref()).collect();
        assert_eq!(ids, ["lead", "orphan"]);
        let children: Vec<&str> = groups[0].sessions[0].children.iter().map(|s| s.id.as_ref()).collect();
        assert_eq!(children, ["w1", "w2", "w3"]);
    }

    /// docs/82 5.5 (docs/81 F C3): every row wears its provider's badge —
    /// Muse's too, where "no badge" used to mean Muse — and the wire id's
    /// mark is the registry's.
    #[test]
    fn every_row_wears_its_providers_badge() {
        let now = Local::now();
        for id in crate::providers::ProviderId::all() {
            let mut row = entry(id.as_str());
            row.provider = id;
            assert_eq!(row.summary(now).providers, vec![id.icon()], "{}", id.as_str());
            assert_eq!(provider_mark(id.as_str()), Some(id.icon()));
        }
        assert_eq!(provider_mark("no-such-provider"), None);
    }

    /// The summary carries the state the trailing slot draws (docs/83 §4):
    /// Waiting (the amber dot), Failed (the danger dot), Running (the
    /// spinner); the trailing words still name the first two.
    #[test]
    fn waiting_failed_and_working_read_on_the_glyph_and_the_trailing_slot() {
        use aui::nav::row_trailing_kind;
        let now = Local::now();
        let mut approval = entry("approval");
        approval.turns = 2;
        approval.approval_command = Some("ls /tmp".into());
        let summary = approval.summary(now);
        assert_eq!(summary.state, AgentState::Waiting);
        assert_eq!(row_trailing_kind(&summary).text(&summary).as_ref(), "Needs approval");
        let mut failed = entry("failed");
        failed.turns = 3;
        failed.last_error = Some("boom".into());
        let summary = failed.summary(now);
        assert_eq!(summary.state, AgentState::Failed);
        assert_eq!(row_trailing_kind(&summary).text(&summary).as_ref(), "Failed");
        let mut working = entry("working");
        working.turns = 1;
        working.running = true;
        working.turn_started = Some(now);
        let summary = working.summary(now);
        assert_eq!(summary.state, AgentState::Running);
        assert!(summary.pulse, "a working row is live");
    }

    /// Task 3.1: one function decides, in one order — needs-you beats
    /// working beats failed beats unread beats idle — and the mark and the
    /// words both come from it. The audit's row (running with an approval
    /// pending) read a spinner beside "Needs approval"; it now reads
    /// waiting in both places.
    #[test]
    fn one_state_drives_the_mark_and_the_words() {
        use aui::nav::{row_trailing_kind, RowStatusKind};
        let now = Local::now();
        let mut running_approval = entry("running-approval");
        running_approval.turns = 1;
        running_approval.running = true;
        running_approval.attention = vec![Attention::ApprovalPending];
        assert_eq!(running_approval.session_state(), SessionState::NeedsYou(NeedKind::Approval));
        let summary = running_approval.summary(now);
        assert_eq!(summary.state, AgentState::Waiting, "no spinner beside Needs approval");
        assert_eq!(summary.status.as_ref().map(|s| s.kind), Some(RowStatusKind::NeedsApproval));
        assert_eq!(row_trailing_kind(&summary).text(&summary).as_ref(), "Needs approval");

        let mut both = entry("both");
        both.attention = vec![Attention::InputPending, Attention::ApprovalPending];
        assert_eq!(both.session_state(), SessionState::NeedsYou(NeedKind::Approval), "an approval beats a question");
        let mut asked = entry("asked");
        asked.running = true;
        asked.pending_question = Some("Which file?".into());
        assert_eq!(asked.session_state(), SessionState::NeedsYou(NeedKind::Question));
        assert_eq!(asked.state(), AgentState::Waiting);

        let mut failed_unread = entry("failed-unread");
        failed_unread.turns = 2;
        failed_unread.last_error = Some("boom".into());
        failed_unread.unread = true;
        assert_eq!(failed_unread.session_state(), SessionState::Failed, "failed beats unread");
        let mut working_unread = entry("working-unread");
        working_unread.running = true;
        working_unread.unread = true;
        assert_eq!(working_unread.session_state(), SessionState::Working, "a new turn's run beats the old unread");

        let mut unread = entry("unread");
        unread.turns = 4;
        unread.unread = true;
        assert_eq!(unread.session_state(), SessionState::DoneUnread);
        let summary = unread.summary(now);
        assert_eq!(summary.state, AgentState::Idle);
        // The production slot (`aui::screens::sidebar` `state_slot`) draws
        // the accent "Unread" dot for an idle row with `unread` set.
        assert!(summary.unread, "the unread dot shows");
        assert_eq!(summary.status.as_ref().map(|s| s.kind), Some(RowStatusKind::Settled));

        let mut idle = entry("idle");
        idle.turns = 4;
        assert_eq!(idle.session_state(), SessionState::Idle);
        assert!(!idle.summary(now).unread);
    }

    /// Every combination of the inputs gives a mark and words that agree:
    /// waiting words only on a waiting mark, `Working` only on a spinner,
    /// `Failed` only on a failed mark.
    #[test]
    fn the_mark_and_the_words_never_disagree() {
        use aui::nav::RowStatusKind;
        let now = Local::now();
        for bits in 0u8..32 {
            let mut row = entry("combo");
            row.turns = 1;
            row.running = bits & 1 != 0;
            if bits & 2 != 0 {
                row.attention.push(Attention::ApprovalPending);
            }
            if bits & 4 != 0 {
                row.attention.push(Attention::InputPending);
            }
            if bits & 8 != 0 {
                row.last_error = Some("boom".into());
            }
            row.unread = bits & 16 != 0;
            let summary = row.summary(now);
            let kind = summary.status.as_ref().map(|s| s.kind).expect("a status");
            let expected = match kind {
                RowStatusKind::NeedsApproval | RowStatusKind::Asked => AgentState::Waiting,
                RowStatusKind::Working => AgentState::Running,
                RowStatusKind::Failed => AgentState::Failed,
                RowStatusKind::Settled | RowStatusKind::NoReply => AgentState::Idle,
            };
            assert_eq!(summary.state, expected, "inputs {bits:05b}: {kind:?} words on a {:?} mark", summary.state);
        }
    }

    /// Whatever label a row carries, the hover card's title stays within
    /// its 160-char budget.
    #[test]
    fn the_hover_card_title_never_exceeds_its_budget() {
        let now = Local::now();
        let mut row = entry("long");
        row.label = "w".repeat(2000);
        let data = row.detail_data(now);
        let title = data.title.expect("a title");
        assert!(title.chars().count() <= 160, "hover title is {} chars", title.chars().count());
    }

    /// The open session's project group is current: for a session with a
    /// row, and for a brand-new session whose project arrives past the
    /// rows through `active_project`.
    #[test]
    fn the_open_session_marks_its_project_current() {
        let projects = grouped_projects();
        let entries = vec![grouped_entry("s1", Some("p-baaz"), Some("/work/p-baaz"), 0)];
        let Grouping::Project(groups) =
            by_project_view(&entries, &projects, &HashSet::new(), &HashSet::new(), Some("s1"))
        else {
            panic!("project grouping must yield project groups");
        };
        assert!(folded_group(&groups, "p-baaz").current, "the open session's project is current");
        assert!(!folded_group(&groups, "p-agentic").current, "other projects stay plain");
        // Brand-new: no row yet, so the project resolves past the rows.
        let empty: Vec<SessionEntry> = Vec::new();
        let closed = HashSet::new();
        let expanded = HashSet::new();
        let view = GroupView {
            closed: &closed,
            expanded: &expanded,
            active: Some("s-new"),
            active_project: Some("p-agentic"),
            pending: None,
        };
        let Grouping::Project(groups) =
            grouping_by_project(&empty, &projects, &HashMap::new(), &view, &crate::layout::Layout::default(), Local::now())
        else {
            panic!("project grouping must yield project groups");
        };
        assert!(folded_group(&groups, "p-agentic").current, "a new session marks its project current");
        assert!(!folded_group(&groups, "p-baaz").current, "other projects stay plain");
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
    /// The sidebar pane's render itself needs a window, so this times the two pure
    /// halves it is made of — the visible filter-and-sort and the grouping
    /// that builds one `SessionSummary` per row — against the cached frame,
    /// which is two `Rc` hand-backs and nothing else now that `sidebar_view`
    /// takes the grouping behind an `Rc` too (finding `performance-13`).
    /// Numbers with `--nocapture`; the assertion is only the ordering, so the
    /// test is not a timing flake.
    /// A pending generated title reads `Naming this session…` until it
    /// lands — the one placeholder the status line did not retire.
    #[test]
    fn a_pending_title_reads_naming_on_the_second_line() {
        let mut pending = entry("pending");
        pending.title_pending = true;
        pending.description = "patched the validator".into();
        assert_eq!(pending.second_line(), Some(Byline::Placeholder(PLACEHOLDER_NAMING.into())));
    }

    /// The byline reads the ask and the result side by side, ahead of the
    /// preview text.
    #[test]
    fn a_byline_reads_ask_and_result_on_the_second_line() {
        let mut lined = entry("lined");
        lined.turns = 2;
        lined.description = "patched the validator".into();
        lined.last_ask = Some("tighten validation".into());
        assert_eq!(
            lined.second_line(),
            Some(Byline::TwoLines { ask: "tighten validation".into(), result: "patched the validator".into() })
        );
    }

    /// No byline but a preview: the summary reads alone on the context
    /// line — the branch and the turn count moved to the status line and
    /// `project · branch`, so they no longer share it.
    #[test]
    fn a_preview_without_a_byline_reads_alone() {
        let mut settled = entry("settled");
        settled.turns = 3;
        settled.description = "patched the validator".into();
        assert_eq!(settled.second_line(), Some(Byline::Preview("patched the validator".into())));
    }

    /// Option B's context priority, end to end through `summary()`: the
    /// approval command beats the pending question, which beats the byline,
    /// which beats the preview, which beats `project · branch`, which beats
    /// the provider mark's meta line.
    #[test]
    fn context_priority_runs_approval_question_byline_preview_project() {
        use aui::nav::{context_line_kind, ContextLineKind};
        let now = Local::now();
        let mut full = entry("full");
        full.last_ask = Some("tighten validation".into());
        full.description = "patched the validator".into();
        full.project_name = Some("acme-web".into());
        full.branch = Some("feature-x".into());
        full.pending_question = Some("Which bucket for staging?".into());
        full.approval_command = Some("sudo apt install notifierd".into());
        assert_eq!(context_line_kind(&full.summary(now)), ContextLineKind::Attention);
        assert_eq!(full.summary(now).attention.as_deref(), Some("sudo apt install notifierd"));
        full.approval_command = None;
        assert_eq!(context_line_kind(&full.summary(now)), ContextLineKind::Attention);
        assert_eq!(full.summary(now).attention.as_deref(), Some("Which bucket for staging?"));
        full.pending_question = None;
        assert_eq!(context_line_kind(&full.summary(now)), ContextLineKind::Byline);
        full.last_ask = None;
        assert_eq!(context_line_kind(&full.summary(now)), ContextLineKind::Preview);
        full.description.clear();
        assert_eq!(context_line_kind(&full.summary(now)), ContextLineKind::Project);
        full.project_name = None;
        full.branch = None;
        // Every row wears its provider's mark (docs/82 5.5), so the last
        // rung is the mark's meta line, never empty space.
        assert_eq!(context_line_kind(&full.summary(now)), ContextLineKind::Meta);
    }

    /// The status vocabulary, one state at a time: the exact sentence the
    /// third line draws.
    #[test]
    fn status_vocabulary_per_state() {
        use Attention;
        let now = Local::now();
        let ago = |minutes: i64| now - chrono::Duration::minutes(minutes);
        // Running with its own elapsed.
        let mut running = entry("running");
        running.running = true;
        running.turns = 3;
        running.updated = ago(60);
        running.turn_started = Some(ago(14));
        assert_eq!(running.row_status(now).text().as_ref(), "Working · 14m");
        // Running without a tracked start falls back to the session's age.
        let mut running_fallback = entry("running-fallback");
        running_fallback.running = true;
        running_fallback.updated = ago(14);
        assert_eq!(running_fallback.row_status(now).text().as_ref(), "Working · 14m");
        // An approval mid-turn still reads `Needs approval`, never
        // `Working` — live command or wire flag alike.
        let mut approval = entry("approval");
        approval.running = true;
        approval.turns = 2;
        approval.updated = ago(8);
        approval.approval_command = Some("sudo apt install notifierd".into());
        assert_eq!(approval.row_status(now).text().as_ref(), "Needs approval");
        let mut flagged_approval = entry("flagged-approval");
        flagged_approval.turns = 2;
        flagged_approval.updated = ago(8);
        flagged_approval.attention = vec![Attention::ApprovalPending];
        assert_eq!(flagged_approval.row_status(now).text().as_ref(), "Needs approval");
        // A pending question quotes its words; the bare flag reads `Asked`.
        let mut asked = entry("asked");
        asked.turns = 4;
        asked.updated = ago(8);
        asked.pending_question = Some("Which bucket for staging?".into());
        assert_eq!(asked.row_status(now).text().as_ref(), "Asked: \"Which bucket for staging?\"");
        let mut flagged_asked = entry("flagged-asked");
        flagged_asked.turns = 4;
        flagged_asked.updated = ago(8);
        flagged_asked.attention = vec![Attention::InputPending];
        assert_eq!(flagged_asked.row_status(now).text().as_ref(), "Asked");
        // The last turn's terminal error, with the session's elapsed.
        let mut failed = entry("failed");
        failed.turns = 6;
        failed.updated = ago(90);
        failed.last_error = Some("modelError: the model was overloaded".into());
        assert_eq!(failed.row_status(now).text().as_ref(), "Failed · 1h");
        // A settled session carries elapsed and turn count; one turn reads
        // singular.
        let mut settled = entry("settled");
        settled.turns = 5;
        settled.updated = ago(12);
        assert_eq!(settled.row_status(now).text().as_ref(), "Settled · 12m · 5 turns");
        let mut one = entry("one");
        one.turns = 1;
        one.updated = ago(12);
        assert_eq!(one.row_status(now).text().as_ref(), "Settled · 12m · 1 turn");
        // Nothing ever replied.
        let fresh = entry("fresh");
        assert_eq!(fresh.row_status(now).text().as_ref(), "No reply yet");
    }

    /// The `attention` mapping: both known flags light their state, and a
    /// future flag the schema left open lights nothing.
    #[test]
    fn attention_flags_map_to_their_states_only() {
        use Attention;
        let mut both = entry("both");
        both.attention = vec![Attention::ApprovalPending, Attention::InputPending];
        assert!(both.needs_approval());
        assert!(both.asked());
        // Approval outranks the question, mirroring the transcript's own
        // phase priority.
        assert_eq!(both.row_status(Local::now()).text().as_ref(), "Needs approval");
        let mut future = entry("future");
        future.attention = vec![Attention::Other("somethingNew".into())];
        assert!(!future.needs_approval());
        assert!(!future.asked());
        assert_eq!(future.row_status(Local::now()).text().as_ref(), "No reply yet");
        let mut empty = entry("empty");
        empty.attention = Vec::new();
        assert!(!empty.needs_approval());
        assert!(!empty.asked());
    }

    /// The `session/statusChanged` broadcast folds whole truth, not a
    /// delta: flags ride present-only-when-nonempty, so absence clears.
    #[test]
    fn status_changed_folds_running_and_attention() {
        let mut row = entry("s");
        row.turns = 2;
        row.apply_status_changed(true, &[Attention::ApprovalPending]);
        assert!(row.running);
        assert!(row.needs_approval());
        assert_eq!(row.row_status(Local::now()).text().as_ref(), "Needs approval");
        // The stand-down clears both: no flags, no longer running.
        row.apply_status_changed(false, &[]);
        assert!(!row.running);
        assert!(!row.needs_approval());
        assert!(!row.asked());
        assert!(row.row_status(Local::now()).text().starts_with("Settled"));
    }

    /// The turn's terminal folds into the row: a failure records its
    /// message for `Failed`; success (or a blank message) records nothing
    /// and stands a recorded error down.
    #[test]
    fn turn_outcome_records_and_stands_down_the_error() {
        let mut row = entry("s");
        row.turns = 2;
        row.apply_turn_outcome(true, Some("modelError: overloaded"));
        assert_eq!(row.last_error.as_deref(), Some("modelError: overloaded"));
        assert!(row.row_status(Local::now()).text().starts_with("Failed"));
        // A blank message is not an error worth keeping.
        let mut blank = entry("b");
        blank.turns = 1;
        blank.apply_turn_outcome(true, Some("   "));
        assert_eq!(blank.last_error, None);
        assert!(blank.row_status(Local::now()).text().starts_with("Settled"));
        // Success stands the recorded error down.
        row.apply_turn_outcome(false, None);
        assert_eq!(row.last_error, None);
        assert!(row.row_status(Local::now()).text().starts_with("Settled"));
    }

    /// The hover detail carries the whole picture and omits what the app
    /// does not know: full title/ask, the status with its detail, branch,
    /// turns, last change and the path footer, in card order — with the
    /// pending question standing the reply line down for the attention box.
    #[test]
    fn detail_data_carries_the_full_picture_in_card_order() {
        use aui::nav::detail_keys;
        let now = Local::now();
        let mut full = entry("full");
        full.label = "auth-session-refresh".into();
        full.last_ask = Some("Refresh the session tokens".into());
        full.description = "Which bucket should I use?".into();
        full.pending_question = Some("Which bucket for staging?".into());
        full.project_name = Some("acme-web".into());
        full.branch = Some("feature/auth-refresh".into());
        full.turns = 5;
        full.updated = now - chrono::Duration::minutes(8);
        let data = full.detail_data(now);
        assert_eq!(data.pending_question.as_deref(), Some("Which bucket for staging?"));
        assert_eq!(data.pending_approval, None);
        assert_eq!(
            detail_keys(&data),
            vec!["Title", "Ask", "Attention", "Branch", "Turns", "Updated", "Path"]
        );
        assert_eq!(data.status.as_ref().map(|s| s.text().to_string()).as_deref(), Some("Asked: \"Which bucket for staging?\""));
        assert_eq!(data.updated.as_deref(), Some("8m ago"));
        // A failure carries the terminal error on its status; a bare row
        // draws title, the muted reply line, turns and age only.
        let mut failed = entry("failed");
        failed.turns = 2;
        failed.last_error = Some("modelError: overloaded".into());
        let data = failed.detail_data(now);
        assert_eq!(data.status.as_ref().map(|s| s.text().to_string()).as_deref(), Some("Failed · modelError: overloaded"));
        assert_eq!(detail_keys(&data), vec!["Title", "Reply", "Turns", "Updated"]);
        assert_eq!(detail_keys(&entry("fresh").detail_data(now)), vec!["Title", "Reply", "Turns", "Updated"]);
    }

    /// Every state through the real `summary()`: the context line keeps one
    /// line and the status line is always set, so every row is three lines
    /// tall whatever the session is in. (Provisional rows are the one
    /// exception: their status line stays unset, which the library draws as
    /// the same-height empty space — covered beside the provisional
    /// constructor, not here.)
    #[test]
    fn every_state_keeps_the_three_line_height() {
        use aui::nav::context_line_kind;
        use Attention;
        let now = Local::now();
        let mut running = entry("running");
        running.running = true;
        running.turn_started = Some(now);
        let mut pending = entry("pending");
        pending.title_pending = true;
        let mut lined = entry("lined");
        lined.last_ask = Some("tighten validation".into());
        lined.description = "patched the validator".into();
        let mut approval = entry("approval");
        approval.attention = vec![Attention::ApprovalPending];
        approval.last_ask = Some("install it".into());
        let mut asked = entry("asked");
        asked.pending_question = Some("Which bucket?".into());
        let mut settled = entry("settled");
        settled.turns = 3;
        settled.description = "patched the validator".into();
        let mut failed = entry("failed");
        failed.turns = 2;
        failed.last_error = Some("boom".into());
        let mut branched = entry("branched");
        branched.turns = 2;
        branched.project_name = Some("acme-web".into());
        branched.branch = Some("feature-x".into());
        let fresh = entry("fresh");
        for built in [&running, &pending, &lined, &approval, &asked, &settled, &failed, &branched, &fresh] {
            let summary = built.summary(now);
            let kind = context_line_kind(&summary);
            assert_eq!(kind.lines(), 1, "{kind:?} keeps one context line");
            assert!(kind.truncate(), "{kind:?} ellipsizes at the row's width");
            assert!(!kind.wraps(), "{kind:?} never wraps onto another line");
            assert!(summary.status.is_some(), "every state sets the status line");
        }
    }

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
    fn wire_session() -> ListedRow {
        ListedRow {
            session_id: "s1".into(),
            created_at: "2026-09-13T10:00:00Z".into(),
            updated_at: "2026-09-13T10:00:00Z".into(),
            status: Some("idle".into()),
            ..Default::default()
        }
    }

    /// A plain idle row, for tests here and in `attention`.
    pub fn entry(id: &str) -> SessionEntry {
        SessionEntry {
            id: id.to_owned(),
            label: "x".into(),
            updated: Local::now(),
            created: Local::now(),
            running: false,
            turns: 0,
            hidden: false,
            pinned: false,
            archived: false,
            description: String::new(),
            replayed: false,
            provider: ProviderId::Muse,
            named: false,
            needs_title: false,
            side_marker: false,
            title_pending: false,
            last_ask: None,
            local: false,
            provisional: false,
            turns_unknown: false,
            workspace: None,
            project: None,
            project_name: None,
            attention: Vec::new(),
            approval_command: None,
            pending_question: None,
            turn_started: None,
            last_error: None,
            unread: false,
            branch: None,
            tasks_running: 0,
            pages: 0,
            pull_request: None,
        }
    }

    /// The row carries its running tasks for the trailing slot's
    /// "N running" (docs/82 6.5); nothing rides the meta line any more.
    #[test]
    fn running_tasks_ride_the_row_not_the_meta_line() {
        let now = Local::now();
        let idle = entry("idle");
        assert_eq!(idle.summary(now).running_tasks, 0);
        let mut busy = entry("busy");
        busy.tasks_running = 3;
        let row = busy.summary(now);
        assert_eq!(row.running_tasks, 3);
        assert!(row.meta.is_empty(), "the old meta hint is gone: {:?}", row.meta);
    }

    /// docs/82 7.3: a row's pull request rides its branch — on the hover
    /// card's meta row and the summary line's `project · branch` — in the
    /// words of its state; no branch, nowhere to ride; the trailing slot
    /// and the meta items stay as they were.
    #[test]
    fn a_rows_pull_request_rides_its_branch() {
        use baaz_core::git_ops::{PrState, RowPr};
        let now = Local::now();
        let mut row = entry("pr");
        row.branch = Some("fix-checkout".into());
        row.project_name = Some("acme-web".into());
        for (state, failing, words) in [
            (PrState::Open, 0, "PR #12 open · fix-checkout"),
            (PrState::Draft, 0, "PR #12 draft · fix-checkout"),
            (PrState::Open, 1, "PR #12 · 1 check failing · fix-checkout"),
            (PrState::Merged, 1, "PR #12 merged · fix-checkout"),
            (PrState::Closed, 0, "PR #12 closed · fix-checkout"),
        ] {
            row.pull_request = Some(RowPr { number: 12, state, failing });
            assert_eq!(row.detail_data(now).branch.as_deref(), Some(words));
            let summary = row.summary(now);
            assert_eq!(summary.branch.as_deref(), Some(words));
            assert!(summary.meta.is_empty());
            assert_eq!(aui::nav::project_branch_text(&summary).as_deref(), Some(format!("acme-web · {words}").as_str()));
        }
        row.branch = None;
        assert_eq!(row.detail_data(now).branch, None, "no branch: the PR has nothing to ride");
    }

    /// The grouping tests' roots on disk: availability hides a missing root,
    /// so fake `/work` paths would group everything into Unfiled.
    /// One shared base per test-binary run; `create_dir_all` is idempotent
    /// across the parallel tests that share it.
    fn roots_base() -> std::path::PathBuf {
        let base = std::env::temp_dir().join(format!("baaz-sidebar-{}", std::process::id()));
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
            project("p-baaz", "baaz", 1, false),
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
        let view = GroupView { closed, expanded, active, active_project: None, pending: None };
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
        let answers = projects.measure_availability();
        projects.refresh_availability(&answers);
        let entries = vec![
            grouped_entry("s1", Some("p-baaz"), Some("ws"), 0),
            grouped_entry("s2", Some("p-gone"), Some("ws"), 0),
        ];
        let Grouping::Project(groups) = by_project(&entries, &projects) else {
            panic!("project grouping must yield project groups");
        };
        // No group for the missing root — but it stays adopted.
        assert!(groups.iter().all(|g| g.id.as_ref() != "p-gone"));
        assert!(projects.find("p-gone").is_some());
        // Its session is not lost: it falls back to Unfiled with
        // Baaz session.
        let group = folded_group(&groups, "p-baaz");
        assert_eq!(group.sessions.len(), 1);
        let other = groups.last().expect("other group");
        assert_eq!(other.id.as_ref(), OTHER_GROUP);
        assert_eq!(other.count.as_ref(), "1");
    }

    #[test]
    fn three_projects_and_two_strays_group_into_four() {
        let projects = grouped_projects();
        let entries = vec![
            grouped_entry("s1", Some("p-baaz"), Some("/work/p-baaz"), 0),
            grouped_entry("s2", Some("p-baaz"), Some("/work/p-baaz"), 2),
            grouped_entry("s3", Some("p-agentic"), Some("/work/p-agentic"), 1),
            grouped_entry("s4", None, Some("/tmp/stray-one"), 0),
            grouped_entry("s5", None, Some("/tmp/stray-two"), 3),
        ];
        let Grouping::Project(groups) = by_project(&entries, &projects) else {
            panic!("project grouping must yield project groups");
        };
        // Name order, never recency: agentic-ui, baaz,
        // empty — even though baaz rows are the newest — then the unfiled
        // group last.
        assert_eq!(groups.len(), 4);
        assert_eq!(groups[0].id.as_ref(), "p-agentic");
        assert_eq!(groups[1].id.as_ref(), "p-baaz");
        assert_eq!(groups[2].id.as_ref(), "p-empty");
        assert_eq!(groups[1].count.as_ref(), "2");
        // No count at all rather than "0": the pill says how much is inside,
        // and at zero the absent rows already say it (audit 2026-09-13).
        assert_eq!(groups[2].count.as_ref(), "", "an empty project carries no count");
        assert!(groups[2].open, "an empty project still gets an open group row");
        let other = &groups[3];
        assert_eq!(other.id.as_ref(), OTHER_GROUP);
        assert_eq!(other.name.as_ref(), UNFILED_LABEL);
        assert!(other.muted);
        // Open unless the person closed it, like every other group: a
        // session started before any project was adopted lands here, and a
        // group that started shut would hide it.
        assert!(other.open, "the unfiled group starts open");
        assert_eq!(other.count.as_ref(), "2");
        assert_eq!(other.sessions.len(), 2, "an open group carries its rows");
        // No coloured marks anywhere: every group row is the plain default.
        assert!(groups.iter().all(|g| g.mark.is_none()), "no group row carries a mark");
        // Newest first inside the group.
        let group = &groups[1];
        assert_eq!(group.sessions[0].id.as_ref(), "s1");
        assert_eq!(group.sessions[1].id.as_ref(), "s2");
    }

    /// Sessions that resolve to no project land after every adoption: "Other
    /// workspaces" is always last, whatever the names say.
    #[test]
    fn other_workspaces_is_last() {
        let projects = grouped_projects();
        let entries = vec![
            grouped_entry("s1", Some("p-baaz"), Some("/work/p-baaz"), 0),
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
        projects.projects.iter_mut().find(|p| p.id == "p-baaz").expect("baaz").pinned = true;
        let entries = vec![
            grouped_entry("s1", Some("p-baaz"), Some("/work/p-baaz"), 5),
            grouped_entry("s3", Some("p-agentic"), Some("/work/p-agentic"), 0),
        ];
        let Grouping::Project(groups) = by_project(&entries, &projects) else {
            panic!("project grouping must yield project groups");
        };
        assert_eq!(groups[0].id.as_ref(), "p-baaz");
        assert_eq!(groups[1].id.as_ref(), "p-agentic");
    }

    #[test]
    fn a_running_session_marks_its_group() {
        let projects = grouped_projects();
        let mut entries = vec![grouped_entry("s1", Some("p-baaz"), Some("/work/p-baaz"), 0)];
        entries[0].running = true;
        let Grouping::Project(groups) = by_project(&entries, &projects) else {
            panic!("project grouping must yield project groups");
        };
        let group = folded_group(&groups, "p-baaz");
        assert_eq!(group.state, Some(AgentState::Running));
        // …while the branch stays off the row until its flag is on.
        assert_eq!(group.trailing.as_deref(), None);
        assert!(!group.chevron);
        assert!(!group.current_bar);
    }

    /// Task 3.2: a project header carries its sessions' strongest state —
    /// needs you over failed over working over unread — and nothing when
    /// all are idle.
    #[test]
    fn a_group_rolls_up_its_strongest_state() {
        let projects = grouped_projects();
        let rolled = |entries: &[SessionEntry]| {
            let Grouping::Project(groups) = by_project(entries, &projects) else {
                panic!("project grouping must yield project groups");
            };
            let group = folded_group(&groups, "p-baaz");
            (group.state, group.unread)
        };
        let row = |id: &str| grouped_entry(id, Some("p-baaz"), Some("/work/p-baaz"), 1);
        let mut running = row("s1");
        running.running = true;
        let mut waiting = row("s2");
        waiting.attention = vec![Attention::ApprovalPending];
        let mut failed = row("s3");
        failed.last_error = Some("boom".into());
        let mut unread = row("s4");
        unread.unread = true;
        let idle = row("s5");
        assert_eq!(rolled(&[running.clone(), waiting.clone(), failed.clone(), unread.clone()]), (Some(AgentState::Waiting), false));
        assert_eq!(rolled(&[running.clone(), failed.clone(), unread.clone()]), (Some(AgentState::Failed), false));
        assert_eq!(rolled(&[running.clone(), unread.clone()]), (Some(AgentState::Running), false));
        assert_eq!(rolled(&[idle.clone(), unread]), (Some(AgentState::Idle), true));
        assert_eq!(rolled(&[idle]), (None, false));
    }

    /// The two flags are state only: the grouping passes each one through
    /// to the row, and the branch trails only when its flag is on. No flag
    /// asks the leading-box chevron (the switch is gone, docs/83 §9).
    #[test]
    fn group_chrome_follows_the_layout_flags() {
        let projects = grouped_projects();
        let entries = vec![grouped_entry("s1", Some("p-baaz"), Some("/work/p-baaz"), 0)];
        let mut branches = HashMap::new();
        branches.insert("p-baaz".to_owned(), "main".to_owned());
        let layout = crate::layout::Layout { group_bar: true, group_branch: true, ..Default::default() };
        let empty = HashSet::new();
        let view = GroupView { closed: &empty, expanded: &empty, active: None, active_project: None, pending: None };
        let Grouping::Project(groups) =
            grouping_by_project(&entries, &projects, &branches, &view, &layout, Local::now())
        else {
            panic!("project grouping must yield project groups");
        };
        let group = folded_group(&groups, "p-baaz");
        assert!(!group.chevron, "no switch asks the leading-box chevron");
        assert!(!group.current_bar, "the retired group_bar flag never draws a second bar");
        assert_eq!(group.trailing.as_deref(), Some("main"));
    }

    #[test]
    fn a_closed_group_keeps_its_count_but_hides_its_rows() {
        let projects = grouped_projects();
        let entries = vec![grouped_entry("s1", Some("p-baaz"), Some("/work/p-baaz"), 0)];
        let mut closed = HashSet::new();
        closed.insert("p-baaz".to_owned());
        let Grouping::Project(groups) = by_project_view(&entries, &projects, &closed, &HashSet::new(), None)
        else {
            panic!("project grouping must yield project groups");
        };
        let group = folded_group(&groups, "p-baaz");
        assert!(!group.open);
        assert_eq!(group.count.as_ref(), "1");
        assert!(group.sessions.is_empty());
        // …and the unfiled group reads that set exactly the same way,
        // rather than inverted as it once did.
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
        let other = groups.last().expect("unfiled group");
        assert!(!other.open, "closing the unfiled group closes it");
        assert_eq!(other.count.as_ref(), "2", "a closed group still counts what is inside");
        assert!(other.sessions.is_empty());

        // Open, it carries its rows: each one named by its workspace's
        // folder, newest first.
        let Grouping::Project(groups) =
            by_project_view(&entries, &projects, &HashSet::new(), &HashSet::new(), None)
        else {
            panic!("project grouping must yield project groups");
        };
        let other = groups.last().expect("unfiled group");
        assert!(other.open);
        assert_eq!(other.sessions.len(), 2);
        assert_eq!(other.sessions[0].id.as_ref(), "s4");
        assert_eq!(other.sessions[0].repo.as_deref(), Some("stray"));
        assert_eq!(other.sessions[1].repo.as_deref(), Some("stray-two"));
    }

    #[test]
    fn pinned_rows_lead_their_group_newest_first() {
        let projects = grouped_projects();
        let mut entries = vec![
            grouped_entry("s1", Some("p-baaz"), Some("/work/p-baaz"), 0),
            grouped_entry("s2", Some("p-baaz"), Some("/work/p-baaz"), 1),
        ];
        entries[1].pinned = true;
        let Grouping::Project(groups) = by_project(&entries, &projects) else {
            panic!("project grouping must yield project groups");
        };
        let group = folded_group(&groups, "p-baaz");
        let ids: Vec<&str> = group.sessions.iter().map(|s| s.id.as_ref()).collect();
        assert_eq!(ids, vec!["s2", "s1"]);
        // The pin flag reaches the library row, which is what draws the
        // meta-line glyph and flips the tray button to `PinOff` (P5).
        assert!(group.sessions[0].pinned);
        assert!(!group.sessions[1].pinned);
    }

    /// Nine sessions fold to the five newest, holding
    /// four back. The count still names all nine.
    #[test]
    fn nine_sessions_fold_to_five_plus_four_hidden() {
        let projects = grouped_projects();
        let entries = nine_sessions("p-baaz");
        let Grouping::Project(groups) = by_project(&entries, &projects) else {
            panic!("project grouping must yield project groups");
        };
        let group = folded_group(&groups, "p-baaz");
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
        let entries = nine_sessions("p-baaz");
        let mut expanded = HashSet::new();
        expanded.insert("p-baaz".to_owned());
        let Grouping::Project(groups) = by_project_view(&entries, &projects, &HashSet::new(), &expanded, None)
        else {
            panic!("project grouping must yield project groups");
        };
        let group = folded_group(&groups, "p-baaz");
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
        let entries = nine_sessions("p-baaz");
        let Grouping::Project(groups) =
            by_project_view(&entries, &projects, &HashSet::new(), &HashSet::new(), Some("s8"))
        else {
            panic!("project grouping must yield project groups");
        };
        let group = folded_group(&groups, "p-baaz");
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
        let entries = nine_sessions("p-baaz");
        let empty = HashSet::new();
        let view = GroupView { closed: &empty, expanded: &empty, active: None, active_project: None, pending: Some("s8") };
        let layout = crate::layout::Layout::default();
        let Grouping::Project(groups) =
            grouping_by_project(&entries, &projects, &HashMap::new(), &view, &layout, Local::now())
        else {
            panic!("project grouping must yield project groups");
        };
        let group = folded_group(&groups, "p-baaz");
        let ids: Vec<&str> = group.sessions.iter().map(|s| s.id.as_ref()).collect();
        assert_eq!(ids, vec!["s0", "s1", "s2", "s3", "s4", "s8"]);
        assert_eq!(group.fold, Some((3, false)));
    }

    /// Pinned rows always show and never count toward the five: two pinned
    /// ancients plus the five newest make seven visible, two held back.
    #[test]
    fn pinned_rows_never_count_toward_the_five() {
        let projects = grouped_projects();
        let mut entries = nine_sessions("p-baaz");
        entries[7].pinned = true;
        entries[8].pinned = true;
        let Grouping::Project(groups) = by_project(&entries, &projects) else {
            panic!("project grouping must yield project groups");
        };
        let group = folded_group(&groups, "p-baaz");
        let ids: Vec<&str> = group.sessions.iter().map(|s| s.id.as_ref()).collect();
        assert_eq!(ids, vec!["s7", "s8", "s0", "s1", "s2", "s3", "s4"]);
        assert_eq!(group.fold, Some((2, false)));
    }

    /// The project-grouping twin of
    /// [`five_hundred_sidebar_rows_cost_less_from_the_cache`]: the pure half
    /// the sidebar pane builds in project mode, timed against the cached
    /// frame, which hands the same `Rc` back. The assertion is only the
    /// ordering, so the test is not a timing flake.
    #[test]
    fn five_hundred_project_rows_cost_less_from_the_cache() {
        const N: usize = 500;
        const FRAMES: usize = 20;
        let now = Local::now();
        let projects = grouped_projects();
        let ids = ["p-baaz", "p-agentic", "p-empty"];
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
        let view = GroupView { closed: &empty, expanded: &empty, active: None, active_project: None, pending: None };
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

    fn index_entry(title: &str, prompt: Option<&str>) -> IndexEntry {
        IndexEntry {
            title: title.to_owned(),
            first_user_prompt: prompt.map(str::to_owned),
            updated_at_us: Some(1_700_000_000_000_000),
            ..IndexEntry::default()
        }
    }

    #[test]
    fn provisional_rows_leave_the_status_line_unset() {
        let projects = Projects::default();
        let mut cache = crate::projects::CanonicalCache::default();
        let now = Local::now();
        let provisional =
            SessionEntry::provisional("s", &index_entry("Fix the parser", Some("why panic")), None, &projects, &mut cache)
                .expect("labelled");
        let summary = provisional.summary(now);
        // No turn count, no status verb, no invented idleness: the meta line
        // is absent — which the library draws as empty space at the same
        // row height, so the real row fills it in without a layout jump.
        assert!(summary.status.is_none());
        // ... while the label and the elapsed tag already read.
        assert_eq!(summary.name.as_ref(), "Fix the parser");
        assert!(!summary.elapsed.is_empty());
        // A joined row always carries its status line, provisional or not.
        let mut wire = entry("s");
        wire.turns = 12;
        assert!(wire.summary(now).status.is_some());
    }

    fn provider_record() -> crate::provider_sessions::ProviderSessionRecord {
        crate::provider_sessions::ProviderSessionRecord {
            provider: "claude-code".into(),
            session_id: "s-provider".into(),
            workspace: None,
            created_ms: 1_700_000_000_000,
            updated_ms: 1_700_000_000_000,
            turns: 2,
            origin: None,
        }
    }

    /// The W5 sidebar arm: a provider session appears in the merged list
    /// with its provider mark, its title and its byline — and a muse row
    /// carries no mark.
    #[test]
    fn provider_rows_wear_their_providers_mark() {
        let projects = Projects::default();
        let now = Local::now();
        let claude = SessionEntry::provider_row(&provider_record(), None, &projects).expect("a registered provider");
        assert_eq!(claude.label, UNNAMED);
        assert!(!claude.needs_title, "no session/read may spend itself on a provider id");
        let summary = claude.summary(now);
        assert!(
            summary.providers.contains(&aui_icons::Provider::Claude),
            "a Claude Code row wears the Claude mark, drew {:?}",
            summary.providers
        );
        let mut codex = provider_record();
        codex.provider = "codex".into();
        let row = SessionEntry::provider_row(&codex, None, &projects).expect("a registered provider");
        assert!(row.summary(now).providers.contains(&aui_icons::Provider::Codex));
        let muse = SessionEntry::join(&wire_session(), None, None, &projects);
        assert_eq!(muse.summary(now).providers, vec![aui_icons::Provider::Muse], "a muse row wears Muse's mark (docs/82 5.5)");
    }

    /// [`lift_pinned`]: pinned rows leave their project groups, in group
    /// then row order, and land in the returned vec; the groups' counts
    /// keep counting them.
    #[test]
    fn lift_pinned_moves_pinned_rows_out_of_project_groups_in_order() {
        let projects = grouped_projects();
        let mut entries = vec![
            grouped_entry("a1", Some("p-agentic"), Some("/w/a"), 0),
            grouped_entry("a2", Some("p-agentic"), Some("/w/a"), 1),
            grouped_entry("b1", Some("p-baaz"), Some("/w/b"), 0),
            grouped_entry("b2", Some("p-baaz"), Some("/w/b"), 1),
        ];
        entries[1].pinned = true; // a2
        entries[2].pinned = true; // b1
        let (grouping, pinned) = lift_pinned(by_project(&entries, &projects));
        let ids: Vec<&str> = pinned.iter().map(|s| s.id.as_ref()).collect();
        assert_eq!(ids, ["a2", "b1"], "group order, then row order");
        let Grouping::Project(groups) = grouping else { panic!("still a project grouping") };
        let left = |id: &str| -> Vec<String> {
            folded_group(&groups, id).sessions.iter().map(|s| s.id.to_string()).collect()
        };
        assert_eq!(left("p-agentic"), ["a1"]);
        assert_eq!(left("p-baaz"), ["b2"]);
        assert_eq!(folded_group(&groups, "p-baaz").count.as_ref(), "2", "the head still counts the lifted row");
    }

    /// [`lift_pinned`] lifts pinned rows out of a Date grouping too: the
    /// v2 screen draws its own Pinned section, and the flattener would
    /// otherwise add a second one.
    #[test]
    fn lift_pinned_lifts_pinned_rows_out_of_a_date_grouping() {
        let mut pinned_entry = entry("p1");
        pinned_entry.pinned = true;
        pinned_entry.turns = 1;
        let entries = vec![pinned_entry, entry("p2")];
        let before = grouping_at(&entries, Local::now());
        let before_count = match &before {
            Grouping::Date(groups) => groups.iter().map(|g| g.sessions.len()).sum::<usize>(),
            _ => panic!("date grouping"),
        };
        let (after, pinned) = lift_pinned(before);
        assert_eq!(pinned.iter().map(|s| s.id.to_string()).collect::<Vec<_>>(), vec!["p1".to_string()]);
        let Grouping::Date(groups) = after else { panic!("still a date grouping") };
        assert_eq!(groups.iter().map(|g| g.sessions.len()).sum::<usize>(), before_count - 1);
        assert!(groups.iter().all(|g| !g.sessions.is_empty()), "no empty bucket survives the lift");
        assert!(groups.iter().flat_map(|g| &g.sessions).all(|s| !s.pinned));
    }
}
