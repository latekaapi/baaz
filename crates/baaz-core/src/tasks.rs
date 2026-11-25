//! Orchestration's task record (docs/85 14.1, D-Q6 idea 1): one row per
//! piece of work a lead session hands to a worker, durable in baaz.db
//! ([`crate::db::tasks`]), reconciled when the app opens.
//!
//! The pure half lives here: the record, the moves its status may make,
//! generation fencing, what a reopen must do, and the typed refusals the
//! lead and worker tools answer with. Nothing here touches a session or a
//! file; the app carries out what [`reconcile`] and the outbox say.
//!
//! * **Changes go through an outbox.** A status change and the side effect
//!   it needs (start the worker, send it a message, stop it, wake the lead)
//!   are written in one transaction: the row and an [`OutboxItem`]. The app
//!   carries the item out and marks it done; an item still pending when the
//!   app opens is carried out again (each is safe to repeat: a start names
//!   its generation, a wake is one message per settle).
//! * **Generation fencing** (Orca): every attempt at a task has a number.
//!   A retry raises it; a worker's `report` names the generation its session
//!   was started for (from the bridge's session, rule 20), and a report from
//!   an older attempt is refused, so a stale worker never completes a
//!   retried task.

use serde::{Deserialize, Serialize};

/// What a task's worker may do (docs/85 14.3): a writer gets its own
/// worktree; a reviewer reads the writer's worktree and changes nothing; a
/// helper works in the lead's folder, read-only too.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Writer,
    Reviewer,
    Helper,
}

impl Role {
    pub fn parse(text: Option<&str>) -> Result<Self, Refusal> {
        match text.map(str::trim).unwrap_or("writer") {
            "" | "writer" | "write" => Ok(Self::Writer),
            "reviewer" | "review" => Ok(Self::Reviewer),
            "helper" | "read" | "research" => Ok(Self::Helper),
            other => Err(Refusal::new(Code::BadArgument, format!("unknown role `{other}` (writer, reviewer, helper)"))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Writer => "writer",
            Self::Reviewer => "reviewer",
            Self::Helper => "helper",
        }
    }

    /// Whether the worker may change files.
    pub fn writes(self) -> bool {
        self == Self::Writer
    }
}

/// Where a task stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Its `after` tasks have not all passed yet.
    Waiting,
    /// Ready; the worker has not started.
    Queued,
    /// The worker is working.
    Running,
    /// The worker said done (`report`); Baaz checks next.
    Reported,
    /// Baaz's check runs (14.5).
    Checking,
    /// The check passed: done.
    Passed,
    /// The check failed after the fix-up turn, or the worker gave up.
    Failed,
    /// Paused for the person: an escalation, a limit, a stall, a failed check.
    NeedsYou,
    /// Stopped by the lead or the person.
    Stopped,
    /// Merged by `task_land` (14.8).
    Landed,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Reported => "reported",
            Self::Checking => "checking",
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::NeedsYou => "needs_you",
            Self::Stopped => "stopped",
            Self::Landed => "landed",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        [
            Self::Waiting,
            Self::Queued,
            Self::Running,
            Self::Reported,
            Self::Checking,
            Self::Passed,
            Self::Failed,
            Self::NeedsYou,
            Self::Stopped,
            Self::Landed,
        ]
        .into_iter()
        .find(|s| s.as_str() == text)
    }

    /// Settled: nothing more happens without the lead or the person.
    pub fn settled(self) -> bool {
        matches!(self, Self::Passed | Self::Failed | Self::NeedsYou | Self::Stopped | Self::Landed)
    }

    /// Whether an `after` link on this task is met.
    pub fn unblocks(self) -> bool {
        matches!(self, Self::Passed | Self::Landed)
    }

    /// The moves a task may make. A retry (`Failed`/`NeedsYou`/`Stopped` →
    /// `Queued`) is a new generation ([`Task::retry`]).
    pub fn can_move(self, to: Self) -> bool {
        use Status::*;
        match (self, to) {
            (a, b) if a == b => false,
            (_, Stopped) => !matches!(self, Passed | Landed),
            (_, NeedsYou) => !matches!(self, Passed | Landed | Stopped),
            (Waiting, Queued) => true,
            (Queued, Running) => true,
            (Running, Reported) => true,
            (Reported, Checking) => true,
            (Checking, Passed | Failed | Running) => true,
            (Running, Failed) => true,
            (Passed, Landed) => true,
            (Failed | NeedsYou | Stopped, Queued) => true,
            (NeedsYou, Running) => true,
            _ => false,
        }
    }
}

