//! The setup sheet (D5 B2, docs/66 §3): every Install, every Claude Code
//! sign-in and every Update happens in one modal over whatever screen asked
//! — the connect screen, Settings, the composer's signed-out banner.
//!
//! The sheet holds a real terminal: Baaz's own `aui-terminal` session (a
//! login shell in the home directory), with the vendor command **typed, not
//! run** — the person reads it and presses Enter (docs/23 §5, owner
//! 2026-10-23). The status line at the top follows the status service, not
//! the terminal's text: opening the sheet starts [`watch_setup`], which
//! re-probes the provider every 3 s, and the window redraws when a probe
//! changes what it shows. Close, ✕ and Escape hide the sheet and nothing
//! else: the shell and the watch keep going, the row updates, and opening
//! the sheet again for the same provider shows the same terminal.
//!
//! [`watch_setup`]: crate::provider_status::watch_setup

use std::collections::HashMap;
use std::path::PathBuf;

use aui::screens::{setup_sheet, sheet_scrim, SetupStatus};
use aui_terminal::{terminal_grid, ScriptChunk, TerminalSession};
use gpui::{prelude::*, AnyElement, Context, Entity, FocusHandle, Focusable, Subscription, Window};

use crate::app::Harness;
use crate::provider_model::{ProviderModelEvent, ProviderStatusModel};
use crate::provider_status::{Headline, Installed, ProviderStatus};
use crate::providers::ProviderId;
use crate::providers::ProviderMark as _;

/// What the sheet is for: it decides the command, the title, the waiting
/// line, the footnote and when the sheet counts as done.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupKind {
    /// The vendor install command (Claude Code, Codex).
    Install,
    /// `claude auth login` (Claude Code only: Codex signs in in its row,
    /// Muse in its own sheet).
    SignIn,
    /// The CLI's own update command, below the version floor.
    Update,
}

impl SetupKind {
    /// The command typed into the terminal, when the provider has one.
    pub fn command(self, id: ProviderId) -> Option<&'static str> {
        match self {
            SetupKind::Install => id.install_command(),
            SetupKind::SignIn => (id == ProviderId::ClaudeCode).then_some("claude auth login"),
            SetupKind::Update => id.update_command(),
        }
    }

    /// The sheet's title: "Install Claude Code", "Sign in to Claude Code",
    /// "Update Codex".
    pub fn title(self, id: ProviderId) -> String {
        match self {
            SetupKind::Install => format!("Install {}", id.label()),
            SetupKind::SignIn => format!("Sign in to {}", id.label()),
            SetupKind::Update => format!("Update {}", id.label()),
        }
    }
}

/// The open sheet: which provider, what for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SetupSheet {
    /// The provider being set up.
    pub provider: ProviderId,
    /// What for.
    pub kind: SetupKind,
}

/// The sheet's terminals, one per provider, kept across Close so a closed
/// sheet never kills what runs in it.
#[derive(Default)]
pub struct SetupTerminals {
    sessions: HashMap<ProviderId, Entity<TerminalSession>>,
    /// What was last typed into each terminal and how many marks it had
    /// then: the same command with no new marks is still on the line.
    typed: HashMap<ProviderId, (String, usize)>,
}

/// How long the sheet waits for a fresh shell's first prompt before typing.
const PROMPT_WAIT: std::time::Duration = std::time::Duration::from_secs(5);
/// How long a chained sign-in waits for the installer to hand the prompt
/// back (the setup watch's own limit).
const CHAIN_WAIT: std::time::Duration = std::time::Duration::from_secs(600);

/// Whether the shell sits at a prompt: its last mark opens one and nothing
/// is running.
fn at_prompt(session: &TerminalSession) -> bool {
    let marks = session.marks();
    let idle = !session.alt_screen() && !session.blocks().iter().any(|block| block.running());
    idle && marks.last().is_some_and(|mark| mark.kind == aui_terminal::MarkKind::PromptStart)
}

/// An install that has landed with the CLI signed out (or unverified):
/// what moves an Install sheet on to signing in.
pub fn install_landed_signed_out(status: &ProviderStatus) -> bool {
    setup_done(SetupKind::Install, status)
        && matches!(status.headline(), Headline::SignedOut | Headline::Unverified)
}

