//! The sessions sidebar's model (spec §3.7).
//!
//! `session/list` filtered to the workspace gives identity and timestamps;
//! `~/.local/share/muse/session-index.db` gives the words a person can read.
//! This module joins the two into [`SessionEntry`] rows: their state, words,
//! order, chains and buckets. The app's `sidebar` module projects them onto
//! the library's rows and groupings (`SessionSummary`, `Grouping`).
//!
//! Everything here is pure: a list in, rows out. The application owns the
//! fetching.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Datelike, Local, TimeZone, Utc};

use crate::index::IndexEntry;
use crate::list_cache::ListedRow;
use crate::providers::ProviderId;
use provider::facts::Attention;
#[cfg(test)]
use crate::projects::Projects;
use crate::sessions::SessionMeta;

/// What a session with nothing to be called is called.
///
/// The last resort, and what a whole sidebar reads when every step before it
/// comes up empty. Each of those steps is a real fact about the session.
pub const UNNAMED: &str = "New session";

/// The label the header crumb and the window title show for a row: while a
/// generated title is in flight and the session has no better name yet,
/// the pending placeholder — otherwise the row's own label. A session that
/// already reads from its first prompt keeps that prompt; only an untitled
/// row borrows the placeholder, so the crumb never flickers between two
/// real labels.
pub fn display_label(label: &str, title_pending: bool) -> &str {
    if title_pending && label == UNNAMED {
        PLACEHOLDER_NAMING
    } else {
        label
    }
}

/// The second line while a generated title is still being written.
pub const PLACEHOLDER_NAMING: &str = "Naming this session…";

/// What a session is doing, from the person's side (task 3.1): the one
/// answer [`SessionEntry::session_state`] gives, read by the row's mark, its
/// words, the project header and (stage 3) notifications and the badge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionState {
    /// A turn is running and nothing waits on the person.
    Working,
    /// The session cannot go on without the person.
    NeedsYou(NeedKind),
    /// A turn finished and the person has not read it to the end.
    DoneUnread,
    /// The last turn ended in an error.
    Failed,
    /// Nothing to report.
    Idle,
}

impl SessionState {
    /// How strongly a project header must show this state when it rolls
    /// several sessions up (task 3.2): what needs the person first, then
    /// what went wrong, then what is running, then what is unread.
    fn strength(self) -> u8 {
        match self {
            SessionState::NeedsYou(_) => 4,
            SessionState::Failed => 3,
            SessionState::Working => 2,
            SessionState::DoneUnread => 1,
            SessionState::Idle => 0,
        }
    }

}

/// The strongest state among `rows`, or `None` when every one is idle (or
/// there are none): a project header rolls its sessions up to this.
pub fn strongest_state<'a>(rows: impl IntoIterator<Item = &'a SessionEntry>) -> Option<SessionState> {
    rows.into_iter().map(SessionEntry::session_state).filter(|s| *s != SessionState::Idle).max_by_key(|s| s.strength())
}

/// What a [`SessionState::NeedsYou`] session waits for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NeedKind {
    /// A tool call waits for an approval.
    Approval,
    /// A question waits for an answer.
    Question,
}

/// One row of the sidebar, joined from the wire, the local index and
/// Baaz's own overrides.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionEntry {
    /// The Muse session id.
    pub id: String,
    /// What to call it: see [`SessionEntry::join`].
    pub label: String,
    /// Last activity, as a local time.
    pub updated: DateTime<Local>,
    /// When the session was created: the Sort by "Created" key.
    pub created: DateTime<Local>,
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
    /// Which provider serves this session. The app's `SessionRow::summary` turns it into
    /// the row's provider mark, and the list merge keeps the rows of
    /// providers that keep no list of their own
    /// ([`provider::ProviderTrait::NativeSessionList`]) — a listing never names them.
    pub provider: ProviderId,
    /// Named with `/name` or the row's pencil. A named session with no turns
    /// is somebody's draft, not noise, so the empty filter leaves it alone.
    pub named: bool,
    /// Whether anything but the fallback was found, which is what tells the
    /// application a `session/read` is worth making (finding F10).
    pub needs_title: bool,
    /// Set at join time when the wire (or the index) itself marks this row
    /// as one of Baaz's throwaway title/summary side sessions — the
    /// side-session prompt prefix or the side workspace — with no local
    /// record — or, the same way, as a session another client made
    /// ([`made_elsewhere`]). What keeps such a row hidden across a restart, a scratch
    /// state dir, a relay lane, or a store that never saw it start:
    /// `hidden` is re-derived from this flag on every join and rejoin, and
    /// the app's side-session checks read it beside the explicit record.
    pub side_marker: bool,
    /// A generated title is in flight for this session (auto-titles): the
    /// second line reads the pending placeholder until it lands. Owned by
    /// the titler, which sets it on `turn/started` and clears it on harvest,
    /// timeout or failure; the ladder below reads it, nothing else writes it.
    pub title_pending: bool,
    /// The user's last request in this session, as the free byline excerpt
    /// saw it: the ask half of the two-line byline (auto-summaries). `None`
    /// is "no byline yet" — the ladder falls through to the preview text.
    /// Owned by the byline recorder; the ladder below reads it.
    pub last_ask: Option<String>,
    /// The row's own derived workspace branch (`Session.branch`, muse 1.2.1),
    /// shown on the meta line. The live-change signal stays
    /// `session/branchChanged`; this is the index's last derived value.
    pub branch: Option<String>,
    /// How many commands this session's agent started in Baaz's terminal
    /// still run (docs/82 6.5). Stamped by the window from the terminal
    /// host's task book on every list build — never stored, never on the
    /// wire — and read by the app's `SessionEntry::summary` for the row's
    /// "N running".
    pub tasks_running: u32,
    /// Placed locally at `session/start`: the wire lists a session only
    /// after its log flushes on `turn/completed`, so the window holds this
    /// row meanwhile. [`merge_session_list`] keeps it until the wire lists
    /// its id, then the joined wire row replaces it.
    pub local: bool,
    /// Provisional: built from the local index before `session/list` lands,
    /// carrying only what the index knows (a label and a time) — never turn
    /// counts, running state or the status verb. The app's `SessionRow::summary` leaves the
    /// status line unset for these rows (the library renders that as empty
    /// space at the same height), and the first `session/list` reply replaces
    /// them wholesale. Exempt from the empty filter: their emptiness is
    /// unknown, and filtering on it would keep the sidebar empty until the
    /// wire answers — but only labelled index entries ever become provisional,
    /// which is what keeps the index's own zero-turn rows out.
    pub provisional: bool,
    /// The row's turn count is not known yet: a boot row built from the
    /// index alone, which knows no turn count, so the empty filter leaves
    /// it alone. A boot row joined from the saved `session/list` knows its
    /// turns and is filtered like the row the reply will bring.
    pub turns_unknown: bool,
    /// The row's `workspace_root`: canonicalized when the path exists,
    /// verbatim otherwise. What project resolution reads — never the current
    /// project, so a worktree session keeps its own folder.
    pub workspace: Option<String>,
    /// The resolved project id: the stored one when it still names a
    /// project, else the adoption whose root equals [`Self::workspace`].
    /// `None` is unfiled.
    pub project: Option<String>,
    /// The resolved project's display name, for the context line's
    /// `project · branch` fallback. Resolved beside [`Self::project`];
    /// `None` renders the branch alone (or empty space with neither).
    pub project_name: Option<String>,
    /// What waits on the person (the listing's or a status broadcast's
    /// attention): a pending approval or question parked on a session other
    /// than the open one. Absence is a non-assertion — nothing pending, or
    /// a row whose pending source declined to answer.
    pub attention: Vec<Attention>,
    /// The pending approval's exact command, when the open session holds
    /// one: the context line's first priority, and what the status line's
    /// `Needs approval` stands on. The wire carries flags only, never text,
    /// so this is live from the open view's fold and `None` everywhere
    /// else — a closed session with the flag still reads `Needs approval`,
    /// over its byline.
    pub approval_command: Option<String>,
    /// The pending question's prompt, when the open session holds one: the
    /// context line when no approval is pending, and the `Asked` status
    /// line's quoted words. Live from the open view's fold, like
    /// [`Self::approval_command`].
    pub pending_question: Option<String>,
    /// When the running turn started, for the `Working` status line's
    /// elapsed (`Working · 14m`). Set on `turn/started`, overtaken by the
    /// next `session/list` rejoin; `None` falls back to [`Self::updated`].
    pub turn_started: Option<DateTime<Local>>,
    /// The last turn's terminal error message, when it failed: what the
    /// `Failed` status line stands on. Recorded from `turn/completed`'s
    /// `error` (terminal `"failed"`), cleared by the next turn's start or
    /// its success.
    pub last_error: Option<String>,
    /// A turn finished here while nobody was reading it: set when a turn
    /// ends anywhere but the open session scrolled to its end, cleared when
    /// that session is opened and read to its end (task 3.1). Persisted in
    /// the cache half of the store (`SessionMeta::unread`); losing it reads
    /// as "nothing unread", never as everything unread.
    pub unread: bool,
}

/// The row's time: content activity, never the server's resume-stamped clock.
/// muse bumps `session/list`'s `updated_at` on `session/resume` — the leased
/// touch that regenerates the view sidecar — which dragged a merely-opened
/// row to the top reading "now". `last_activity_at` (muse 1.3.0) moves only
/// on content and never precedes `created_at`, so the row sorts and reads by
/// it; with no content record yet it falls back to `created_at`, which a
/// resume cannot advance either. `updated_at` stays deliberately unread.
fn activity_time(session: &ListedRow) -> DateTime<Local> {
    session
        .last_activity_at
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(parse_time)
        .unwrap_or_else(|| parse_time(&session.created_at))
}

/// A Muse row's label, best first — the one ladder the list join, the boot
/// rows and the app's `Harness::rejoin` all read, so a row never reads
/// one thing before the list lands and another after it (and its search
/// row is not rewritten each time it flips — 63 rows a launch on the
/// owner's store, docs/51 §8). `wire` is the `session/list` row when one is
/// known; see [`SessionEntry::join`] for the order and why.
pub fn label_ladder<'a>(
    meta: Option<&'a SessionMeta>,
    wire: Option<&'a ListedRow>,
    index: Option<&'a IndexEntry>,
) -> Option<&'a str> {
    fn pick(value: Option<&str>) -> Option<&str> {
        value.map(str::trim).filter(|s| !s.is_empty())
    }
    // Muse's own `name` is not here: Muse 1.4 gives every session a random
    // two-word name ("flax-deneb") and the list never says which were typed
    // by a person, so a name there labelled every untitled row with noise
    // (owner, 2026-11-12). Baaz's own rename is `meta.name`, above.
    pick(meta.and_then(|m| m.name.as_deref()))
        .or_else(|| pick(meta.and_then(|m| m.handoff_title.as_deref())))
        .or_else(|| pick(meta.and_then(|m| m.generated_title.as_deref())))
        .or_else(|| pick(wire.and_then(|w| w.title.as_deref())))
        .or_else(|| pick(wire.and_then(|w| w.first_user_prompt.as_deref())))
        .or_else(|| index.and_then(IndexEntry::label))
        .or_else(|| pick(meta.and_then(|m| m.derived_title.as_deref())))
}

/// Whether Muse's index says another client made this session — the `muse`
/// CLI (a `muse exec` script, a relay lane, a terminal session) or another
/// app — and nothing in Baaz's store says the person has used it here.
///
/// Muse keeps one session list for every client on the Mac, so without this
/// Baaz drew all of them: 2,874 rows on the owner's Mac on 12 Nov 2026, of
/// which Baaz made 187 and a scripted exam run 1,450. Claude Code and Codex
/// only ever show what Baaz started or the person imported; this puts Muse on
/// the same footing. Such a row is treated like a side session: no row, no
/// search entry, no title call.
///
/// Unknown never hides: an older Muse or a cache from before the field
/// reads `created_by: None` and keeps today's behaviour.
///
/// A session in a temp folder is a scripted or disposable run whoever made
/// it — the rule Import applies to Claude Code and Codex (`import::under_temp`)
/// — except the running Baaz's own state dir, so a check run still lists
/// what it starts. A person's own session without a project runs in
/// `~/baaz-sessions`, never a temp folder.
pub fn made_elsewhere(index: Option<&IndexEntry>, workspace: Option<&str>, meta: Option<&SessionMeta>) -> bool {
    let other_client = index.and_then(|i| i.created_by.as_deref()).is_some_and(|creator| !is_baaz_client(creator));
    if !other_client && !workspace.is_some_and(is_scripted_root) {
        return false;
    }
    // A session the person has adopted in Baaz stays: named, pinned, put in
    // a project or handed off. A recorded byline is no evidence — until
    // 12 Nov 2026 merely opening a row wrote one, so the exam runs the owner
    // clicked to ask about carried one.
    let used_here = meta.is_some_and(|m| {
        let set = |v: &Option<String>| v.as_deref().is_some_and(|s| !s.trim().is_empty());
        m.pinned
            || set(&m.name)
            || set(&m.project)
            || set(&m.generated_title)
            || set(&m.handoff_from)
            || set(&m.handoff_to)
    });
    !used_here
}

/// Whether `root` is a temp folder (or under one) other than this run's own
/// state dir.
fn is_scripted_root(root: &str) -> bool {
    static ROOTS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    let roots = ROOTS.get_or_init(crate::import::temp_roots);
    if !crate::import::under_temp(root, roots) {
        return false;
    }
    let own = crate::projects::canonical_cached(&crate::store::support_dir().to_string_lossy());
    let here = crate::projects::canonical_cached(root);
    !(here == own || here.starts_with(&format!("{}/", own.trim_end_matches('/'))))
}