/// How a criterion is proven (docs/85 14.4): a command check, the
/// reviewer's verdict, or the person. Anything else is "not proven".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Proof {
    Check,
    Reviewer,
    User,
    NotProven,
}

/// One acceptance criterion, frozen when the task is made.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Criterion {
    pub text: String,
    pub proof: Proof,
}

/// A task's ceiling (docs/85 14.2): tokens and minutes, then pause and ask.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budget {
    pub tokens: u64,
    pub minutes: u32,
}

impl Default for Budget {
    fn default() -> Self {
        Self { tokens: 2_000_000, minutes: 60 }
    }
}

/// One task.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    /// The lead: its session id and provider (the bridge's, rule 20).
    pub lead_session: String,
    pub lead_provider: String,
    /// The worker, once started (or the watched session, 14.2).
    pub worker_session: Option<String>,
    pub provider: String,
    pub model: Option<String>,
    /// The task that made this one (a worker delegating), if any.
    pub parent: Option<String>,
    /// 1 for a lead's own task, 2 for a worker's (the ceiling).
    pub depth: u8,
    pub role: Role,
    /// A writer's worktree folder.
    pub worktree: Option<String>,
    pub status: Status,
    pub title: String,
    pub prompt: String,
    /// What the worker gets beyond the prompt (a previous task's report and
    /// branch, 14.6; files; notes).
    pub inputs: Vec<String>,
    pub criteria: Vec<Criterion>,
    /// The check Baaz runs (14.5): the lead's, or the detected one.
    pub check: Option<String>,
    /// Paths the diff may touch (default: the worktree).
    pub scope: Vec<String>,
    pub verdict: Option<serde_json::Value>,
    pub report: Option<serde_json::Value>,
    pub budget: Budget,
    pub used_tokens: u64,
    pub generation: u32,
    /// Tasks that must pass first.
    pub after: Vec<String>,
    /// A watched session (`session_watch`): the task wraps a session the
    /// lead did not start.
    pub watched: bool,
    /// Workers of earlier attempts: their report is a stale generation's.
    #[serde(default)]
    pub past_workers: Vec<String>,
    /// The commit a writer's worktree was cut from (the verdict's "a
    /// commit exists" counts from here).
    #[serde(default)]
    pub base: Option<String>,
    /// Automatic fix-up turns spent on a failing check (one, 14.5).
    #[serde(default)]
    pub fixups: u8,
    pub created_at: i64,
    pub updated_at: i64,
    pub started_at: Option<i64>,
}

impl Task {
    /// Move to `to`, or refuse a move the status does not allow.
    pub fn move_to(&mut self, to: Status, now: i64) -> Result<(), Refusal> {
        if !self.status.can_move(to) {
            return Err(Refusal::new(Code::WrongState, format!("task {} is {} and cannot become {}", self.id, self.status.as_str(), to.as_str())));
        }
        if to == Status::Running && self.started_at.is_none() {
            self.started_at = Some(now);
        }
        self.status = to;
        self.updated_at = now;
        Ok(())
    }

