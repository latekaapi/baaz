//! The scripting surface: `--steps` and `--login-steps`, in one place.
//!
//! Every screenshot in `docs/images/` is taken from a command line rather than
//! from a pointer, and these two flags are how. Until now the verbs lived in
//! three separate matches — window verbs in `app.rs`, session verbs in
//! `session.rs`, login verbs in `app.rs` again — with no shared dispatch and
//! nothing that could say what the whole surface is (finding `app-core-4` /
//! `app-core-13`). This module is that one place: one parser
//! ([`split`]), three verb tables ([`WINDOW_VERBS`], [`SESSION_VERBS`],
//! [`LOGIN_VERBS`]), the two runners, and the tables below that document them.
//!
//! A handler is a plain function pointer into the entity that owns the state
//! the verb touches, so the state stays private where it was; what moved here
//! is the vocabulary and the dispatch.
//!
//! # Cost
//!
//! **The whole surface is scripting only**: no verb here has any other entry
//! point, so this module is the one place that documents them. Only `send:`
//! and `steer:` reach `turn/start` and bill a real turn (like `--send`);
//! every other step — including `shell:`, which the **server** runs, and which
//! is how an approval is raised for nothing — is free. `--login-steps`
//! reaches `turn/start` nowhere at all, so none of it costs anything.
//!
//! # `--steps <a;b;c>`
//!
//! One step per item, `;`-separated because a step's payload may contain a
//! comma. A step is `verb` or `verb:<payload>`. A verb the window owns is
//! tried first; anything else goes to the open session.
//!
//! | step | what it does |
//! |---|---|
//! | `draft:<text>` | put text in the composer |
//! | `send:<text>` | send a turn — **costs a turn** |
//! | `steer:<text>` | steer the running turn — **costs a turn** |
//! | `model` | open the model picker |
//! | `effort` | open the reasoning-effort picker |
//! | `mode` | open the approval-mode picker |
//! | `confirm` | activate the open menu's selected row |
//! | `name:<name>` | `/name`, the session rename command |
//! | `hide` | `/hide` the open session |
//! | `setmodel:<id>` | `session/setModel`, without waiting for the catalog |
//! | `compact` | `session/compact` |
//! | `meter` | pin the context meter's breakdown open |
//! | `context:<used>/<window>/<level>` | a synthetic `session/contextUsage` |
//! | `plan` | turn plan mode on |
//! | `image:<path>` | attach an image |
//! | `file:<path>` | attach a file |
//! | `plus` | open the composer's `+` menu |
//! | `drop` | raise the drop overlay |
//! | `command:<filter>` | open the `/` caret popover |
//! | `mention:<filter>` | open the `@` caret popover |
//! | `shell:<cmd>` | `session/userShell`, the approval generator that makes no model call |
//! | `setmode:<mode>` | `session/setApprovalMode`, without opening the picker |
//! | `choose:<n>` | the n-th choice of the newest pending approval |
//! | `feedback:<text>` | type into an open feedback or clarify field |
//! | `answer:<label>` | pick one option on the newest question and send |
//! | `answers:<a\|b>` | pick several options on the newest question and send |
//! | `clarify:<text>` | "Explain instead"; with no text, only opens the field |
//! | `preview:<n>` | open the n-th option's preview (0-based) |
//! | `select:<label>` | pick an option without sending |
//! | `skip` | decline the newest question |
//! | `select-text:<turn>:<from>-<to>` | hold a text selection over a turn |
//! | `select-span:<turn>` | hold the whole turn as one cross-block span |
//! | `copy:<turn>` | capture aid: press the turn's copy button (0-based, like `select-span`), so the success check holds for the screenshot, free |
//! | `top` | jump to the head of the transcript |
//! | `mid` | jump to the middle of the transcript |
//! | `end` | jump to the tail of the transcript |
//! | `expand-groups` | open every folded tool group |
//! | `bench:<n>` | drive N back-to-back frames for `BAAZ_FRAME_STATS` |
//! | `fork` | `session/fork` at the newest completed turn |
//! | `retry` | retry the newest failed turn |
//! | `search:<query>` | open the search palette, optionally on a query |
//! | `open:<session_id>` | open a session as an outside activation does (palette, boot; scripting only) — arms the one-shot reveal |
//! | `click:<session_id>` | open a session as a sidebar click does (scripting only) — never arms the reveal, never moves the list |
//! | `palette` | open the command palette |
//! | `resume` | open the resume picker |
//! | `fork-picker` | open the fork picker |
//! | `rename:<name>` | open the sidebar row's inline rename field |
//! | `hidden` | list hidden sessions anyway |
//! | `empty` | list sessions with no turns anyway |
//! | `show-archived` | list archived sessions anyway |
//! | `sidebar` | collapse or expand the sidebar |
//! | `sidebar-width:<px>` | settle the sidebar divider at a width |
//! | `row-detail:<session_id>` | capture aid: pin the hover card open for one row, seated at the selected row's bounds (pair with `click:` on the same id; empty clears), free |
//! | `hover:<session_id>` | capture aid: deliver the selected row's own hover report (what its hover event sends), arming the card past the delay seated from the row's bounds at the sidebar's right edge (pair with `click:` on the same id and a `wait:` past the delay; empty means the selected row), free |
//! | `resize-begin:<x>` | start a scripted resize drag through the real divider handler, logging `baaz: rsdrag` with the width, the sidebar offset, the reveal arm, the scrolled flag and the drag |
//! | `resize-move:<x>` | move a scripted resize drag through the real divider handler (same log) |
//! | `resize-end` | end a scripted resize drag through the real divider handler (same log) |
//! | `resize-sweep:<to_w,step_px>` | march the divider toward `to_w` one `step_px` per rendered frame, logging `baaz: rssweep w=<width> pane=<pane> root=<root> rehint=<0/1>` per tick |
//! | `sidebar-scroll-sweep:<dy,finger_ticks,tail_ticks>` | a frame-paced sidebar wheel gesture: `dy` for `finger_ticks` rendered frames, then decaying to 5% of `dy` over `tail_ticks` more, one push per tick (the in-process fallback for a real `CGEvent` gesture where the environment cannot confirm a landing window; pair with `BAAZ_FRAME_TRACE=1`) |
//! | `transcript-scroll-sweep:<dy,finger_ticks,tail_ticks>` | the transcript's twin of `sidebar-scroll-sweep:`, pushing the active session's own wheel accumulator |
//! | `overflow` | open the header's overflow menu |
//! | `view-menu` | open the Sessions caption's view menu |
//! | `account` | open the account menu |
//! | `settings:<section>` | open the Settings dialog, optionally on the section with that id (`settings` alone opens the first section) |
//! | `pin` | pin or unpin the open session |
//! | `archive` | raise the archive confirmation |
//! | `archive-confirm` | confirm it |
//! | `error-dialog:<offline\|blocked\|sorry>` | capture aid: raise a critical error dialog of that family through the real dialog path (default `sorry`), free |
//! | `projects` | open the Projects palette |
//! | `project:<path>` | adopt `path` as a project and make it current (no panel, no session) |
//! | `project-menu` | open the header's project menu (`project-menu:<name>` opens the group row menu for the project named) |
//! | `project-colour:<n>` | set the current project's colour slot (1–8); with no payload, open the Colour submenu |
//! | `group-by:<date\|project>` | persist the sidebar grouping and regroup |
//! | `group-bar` | flip the current-project accent bar flag |
//! | `group-branch` | flip the trailing-branch flag |
//! | `group-chevron` | flip the group-row chevron flag |
//! | `auto-title` | flip the automatic session-naming switch |
//! | `auto-summary` | flip the sidebar-summaries switch |
//! | `title-pending` | capture aid: the open session reads as if its title generation were in flight (`Naming this session…`), free |
//! | `title-timeout` | capture aid: stand that generation down through the watchdog's own path, so the row falls back to the first prompt, free |
//! | `title-land:<text>` | capture aid: land `<text>` as the open session's generated title, so the row and the crumb update — after `title-timeout`, like a late answer would — free |
//! | `remove-project:<name>` | raise the project removal dialog |
//! | `remove-confirm` | confirm it |
//! | `new` | the same as ⌘N |
//! | `new:<project>` | the group row's `+` for the project named; the session opens on the `session/start` round-trip, so following session verbs (`name:`, `draft:`, `send:`) wait for the switch (bounded, 10 s) instead of acting on the session that is still open |
//! | `wheel:<dy>` | dispatch one synthetic wheel event at the window centre and log `baaz: wheel dy=<dy> list_px=<before>-><after>` (the palette-scroll instrument) |
//! | `sidebar-wheel:<dy>[,n]` | dispatch n synthetic wheel events at a sidebar point and log `baaz: sbwheel dy=<dy> n=<n> sidebar_ix=<before_ix>+<before_off>-><after_ix>+<after_off> rows=<entries> pane=<pane> root=<root> centre=<centre> drains=<drains>` (the sidebar-scroll instrument: the virtual list's `ListOffset`, item index plus the pixel offset into that row, in place of the old div's pixel offset; pair with `wait:<ms>` and a trailing `sidebar-wheel:0,0` to read the burst's renders; `centre` is the cached transcript column's rebuilds) |
//! | `centre` | log `baaz: centre hero=<hero> loading=<loading>`: hero vs loading-row paints since the last call (the open-flicker instrument) |
//! | `wait:<ms>` | let the wire catch up before the next step |
//!
//! # `--login-steps <a;b;c>`
//!
//! Honoured only when the app is really connected (never with `--no-connect`
//! or `--replay`), run once the login screen is up, with the same parsing.
//! A step that fails ends the run with a stderr line.
//!
//! | step | what it does |
//! |---|---|
//! | `account` | start the device flow (the browser opens) |
//! | `apikey` | open the API-key form |
//! | `key-from-env:<VAR>` | put the value of environment variable `VAR` into the API-key field |
//! | `submit` | submit the API-key form |
//! | `wait:<ms>` | let the wire catch up before the next step |
//!
//! The key travels from the environment into the field and then into the wire
//! call: it never appears in argv, a log or a screenshot argument.

