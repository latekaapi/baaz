//! The Changes screen's scopes (docs/82 6.3): what the list is a diff of.
//!
//! * **All changes** — the working tree against `HEAD`, the screen as it
//!   always was (the default, and the only scope the commit box, Push and
//!   the PR act in).
//! * **This session** — the session's first pre-turn checkpoint against the
//!   folder as it is now (written as a tree through a temporary index, so
//!   untracked files count).
//! * **Last turn** — the last finished turn's `pre..post`.
//! * **Turn N** — a turn's own `pre..post`, transient: what the
//!   transcript's `+N −M` chip opens.
//!
//! The checkpoints are [`crate::checkpoints`]' refs, read here as a
//! [`CheckpointIndex`] for the open session (its uuid from `baaz.db`, its
//! refs from the repository, numbered and ordered by its transcript). The
//! scope row shows only when that index has a finished turn: a folder
//! without git, a session that has not finished a turn here, or a capture
//! (unless a probe step loads the fixture index) shows today's screen and
//! no control (rule 13). Every read runs in the background; a file's diff
//! is read when its row opens, never the whole list.

use std::path::Path;

#[cfg(feature = "probe")]
use aui::protocol::ChangeKind;
use aui::protocol::FileChange;
use gpui::{Context, SharedString};

use crate::right_pane::RightPane;

/// What the Changes list is a diff of (see the module doc).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum ChangesScope {
    #[default]
    All,
    Session,
    Last,
    /// One turn, by id (the chip's).
    Turn(String),
}

impl ChangesScope {
    /// `all`, `session` or `last` (the probe verb's words).
    #[cfg(feature = "probe")]
    pub(crate) fn parse(text: &str) -> Option<Self> {
        match text {
            "all" => Some(Self::All),
            "session" => Some(Self::Session),
            "last" => Some(Self::Last),
            _ => None,
        }
    }
}

/// One turn of the open session that has a checkpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct IndexedTurn {
    pub turn_id: String,
    /// Its place among the session's agent turns, from 1.
    pub number: usize,
    pub pre: Option<String>,
    pub post: Option<String>,
}

impl IndexedTurn {
    fn ends(&self) -> Option<(String, String)> {
        Some((self.pre.clone()?, self.post.clone()?))
    }
}

/// The open session's checkpoints, in transcript order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CheckpointIndex {
    pub session_id: String,
    pub turns: Vec<IndexedTurn>,
}

impl CheckpointIndex {
    /// The index of `session_id` from its refs (by ref component) and its
    /// agent turns in order: turns with no checkpoint are left out.
    pub(crate) fn from_refs(session_id: &str, order: &[String], refs: &std::collections::HashMap<String, crate::checkpoints::TurnRefs>) -> Self {
        let turns = order
            .iter()
            .enumerate()
            .filter_map(|(i, turn_id)| {
                let found = refs.get(&crate::checkpoints::component(turn_id))?;
                Some(IndexedTurn { turn_id: turn_id.clone(), number: i + 1, pre: found.pre.clone(), post: found.post.clone() })
            })
            .collect();
        Self { session_id: session_id.to_owned(), turns }
    }

    /// The turn `turn_id`, when it has both checkpoints.
    pub(crate) fn finished(&self, turn_id: &str) -> Option<&IndexedTurn> {
        self.turns.iter().find(|turn| turn.turn_id == turn_id && turn.ends().is_some())
    }

    /// The last turn with both checkpoints.
    pub(crate) fn last(&self) -> Option<&IndexedTurn> {
        self.turns.iter().rev().find(|turn| turn.ends().is_some())
    }

    /// Whether the scope row has anything to offer: a finished turn.
    pub(crate) fn offers(&self) -> bool {
        self.last().is_some()
    }

    /// `scope`'s two ends: `(from, Some(to))` between checkpoints, `(from,
    /// None)` to the folder as it is now. `None` for All changes or a scope
    /// this index cannot answer.
    pub(crate) fn ends(&self, scope: &ChangesScope) -> Option<(String, Option<String>)> {
        match scope {
            ChangesScope::All => None,
            ChangesScope::Session => self.turns.iter().find_map(|turn| turn.pre.clone()).map(|pre| (pre, None)),
            ChangesScope::Last => self.last()?.ends().map(|(pre, post)| (pre, Some(post))),
            ChangesScope::Turn(id) => self.finished(id)?.ends().map(|(pre, post)| (pre, Some(post))),
        }
    }