    /// A new attempt: the generation rises, the last attempt's report,
    /// verdict and worker go (a stale worker's report is fenced off).
    pub fn retry(&mut self, now: i64) -> Result<(), Refusal> {
        self.move_to(Status::Queued, now)?;
        self.generation += 1;
        self.report = None;
        self.verdict = None;
        if let Some(old) = self.worker_session.take() {
            self.past_workers.push(old);
        }
        self.started_at = None;
        Ok(())
    }

    /// Whether a report from the worker started for `generation` may land.
    pub fn accepts_report(&self, generation: u32) -> Result<(), Refusal> {
        if generation != self.generation {
            return Err(Refusal::new(Code::StaleGeneration, format!("this worker ran attempt {generation}; task {} is on attempt {}", self.id, self.generation)));
        }
        if self.status != Status::Running {
            return Err(Refusal::new(Code::WrongState, format!("task {} is {}, not running", self.id, self.status.as_str())));
        }
        Ok(())
    }

    /// Its `after` links, all met in `all`.
    pub fn ready(&self, all: &[Task]) -> bool {
        self.after.iter().all(|id| all.iter().any(|t| &t.id == id && t.status.unblocks()))
    }
}

/// The deepest a task may sit: a lead's worker (1) may delegate once more (2).
pub const MAX_DEPTH: u8 = 2;
/// Writers at work at once, across every lead (fixed for now).
pub const MAX_WRITERS: usize = 4;

/// The depth a new task gets when the session `caller` delegates: 1 from a
/// lead, one more from a worker, refused past [`MAX_DEPTH`].
pub fn child_depth(caller_task: Option<&Task>) -> Result<u8, Refusal> {
    let depth = caller_task.map_or(1, |task| task.depth + 1);
    if depth > MAX_DEPTH {
        return Err(Refusal::new(Code::TooDeep, format!("a worker at depth {MAX_DEPTH} cannot delegate further")));
    }
    Ok(depth)
}

/// Writers running or about to (queued, running, reported, checking).
pub fn active_writers(tasks: &[Task]) -> usize {
    tasks.iter().filter(|t| t.role.writes() && matches!(t.status, Status::Queued | Status::Running | Status::Reported | Status::Checking)).count()
}

/// Refuse a fifth writer.
pub fn room_for(role: Role, tasks: &[Task]) -> Result<(), Refusal> {
    if role.writes() && active_writers(tasks) >= MAX_WRITERS {
        return Err(Refusal::new(Code::TooManyWriters, format!("{MAX_WRITERS} writers are already at work; wait for one (task_wait) or stop one")));
    }
    Ok(())
}

/// How permitted a mode is: Full access above Auto above Ask above Read-only.
fn permission_rank(mode: aui_protocol::PermissionMode) -> u8 {
    use aui_protocol::PermissionMode::*;
    match mode {
        AllowAll => 3,
        OnRequest => 2,
        PromptUnmatched => 1,
        DenyUnmatched => 0,
    }
}

/// The worker's mode: never more permitted than its parent's, and
/// Read-only for a reviewer or a helper (docs/85 14.2, 14.3).
pub fn child_mode(parent: aui_protocol::PermissionMode, role: Role) -> aui_protocol::PermissionMode {
    let cap = if role.writes() { aui_protocol::PermissionMode::AllowAll } else { aui_protocol::PermissionMode::DenyUnmatched };
    if permission_rank(cap) < permission_rank(parent) { cap } else { parent }
}

/// Over its ceiling: tokens used, or minutes since it started.
pub fn over_budget(task: &Task, now: i64) -> Option<String> {
    if task.used_tokens >= task.budget.tokens {
        return Some(format!("used {} tokens of its {}", task.used_tokens, task.budget.tokens));
    }
    let minutes = task.started_at.map(|at| (now - at).max(0) / 60_000).unwrap_or(0);
    (minutes >= i64::from(task.budget.minutes)).then(|| format!("ran {minutes} minutes of its {}", task.budget.minutes))
}