/// Whether `status` is what the sheet was waiting for: Connected for a
/// sign-in; installed (any answer but Not installed) for an install; no
/// longer too old for an update.
pub fn setup_done(kind: SetupKind, status: &ProviderStatus) -> bool {
    if status.checked_at.is_none() || status.headline() == Headline::Checking {
        return false;
    }
    match kind {
        SetupKind::SignIn => status.headline() == Headline::Connected,
        SetupKind::Install => matches!(status.installed, Installed::Yes { .. }),
        SetupKind::Update => {
            matches!(status.installed, Installed::Yes { .. }) && crate::connect::too_old(status).is_none()
        }
    }
}

/// The status line: what the sheet waits for, then what the probe saw.
pub fn setup_status(kind: SetupKind, status: &ProviderStatus) -> SetupStatus {
    if !setup_done(kind, status) {
        return SetupStatus::Waiting(
            match kind {
                SetupKind::Install => "Waiting for the install to finish…",
                SetupKind::SignIn => "Waiting for sign-in…",
                SetupKind::Update => "Waiting for the update to finish…",
            }
            .into(),
        );
    }
    let line = crate::connect::status_line(status, true).text;
    if status.headline() == Headline::Connected {
        return SetupStatus::Connected(line);
    }
    // Installed or updated but not signed in yet: say what is on disk.
    let version = match &status.installed {
        Installed::Yes { version, .. } => crate::provider_status::version_triplet(version)
            .map(|(a, b, c)| format!(" · {a}.{b}.{c}"))
            .unwrap_or_default(),
        _ => String::new(),
    };
    SetupStatus::Connected(format!("Installed{version}").into())
}

/// The footnote under the terminal (the board's copy; the installed-but-
/// signed-out line is ours — the board shows only the Connected end).
pub fn setup_note(kind: SetupKind, status: &ProviderStatus) -> String {
    let name = status.provider.label();
    if setup_done(kind, status) {
        return if status.headline() == Headline::Connected {
            format!("{name} is ready. New sessions can start in it.")
        } else {
            match status.provider {
                ProviderId::Codex => format!("{name} is installed. Sign in next: it opens your browser."),
                _ => format!("{name} is installed. Sign in from its row next."),
            }
        };
    }
    match kind {
        // The command waits typed at the prompt: say so first. "Waiting for
        // sign-in" over a command nobody had run read as stuck (owner,
        // 2026-10-29: "where's the terminal to type the command?").
        SetupKind::SignIn => format!(
            "Press Return to run it: {name} opens your browser to sign in. This terminal is {name}’s own; Baaz never sees your credentials."
        ),
        SetupKind::Install | SetupKind::Update => format!(
            "The command is typed, not run: read it, then press Enter. Baaz checks every few seconds and shows Connected here when {name} answers."
        ),
    }
}

/// What a scripted (deterministic) sheet terminal shows: the prompt and the
/// typed command, never a shell.
fn scripted_terminal(command: &str) -> Vec<ScriptChunk> {
    vec![ScriptChunk::new(0, format!("~ % {command}"))]
}

/// The setup sheet's owner (task 4.4): its terminals and their focus; the
/// open sheet itself is an entry on the window's overlay stack (plan 4.6). It follows the shared [`ProviderStatusModel`] itself — the
/// status line redraws as probes land, and an Install sheet whose CLI has
/// landed signed out moves on to signing in — and renders itself over
/// whatever screen asked; the root seats it (`Harness::compose_setup_sheet`).
pub(crate) struct SetupHost {
    /// The window's overlay stack: the sheet while it is up (which
    /// provider, what for) is its `SetupSheet` entry.
    overlays: Entity<crate::overlays::Overlays>,
    /// The sheet's terminals, one per provider, kept across Close.
    terminals: SetupTerminals,
    /// The sheet terminal's keyboard focus.
    focus: FocusHandle,
    /// The window's provider facts, which the status line follows.
    model: Entity<ProviderStatusModel>,
    /// The model observation (redraw) and its change subscription (advance).
    _subscriptions: [Subscription; 2],
}