    /// The words the scope row and the list's caption use.
    pub(crate) fn label(&self, scope: &ChangesScope) -> SharedString {
        match scope {
            ChangesScope::All => "All changes".into(),
            ChangesScope::Session => "This session".into(),
            ChangesScope::Last => "Last turn".into(),
            ChangesScope::Turn(id) => match self.turns.iter().find(|turn| &turn.turn_id == id) {
                Some(turn) => format!("Turn {}", turn.number).into(),
                None => "Turn".into(),
            },
        }
    }
}

/// The list a scope other than All changes shows: its ends, its files, and
/// the working tree's generation it was read at (This session follows it).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ScopedList {
    pub scope: ChangesScope,
    pub from: String,
    /// A commit, or the tree the folder was written as.
    pub to: String,
    pub files: Vec<FileChange>,
    pub generation: u64,
}

/// The scope state the Changes screen keeps (in its `ChangesUi`, so it
/// resets with the root).
#[derive(Default)]
pub(crate) struct ScopeState {
    pub scope: ChangesScope,
    pub index: Option<CheckpointIndex>,
    index_reading: bool,
    /// A read was asked for while one ran: read again when it lands.
    index_stale: bool,
    pub scoped: Option<ScopedList>,
    scoped_reading: bool,
    /// The chip's turn and files, waiting on a fresh index.
    chip: Option<(String, Vec<String>)>,
    /// Files the chip opens once the turn's list lands.
    open_when_listed: Option<Vec<String>>,
    /// The open session the pane last drew for (set each frame).
    pub shown_session: Option<String>,
    /// A probe step asked for the fixture index (captures read no refs).
    #[cfg(feature = "probe")]
    pub fixture: bool,
}

impl ScopeState {
    /// The scope row's choices and which is chosen, when it shows: only for
    /// the open session's index, and only when it has a finished turn.
    pub(crate) fn row(&self, session_id: Option<&str>) -> Option<(Vec<SharedString>, Vec<ChangesScope>, usize)> {
        let index = self.index.as_ref().filter(|index| Some(index.session_id.as_str()) == session_id && index.offers())?;
        let mut scopes = vec![ChangesScope::All, ChangesScope::Session, ChangesScope::Last];
        if let ChangesScope::Turn(_) = &self.scope {
            scopes.push(self.scope.clone());
        }
        let active = scopes.iter().position(|scope| scope == &self.scope).unwrap_or(0);
        Some((scopes.iter().map(|scope| index.label(scope)).collect(), scopes, active))
    }

    /// The shown list's ends when it is not All changes.
    pub(crate) fn scoped_ends(&self) -> Option<(String, String)> {
        self.scoped.as_ref().filter(|list| list.scope == self.scope && self.scope != ChangesScope::All).map(|list| (list.from.clone(), list.to.clone()))
    }
}

/// What a Changes read brought back ([`RightPane::land`]).
pub(crate) enum Landed {
    /// The fixture index (a probe step), or one read and already settled.
    Index(CheckpointIndex),
    /// The background index read.
    IndexRead(CheckpointIndex),
    /// A scope's list (`None`: it could not be read).
    Scoped(ChangesScope, Option<ScopedList>),
    /// The scope row's pick.
    Picked(ChangesScope),
    /// A row's diff, read between `ends` (`None`: against `HEAD`).
    Diff { path: String, diff: Option<aui::protocol::Diff>, ends: Option<(String, String)> },
    /// An unchanged run after hunk `hunk` of `path`.
    Gap { path: String, hunk: usize, lines: Vec<String> },
}

/// What the background index read needs from the session.
struct SessionFacts {
    id: String,
    provider: String,
    order: Vec<String>,
}

/// The fixture index a probe step loads: two finished turns.
#[cfg(feature = "probe")]
fn fixture_index(session_id: &str, order: &[String]) -> CheckpointIndex {
    let ids: Vec<String> = if order.len() >= 2 { order[order.len() - 2..].to_vec() } else { vec!["fixture-1".into(), "fixture-2".into()] };
    let turns = ids
        .into_iter()
        .enumerate()
        .map(|(i, turn_id)| IndexedTurn { turn_id, number: i + 1, pre: Some(format!("fixture-pre-{i}")), post: Some(format!("fixture-post-{i}")) })
        .collect();
    CheckpointIndex { session_id: session_id.to_owned(), turns }
}