/// The `clientInfo.name`s Baaz has started Muse sessions under: `baaz`
/// ([`crate::conn::CLIENT_NAME`]), its check and test builds (`baaz_check`,
/// `baaz_t2`) and its first name, `harness`.
fn is_baaz_client(name: &str) -> bool {
    name.starts_with(crate::conn::CLIENT_NAME) || name == "harness"
}

impl SessionEntry {
    #[cfg(feature = "probe")]
    /// Join one `session/list` row with what the index and the store know.
    ///
    /// The title, best first (finding F10). Since muse 1.2.1 the row itself
    /// carries what only the index used to know, so the row's own copies come
    /// before the index's — and the index stays as the fallback for older
    /// rows whose list entries predate the derivation:
    ///
    /// 1. the name someone gave it with `/name` or the row's pencil;
    /// 2. the generated title one cheap model call wrote (auto-titles);
    /// 3. the row's own `title`;
    /// 4. the row's own `first_user_prompt`;
    /// 5. the index's generated `title`;
    /// 6. the index's `first_user_prompt`;
    /// 7. the transcript's first user prompt — else its first `userShell`
    ///    command when the session has no user text at all — cached in the
    ///    store by the application after a `session/read` or straight from
    ///    the open transcript;
    /// 8. [`UNNAMED`].
    ///
    /// Muse's served `name` and the index's `session_name` are never a label:
    /// Muse 1.4 names every session at random and says nowhere which names a
    /// person chose.
    ///
    /// The session id is never a title. "Session 01a081ef" tells a person
    /// nothing they can act on, and it reads like something went wrong.
    pub fn join(
        session: &ListedRow,
        index: Option<&IndexEntry>,
        meta: Option<&SessionMeta>,
        projects: &crate::projects::Projects,
    ) -> Self {
        Self::join_cached(session, index, meta, projects, &mut crate::projects::CanonicalCache::default())
    }

    /// [`Self::join`], reading every workspace root through `cache` so a
    /// refresh of many rows sharing few roots pays one `canonicalize` per
    /// distinct root. Row-for-row identical to [`Self::join`]: the cache is
    /// a pure memo within the pass.
    pub fn join_cached(
        session: &ListedRow,
        index: Option<&IndexEntry>,
        meta: Option<&SessionMeta>,
        projects: &crate::projects::Projects,
        cache: &mut crate::projects::CanonicalCache,
    ) -> Self {
        let name = meta.and_then(|m| m.name.as_deref()).map(str::trim).filter(|s| !s.is_empty());
        let label = label_ladder(meta, Some(session), index);
        // A user-given name always earns the first prompt below it; any other
        // label earns it only when it does not already say it (see
        // `describe`): Muse writes whole first prompts into the index title,
        // and Baaz's derived title is cut from the prompt the same
        // way, so comparing the fallthrough alone misses both.
        let user_named = name.is_some();
        let text = label.unwrap_or(UNNAMED);
        let workspace = session.workspace_root.as_deref().map(|root| cache.get(root));
        // A missing root resolves nowhere: the row falls back to "Other
        // workspaces" while the adoption stays in the store.
        let resolved = projects.resolve_available_cached(
            session.workspace_root.as_deref(),
            meta.and_then(|m| m.project.as_deref()),
            cache,
        );
        let project = resolved.as_ref().map(|p| p.id.clone());
        let project_name = resolved.as_ref().map(|p| p.name.clone());
        // A side session hides from the wire alone: the prompt prefix or
        // the side workspace marks it with no local record, so one started
        // by any other state dir never draws a row here.
        let side_marker = crate::titles::looks_like_side_session(
            session.first_user_prompt.as_deref(),
            session.workspace_root.as_deref(),
        ) || made_elsewhere(index, session.workspace_root.as_deref(), meta);
        Self {
            id: session.session_id.clone(),
            label: one_line(text),
            updated: activity_time(session),
            created: parse_time(&session.created_at),
            running: session.running(),
            // A destination's wire count includes its hidden pack
            // exchange; the row matches the transcript (§8.4).
            turns: visible_turns(
                session.turn_count,
                meta.and_then(|m| m.handoff_from.as_deref()).map(str::trim).is_some_and(|s| !s.is_empty()),
            ),
            hidden: meta.is_some_and(|m| m.hidden) || side_marker,
            pinned: meta.is_some_and(|m| m.pinned),
            archived: meta.is_some_and(|m| m.archived),
            description: with_handoff_origin(
                describe(meta, index, text, user_named),
                meta.and_then(|m| m.handoff_from_provider.as_deref()),
            ),
            last_ask: meta
                .and_then(|m| m.last_ask.as_deref())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            replayed: false,
            provider: ProviderId::Muse,
            named: name.is_some(),
            needs_title: label.is_none(),
            side_marker,
            title_pending: false,
            local: false,
            provisional: false,
            turns_unknown: false,
            workspace,
            project,
            project_name,
            attention: session.attention(),
            approval_command: None,
            pending_question: None,
            turn_started: None,
            // The last turn's terminal error, persisted across restarts: a
            // failure Baaz recorded (see `apply_turn_outcome`) still
            // reads `Failed` after one.
            last_error: meta
                .and_then(|m| m.last_error.as_deref())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            unread: meta.is_some_and(|m| m.unread),
            branch: session.branch.clone(),
            tasks_running: 0,
        }
    }

    /// One provisional row from the local index, before `session/list` lands.
    ///
    /// Only what the index knows: the label through [`IndexEntry::label`]'s
    /// own ladder (no second ladder invented here), the time from
    /// `updated_at_us`, and the workspace for project resolution. The store
    /// overrides that the real rows get (hidden / pinned / archived, the
    /// name, the byline halves) apply here too, so a hidden session never
    /// flashes into view. Everything the wire owns stays at its zero value —
    /// `turns: 0`, idle, no attention, no branch, no error — and
    /// the app's `SessionRow::summary` leaves the status line unset for these rows, so
    /// none of those zeroes ever renders.
    ///
    /// `None` (no row) when the index entry carries no label: with no name,
    /// no title and no first prompt the row would read [`UNNAMED`] with an
    /// unknown turn count, and the empty filter would hide it the moment the
    /// wire answered — the index's own zero-turn rows are exactly these, so
    /// leaving them out keeps the provisional list close to the real one.
    ///
    /// `needs_title` stays false on purpose: nothing may spend a
    /// `session/read` on a row the wire has not listed yet, and a read now
    /// would mark the id titled so the real row never earned one either.
    pub fn provisional(
        session_id: &str,
        index: &IndexEntry,
        meta: Option<&SessionMeta>,
        projects: &crate::projects::Projects,
        cache: &mut crate::projects::CanonicalCache,
    ) -> Option<Self> {
        let name = meta.and_then(|m| m.name.as_deref()).map(str::trim).filter(|s| !s.is_empty());
        let text = label_ladder(meta, None, Some(index))?;
        let user_named = name.is_some();
        let updated = index
            .updated_at_us
            .and_then(DateTime::from_timestamp_micros)
            .map(|t| t.with_timezone(&Local))
            .unwrap_or_else(|| Local.from_utc_datetime(&DateTime::<Utc>::UNIX_EPOCH.naive_utc()));
        let workspace = index.workspace_root.as_deref().map(|root| cache.get(root));
        let resolved = projects.resolve_available_cached(
            index.workspace_root.as_deref(),
            meta.and_then(|m| m.project.as_deref()),
            cache,
        );
        let project = resolved.as_ref().map(|p| p.id.clone());
        let project_name = resolved.as_ref().map(|p| p.name.clone());
        // Same wire-alone rule as the joined rows, from what the index
        // knows — so a side session never flashes into view before the
        // wire answers either.
        let side_marker = crate::titles::looks_like_side_session(
            index.first_user_prompt.as_deref(),
            index.workspace_root.as_deref(),
        ) || made_elsewhere(Some(index), index.workspace_root.as_deref(), meta);
        Some(Self {
            id: session_id.to_owned(),
            label: one_line(text),
            updated,
            created: updated,
            running: false,
            turns: 0,
            hidden: meta.is_some_and(|m| m.hidden) || side_marker,
            pinned: meta.is_some_and(|m| m.pinned),
            archived: meta.is_some_and(|m| m.archived),
            description: with_handoff_origin(
                describe(meta, Some(index), text, user_named),
                meta.and_then(|m| m.handoff_from_provider.as_deref()),
            ),
            last_ask: meta
                .and_then(|m| m.last_ask.as_deref())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            replayed: false,
            provider: ProviderId::Muse,
            named: name.is_some(),
            needs_title: false,
            side_marker,
            title_pending: false,
            local: false,
            provisional: true,
            turns_unknown: true,
            workspace,
            project,
            project_name,
            attention: Vec::new(),
            approval_command: None,
            pending_question: None,
            turn_started: None,
            last_error: meta
                .and_then(|m| m.last_error.as_deref())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            unread: meta.is_some_and(|m| m.unread),
            branch: None,
            tasks_running: 0,
        })
    }

    /// One provider-lane row from the local record, joined with Baaz's own
    /// overrides exactly like a wire row: the name, the generated title,
    /// the byline halves and the hidden/pinned/archived flags all read the
    /// same store a muse row reads — only identity, workspace and recency
    /// come from the provider record, because `session/list` never names
    /// these sessions.
    ///
    /// The label is the session row's own title ([`SessionMeta::title`]:
    /// the name someone gave it, the chain's title, the generated title,
    /// the provider's own ack title, the first prompt this app sent, the
    /// transcript-derived title), else [`UNNAMED`]. `needs_title` stays
    /// false on purpose: the derived title for these rows is the first
    /// prompt, already held, and no `session/read` may ever spend itself on
    /// an id the muse wire never listed.
    pub fn provider_row(
        record: &crate::provider_sessions::ProviderSessionRecord,
        meta: Option<&SessionMeta>,
        projects: &crate::projects::Projects,
    ) -> Option<Self> {
        // A record naming a provider this build does not register is not
        // listed — never shown as some other provider's session.
        let provider = ProviderId::parse_known(&record.provider)?;
        let name = meta.and_then(|m| m.name.as_deref()).map(str::trim).filter(|s| !s.is_empty());
        let label = meta.and_then(SessionMeta::title);
        let user_named = name.is_some();
        let text = label.unwrap_or(UNNAMED);
        let updated = DateTime::from_timestamp_millis(record.updated_ms)
            .map(|t| t.with_timezone(&Local))
            .unwrap_or_else(crate::clock::now_local);
        let workspace = record.workspace.as_deref().map(crate::projects::canonical_cached);
        let stored_project = meta.and_then(|m| m.project.as_deref());
        let resolved = projects.resolve_available(record.workspace.as_deref(), stored_project);
        let project = resolved.as_ref().map(|p| p.id.clone());
        let project_name = resolved.as_ref().map(|p| p.name.clone());
        Some(Self {
            id: record.session_id.clone(),
            label: one_line(text),
            updated,
            created: updated,
            running: false,
            // A destination's record counts its hidden pack
            // acknowledgement; the row matches the transcript (§8.4). A
            // lone destination row reads this too, wherever it is shown.
            turns: visible_turns(
                record.turns,
                meta.and_then(|m| m.handoff_from.as_deref()).map(str::trim).is_some_and(|s| !s.is_empty()),
            ),
            hidden: meta.is_some_and(|m| m.hidden),
            pinned: meta.is_some_and(|m| m.pinned),
            archived: meta.is_some_and(|m| m.archived),
            description: with_handoff_origin(
                describe(meta, None, text, user_named),
                meta.and_then(|m| m.handoff_from_provider.as_deref()),
            ),
            last_ask: meta
                .and_then(|m| m.last_ask.as_deref())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            replayed: false,
            provider,
            named: name.is_some(),
            needs_title: false,
            side_marker: false,
            title_pending: false,
            local: false,
            provisional: false,
            turns_unknown: false,
            workspace,
            project,
            project_name,
            attention: Vec::new(),
            approval_command: None,
            pending_question: None,
            turn_started: None,
            last_error: meta
                .and_then(|m| m.last_error.as_deref())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            unread: meta.is_some_and(|m| m.unread),
            branch: None,
            tasks_running: 0,
        })
    }