use aui::screens::LoginIntent;
use gpui::{Context, Window};

use crate::app::Harness;
use crate::overlays::{Command, MenuKind, PaletteKind};
use crate::session::{SessionEvent, SessionView};
use crate::wire::WireCall;

/// The one parser: a step is `verb` or `verb:<payload>`.
///
/// The payload keeps every character after the first colon, which is what lets
/// `select-text:2:0-40` and `context:120000/200000/warn` be one step each.
pub(crate) fn split(step: &str) -> (&str, &str) {
    step.split_once(':').unwrap_or((step, ""))
}

/// A `--steps` verb the window owns.
pub(crate) struct WindowVerb {
    /// What the step says.
    pub verb: &'static str,
    /// What it does to the window.
    pub run: fn(&mut Harness, &str, &mut Window, &mut Context<Harness>),
}

/// A `--steps` verb the open session owns.
pub(crate) struct SessionVerb {
    /// What the step says.
    pub verb: &'static str,
    /// What it does to the session.
    pub run: fn(&mut SessionView, &str, &mut Window, &mut Context<SessionView>),
}

/// A `--login-steps` verb. The handler returns whether the run continues.
pub(crate) struct LoginVerb {
    /// What the step says.
    pub verb: &'static str,
    /// What it does to the login screen.
    pub run: fn(&mut Harness, &str, &mut Window, &mut Context<Harness>) -> bool,
}

