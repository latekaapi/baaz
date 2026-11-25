//! The pane tools, the app half (docs/85 13.1–13.4, W4): an agent's call to
//! `pane_show`, `changes_read`, `changes_revert`, `changes_comment`,
//! `changes_commit` or `pr_status` arrives from the terminal service as a
//! [`PaneCall`] and is answered here, through the right pane's own paths
//! (what the person would click), never a second implementation.
//!
//! * The calling session is the bridge's (rule 20). Only the session on
//!   screen changes what is on screen: a `pane_show` from a session in the
//!   background is kept on the pane ([`RightPane::pending_shows`]) and
//!   shown when the person opens that session.
//! * Git reads run on a worker thread and answer the call from there.
//! * What a read means is [`baaz_core::pane_tools`] (pure, tested on a
//!   fixture repository).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use baaz_core::pane_tools::{self as core, Scope, Show, TurnEnds};
use gpui::Context;
use serde_json::{json, Value};

use crate::app::Harness;
use crate::layout::RightKind;
use crate::right_scopes::ChangesScope;
use crate::terminal::service::PaneCall;

/// One agent's line comment in Changes (docs/85 13.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LineComment {
    pub line: u32,
    pub text: String,
    /// Who wrote it: the agent's name ("Claude Code", "Codex", …).
    pub author: String,
}

/// Agents' comments: project root → path → comments, in the order written.
pub(crate) type Comments = HashMap<PathBuf, HashMap<String, Vec<LineComment>>>;

/// An agent's push waiting on the person (docs/85 13.4, rule 29): the
/// dialog carries `seq`; `done` answers the tool if it still waits.
pub(crate) struct AgentPush {
    pub seq: u64,
    pub root: PathBuf,
    pub done: std::sync::mpsc::Sender<Result<Value, String>>,
}

/// How long `changes_commit` waits for the person to approve a push before
/// it answers "waiting" (the confirm stays up and still pushes).
pub(crate) const PUSH_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

/// How long `changes_revert` waits for the person before it answers
/// "waiting for the person" (the confirm stays up and still applies).
pub(crate) const REVERT_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

impl Harness {
    /// The service's hook: one pane tool call (see the module doc).
    pub(crate) fn pane_tool_call(&mut self, call: PaneCall, cx: &mut Context<Self>) {
        match call.tool.as_str() {
            "pane_show" => {
                let outcome = self.pane_show(&call.session, &call.root, &call.params, cx);
                call.answer(outcome);
            }
            "changes_read" => self.changes_read_call(call, cx),
            "changes_revert" => self.changes_revert_call(call, cx),
            "changes_commit" => self.changes_commit_call(call, cx),
            "changes_comment" => {
                let outcome = self.changes_comment(&call.session, &call.root, &call.params, cx);
                call.answer(outcome);
            }
            "pr_status" => {
                std::thread::spawn(move || {
                    let pr = baaz_core::git_ops::pr_for_branch(&call.root);
                    call.answer(Ok(core::pr_summary(pr.as_ref())));
                });
            }
            other => call.answer(Err(format!("unknown pane tool: {other}"))),
        }
    }

    /// The id of the session on screen, if any.
    fn session_in_front(&self, cx: &gpui::App) -> Option<String> {
        self.active.as_ref().map(|view| view.read(cx).rt(cx).session_id.clone())
    }

    /// `pane_show`: show it now when `session` is on screen, else keep it
    /// for when the person opens that session.
    pub(crate) fn pane_show(&mut self, session: &str, root: &Path, params: &Value, cx: &mut Context<Self>) -> Result<Value, String> {
        let show = Show::parse(root, params)?;
        if let Show::Files { path: Some(path), .. } = &show {
            if !root.join(path).exists() {
                return Err(format!("no such file in the project: {path}"));
            }
        }
        if self.session_in_front(cx).as_deref() != Some(session) {
            let words = show.words();
            self.right_pane.update(cx, |pane, _| pane.pending_shows.insert(session.to_owned(), show));
            return Ok(json!({"shown": false, "queued": format!("{words} will show when the person opens this session")}));
        }
        self.apply_show(session, root, show.clone(), cx)?;
        Ok(json!({"shown": show.words()}))
    }

