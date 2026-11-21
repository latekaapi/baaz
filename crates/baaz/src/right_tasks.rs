//! The right pane's Tasks screen (docs/82 6.5, docs/83 §6): what the open
//! session's agent started through Baaz's terminal tools, and the session's
//! Run (docs/82 7.4) — command, state, port, duration — newest first, with Stop on what still runs. A row with a
//! tab shows the terminal on it, where the terminal sits (docs/82 8.1).
//!
//! The rows come from the terminal host's task book
//! ([`crate::terminal::tasks`]); the pane copies them into its cache on each
//! render ([`RightPane::sync_tasks`]) and this module only draws that copy.

use aui::interact::{row_list, LIST_INSET};
use aui::util::ScrollAxisLock as _;
use aui::util::{h_flex, v_flex};
use aui_tokens::{scale, ActiveAui, AuiStyled, TextRole};
use gpui::{div, prelude::*, px, AnyElement, Context, IntoElement, SharedString};

use crate::app::Harness;
use crate::right_pane::{RightPane, RightPaneEvent};
use crate::terminal::tasks::{duration_label, TaskRow, TaskState};

/// The empty screen's one line (decision 4, rule 14: no provider named).
pub(crate) const EMPTY_LINE: &str = "Nothing running. Commands the agent starts in Baaz's terminal, and the project's Run, show here.";

impl RightPane {
    /// Copy the open session's tasks out of the host for this frame (and
    /// tell the book someone is looking, so running durations tick).
    pub(crate) fn sync_tasks(&mut self, cx: &mut Context<Self>) {
        let session = self.facts.session.as_ref().map(|view| view.read(cx).rt(cx).session_id.clone());
        self.cache.tasks = match session {
            Some(id) => self.host.read(cx).task_rows(&id),
            None => Vec::new(),
        };
    }

    /// Whether the tab row carries Tasks: while it shows, or while the open
    /// session has any task listed (docs/83 §6: each open view is a tab).
    pub(crate) fn tasks_tab_shown(&self, shown: crate::layout::RightKind, cx: &gpui::App) -> bool {
        if shown == crate::layout::RightKind::Tasks {
            return true;
        }
        let Some(view) = self.facts.session.as_ref() else { return false };
        self.host.read(cx).tasks.has_tasks(&view.read(cx).rt(cx).session_id)
    }

    /// The row's Stop: end task `id`'s process group. A refusal (it ended
    /// meanwhile, or nothing of its own runs in the tab) says why in a toast.
    pub(crate) fn stop_task(&mut self, id: u64, cx: &mut Context<Self>) {
        let outcome = self.host.update(cx, |host, cx| host.stop_task(id, cx));
        if let Err(why) = outcome {
            self.toast("Couldn't stop it", why, cx);
        }
    }
}

impl Harness {
    /// Open `session_id` (unless it is open already) with the right pane on
    /// its Tasks: the sidebar row's "N running", and its probe verb.
    ///
    /// The session's stored pane state says Tasks first: a switch restores
    /// the pane from it when the session lands, which for a session that
    /// opens later (a provider reopen) is after this returns.
    pub(crate) fn open_session_tasks(&mut self, session_id: String, window: &mut gpui::Window, cx: &mut Context<Self>) {
        if self.active_id(cx).as_deref() != Some(session_id.as_str()) {
            let mut state = self.session_list.read(cx).overrides.get(&session_id).and_then(|meta| meta.right.clone()).unwrap_or_default();
            state.open = true;
            state.kind = crate::layout::RightKind::Tasks;
            self.store_right_state_cheap(&session_id, state, cx);
            self.resume_quiet(session_id, window, cx);
        }
        self.select_right(crate::layout::RightKind::Tasks, cx);
    }
}

impl Harness {
    #[cfg(feature = "probe")]
    /// `tasks-fixture[:<session id>]`: the capture's four tasks (running on
    /// a port, exited 0, exited 1, stopped) on that session, else the open
    /// one, with no process behind any of them. No session named or open
    /// is an `Err`; so is a session that already has the fixture.
    pub(crate) fn step_tasks_fixture(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let named = rest.trim();
        let session = if named.is_empty() { self.active_id(cx).ok_or("no session is open and none was named")? } else { named.to_owned() };
        let host = self.terminal_host(cx);
        if host.read(cx).tasks.has_tasks(&session) {
            return Err(format!("session {session} already has tasks"));
        }
        host.update(cx, |host, cx| {
            host.tasks.fixture(&session);
            host.ensure_task_poll(cx);
        });
        Ok(())
    }