/// The window's own `--steps` verbs, tried before the session's.
///
/// `resume` is here and in [`SESSION_VERBS`]; the window's picker wins,
/// exactly as it did when the two matches were tried in this order.
pub(crate) const WINDOW_VERBS: &[WindowVerb] = &[
    WindowVerb { verb: "search", run: |this, rest, window, cx| this.step_search(rest, window, cx) },
    WindowVerb { verb: "open", run: |this, rest, window, cx| this.resume(rest.to_owned(), window, cx) },
    WindowVerb { verb: "click", run: |this, rest, window, cx| this.resume_quiet(rest.to_owned(), window, cx) },
    WindowVerb { verb: "palette", run: |this, _, _, cx| this.open_palette(PaletteKind::Commands, cx) },
    WindowVerb { verb: "resume", run: |this, _, _, cx| this.open_palette(PaletteKind::Resume, cx) },
    WindowVerb { verb: "fork-picker", run: |this, _, _, cx| this.open_palette(PaletteKind::Fork, cx) },
    WindowVerb { verb: "rename", run: |this, rest, window, cx| this.step_rename(rest, window, cx) },
    WindowVerb { verb: "hidden", run: |this, _, _, cx| this.step_toggle_hidden(cx) },
    WindowVerb { verb: "empty", run: |this, _, _, cx| this.step_toggle_empty(cx) },
    WindowVerb { verb: "sidebar-width", run: |this, rest, _, cx| this.step_sidebar_width(rest, cx) },
    WindowVerb { verb: "row-detail", run: |this, rest, _, cx| this.step_row_detail(rest, cx) },
    WindowVerb { verb: "hover", run: |this, rest, _, cx| this.step_hover(rest, cx) },
    WindowVerb { verb: "resize-begin", run: |this, rest, _, cx| this.step_resize_drag("begin", rest, cx) },
    WindowVerb { verb: "resize-move", run: |this, rest, _, cx| this.step_resize_drag("move", rest, cx) },
    WindowVerb { verb: "resize-end", run: |this, rest, _, cx| this.step_resize_drag("end", rest, cx) },
    WindowVerb { verb: "resize-sweep", run: |this, rest, _, cx| this.step_resize_sweep(rest, cx) },
    WindowVerb { verb: "sidebar-scroll-sweep", run: |this, rest, _, cx| this.step_sidebar_scroll_sweep(rest, cx) },
    WindowVerb {
        verb: "transcript-scroll-sweep",
        run: |this, rest, _, cx| this.step_transcript_scroll_sweep(rest, cx),
    },
    WindowVerb { verb: "sidebar", run: |this, _, _, cx| this.toggle_sidebar(cx) },
    WindowVerb { verb: "overflow", run: |this, _, _, cx| this.open_menu(MenuKind::Overflow, cx) },
    WindowVerb { verb: "view-menu", run: |this, _, _, cx| this.open_menu(MenuKind::ViewOptions, cx) },
    WindowVerb { verb: "account", run: |this, _, _, cx| this.open_menu(MenuKind::Account, cx) },
    WindowVerb { verb: "settings", run: |this, rest, _, cx| this.step_settings(rest, cx) },
    WindowVerb { verb: "pin", run: |this, rest, _, cx| this.step_pin(rest, cx) },
    WindowVerb { verb: "archive", run: |this, _, _, cx| this.step_archive(cx) },
    WindowVerb { verb: "error-dialog", run: |this, rest, _, cx| this.step_error_dialog(rest, cx) },
    WindowVerb { verb: "archive-confirm", run: |this, _, window, cx| this.confirm_archive_dialog(window, cx) },
    WindowVerb { verb: "show-archived", run: |this, _, _, cx| this.step_toggle_archived(cx) },
    WindowVerb { verb: "projects", run: |this, _, window, cx| this.step_projects(window, cx) },
    WindowVerb { verb: "project", run: |this, rest, _, cx| this.step_adopt(rest, cx) },
    WindowVerb { verb: "project-menu", run: |this, rest, _, cx| this.step_project_menu(rest, cx) },
    WindowVerb { verb: "project-colour", run: |this, rest, _, cx| this.step_project_colour(rest, cx) },
    WindowVerb { verb: "group-by", run: |this, rest, _, cx| this.step_group_by(rest, cx) },
    WindowVerb { verb: "title-pending", run: |this, _, _, cx| this.step_title_pending(cx) },
    WindowVerb { verb: "title-timeout", run: |this, _, _, cx| this.step_title_timeout(cx) },
    WindowVerb { verb: "title-land", run: |this, rest, _, cx| this.step_title_land(rest, cx) },
    WindowVerb { verb: "auto-title", run: |this, _, _, cx| this.step_auto_title(cx) },
    WindowVerb { verb: "auto-summary", run: |this, _, _, cx| this.step_auto_summary(cx) },
    WindowVerb { verb: "group-bar", run: |this, _, _, cx| this.step_group_bar(cx) },
    WindowVerb { verb: "group-branch", run: |this, _, _, cx| this.step_group_branch(cx) },
    WindowVerb { verb: "group-chevron", run: |this, _, _, cx| this.step_group_chevron(cx) },
    WindowVerb { verb: "remove-project", run: |this, rest, _, cx| this.step_remove_project(rest, cx) },
    WindowVerb { verb: "remove-confirm", run: |this, _, _, cx| this.step_remove_confirm(cx) },
    WindowVerb { verb: "new", run: |this, rest, window, cx| this.step_new(rest, window, cx) },
    WindowVerb { verb: "wheel", run: |this, rest, window, cx| this.step_wheel(rest, window, cx) },
    WindowVerb { verb: "sidebar-wheel", run: |this, rest, window, cx| this.step_sidebar_wheel(rest, window, cx) },
    WindowVerb { verb: "centre", run: |this, _, _, _| this.step_centre() },
];