    #[cfg(feature = "probe")]
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
        let resolved = workspace
            .as_deref()
            .and_then(|root| projects.resolve_available(Some(root), None));
        let project = resolved.as_ref().map(|p| p.id.clone());
        let project_name = resolved.as_ref().map(|p| p.name.clone());
        // A capture is labelled by file, never by the wire — but its
        // workspace still speaks: the same wire-alone rule as live rows.
        let side_marker = crate::titles::looks_like_side_session(None, workspace.as_deref());
        Self {
            id: session_id.to_owned(),
            label,
            updated: crate::clock::now_local(),
            created: crate::clock::now_local(),
            running: false,
            turns: 0,
            hidden: side_marker,
            pinned: false,
            archived: false,
            description: String::new(),
            replayed: true,
            provider: ProviderId::Muse,
            named: false,
            needs_title: false,
            side_marker,
            title_pending: false,
            last_ask: None,
            local: false,
            provisional: false,
            turns_unknown: false,
            workspace,
            project,
            project_name,
            attention: Vec::new(),
            approval_command: None,
            pending_question: None,
            turn_started: None,
            last_error: None,
            unread: false,
            branch: None,
            tasks_running: 0,
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
    ///
    /// A row with a recorded ask has been sent to, and is never noise even
    /// at zero turns: a first turn that never settled (a dead permission
    /// channel, a crash) leaves `turns` at 0 once `running` reads false,
    /// and the row used to vanish under the person's feet (live,
    /// 2026-10-16). [`sent_to`] reads the same evidence off the stores.
    pub fn is_empty(&self) -> bool {
        self.turns == 0
            && !self.running
            && !self.named
            && self.last_ask.as_deref().map(str::trim).is_none_or(str::is_empty)
    }

    /// The session's one state (task 3.1): every mark, word and count that
    /// says what a session is doing reads this, so the dot and the text can
    /// no longer disagree. Total and ordered:
    ///
    /// 1. waiting on a person beats everything — an approval mid-turn is
    ///    `NeedsYou(Approval)`, never `Working` (the audit's "running dot +
    ///    needs approval text" row); an approval beats a question;
    /// 2. a running turn is `Working`;
    /// 3. the last turn's terminal error is `Failed`;
    /// 4. a turn that finished while nobody read it is `DoneUnread`;
    /// 5. anything else is `Idle`.
    pub fn session_state(&self) -> SessionState {
        if self.needs_approval() {
            SessionState::NeedsYou(NeedKind::Approval)
        } else if self.asked() {
            SessionState::NeedsYou(NeedKind::Question)
        } else if self.running {
            SessionState::Working
        } else if self.last_error.as_deref().map(str::trim).is_some_and(|s| !s.is_empty()) {
            SessionState::Failed
        } else if self.unread {
            SessionState::DoneUnread
        } else {
            SessionState::Idle
        }
    }

    /// The pending approval's command, trimmed, when the open session holds
    /// one.
    pub fn approval_text(&self) -> Option<&str> {
        self.approval_command.as_deref().map(str::trim).filter(|s| !s.is_empty())
    }

    /// The pending question's prompt, trimmed, when the open session holds
    /// one.
    pub fn question_text(&self) -> Option<&str> {
        self.pending_question.as_deref().map(str::trim).filter(|s| !s.is_empty())
    }

    /// Fold a `session/statusChanged` broadcast into the row: the load state
    /// after the transition, and the attention flags after it. The flags
    /// ride present-only-when-nonempty, so an absent list clears — the
    /// broadcast is the whole truth, not a delta.
    pub fn apply_status_changed(&mut self, running: bool, attention: &[Attention]) {
        self.running = running;
        self.attention = attention.to_vec();
    }

    /// Fold a `turn/completed` terminal into the row: a failed turn records
    /// its error message for the `Failed` state; any other terminal stands
    /// a recorded error down (a retry's start clears it sooner, in
    /// `sync_row_live`). Blank messages record nothing.
    pub fn apply_turn_outcome(&mut self, failed: bool, error: Option<&str>) {
        if failed {
            if let Some(message) = error.map(str::trim).filter(|s| !s.is_empty()) {
                self.last_error = Some(message.to_owned());
            }
        } else if self.last_error.is_some() {
            self.last_error = None;
        }
    }

    /// Whether the row waits on a person for an approval: a live command on
    /// the open session, or the wire's `approvalPending` flag on any other.
    /// Unknown flag values never count (the schema declares the domain
    /// open; a future kind is not an approval).
    pub fn needs_approval(&self) -> bool {
        self.approval_text().is_some() || self.attention.contains(&Attention::ApprovalPending)
    }

    /// Whether the row asked a question: a live prompt on the open session,
    /// or the wire's `inputPending` flag on any other. Like
    /// [`Self::needs_approval`], unknown flags never count.
    pub fn asked(&self) -> bool {
        self.question_text().is_some() || self.attention.contains(&Attention::InputPending)
    }
}

#[cfg(feature = "probe")]
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
        created: updated,
        running: true,
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
        local: true,
        provisional: false,
        turns_unknown: false,
        workspace,
        project,
        project_name: None,
        attention: Vec::new(),
        approval_command: None,
        pending_question: None,
        turn_started: Some(updated),
        last_error: None,
        unread: false,
        branch: None,
        tasks_running: 0,
    }
}

/// Whether `session_id` has ever been sent to, by the stores' own
/// record: a byline ask in the overrides, or the first prompt a provider
/// lane noted on admission. Either means a person typed into it, so the
/// empty filter keeps its row even while `turns` still reads 0 — the
/// admission writes the first prompt before any turn settles.
pub fn sent_to(session_id: &str, overrides: &crate::sessions::Overrides) -> bool {
    let said = |value: Option<&str>| value.map(str::trim).is_some_and(|s| !s.is_empty());
    said(overrides.get(session_id).and_then(|m| m.last_ask.as_deref()))
        || said(overrides.get(session_id).and_then(|m| m.first_prompt.as_deref()))
}

/// The prompt a send may title `session_id`'s row from: none once a
/// generated title stands. [`first_send_update`] titles any zero-turn row
/// from its send, and a first turn that never settled keeps the row at
/// zero turns — so a retry (which resends the first prompt) put the raw
/// prompt back over the generated title, in the row and in the header that
/// reads it (live, 2026-10-16). The store's ladder ranks the generated
/// title above the prompt; a send must not reorder it.
pub fn send_title<'a>(session_id: &str, prompt: Option<&'a str>, overrides: &crate::sessions::Overrides) -> Option<&'a str> {
    let generated = overrides
        .get(session_id)
        .and_then(|m| m.generated_title.as_deref())
        .map(str::trim)
        .is_some_and(|s| !s.is_empty());
    prompt.filter(|_| !generated)
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
pub fn first_send_update(
    entry: &mut SessionEntry,
    prompt: Option<&str>,
    now: DateTime<Local>,
    handoff_dest: bool,
) -> bool {
    if entry.turns > 0 {
        return false;
    }
    entry.running = true;
    // A handoff destination is never titled from a send: the pack submit
    // reveals the row, and the destination's first real message keeps the
    // chain title (`docs/22-handoff.md` §8, Y2a). Any other session is
    // titled from its own words — even when they start with "Handed off
    // from ": only a handoff destination ever has a pack turn (Y2a3).
    if !handoff_dest && !entry.named {
        if let Some(prompt) = prompt.map(str::trim).filter(|p| !p.is_empty()) {
            entry.label = one_line(prompt);
        }
    }
    entry.updated = now;
    entry.turn_started = Some(now);
    entry.last_error = None;
    true
}

/// Whether `text` is a handoff pack turn or its short display summary: the
/// full pack starts with the pack header (`PACK_HEAD_PREFIX`), the bubble
/// with "Handed off from …" (`handoff::display_text`). A text match alone,
/// never a verdict: only a handoff destination ever has a pack turn, so
/// callers gate this on [`is_handoff_dest`] via [`is_pack_turn`] (Y2a3) —
/// a normal message starting with that phrase keeps its title and byline.
pub fn is_pack_text(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with(crate::handoff_snapshot::PACK_HEAD_PREFIX) || t.starts_with("Handed off from ")
}

/// The byline halves that land: the pack turn and its acknowledgement are
/// the handoff speaking, so when the newest ask is the destination's pack,
/// neither half lands (`record_last_summary`, Y2a). Any other session's
/// words land untouched, even pack-shaped ones (Y2a3).
pub fn byline_landable(
    summary: Option<String>,
    ask: Option<String>,
    newest_ask_is_pack: bool,
) -> (Option<String>, Option<String>) {
    match ask {
        Some(_) if newest_ask_is_pack => (None, None),
        _ => (summary, ask),
    }
}

/// Whether `id` was born from a handoff.
pub fn is_handoff_dest(id: &str, overrides: &crate::sessions::Overrides) -> bool {
    handoff_from_of(id, overrides).is_some()
}

/// The persisted chain title for `id`.
pub fn handoff_title_of(id: &str, overrides: &crate::sessions::Overrides) -> Option<String> {
    overrides
        .get(id)
        .and_then(|m| m.handoff_title.clone())
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty())
}

pub fn merge_session_list(wire: Vec<SessionEntry>, existing: &[SessionEntry]) -> Vec<SessionEntry> {
    let mut merged = wire;
    let listed: std::collections::HashSet<String> = merged.iter().map(|entry| entry.id.clone()).collect();
    // Local rows survive until the wire lists them — and provider-lane rows
    // survive every merge, because `session/list` never lists them: their
    // only record is the local one.
    merged.extend(
        existing
            .iter()
            .filter(|entry| (entry.local || !entry.provider.has(provider::ProviderTrait::NativeSessionList)) && !listed.contains(&entry.id))
            .cloned(),
    );
    merged
}

/// Whether a `session/list` reply changes nothing on screen: every listed
/// row (locals aside — they are the window's own and survive any merge)
/// already matches, in order. Provisional rows never match: they carry the
/// provisional flag and no wire facts, so the first reply after the index
/// always applies wholesale. The caller ANDs the list-having-landed bit —
/// the first reply applies even against an empty list.
pub fn list_apply_unchanged(existing: &[SessionEntry], wire: &[SessionEntry]) -> bool {
    // Provider-lane rows are the window's own, like locals: the wire never
    // lists them, so they never count toward (or against) a match.
    let listed: Vec<&SessionEntry> =
        existing.iter().filter(|entry| !entry.local && entry.provider.has(provider::ProviderTrait::NativeSessionList)).collect();
    listed.len() == wire.len() && listed.iter().zip(wire.iter()).all(|(a, b)| a == &b)
}

// A handoff chain is one sidebar row (`docs/22-handoff.md` §8).
//
// Sessions linked by `handoff_to` / `handoff_from` — each half on its own
// session's facts, whatever the provider — form a chain whose head (no
// `handoff_to`) is the only listed row. The helpers
// below are pure: links in, rows out. The application owns fetching and
// opening, like everything else in this module.
//
// A link counts only when its destination is a known session (a record or
// facts) or a row: a dangling `handoff_to` (destination missing) keeps the source
// listed, exactly like an unlinked session.

/// The `handoff_to` half for one id.
fn handoff_to_of(id: &str, overrides: &crate::sessions::Overrides) -> Option<String> {
    overrides
        .get(id)
        .and_then(|meta| meta.handoff_to.clone())
        .map(|dest| dest.trim().to_owned())
        .filter(|dest| !dest.is_empty() && dest != id)
}

/// The `handoff_from` half for one id.
fn handoff_from_of(id: &str, overrides: &crate::sessions::Overrides) -> Option<String> {
    overrides
        .get(id)
        .and_then(|meta| meta.handoff_from.clone())
        .map(|source| source.trim().to_owned())
        .filter(|source| !source.is_empty() && source != id)
}

/// One build of the handoff chains over full storage: every known id
/// (both stores plus the rows) resolved to its head, and every head to its
/// sorted members. Built once per storage change and shared by the head,
/// member and view lookups — never rebuilt per frame or per row (Y2a3).
///
/// A link counts only when its destination is known (a record in either
/// store, or a built row): a dangling `handoff_to` keeps the source
/// listed, exactly like an unlinked session. Cycles stop the walk rather
/// than hanging the list build on a corrupt store.
#[derive(Clone, Debug, Default)]
pub struct ChainIndex {
    /// Every known id to its chain head. An id nothing knows is absent:
    /// callers read it as its own singleton head.
    heads: HashMap<String, String>,
    /// Every head to its sorted members. Heads with no other member are
    /// absent: callers read them as their own singleton chain.
    members: HashMap<String, Vec<String>>,
}

impl ChainIndex {
    /// Resolve every chain in one pass over the links.
    pub fn build(
        provider_sessions: &crate::provider_sessions::ProviderSessionStore,
        overrides: &crate::sessions::Overrides,
        entries: &[SessionEntry],
    ) -> Self {
        let mut known: HashSet<String> = provider_sessions.keys().cloned().collect();
        known.extend(overrides.keys().cloned());
        known.extend(entries.iter().map(|entry| entry.id.clone()));
        // The live `handoff_to` step for one id: the destination when the
        // link names another known session, else the walk stops here.
        let step = |id: &str| -> Option<String> {
            let next = handoff_to_of(id, overrides)?;
            (next != id && known.contains(&next)).then_some(next)
        };
        let mut heads: HashMap<String, String> = HashMap::with_capacity(known.len());
        // One sorted pass, so every shared prefix walk compresses the same
        // way whatever order the rows arrived in.
        let mut ids: Vec<&String> = known.iter().collect();
        ids.sort();
        for id in ids {
            if heads.contains_key(id.as_str()) {
                continue;
            }
            // The walk from this id, with its own cycle guard.
            let mut trail: Vec<String> = Vec::new();
            let mut seen: HashSet<String> = HashSet::new();
            let mut cursor = id.clone();
            loop {
                if let Some(settled) = heads.get(cursor.as_str()) {
                    let head = settled.clone();
                    for walked in trail {
                        heads.insert(walked, head.clone());
                    }
                    break;
                }
                if !seen.insert(cursor.clone()) {
                    // A cycle: every id on it is its own stop, so the rows
                    // list rather than collapse or loop.
                    for walked in &trail {
                        heads.entry(walked.clone()).or_insert_with(|| walked.clone());
                    }
                    heads.entry(cursor.clone()).or_insert_with(|| cursor.clone());
                    break;
                }
                trail.push(cursor.clone());
                match step(&cursor) {
                    Some(next) => cursor = next,
                    None => {
                        for walked in trail {
                            heads.insert(walked, cursor.clone());
                        }
                        break;
                    }
                }
            }
        }
        let mut members: HashMap<String, Vec<String>> = HashMap::new();
        for (id, head) in &heads {
            members.entry(head.clone()).or_default().push(id.clone());
        }
        for chain in members.values_mut() {
            chain.sort();
        }
        // Singletons carry no information: drop them so the view's
        // collapsible check reads the map directly.
        members.retain(|head, chain| !(chain.len() == 1 && head == &chain[0]));
        Self { heads, members }
    }

    /// Whether `id` names a session the list could show: a record in
    /// either store, or a row already built.
    pub fn contains(&self, id: &str) -> bool {
        self.heads.contains_key(id)
    }

    /// Follow `handoff_to` from `id` to the chain head. An unknown id
    /// resolves to itself, so callers act on the id they were given.
    pub fn head(&self, id: &str) -> String {
        self.heads.get(id).cloned().unwrap_or_else(|| id.to_owned())
    }

    /// Every member of `id`'s chain, sorted, so archive and rename batches
    /// over it are deterministic. An unlinked id yields itself alone.
    pub fn members(&self, id: &str) -> Vec<String> {
        let head = self.head(id);
        self.members.get(&head).cloned().unwrap_or_else(|| vec![head])
    }
}

