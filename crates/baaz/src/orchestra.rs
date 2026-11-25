//! Orchestration, the app half (docs/85 Stage 14, D-Q6 option B): the lead
//! and worker tools on the bridge, carried out on sessions.
//!
//! A lead session delegates a task to a worker session on any provider; the
//! task is a row in baaz.db ([`baaz_core::tasks`], [`crate::db::tasks`]),
//! its side effects go through the outbox, and the worker's session is an
//! ordinary Baaz session opened in the background
//! ([`Harness::open_worker`]): the person is never moved to it.
//!
//! * The caller is the bridge's session (rule 20): a lead is whoever calls
//!   `delegate`; a worker is the session a task row names.
//! * Every refusal carries a typed code ([`baaz_core::tasks::Code`]).
//! * The database is read and written off the UI thread; this module hops
//!   back to it to touch sessions.

use std::collections::HashMap;

use baaz_core::tasks::{self as core, Budget, Code, Criterion, Effect, Proof, Refusal, Role, Status, Task};
use gpui::{AppContext as _, Context};
use serde_json::{json, Value};

use crate::app::Harness;
use crate::terminal::service::PaneCall;

/// The orchestration tools the harness answers (lead, then worker).
pub const ORCHESTRA_TOOL_NAMES: [&str; 11] = [
    "agents_available",
    "delegate",
    "task_status",
    "task_send",
    "task_stop",
    "task_wait",
    "task_land",
    "session_send",
    "session_watch",
    "report",
    "escalate",
];

/// What the app keeps of orchestration in memory: which live session is
/// which task's worker (the database is the truth; this answers "is this
/// session a worker?" on every event without a read). Owned by the
/// session registry.
#[derive(Default)]
pub(crate) struct Orchestra {
    /// Worker session id → task id.
    pub workers: HashMap<String, String>,
}

/// A refusal as the tool's error text: its code first, so a lead can act
/// on the kind.
fn refuse(refusal: Refusal) -> String {
    let code = serde_json::to_value(refusal.code).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default();
    format!("[{code}] {}", refusal.message)
}

fn now() -> i64 {
    baaz_core::clock::now_ms() as i64
}

fn text(params: &Value, key: &str) -> Option<String> {
    params.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned)
}

fn texts(params: &Value, key: &str) -> Vec<String> {
    match params.get(key) {
        Some(Value::Array(items)) => items.iter().filter_map(|v| v.as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned)).collect(),
        Some(Value::String(one)) if !one.trim().is_empty() => vec![one.trim().to_owned()],
        _ => Vec::new(),
    }
}

/// Run `work` against baaz.db on the writer thread, from any thread.
fn with_db<R: Send + 'static>(work: impl FnOnce(&mut rusqlite::Connection) -> R + Send + 'static) -> Option<R> {
    crate::db::call(crate::db::path(), move |opened| work(&mut opened.conn))
}

/// One task as a lead reads it: short (rule 19).
pub(crate) fn task_summary(task: &Task, live: Option<&str>) -> Value {
    let mut v = json!({
        "task": task.id,
        "title": task.title,
        "provider": task.provider,
        "role": task.role.as_str(),
        "status": task.status.as_str(),
        "attempt": task.generation + 1,
    });
    if let Some(started) = task.started_at {
        v["minutes"] = json!((now() - started).max(0) / 60_000);
    }
    if let Some(worker) = &task.worker_session {
        v["session"] = json!(worker);
    }
    if let Some(line) = live {
        v["now"] = json!(line);
    }
    if let Some(report) = &task.report {
        v["report"] = report.clone();
    }
    if let Some(verdict) = &task.verdict {
        v["verdict"] = verdict.clone();
    }
    v
}