    /// A kept `pane_show` for `session`, now on screen: show it.
    pub(crate) fn show_pending_pane(&mut self, session: &str, cx: &mut Context<Self>) {
        let Some(show) = self.right_pane.update(cx, |pane, _| pane.pending_shows.remove(session)) else { return };
        let Some((root, _)) = self.right_project(cx) else { return };
        if let Err(why) = self.apply_show(session, &root, show, cx) {
            crate::baaz_log!("pane_show: a kept ask could not show ({why})");
        }
    }

    /// Put `show` on screen through the pane's own paths. The agent's
    /// open never takes the person's keyboard.
    fn apply_show(&mut self, session: &str, root: &Path, show: Show, cx: &mut Context<Self>) -> Result<(), String> {
        match show {
            Show::Files { path: None, .. } => self.open_right_on(RightKind::Files, cx),
            Show::Files { path: Some(path), line } => {
                let abs = root.join(&path);
                let is_dir = abs.is_dir();
                self.reveal_link_path(root, &abs, Some(is_dir), line.map(|line| line..line + 1), cx);
            }
            Show::Changes { scope, path } => {
                self.open_right_on(RightKind::Changes, cx);
                #[cfg(feature = "probe")]
                if scope != Scope::All {
                    self.with_right_pane(cx, |pane, cx| pane.fixture_scopes_for_capture(root, cx));
                }
                let scope = self.changes_scope_for(session, root, &scope, cx)?;
                let files: Vec<String> = path.into_iter().collect();
                self.with_right_pane(cx, |pane, cx| pane.show_scope(root, scope, files, cx))?;
            }
            Show::Browser => self.show_browser_for_agent(session, cx),
            Show::Terminal => self.show_terminal(cx),
            Show::Tasks => self.open_right_on(RightKind::Tasks, cx),
            Show::Pr => {
                self.open_right_on(RightKind::Changes, cx);
                self.with_right_pane(cx, |pane, cx| {
                    let _ = pane.show_scope(root, ChangesScope::All, Vec::new(), cx);
                    pane.changes_ui(root).pr_read_at = None;
                    pane.refresh_now(cx);
                });
            }
        }
        // Every arm above goes through a path that already redraws.
        Ok(())
    }

    /// The pane's scope for an agent's: Turn N and Last turn by turn id
    /// (the loaded index's last finished turn, else the session's turns
    /// with the running one left out).
    fn changes_scope_for(&mut self, session: &str, root: &Path, scope: &Scope, cx: &mut Context<Self>) -> Result<ChangesScope, String> {
        let order = |this: &Self, cx: &gpui::App| -> Result<(Vec<String>, bool), String> {
            let runtime = this.registry.find_runtime(this.active.as_ref(), session, cx).ok_or("this session is not open")?;
            let rt = runtime.read(cx);
            Ok((rt.assistant_turn_ids(), rt.working()))
        };
        Ok(match scope {
            Scope::All => ChangesScope::All,
            Scope::Session => ChangesScope::Session,
            Scope::LastTurn => {
                if let Some(id) = self.with_right_pane(cx, |pane, _| pane.last_indexed_turn(root)) {
                    ChangesScope::Turn(id)
                } else {
                    let (mut ids, working) = order(self, cx)?;
                    if working {
                        ids.pop();
                    }
                    ChangesScope::Turn(ids.pop().ok_or("this session has no finished turn yet")?)
                }
            }
            Scope::Turn(n) => {
                let (ids, _) = order(self, cx)?;
                ChangesScope::Turn(ids.get(n - 1).cloned().ok_or_else(|| format!("this session has {} agent turns", ids.len()))?)
            }
        })
    }

    /// `changes_read`: the scope's turns are named here (the session's
    /// order), their checkpoints and the diff read on a worker thread.
    fn changes_read_call(&mut self, call: PaneCall, cx: &mut Context<Self>) {
        let text = |key: &str| call.params.get(key).and_then(Value::as_str).map(str::to_owned);
        let scope = match Scope::parse(text("scope").as_deref()) {
            Ok(scope) => scope,
            Err(why) => return call.answer(Err(why)),
        };
        let file = text("file").or_else(|| text("path"));
        let facts = if scope == Scope::All {
            None
        } else {
            match self.registry.find_runtime(self.active.as_ref(), &call.session, cx) {
                Some(runtime) => {
                    let rt = runtime.read(cx);
                    Some((rt.provider_kind().as_str().to_owned(), rt.session_id.clone(), rt.assistant_turn_ids()))
                }
                None => return call.answer(Err("this session is not open in Baaz".into())),
            }
        };
        std::thread::spawn(move || {
            let outcome = (|| {
                let ends = match facts {
                    None => None,
                    Some((provider, id, order)) => core::ends(&scope, &session_turns(&call.root, &provider, &id, &order))?,
                };
                core::read(&call.root, &scope, ends.as_ref(), file.as_deref())
            })();
            call.answer(outcome);
        });
    }
}