/// The open session's `--steps` verbs.
pub(crate) const SESSION_VERBS: &[SessionVerb] = &[
    SessionVerb { verb: "draft", run: |v, rest, window, cx| v.set_draft(rest.to_owned(), window, cx) },
    SessionVerb { verb: "send", run: |v, rest, _, cx| v.send_text(rest.to_owned(), cx) },
    SessionVerb { verb: "steer", run: |v, rest, _, cx| v.steer_text(rest.to_owned(), cx) },
    SessionVerb { verb: "model", run: |v, _, _, cx| v.toggle_picker(MenuKind::Model, cx) },
    SessionVerb { verb: "effort", run: |v, _, _, cx| v.toggle_picker(MenuKind::Effort, cx) },
    SessionVerb { verb: "mode", run: |v, _, _, cx| v.toggle_picker(MenuKind::Mode, cx) },
    SessionVerb { verb: "confirm", run: |v, _, window, cx| v.confirm_menu(window, cx) },
    // The session operations, so their screenshots come from a command
    // line rather than from a pointer.
    SessionVerb {
        verb: "name",
        run: |v, rest, window, cx| v.run_command_with(Command::Name, rest.to_owned(), window, cx),
    },
    SessionVerb { verb: "hide", run: |_, _, _, cx| cx.emit(SessionEvent::Hide) },
    SessionVerb { verb: "resume", run: |_, _, _, cx| cx.emit(SessionEvent::Resume) },
    SessionVerb { verb: "setmodel", run: |v, rest, _, cx| v.set_model(rest, cx) },
    SessionVerb { verb: "compact", run: |v, _, _, cx| v.compact(cx) },
    SessionVerb { verb: "meter", run: |v, _, _, cx| v.step_meter(cx) },
    SessionVerb { verb: "context", run: |v, rest, _, cx| v.step_context(rest, cx) },
    SessionVerb { verb: "plan", run: |v, _, _, cx| v.set_plan(true, cx) },
    SessionVerb { verb: "image", run: |v, rest, _, cx| v.attach_paths(vec![rest.into()], cx) },
    SessionVerb { verb: "file", run: |v, rest, _, cx| v.attach_paths(vec![rest.into()], cx) },
    SessionVerb { verb: "plus", run: |v, _, _, cx| v.step_plus(cx) },
    SessionVerb { verb: "drop", run: |v, _, _, cx| v.step_drop(cx) },
    SessionVerb { verb: "command", run: |v, rest, window, cx| v.step_caret_menu("/", rest, window, cx) },
    SessionVerb { verb: "mention", run: |v, rest, window, cx| v.step_caret_menu("@", rest, window, cx) },
    // `session/userShell`: free on every provider, and the only way to raise a
    // real approval without spending a turn.
    SessionVerb { verb: "shell", run: |v, rest, _, cx| v.run_user_shell(rest.to_owned(), cx) },
    SessionVerb { verb: "setmode", run: |v, rest, _, cx| v.step_setmode(rest, cx) },
    SessionVerb { verb: "choose", run: |v, rest, window, cx| v.step_choose(rest, window, cx) },
    SessionVerb { verb: "feedback", run: |v, rest, window, cx| v.step_feedback(rest, window, cx) },
    SessionVerb { verb: "answer", run: |v, rest, _, cx| v.step_answer(rest, cx) },
    SessionVerb { verb: "answers", run: |v, rest, _, cx| v.step_answer(rest, cx) },
    SessionVerb { verb: "clarify", run: |v, rest, window, cx| v.step_clarify(rest, window, cx) },
    SessionVerb { verb: "preview", run: |v, rest, _, cx| v.step_preview(rest, cx) },
    SessionVerb { verb: "select", run: |v, rest, _, cx| v.step_select(rest, cx) },
    SessionVerb { verb: "skip", run: |v, _, _, cx| v.step_skip(cx) },
    // Transcript inspection: jump without touching the pointer, hold a
    // selection, and open every tool group for its screenshot.
    SessionVerb { verb: "select-text", run: |v, rest, _, cx| v.select_text_step(rest, cx) },
    SessionVerb { verb: "select-span", run: |v, rest, _, cx| v.select_span_step(rest, cx) },
    SessionVerb { verb: "copy", run: |v, rest, window, cx| v.step_copy(rest, window, cx) },
    SessionVerb { verb: "top", run: |v, _, _, cx| v.step_top(cx) },
    SessionVerb { verb: "end", run: |v, _, _, cx| v.step_end(cx) },
    SessionVerb { verb: "mid", run: |v, _, _, cx| v.step_mid(cx) },
    SessionVerb { verb: "expand-groups", run: |v, _, _, cx| v.expand_all_groups(cx) },
    SessionVerb { verb: "bench", run: |v, rest, _, cx| v.step_bench(rest, cx) },
    SessionVerb { verb: "fork", run: |v, _, _, cx| v.fork(None, cx) },
    SessionVerb { verb: "retry", run: |v, _, _, cx| v.step_retry(cx) },
    // `wait` is handled by the runners, which are the only thing that can let
    // the wire catch up; reaching a table means it slipped through.
    SessionVerb { verb: "wait", run: |_, _, _, _| {} },
];