/// The fixture list for a scope: a slice of the fixture working tree.
#[cfg(feature = "probe")]
fn fixture_files(scope: &ChangesScope, all: &[FileChange]) -> Vec<FileChange> {
    match scope {
        ChangesScope::Session => all.to_vec(),
        ChangesScope::Last => all.iter().filter(|f| f.change != ChangeKind::Deleted).take(2).cloned().collect(),
        _ => all.iter().take(1).cloned().collect(),
    }
}

impl RightPane {
    fn session_facts(&self, cx: &Context<Self>) -> Option<SessionFacts> {
        let view = self.facts.session.as_ref()?;
        let rt = view.read(cx).rt(cx);
        Some(SessionFacts { id: rt.session_id.clone(), provider: rt.provider_kind().as_str().to_owned(), order: rt.assistant_turn_ids() })
    }

    /// The open session's id, if any.
    pub(crate) fn scope_session_id(&self, cx: &Context<Self>) -> Option<String> {
        self.facts.session.as_ref().map(|view| view.read(cx).rt(cx).session_id.clone())
    }

    /// After a snapshot: read the session's index when it is missing,
    /// another session's or stale, and follow the working tree in This
    /// session.
    pub(crate) fn scopes_after_snapshot(&mut self, root: &Path, cx: &mut Context<Self>) {
        let session = self.scope_session_id(cx);
        let generation = self.cache.git_generation;
        let state = &mut self.changes_ui(root).scopes;
        let fresh = state.index.as_ref().is_some_and(|index| Some(&index.session_id) == session.as_ref());
        if !fresh {
            // Another session (or none): its scope goes back to All changes.
            if state.scope != ChangesScope::All || state.scoped.is_some() {
                state.scope = ChangesScope::All;
                state.scoped = None;
                self.cache.file_diffs.clear();
                self.cache.gap_lines.clear();
            }
            self.read_index(root, cx);
            return;
        }
        let follow = state.scope == ChangesScope::Session && !state.scoped_reading && state.scoped.as_ref().is_none_or(|list| list.generation != generation);
        if follow {
            self.read_scoped(root, cx);
        }
    }

    /// A turn's checkpoint landed for `session_id`: the index is read again
    /// when that session is the one shown.
    pub(crate) fn note_checkpoints(&mut self, session_id: &str, cx: &mut Context<Self>) {
        if self.scope_session_id(cx).as_deref() != Some(session_id) {
            return;
        }
        let Some((root, _)) = self.facts.project.clone() else { return };
        self.read_index(&root, cx);
    }

    /// Read the open session's index in the background (once at a time; a
    /// read asked for meanwhile follows the one running).
    fn read_index(&mut self, root: &Path, cx: &mut Context<Self>) {
        let facts = self.session_facts(cx);
        let state = &mut self.changes_ui(root).scopes;
        if state.index_reading {
            state.index_stale = true;
            return;
        }
        let Some(facts) = facts else {
            state.index = None;
            return;
        };
        #[cfg(feature = "probe")]
        if state.fixture {
            let index = fixture_index(&facts.id, &facts.order);
            self.land(root, Landed::Index(index), cx);
            return;
        }
        if crate::runtime::captures() {
            return;
        }
        state.index_reading = true;
        state.index_stale = false;
        let root_buf = root.to_path_buf();
        cx.spawn(async move |this, cx| {
            let dir = root_buf.clone();
            let index = cx
                .background_executor()
                .spawn(async move {
                    let (provider, id) = (facts.provider.clone(), facts.id.clone());
                    let state = crate::store::support_dir();
                    let key = crate::db::read(crate::db::path_in(&state), move |db| crate::db::sessions::read_row(&db.conn, &provider, &id).ok().flatten()).flatten();
                    let refs = key.and_then(|key| crate::checkpoints::session_refs(&dir, &key)).unwrap_or_default();
                    CheckpointIndex::from_refs(&facts.id, &facts.order, &refs)
                })
                .await;
            let _ = this.update(cx, |this: &mut Self, cx| this.land(&root_buf, Landed::IndexRead(index), cx));
        })
        .detach();
    }

