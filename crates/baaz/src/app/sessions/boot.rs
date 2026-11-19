//! The boot session: `--session <id>` (or `latest`) resumes one session as
//! the window opens, and a scripted run (`--send`, `--steps`, `probe` builds
//! only) gets a session to act on.
//!
//! Part of [`Harness`]; see [`crate::app`] for what the entity owns.

use super::*;

/// What [`Harness::ensure_boot_session`] should do about the boot session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BootDecision {
    /// Open the boot session now: `--session <id>` resumes and anything
    /// else starts, all without waiting for `session/list`.
    Open,
    /// `--session latest` cannot resolve until the list lands: wait for it.
    WaitForList,
    /// Nothing to do: a session is open or on its way, the boot was already
    /// attempted, the wire is down, or nothing scripted wants a session.
    Idle,
}

/// The inputs [`boot_decision`] reads, bundled so the decision stays a plain
/// function of named state.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BootState<'a> {
    /// A session view is already open.
    pub active: bool,
    /// A `session/start` round-trip is still in flight.
    pub switch_pending: bool,
    /// The boot session was already attempted once.
    pub attempted: bool,
    /// The wire child exists.
    pub connected: bool,
    /// The `--session` argument, if given.
    pub session_arg: Option<&'a str>,
    /// A `--send` turn is waiting for a session.
    pub send: bool,
    /// A `--steps` list is waiting for a session.
    pub steps_pending: bool,
    /// That list starts its own session (`new` up front): booting one
    /// eagerly ahead of it opened two sessions for one action.
    pub steps_begin_with_new: bool,
    /// The session list has landed at least once.
    pub sessions_loaded: bool,
}

/// The boot-session decision, in pure form: at most one attempt, only on a
/// live wire, and `latest` alone waits for the list — everything else the
/// wire and a project can already answer. A `--steps` list headed by `new`
/// starts its own session, so booting one ahead of it is never wanted:
/// boot plus the head's `new` opened two sessions (two `provider lane open`
/// lines, two sidebar rows) for one action.
pub(crate) fn boot_decision(state: BootState<'_>) -> BootDecision {
    if state.active || state.switch_pending || state.attempted {
        return BootDecision::Idle;
    }
    if !state.connected {
        return BootDecision::Idle;
    }
    if state.session_arg == Some("latest") && !state.sessions_loaded {
        return BootDecision::WaitForList;
    }
    if state.session_arg.is_none() && !state.send && !state.steps_pending {
        return BootDecision::Idle;
    }
    if state.session_arg.is_none() && state.steps_begin_with_new {
        return BootDecision::Idle;
    }
    BootDecision::Open
}

impl Harness {
    /// Open the boot session without waiting for `session/list`: a scripted
    /// run's new session needs only the wire and a project, and an explicit
    /// `--session <id>` resumes without the list too. Only `--session
    /// latest` waits for it. Runs at most once per boot: the named session
    /// is consumed, and the unnamed path is guarded by the in-flight switch.
    pub(in crate::app) fn ensure_boot_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match boot_decision(BootState {
            active: self.active.is_some(),
            switch_pending: self.open_state.switch_pending,
            attempted: self.open_state.boot_session_attempted,
            connected: self.providers.shared_ready(),
            session_arg: self.args.session.as_deref(),
            send: self.args.send.is_some(),
            steps_pending: self.boot_steps_pending(),
            steps_begin_with_new: self.boot_steps_begin_with_new(),
            sessions_loaded: self.session_list.read(cx).sessions_loaded,
        }) {
            BootDecision::Idle | BootDecision::WaitForList => return,
            BootDecision::Open => {}
        }
        self.open_state.boot_session_attempted = true;
        self.open_boot_session(window, cx);
    }

    /// `--session <id>` (or `latest`): open one session at boot, once the list
    /// has arrived. Consumed, so a later refresh does not re-open it.
    pub(in crate::app) fn open_boot_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(wanted) = self.args.session.take() else {
            // A scripted turn or a scripted capture with no session named needs
            // somewhere to go — unless one is already on its way: the list
            // reply calls this too, and must not start a second session
            // behind the one [`Self::ensure_boot_session`] issued.
            if (self.args.send.is_some() || self.boot_steps_pending())
                && self.active.is_none()
                && !self.open_state.switch_pending
            {
                self.new_session(window, cx);
            }
            return;
        };
        let id = if wanted == "latest" {
            // The list arrives `updatedAt` descending, and the sidebar sorts on
            // the same field, so the newest row is the head of the grouping.
            self.session_list.read(cx).sessions.iter().filter(|e| !e.hidden).max_by_key(|e| e.updated).map(|e| e.id.clone())
        } else {
            Some(wanted)
        };
        if let Some(id) = id {
            self.resume(id, window, cx);
        }
    }

    /// Whether a `--steps` list waits for a session (never without the
    /// `probe` feature, which has no `--steps`).
    fn boot_steps_pending(&self) -> bool {
        #[cfg(feature = "probe")]
        return crate::steps::steps_need_a_session(&self.args.steps);
        #[cfg(not(feature = "probe"))]
        false
    }

    /// Whether that list starts its own session with `new` up front.
    fn boot_steps_begin_with_new(&self) -> bool {
        #[cfg(feature = "probe")]
        return crate::steps::steps_begin_with_new(&self.args.steps);
        #[cfg(not(feature = "probe"))]
        false
    }
}
