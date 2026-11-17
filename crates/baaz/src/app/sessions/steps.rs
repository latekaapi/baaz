//! The scripted run's glue (`--session`, `--send`, `--steps`): the boot
//! session, the readiness predicate the step runner waits on, and the
//! `step_*` capture verbs that act on the session list and the window.
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

/// The `--steps` readiness decision, in pure form: a live run needs the wire
/// and an open session; a `--replay` or `--no-connect` run has no wire, so
/// an open session alone is enough. Never the session list (see
/// [`Harness::steps_ready`]).
///
/// `window_only` covers two cases: a script that only touches the window,
/// and a mixed script whose head runs without a session (see
/// [`crate::steps::head_runs_without_session`]). Offline with `--login
/// signed-in` never opens a boot session, so requiring a session before the
/// head runs meant `new;effort` could never start: the `new` that would have
/// created the session never ran, and the capture drained an unrun list as
/// `ran=2 failed=0` over the empty state.
pub(crate) fn steps_ready_for(
    steps_pending: bool,
    replay: bool,
    offline: bool,
    connected: bool,
    session_open: bool,
    window_only: bool,
    switch_pending: bool,
) -> bool {
    if !steps_pending {
        return false;
    }
    // A script that only touches the window needs no session. Offline with
    // `--login signed-in` never opens one, so requiring a session there meant
    // every window verb — the right pane, the menus, the sidebar flags —
    // silently never ran: the capture came out clean and the run still exited
    // 0. That is how five right-pane entries were written, gated green, and
    // produced five byte-identical screenshots of a shell with no pane in it.
    // The mixed-script head folds in here through the caller's `window_only`
    // for the same reason: holding `new;effort` for a session its own head
    // would create deadlocks the script before it starts.
    if window_only {
        return true;
    }
    // A provider lane opens on a background round-trip exactly like
    // `session/start` does, and session verbs must wait for it the same
    // way: `new;setprovider:codex;send:` failed with `no open session`
    // because `send` ran before the lane's open finished. Window verbs
    // never touch the session, so they still run — the capture's bound,
    // not this gate, is what releases a switch that never lands.
    if switch_pending {
        return false;
    }
    if replay || offline {
        return session_open;
    }
    connected && session_open
}

/// Take a scripted list out of its holder, so a later pass finds nothing to
/// do: the drain-once rule behind `--steps` and `--login-steps`. Both
/// runners take through here, so the second caller — a later frame, a later
/// activation — always sees an empty list.
pub(crate) fn drain_steps(steps: &mut Vec<String>) -> Vec<String> {
    std::mem::take(steps)
}