impl Harness {
    /// `changes_revert`: the pane's Revert for one file — back to `HEAD`,
    /// or out of one turn — through the session's own confirm. The answer
    /// waits [`REVERT_WAIT`] for the person.
    fn changes_revert_call(&mut self, call: PaneCall, cx: &mut Context<Self>) {
        let text = |key: &str| call.params.get(key).and_then(Value::as_str).map(str::to_owned);
        let Some(path) = text("path").map(|p| core::normalise_path(&call.root, &p)).filter(|p| !p.is_empty()) else {
            return call.answer(Err("name the file to revert (path)".into()));
        };
        let scope = match Scope::parse(text("scope").as_deref()) {
            Ok(Scope::Session) => return call.answer(Err("revert works on all changes (back to HEAD) or one turn (last_turn, turn_<n>)".into())),
            Ok(scope) => scope,
            Err(why) => return call.answer(Err(why)),
        };
        let Some(runtime) = self.registry.find_runtime(self.active.as_ref(), &call.session, cx) else {
            return call.answer(Err("this session is not open in Baaz".into()));
        };
        let facts = {
            let rt = runtime.read(cx);
            (rt.provider_kind().as_str().to_owned(), rt.session_id.clone(), rt.assistant_turn_ids())
        };
        cx.spawn(async move |this, cx| {
            let (root, read_scope) = (call.root.clone(), scope.clone());
            let turn = cx
                .background_executor()
                .spawn(async move {
                    if read_scope == Scope::All {
                        return Ok(None);
                    }
                    let (provider, id, order) = facts;
                    let ends = core::ends(&read_scope, &session_turns(&root, &provider, &id, &order))?.ok_or("no turn")?;
                    let to = ends.to.clone().ok_or("that turn has no end checkpoint")?;
                    Ok::<_, String>(Some((ends.turn_id.unwrap_or_default(), ends.number.unwrap_or(0), ends.from, to)))
                })
                .await;
            let turn = match turn {
                Ok(turn) => turn,
                Err(why) => return call.answer(Err(why)),
            };
            let (tx, rx) = std::sync::mpsc::channel();
            let asked = this.update(cx, |_, cx| runtime.update(cx, |rt, cx| rt.ask_revert_file_for_agent(&call.root, &path, turn, tx, cx)));
            match asked {
                Ok(Ok(())) => {
                    std::thread::spawn(move || match rx.recv_timeout(REVERT_WAIT) {
                        Ok(outcome) => call.answer(outcome),
                        Err(_) => call.answer(Ok(json!({"waiting": "waiting for the person: the confirm is still up, and it reverts if they press Revert"}))),
                    });
                }
                Ok(Err(why)) => call.answer(Err(why)),
                Err(_) => call.answer(Err("the window closed".into())),
            }
        })
        .detach();
    }

    /// `changes_comment`: a line comment the person sees under that file's
    /// diff in Changes. On screen, Changes opens on the file.
    pub(crate) fn changes_comment(&mut self, session: &str, root: &Path, params: &Value, cx: &mut Context<Self>) -> Result<Value, String> {
        let text = |key: &str| params.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
        let path = text("path").map(|p| core::normalise_path(root, p)).filter(|p| !p.is_empty()).ok_or("name the file (path)")?;
        let line = params.get("line").and_then(|l| l.as_u64().or_else(|| l.as_str().and_then(|s| s.parse().ok()))).filter(|l| *l > 0).ok_or("give the line (a number from 1)")? as u32;
        let body = text("text").ok_or("the comment needs text")?.to_owned();
        let author = self
            .registry
            .find_runtime(self.active.as_ref(), session, cx)
            .map(|runtime| runtime.read(cx).provider_kind().label().to_owned())
            .unwrap_or_else(|| crate::providers::THE_AGENT.to_owned());
        // Kept for Changes; the open below redraws it when the session is
        // on screen (off screen nothing needs a frame).
        self.right_pane.update(cx, |pane, _| {
            pane.cache.agent_comments.entry(root.to_path_buf()).or_default().entry(path.clone()).or_default().push(LineComment { line, text: body, author });
        });
        if self.session_in_front(cx).as_deref() == Some(session) {
            self.apply_show(session, root, Show::Changes { scope: Scope::All, path: Some(path.clone()) }, cx)?;
        }
        Ok(json!({"commented": format!("{path}:{line}"), "note": "shown under the file's diff in Changes"}))
    }
}