impl Harness {
    /// The service's hook for an orchestration tool.
    pub(crate) fn orchestra_call(&mut self, call: PaneCall, cx: &mut Context<Self>) {
        match call.tool.as_str() {
            "agents_available" => {
                let answer = self.agents_available(cx);
                call.answer(Ok(answer));
            }
            "delegate" => self.delegate_call(call, cx),
            "task_status" => self.task_status_call(call, cx),
            "task_stop" => self.task_stop_call(call, cx),
            "task_send" => self.task_send_call(call, cx),
            "session_send" => {
                let outcome = match (text(&call.params, "session").or_else(|| text(&call.params, "sessionId")), text(&call.params, "text")) {
                    (Some(to), Some(words)) => self.deliver_from(&call.session, &to, &words, cx),
                    _ => Err(refuse(Refusal::new(Code::BadArgument, "name the session and the text"))),
                };
                call.answer(outcome);
            }
            "session_watch" => self.session_watch_call(call, cx),
            "task_wait" => self.task_wait_call(call, cx),
            other => call.answer(Err(refuse(Refusal::new(Code::NotFound, format!("{other} is not built yet"))))),
        }
    }

    /// `agents_available`: per provider, whether it can take a task now.
    fn agents_available(&self, cx: &mut Context<Self>) -> Value {
        use baaz_core::provider_status::{Auth, Installed};
        let statuses = self.provider_statuses(cx).to_vec();
        let agents: Vec<Value> = crate::providers::ProviderId::all()
            .into_iter()
            .map(|id| {
                let status = statuses.iter().find(|s| s.provider == id);
                let installed = status.is_some_and(|s| matches!(s.installed, Installed::Yes { .. }));
                let signed_in = status.is_some_and(|s| matches!(s.auth, Auth::SignedIn { .. }));
                let billing = status.map(|s| s.auth.billing_mode());
                let enabled = baaz_core::provider_status::provider_enabled(id);
                let models: Vec<&str> = if id.as_str() == "claude-code" { baaz_core::providers::claude_code_models().iter().map(|m| m.id).collect() } else { Vec::new() };
                json!({
                    "provider": id.as_str(),
                    "name": id.label(),
                    "ready": installed && signed_in && enabled,
                    "signed_in": signed_in,
                    "billing": billing.map(|b| format!("{b:?}").to_lowercase()),
                    "models": if models.is_empty() { json!("the provider's own (pass a model id it knows, or omit for its default)") } else { json!(models) },
                    "steers": baaz_core::providers::capability_state(id, provider::Capability::SteerTurn).allows_attempt(),
                })
            })
            .collect();
        json!({"agents": agents, "limits": {"depth": core::MAX_DEPTH, "writers": core::MAX_WRITERS, "budget": {"tokens": Budget::default().tokens, "minutes": Budget::default().minutes}}})
    }

