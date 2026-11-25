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
        self.worker_session = None;
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

    #[test]
    fn roles_parse_and_say_whether_they_write() {
        assert_eq!(Role::parse(None).unwrap(), Role::Writer);
        assert!(!Role::parse(Some("reviewer")).unwrap().writes());
        assert_eq!(Role::parse(Some("boss")).unwrap_err().code, Code::BadArgument);
    }
}
