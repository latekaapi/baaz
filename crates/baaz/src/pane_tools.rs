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

use std::path::Path;

use baaz_core::pane_tools::{self as core, Scope, Show, TurnEnds};
use gpui::Context;
use serde_json::{json, Value};

use crate::app::Harness;
use crate::layout::RightKind;
use crate::right_scopes::ChangesScope;
use crate::terminal::service::PaneCall;

impl Harness {
    /// The service's hook: one pane tool call (see the module doc).
    pub(crate) fn pane_tool_call(&mut self, call: PaneCall, cx: &mut Context<Self>) {
        match call.tool.as_str() {
            "pane_show" => {
                let outcome = self.pane_show(&call.session, &call.root, &call.params, cx);
                call.answer(outcome);
            }
            "changes_read" => self.changes_read_call(call, cx),
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
        cx.notify();
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