impl SetupHost {
    /// No sheet up, no terminal made yet.
    pub(crate) fn new(model: Entity<ProviderStatusModel>, overlays: Entity<crate::overlays::Overlays>, cx: &mut Context<Self>) -> Self {
        let redraw = cx.observe(&model, |this, _, cx| {
            if this.sheet(cx).is_some() {
                cx.notify();
            }
        });
        let advance = cx.subscribe(&model, |this, _, event: &ProviderModelEvent, cx| match event {
            ProviderModelEvent::Changed => this.advance(cx),
        });
        SetupHost {
            overlays,
            terminals: SetupTerminals::default(),
            focus: cx.focus_handle(),
            model,
            _subscriptions: [redraw, advance],
        }
    }

    /// The sheet while it is up.
    pub(crate) fn sheet(&self, cx: &gpui::App) -> Option<SetupSheet> {
        self.overlays.read(cx).stack.setup_sheet().copied()
    }


    /// Put the sheet up (or change what an open one is for).
    fn show(&mut self, sheet: SetupSheet, cx: &mut Context<Self>) {
        let owner = cx.entity_id();
        let view: gpui::AnyView = cx.entity().into();
        self.overlays.update(cx, |o, cx| match o.stack.setup_sheet_mut() {
            Some(open) => *open = sheet,
            // Drawn by this host; its `DismissEvent` reaches the root
            // through the root's own subscription (the host outlives its
            // sheets).
            None => o.push(
                crate::overlay_stack::Entry::new(crate::overlay_stack::Layer::SetupSheet(sheet)).owner(owner).view(view),
             cx),
        });
    }