/// Where a task's worker works (docs/85 14.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    /// A writer's own new worktree, to be made.
    NewWorktree,
    /// An existing folder: a writer's worktree made already, a reviewer's
    /// writer's worktree, or the project root.
    Folder(String),
}

/// A writer works in a worktree of its own; a reviewer or helper reads
/// the worktree of the writer it follows (its first `after` writer that has
/// one), else the project root.
pub fn place_for(task: &Task, all: &[Task], root: &str) -> Place {
    if let Some(own) = &task.worktree {
        return Place::Folder(own.clone());
    }
    if task.role.writes() {
        return Place::NewWorktree;
    }
    let followed = task.after.iter().filter_map(|id| all.iter().find(|t| &t.id == id)).find(|t| t.role.writes() && t.worktree.is_some());
    Place::Folder(followed.and_then(|t| t.worktree.clone()).unwrap_or_else(|| root.to_owned()))
}

/// What the worker is told: the task, its inputs, its frozen criteria and
/// how to finish (rule 20: it never says who it is; Baaz knows).
pub fn worker_prompt(task: &Task) -> String {
    let mut out = String::new();
    out.push_str(&task.prompt);
    if !task.inputs.is_empty() {
        out.push_str("\n\nInputs:\n");
        for input in &task.inputs {
            out.push_str(&format!("- {input}\n"));
        }
    }
    if !task.criteria.is_empty() {
        out.push_str("\n\nAcceptance criteria (fixed; you cannot change them):\n");
        for (i, c) in task.criteria.iter().enumerate() {
            out.push_str(&format!("{}. {}\n", i + 1, c.text));
        }
    }
    if let Some(check) = &task.check {
        out.push_str(&format!("\nBaaz runs `{check}` after you report; it must pass.\n"));
    }
    let finish = if task.role.writes() {
        "When you are done, commit your work, then call the `report` tool with your result and, for each criterion, how you met it. If you are blocked or need a decision, call `escalate` with your question instead of guessing."
    } else {
        "Change nothing. When you are done, call the `report` tool with your findings and, for each criterion, your verdict. If you are blocked, call `escalate` with your question."
    };
    out.push_str(&format!("\n{finish}"));
    out
}

/// One criterion as the worker reported it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    pub criterion: String,
    pub met: bool,
    #[serde(default)]
    pub evidence: String,
}

/// A worker's claims, checked against the frozen criteria (docs/85 14.4):
/// a claim may name a criterion by its text or its number; one that names
/// no frozen criterion is an amendment, refused.
pub fn match_claims(criteria: &[Criterion], claims: &[Claim]) -> Result<Vec<Option<Claim>>, Refusal> {
    let mut out: Vec<Option<Claim>> = vec![None; criteria.len()];
    for claim in claims {
        let key = claim.criterion.trim();
        let by_number = key.trim_start_matches('#').trim_end_matches('.').parse::<usize>().ok().filter(|n| *n >= 1 && *n <= criteria.len()).map(|n| n - 1);
        let by_text = criteria.iter().position(|c| c.text.eq_ignore_ascii_case(key) || key.to_lowercase().contains(&c.text.to_lowercase()));
        match by_number.or(by_text) {
            Some(i) => out[i] = Some(claim.clone()),
            None => return Err(Refusal::new(Code::CriteriaFrozen, format!("\"{key}\" is not one of this task's criteria; they are fixed (report against them as given)"))),
        }
    }
    Ok(out)
}