    /// `delegate`: make the task, then start its worker (or leave it
    /// waiting on its `after` links).
    fn delegate_call(&mut self, call: PaneCall, cx: &mut Context<Self>) {
        let params = call.params.clone();
        let Some(prompt) = text(&params, "task").or_else(|| text(&params, "prompt")) else {
            return call.answer(Err(refuse(Refusal::new(Code::BadArgument, "say what the worker should do (task)"))));
        };
        let role = match Role::parse(text(&params, "role").as_deref()) {
            Ok(role) => role,
            Err(r) => return call.answer(Err(refuse(r))),
        };
        let provider = text(&params, "provider").unwrap_or_else(|| "claude-code".into());
        let Some(provider_id) = crate::providers::ProviderId::parse_known(&provider) else {
            return call.answer(Err(refuse(Refusal::new(Code::BadArgument, format!("unknown provider `{provider}` (agents_available lists them)")))));
        };
        let Some(lead) = self.registry.find_runtime(self.active.as_ref(), &call.session, cx) else {
            return call.answer(Err(refuse(Refusal::new(Code::NotFound, "this session is not open in Baaz"))));
        };
        let (lead_provider, lead_mode) = {
            let rt = lead.read(cx);
            (rt.provider_kind().as_str().to_owned(), rt.mode())
        };
        let criteria: Vec<Criterion> = texts(&params, "criteria").into_iter().map(|text| Criterion { proof: proof_for(&text), text }).collect();
        let budget = Budget {
            tokens: params.pointer("/budget/tokens").and_then(Value::as_u64).unwrap_or(Budget::default().tokens),
            minutes: params.pointer("/budget/minutes").and_then(Value::as_u64).map(|m| m as u32).unwrap_or(Budget::default().minutes),
        };
        let caller = call.session.clone();
        let root = call.root.to_string_lossy().into_owned();
        let task = Task {
            id: format!("task-{}", uuid::Uuid::new_v4().simple()),
            lead_session: caller.clone(),
            lead_provider,
            worker_session: None,
            provider: provider_id.as_str().to_owned(),
            model: text(&params, "model"),
            parent: None,
            depth: 1,
            role,
            worktree: None,
            status: Status::Queued,
            title: text(&params, "title").unwrap_or_else(|| prompt.chars().take(60).collect()),
            prompt,
            inputs: texts(&params, "inputs"),
            criteria,
            check: text(&params, "check"),
            scope: texts(&params, "scope"),
            verdict: None,
            report: None,
            budget,
            used_tokens: 0,
            generation: 0,
            after: texts(&params, "after"),
            watched: false,
            past_workers: Vec::new(),
            created_at: now(),
            updated_at: now(),
            started_at: None,
        };
        let mode = core::child_mode(lead_mode, role);
        cx.spawn(async move |this, cx| {
            // Off the UI thread: the limits read every task.
            let made = cx
                .background_spawn(async move {
                    with_db(move |conn| -> Result<Task, Refusal> {
                        let all = crate::db::tasks::load_all(conn).map_err(|e| Refusal::new(Code::WrongState, e.to_string()))?;
                        let mut task = task;
                        let parent = all.iter().find(|t| t.worker_session.as_deref() == Some(caller.as_str()) && !t.status.settled());
                        task.depth = core::child_depth(parent)?;
                        task.parent = parent.map(|p| p.id.clone());
                        if let Some(missing) = task.after.iter().find(|id| !all.iter().any(|t| &t.id == *id)) {
                            return Err(Refusal::new(Code::NotFound, format!("no task {missing} to wait for")));
                        }
                        if !task.after.is_empty() && !task.ready(&all) {
                            task.status = Status::Waiting;
                        } else {
                            core::room_for(task.role, &all)?;
                        }
                        let effect = (task.status == Status::Queued).then_some(Effect::Start { generation: 0 });
                        crate::db::tasks::save_with(conn, &task, effect.as_ref()).map_err(|e| Refusal::new(Code::WrongState, e.to_string()))?;
                        Ok(task)
                    })
                    .unwrap_or_else(|| Err(Refusal::new(Code::WrongState, "Baaz's database is not open")))
                })
                .await;
            let task = match made {
                Ok(task) => task,
                Err(refusal) => return call.answer(Err(refuse(refusal))),
            };
            let answer = json!({"task": task.id, "status": task.status.as_str(), "role": task.role.as_str(), "provider": task.provider,
                "note": if task.status == Status::Waiting { "waits for its after tasks; starts by itself" } else { "starting; task_status shows it, and you are woken when your tasks settle" }});
            call.answer(Ok(answer));
            if task.status == Status::Queued {
                let _ = this.update(cx, |this, cx| this.start_worker(task, mode, root, cx));
            }
        })
        .detach();
    }

    /// Carry out a task's start: open its worker in the background, send
    /// it the task, mark it running.
    pub(crate) fn start_worker(&mut self, task: Task, mode: aui_protocol::PermissionMode, root: String, cx: &mut Context<Self>) {
        let Some(provider_id) = crate::providers::ProviderId::parse_known(&task.provider) else { return };
        let workspace = task.worktree.clone().unwrap_or(root);
        let project = self.session_list.read(cx).projects.resolve(Some(&workspace), None).map(|p| p.id.clone());
        let model = task.model.clone();
        self.run_on_main(cx, move |this, window, cx| {
            this.open_worker(provider_id, model, mode, project, workspace, move |this, opened, cx| this.worker_opened(task, opened, cx), window, cx);
        });
    }