    #[cfg(feature = "probe")]
    /// `tasks-row:<session id>`: the sidebar row's "N running" click — the
    /// session opens with the pane on its Tasks. A session with nothing
    /// running has no such button, so that is an `Err`.
    pub(crate) fn step_tasks_row(&mut self, rest: &str, window: &mut gpui::Window, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let session = rest.trim();
        if session.is_empty() {
            return Err("tasks-row needs a session id".into());
        }
        let running = self.terminal_host(cx).read(cx).tasks.running_counts().into_iter().any(|(id, n)| id == session && n > 0);
        if !running {
            return Err(format!("session {session} has nothing running"));
        }
        self.open_session_tasks(session.to_owned(), window, cx);
        Ok(())
    }
}

/// The Tasks screen's body: the quiet empty line, or the rows.
pub(crate) fn tasks_body(rows: &[TaskRow], cx: &mut Context<RightPane>) -> AnyElement {
    let p = cx.aui().colors;
    if rows.is_empty() {
        return div()
            .id("right-tasks-empty")
            .size_full()
            .bg(p.surface_1)
            .p(px(scale::SP_4))
            .role(gpui::Role::Status)
            .aria_label(EMPTY_LINE)
            .text_role(TextRole::Body)
            .text_color(p.fg_muted)
            .child(EMPTY_LINE)
            .into_any_element();
    }
    let tabs: Vec<Option<String>> = rows.iter().map(|row| row.tab.clone()).collect();
    let open = cx.listener(move |_: &mut RightPane, index: &usize, _, cx| {
        if let Some(Some(tab)) = tabs.get(*index) {
            cx.emit(RightPaneEvent::OpenTerminalTab(tab.clone()));
        }
    });
    let mut list = row_list("right-tasks-list").label("Tasks").on_activate(move |index, window, cx| open(&index, window, cx));
    for (index, row) in rows.iter().enumerate() {
        list = list.item(index, task_row(row, cx));
    }
    div()
        .id("right-tasks-scroll")
        .size_full()
        .bg(p.surface_1)
        .overflow_y_scroll()
        .scroll_axis_locked()
        .pt(px(scale::SP_2))
        .px(px(LIST_INSET))
        .child(list)
        .into_any_element()
}

/// One row (docs/83 §6, rows 32): the command (one line, truncated, whole
/// on hover), its state, its port, its duration, and Stop while it runs.
fn task_row(row: &TaskRow, cx: &mut Context<RightPane>) -> AnyElement {
    let p = cx.aui().colors;
    let metrics = aui_tokens::Metrics::active();
    let state_ink = match row.state {
        TaskState::Exited(code) if code != 0 => p.danger,
        TaskState::Running => p.fg,
        _ => p.fg_muted,
    };
    let command: SharedString = row.command.clone().into();
    let label: SharedString = match &row.tab {
        Some(_) => format!("{} · {} · open its terminal", row.command, row.state.label()).into(),
        None => format!("{} · {}", row.command, row.state.label()).into(),
    };
    let mut line = h_flex()
        .id(SharedString::from(format!("right-task-{}", row.id)))
        .w_full()
        .h(metrics.control)
        .px(px(scale::SP_2))
        .gap(px(scale::SP_3))
        .items_center()
        .role(gpui::Role::ListItem)
        .aria_label(label)
        .child(
            div()
                .id(SharedString::from(format!("right-task-{}-command", row.id)))
                .flex_1()
                .min_w(px(0.0))
                .truncate()
                .mono(TextRole::Mono)
                .text_color(p.fg)
                .tooltip(aui::data::tip(command.clone()))
                .child(command.clone()),
        )
        .child(div().flex_none().text_role(TextRole::Caption).text_color(state_ink).child(row.state.label()));
    if let Some(port) = row.port {
        line = line.child(div().flex_none().mono(TextRole::Caption).text_color(p.fg_muted).child(format!(":{port}")));
    }
    line = line.child(div().flex_none().mono(TextRole::Caption).text_color(p.fg_muted).child(duration_label(row.duration)));
    if row.state.running() {
        let id = row.id;
        let stop = cx.listener(move |this: &mut RightPane, _: &gpui::ClickEvent, _, cx| {
            cx.stop_propagation();
            this.stop_task(id, cx);
        });
        line = line.child(
            div()
                .flex_none()
                // The press stays off the row: Stop never also opens the tab.
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    aui::data::button(SharedString::from(format!("right-task-{id}-stop")), "Stop")
                        .ghost()
                        .xs()
                        .accessibility_label(format!("Stop {}", row.command))
                        .on_click(stop),
                ),
        );
    }
    v_flex().w_full().child(line).into_any_element()
}