/// One criterion's line in the lead's summary: met and proven, met by the
/// worker's word only (not proven), or not met.
pub fn criterion_line(criterion: &Criterion, claim: Option<&Claim>, check_passed: Option<bool>, reviewer_ok: Option<bool>) -> serde_json::Value {
    let (state, by) = match criterion.proof {
        Proof::Check => match check_passed {
            Some(true) => ("met", "the check"),
            Some(false) => ("not met", "the check"),
            None => ("not proven", "no check ran"),
        },
        Proof::Reviewer => match reviewer_ok {
            Some(true) => ("met", "the reviewer"),
            Some(false) => ("not met", "the reviewer"),
            None => ("not proven", "no reviewer yet"),
        },
        Proof::User => ("for you to judge", "you"),
        Proof::NotProven => ("not proven", "nothing checks it"),
    };
    serde_json::json!({"criterion": criterion.text, "state": state, "by": by, "worker_says": claim.map(|c| if c.met { "met" } else { "not met" }), "evidence": claim.map(|c| c.evidence.clone())})
}

/// A side effect a status change needs, carried out by the app.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Effect {
    /// Start (or restart) the worker for `generation`.
    Start { generation: u32 },
    /// Send the worker a message.
    Send { text: String },
    /// Stop the worker's turn.
    Stop,
    /// Run the check (14.5).
    Check,
    /// Wake the lead once: its tasks settled, or one needs the person.
    WakeLead { lead_session: String },
}

/// One outbox row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutboxItem {
    pub id: i64,
    pub task_id: String,
    pub effect: Effect,
}

/// What reopening the app must do (docs/85 14.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reconcile {
    /// Carry out an outbox item that never finished.
    Replay(OutboxItem),
    /// A running task whose worker session is not live: resume it from the
    /// provider's own files (rule 18) with "Continue from where you
    /// stopped", same generation.
    Resume { task_id: String },
    /// A waiting task whose `after` tasks all passed meanwhile.
    Release { task_id: String },
}

/// The reconcile on open: pending outbox items first, then running tasks
/// with no live worker, then waiting tasks now ready. `live` says whether a
/// session's provider child is up.
pub fn reconcile(tasks: &[Task], pending: &[OutboxItem], live: &dyn Fn(&str) -> bool) -> Vec<Reconcile> {
    let mut out: Vec<Reconcile> = pending.iter().cloned().map(Reconcile::Replay).collect();
    let replayed = |id: &str| pending.iter().any(|item| item.task_id == id && matches!(item.effect, Effect::Start { .. }));
    for task in tasks {
        match task.status {
            Status::Running if !replayed(&task.id) && task.worker_session.as_deref().is_none_or(|s| !live(s)) => {
                out.push(Reconcile::Resume { task_id: task.id.clone() });
            }
            Status::Waiting if task.ready(tasks) => out.push(Reconcile::Release { task_id: task.id.clone() }),
            _ => {}
        }
    }
    out
}

/// A refusal's typed code (docs/85 14.2): the tools answer with it so a
/// lead can act on the kind, not parse words.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Code {
    BadArgument,
    NotFound,
    NotYours,
    WrongState,
    StaleGeneration,
    TooDeep,
    TooManyWriters,
    MorePermitted,
    ProviderUnavailable,
    CriteriaFrozen,
    OverBudget,
}

/// A refusal: its code and plain words.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refusal {
    pub code: Code,
    pub message: String,
}