    /// A fresh index: keep the scope while it still answers, settle a
    /// waiting chip, and re-read the shown list when its ends moved.
    fn index_landed(&mut self, root: &Path, index: CheckpointIndex, cx: &mut Context<Self>) {
        let current = self.scope_session_id(cx);
        if current.as_deref() != Some(index.session_id.as_str()) {
            // The session moved on while this was read.
            self.changes_ui(root).scopes.index = None;
            return;
        }
        let state = &mut self.changes_ui(root).scopes;
        let chip = state.chip.take();
        state.index = Some(index);
        if let Some((turn_id, files)) = chip {
            let has = state.index.as_ref().is_some_and(|index| index.finished(&turn_id).is_some());
            if has {
                state.open_when_listed = Some(files);
                self.switch_scope(root, ChangesScope::Turn(turn_id), cx);
            } else {
                // No checkpoint for that turn (an old session, no git):
                // the chip does what it always did.
                self.switch_scope(root, ChangesScope::All, cx);
                self.open_turn_files(root, &files, cx);
            }
            return;
        }
        let state = &self.changes_ui(root).scopes;
        let answers = state.index.as_ref().is_some_and(|index| index.ends(&state.scope).is_some());
        if state.scope != ChangesScope::All && !answers {
            self.switch_scope(root, ChangesScope::All, cx);
            return;
        }
        let moved = match (state.index.as_ref().and_then(|index| index.ends(&state.scope)), state.scoped.as_ref()) {
            (Some((from, Some(to))), Some(list)) => list.from != from || list.to != to,
            (Some((from, None)), Some(list)) => list.from != from,
            (Some(_), None) => true,
            (None, _) => false,
        };
        if moved {
            self.read_scoped(root, cx);
        }
    }

    /// Show `scope`: the open rows' diffs go (they were another diff's),
    /// and its list is read.
    fn switch_scope(&mut self, root: &Path, scope: ChangesScope, cx: &mut Context<Self>) {
        let state = &mut self.changes_ui(root).scopes;
        if state.scope != scope {
            state.scope = scope;
            state.scoped = None;
            self.cache.file_diffs.clear();
            self.cache.gap_lines.clear();
            self.cache.changes_expanded.remove(root);
        }
        if self.changes_ui(root).scopes.scope != ChangesScope::All {
            self.read_scoped(root, cx);
        }
        self.changes_ui(root).scroll.set_offset(gpui::Point::default());
    }

    /// The scope row's pick (and the probe verb's): `Err` when the row is
    /// not shown or the index cannot answer `scope`.
    pub(crate) fn set_scope(&mut self, root: &Path, scope: ChangesScope, cx: &mut Context<Self>) -> Result<(), String> {
        let session = self.scope_session_id(cx);
        let state = &self.changes_ui(root).scopes;
        if state.row(session.as_deref()).is_none() {
            return Err("no finished turn with checkpoints in this session: the scope row is not shown".into());
        }
        if scope != ChangesScope::All && state.index.as_ref().and_then(|index| index.ends(&scope)).is_none() {
            return Err(format!("no checkpoints answer {scope:?}"));
        }
        self.land(root, Landed::Picked(scope), cx);
        Ok(())
    }

    /// Read the shown scope's list in the background.
    fn read_scoped(&mut self, root: &Path, cx: &mut Context<Self>) {
        let generation = self.cache.git_generation;
        let state = &mut self.changes_ui(root).scopes;
        let scope = state.scope.clone();
        let Some((from, to)) = state.index.as_ref().and_then(|index| index.ends(&scope)) else { return };
        #[cfg(feature = "probe")]
        if state.fixture {
            let all = self.cache.git_for(root).flatten().map(|tree| tree.files).unwrap_or_default();
            let list = ScopedList { files: fixture_files(&scope, &all), scope: scope.clone(), from, to: to.unwrap_or_else(|| "fixture-tree".into()), generation };
            self.land(root, Landed::Scoped(scope, Some(list)), cx);
            return;
        }
        state.scoped_reading = true;
        let root_buf = root.to_path_buf();
        cx.spawn(async move |this, cx| {
            let dir = root_buf.clone();
            let read_scope = scope.clone();
            let list = cx
                .background_executor()
                .spawn(async move {
                    let to = match to {
                        Some(to) => to,
                        None => crate::checkpoints::write_tree(&dir).ok()?,
                    };
                    let files = crate::checkpoints::diff_files(&dir, &from, &to)?;
                    Some(ScopedList { scope: read_scope, from, to, files, generation })
                })
                .await;
            let _ = this.update(cx, |this: &mut Self, cx| this.land(&root_buf, Landed::Scoped(scope, list), cx));
        })
        .detach();
    }