/// The `--login-steps` verbs.
pub(crate) const LOGIN_VERBS: &[LoginVerb] = &[
    LoginVerb {
        verb: "account",
        run: |this, _, window, cx| {
            this.login_intent(LoginIntent::StartAccount, window, cx);
            true
        },
    },
    LoginVerb {
        verb: "apikey",
        run: |this, _, window, cx| {
            this.login_intent(LoginIntent::UseApiKey, window, cx);
            true
        },
    },
    LoginVerb { verb: "key-from-env", run: |this, rest, window, cx| this.step_key_from_env(rest, window, cx) },
    LoginVerb {
        verb: "submit",
        run: |this, _, window, cx| {
            this.login_intent(LoginIntent::SubmitApiKey, window, cx);
            true
        },
    },
    // As above: the runner owns `wait:<ms>`.
    LoginVerb { verb: "wait", run: |_, _, _, _| true },
];

/// How often a scripted `image:` step asks whether the background decode has
/// landed, and how many times before it gives up (finding `performance-14`).
const ATTACH_POLL: std::time::Duration = std::time::Duration::from_millis(20);
/// 100 × 20 ms = two seconds, which is far past any image under the 10 MB cap.
const ATTACH_WAIT_POLLS: usize = 100;

/// One `--steps` item, tried against the window's verbs.
///
/// Returns whether it was one of them; anything else goes on to
/// [`session_step`], exactly as the two matches used to be tried in turn.
pub(crate) fn window_step(this: &mut Harness, step: &str, window: &mut Window, cx: &mut Context<Harness>) -> bool {
    let (head, rest) = split(step);
    let Some(verb) = WINDOW_VERBS.iter().find(|v| v.verb == head) else { return false };
    (verb.run)(this, rest, window, cx);
    cx.notify();
    true
}