    /// Open the setup sheet for `id`: the provider's terminal (made on first
    /// use, a login shell in the home directory) gets the command typed, the
    /// watch starts, and the terminal takes the keyboard. Nothing runs until
    /// the person presses Enter.
    pub(crate) fn open(
        &mut self,
        id: ProviderId,
        kind: SetupKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(command) = kind.command(id) else { return };
        let session = self.setup_terminal(id, command, cx);
        if !crate::runtime::providers_scripted() {
            self.type_when_ready(id, session, command, PROMPT_WAIT, cx);
        }
        self.show(SetupSheet { provider: id, kind }, cx);
        crate::provider_status::watch_setup(id);
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Type `command` once the shell sits at a prompt — its last mark is a
    /// prompt start (`OSC 133 A`) and nothing runs — never before: pasted
    /// into a shell still starting, the bytes echoed raw and zsh drew them a
    /// second time (found live, D5 B2). (B2 waited for a `B` mark the hooks
    /// never emit, so it always typed at the 5 s timeout.) After `wait` it
    /// types anyway unless something is running. Nothing is typed into a
    /// busy shell, or when this command is already typed and nothing ran
    /// since (Close, then the same button again).
    fn type_when_ready(
        &mut self,
        id: ProviderId,
        session: Entity<TerminalSession>,
        command: &str,
        wait: std::time::Duration,
        cx: &mut Context<Self>,
    ) {
        let command = command.to_owned();
        cx.spawn(async move |this, cx| {
            let began = std::time::Instant::now();
            loop {
                let ready = session.read_with(cx, |session, _| at_prompt(session));
                if ready || began.elapsed() >= wait {
                    break;
                }
                cx.background_executor().timer(std::time::Duration::from_millis(50)).await;
            }
            let _ = this.update(cx, |this, cx| {
                let (busy, ran) = session.read_with(cx, |session, _| {
                    let busy = session.alt_screen() || session.blocks().iter().any(|block| block.running());
                    (busy, session.marks().len())
                });
                let already = this.terminals.typed.get(&id) == Some(&(command.clone(), ran));
                if busy || already {
                    return;
                }
                session.update(cx, |session, _| {
                    // Clear the line first (Ctrl-U, kill-whole-line in zsh's
                    // and bash's default keymaps): a command typed earlier and
                    // never run — the install, when the CLI arrived another
                    // way — must not prefix this one.
                    session.write(b"\x15");
                    session.paste(&command);
                });
                let ran = session.read(cx).marks().len();
                this.terminals.typed.insert(id, (command, ran));
            });
        })
        .detach();
    }

    /// After a status change: an Install sheet whose CLI has landed but is
    /// signed out moves on to signing in, in the same sheet (owner
    /// 2026-10-25: one trip, not two). Claude Code's terminal gets `claude
    /// auth login` typed once the installer has finished; Codex, which signs
    /// in through its app-server, shows its next step instead (see
    /// the sheet's render).
    fn advance(&mut self, cx: &mut Context<Self>) {
        let Some(sheet) = self.sheet(cx) else { return };
        if sheet.kind != SetupKind::Install || sheet.provider != ProviderId::ClaudeCode {
            return;
        }
        let status = self.model.read(cx).status(sheet.provider);
        if !install_landed_signed_out(&status) {
            return;
        }
        let Some(command) = SetupKind::SignIn.command(sheet.provider) else { return };
        self.show(SetupSheet { provider: sheet.provider, kind: SetupKind::SignIn }, cx);
        if let Some(session) = self.terminals.sessions.get(&sheet.provider).cloned() {
            if !crate::runtime::providers_scripted() {
                // The installer may still be printing: wait for its prompt.
                self.type_when_ready(sheet.provider, session, command, CHAIN_WAIT, cx);
            }
        }
        crate::provider_status::watch_setup(sheet.provider);
        cx.notify();
    }

    /// The provider's sheet terminal, made on first use. In deterministic
    /// mode (captures, tests) a scripted backend shows the prompt and the
    /// typed command and never spawns a shell.
    fn setup_terminal(&mut self, id: ProviderId, command: &str, cx: &mut Context<Self>) -> Entity<TerminalSession> {
        if let Some(session) = self.terminals.sessions.get(&id) {
            return session.clone();
        }
        let session = if crate::runtime::providers_scripted() {
            TerminalSession::new(Box::new(crate::terminal::host::ScriptBackend::new(scripted_terminal(command))), 80, 7)
        } else {
            let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"));
            let mut config = aui_terminal::PtyConfig::login(&home);
            for (key, value) in crate::terminal::host::pty_extra_env() {
                config = config.with_env(key, value);
            }
            // A clean shell (owner 2026-10-25): none of the person's rc files,
            // so no prompt theme or rc errors (a missing `starship` printed
            // on every open) — just the board's `~ %` prompt. Its PATH is the
            // login-shell PATH Baaz computed: launched from the Dock, Baaz's
            // own names almost nothing.
            config = config
                .without_user_rc()
                .with_env("PATH", provider::env_path::login_path().to_string_lossy().into_owned());
            let nonce = config.nonce().to_owned();
            let mut pty = aui_terminal::Pty::new();
            let _ = pty.spawn_config(&config);
            TerminalSession::new(Box::new(pty), 80, 7).with_nonce(&nonce)
        };
        let entity = cx.new(|_| session);
        self.terminals.sessions.insert(id, entity.clone());
        entity
    }

    #[cfg(feature = "probe")]
    /// `setup:<provider>:<install|signin|update>` (`--steps`): open the sheet
    /// as its button would. Deterministic runs show a scripted terminal.
    /// `Err` for a provider id no registry entry owns (never the Muse
    /// fallback), an unknown or missing stage, extra payload, or a stage
    /// that provider has no command for.
    pub(crate) fn step(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let mut parts = rest.split(':');
        let raw_id = parts.next().unwrap_or("").trim();
        let Some(id) = ProviderId::parse_known(raw_id) else {
            return Err(format!("unknown provider {raw_id:?}"));
        };
        let kind = match parts.next().map(str::trim) {
            Some("install") => SetupKind::Install,
            Some("signin") => SetupKind::SignIn,
            Some("update") => SetupKind::Update,
            other => return Err(format!("unknown setup stage {other:?} (expected install, signin or update)")),
        };
        if let Some(extra) = parts.next() {
            return Err(format!("unexpected extra payload {extra:?}"));
        }
        if kind.command(id).is_none() {
            return Err(format!("{} has no command for that setup stage", id.as_str()));
        }
        self.open(id, kind, window, cx);
        Ok(())
    }

    /// Close the sheet (Close, Done, ✕, Escape). The terminal and the watch
    /// keep going.
    pub(crate) fn close(&mut self, cx: &mut Context<Self>) -> bool {
        self.overlays.update(cx, |o, cx| o.close(crate::overlay_stack::LayerKind::SetupSheet, cx)).is_some()
    }

    /// The sheet over whatever screen asked, or `None` while closed.
    fn render_sheet(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let sheet = self.sheet(cx)?;
        let id = sheet.provider;
        let status = self.model.read(cx).status(id);
        let session = self.terminals.sessions.get(&id)?.clone();
        // ✕ and Escape dismiss it through the overlay stack, newest first.
        let close = cx.listener(|_: &mut Self, _: &(), _, cx| cx.emit(gpui::DismissEvent));
        let escape = cx.listener(|_: &mut Self, event: &gpui::KeyDownEvent, _, cx| {
            if event.keystroke.key == "escape" {
                cx.emit(gpui::DismissEvent);
                cx.stop_propagation();
            }
        });
        let terminal = terminal_grid(&session).focus_handle(self.focus.clone());
        let mut body = setup_sheet("setup-sheet", id.icon(), sheet.kind.title(id))
            .status(setup_status(sheet.kind, &status))
            .terminal(gpui::div().size_full().track_focus(&self.focus).child(terminal))
            .note(setup_note(sheet.kind, &status))
            .on_close(move |window, cx| close(&(), window, cx));
        // Codex installed but signed out: its sign-in is the in-row flow,
        // one press away from here (the sheet closes onto the waiting row).
        if id == ProviderId::Codex && sheet.kind == SetupKind::Install && install_landed_signed_out(&status) {
            let sign_in = cx.listener(|this: &mut Self, _: &(), _, cx| {
                this.close(cx);
                this.model.update(cx, |model, cx| model.begin_codex_signin(cx));
            });
            body = body.next_step(format!("Sign in to {}", id.label()), move |window, cx| sign_in(&(), window, cx));
        }
        Some(
            gpui::div()
                .absolute()
                .inset_0()
                .capture_key_down(move |event, window, cx| escape(event, window, cx))
                .child(sheet_scrim("setup-sheet-scrim", body, None))
                .into_any_element(),
        )
    }
}

impl gpui::EventEmitter<gpui::DismissEvent> for SetupHost {}

impl Focusable for SetupHost {
    /// The sheet terminal's keyboard.
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for SetupHost {
    /// The sheet's layer while one is up; nothing otherwise (the root seats
    /// the view only while a sheet stands).
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.render_sheet(cx).unwrap_or_else(|| gpui::Empty.into_any_element())
    }
}

// ── the root's side ─────────────────────────────────────────────────────

impl Harness {
    /// Open the setup sheet for `id` ([`SetupHost::open`]).
    pub(crate) fn open_setup_sheet(&mut self, id: ProviderId, kind: SetupKind, window: &mut Window, cx: &mut Context<Self>) {
        self.setup.update(cx, |setup, cx| setup.open(id, kind, window, cx));
    }