impl Harness {
    /// `changes_commit`: the commit box's flow, then — when asked — a push
    /// the person approves unless the session is in Full access (rule 29).
    /// A Read-only session commits nothing.
    fn changes_commit_call(&mut self, call: PaneCall, cx: &mut Context<Self>) {
        let message = call.params.get("message").and_then(Value::as_str).map(str::to_owned);
        let push = call.params.get("push").and_then(Value::as_bool).unwrap_or(false);
        let Some(runtime) = self.registry.find_runtime(self.active.as_ref(), &call.session, cx) else {
            return call.answer(Err("this session is not open in Baaz".into()));
        };
        let (mode, agent) = {
            let rt = runtime.read(cx);
            (rt.mode(), rt.provider_kind().label().to_owned())
        };
        if mode == aui_protocol::PermissionMode::DenyUnmatched {
            return call.answer(Err("this session is Read-only: it commits nothing".into()));
        }
        let in_front = self.session_in_front(cx).as_deref() == Some(call.session.as_str());
        let (done, committed) = futures::channel::oneshot::channel();
        let root = call.root.clone();
        self.with_right_pane(cx, |pane, cx| pane.commit_for_agent(&root, message, in_front, done, cx));
        cx.spawn(async move |this, cx| {
            let committed = committed.await.unwrap_or_else(|_| Err("the commit never answered".into()));
            let commit = match committed {
                Ok(commit) if push => commit,
                other => return call.answer(other),
            };
            let (tx, rx) = std::sync::mpsc::channel();
            let full_access = mode == aui_protocol::PermissionMode::AllowAll;
            let asked = this.update(cx, |this, cx| {
                if full_access {
                    this.with_right_pane(cx, |pane, cx| pane.push_for_agent(&call.root, tx, cx));
                } else {
                    let branch = commit["branch"].as_str().unwrap_or("this branch").to_owned();
                    this.ask_agent_push(&call.root, &branch, &agent, tx, cx);
                }
            });
            if asked.is_err() {
                return call.answer(Err("the window closed".into()));
            }
            std::thread::spawn(move || {
                let wait = if full_access { std::time::Duration::from_secs(120) } else { PUSH_WAIT };
                let mut answer = commit;
                match rx.recv_timeout(wait) {
                    Ok(Ok(pushed)) => answer["push"] = pushed,
                    Ok(Err(why)) => answer["push"] = json!({"error": why}),
                    Err(_) => answer["push"] = json!("waiting for the person to approve the push; it goes ahead if they do"),
                }
                call.answer(Ok(answer));
            });
        })
        .detach();
    }

    /// Put up Baaz's own approval for an agent's push.
    fn ask_agent_push(&mut self, root: &Path, branch: &str, agent: &str, done: std::sync::mpsc::Sender<Result<Value, String>>, cx: &mut Context<Self>) {
        let seq = self.right_pane.update(cx, |pane, _| {
            let seq = pane.agent_push.as_ref().map_or(1, |old| old.seq + 1);
            if let Some(old) = pane.agent_push.take() {
                let _ = old.done.send(Err("another push ask replaced this one; nothing was pushed".into()));
            }
            pane.agent_push = Some(AgentPush { seq, root: root.to_path_buf(), done });
            seq
        });
        self.set_dialog(cx, crate::overlays::Dialog {
            title: format!("Push {branch}?"),
            detail: format!("{agent} committed and asks to push {branch} to its remote. Others can see the branch once it is pushed."),
            kind: aui::overlay::DialogKind::Warning,
            primary: "Push",
            action: crate::overlays::DialogAction::AgentPush,
            archive_target: Some(seq.to_string()),
        });
    }

    /// The push confirm's Push: push now, through the pane.
    pub(crate) fn confirm_agent_push(&mut self, target: Option<&str>, cx: &mut Context<Self>) {
        let Some(seq) = target.and_then(|t| t.parse::<u64>().ok()) else { return };
        let ask = self.right_pane.update(cx, |pane, _| pane.agent_push.take_if(|ask| ask.seq == seq));
        if let Some(ask) = ask {
            self.with_right_pane(cx, |pane, cx| pane.push_for_agent(&ask.root, ask.done, cx));
        }
    }