/// Follow `handoff_to` from `id` to the chain head: the first id with no live
/// link onward. A dangling destination (nothing known under it) stops the
/// walk, so its source stays listed; a cycle stops too, rather than hanging
/// the list build on a corrupt store.
#[cfg(any(test, feature = "test-support"))]
pub fn chain_head(
    id: &str,
    provider_sessions: &crate::provider_sessions::ProviderSessionStore,
    overrides: &crate::sessions::Overrides,
    entries: &[SessionEntry],
) -> String {
    ChainIndex::build(provider_sessions, overrides, entries).head(id)
}

/// Every member of `id`'s chain, tail first: all known ids (both stores plus
/// the rows) that resolve to the same head. Sorted, so archive and rename
/// batches over it are deterministic. An unlinked id yields itself alone.
#[cfg(any(test, feature = "test-support"))]
pub fn chain_members(
    id: &str,
    provider_sessions: &crate::provider_sessions::ProviderSessionStore,
    overrides: &crate::sessions::Overrides,
    entries: &[SessionEntry],
) -> Vec<String> {
    ChainIndex::build(provider_sessions, overrides, entries).members(id)
}

/// Collapse the rows to one per chain, preserving order: a session whose link
/// points at a live destination is not listed, and its head is emitted where
/// the chain's first member stands — so on activation the destination row
/// takes the source row's place rather than jumping elsewhere. A headless
/// chain (the head has no row yet) is left alone; the next list build heals
/// it once the head's row arrives.
///
/// The head row keeps its own `updated`, provider badge and flags; its label
/// is the chain's original title (the tail member's row) unless the head was
/// user-renamed, and its turn count is the chain's visible total — each
/// member row already hides its pack exchange ([`visible_turns`]), so the
/// plain sum matches the transcript.
/// Walk `handoff_from` back from the head to the tail member: the chain's
/// first session, whose title the collapsed row keeps. Stops at an unknown
/// source or a cycle; `None` when the head itself names no source.
fn chain_tail(head: &str, overrides: &crate::sessions::Overrides, index: &ChainIndex) -> Option<String> {
    let mut tail = head.to_owned();
    let mut seen = HashSet::from([tail.clone()]);
    let mut found = false;
    while let Some(source) = handoff_from_of(&tail, overrides) {
        if !index.contains(&source) || !seen.insert(source.clone()) {
            break;
        }
        tail = source;
        found = true;
    }
    found.then_some(tail)
}

/// The turn count the sidebar shows for a row: the transcript hides the
/// handoff pack exchange (the pack's user turn plus its one-sentence
/// acknowledgement), while the stores count the acknowledgement as a
/// settled turn. A member born from a handoff therefore reads one turn
/// high until this subtracts it (`docs/22-handoff.md` §8.4).
pub fn visible_turns(turns: u64, has_handoff_from: bool) -> u64 {
    if has_handoff_from { turns.saturating_sub(1) } else { turns }
}

/// Whether `id` carries a handoff link in either direction. Blank links read
/// as none, like [`handoff_to_of`] treats them.
fn has_handoff_link(id: &str, overrides: &crate::sessions::Overrides) -> bool {
    handoff_to_of(id, overrides).is_some()
        || handoff_from_of(id, overrides).is_some()
}

/// Whether settling `id` can change the chain collapse: the touched session
/// carries a handoff link, or is the endpoint of another session's link.
/// What keeps an unlinked settle on the O(1) row refresh instead of the
/// full clone + scan (`docs/22-handoff.md` §8.4).
///
pub fn needs_collapse(id: &str, overrides: &crate::sessions::Overrides) -> bool {
    if has_handoff_link(id, overrides) {
        return true;
    }
    // The touched id may carry no link itself and still anchor a chain:
    // another session names it as its handoff source or destination.
    fn names(id: &str, link: Option<&str>) -> bool {
        link.map(str::trim).is_some_and(|target| target == id)
    }
    overrides
        .values()
        .any(|meta| names(id, meta.handoff_to.as_deref()) || names(id, meta.handoff_from.as_deref()))
}

#[cfg(any(test, feature = "test-support"))]
pub fn collapse_handoff_chains(
    entries: Vec<SessionEntry>,
    provider_sessions: &crate::provider_sessions::ProviderSessionStore,
    overrides: &crate::sessions::Overrides,
) -> Vec<SessionEntry> {
    // One index build for the whole view: every row reads its head from
    // the same map, never a walk per row (Y2a3).
    let index = ChainIndex::build(provider_sessions, overrides, &entries);
    collapse_with_index(entries, &index, overrides)
}

/// Collapse the rows against a caller-held [`ChainIndex`]: the same
/// one-row-per-chain view, without rebuilding the map. The frame's cached
/// index serves the collapse and the selection heads together, so the view
/// costs one build per storage change (Y2a3).
pub fn collapse_with_index(
    entries: Vec<SessionEntry>,
    index: &ChainIndex,
    overrides: &crate::sessions::Overrides,
) -> Vec<SessionEntry> {
    let heads: Vec<String> = entries.iter().map(|entry| index.head(&entry.id)).collect();
    let mut by_head: HashMap<String, Vec<usize>> = HashMap::new();
    for (ix, head) in heads.iter().enumerate() {
        by_head.entry(head.clone()).or_default().push(ix);
    }
    // Collapsible: more than one row resolving to a head that itself has a
    // row. Singletons already read as themselves; a headless chain waits for
    // the head's row to arrive.
    let collapsible: HashSet<String> = by_head
        .iter()
        .filter(|(head, ixs)| ixs.len() > 1 && ixs.iter().any(|ix| entries[*ix].id == **head))
        .map(|(head, _)| head.clone())
        .collect();
    if collapsible.is_empty() {
        return entries;
    }
    // The collapsed head rows, built before the rows move (Y2a: one
    // identity per chain). The head's own row keeps its provider badge;
    // the display derives from the whole chain: the title ladder (a user
    // rename of the head, else the persisted `handoff_title`, else the
    // tail member's row label, else the head's own label), the summed
    // visible turns, the newest time, pinned/archived ORed across members
    // (copied at activation, so usually already on the head), and
    // needs-you attention ORed across members. Pure and idempotent: a
    // second run over collapsed rows changes nothing, whatever order the
    // rows arrived in.
    let mut collapsed: HashMap<String, SessionEntry> = HashMap::new();
    for head in &collapsible {
        let mut row = entries.iter().find(|entry| entry.id == *head).expect("collapsible heads have a row").clone();
        if !row.named {
            if let Some(kept) = handoff_title_of(head, overrides) {
                row.label = one_line(&kept);
            } else {
                let original = chain_tail(head, overrides, index)
                    .filter(|tail| tail != head)
                    .and_then(|tail| entries.iter().find(|entry| entry.id == tail).map(|tail_row| tail_row.label.clone()));
                if let Some(original) = original {
                    row.label = original;
                }
            }
        }
        row.turns = by_head[head].iter().map(|ix| entries[*ix].turns).sum();
        let mut newest = row.updated;
        let mut running = row.running;
        let mut turn_started = row.turn_started;
        let mut pinned = row.pinned;
        let mut archived = row.archived;
        let mut attention = row.attention.clone();
        let mut approval = row.approval_command.clone();
        let mut question = row.pending_question.clone();
        for ix in &by_head[head] {
            let member = &entries[*ix];
            if member.updated > newest {
                newest = member.updated;
            }
            // A live turn on any member shows on the one row: the head
            // would otherwise read idle while its chain works (Y2a3).
            running |= member.running;
            if turn_started.is_none_or(|started| member.turn_started.is_some_and(|cand| cand > started)) {
                turn_started = member.turn_started;
            }
            pinned |= member.pinned;
            archived |= member.archived;
            for flag in &member.attention {
                if !attention.contains(flag) {
                    attention.push(flag.clone());
                }
            }
            if approval.is_none() {
                approval = member.approval_command.clone();
            }
            if question.is_none() {
                question = member.pending_question.clone();
            }
        }
        row.updated = newest;
        row.running = running;
        row.turn_started = turn_started;
        row.pinned = pinned;
        row.archived = archived;
        row.attention = attention;
        row.approval_command = approval;
        row.pending_question = question;
        collapsed.insert(head.clone(), row);
    }
    let mut emitted: HashSet<String> = HashSet::new();
    let mut out = Vec::with_capacity(entries.len());
    for (ix, entry) in entries.into_iter().enumerate() {
        let head = heads[ix].clone();
        if !collapsible.contains(&head) {
            out.push(entry);
            continue;
        }
        if !emitted.insert(head.clone()) {
            continue;
        }
        out.push(collapsed.remove(&head).expect("collapsible heads are prebuilt"));
    }
    out
}

/// The single "now" a grouping is built against.
///
/// Under `BAAZ_DETERMINISTIC=1` it is the newest `updated` in the data, so
/// the newest row reads "now" however old the fixture is and two runs group
/// and label identically.
///
/// One clock per grouping, never one per row (findings `performance-6`,
/// `support-3`), and the window keys its grouping cache on the minute of it
/// (finding `support-2`), which is the finest thing an elapsed tag says.
pub fn grouping_now(entries: &[SessionEntry]) -> DateTime<Local> {
    if crate::runtime::clock().is_frozen() {
        entries.iter().map(|e| e.updated).max().unwrap_or_else(crate::clock::now_local)
    } else {
        Local::now()
    }
}

/// The unfiled group id: sessions whose workspace no adoption holds —
/// the default workspace's own, and any root Muse ran in elsewhere.
/// Always last and muted; open unless the person closed it.
///
/// The id stays `"other"` so a stored fold state keeps working.
pub const OTHER_GROUP: &str = "other";

/// What that group is called. Everything not filed under a project is in it,
/// which is what the word has to carry.
pub const UNFILED_LABEL: &str = "Unfiled";

/// How many of a project's newest unpinned sessions a folded group shows.
///
/// Pinned rows always show and never count toward this; the open session is
/// appended past it when it would otherwise be cut.
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
    /// The open session's project, when it has no row in `entries` (a
    /// brand-new session before its first send): resolved by the caller
    /// through [`rowless_session_project`], so the session's project still
    /// counts as current. `None` when the session has a row (its project
    /// reads from the row) or when no session is open.
    pub active_project: Option<&'a str>,
    /// The click's target: `resume` names it before any view exists (and on
    /// a client-less run no view ever comes), so without the rescue below
    /// the highlight it earned would sit behind "Show N more" unseen.
    pub pending: Option<&'a str>,
}

/// Orders a group's rows: pinned first, then by `order` — latest activity,
/// newest created, or title (case-insensitive, ties by activity).
pub fn sort_rows<E: std::borrow::Borrow<SessionEntry>>(rows: &mut [E], order: crate::layout::SortBy) {
    use crate::layout::SortBy;
    match order {
        SortBy::Recent => rows.sort_by_key(|e| (!e.borrow().pinned, std::cmp::Reverse(e.borrow().updated))),
        SortBy::Created => rows.sort_by_key(|e| (!e.borrow().pinned, std::cmp::Reverse(e.borrow().created))),
        SortBy::Name => rows.sort_by_cached_key(|e| (!e.borrow().pinned, e.borrow().label.to_lowercase(), std::cmp::Reverse(e.borrow().updated))),
    }
}

/// The last path component of a workspace root, for the unfiled
/// rows' repo tag — and the search palette's badge for sessions no project
/// holds. A root with no final component names itself whole rather than
/// tagging nothing.
pub fn workspace_folder(workspace: &str) -> String {
    std::path::Path::new(workspace)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| workspace.to_owned())
}