impl Refusal {
    pub fn new(code: Code, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

#[cfg(test)]
pub(crate) fn sample(id: &str, status: Status) -> Task {
    Task {
        id: id.into(),
        lead_session: "lead".into(),
        lead_provider: "claude-code".into(),
        worker_session: None,
        provider: "codex".into(),
        model: None,
        parent: None,
        depth: 1,
        role: Role::Writer,
        worktree: None,
        status,
        title: format!("task {id}"),
        prompt: "do it".into(),
        inputs: Vec::new(),
        criteria: vec![Criterion { text: "tests pass".into(), proof: Proof::Check }],
        check: Some("cargo test".into()),
        scope: Vec::new(),
        verdict: None,
        report: None,
        budget: Budget::default(),
        used_tokens: 0,
        generation: 0,
        after: Vec::new(),
        watched: false,
        past_workers: Vec::new(),
        base: None,
        fixups: 0,
        created_at: 1,
        updated_at: 1,
        started_at: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_move_only_along_the_flow() {
        let mut t = sample("t", Status::Queued);
        assert!(t.move_to(Status::Passed, 2).is_err(), "no skipping the check");
        t.move_to(Status::Running, 2).unwrap();
        assert_eq!(t.started_at, Some(2));
        t.move_to(Status::Reported, 3).unwrap();
        t.move_to(Status::Checking, 4).unwrap();
        t.move_to(Status::Passed, 5).unwrap();
        assert!(t.move_to(Status::Stopped, 6).is_err(), "a passed task is not stopped");
        t.move_to(Status::Landed, 6).unwrap();
        assert_eq!(t.move_to(Status::Running, 7).unwrap_err().code, Code::WrongState);
        for s in ["waiting", "needs_you", "landed"] {
            assert_eq!(Status::parse(s).unwrap().as_str(), s);
        }
    }

    /// Orca's generation fencing: a retried task refuses the old worker's
    /// report and takes the new one's.
    #[test]
    fn a_stale_worker_cannot_complete_a_retried_task() {
        let mut t = sample("t", Status::Queued);
        t.move_to(Status::Running, 1).unwrap();
        t.worker_session = Some("w-1".into());
        t.move_to(Status::NeedsYou, 2).unwrap();
        t.retry(3).unwrap();
        assert_eq!((t.generation, t.worker_session.clone(), t.status), (1, None, Status::Queued));
        t.move_to(Status::Running, 4).unwrap();
        assert_eq!(t.accepts_report(0).unwrap_err().code, Code::StaleGeneration);
        assert!(t.accepts_report(1).is_ok());
    }

    /// A restart mid-task: the pending start replays; a running task with
    /// no live worker resumes; a waiting task whose link passed while the
    /// app was closed is released; settled and live ones are left.
    #[test]
    fn reopening_replays_the_outbox_resumes_and_releases() {
        let mut running = sample("run", Status::Running);
        running.worker_session = Some("w-dead".into());
        let mut live = sample("live", Status::Running);
        live.worker_session = Some("w-live".into());
        let mut started = sample("started", Status::Running);
        started.worker_session = Some("w-2".into());
        let passed = sample("first", Status::Passed);
        let mut waiting = sample("second", Status::Waiting);
        waiting.after = vec!["first".into()];
        let mut blocked = sample("third", Status::Waiting);
        blocked.after = vec!["second".into()];
        let tasks = vec![running, live, started, passed, waiting, blocked];
        let pending = vec![OutboxItem { id: 7, task_id: "started".into(), effect: Effect::Start { generation: 0 } }];
        let out = reconcile(&tasks, &pending, &|s| s == "w-live");
        assert_eq!(
            out,
            vec![
                Reconcile::Replay(pending[0].clone()),
                Reconcile::Resume { task_id: "run".into() },
                Reconcile::Release { task_id: "second".into() },
            ]
        );
    }

    /// docs/85 14.2's limits: depth 2, four writers, a child never more
    /// permitted than its parent, the ceiling.
    #[test]
    fn the_limits_hold() {
        use aui_protocol::PermissionMode::*;
        assert_eq!(child_depth(None).unwrap(), 1);
        let mut worker = sample("w", Status::Running);
        assert_eq!(child_depth(Some(&worker)).unwrap(), 2);
        worker.depth = 2;
        assert_eq!(child_depth(Some(&worker)).unwrap_err().code, Code::TooDeep);
        let tasks: Vec<Task> = (0..4).map(|i| sample(&format!("t{i}"), Status::Running)).collect();
        assert_eq!(room_for(Role::Writer, &tasks).unwrap_err().code, Code::TooManyWriters);
        assert!(room_for(Role::Reviewer, &tasks).is_ok(), "readers are not counted");
        assert!(room_for(Role::Writer, &tasks[..3]).is_ok());
        assert_eq!(child_mode(OnRequest, Role::Writer), OnRequest);
        assert_eq!(child_mode(PromptUnmatched, Role::Writer), PromptUnmatched, "never more permitted than the parent");
        assert_eq!(child_mode(AllowAll, Role::Reviewer), DenyUnmatched, "a reviewer reads");
        assert_eq!(child_mode(DenyUnmatched, Role::Writer), DenyUnmatched);
        let mut t = sample("b", Status::Running);
        t.started_at = Some(0);
        assert!(over_budget(&t, 59 * 60_000).is_none());
        assert!(over_budget(&t, 60 * 60_000).unwrap().contains("60 minutes"));
        t.used_tokens = 2_000_000;
        assert!(over_budget(&t, 0).unwrap().contains("tokens"));
    }

    /// docs/85 14.3: roles decide where a worker works.
    #[test]
    fn writers_get_a_worktree_and_reviewers_read_the_writers() {
        let mut writer = sample("w", Status::Passed);
        let fresh = sample("w2", Status::Queued);
        assert_eq!(place_for(&fresh, &[], "/repo"), Place::NewWorktree);
        writer.worktree = Some("/state/worktrees/repo/w".into());
        assert_eq!(place_for(&writer, &[], "/repo"), Place::Folder("/state/worktrees/repo/w".into()), "a made worktree is kept on a retry");
        let mut reviewer = sample("r", Status::Queued);
        reviewer.role = Role::Reviewer;
        reviewer.after = vec!["w".into()];
        assert_eq!(place_for(&reviewer, &[writer.clone()], "/repo"), Place::Folder("/state/worktrees/repo/w".into()));
        reviewer.after.clear();
        assert_eq!(place_for(&reviewer, &[writer], "/repo"), Place::Folder("/repo".into()), "no writer followed: the root, read-only");
    }

    #[test]
    fn the_worker_is_told_its_criteria_check_and_how_to_finish() {
        let mut t = sample("p", Status::Queued);
        t.inputs = vec!["branch baaz/q-1".into()];
        let text = worker_prompt(&t);
        assert!(text.starts_with("do it"));
        assert!(text.contains("1. tests pass") && text.contains("`cargo test`") && text.contains("commit your work") && text.contains("branch baaz/q-1"));
        t.role = Role::Reviewer;
        assert!(worker_prompt(&t).contains("Change nothing"));
    }

    /// docs/85 14.4: a worker reports against the frozen criteria, by text
    /// or number; anything else is an amendment, refused.
    #[test]
    fn a_worker_cannot_amend_the_criteria() {
        let criteria = vec![Criterion { text: "tests pass".into(), proof: Proof::Check }, Criterion { text: "README updated".into(), proof: Proof::NotProven }];
        let claims = vec![Claim { criterion: "2".into(), met: true, evidence: "added a section".into() }, Claim { criterion: "Tests pass".into(), met: true, evidence: String::new() }];
        let matched = match_claims(&criteria, &claims).unwrap();
        assert!(matched.iter().all(Option::is_some));
        let amended = vec![Claim { criterion: "it compiles, which is enough".into(), met: true, evidence: String::new() }];
        assert_eq!(match_claims(&criteria, &amended).unwrap_err().code, Code::CriteriaFrozen);
        assert_eq!(criterion_line(&criteria[0], None, Some(true), None)["state"], "met");
        assert_eq!(criterion_line(&criteria[1], matched[0].as_ref(), Some(true), None)["state"], "not proven", "a worker's word alone proves nothing");
    }

    #[test]
    fn roles_parse_and_say_whether_they_write() {
        assert_eq!(Role::parse(None).unwrap(), Role::Writer);
        assert!(!Role::parse(Some("reviewer")).unwrap().writes());
        assert_eq!(Role::parse(Some("boss")).unwrap_err().code, Code::BadArgument);
    }
}