/// One `--steps` item, run against the open session.
pub(crate) fn session_step(view: &mut SessionView, step: &str, window: &mut Window, cx: &mut Context<SessionView>) {
    let (head, rest) = split(step);
    match SESSION_VERBS.iter().find(|v| v.verb == head) {
        Some(verb) => (verb.run)(view, rest, window, cx),
        None => crate::baaz_log!("unknown step `{head}`"),
    }
}

/// Whether a `--steps` item runs against the open session (as opposed to
/// the window): mirrors [`window_step`]'s lookup without running anything.
fn is_session_step(step: &str) -> bool {
    let (head, _) = split(step);
    WINDOW_VERBS.iter().all(|v| v.verb != head)
}

/// How long session verbs wait for a `new:` switch: the `session/start`
/// round-trip is usually far under a second; the bound only fires when the
/// switch never comes, and then the step runs against whatever is open
/// (today's behaviour) with a log line.
const SWITCH_POLL: std::time::Duration = std::time::Duration::from_millis(50);
const SWITCH_WAIT_POLLS: usize = 200;

/// One `--login-steps` item. Returns whether the run continues; a failed or
/// unknown step ends it with a stderr line.
pub(crate) fn login_step(this: &mut Harness, step: &str, window: &mut Window, cx: &mut Context<Harness>) -> bool {
    let (head, rest) = split(step);
    match LOGIN_VERBS.iter().find(|v| v.verb == head) {
        Some(verb) => (verb.run)(this, rest, window, cx),
        None => {
            crate::baaz_log!("unknown login step `{step}`");
            false
        }
    }
}