    /// The push confirm dismissed (Cancel, Escape, the scrim): nothing is
    /// pushed, and a waiting agent hears so.
    pub(crate) fn decline_agent_push(&mut self, target: Option<&str>, cx: &mut Context<Self>) {
        let Some(seq) = target.and_then(|t| t.parse::<u64>().ok()) else { return };
        if let Some(ask) = self.right_pane.update(cx, |pane, _| pane.agent_push.take_if(|ask| ask.seq == seq)) {
            let _ = ask.done.send(Ok(json!({"pushed": false, "declined": "the person said no; the commit stays local"})));
        }
    }
}

/// A session's agent turns with their checkpoints, in order (blocking):
/// its uuid from baaz.db, its refs from the repository.
pub(crate) fn session_turns(root: &Path, provider: &str, id: &str, order: &[String]) -> Vec<TurnEnds> {
    let (p, i) = (provider.to_owned(), id.to_owned());
    let key = crate::db::read(crate::db::path(), move |db| crate::db::sessions::read_row(&db.conn, &p, &i).ok().flatten()).flatten();
    let refs = key.and_then(|key| crate::checkpoints::session_refs(root, &key)).unwrap_or_default();
    order
        .iter()
        .map(|turn_id| {
            let found = refs.get(&crate::checkpoints::component(turn_id));
            TurnEnds { turn_id: turn_id.clone(), pre: found.and_then(|r| r.pre.clone()), post: found.and_then(|r| r.post.clone()) }
        })
        .collect()
}

impl Harness {
    /// `pane-show:view=<v>[,path=<p>][,line=<n>][,scope=<s>]`: the open
    /// session's `pane_show`, through the bridge call's own handler (the
    /// arguments as a tool call would carry them). In a capture a scoped
    /// view loads the fixture checkpoint index. A refusal fails the step.
    #[cfg(feature = "probe")]
    pub(crate) fn step_pane_show(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let mut params = serde_json::Map::new();
        for pair in rest.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let (key, value) = pair.split_once('=').ok_or_else(|| format!("pane-show wants key=value pairs, got `{pair}`"))?;
            let value = match key {
                "line" => json!(value.parse::<u64>().map_err(|_| format!("line `{value}` is not a number"))?),
                _ => json!(value),
            };
            params.insert(key.to_owned(), value);
        }
        let session = self.session_in_front(cx).ok_or("no open session: pane_show acts for a session")?;
        let (root, _) = self.right_project(cx).ok_or("no project for the right pane")?;
        let answer = self.pane_show(&session, &root, &Value::Object(params), cx)?;
        if answer["shown"] == json!(false) {
            return Err(format!("pane_show did not show: {answer}"));
        }
        Ok(())
    }
}

impl Harness {
    /// `pane-comment:path=<p>,line=<n>,text=<words>`: the open session's
    /// `changes_comment` through the tool's own handler (the text may not
    /// hold a comma). A refusal fails the step.
    #[cfg(feature = "probe")]
    pub(crate) fn step_pane_comment(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let mut params = serde_json::Map::new();
        for pair in rest.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let (key, value) = pair.split_once('=').ok_or_else(|| format!("pane-comment wants key=value pairs, got `{pair}`"))?;
            params.insert(key.to_owned(), json!(value));
        }
        let session = self.session_in_front(cx).ok_or("no open session: changes_comment acts for a session")?;
        let (root, _) = self.right_project(cx).ok_or("no project for the right pane")?;
        self.changes_comment(&session, &root, &Value::Object(params), cx).map(|_| ())
    }
}

impl Harness {
    /// `pane-push-ask:<branch>`: the approval an agent's `changes_commit
    /// {push: true}` raises (docs/85 13.4), for a capture: nothing waits on
    /// it and its Push would push nothing (no ask behind the number).
    #[cfg(feature = "probe")]
    pub(crate) fn step_pane_push_ask(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let branch = rest.trim();
        if branch.is_empty() {
            return Err("pane-push-ask needs a branch name".into());
        }
        let (root, _) = self.right_project(cx).ok_or("no project for the right pane")?;
        let agent = self.active.as_ref().map(|view| view.read(cx).rt(cx).provider_kind().label().to_owned()).ok_or("no open session")?;
        let (tx, _rx) = std::sync::mpsc::channel();
        self.ask_agent_push(&root, branch, &agent, tx, cx);
        self.right_pane.update(cx, |pane, _| pane.agent_push = None);
        Ok(())
    }
}