/// Which date header a moment belongs under, on the calendar and not on a
/// rolling 24 hours: something from 00:30 this morning is "Today" at 23:00.
pub fn bucket(at: DateTime<Local>, now: DateTime<Local>) -> &'static str {
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
pub fn elapsed_at(at: DateTime<Local>, now: DateTime<Local>) -> String {
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
/// A handoff destination row's byline origin: "from <Provider>", joined
/// onto whatever the row already says — or the whole byline when it says
/// nothing. `provider` is the source session's wire id; an id no
/// registered provider answers to names no origin.
pub fn with_handoff_origin(base: String, provider: Option<&str>) -> String {
    let Some(wire) = provider.map(str::trim).filter(|s| !s.is_empty()) else { return base };
    let Ok(from) = crate::providers::ProviderId::resolve(wire) else { return base };
    let from = format!("from {}", from.label());
    if base.trim().is_empty() { from } else { format!("{base} · {from}") }
}

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
    // Only the first 40 characters are compared, so a whole-word prefix
    // comfortably past that is all either side needs (see
    // [`flattened_prefix`]); lowercasing a prefix cut at a word boundary
    // reads the same as lowercasing the whole. The label's trailing
    // elision trim reaches the first 40 only when everything past the
    // prefix is trimmable, so a cut label takes the full path exactly then.
    fn norm(text: &str) -> String {
        text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
    }
    fn first_40(text: &str) -> String {
        text.chars().take(40).collect()
    }
    let label: String = match flattened_prefix(label, 64) {
        (prefix, Some(end))
            if !label[end..].chars().all(|c| c.is_whitespace() || matches!(c, '\u{2026}' | '.')) =>
        {
            first_40(&prefix.to_lowercase())
        }
        _ => first_40(norm(label).trim_end_matches(['\u{2026}', '.', ' '])),
    };
    if label.is_empty() {
        return false;
    }
    let prompt = first_40(&flattened_prefix(prompt, 64).0.to_lowercase());
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
pub fn one_line(text: &str) -> String {
    let (flattened, cut) = flattened_prefix(text, 80);
    if cut.is_none() {
        return flattened;
    }
    flattened.chars().take(79).collect::<String>() + "\u{2026}"
}

/// `text` with its whitespace runs collapsed to single spaces, built a word
/// at a time and stopped once it holds more than `chars` characters; the
/// byte offset where the unread remainder starts comes back when it
/// stopped early. The prefix is exactly the start of the fully flattened
/// text, so callers that only read the first few dozen characters never
/// flatten a whole transcript-sized prompt: every list join and rejoin
/// runs this per row, and flattening ~2,600 whole prompts was most of a
/// 600–800 ms frozen window per `session/list` reply.
fn flattened_prefix(text: &str, chars: usize) -> (String, Option<usize>) {
    let mut out = String::new();
    let mut count = 0usize;
    for word in text.split_whitespace() {
        if count > 0 {
            out.push(' ');
            count += 1;
        }
        out.push_str(word);
        count += word.chars().count();
        if count > chars {
            let end = word.as_ptr() as usize - text.as_ptr() as usize + word.len();
            return (out, Some(end));
        }
    }
    (out, None)
}

/// The hover card's title budget: the card clamps to three laid-out lines
/// itself (agentic-ui v0.3.15), and this char budget keeps one long line
/// from ever reaching it unclamped. Row labels already pass through
/// [`one_line`], so this only ever bites on labels built another way.
pub fn hover_title(label: &str) -> String {
    const BUDGET: usize = 160;
    let flattened: String = label.split_whitespace().collect::<Vec<_>>().join(" ");
    if flattened.chars().count() <= BUDGET {
        return flattened;
    }
    flattened.chars().take(BUDGET - 1).collect::<String>() + "\u{2026}"
}

/// The project a rowless session belongs to: its built row first, then
/// Baaz's own override (drafts file their project before their first
/// send), then the provider-lane record, else nothing. What marks the
/// rowless active session's project current and what the scroll-follow
/// falls back to when the session has no row yet.
pub fn rowless_session_project(
    session_id: &str,
    entries: &[SessionEntry],
    overrides: &crate::sessions::Overrides,
) -> Option<String> {
    entries
        .iter()
        .find(|e| e.id == session_id)
        .and_then(|e| e.project.clone())
        .or_else(|| overrides.get(session_id).and_then(|m| m.project.clone()))
        .map(|p| p.trim().to_owned())
        .filter(|p| !p.is_empty())
}


#[cfg(test)]
pub mod tests {
    use super::*;

    /// The prefix-only flattening reads exactly what flattening the whole
    /// text read: `one_line` and the echo check agree with their old
    /// whole-text forms on short, long, cut-at-the-edge and elided input.
    #[test]
    fn prefix_flattening_matches_the_whole_text() {
        fn one_line_whole(text: &str) -> String {
            let flattened: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
            if flattened.chars().count() <= 80 {
                return flattened;
            }
            flattened.chars().take(79).collect::<String>() + "\u{2026}"
        }
        fn echoes_whole(label: &str, prompt: &str) -> bool {
            fn norm(text: &str) -> String {
                text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
            }
            let label: String =
                norm(label).trim_end_matches(['\u{2026}', '.', ' ']).chars().take(40).collect();
            if label.is_empty() {
                return false;
            }
            let prompt: String = norm(prompt).chars().take(40).collect();
            prompt.starts_with(&label)
        }
        let long = "Fix   the header\n\nso it aligns ".repeat(400);
        let texts = [
            String::new(),
            "   ".to_owned(),
            "Short prompt".to_owned(),
            "x".repeat(80),
            "x".repeat(81),
            format!("{} tail", "word ".repeat(16)),
            long.clone(),
            format!("{}{}", "Fix the header so it aligns with the rail ", ". ".repeat(200)),
            format!("{}…", &long[..120]),
            "ΣΊΣΥΦΟΣ ΚΑΙ ΟΔΥΣΣΕΑΣ ".repeat(10),
        ];
        for text in &texts {
            assert_eq!(one_line(text), one_line_whole(text), "one_line on {text:?}");
            for prompt in &texts {
                assert_eq!(
                    echoes_prompt(text, prompt),
                    echoes_whole(text, prompt),
                    "echoes_prompt on {text:?} / {prompt:?}"
                );
            }
        }
    }

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

    /// A 2,000-char single-line first message joins to a one-line row
    /// title of at most 80 chars.
    #[test]
    fn a_two_thousand_char_first_message_yields_a_short_row_title() {
        let projects = crate::projects::Projects::default();
        let mut session = wire_session();
        session.first_user_prompt = Some("w".repeat(2000));
        session.turn_count = 1;
        let row = SessionEntry::join(&session, None, None, &projects);
        assert!(row.label.chars().count() <= 80, "row title is {} chars", row.label.chars().count());
        assert!(row.label.ends_with("\u{2026}"), "a cut title carries its elision");
    }

    /// [`rowless_session_project`]: the row first, then the facts a draft
    /// files before its first send.
    #[test]
    fn a_rowless_session_names_its_project_past_the_rows() {
        let entries = vec![grouped_entry("s1", Some("p-baaz"), Some("/work/p-baaz"), 0)];
        let overrides: crate::sessions::Overrides = HashMap::new();
        assert_eq!(
            rowless_session_project("s1", &entries, &overrides).as_deref(),
            Some("p-baaz"),
            "a row names its own project"
        );
        assert_eq!(rowless_session_project("s-new", &entries, &overrides), None);
        let mut overrides = overrides;
        overrides.insert(
            "s-new".to_owned(),
            crate::sessions::SessionMeta { project: Some("p-agentic".to_owned()), ..Default::default() },
        );
        assert_eq!(
            rowless_session_project("s-new", &entries, &overrides).as_deref(),
            Some("p-agentic"),
            "a draft's override files its project before the first send"
        );
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

    fn old_index() -> IndexEntry {
        IndexEntry {
            session_name: Some("Old Name".into()),
            title: "Old Title".into(),
            first_user_prompt: Some("Old prompt".into()),
            search_text: String::new(),
            updated_at_us: None,
            workspace_root: None,
            created_by: None,
        }
    }

    /// The row's own `name` / `title` / `firstUserPrompt`
    /// come before the index's copies, and the index stays the fallback for
    /// older rows that predate the derivation.
    #[test]
    fn the_rows_own_words_come_before_the_index_copies() {
        let projects = crate::projects::Projects::default();
        let index = old_index();
        // An old row: nothing of its own, the index names it — by its
        // title, never Muse's random `session_name`.
        let old = SessionEntry::join(&wire_session(), Some(&index), None, &projects);
        assert_eq!(old.label, "Old Title");
        assert!(!old.needs_title, "the index named it");
        // Muse's served name is never a label: Muse 1.4 names every
        // session at random ("flax-deneb") and never says which a person
        // typed (owner, 2026-11-12). With nothing else the row is unnamed
        // and asks the transcript.
        let mut named = wire_session();
        named.name = Some("flax-deneb".into());
        let entry = SessionEntry::join(&named, None, None, &projects);
        assert_eq!(entry.label, UNNAMED);
        assert!(entry.needs_title);
        // …its own title wins over the index's…
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

    /// Newest-first ids, the order `visible_sessions` and both groupings draw.
    fn order_newest_first(rows: &[SessionEntry]) -> Vec<String> {
        let mut rows: Vec<&SessionEntry> = rows.iter().collect();
        rows.sort_by_key(|e| std::cmp::Reverse(e.updated));
        rows.into_iter().map(|e| e.id.clone()).collect()
    }

    /// B8: a Muse row's time is content activity, never the server clock.
    /// muse stamps `updated_at` on `session/resume` (the leased touch that
    /// regenerates the view sidecar); `last_activity_at` moves only on
    /// content. The row sorts and reads by the activity — and by creation
    /// while no content record exists, which a resume cannot advance either.
    #[test]
    fn muse_rows_sort_and_read_by_content_activity() {
        let projects = crate::projects::Projects::default();
        // Activity older than the server stamp: the row reads the activity.
        let mut resumed = wire_session();
        resumed.session_id = "resumed".into();
        resumed.created_at = "2026-09-10T10:00:00Z".into();
        resumed.last_activity_at = Some("2026-09-11T10:00:00Z".into());
        resumed.updated_at = "2026-09-13T10:00:00Z".into();
        resumed.turn_count = 3;
        let row = SessionEntry::join(&resumed, None, None, &projects);
        assert_eq!(row.updated.timestamp(), parse_time("2026-09-11T10:00:00Z").timestamp());
        // No content record yet: the row reads creation, not the resume stamp.
        let mut fresh = wire_session();
        fresh.session_id = "fresh".into();
        fresh.created_at = "2026-09-10T10:00:00Z".into();
        fresh.updated_at = "2026-09-13T10:00:00Z".into();
        let row = SessionEntry::join(&fresh, None, None, &projects);
        assert_eq!(row.updated.timestamp(), parse_time("2026-09-10T10:00:00Z").timestamp());
    }

    /// B8: a refresh where only `updated_at` moved (a resume, no new turn)
    /// changes nothing on screen: same times, same order — and the reply
    /// compares unchanged, so no regroup, retitle or search rebuild runs.
    #[test]
    fn a_resume_only_refresh_keeps_position_and_time() {
        let projects = crate::projects::Projects::default();
        let mut older = wire_session();
        older.session_id = "older".into();
        older.created_at = "2026-09-10T10:00:00Z".into();
        older.last_activity_at = Some("2026-09-11T10:00:00Z".into());
        older.updated_at = "2026-09-11T10:00:00Z".into();
        older.turn_count = 3;
        let mut newer = wire_session();
        newer.session_id = "newer".into();
        newer.created_at = "2026-09-10T12:00:00Z".into();
        newer.last_activity_at = Some("2026-09-12T10:00:00Z".into());
        newer.updated_at = "2026-09-12T10:00:00Z".into();
        newer.turn_count = 5;
        let before: Vec<SessionEntry> =
            [&older, &newer].iter().map(|s| SessionEntry::join(s, None, None, &projects)).collect();
        assert_eq!(order_newest_first(&before), vec!["newer".to_owned(), "older".to_owned()]);
        // The resume: the server stamps `updated_at` "now"; no content moved.
        older.updated_at = "2026-09-13T10:00:00Z".into();
        let after: Vec<SessionEntry> =
            [&older, &newer].iter().map(|s| SessionEntry::join(s, None, None, &projects)).collect();
        assert_eq!(
            order_newest_first(&after),
            vec!["newer".to_owned(), "older".to_owned()],
            "a resume alone never moves the row"
        );
        let time = |rows: &[SessionEntry], id: &str| {
            rows.iter().find(|e| e.id == id).expect("row").updated.timestamp()
        };
        assert_eq!(time(&after, "older"), time(&before, "older"), "a resume alone never restamps the row");
        assert!(list_apply_unchanged(&before, &after), "a resume-only reply applies as unchanged");
    }

    /// B8: a new turn moves the row. Only the activity field advances here
    /// (the server stamp held fixed) to prove the row follows the activity
    /// field, not the server clock.
    #[test]
    fn a_new_turn_moves_the_row() {
        let projects = crate::projects::Projects::default();
        let mut older = wire_session();
        older.session_id = "older".into();
        older.created_at = "2026-09-10T10:00:00Z".into();
        older.last_activity_at = Some("2026-09-11T10:00:00Z".into());
        older.updated_at = "2026-09-11T10:00:00Z".into();
        older.turn_count = 3;
        let mut newer = wire_session();
        newer.session_id = "newer".into();
        newer.created_at = "2026-09-10T12:00:00Z".into();
        newer.last_activity_at = Some("2026-09-12T10:00:00Z".into());
        newer.updated_at = "2026-09-12T10:00:00Z".into();
        newer.turn_count = 5;
        let before: Vec<SessionEntry> =
            [&older, &newer].iter().map(|s| SessionEntry::join(s, None, None, &projects)).collect();
        assert_eq!(order_newest_first(&before), vec!["newer".to_owned(), "older".to_owned()]);
        // A turn settles on "older": content (and the count) move.
        older.last_activity_at = Some("2026-09-13T10:00:00Z".into());
        older.turn_count = 4;
        let after: Vec<SessionEntry> =
            [&older, &newer].iter().map(|s| SessionEntry::join(s, None, None, &projects)).collect();
        assert_eq!(order_newest_first(&after), vec!["older".to_owned(), "newer".to_owned()]);
        assert_eq!(
            after.iter().find(|e| e.id == "older").expect("row").updated.timestamp(),
            parse_time("2026-09-13T10:00:00Z").timestamp()
        );
        assert!(!list_apply_unchanged(&before, &after), "a turn is a real change and applies");
    }

    /// B8: the activity time needs no local memory — it rides the wire, so
    /// rejoining the same rows from scratch after a restart reproduces the
    /// same times in the same order, even past a resume that stamped
    /// `updated_at` "now" in between.
    #[test]
    fn activity_time_survives_a_restart_past_a_resume() {
        let projects = crate::projects::Projects::default();
        let mut older = wire_session();
        older.session_id = "older".into();
        older.created_at = "2026-09-10T10:00:00Z".into();
        older.last_activity_at = Some("2026-09-11T10:00:00Z".into());
        older.updated_at = "2026-09-11T10:00:00Z".into();
        older.turn_count = 3;
        let mut newer = wire_session();
        newer.session_id = "newer".into();
        newer.created_at = "2026-09-10T12:00:00Z".into();
        newer.last_activity_at = Some("2026-09-12T10:00:00Z".into());
        newer.updated_at = "2026-09-12T10:00:00Z".into();
        newer.turn_count = 5;
        let before: Vec<SessionEntry> =
            [&older, &newer].iter().map(|s| SessionEntry::join(s, None, None, &projects)).collect();
        // Restart: every local row dropped; meanwhile a resume stamped
        // "older"'s `updated_at`. Rejoin from the wire alone.
        older.updated_at = "2026-09-13T10:00:00Z".into();
        let rejoined: Vec<SessionEntry> =
            [&older, &newer].iter().map(|s| SessionEntry::join(s, None, None, &projects)).collect();
        assert_eq!(order_newest_first(&rejoined), order_newest_first(&before));
        for entry in &before {
            let again = rejoined.iter().find(|e| e.id == entry.id).expect("row");
            assert_eq!(again.updated.timestamp(), entry.updated.timestamp(), "restart keeps {}", entry.id);
        }
    }

    /// The cached join is the plain join, row for row, while reading each
    /// distinct root once: repeats hit the cache, a missing root falls back
    /// to its verbatim spelling exactly as the uncached path does, and a
    /// row with no root reads nothing at all.
    #[test]
    fn join_cached_matches_join_and_reads_each_root_once() {
        let base = roots_base();
        let root_a = base.join("cache-a");
        let root_b = base.join("cache-b");
        std::fs::create_dir_all(&root_a).expect("temp root");
        std::fs::create_dir_all(&root_b).expect("temp root");
        // Never created: canonicalization fails and the verbatim spelling
        // is what both paths must keep.
        let gone = base.join("cache-gone");
        let adopted = |id: &str, root: std::path::PathBuf| crate::projects::Project {
            id: id.to_owned(),
            root,
            name: id.to_owned(),
            colour: 1,
            pinned: false,
            added_at: "2026-09-13T10:00:00Z".into(),
            last_opened_at: "2026-09-13T10:00:00Z".into(),
            defaults: crate::projects::ProjectDefaults::default(),
        };
        let mut store = crate::projects::Projects::default();
        store.projects = vec![adopted("p-a", root_a.clone()), adopted("p-b", root_b.clone())];
        let roots: Vec<Option<String>> = vec![
            Some(root_a.to_string_lossy().into_owned()),
            Some(root_a.to_string_lossy().into_owned()),
            Some(root_b.to_string_lossy().into_owned()),
            Some(gone.to_string_lossy().into_owned()),
            None,
        ];
        let mut cache = crate::projects::CanonicalCache::default();
        for (i, root) in roots.iter().enumerate() {
            let mut session = wire_session();
            session.session_id = format!("c{i}");
            session.workspace_root = root.clone();
            let plain = SessionEntry::join(&session, None, None, &store);
            let cached = SessionEntry::join_cached(&session, None, None, &store, &mut cache);
            assert_eq!(plain, cached, "row {i} differs");
        }
        // The two adopted roots plus the missing one. The repeat and the
        // rootless row read nothing new.
        assert_eq!(cache.len(), 3);
        // The adopted rows resolved; the missing one fell back to Other.
        let resolved: Vec<Option<String>> = roots
            .iter()
            .map(|root| {
                let mut session = wire_session();
                session.workspace_root = root.clone();
                SessionEntry::join(&session, None, None, &store).project
            })
            .collect();
        assert_eq!(resolved[0].as_deref(), Some("p-a"));
        assert_eq!(resolved[2].as_deref(), Some("p-b"));
        assert_eq!(resolved[3], None);
        assert_eq!(resolved[4], None);
    }

    /// A generated title ranks directly under a user-given name: above the
    /// row's own words, the index title, and the derived first-prompt
    /// cache — and it settles `needs_title`, so no `session/read` chases a
    /// session that already has a name.
    #[test]
    fn a_generated_title_ranks_under_a_name_and_above_everything_else() {
        let projects = crate::projects::Projects::default();
        let index = old_index();
        let generated = || crate::sessions::SessionMeta {
            generated_title: Some("Tighten validation".into()),
            ..Default::default()
        };
        // Above the row's own title, the index title and the derived cache.
        let mut wired = wire_session();
        wired.title = Some("Wire Title".into());
        wired.first_user_prompt = Some("Wire prompt".into());
        let derived = crate::sessions::SessionMeta {
            derived_title: Some("First prompt".into()),
            ..Default::default()
        };
        let entry = SessionEntry::join(&wired, Some(&index), Some(&generated()), &projects);
        assert_eq!(entry.label, "Tighten validation");
        let entry = SessionEntry::join(&wired, Some(&index), Some(&derived), &projects);
        assert_eq!(entry.label, "Wire Title");
        let titled = SessionEntry::join(&wire_session(), None, Some(&generated()), &projects);
        assert_eq!(titled.label, "Tighten validation");
        assert!(!titled.needs_title, "a generated title settles the row");
        // …but a user-given name still wins.
        let named = crate::sessions::SessionMeta {
            name: Some("Ship it".into()),
            generated_title: Some("Tighten validation".into()),
            ..Default::default()
        };
        let entry = SessionEntry::join(&wired, Some(&index), Some(&named), &projects);
        assert_eq!(entry.label, "Ship it");
    }

    /// The crumb borrows the pending placeholder only while the row has no
    /// better name: a first-prompt label never flickers.
    #[test]
    fn the_crumb_borrows_the_placeholder_only_while_untitled() {
        assert_eq!(display_label(UNNAMED, true), PLACEHOLDER_NAMING);
        assert_eq!(display_label(UNNAMED, false), UNNAMED);
        assert_eq!(display_label("Fix the header", true), "Fix the header");
    }

    #[test]
    fn a_side_session_hides_from_the_wire_alone_with_no_local_record() {
        let projects = Projects::default();
        // The observed bug: a title side session listed with no override
        // drew a row titled with its own prompt.
        let mut side = wire_session();
        side.first_user_prompt = Some(crate::titles::title_prompt("Explain the layout"));
        let row = SessionEntry::join(&side, None, None, &projects);
        assert!(row.label.starts_with("Suggest a short title"), "the symptom: titled with its own prompt");
        assert!(row.side_marker, "recognised from the prompt prefix");
        assert!(row.hidden, "hidden with no local record");
        // The merge keeps it a hidden row, which is what the sidebar filter drops.
        let merged = merge_session_list(vec![row], &[]);
        assert!(merged.iter().all(|entry| entry.hidden));
        // The byline kind hides the same way; ordinary sessions are unaffected.
        let mut byline = wire_session();
        byline.first_user_prompt = Some(crate::byline::rewrite_prompt("fix it", "patched it"));
        let row = SessionEntry::join(&byline, None, None, &projects);
        assert!(row.hidden && row.side_marker);
        let mut ordinary = wire_session();
        ordinary.first_user_prompt = Some("Explain the layout".into());
        let row = SessionEntry::join(&ordinary, None, None, &projects);
        assert!(!row.hidden && !row.side_marker);
        // A side workspace marks the row even when the prompt is underivable.
        let mut rootless = wire_session();
        rootless.workspace_root =
            Some(crate::titles::side_workspace_dir().to_string_lossy().into_owned());
        let row = SessionEntry::join(&rootless, None, None, &projects);
        assert!(row.hidden && row.side_marker);
        // The local override path still hides exactly as before.
        let mut named = wire_session();
        named.first_user_prompt = Some("Explain the layout".into());
        let meta = SessionMeta { hidden: true, ..SessionMeta::default() };
        let row = SessionEntry::join(&named, None, Some(&meta), &projects);
        assert!(row.hidden && !row.side_marker);
        // Provisional rows hide the same way before the wire answers.
        let mut cache = crate::projects::CanonicalCache::default();
        let index = index_entry("", Some(&crate::titles::title_prompt("Explain the layout")));
        let row =
            SessionEntry::provisional("s", &index, None, &projects, &mut cache).expect("labelled");
        assert!(row.hidden && row.side_marker);
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
        let changed = first_send_update(&mut row, Some("Fix the header"), now, false);
        assert!(changed);
        assert!(row.running);
        assert_eq!(row.label, "Fix the header");
        assert_eq!(row.updated, now);
        assert!(!row.is_empty());
        // Titling is not naming: the empty filter's `named` exemption still
        // reads this as a first send, not a person's own choice.
        assert!(!row.named);
    }

    /// A send titles a zero-turn row only while no generated title stands,
    /// and a recorded send makes a zero-turn row non-empty.
    #[test]
    fn a_send_never_retitles_over_a_generated_title_and_a_sent_row_is_not_empty() {
        let mut overrides = crate::sessions::Overrides::new();
        assert_eq!(send_title("s-1", Some("raw prompt"), &overrides), Some("raw prompt"));
        overrides.entry("s-1".into()).or_default().generated_title = Some("Generated".into());
        assert_eq!(send_title("s-1", Some("raw prompt"), &overrides), None);
        let mut row = entry("s-1");
        row.label = "Generated".into();
        first_send_update(&mut row, send_title("s-1", Some("raw prompt"), &overrides), Local::now(), false);
        assert_eq!(row.label, "Generated", "the retry keeps the generated title");
        let mut sent = entry("s-2");
        assert!(sent.is_empty());
        sent.last_ask = Some("Explain the schema".into());
        assert!(!sent.is_empty(), "a recorded ask is a send");
        let mut store = crate::provider_sessions::ProviderSessionStore::new();
        assert!(!sent_to("s-3", &overrides));
        crate::provider_sessions::upsert_open(&mut store, &mut overrides, "codex", "s-3", None, None, None);
        crate::provider_sessions::note_first_prompt(&store, &mut overrides, "s-3", "Draw the db");
        assert!(sent_to("s-3", &overrides), "an admitted first prompt is a send");
    }

    /// The pre-1.3.0 shape still works the same way: a `local` placeholder
    /// row already in the list (rather than newly inserted) is revealed
    /// exactly like a wire one.
    #[test]
    fn first_send_update_reveals_a_local_entry() {
        let now = Local::now();
        let mut row = local_started_row("s-local", UNNAMED.to_owned(), None, None, now);
        row.running = false; // as it would be once parked back into `self.sessions`
        let changed = first_send_update(&mut row, Some("Fix the header"), now, false);
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
        let changed = first_send_update(&mut row, Some("Ignored prompt"), now, false);
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
        first_send_update(&mut row, Some("Ignored prompt"), now, false);
        assert_eq!(row.label, "My name");
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

    fn grouped_entry(id: &str, project: Option<&str>, workspace: Option<&str>, days_ago: i64) -> SessionEntry {
        let mut e = entry(id);
        e.label = format!("session {id}");
        e.project = project.map(str::to_owned);
        e.workspace = workspace.map(str::to_owned);
        e.updated = Local::now() - chrono::Duration::days(days_ago);
        e.turns = 1;
        e
    }

    #[test]
    fn a_replay_takes_its_workspace_from_the_capture() {
        let dir = std::env::temp_dir().join(format!("baaz-replay-ws-{}", std::process::id()));
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

    /// A session another client made — the `muse` CLI (no client name) or
    /// another app — draws no row and no hidden count, unless the person
    /// has used it in Baaz; Baaz's own, and an index that cannot say, draw
    /// as before (owner, 2026-11-12: 1,450 `muse exec` exam runs filled the
    /// sidebar).
    #[test]
    fn a_session_another_client_made_is_not_listed() {
        // Reads the state dir (`support_dir`), so it holds the lock the
        // tests that point `BAAZ_STATE_DIR` elsewhere hold.
        let _guard = crate::store::test_env_lock();
        // The state dir exists before the first look: the own-root check
        // canonicalizes it once, and a missing dir keeps its raw (`/var/…`)
        // spelling while the workspace made under it later resolves
        // (`/private/var/…`). Nothing else in the run is relied on to have
        // made it.
        let _ = std::fs::create_dir_all(crate::store::support_dir().join("workspace"));
        let projects = Projects::default();
        let made_by = |creator: Option<&str>| IndexEntry {
            title: "You are writing items for an exam".into(),
            created_by: creator.map(str::to_owned),
            ..IndexEntry::default()
        };
        let wire = wire_session();
        for creator in [Some(""), Some("cockpit")] {
            let index = made_by(creator);
            let row = SessionEntry::join(&wire, Some(&index), None, &projects);
            assert!(row.hidden && row.side_marker, "{creator:?} made it: no row");
            assert!(made_elsewhere(Some(&index), None, None));
        }
        for creator in [Some("baaz"), Some("baaz_check"), Some("harness"), None] {
            let index = made_by(creator);
            let row = SessionEntry::join(&wire, Some(&index), None, &projects);
            assert!(!row.hidden && !row.side_marker, "{creator:?}: listed as before");
        }
        // Adopted in Baaz — a pin, a project, a name — keeps it.
        let cli = made_by(Some(""));
        for meta in [
            SessionMeta { pinned: true, ..SessionMeta::default() },
            SessionMeta { project: Some("p".into()), ..SessionMeta::default() },
            SessionMeta { name: Some("mine".into()), ..SessionMeta::default() },
        ] {
            let row = SessionEntry::join(&wire, Some(&cli), Some(&meta), &projects);
            assert!(!row.side_marker, "used here: listed ({meta:?})");
        }
        // Archiving is not use, and neither is a byline: opening a row
        // used to write one, so the exam runs the owner clicked carried it.
        let archived = SessionMeta { archived: true, ..SessionMeta::default() };
        assert!(made_elsewhere(Some(&cli), None, Some(&archived)));
        let clicked = SessionMeta {
            last_ask: Some("You are writing items".into()),
            last_summary: Some("[{\"level\": \"tgt_lt\"".into()),
            ..SessionMeta::default()
        };
        assert!(made_elsewhere(Some(&cli), None, Some(&clicked)));
        // A temp-folder run is scripted whoever made it — Baaz's own
        // test and check runs included — but this run's own state dir is not.
        let baaz = made_by(Some("baaz"));
        assert!(made_elsewhere(Some(&baaz), Some("/private/var/folders/h8/x/T/exam-muse-dsyzyF"), None));
        assert!(made_elsewhere(Some(&baaz), Some("/tmp/bz.kfMJ/side-sessions"), None));
        assert!(!made_elsewhere(Some(&baaz), Some("/Users/me/baaz-sessions"), None));
        // Made first: a missing folder keeps its raw (`/var/…`) spelling
        // while the state dir above it resolves (`/private/var/…`), so the
        // answer would turn on whether another test had made it already.
        let own = crate::store::support_dir().join("workspace");
        let _ = std::fs::create_dir_all(&own);
        assert!(!made_elsewhere(Some(&baaz), Some(&own.to_string_lossy()), None), "this run's own workspace");
        // Baaz's helper sessions, from any state dir, by their label.
        assert!(crate::titles::looks_like_side_session(Some("baaz auto-title"), Some("/Users/me/Projects/x")));
        assert!(!crate::titles::looks_like_side_session(Some("baaz draft of my essay"), None));
        // The provisional boot row follows the same rule.
        let mut cache = crate::projects::CanonicalCache::default();
        let row = SessionEntry::provisional("s", &cli, None, &projects, &mut cache).expect("labelled");
        assert!(row.hidden && row.side_marker);
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
    fn provisional_rows_carry_only_what_the_index_knows() {
        let projects = Projects::default();
        let mut cache = crate::projects::CanonicalCache::default();
        // The label ladder is the index's own: title, then first prompt —
        // never Muse's random session name, and Muse's placeholder title
        // counts as none.
        let mut named = index_entry("Generated title", Some("why panic"));
        named.session_name = Some("flax-deneb".into());
        let row = SessionEntry::provisional("s", &named, None, &projects, &mut cache).expect("labelled");
        assert_eq!(row.label, "Generated title");
        let row = SessionEntry::provisional("s", &index_entry("Fix the parser", Some("why panic")), None, &projects, &mut cache).expect("labelled");
        assert_eq!(row.label, "Fix the parser");
        let row = SessionEntry::provisional("s", &index_entry("", Some("why panic")), None, &projects, &mut cache).expect("labelled");
        assert_eq!(row.label, "why panic");
        assert!(SessionEntry::provisional("s", &index_entry("New session", None), None, &projects, &mut cache).is_none());
        assert!(SessionEntry::provisional("s", &IndexEntry::default(), None, &projects, &mut cache).is_none());
        // A store name outranks the index, and the store's flags ride along
        // so a hidden session never flashes into view.
        let meta = SessionMeta { name: Some("mine".into()), hidden: true, pinned: true, ..SessionMeta::default() };
        let row = SessionEntry::provisional("s", &named, Some(&meta), &projects, &mut cache).expect("labelled");
        assert_eq!(row.label, "mine");
        assert!(row.hidden && row.pinned);
        assert!(row.provisional);
        // Nothing the wire owns: no turns, idle, no branch, no error — and
        // no title read may ever spend itself on a row the wire has not
        // listed yet.
        assert_eq!((row.turns, row.running, row.branch.clone(), row.last_error.clone()), (0, false, None, None));
        assert!(!row.needs_title);
        assert!(!row.local && !row.replayed);
        // The time is the index's, in epoch microseconds.
        assert_eq!(row.updated.timestamp_micros(), 1_700_000_000_000_000);
    }

    #[test]
    fn the_first_list_reply_replaces_provisional_rows_wholesale() {
        let projects = Projects::default();
        let mut cache = crate::projects::CanonicalCache::default();
        let provisional =
            SessionEntry::provisional("s", &index_entry("Fix the parser", Some("why panic")), None, &projects, &mut cache)
                .expect("labelled");
        // Provisional rows are never local, so the merge drops every one of
        // them the moment the wire answers — locals alone survive.
        let merged = merge_session_list(Vec::new(), std::slice::from_ref(&provisional));
        assert!(merged.is_empty());
        let wire = entry("s");
        let merged = merge_session_list(vec![wire.clone()], std::slice::from_ref(&provisional));
        assert_eq!(merged, vec![wire]);
        assert!(!merged.iter().any(|e| e.provisional));
    }

    #[test]
    fn a_repeat_list_reply_applies_nothing() {
        let wire = vec![entry("a"), entry("b")];
        // The same rows in the same order: a second apply must cost nothing.
        assert!(list_apply_unchanged(&wire, &wire));
        // A local row is the window's own and survives any merge, so it
        // never counts as a change either.
        let mut local = entry("new");
        local.local = true;
        let existing = vec![wire[0].clone(), wire[1].clone(), local];
        assert!(list_apply_unchanged(&existing, &wire));
        // Anything else — a reorder, a new row, a dropped row, a relabel —
        // is a real change. (Reordered from clones, so only the order
        // differs — fresh `entry` values would differ in their timestamps
        // too and prove nothing.)
        assert!(!list_apply_unchanged(&wire, &[wire[1].clone(), wire[0].clone()]));
        assert!(!list_apply_unchanged(&wire, &[entry("a")]));
        let mut relabelled = entry("a");
        relabelled.label = "changed".into();
        assert!(!list_apply_unchanged(&[relabelled, entry("b")], &wire));
        // And a provisional row never matches its wire successor: the first
        // reply always applies wholesale.
        let projects = Projects::default();
        let mut cache = crate::projects::CanonicalCache::default();
        let provisional =
            SessionEntry::provisional("a", &index_entry("Fix the parser", Some("why panic")), None, &projects, &mut cache)
                .expect("labelled");
        assert!(!list_apply_unchanged(&[provisional, entry("b")], &wire));
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

    /// The W5 title arm: the provider row's label is its row's own title —
    /// name, generated title, ack title, first prompt, derived title — in
    /// that order, all read from the one row.
    #[test]
    fn provider_labels_prefer_a_name_then_the_generated_title() {
        let projects = Projects::default();
        let record = provider_record();
        let mut facts = SessionMeta {
            provider_title: Some("Ack title".into()),
            first_prompt: Some("Fix the header".into()),
            ..SessionMeta::default()
        };
        let row = SessionEntry::provider_row(&record, Some(&facts), &projects).expect("a registered provider");
        assert_eq!(row.label, "Ack title");
        facts.provider_title = None;
        let row = SessionEntry::provider_row(&record, Some(&facts), &projects).expect("a registered provider");
        assert_eq!(row.label, "Fix the header");
        let meta = SessionMeta {
            first_prompt: Some("Fix the header".into()),
            generated_title: Some("Shiny generated".into()),
            last_ask: Some("Fix the header".into()),
            last_summary: Some("Fixed it".into()),
            ..SessionMeta::default()
        };
        let row = SessionEntry::provider_row(&record, Some(&meta), &projects).expect("a registered provider");
        assert_eq!(row.label, "Shiny generated", "the auto-title outranks the first prompt");
        assert_eq!(row.last_ask.as_deref(), Some("Fix the header"), "the byline ask rides along");
        let named = SessionMeta { name: Some("Mine".into()), ..SessionMeta::default() };
        let row = SessionEntry::provider_row(&record, Some(&named), &projects).expect("a registered provider");
        assert_eq!(row.label, "Mine", "a user-given name outranks everything");
    }

    /// The W5 merge arm: provider rows survive a `session/list` reply that
    /// never names them, and never count toward its unchanged check.
    #[test]
    fn the_list_merge_keeps_provider_rows_past_every_reply() {
        let projects = Projects::default();
        let provider = SessionEntry::provider_row(&provider_record(), None, &projects).expect("a registered provider");
        let merged = merge_session_list(Vec::new(), std::slice::from_ref(&provider));
        assert_eq!(merged.len(), 1, "the provider row survives an empty reply");
        assert_eq!(merged[0].provider, ProviderId::ClaudeCode);
        // A reply that matches the muse rows still applies wholesale with
        // a provider row standing beside them.
        let wire = SessionEntry::join(&wire_session(), None, None, &projects);
        let existing = vec![wire.clone(), provider];
        assert!(
            list_apply_unchanged(&existing, std::slice::from_ref(&wire)),
            "provider rows never count against the unchanged check"
        );
        let merged = merge_session_list(vec![wire], &existing);
        assert_eq!(merged.len(), 2, "wire rows and provider rows merge, newest-first downstream");
    }

    /// X3b fixtures: a 2-hop mixed chain, claude-code (A) → codex (B) →
    /// muse (C). Lane halves live in the provider store, the muse
    /// destination's half in the overrides — exactly what
    /// `persist_handoff_links` writes.
    fn chain_record(
        facts: &mut crate::sessions::Overrides,
        id: &str,
        provider: &str,
        turns: u64,
        prompt: Option<&str>,
        to: Option<&str>,
        from: Option<(&str, &str)>,
    ) -> crate::provider_sessions::ProviderSessionRecord {
        let meta = facts.entry(id.to_owned()).or_default();
        meta.first_prompt = prompt.map(str::to_owned);
        meta.handoff_to = to.map(str::to_owned);
        meta.handoff_from = from.map(|(source, _)| source.to_owned());
        meta.handoff_from_provider = from.map(|(_, provider)| provider.to_owned());
        crate::provider_sessions::ProviderSessionRecord {
            provider: provider.into(),
            session_id: id.into(),
            workspace: None,
            created_ms: 1_700_000_000_000,
            updated_ms: 1_700_000_000_000,
            turns,
            origin: None,
        }
    }

    fn chain_stores() -> (crate::provider_sessions::ProviderSessionStore, crate::sessions::Overrides) {
        let mut provider = crate::provider_sessions::ProviderSessionStore::new();
        let mut overrides = crate::sessions::Overrides::new();
        provider.insert(
            "chain-a".into(),
            chain_record(&mut overrides, "chain-a", "claude-code", 2, Some("Chart the Greyport ferry routes"), Some("chain-b"), None),
        );
        provider.insert(
            "chain-b".into(),
            chain_record(
                &mut overrides,
                "chain-b",
                "codex",
                3,
                Some("Second leg"),
                Some("chain-c"),
                Some(("chain-a", "claude-code")),
            ),
        );
        overrides.insert(
            "chain-c".into(),
            SessionMeta {
                handoff_from: Some("chain-b".into()),
                handoff_from_provider: Some("codex".into()),
                handoff_title: None,
                ..SessionMeta::default()
            },
        );
        (provider, overrides)
    }

    fn chain_rows(
        provider: &crate::provider_sessions::ProviderSessionStore,
        overrides: &crate::sessions::Overrides,
    ) -> Vec<SessionEntry> {
        let projects = Projects::default();
        let mut row_a = SessionEntry::provider_row(&provider["chain-a"], overrides.get("chain-a"), &projects).expect("a registered provider");
        row_a.updated = Local::now() - chrono::Duration::days(2);
        let mut row_b = SessionEntry::provider_row(&provider["chain-b"], overrides.get("chain-b"), &projects).expect("a registered provider");
        row_b.updated = Local::now() - chrono::Duration::days(1);
        let mut row_c = entry("chain-c");
        row_c.label = "Later words".into();
        // The wire's 5 minus the hidden pack exchange: the fixture mirrors
        // what `join` builds for a destination (see `visible_turns`).
        row_c.turns = visible_turns(5, true);
        row_c.updated = Local::now();
        vec![row_a, row_b, row_c]
    }

    /// A 2-hop mixed chain lists one row: the head's id, the original
    /// title, the summed visible turns, the head's provider badge and time.
    #[test]
    fn a_two_hop_mixed_chain_lists_one_row_with_the_original_title_and_summed_turns() {
        let (provider, overrides) = chain_stores();
        let rows = chain_rows(&provider, &overrides);
        assert_eq!(rows[0].label, "Chart the Greyport ferry routes");
        let collapsed = collapse_handoff_chains(rows, &provider, &overrides);
        assert_eq!(collapsed.len(), 1, "three members, one row, drew {collapsed:?}");
        let row = &collapsed[0];
        assert_eq!(row.id, "chain-c", "the head is what is listed");
        assert_eq!(row.label, "Chart the Greyport ferry routes", "the chain keeps its original title");
        assert_eq!(row.turns, 8, "2 + (3 - 1) + (5 - 1): each destination hides its pack exchange");
        assert_eq!(row.provider, ProviderId::Muse, "the chain's row is its head's: the Muse head, wearing Muse's mark");
    }

    /// The collapsed row keeps the head's own recency: no reorder jump
    /// beyond what `updated` would cause anyway.
    #[test]
    fn the_collapsed_row_reads_the_heads_own_time() {
        let (provider, overrides) = chain_stores();
        let rows = chain_rows(&provider, &overrides);
        let head_time = rows[2].updated;
        let collapsed = collapse_handoff_chains(rows, &provider, &overrides);
        assert_eq!(collapsed[0].updated, head_time);
    }

    /// Member ids resolve to the head; unlinked ids resolve to themselves.
    #[test]
    fn member_ids_resolve_to_the_head() {
        let (provider, overrides) = chain_stores();
        let rows = chain_rows(&provider, &overrides);
        assert_eq!(chain_head("chain-a", &provider, &overrides, &rows), "chain-c");
        assert_eq!(chain_head("chain-b", &provider, &overrides, &rows), "chain-c");
        assert_eq!(chain_head("chain-c", &provider, &overrides, &rows), "chain-c");
        assert_eq!(chain_head("unlinked", &provider, &overrides, &rows), "unlinked");
    }

    /// The whole chain is one expansion unit: archiving the head can cover
    /// every member, so none resurfaces.
    #[test]
    fn chain_members_cover_the_whole_chain() {
        let (provider, overrides) = chain_stores();
        let rows = chain_rows(&provider, &overrides);
        for id in ["chain-a", "chain-b", "chain-c"] {
            assert_eq!(
                chain_members(id, &provider, &overrides, &rows),
                vec!["chain-a".to_owned(), "chain-b".to_owned(), "chain-c".to_owned()],
                "every member expands to the whole chain from {id}"
            );
        }
        assert_eq!(chain_members("unlinked", &provider, &overrides, &rows), vec!["unlinked".to_owned()]);
    }

    /// Archiving the head hides all members: with every member flagged, the
    /// collapse still yields the single head row, archived.
    #[test]
    fn archiving_the_head_hides_all_members() {
        let (provider, mut overrides) = chain_stores();
        for id in ["chain-a", "chain-b", "chain-c"] {
            overrides.entry(id.into()).or_default().archived = true;
        }
        let projects = Projects::default();
        let mut rows = vec![
            SessionEntry::provider_row(&provider["chain-a"], overrides.get("chain-a"), &projects).expect("a registered provider"),
            SessionEntry::provider_row(&provider["chain-b"], overrides.get("chain-b"), &projects).expect("a registered provider"),
        ];
        let mut head = entry("chain-c");
        head.archived = true;
        head.turns = visible_turns(5, true);
        rows.push(head);
        let collapsed = collapse_handoff_chains(rows, &provider, &overrides);
        assert_eq!(collapsed.len(), 1);
        assert!(collapsed[0].archived, "the one row carries the archive flag: the visible filter drops it");
    }

    /// A user rename of the head wins over the chain's original title.
    #[test]
    fn a_renamed_head_keeps_its_own_title() {
        let (provider, overrides) = chain_stores();
        let mut rows = chain_rows(&provider, &overrides);
        rows[2].label = "Mine".into();
        rows[2].named = true;
        let collapsed = collapse_handoff_chains(rows, &provider, &overrides);
        assert_eq!(collapsed[0].label, "Mine");
        assert_eq!(collapsed[0].turns, 8, "the rename changes the title, not the count");
    }

    /// Y2a: the pack turn and its bubble are the handoff speaking, never
    /// words to title or byline from — matched by header and by display.
    #[test]
    fn pack_text_is_never_title_or_byline_material() {
        assert!(is_pack_text("Continuing a session handed off from Muse: goal words"));
        assert!(is_pack_text("Handed off from Muse: Fix it (3 recent turns, 1 open todo, 2 files touched)"));
        assert!(!is_pack_text("Fix the header"));
        assert!(!is_pack_text(""));
    }

    /// Y2a: the destination's first real message is a live turn, not a
    /// first send — the row goes running, the chain title stands.
    #[test]
    fn a_handoff_destinations_first_send_keeps_the_chain_title() {
        let now = Local::now();
        let mut row = entry("dest");
        row.label = "Chart the Greyport ferry routes".into();
        let changed = first_send_update(&mut row, Some("my next message"), now, true);
        assert!(changed, "the live turn still reveals the row");
        assert!(row.running);
        assert_eq!(row.label, "Chart the Greyport ferry routes");
        assert_eq!(row.updated, now);
    }

    /// Y2a3: only a handoff destination has a pack turn — a normal
    /// session whose first words start with "Handed off from " is titled
    /// from them like any other message.
    #[test]
    fn a_non_handoff_prompts_pack_shaped_words_still_title_the_row() {
        let now = Local::now();
        let mut row = entry("fresh");
        row.label = UNNAMED.into();
        let changed = first_send_update(
            &mut row,
            Some("Handed off from Muse: Fix it (3 recent turns, 0 open todos, 0 files touched)"),
            now,
            false,
        );
        assert!(changed, "the first send still reveals the row");
        assert!(row.running);
        assert_eq!(
            row.label,
            "Handed off from Muse: Fix it (3 recent turns, 0 open todos, 0 files touched)",
            "a non-destination's own words title it, pack-shaped or not"
        );
    }

    /// Y2a: the persisted chain title outranks the destination's own
    /// title and first prompt, all on the destination's one row.
    #[test]
    fn the_chain_title_outranks_a_destinations_own_words() {
        let projects = Projects::default();
        let mut record = provider_record();
        record.session_id = "dest".into();
        let mut meta = SessionMeta {
            provider_title: Some("Handed off from Muse: Fix it".into()),
            first_prompt: Some("my next message".into()),
            handoff_title: Some("Chart the Greyport ferry routes".into()),
            ..SessionMeta::default()
        };
        let row = SessionEntry::provider_row(&record, Some(&meta), &projects).expect("a registered provider");
        assert_eq!(row.label, "Chart the Greyport ferry routes");
        meta.handoff_title = None;
        let row = SessionEntry::provider_row(&record, Some(&meta), &projects).expect("a registered provider");
        assert_eq!(row.label, "Handed off from Muse: Fix it", "without a chain title the ladder is untouched");
    }

    /// Y2a: the head reads the chain title from its row.
    #[test]
    fn handoff_title_reads_from_the_row() {
        let mut overrides = crate::sessions::Overrides::new();
        assert!(handoff_title_of("dest", &overrides).is_none());
        overrides.insert(
            "dest".into(),
            SessionMeta { handoff_title: Some("  Chart the routes  ".into()), ..SessionMeta::default() },
        );
        assert_eq!(handoff_title_of("dest", &overrides).as_deref(), Some("Chart the routes"));
    }

    /// Y2a: the pack turn and its acknowledgement never become the
    /// byline — when the newest ask is the destination's pack, neither
    /// half lands — while real turns pass through untouched. Y2a3: the
    /// skip is gated on the destination — pack-shaped words on any other
    /// session land like any other byline.
    #[test]
    fn byline_skips_the_pack_turn_and_its_acknowledgement() {
        let (summary, ask) = byline_landable(
            Some("Context received; I will wait.".into()),
            Some("Handed off from Muse: Fix it (1 recent turn, 0 open todos, 0 files touched)".into()),
            true,
        );
        assert_eq!((summary, ask), (None, None));
        let (summary, ask) =
            byline_landable(Some("Fixed it".into()), Some("Fix the header".into()), false);
        assert_eq!(summary.as_deref(), Some("Fixed it"));
        assert_eq!(ask.as_deref(), Some("Fix the header"));
        let (summary, ask) = byline_landable(
            Some("Context received; I will wait.".into()),
            Some("Handed off from Muse: Fix it (1 recent turn, 0 open todos, 0 files touched)".into()),
            false,
        );
        assert_eq!(summary.as_deref(), Some("Context received; I will wait."));
        assert_eq!(
            ask.as_deref(),
            Some("Handed off from Muse: Fix it (1 recent turn, 0 open todos, 0 files touched)")
        );
    }

    /// Y2a: collapse is idempotent and order-independent — storage keeps
    /// every member, the view derives the same head row however the rows
    /// arrived.
    #[test]
    fn collapse_is_idempotent_in_any_order() {
        let (provider, mut overrides) = chain_stores();
        overrides.entry("chain-b".into()).or_default().handoff_title = Some("Chart the Greyport ferry routes".into());
        overrides.entry("chain-c".into()).or_default().handoff_title =
            Some("Chart the Greyport ferry routes".into());
        let mut rows = chain_rows(&provider, &overrides);
        // The tail row reads stale words now; the persisted chain title —
        // copied at activation — still wins over any member relabel.
        rows[0].label = "Stale tail words".into();
        let once = collapse_handoff_chains(rows.clone(), &provider, &overrides);
        assert_eq!(once.len(), 1, "three members, one row");
        assert_eq!(once[0].id, "chain-c");
        assert_eq!(once[0].label, "Chart the Greyport ferry routes", "the persisted title, not a member relabel");
        let twice = collapse_handoff_chains(once.clone(), &provider, &overrides);
        assert_eq!(twice, once, "a second run changes nothing");
        let mut reversed = rows;
        reversed.reverse();
        let flipped = collapse_handoff_chains(reversed, &provider, &overrides);
        assert_eq!(flipped, once, "arrival order changes nothing");
    }

    /// Y2a: the collapsed head carries the chain's state — pinned and
    /// needs-you attention ORed across members, newest time, summed
    /// honest turns. Y2a3: a live turn ORs across members too.
    #[test]
    fn the_collapsed_head_carries_the_chains_state() {
        use Attention;
        let (provider, overrides) = chain_stores();
        let mut rows = chain_rows(&provider, &overrides);
        rows[0].pinned = true;
        rows[0].running = true;
        rows[0].attention = vec![Attention::ApprovalPending];
        rows[1].attention = vec![Attention::InputPending];
        let collapsed = collapse_handoff_chains(rows, &provider, &overrides);
        assert_eq!(collapsed.len(), 1);
        let head = &collapsed[0];
        assert!(head.running, "a live turn on a member shows on the one row (Y2a3)");
        assert!(head.pinned, "pin rides the head even though the source holds it");
        assert!(
            head.attention.contains(&Attention::ApprovalPending)
                && head.attention.contains(&Attention::InputPending),
            "needs-you from any member shows on the one row, drew {:?}",
            head.attention
        );
    }

    /// An unlinked session passes through untouched.
    #[test]
    fn an_unlinked_session_is_unaffected() {
        let (provider, overrides) = chain_stores();
        let plain = entry("plain");
        let collapsed = collapse_handoff_chains(vec![plain.clone()], &provider, &overrides);
        assert_eq!(collapsed, vec![plain]);
    }

    /// A dangling `handoff_to` (destination missing from both stores and the
    /// rows) keeps the source listed.
    #[test]
    fn a_dangling_handoff_to_keeps_the_source_listed() {
        let (mut provider, mut overrides) = chain_stores();
        provider.insert(
            "dangling".into(),
            chain_record(&mut overrides, "dangling", "codex", 1, Some("Lost leg"), Some("gone"), None),
        );
        let projects = Projects::default();
        let rows = vec![
            SessionEntry::provider_row(&provider["dangling"], overrides.get("dangling"), &projects).expect("a registered provider"),
            entry("plain"),
        ];
        assert_eq!(chain_head("dangling", &provider, &overrides, &rows), "dangling");
        let collapsed = collapse_handoff_chains(rows, &provider, &overrides);
        assert_eq!(collapsed.len(), 2, "a missing destination lists the source");
        assert_eq!(collapsed[0].id, "dangling");
    }

    /// The destination row takes the source row's place: the head is
    /// emitted where the chain's first member stands.
    #[test]
    fn the_head_takes_the_source_rows_place() {
        let (provider, overrides) = chain_stores();
        let mut rows = chain_rows(&provider, &overrides);
        let plain = entry("plain");
        rows.insert(1, plain);
        let collapsed = collapse_handoff_chains(rows, &provider, &overrides);
        assert_eq!(collapsed.len(), 2);
        assert_eq!(collapsed[0].id, "chain-c", "the head sits in the source's slot");
        assert_eq!(collapsed[1].id, "plain");
    }

    /// A head with no row yet waits: members stay listed until the head's
    /// row arrives, and the next build heals the chain.
    #[test]
    fn a_headless_chain_waits_for_the_heads_row() {
        let (provider, overrides) = chain_stores();
        let rows = chain_rows(&provider, &overrides);
        let member_rows = vec![rows[0].clone(), rows[1].clone()];
        let collapsed = collapse_handoff_chains(member_rows, &provider, &overrides);
        assert_eq!(collapsed.len(), 2, "no head row, no collapse");
    }

    /// X3d: a settle on an unlinked session needs no collapse — the O(1)
    /// row refresh stands on its own — while any chain member's settle
    /// re-sums the chain.
    #[test]
    fn x3d_an_unlinked_settle_needs_no_collapse() {
        let (_, overrides) = chain_stores();
        assert!(!needs_collapse("unlinked", &overrides));
        assert!(!needs_collapse("no-such-session", &overrides));
        assert!(needs_collapse("chain-a", &overrides));
        assert!(needs_collapse("chain-b", &overrides));
        assert!(needs_collapse("chain-c", &overrides));
    }

    /// X3d: the hidden pack exchange is exactly one counted turn per
    /// destination member.
    #[test]
    fn x3d_visible_turns_hide_the_pack_exchange() {
        assert_eq!(visible_turns(5, false), 5);
        assert_eq!(visible_turns(2, true), 1);
        assert_eq!(visible_turns(0, true), 0, "saturates, never wraps");
    }

    /// Corrupt links must not hang the list build: a two-cycle lists both
    /// rows rather than looping.
    #[test]
    fn a_handoff_cycle_lists_rather_than_loops() {
        let mut provider = crate::provider_sessions::ProviderSessionStore::new();
        let mut overrides = crate::sessions::Overrides::new();
        provider.insert(
            "loop-a".into(),
            chain_record(&mut overrides, "loop-a", "claude-code", 1, Some("A leg"), Some("loop-b"), None),
        );
        provider.insert(
            "loop-b".into(),
            chain_record(&mut overrides, "loop-b", "codex", 1, Some("B leg"), Some("loop-a"), None),
        );
        let projects = Projects::default();
        let rows = vec![
            SessionEntry::provider_row(&provider["loop-a"], overrides.get("loop-a"), &projects).expect("a registered provider"),
            SessionEntry::provider_row(&provider["loop-b"], overrides.get("loop-b"), &projects).expect("a registered provider"),
        ];
        let collapsed = collapse_handoff_chains(rows, &provider, &overrides);
        assert_eq!(collapsed.len(), 2, "a cycle is two singletons, never a hang");
    }

    fn sorted_ids(rows: &mut [SessionEntry], order: crate::layout::SortBy) -> Vec<String> {
        sort_rows(rows, order);
        rows.iter().map(|e| e.id.clone()).collect()
    }

    #[test]
    fn sort_rows_orders_by_the_chosen_key_with_pinned_first() {
        use crate::layout::SortBy;
        let base = Local::now();
        let mk = |id: &str, label: &str, updated: i64, created: i64, pinned: bool| {
            let mut e = entry(id);
            e.label = label.into();
            e.updated = base - chrono::Duration::minutes(updated);
            e.created = base - chrono::Duration::minutes(created);
            e.pinned = pinned;
            e
        };
        let mut rows = vec![
            mk("old-pinned", "zeta", 500, 500, true),
            mk("a", "banana", 10, 400, false),
            mk("b", "Apple", 30, 100, false),
            mk("c", "apple", 20, 300, false),
        ];
        assert_eq!(sorted_ids(&mut rows, SortBy::Recent), ["old-pinned", "a", "c", "b"]);
        assert_eq!(sorted_ids(&mut rows, SortBy::Created), ["old-pinned", "b", "c", "a"]);
        // Case-insensitive name; the "apple" tie breaks by recency (c is newer than b).
        assert_eq!(sorted_ids(&mut rows, SortBy::Name), ["old-pinned", "c", "b", "a"]);
    }
}