/// `--steps`: drive the open session from the command line.
///
/// The steps run on a task rather than in a loop, because `wait:<ms>` is the
/// only way a scripted approval round-trip can exist: a decision has to reach
/// the wire and its `approval/updated` has to come back before the next
/// `choose:` means anything.
pub(crate) fn run_steps(this: &mut Harness, cx: &mut Context<Harness>) {
    let steps = this.take_steps();
    if steps.is_empty() {
        return;
    }
    crate::baaz_log!("steps: running {} scripted steps", steps.len());
    let capture = this.capture.clone();
    capture.set_steps_running(true);
    let task = cx.spawn(async move |this, cx| {
        for step in steps {
            if let Some(ms) = step.strip_prefix("wait:") {
                let ms: u64 = ms.parse().unwrap_or(0);
                cx.background_executor().timer(std::time::Duration::from_millis(ms)).await;
                continue;
            }
            // A `new:` step's session opens on the `session/start`
            // round-trip, after the following steps would run: session
            // verbs wait for the switch (bounded) so `name:`/`draft:`/
            // `send:` reach the session the script meant, not the one
            // still open. Without this, `new:demo` followed at once by
            // `send:` could bill the turn on the session that was open
            // before.
            if is_session_step(&step) {
                for _ in 0..SWITCH_WAIT_POLLS {
                    let pending = this
                        .read_with(cx, |this, _| this.session_switch_pending)
                        .unwrap_or(false);
                    if !pending {
                        break;
                    }
                    cx.background_executor().timer(SWITCH_POLL).await;
                }
                let still_pending = this
                    .read_with(cx, |this, _| this.session_switch_pending)
                    .unwrap_or(false);
                if still_pending {
                    let active = this
                        .read_with(cx, |this, cx| this.active_id(cx))
                        .unwrap_or(None);
                    crate::baaz_log!(
                        "session switch never arrived; `{step}` runs against {active:?}"
                    );
                }
                // A session verb with no open session has nowhere to go:
                // say so and skip it rather than vanishing into
                // `with_session`'s silent no-op below.
                let has_session = this
                    .read_with(cx, |this, cx| this.active_id(cx).is_some())
                    .unwrap_or(false);
                if !has_session {
                    crate::baaz_log!("`{step}`: no open session; skipped");
                    continue;
                }
            }
            let ran = this.update_in(cx, |this, window, cx| {
                if !window_step(this, &step, window, cx) {
                    this.with_session(cx, |view, cx| session_step(view, &step, window, cx));
                }
            });
            if ran.is_err() {
                crate::baaz_log!("steps: `{step}` lost its window; ending script");
                capture.set_steps_running(false);
                capture.set_steps_done(true);
                return;
            }
            // `image:` hands its read and decode to the background executor
            // (finding `performance-14`), so the step is not done when the
            // call returns — it is done when the chip stops being a
            // placeholder. Bounded, so a file that never decodes cannot hang
            // a scripted capture.
            for _ in 0..ATTACH_WAIT_POLLS {
                let pending = this
                    .read_with(cx, |this, cx| {
                        this.active.as_ref().is_some_and(|view| view.read(cx).attachments_pending())
                    })
                    .unwrap_or(false);
                if !pending {
                    break;
                }
                cx.background_executor().timer(ATTACH_POLL).await;
            }
        }
        capture.set_steps_running(false);
        capture.set_steps_done(true);
    });
    this.wire_tasks().push(task);
}