    fn worker_opened(&mut self, mut task: Task, opened: Result<String, String>, cx: &mut Context<Self>) {
        let session = match opened {
            Ok(session) => session,
            Err(why) => {
                crate::baaz_log!("orchestra: worker for {} did not open ({why})", task.id);
                let _ = task.move_to(Status::NeedsYou, now());
                task.verdict = Some(json!({"needs_you": format!("the worker did not start: {why}")}));
                let lead = task.lead_session.clone();
                cx.background_spawn(async move {
                    with_db(move |conn| crate::db::tasks::save_with(conn, &task, Some(&Effect::WakeLead { lead_session: lead })));
                })
                .detach();
                return;
            }
        };
        self.registry.orchestra.workers.insert(session.clone(), task.id.clone());
        task.worker_session = Some(session.clone());
        let _ = task.move_to(Status::Running, now());
        let prompt = core::worker_prompt(&task);
        if let Some(runtime) = self.registry.find_runtime(self.active.as_ref(), &session, cx) {
            runtime.update(cx, |rt, cx| rt.send_or_queue(prompt, cx));
        }
        cx.background_spawn(async move {
            with_db(move |conn| {
                let _ = crate::db::tasks::save(conn, &task);
                if let Ok(pending) = crate::db::tasks::pending(conn) {
                    for item in pending.iter().filter(|i| i.task_id == task.id && matches!(i.effect, Effect::Start { .. })) {
                        let _ = crate::db::tasks::mark_done(conn, item.id);
                    }
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// `task_status`: the lead's tasks (or one), each short, with what its
    /// worker is doing now.
    fn task_status_call(&mut self, call: PaneCall, cx: &mut Context<Self>) {
        let lead = call.session.clone();
        let one = text(&call.params, "task");
        cx.spawn(async move |this, cx| {
            let tasks = cx.background_spawn(async move { with_db(move |conn| crate::db::tasks::for_lead(conn, &lead).unwrap_or_default()).unwrap_or_default() }).await;
            let tasks: Vec<Task> = tasks.into_iter().filter(|t| one.as_ref().is_none_or(|id| &t.id == id)).collect();
            if let Some(id) = &one {
                if tasks.is_empty() {
                    return call.answer(Err(refuse(Refusal::new(Code::NotFound, format!("no task {id} of yours")))));
                }
            }
            let rows = this
                .update(cx, |this, cx| {
                    tasks.iter().map(|t| task_summary(t, this.worker_line(t, cx).as_deref())).collect::<Vec<_>>()
                })
                .unwrap_or_default();
            call.answer(Ok(json!({"tasks": rows})));
        })
        .detach();
    }

    /// What a task's worker shows now: working, idle, waiting on the person.
    fn worker_line(&self, task: &Task, cx: &gpui::App) -> Option<String> {
        let session = task.worker_session.as_deref()?;
        let runtime = self.registry.find_runtime(self.active.as_ref(), session, cx)?;
        let rt = runtime.read(cx);
        Some(if rt.working() { "working".into() } else { "idle".into() })
    }

    /// `task_stop`: stop the worker's turn; the task is stopped.
    fn task_stop_call(&mut self, call: PaneCall, cx: &mut Context<Self>) {
        let Some(id) = text(&call.params, "task") else {
            return call.answer(Err(refuse(Refusal::new(Code::BadArgument, "name the task"))));
        };
        let lead = call.session.clone();
        cx.spawn(async move |this, cx| {
            let stopped = cx
                .background_spawn(async move {
                    with_db(move |conn| -> Result<Task, Refusal> {
                        let mut task = crate::db::tasks::load(conn, &id).ok().flatten().ok_or_else(|| Refusal::new(Code::NotFound, format!("no task {id}")))?;
                        if task.lead_session != lead {
                            return Err(Refusal::new(Code::NotYours, format!("task {id} is another session's")));
                        }
                        task.move_to(Status::Stopped, now())?;
                        crate::db::tasks::save_with(conn, &task, Some(&Effect::Stop)).map_err(|e| Refusal::new(Code::WrongState, e.to_string()))?;
                        Ok(task)
                    })
                    .unwrap_or_else(|| Err(Refusal::new(Code::WrongState, "Baaz's database is not open")))
                })
                .await;
            match stopped {
                Ok(task) => {
                    let _ = this.update(cx, |this, cx| {
                        if let Some(runtime) = task.worker_session.as_deref().and_then(|s| this.registry.find_runtime(this.active.as_ref(), s, cx)) {
                            runtime.update(cx, |rt, cx| rt.interrupt(cx));
                        }
                    });
                    call.answer(Ok(json!({"task": task.id, "status": "stopped"})));
                }
                Err(refusal) => call.answer(Err(refuse(refusal))),
            }
        })
        .detach();
    }
}

impl Harness {
    /// Deliver `words` from the session `from` to the session `to`, marked
    /// as an agent's (it carries no approval): steered into a running turn
    /// where the provider steers, queued behind it where it doesn't, a new
    /// turn on an idle session. Only a session open in Baaz.
    pub(crate) fn deliver_from(&mut self, from: &str, to: &str, words: &str, cx: &mut Context<Self>) -> Result<Value, String> {
        if from == to {
            return Err(refuse(Refusal::new(Code::BadArgument, "that is this session")));
        }
        let runtime = self
            .registry
            .find_runtime(self.active.as_ref(), to, cx)
            .ok_or_else(|| refuse(Refusal::new(Code::NotFound, format!("session {to} is not open in Baaz; open it first"))))?;
        let title = self.session_list.read(cx).sessions.iter().find(|row| row.id == from).map(|row| row.label.clone()).unwrap_or_else(|| "another session".into());
        let message = agent_message(&title, words);
        let (running, busy, steers) = {
            let rt = runtime.read(cx);
            (rt.turn_running(), rt.is_sending(), baaz_core::providers::capability_state(rt.provider_kind(), provider::Capability::SteerTurn).allows_attempt())
        };
        // A steer needs a turn the server has accepted; one still being
        // submitted (or a provider that does not steer) queues behind.
        let steer = running && steers;
        let how = match (steer, busy) {
            (true, _) => "steered into its running turn",
            (false, true) => "queued for the end of its turn",
            (false, false) => "sent; it starts a turn",
        };
        runtime.update(cx, |rt, cx| if steer { rt.steer_text(message, cx) } else { rt.send_or_queue(message, cx) });
        Ok(json!({"delivered": how, "session": to}))
    }

    /// `task_send`: a message to one of the caller's tasks' worker.
    fn task_send_call(&mut self, call: PaneCall, cx: &mut Context<Self>) {
        let (Some(id), Some(words)) = (text(&call.params, "task"), text(&call.params, "text")) else {
            return call.answer(Err(refuse(Refusal::new(Code::BadArgument, "name the task and the text"))));
        };
        let lead = call.session.clone();
        cx.spawn(async move |this, cx| {
            let found = cx.background_spawn(async move { with_db(move |conn| crate::db::tasks::load(conn, &id).ok().flatten()).flatten() }).await;
            let outcome = match found {
                None => Err(refuse(Refusal::new(Code::NotFound, "no such task"))),
                Some(task) if task.lead_session != lead => Err(refuse(Refusal::new(Code::NotYours, format!("task {} is another session's", task.id)))),
                Some(task) => match task.worker_session.clone() {
                    None => Err(refuse(Refusal::new(Code::WrongState, format!("task {} has no worker yet ({})", task.id, task.status.as_str())))),
                    Some(worker) => this.update(cx, |this, cx| this.deliver_from(&lead, &worker, &words, cx)).unwrap_or_else(|_| Err("the window closed".into())),
                },
            };
            call.answer(outcome);
        })
        .detach();
    }

    /// `session_watch`: existing sessions become the caller's tasks, so
    /// the verdict (14.5) and the land (14.8) apply to them.
    fn session_watch_call(&mut self, call: PaneCall, cx: &mut Context<Self>) {
        let sessions = texts(&call.params, "sessions");
        if sessions.is_empty() {
            return call.answer(Err(refuse(Refusal::new(Code::BadArgument, "name the sessions to watch"))));
        }
        let mut tasks = Vec::new();
        for session in &sessions {
            if session == &call.session {
                return call.answer(Err(refuse(Refusal::new(Code::BadArgument, "a session cannot watch itself"))));
            }
            let Some(runtime) = self.registry.find_runtime(self.active.as_ref(), session, cx) else {
                return call.answer(Err(refuse(Refusal::new(Code::NotFound, format!("session {session} is not open in Baaz")))));
            };
            let rt = runtime.read(cx);
            let workspace = rt.workspace_path().to_owned();
            let title = self.session_list.read(cx).sessions.iter().find(|row| &row.id == session).map(|row| row.label.clone()).unwrap_or_else(|| session.clone());
            let working = rt.working();
            tasks.push(Task {
                id: format!("task-{}", uuid::Uuid::new_v4().simple()),
                lead_session: call.session.clone(),
                lead_provider: String::new(),
                worker_session: Some(session.clone()),
                provider: rt.provider_kind().as_str().to_owned(),
                model: None,
                parent: None,
                depth: 1,
                role: Role::Writer,
                worktree: Self::in_worktree(&workspace).then_some(workspace),
                status: if working { Status::Running } else { Status::Reported },
                title,
                prompt: String::new(),
                inputs: Vec::new(),
                criteria: Vec::new(),
                check: text(&call.params, "check"),
                scope: Vec::new(),
                verdict: None,
                report: None,
                budget: Budget::default(),
                used_tokens: 0,
                generation: 0,
                after: Vec::new(),
                watched: true,
                past_workers: Vec::new(),
                created_at: now(),
                updated_at: now(),
                started_at: Some(now()),
            });
        }
        for task in &tasks {
            if let Some(worker) = &task.worker_session {
                self.registry.orchestra.workers.insert(worker.clone(), task.id.clone());
            }
        }
        let rows: Vec<Value> = tasks.iter().map(|t| task_summary(t, None)).collect();
        cx.background_spawn(async move {
            with_db(move |conn| {
                for task in &tasks {
                    // A watched session already idle is checked at once (14.5).
                    let effect = (task.status == Status::Reported).then_some(Effect::Check);
                    let _ = crate::db::tasks::save_with(conn, task, effect.as_ref());
                }
            });
        })
        .detach();
        call.answer(Ok(json!({"watching": rows})));
    }

    /// `task_wait`: up to 60 seconds for a task to settle (short only: a
    /// lead is woken when its tasks settle, 14.6).
    fn task_wait_call(&mut self, call: PaneCall, cx: &mut Context<Self>) {
        let Some(id) = text(&call.params, "task") else {
            return call.answer(Err(refuse(Refusal::new(Code::BadArgument, "name the task"))));
        };
        let seconds = call.params.get("seconds").and_then(Value::as_u64).unwrap_or(30).clamp(1, 60);
        let lead = call.session.clone();
        cx.background_spawn(async move {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
            loop {
                let (id2, lead2) = (id.clone(), lead.clone());
                let found = with_db(move |conn| crate::db::tasks::load(conn, &id2).ok().flatten()).flatten();
                match found {
                    None => break call.answer(Err(refuse(Refusal::new(Code::NotFound, format!("no task {id}"))))),
                    Some(task) if task.lead_session != lead2 => break call.answer(Err(refuse(Refusal::new(Code::NotYours, format!("task {id} is another session's"))))),
                    Some(task) if task.status.settled() || std::time::Instant::now() >= deadline => {
                        let mut v = task_summary(&task, None);
                        if !task.status.settled() {
                            v["note"] = json!("not settled yet; end your turn and you will be woken when it is");
                        }
                        break call.answer(Ok(v));
                    }
                    Some(_) => std::thread::sleep(std::time::Duration::from_millis(500)),
                }
            }
        })
        .detach();
    }
}

/// A message from another session, as its receiver reads it: from an
/// agent, never the person, so it carries no approval (docs/85 14.2).
pub(crate) fn agent_message(from_title: &str, words: &str) -> String {
    format!("From session \"{from_title}\" (an agent, not the user):\n\n{words}")
}

/// How a criterion will be proven, from its words (docs/85 14.4): a
/// command or test is the check's, a review the reviewer's, a taste the
/// person's; anything else is not proven.
pub(crate) fn proof_for(text: &str) -> Proof {
    let t = text.to_lowercase();
    if ["test", "build", "compile", "lint", "pass", "exit", "runs", "check"].iter().any(|w| t.contains(w)) {
        Proof::Check
    } else if ["review", "reviewer", "approved", "correct", "accurate"].iter().any(|w| t.contains(w)) {
        Proof::Reviewer
    } else if ["you", "the user", "i like", "looks", "feel"].iter().any(|w| t.contains(w)) {
        Proof::User
    } else {
        Proof::NotProven
    }
}