/// The `<dy,finger_ticks,tail_ticks>` payload of the two scroll-sweep
/// steps: an absent or empty part takes its default (-20, 60, 40), a
/// present one that does not parse is the step's failure.
fn parse_scroll_sweep(rest: &str) -> Result<(f32, u32, u32), String> {
    let mut parts = rest.split(',').map(str::trim);
    let mut next = || parts.next().filter(|part| !part.is_empty());
    let dy = next().map(|text| text.parse::<f32>().map_err(|_| format!("`{text}` is not a wheel delta"))).transpose()?;
    let finger = next().map(|text| text.parse::<u32>().map_err(|_| format!("`{text}` is not a tick count"))).transpose()?;
    let tail = next().map(|text| text.parse::<u32>().map_err(|_| format!("`{text}` is not a tick count"))).transpose()?;
    Ok((dy.unwrap_or(-20.0), finger.unwrap_or(60), tail.unwrap_or(40)))
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
            connected: self.muse_slot.is_ready(),
            session_arg: self.args.session.as_deref(),
            send: self.args.send.is_some(),
            steps_pending: crate::steps::steps_need_a_session(&self.args.steps),
            steps_begin_with_new: crate::steps::steps_begin_with_new(&self.args.steps),
            sessions_loaded: self.sessions_loaded,
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
            if (self.args.send.is_some() || crate::steps::steps_need_a_session(&self.args.steps))
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
            self.sessions.iter().filter(|e| !e.hidden).max_by_key(|e| e.updated).map(|e| e.id.clone())
        } else {
            Some(wanted)
        };
        if let Some(id) = id {
            self.resume(id, window, cx);
        }
    }

    /// `--send <text>`: one scripted turn, once a session is open. The hook a
    /// screenshot of a live turn needs; it goes through the same `send` a key
    /// press does, never straight into the fold.
    pub(in crate::app) fn send_scripted(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = self.args.send.take() else { return };
        let Some(view) = self.active.clone() else { return };
        view.update(cx, |view, cx| {
            view.set_draft(text, window, cx);
            view.send(window, cx);
        });
    }

    /// Whether a scripted `--steps` list may run now: there is one left, and
    /// the app can execute it. A live run needs the wire and an open
    /// session; a `--replay` or `--no-connect` run has no wire, so an open
    /// session alone is enough.
    ///
    /// Deliberately not the session list: no verb needs it — the boot
    /// session opens without it ([`Self::ensure_boot_session`]), and only
    /// `--session latest` waits for it, via no open session yet. Gating the
    /// script on the list hung every scripted run behind a `session/list`
    /// that never answered, while the capture mistook the stalled boot for
    /// a finished script.
    pub(crate) fn steps_ready(&self) -> bool {
        steps_ready_for(
            !self.args.steps.is_empty(),
            self.args.replay.is_some(),
            self.args.offline,
            self.muse_slot.is_ready(),
            self.active.is_some(),
            // A mixed script headed by a window verb can start: `new`
            // opens the session the later verbs need, so holding the whole
            // list for a session the head would create deadlocks
            // `new;effort` offline into "never became ready" (see
            // [`crate::steps::head_runs_without_session`]).
            crate::steps::all_window_steps(&self.args.steps)
                || crate::steps::head_runs_without_session(&self.args.steps),
            // An in-flight open — `session/start` or a provider lane —
            // holds session verbs until it lands; window verbs run on.
            self.session_opening(),
        )
    }

    /// Whether a session the scripted verbs should wait for is still
    /// opening: a switch in flight, or a new muse session whose view is up
    /// while its `session/start` is still on the wire.
    pub(crate) fn session_opening(&self) -> bool {
        self.open_state.session_opening()
    }

    /// `--steps`: drive the open session from the command line so a screenshot
    /// is reproducible. Runs once, when [`Self::steps_ready`] holds: the
    /// drain inside ([`Harness::take_steps`]) consumes the list, so a second
    /// call — a later frame, a later activation — finds nothing to do.
    /// The verbs, and the loop that runs them, are [`crate::steps`].
    ///
    /// Called every frame ([`Harness::on_frame`]) and after the replay fold
    /// ([`Harness::open_replay`]): those are the two transitions that can
    /// make the predicate true, and the take makes the double call harmless.
    /// Session activations deliberately do not call this: `activate` fires
    /// on every swap, including swaps the script itself causes, so draining
    /// there raced the boot it was meant to follow.
    pub(in crate::app) fn maybe_run_steps(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let _ = window;
        if !self.steps_ready() {
            return;
        }
        self.capture.set_steps_ready(true);
        crate::steps::run_steps(self, cx);
    }

    /// `--steps`, taken out of the arguments so a later refresh does not
    /// replay them. Drains through [`drain_steps`]: the first caller gets
    /// the script, every later caller gets nothing.
    pub(crate) fn take_steps(&mut self) -> Vec<String> {
        drain_steps(&mut self.args.steps)
    }

    // One handler per `--steps` verb that belongs to the window rather than to
    // a session and needs more than a single existing call. The verb table
    // that reaches them is [`crate::steps`].

    /// `search:<query>`: open the search palette, optionally on a query.
    pub(crate) fn step_search(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> crate::steps::StepResult {
        self.open_search(window, cx);
        if !rest.is_empty() {
            self.search_query.update(cx, |state, cx| {
                state.set_value(rest.to_owned(), window, cx);
                // `set_value` raises no `InputEvent::Change`; typing does.
                // The step raises it on the field, so the query runs through
                // the same subscription a keystroke does — calling
                // `refresh_search` by hand here is how a dead subscription
                // once passed every scripted check (docs/81 A F2).
                cx.emit(gpui_kit::base::input::InputEvent::Change);
            });
        }
        Ok(())
    }

    /// `rename:<name>`: open the active row's inline field, optionally
    /// filled. No open session fails the step.
    pub(crate) fn step_rename(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let session_id = self.active.as_ref().map(|a| a.read(cx).session_id.clone()).ok_or("no open session")?;
        self.start_rename(session_id, window, cx);
        if !rest.is_empty() {
            self.rename.update(cx, |state, cx| state.set_value(rest.to_owned(), window, cx));
        }
        Ok(())
    }

    /// `hidden`: list hidden sessions anyway (a toggle).
    pub(crate) fn step_toggle_hidden(&mut self, cx: &mut Context<Self>) -> crate::steps::StepResult {
        self.show_hidden = !self.show_hidden;
        self.invalidate_list();
        cx.notify();
        Ok(())
    }

    /// `empty`: list sessions with no turns anyway (a toggle).
    pub(crate) fn step_toggle_empty(&mut self, cx: &mut Context<Self>) -> crate::steps::StepResult {
        self.show_empty = !self.show_empty;
        self.invalidate_list();
        cx.notify();
        Ok(())
    }

    /// `show-archived`: list archived sessions anyway (a toggle).
    pub(crate) fn step_toggle_archived(&mut self, cx: &mut Context<Self>) -> crate::steps::StepResult {
        self.show_archived = !self.show_archived;
        self.invalidate_list();
        cx.notify();
        Ok(())
    }

    /// `wheel:<dy>[@<x>,<y>]`: dispatch one synthetic wheel event at the
    /// window centre (or at window point `x,y`, e.g. over the right pane)
    /// and log the transcript's pixel offset before and after — the
    /// palette-scroll instrument: over the open palette the
    /// palette's list scrolls and the transcript never moves, while over the
    /// bare transcript the same wheels move it. An unparseable `dy` or
    /// point fails the step. Free: no turn, no wire.
    pub(crate) fn step_wheel(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let (dy, at) = rest.split_once('@').unwrap_or((rest, ""));
        let dy: f32 = dy.trim().parse().map_err(|_| format!("`{dy}` is not a wheel delta"))?;
        let before = self.active.as_ref().map(|view| view.read(cx).bench_list_px()).unwrap_or(0.0);
        let size = window.bounds().size;
        let position = if at.trim().is_empty() {
            point(size.width * 0.5, size.height * 0.5)
        } else {
            at.split_once(',')
                .and_then(|(x, y)| Some(point(px(x.trim().parse().ok()?), px(y.trim().parse().ok()?))))
                .ok_or_else(|| format!("`{at}` is not a window point x,y"))?
        };
        window.dispatch_event(
            PlatformInput::Scroll(ScrollEvent {
                position,
                delta: ScrollDelta::Pixels(point(px(0.0), px(dy))),
                ..Default::default()
            }),
            cx,
        );
        // The handler only accumulates, so without this
        // the read below would always equal `before` and the log could no
        // longer tell a moved transcript from an occluded one. Apply the
        // frame's drain eagerly — with nothing pending it is gpui's own
        // early-return, so over the palette this still logs X->X.
        if let Some(view) = self.active.clone() {
            view.update(cx, |view, _| view.drain_pending_wheel());
        }
        let after = self.active.as_ref().map(|view| view.read(cx).bench_list_px()).unwrap_or(0.0);
        crate::baaz_log!("wheel dy={dy} list_px={before}->{after}");
        Ok(())
    }

    /// `centre`: the open-flicker instrument. Log
    /// `baaz: centre hero=<hero> loading=<loading>`: the new-session
    /// hero vs loading-row paints since the last call. An existing session
    /// opened through `open:`/`click:` must read `hero=0`; a draft opened
    /// through `new` still reads `hero>0`. Free: no turn, no wire.
    pub(crate) fn step_centre(&mut self) -> crate::steps::StepResult {
        let (hero, loading) = crate::session::take_centre_paints();
        crate::baaz_log!("centre hero={hero} loading={loading}");
        Ok(())
    }

    /// `sidebar-wheel:<dy>[,n]`: the sidebar-scroll instrument. Dispatch
    /// n synthetic wheel events (default 1) at a sidebar
    /// point — x = 100 sits in the sessions list at the default width, y =
    /// 40 % of the window height — synchronously, so one step lands between
    /// two frames: the burst shape a trackpad really delivers, which
    /// `--steps wheel:` (window centre, transcript) can never hit. Logs the
    /// sidebar offset before/after plus the pane and root renders drained
    /// since the last call — pair with `wait:<ms>` and a trailing
    /// `sidebar-wheel:0,0` to read a burst's renders after its frames paint
    /// (renders land on frames, not inside the dispatch). Free: no turn, no
    /// wire.
    /// `pointer:<x>,<y>`: dispatch one mouse move to window point (x, y) —
    /// the pointer resting on a row, for captures of hover under a scroll
    /// (`sidebar-scroll-sweep:`). An unparseable point fails the step.
    /// Free: no turn, no wire.
    pub(crate) fn step_pointer(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let (x, y) = rest
            .split_once(',')
            .and_then(|(x, y)| Some((x.trim().parse::<f32>().ok()?, y.trim().parse::<f32>().ok()?)))
            .ok_or_else(|| format!("`{rest}` is not a window point x,y"))?;
        // Dispatched after this update: the list's hover handler reports to
        // Harness, which is mid-update here.
        window.defer(cx, move |window, cx| {
            window.dispatch_event(PlatformInput::MouseMove(gpui::MouseMoveEvent { position: point(px(x), px(y)), ..Default::default() }), cx);
        });
        Ok(())
    }

    pub(crate) fn step_sidebar_wheel(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let (dy_text, n_text) = rest.split_once(',').unwrap_or((rest, "1"));
        let dy: f32 = dy_text.trim().parse().map_err(|_| format!("`{dy_text}` is not a wheel delta"))?;
        let n: usize = n_text.trim().parse().map_err(|_| format!("`{n_text}` is not a wheel count"))?;
        // Drained first: what this line reports is everything since the
        // previous drain, i.e. the earlier burst's frames once a `wait:`
        // separates the two calls.
        let pane = crate::sidebar_view::take_sidebar_pane_renders();
        let root = crate::sidebar_view::take_baaz_root_renders();
        let drains = crate::sidebar_view::take_sidebar_wheel_drains();
        let before = self.sidebar_list_top();
        let size = window.bounds().size;
        let at = point(px(100.0), size.height * 0.4);
        // Dispatched after this update returns: the sidebar's own wheel
        // handler updates `Harness`, and dispatching from inside the
        // step's update re-entered it (a double-lease abort, found when a
        // seeded sidebar was tall enough to take the wheel).
        let handle = window.window_handle();
        let this = cx.entity().downgrade();
        cx.defer(move |cx| {
            let _ = handle.update(cx, |_, window, cx| {
                for _ in 0..n {
                    window.dispatch_event(
                        PlatformInput::Scroll(ScrollEvent {
                            position: at,
                            delta: ScrollDelta::Pixels(point(px(0.0), px(dy))),
                            ..Default::default()
                        }),
                        cx,
                    );
                }
            });
            let _ = this.update(cx, |this, _| {
                let after = this.sidebar_list_top();
                // The flattened row count (heads + folds + sessions): `item_ix`
                // walks the flattened model, so a probe comparing it against
                // `rows=` needs the same count.
                let rows = this.prev_sidebar_rows.len();
                let centre = crate::session::take_centre_renders();
                crate::baaz_log!(
                    "sbwheel dy={dy} n={n} sidebar_ix={bix}+{boff:.1}->{aix}+{aoff:.1} rows={rows} pane={pane} root={root} centre={centre} drains={drains}",
                    bix = before.item_ix,
                    boff = f32::from(before.offset_in_item),
                    aix = after.item_ix,
                    aoff = f32::from(after.offset_in_item),
                );
            });
        });
        Ok(())
    }

    /// `resize-begin:<x>` / `resize-move:<x>` / `resize-end`: the scripted
    /// resize drag. They drive the same [`Harness`] handlers
    /// the divider strip calls — `begin_resize` / `drag_resize` /
    /// `end_resize` — so an overlap probe (a press with an outside open
    /// still armed, moves with frames interleaved) exercises the real press
    /// disarm and the in-flight install gate. Each logs `baaz: rsdrag`
    /// with the width, the sidebar list index, the reveal arm, the scrolled
    /// flag and the drag: a probe the drag must not move keeps every
    /// line's `sidebar_ix` equal.
    /// `resize-sweep:<to_w,step_px>`: march the divider toward `to_w` one
    /// `step_px` per rendered frame (scripting only) — the
    /// display link's pace on a real display — logging
    /// `baaz: rssweep w=<width> pane=<pane> root=<root> rehint=<0/1>` per
    /// tick. Like a press it disarms the reveal and owns the list while it
    /// runs; it never persists. An unparseable width or step fails the
    /// step; `step_px` defaults to 4 when absent.
    pub(crate) fn step_resize_sweep(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let (to, step) = rest.split_once(',').unwrap_or((rest, "4"));
        let to: f32 = to.trim().parse().map_err(|_| format!("`{to}` is not a width"))?;
        let step: f32 = step.trim().parse().map_err(|_| format!("`{step}` is not a step in px"))?;
        self.reveal = None;
        self.reveal_unknown = None;
        self.sidebar_user_scrolled = true;
        self.resize.sweep = Some(crate::resize::ResizeSweep::new(to, step));
        self.resize.active = true;
        self.resize.scripted = true;
        cx.notify();
        Ok(())
    }

    /// `sidebar-scroll-sweep:<dy,finger_ticks,tail_ticks>`: a frame-paced
    /// sidebar wheel gesture — `dy` held for
    /// `finger_ticks` rendered frames, then decaying to 5% of `dy` over
    /// `tail_ticks` more, one push per tick via
    /// [`Self::push_sidebar_scroll_sweep`]. The
    /// in-process fallback for a real `CGEvent` gesture: where the session
    /// cannot post or confirm a real event (no activatable app, no readable
    /// screencapture), a posted event's landing window cannot be confirmed
    /// (see `docs/02-app.md`). Pair with
    /// `BAAZ_FRAME_TRACE=1` and read `scripts/frame-trace.py --metric
    /// sidebar`; free, no turn, no wire. An absent part takes its
    /// default (-20, 60, 40); a present one that does not parse fails the
    /// step.
    pub(crate) fn step_sidebar_scroll_sweep(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let (dy, finger, tail) = parse_scroll_sweep(rest)?;
        self.sidebar_scroll_sweep = Some(crate::resize::ScrollSweep::new(dy, finger, tail));
        cx.notify();
        Ok(())
    }

    /// `transcript-scroll-sweep:<dy,finger_ticks,tail_ticks>`: the
    /// transcript's twin of `sidebar-scroll-sweep:`, pushing into the
    /// active session's own wheel accumulator
    /// (`SessionView::push_wheel`) once per rendered frame. Same shape,
    /// same fallback reason, same defaults and failures, same
    /// free/no-turn/no-wire guarantee.
    pub(crate) fn step_transcript_scroll_sweep(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let (dy, finger, tail) = parse_scroll_sweep(rest)?;
        self.transcript_scroll_sweep = Some(crate::resize::ScrollSweep::new(dy, finger, tail));
        cx.notify();
        Ok(())
    }

    /// See the `resize-begin` doc above: `begin` and `move` fail the step
    /// on an unparseable `x`; `end` ignores its payload.
    pub(crate) fn step_resize_drag(&mut self, phase: &str, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let x: f32 = if phase == "begin" || phase == "move" {
            rest.trim().parse().map_err(|_| format!("`{rest}` is not an x position"))?
        } else {
            0.0
        };
        match phase {
            "begin" => {
                self.begin_resize(x, cx);
                // No pointer behind a scripted drag, so `on_frame`'s
                // release-outside-the-window rule must not end it between
                // steps (a headless window is never active).
                self.resize.scripted = true;
            }
            "move" => self.drag_resize(x, cx),
            _ => self.end_resize(cx),
        }
        // A probe the drag must not move keeps every line's index equal;
        // the list walks down as `item_ix` grows.
        let top = self.sidebar_list_top();
        crate::baaz_log!(
            "rsdrag {phase} w={} sidebar_ix={}+{:.1} reveal={:?} scrolled={} ract={}",
            self.resize.width,
            top.item_ix,
            f32::from(top.offset_in_item),
            self.reveal,
            self.sidebar_user_scrolled,
            self.resize.active
        );
        Ok(())
    }

    /// `sidebar-width:<px>`: a scripted width for the resize screenshots,
    /// clamped and settled exactly like a released drag, minus the pointer.
    /// An unparseable width fails the step.
    pub(crate) fn step_sidebar_width(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let width = rest.trim().parse::<f32>().map_err(|_| format!("`{rest}` is not a width"))?;
        self.resize.width = clamp_sidebar_width(width);
        self.resize.active = false;
        self.resize.persist(cx);
        cx.notify();
        Ok(())
    }

    /// `row-detail:<session_id>`: capture aid — pin the hover card open for
    /// one row, seated at the selected row's bounds (pair with `click:` on
    /// the same id). Empty clears the pin; an id that names no sidebar row
    /// fails the step. Free: no pointer, no turn.
    pub(crate) fn step_row_detail(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let id = rest.trim();
        if !id.is_empty() && !self.sessions.iter().any(|entry| entry.id == id) {
            return Err(format!("no sidebar row `{id}`"));
        }
        self.forced_detail = if id.is_empty() { None } else { Some(id.to_owned()) };
        cx.notify();
        Ok(())
    }

    /// `hover:<session_id>`: capture aid — deliver the selected row's own
    /// hover report, exactly what the row's hover event sends: arms the
    /// card's delay and seats it from the row's bounds, the same side seat
    /// as a real pointer (pair with `click:` on the same id and a `wait:`
    /// past the delay; empty means the selected row). A real mouse-move
    /// event cannot be dispatched from a step — steps run inside a `Harness`
    /// update, and the row's hover handler updates the entity, which panics
    /// re-entrantly (the wheel steps survive it only because their handlers
    /// never touch the entity) — so this makes the report's own call. The
    /// pointer-to-report half lives in the
    /// `hover_report_seats_the_card_without_a_pane_render` test, which
    /// sweeps a real pointer over the real pane. No selected row bounds
    /// yet, or an id that is not the selected row, fails the step. Free: no
    /// turn, no wire.
    pub(crate) fn step_hover(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let id = rest.trim();
        let (known, bounds) = self
            .selected_row_bounds
            .clone()
            .ok_or("no selected row bounds yet (pair with `click:` and a `wait:`)")?;
        if !id.is_empty() && known != id {
            return Err(format!("`{id}` is not the selected row (`{known}`); pair with `click:{id}` first"));
        }
        self.note_row_hover(known.clone(), true, Some(bounds), cx);
        match crate::sidebar_view::row_detail_seat(&self.sidebar_bounds, &bounds) {
            Some(seat) => crate::baaz_log!(
                "hover id={known} seat={:.0},{:.0}",
                f32::from(seat.x),
                f32::from(seat.y)
            ),
            None => crate::baaz_log!("hover id={known} no-sidebar-edge"),
        }
        Ok(())
    }

    /// `new`: the same as ⌘N, in the current project. `new:<project name>`:
    /// the group row's `+` for the project named — an unknown name fails
    /// the step.
    pub(crate) fn step_new(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let name = rest.trim();
        if name.is_empty() {
            self.new_session(window, cx);
            return Ok(());
        }
        let id = self
            .projects
            .projects
            .iter()
            .find(|p| p.name == name)
            .map(|p| p.id.clone())
            .ok_or_else(|| format!("unknown project `{name}`"))?;
        self.new_session_in(Some(id), window, cx);
        Ok(())
    }

    /// `pin`: toggle the active session's pin; `pin:<id>`: toggle that row's.
    /// The id form is for captures: the replay row lives in closed Other, so
    /// pinning it shows nothing — `pin:s-web-1` pins a row on screen. No
    /// open session, or an id this window does not know, fails the step.
    pub(crate) fn step_pin(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let id = rest.trim();
        let session_id = if id.is_empty() {
            self.active_id(cx).ok_or("no open session")?
        } else if self.knows_session(id, cx) {
            id.to_owned()
        } else {
            return Err(format!("no session `{id}`"));
        };
        self.toggle_pin(session_id, cx);
        Ok(())
    }

    /// `archive`: raise the active session's archive confirmation. No open
    /// session fails the step.
    pub(crate) fn step_archive(&mut self, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let session_id = self.active_id(cx).ok_or("no open session")?;
        self.open_archive_dialog(session_id, cx);
        Ok(())
    }

    /// `new-in:<path>`: [`Self::new_session_in_root`] against a named root,
    /// so a capture can put a session in a folder of its own rather than in
    /// the default workspace. Relative paths resolve against the process's
    /// own directory, like `project:<path>` already does for adoption. An
    /// empty path, or one that is not a directory on disk, fails the step.
    pub(crate) fn step_new_in(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let rest = rest.trim();
        if rest.is_empty() {
            return Err("no folder path".into());
        }
        let path = std::path::PathBuf::from(rest);
        let path =
            if path.is_absolute() { path } else { std::env::current_dir().unwrap_or_default().join(path) };
        if !path.is_dir() {
            return Err(format!("`{}` is not a directory", path.display()));
        }
        self.new_session_in_root(&path, window, cx);
        Ok(())
    }

    /// `open:<session_id>` / `click:<session_id>`: [`Self::resume`] (an
    /// outside activation, reveal armed) or [`Self::resume_quiet`] (a row
    /// click). Fails the step when the id is empty, when it names no
    /// session this window knows — no sidebar row, no provider record, no
    /// index entry, no open or parked view, after the chain-head redirect
    /// `resume` applies — and when `resume` would refuse it as hidden or
    /// archived; offline, `resume` would otherwise open any id as a blank
    /// local view and the capture would pass on a session that is not
    /// there.
    pub(crate) fn step_open(
        &mut self,
        rest: &str,
        quiet: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> crate::steps::StepResult {
        let id = rest.trim();
        if id.is_empty() {
            return Err("no session id".into());
        }
        let head = self.chain_head(id);
        if !self.knows_session(id, cx) && !self.knows_session(&head, cx) {
            return Err(format!("no session `{id}`"));
        }
        if self.overrides.get(&head).is_some_and(|m| m.hidden) && !self.show_hidden {
            return Err(format!("session `{id}` is hidden"));
        }
        if self.overrides.get(&head).is_some_and(|m| m.archived) && !self.show_archived {
            return Err(format!("session `{id}` is archived"));
        }
        if quiet {
            self.resume_quiet(id.to_owned(), window, cx);
        } else {
            self.resume(id.to_owned(), window, cx);
        }
        Ok(())
    }

    /// B12fix `transcript:fold`: flip the persisted "Fold finished
    /// turns" switch (Settings → General, default on); every open view
    /// observes the layout global and re-folds — the same path as the
    /// settings row, so the probe, the palette and the switch never
    /// disagree. Registered in
    /// [`crate::steps::WINDOW_VERBS`]; any other payload fails the step
    /// instead of toggling something nobody asked for.
    pub(crate) fn step_transcript_fold(
        &mut self,
        rest: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> crate::steps::StepResult {
        let _ = window;
        if rest.trim() != "fold" {
            return Err(format!("unknown transcript payload `{rest}`; the only one is fold"));
        }
        let on = !crate::layout::Layout::get(cx).fold_finished_turns;
        crate::layout::Layout::update(cx, |layout| crate::settings::apply_setting(layout, "fold_finished_turns", on));
        self.invalidate_list();
        Ok(())
    }
}