/// `--login-steps`: drive the login screen from the command line.
///
/// Raises the flags the capture waits on exactly like [`run_steps`] does, so
/// a headless `--screenshot` waits for the login script to finish before the
/// settling delay.
pub(crate) fn run_login_steps(this: &mut Harness, cx: &mut Context<Harness>) {
    let steps = this.take_login_steps();
    if steps.is_empty() {
        return;
    }
    crate::baaz_log!("login-steps: running {} scripted steps", steps.len());
    let capture = this.capture.clone();
    // The login screen is up — the list this runs against needs no boot
    // session — so readiness holds from the drain, like `maybe_run_steps`
    // raising it before [`run_steps`].
    capture.set_steps_ready(true);
    capture.set_steps_running(true);
    let task = cx.spawn(async move |this, cx| {
        for step in steps {
            if let Some(ms) = step.strip_prefix("wait:") {
                let ms: u64 = ms.parse().unwrap_or(0);
                cx.background_executor().timer(std::time::Duration::from_millis(ms)).await;
                continue;
            }
            // `update_in` for the window the field and the focus need.
            let ran = this.update_in(cx, |this, window, cx| login_step(this, &step, window, cx));
            match ran {
                Ok(true) => {}
                _ => {
                    crate::baaz_log!("login-steps: `{step}` failed; ending script");
                    capture.set_steps_running(false);
                    capture.set_steps_done(true);
                    return;
                }
            }
        }
        capture.set_steps_running(false);
        capture.set_steps_done(true);
    });
    this.wire_tasks().push(task);
}

#[cfg(test)]
mod tests {
    use super::{LOGIN_VERBS, SESSION_VERBS, WINDOW_VERBS};
    use std::collections::BTreeSet;

    /// This very file, so the documented tables and the verb tables can be
    /// compared without a build script: the module rustdoc above is the one
    /// place `--steps` is documented, and a verb that is in the code and not
    /// in the table (or the other way round) is a verb nobody can find.
    const SOURCE: &str = include_str!("steps.rs");

    /// The verbs one `//! | `x` | … |` table documents, between two headings.
    fn documented(section: &str) -> BTreeSet<String> {
        let start = SOURCE.find(section).unwrap_or_else(|| panic!("no `{section}` heading in steps.rs"));
        let rest = &SOURCE[start + section.len()..];
        let end = rest.find("//! # ").unwrap_or(rest.len());
        rest[..end]
            .lines()
            .filter_map(|line| line.trim().strip_prefix("//! | "))
            .filter_map(|row| row.split('|').next())
            .filter_map(|cell| cell.trim().strip_prefix('`'))
            .map(|cell| {
                let cell = cell.split('`').next().unwrap_or(cell);
                cell.split(':').next().unwrap_or(cell).to_owned()
            })
            .collect()
    }

    #[test]
    fn every_steps_verb_is_documented_and_every_documented_verb_exists() {
        let known: BTreeSet<String> = WINDOW_VERBS
            .iter()
            .map(|v| v.verb.to_owned())
            .chain(SESSION_VERBS.iter().map(|v| v.verb.to_owned()))
            .collect();
        assert_eq!(documented("//! # `--steps <a;b;c>`"), known);
    }

    #[test]
    fn every_login_steps_verb_is_documented_and_every_documented_verb_exists() {
        let known: BTreeSet<String> = LOGIN_VERBS.iter().map(|v| v.verb.to_owned()).collect();
        assert_eq!(documented("//! # `--login-steps <a;b;c>`"), known);
    }

    #[test]
    fn a_step_splits_at_its_first_colon_only() {
        assert_eq!(super::split("plan"), ("plan", ""));
        assert_eq!(super::split("draft:hello"), ("draft", "hello"));
        assert_eq!(super::split("select-text:2:0-40"), ("select-text", "2:0-40"));
    }

    #[test]
    fn the_window_owns_new_and_the_session_owns_the_rest() {
        // The skip in `run_steps` ("no open session") keys off this: a
        // session verb must never classify as a window verb, and `wait:`
        // never reaches a table at all.
        assert!(!super::is_session_step("new"));
        assert!(!super::is_session_step("new:demo"));
        assert!(super::is_session_step("draft:hello"));
        assert!(super::is_session_step("send:hi"));
        assert!(super::is_session_step("wait:3000"));
        assert!(super::is_session_step("bogusverb"));
    }
}
