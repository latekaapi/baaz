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
//! | `top` | jump to the head of the transcript |
//! | `mid` | jump to the middle of the transcript |
//! | `end` | jump to the tail of the transcript |
//! | `expand-groups` | open every folded tool group |
//! | `bench:<n>` | drive N back-to-back frames for `HARNESS_FRAME_STATS` |
//! | `fork` | `session/fork` at the newest completed turn |
//! | `retry` | retry the newest failed turn |
//! | `search:<query>` | open the search palette, optionally on a query |
//! | `palette` | open the command palette |
//! | `resume` | open the resume picker |
//! | `fork-picker` | open the fork picker |
//! | `rename:<name>` | open the sidebar row's inline rename field |
//! | `hidden` | list hidden sessions anyway |
//! | `empty` | list sessions with no turns anyway |
//! | `show-archived` | list archived sessions anyway |
//! | `sidebar` | collapse or expand the sidebar |
//! | `sidebar-width:<px>` | settle the sidebar divider at a width |
//! | `overflow` | open the header's overflow menu |
//! | `view-menu` | open the Sessions caption's view menu |
//! | `account` | open the account menu |
//! | `pin` | pin or unpin the open session |
//! | `archive` | raise the archive confirmation |
//! | `archive-confirm` | confirm it |
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
    WindowVerb { verb: "palette", run: |this, _, _, cx| this.open_palette(PaletteKind::Commands, cx) },
    WindowVerb { verb: "resume", run: |this, _, _, cx| this.open_palette(PaletteKind::Resume, cx) },
    WindowVerb { verb: "fork-picker", run: |this, _, _, cx| this.open_palette(PaletteKind::Fork, cx) },
    WindowVerb { verb: "rename", run: |this, rest, window, cx| this.step_rename(rest, window, cx) },
    WindowVerb { verb: "hidden", run: |this, _, _, cx| this.step_toggle_hidden(cx) },
    WindowVerb { verb: "empty", run: |this, _, _, cx| this.step_toggle_empty(cx) },
    WindowVerb { verb: "sidebar-width", run: |this, rest, _, cx| this.step_sidebar_width(rest, cx) },
    WindowVerb { verb: "sidebar", run: |this, _, _, cx| this.toggle_sidebar(cx) },
    WindowVerb { verb: "overflow", run: |this, _, _, cx| this.open_menu(MenuKind::Overflow, cx) },
    WindowVerb { verb: "view-menu", run: |this, _, _, cx| this.open_menu(MenuKind::ViewOptions, cx) },
    WindowVerb { verb: "account", run: |this, _, _, cx| this.open_menu(MenuKind::Account, cx) },
    WindowVerb { verb: "pin", run: |this, _, _, cx| this.step_pin(cx) },
    WindowVerb { verb: "archive", run: |this, _, _, cx| this.step_archive(cx) },
    WindowVerb { verb: "archive-confirm", run: |this, _, window, cx| this.confirm_archive_dialog(window, cx) },
    WindowVerb { verb: "show-archived", run: |this, _, _, cx| this.step_toggle_archived(cx) },
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
    // The Phase 5 session operations, so their screenshots come from a command
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
        None => crate::harness_log!("unknown step `{head}`"),
    }
}

/// One `--login-steps` item. Returns whether the run continues; a failed or
/// unknown step ends it with a stderr line.
pub(crate) fn login_step(this: &mut Harness, step: &str, window: &mut Window, cx: &mut Context<Harness>) -> bool {
    let (head, rest) = split(step);
    match LOGIN_VERBS.iter().find(|v| v.verb == head) {
        Some(verb) => (verb.run)(this, rest, window, cx),
        None => {
            crate::harness_log!("unknown login step `{step}`");
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
    crate::shot::set_steps_running(true);
    let task = cx.spawn(async move |this, cx| {
        for step in steps {
            if let Some(ms) = step.strip_prefix("wait:") {
                let ms: u64 = ms.parse().unwrap_or(0);
                cx.background_executor().timer(std::time::Duration::from_millis(ms)).await;
                continue;
            }
            let ran = this.update_in(cx, |this, window, cx| {
                if !window_step(this, &step, window, cx) {
                    this.with_session(cx, |view, cx| session_step(view, &step, window, cx));
                }
            });
            if ran.is_err() {
                crate::shot::set_steps_running(false);
                return;
            }
        }
        crate::shot::set_steps_running(false);
    });
    this.wire_tasks().push(task);
}

/// `--login-steps`: drive the login screen from the command line.
///
/// Raises the flag the capture waits on exactly like [`run_steps`] does, so a
/// headless `--screenshot` waits for the login script to finish before the
/// settling delay.
pub(crate) fn run_login_steps(this: &mut Harness, cx: &mut Context<Harness>) {
    let steps = this.take_login_steps();
    if steps.is_empty() {
        return;
    }
    crate::shot::set_steps_running(true);
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
                    crate::shot::set_steps_running(false);
                    return;
                }
            }
        }
        crate::shot::set_steps_running(false);
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
}