    fn scoped_landed(&mut self, root: &Path, list: ScopedList, cx: &mut Context<Self>) {
        let state = &mut self.changes_ui(root).scopes;
        let moved = state.scoped.as_ref().is_none_or(|old| old.from != list.from || old.to != list.to);
        state.scoped = Some(list);
        let open = state.open_when_listed.take();
        if moved {
            self.cache.file_diffs.clear();
            self.cache.gap_lines.clear();
            self.reload_open_diffs(root, cx);
        }
        if let Some(files) = open {
            self.open_turn_files(root, &files, cx);
        }
    }

    /// Everything a Changes read brings back lands here, and the screen
    /// redraws once: the index, a scope's list, a pick, a row's diff, an
    /// unchanged run.
    pub(crate) fn land(&mut self, root: &Path, landed: Landed, cx: &mut Context<Self>) {
        match landed {
            Landed::Index(index) => self.index_landed(root, index, cx),
            Landed::IndexRead(index) => {
                let state = &mut self.changes_ui(root).scopes;
                state.index_reading = false;
                if std::mem::take(&mut state.index_stale) {
                    self.read_index(root, cx);
                } else {
                    self.index_landed(root, index, cx);
                }
            }
            Landed::Scoped(scope, list) => {
                let state = &mut self.changes_ui(root).scopes;
                state.scoped_reading = false;
                match list {
                    Some(list) if list.scope == state.scope => self.scoped_landed(root, list, cx),
                    Some(_) => {}
                    None => crate::baaz_log!("changes: the {scope:?} diff could not be read"),
                }
            }
            Landed::Picked(scope) => self.switch_scope(root, scope, cx),
            Landed::Diff { path, diff, ends } => {
                // Read for another scope's list: not this one's row.
                if self.changes_ui(root).scopes.scoped_ends() != ends {
                    return;
                }
                match diff {
                    Some(diff) => {
                        self.cache.file_diffs.insert(path, Some(std::sync::Arc::new(diff)));
                    }
                    None => {
                        self.cache.file_diffs.remove(&path);
                    }
                }
            }
            Landed::Gap { path, hunk, lines } => {
                self.cache.gap_lines.insert((path, hunk), std::sync::Arc::new(lines));
            }
        }
        cx.notify();
    }

    /// The transcript's `+N −M` chip: that turn's own diff (Turn N) when
    /// it has checkpoints, else the files in All changes as before. The
    /// index is read fresh first: the turn's refs may have just landed.
    pub(crate) fn open_turn(&mut self, root: &Path, turn_id: &str, files: Vec<String>, cx: &mut Context<Self>) {
        let live = self.changes_ui(root).scopes.fixture_on() || !crate::runtime::captures();
        if self.facts.session.is_none() || !live {
            self.open_turn_files(root, &files, cx);
            return;
        }
        self.changes_ui(root).scopes.chip = Some((turn_id.to_owned(), files));
        self.read_index(root, cx);
    }

    /// The row the shown list has at `path`: the working tree's in All
    /// changes, the scope's list otherwise.
    pub(crate) fn shown_change(&mut self, root: &Path, path: &str) -> Option<FileChange> {
        if self.changes_ui(root).scopes.scope == ChangesScope::All {
            return self.cache.git_for(root).flatten().and_then(|tree| tree.change(path).cloned());
        }
        self.changes_ui(root).scopes.scoped.as_ref().and_then(|list| list.files.iter().find(|f| f.path == path).cloned())
    }
}

impl RightPane {
    /// `changes-scope:`'s pane half: in a capture the fixture index stands
    /// in for refs (a capture never writes or reads them); then the pick,
    /// with [`Self::set_scope`]'s refusals.
    #[cfg(feature = "probe")]
    pub(crate) fn step_scope(&mut self, root: &Path, scope: ChangesScope, cx: &mut Context<Self>) -> crate::steps::StepResult {
        if crate::runtime::captures() {
            if self.cache.git_for(root).is_none() {
                self.refresh_now(cx);
            }
            if !self.changes_ui(root).scopes.fixture {
                self.changes_ui(root).scopes.fixture = true;
                self.read_index(root, cx);
            }
        }
        self.set_scope(root, scope, cx)
    }
}

impl ScopeState {
    /// Whether a probe step loaded the fixture index.
    fn fixture_on(&self) -> bool {
        #[cfg(feature = "probe")]
        {
            self.fixture
        }
        #[cfg(not(feature = "probe"))]
        {
            false
        }
    }
}