    #[cfg(feature = "probe")]
    /// `setup:<provider>:<stage>` ([`SetupHost::step`]).
    pub(crate) fn step_setup(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> crate::steps::StepResult {
        self.setup.update(cx, |setup, cx| setup.step(rest, window, cx))
    }

    /// The sheet's view while one stands, for the connect screen's stack
    /// or the shell's.
    pub(crate) fn compose_setup_sheet(&self, cx: &gpui::App) -> Option<AnyElement> {
        self.overlays.read(cx).stack.view(crate::overlay_stack::LayerKind::SetupSheet).map(|view| view.into_any_element())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_status::{Advisory, Auth};

    fn status(installed: Installed, auth: Auth) -> ProviderStatus {
        ProviderStatus {
            provider: ProviderId::ClaudeCode,
            installed,
            auth,
            enabled: true,
            advisory: Advisory::None,
            checked_at: Some(1),
            usage: None,
        }
    }

    #[test]
    fn every_kind_types_the_vendor_command_and_never_one_for_muse() {
        assert_eq!(SetupKind::Install.command(ProviderId::ClaudeCode), Some("curl -fsSL https://claude.ai/install.sh | bash"));
        assert_eq!(SetupKind::SignIn.command(ProviderId::ClaudeCode), Some("claude auth login"));
        assert_eq!(SetupKind::Install.command(ProviderId::Codex), Some("curl -fsSL https://chatgpt.com/codex/install.sh | sh"));
        assert_eq!(SetupKind::Update.command(ProviderId::ClaudeCode), Some("claude update"));
        assert_eq!(SetupKind::SignIn.command(ProviderId::Codex), None, "Codex signs in in its row");
        for kind in [SetupKind::Install, SetupKind::SignIn, SetupKind::Update] {
            assert_eq!(kind.command(ProviderId::Muse), None, "Muse has no command");
        }
        assert_eq!(SetupKind::Install.title(ProviderId::ClaudeCode), "Install Claude Code");
        assert_eq!(SetupKind::SignIn.title(ProviderId::ClaudeCode), "Sign in to Claude Code");
        assert_eq!(SetupKind::Update.title(ProviderId::Codex), "Update Codex");
    }

    #[test]
    fn the_status_line_waits_then_follows_the_probe() {
        let missing = status(Installed::No, Auth::Unknown);
        assert_eq!(setup_status(SetupKind::Install, &missing), SetupStatus::Waiting("Waiting for the install to finish…".into()));
        let installed = status(Installed::yes("2.1.276 (Claude Code)", "/bin/claude"), Auth::SignedOut);
        assert_eq!(setup_status(SetupKind::Install, &installed), SetupStatus::Connected("Installed · 2.1.276".into()));
        assert_eq!(setup_status(SetupKind::SignIn, &installed), SetupStatus::Waiting("Waiting for sign-in…".into()));
        assert!(install_landed_signed_out(&installed), "installed + signed out moves the sheet on");
        assert!(!install_landed_signed_out(&missing));
        let signed_in = status(
            Installed::yes("2.1.276", "/bin/claude"),
            Auth::SignedIn { email: Some("ada@example.com".into()), plan: Some("Max".into()), method: None, source: None },
        );
        assert_eq!(setup_status(SetupKind::SignIn, &signed_in), SetupStatus::Connected("Connected · ada@example.com · Max".into()));
        assert_eq!(setup_note(SetupKind::SignIn, &signed_in), "Claude Code is ready. New sessions can start in it.");
    }

    /// The prompt wait reads the marks the hooks really emit: a session
    /// whose last mark opens a prompt is ready; one still running a
    /// command, or that has shown no prompt yet, is not.
    #[test]
    fn the_sheet_types_only_at_a_prompt() {
        let session = |bytes: &str| {
            let session = TerminalSession::new(
                Box::new(crate::terminal::host::ScriptBackend::new(vec![ScriptChunk::new(0, bytes)])),
                80,
                7,
            )
            .with_nonce("n1");
            for _ in 0..8 {
                if !session.pump() {
                    break;
                }
            }
            session
        };
        assert!(!at_prompt(&session("Last login: today\r\n")), "no prompt yet");
        assert!(at_prompt(&session("\x1b]133;A;k=n1\x07~ % ")), "at the first prompt");
        let running = "\x1b]133;A;k=n1\x07~ % \x1b]133;C;k=n1;cmd=Y3VybA==;enc=b64\x07\r\ninstalling…\r\n";
        assert!(!at_prompt(&session(running)), "the installer is still running");
        let back = format!("{running}\x1b]133;D;0;k=n1\x07\x1b]133;A;k=n1\x07~ % ");
        assert!(at_prompt(&session(&back)), "the installer finished and the prompt is back");
    }

    #[test]
    fn an_update_is_done_when_the_floor_is_met() {
        let mut old = status(Installed::yes("2.0.9", "/bin/claude"), Auth::SignedOut);
        old.advisory = Advisory::TooOld { need: "2.1.276".into() };
        assert!(!setup_done(SetupKind::Update, &old));
        old.advisory = Advisory::None;
        old.installed = Installed::yes("2.1.300", "/bin/claude");
        assert!(setup_done(SetupKind::Update, &old));
    }

    #[test]
    fn a_status_still_checking_is_never_done() {
        let mut pending = status(Installed::yes("2.1.276", "/bin/claude"), Auth::SignedOut);
        pending.checked_at = None;
        assert!(!setup_done(SetupKind::Install, &pending));
    }
}
