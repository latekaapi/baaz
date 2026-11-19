//! The terminal dock as its own entity (task 4.4): a
//! [`Region`](crate::region::Region) that hangs under the composer in the
//! centre column (D42).
//!
//! [`TerminalDock`] owns everything only the dock reads: the tab host it
//! draws ([`TerminalHost`], shared as an entity with the session views that
//! run commands in it, the agent's terminal service and the quit check), its
//! keyboard focus, the resize drag in flight, and which provider each
//! terminal bridge session answers for (the tabs' agent marks). It renders
//! itself; `Harness::render` seats its view under the centre's body.
//!
//! Its open state and height persist in [`Layout`][crate::layout::Layout];
//! the height is clamped to `[120px, 70% of the centre column]`.
//!
//! What only the root knows reaches the dock as [`DockFacts`] (the current
//! project, the active session, the root's provider records for the tabs'
//! origins), pushed every frame before the root composes the dock and before
//! every root call into it (`Harness::with_terminal_dock`). What the dock
//! cannot do itself goes out as a [`TerminalDockEvent`]: a link for the
//! Browser pane, a busy tab's close confirmation (the window's dialog), a
//! block for the composer's draft.
//!
//! The root keeps the gestures that must work while the dock does not hold
//! the keyboard — ⌃` and the palette's "New terminal" — and the B11b rule
//! with them: while the Settings page stands open those are ignored and the
//! dock is hidden (the page replaces the centre), never closed.

use std::collections::HashMap;
use std::path::PathBuf;

use aui::workbench::{terminal_dock, terminal_tabs, TermTab, TerminalDockAction, TerminalTabsAction};
use aui_icons::Provider;
use aui_motion::{spring_px, SpringKind};
use aui_terminal::{terminal_grid, TerminalGridIntent};
use aui_tokens::scale;
use gpui::{
    div, prelude::*, px, AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, Pixels,
    SharedString, Window,
};

use super::{Pick, TabOwner, TerminalHost};
use crate::overlays::Overlays;
use crate::region::Region;
use crate::layout::LayoutGlobal as _;

/// The dock's shortest height, in window pixels.
pub const DOCK_MIN_HEIGHT: f32 = 120.0;

/// The dock's height before the person ever resized it.
pub const DOCK_DEFAULT_HEIGHT: f32 = 260.0;

/// Clamp a dock height into `[120px, 70% of the centre column]`.
pub fn clamp_dock_height(want: f32, centre_height: f32) -> f32 {
    let max = (centre_height * 0.7).max(DOCK_MIN_HEIGHT);
    want.clamp(DOCK_MIN_HEIGHT, max)
}

/// What the dock needs from the root: pushed every frame and before every
/// root call into the dock (see the module doc). Never persists.
#[derive(Clone, Default)]
pub(crate) struct DockFacts {
    /// The current project's root: the tabs the dock shows. `None` draws
    /// no dock at all.
    pub project: Option<PathBuf>,
    /// The active session's id: the origin a tab opened now records.
    pub origin: Option<String>,
    /// The root's durable provider records for the shown tabs' origins
    /// (origin session id → provider id string): the first answer to
    /// whose agent a tab belongs to; the dock's own bridge map is the
    /// second.
    pub records: HashMap<String, String>,
}

/// What the dock asks of the root (see the module doc).
#[derive(Clone, Debug)]
pub(crate) enum TerminalDockEvent {
    /// A block's `http`/`https` link: open it in the Browser pane.
    OpenUrl(String),
    /// A busy tab's ✕: ask first (D52), naming the running command.
    ConfirmClose { tab: String, running: String },
    /// A block's Ask: its command and output go into the active session's
    /// composer draft, unsent.
    Ask { command: String, output: String },
}

/// The terminal dock: per-project tabs over the library's pty backend.
pub(crate) struct TerminalDock {
    /// The terminal tabs behind the dock, keyed by project root (D43).
    host: Entity<TerminalHost>,
    /// The dock's own focus: ⌃` focuses it on open, so keys reach the pty
    /// through the wrapper until a click hands the keyboard to the grid.
    focus: FocusHandle,
    /// A dock resize drag in flight: the grab position and the start
    /// height, both in window pixels.
    drag: Option<(f32, f32)>,
    /// Which provider each terminal bridge session answers for, by the id
    /// the bridge names (`--session`). A Codex open mints its thread id
    /// after the bridge is already registered under the request id, so the
    /// root's durable record never names that id — without this map the
    /// dock cannot tell whose agent tab it draws (T3b).
    providers: HashMap<String, String>,
    /// The window's floating layer, shared with the root: the dock's toasts.
    overlays: Entity<Overlays>,
    /// What the root told the dock (see [`DockFacts`]).
    pub(crate) facts: DockFacts,
}

impl EventEmitter<TerminalDockEvent> for TerminalDock {}

impl TerminalDock {
    /// The dock at boot over `host`, no tab drawn yet.
    pub(crate) fn new(host: Entity<TerminalHost>, overlays: Entity<Overlays>, cx: &mut Context<Self>) -> Self {
        Self { host, focus: cx.focus_handle(), drag: None, providers: HashMap::new(), overlays, facts: DockFacts::default() }
    }

    /// The tab host, for the session views that run commands in it and the
    /// quit check that asks whether any is busy.
    pub(crate) fn host(&self) -> &Entity<TerminalHost> {
        &self.host
    }

    /// Note which provider `session_id`'s terminal bridge answers for.
    pub(crate) fn note_provider(&mut self, session_id: &str, provider: &str) {
        self.providers.insert(session_id.to_owned(), provider.to_owned());
    }

    /// Forget a session id the dock keyed its bridge provider by (a
    /// provisional id that never landed, or one rekeyed to its real id).
    pub(crate) fn forget_provider(&mut self, session_id: &str) {
        self.providers.remove(session_id);
    }

    /// Whether the dock keyed a bridge provider under `session_id`.
    #[cfg(test)]
    pub(crate) fn knows_provider(&self, session_id: &str) -> bool {
        self.providers.contains_key(session_id)
    }

    /// The agent mark for a terminal tab: the provider of the session that
    /// opened it — Claude Code's C, Codex's O — and `None` when the person
    /// opened it or no record names the lane (D43). `record` is the root's
    /// durable record for the origin, read first. The strip still wears
    /// Muse's M for an unknown tab, which is what the deterministic
    /// capture's baseline holds; only the dock hint goes neutral there.
    pub(crate) fn tab_provider(&self, origin: Option<&str>, record: Option<&str>) -> Option<Provider> {
        record
            .and_then(crate::sidebar::provider_mark)
            .or_else(|| origin.and_then(|session| self.providers.get(session)).and_then(|provider| crate::sidebar::provider_mark(provider)))
    }

    /// The dock header's hint for the active tab's agent: whose session the
    /// person shares the terminal with (D43). Each lane names its own agent
    /// by the label of the provider wearing that mark, read from the
    /// registry (rule 14); a tab no session owns names none, rather than
    /// wearing Muse's name.
    pub(crate) fn hint_name(provider: Option<Provider>) -> String {
        use crate::providers::{ProviderId, ProviderMark as _};
        match provider.and_then(|mark| ProviderId::all().into_iter().find(|id| id.icon() == mark)) {
            Some(id) => format!("{} can type here", id.label()),
            None => "The agent can type here".to_owned(),
        }
    }

    // ── what the root asks ──────────────────────────────────────────────

    /// ⌃` opened the dock: the first open creates the project's tab (later
    /// opens keep it), and the dock takes the keyboard so keys reach the
    /// pty. Tabs belong to the project, not the session, so the tab is
    /// created once per project in a window's life (D43).
    ///
    /// No notify here or in the other opens below: each writes the layout
    /// (`terminal_open`), whose observer redraws the window, and the root
    /// composes the dock uncached, so the dock draws on that frame.
    pub(crate) fn opened(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(root) = self.facts.project.clone() {
            if self.host.read(cx).tabs_for(&root).is_empty() {
                let origin = self.facts.origin.clone();
                self.host.update(cx, |host, cx| {
                    host.open(&root, "shell".to_owned(), TabOwner::User, origin, cx);
                });
            }
        }
        window.focus(&self.focus, cx);
    }

    /// A new terminal tab on the current project, opening the dock for it.
    ///
    /// Goes through D43's [`pick`](TerminalHost::pick) with a `"new"`
    /// route, so the rule the unit tests pin is the rule this runs. The
    /// root's B11b guard runs before this.
    pub(crate) fn new_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        crate::layout::Layout::stage(cx, |layout| layout.terminal_open = true);
        if let Some(root) = self.facts.project.clone() {
            let decision = self.host.read(cx).pick(cx, &root, false);
            match decision {
                Pick::New => {
                    let origin = self.facts.origin.clone();
                    let title = super::title_from_command("shell");
                    self.host.update(cx, |host, cx| {
                        host.open(&root, title, TabOwner::User, origin, cx);
                    });
                }
                Pick::Existing(id) => {
                    self.host.update(cx, |host, _| host.activate(&id));
                }
            }
        }
        window.focus(&self.focus, cx);
        crate::layout::Layout::save(cx);
    }

    /// D51 "Open terminal" on a terminal tool card: open the dock and focus
    /// the card's tab. A tab the host no longer holds (or none named) opens
    /// the dock on its active tab instead of failing. Only its test reaches
    /// it since the setup sheet took the typed commands (D5 B2); kept for
    /// the tool card it was written for.
    #[cfg(test)]
    pub(crate) fn open_tab(&mut self, tab: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        crate::layout::Layout::stage(cx, |layout| layout.terminal_open = true);
        if let Some(tab) = tab {
            if self.host.read(cx).get(&tab).is_some() {
                self.host.update(cx, |host, _| host.activate(&tab));
            }
        }
        window.focus(&self.focus, cx);
        crate::layout::Layout::save(cx);
    }

    #[cfg(feature = "probe")]
    /// The `--steps` verb's dock: open it over a FakePty-backed tab named
    /// `title` on `root` and drain its script at once, so the capture
    /// replays the same bytes on every run.
    pub(crate) fn open_scripted(&mut self, root: &std::path::Path, title: String, window: &mut Window, cx: &mut Context<Self>) {
        crate::layout::Layout::stage(cx, |layout| layout.terminal_open = true);
        let origin = self.facts.origin.clone();
        let nonce = aui_terminal::generate_nonce();
        let script = super::deterministic_script(&nonce);
        self.host.update(cx, |host, cx| {
            let id = host.open_fake(root, title, TabOwner::User, origin, script, &nonce, cx);
            host.drain(&id, cx);
        });
        crate::layout::Layout::save(cx);
        window.focus(&self.focus, cx);
    }

    /// Close a tab now, with no question (the busy tab's confirmed close).
    pub(crate) fn close_tab_now(&mut self, tab: &str, cx: &mut Context<Self>) {
        self.host.update(cx, |host, _| host.close(tab));
        cx.notify();
    }

    // ── the dock's own controls ─────────────────────────────────────────

    /// ⌃C while the dock holds the keyboard: SIGINT to the project's
    /// active tab, where ⌃C while the composer holds it stops the running
    /// turn.
    fn sigint(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.facts.project.clone() else { return };
        let session = self.host.read(cx).active_for(&root).map(|tab| tab.session.clone());
        if let Some(session) = session {
            session.update(cx, |session, _| session.write(b"\x03"));
        }
    }

    /// Point the project's active tab at its n-th tab, in open order.
    pub(crate) fn activate_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(root) = self.facts.project.clone() else { return };
        let id = self.host.read(cx).tabs_for(&root).get(index).map(|tab| tab.id.clone());
        if let Some(id) = id {
            self.host.update(cx, |host, _| host.activate(&id));
            cx.notify();
        }
    }

    /// Close the project's n-th tab, in open order — asking first when it
    /// is busy (D52), naming the running command. An idle tab closes
    /// outright.
    fn close_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(root) = self.facts.project.clone() else { return };
        let id = self.host.read(cx).tabs_for(&root).get(index).map(|tab| tab.id.clone());
        if let Some(id) = id {
            // A busy tab asks first (the root raises the dialog, nothing
            // here moves yet); an idle one closes now.
            match self.host.read(cx).running_command(cx, &id) {
                Some(running) => cx.emit(TerminalDockEvent::ConfirmClose { tab: id, running }),
                None => self.close_tab_now(&id, cx),
            }
        }
    }

    /// One block hover action from the active tab's grid (D44). The library
    /// performs none of these — it only reports the intent with a block
    /// index, and the dock reads the block back and decides. Nothing here
    /// touches the wire: no turn, no send.
    fn handle_intent(&mut self, tab_id: &str, intent: TerminalGridIntent, window: &mut Window, cx: &mut Context<Self>) {
        match intent {
            TerminalGridIntent::OpenUrl(url) => {
                // A program can print any OSC 8 link it likes, so only
                // `http`/`https` ever reach the browser — the rest are
                // ignored (D53). Reached links open in Baaz's Browser pane
                // (B9), like transcript links.
                if super::intents::openable_url(&url) {
                    cx.emit(TerminalDockEvent::OpenUrl(url.to_string()));
                }
            }
            TerminalGridIntent::Copy(block) => {
                let text = self.host.read(cx).get(tab_id).and_then(|tab| tab.session.read(cx).block_text(block));
                if let Some(text) = text {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                }
            }
            TerminalGridIntent::Stop(block) => {
                // A finished (or missing) block is a no-op; a running one
                // takes `⌃C`.
                let running = self.host.read(cx).get(tab_id).is_some_and(|tab| {
                    tab.session.read(cx).blocks().get(block).is_some_and(|block| block.running())
                });
                if running {
                    let session = self.host.read(cx).get(tab_id).map(|tab| tab.session.clone());
                    if let Some(session) = session {
                        session.update(cx, |session, _| session.write(b"\x03"));
                    }
                }
            }
            TerminalGridIntent::Rerun(block) => self.rerun_block(tab_id, block, window, cx),
            TerminalGridIntent::Ask(block) => self.ask_about_block(tab_id, block, cx),
        }
    }

    /// Rerun a block's command: the same tab when it is idle, otherwise a
    /// new one — [`super::host::pick_tab`]'s rule (D43) with the
    /// originating tab as the candidate, through [`Self::run_in_picked`].
    fn rerun_block(&mut self, tab_id: &str, block: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.facts.project.clone() else { return };
        let command = self
            .host
            .read(cx)
            .get(tab_id)
            .and_then(|tab| tab.session.read(cx).blocks().get(block).map(|block| block.command.clone()))
            .filter(|command| !command.trim().is_empty());
        let Some(command) = command else { return };
        let pick = self.host.read(cx).pick_rerun(cx, &root, tab_id);
        self.run_in_picked(&root, pick, &command, true, window, cx);
    }

    /// Ask about a block: its command and capped ANSI-free output go to the
    /// root for the current session's composer draft as a quoted block —
    /// and stay there unsent. With no open session a toast says so.
    fn ask_about_block(&mut self, tab_id: &str, block: usize, cx: &mut Context<Self>) {
        if self.facts.origin.is_none() {
            self.overlays.update(cx, |overlays, cx| {
                overlays.toast("No open session", "Open a session to ask about this terminal output.", cx);
            });
            return;
        }
        let payload = self.host.read(cx).get(tab_id).and_then(|tab| {
            let session = tab.session.read(cx);
            let command = session.blocks().get(block).map(|block| block.command.clone())?;
            let output = session.block_text(block)?;
            Some((command, output))
        });
        if let Some((command, output)) = payload {
            cx.emit(TerminalDockEvent::Ask { command, output });
        }
    }

    /// The centre column's height: the window's — the flat shell's columns
    /// run top to bottom with no inset (docs/83 §3).
    fn centre_height(window: &Window) -> f32 {
        f32::from(window.bounds().size.height).max(0.0)
    }

    /// The dock's height now: the stored one, clamped into `[120px, 70% of
    /// the centre column]`.
    fn resting_height(window: &Window, cx: &App) -> f32 {
        let want = crate::layout::Layout::get(cx).terminal_height.unwrap_or(DOCK_DEFAULT_HEIGHT);
        clamp_dock_height(want, Self::centre_height(window))
    }

    /// The agent mark for one of the shown tabs (see [`Self::tab_provider`]).
    fn mark_for(&self, origin: Option<&str>) -> Option<Provider> {
        let record = origin.and_then(|session| self.facts.records.get(session)).map(String::as_str);
        self.tab_provider(origin, record)
    }
}

impl Focusable for TerminalDock {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Region for TerminalDock {
    /// The active tab's title, or "Terminal" with none.
    fn title(&self, cx: &App) -> SharedString {
        self.facts
            .project
            .as_ref()
            .and_then(|root| self.host.read(cx).active_for(root).map(|tab| SharedString::from(tab.title.clone())))
            .unwrap_or_else(|| "Terminal".into())
    }

    /// The dock's tab strip (`terminal_tabs`): one tab per terminal on the
    /// project with its agent mark and busy dot, ✕ to close, ＋ for a new
    /// one. The dock seats it in its own frame's header.
    fn actions(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let root = self.facts.project.clone()?;
        let host = self.host.read(cx);
        let active_id = host.active_for(&root).map(|tab| tab.id.clone());
        let mut tabs = Vec::new();
        let mut active_ix = 0;
        for tab in host.tabs_for(&root) {
            let provider = self.mark_for(tab.origin_session.as_deref());
            if Some(tab.id.as_str()) == active_id.as_deref() {
                active_ix = tabs.len();
            }
            let mut view = TermTab::new(tab.id.clone(), tab.title.clone());
            if tab.owner == TabOwner::Agent {
                view = view.agent(provider.unwrap_or(Provider::Muse));
            }
            if host.busy(cx, &tab.id) {
                view = view.busy(true);
            }
            tabs.push(view);
        }
        let select = cx.processor(|this: &mut Self, action: TerminalTabsAction, window, cx| match action {
            TerminalTabsAction::Select(index) => this.activate_tab(index, cx),
            TerminalTabsAction::Close(index) => this.close_tab(index, cx),
            TerminalTabsAction::New => this.new_terminal(window, cx),
        });
        Some(terminal_tabs("terminal-tabs", tabs, active_ix).on_action(select).into_any_element())
    }

    /// Never: the Browse page is laid out above the open dock
    /// ([`crate::browser::column_page_rect`]), so the dock draws beside the
    /// page, not over it.
    fn covers_page(&self, _cx: &App) -> bool {
        false
    }
}

impl Render for TerminalDock {
    /// The dock under the composer (D42): the library's `terminal_dock`
    /// frame over the tab strip ([`Region::actions`]) and the active tab's
    /// `terminal_grid`, or the empty state when the project has no tab.
    ///
    /// Opens and closes on the layout spring like the right pane: the outer
    /// height springs between the resting height and zero while the body
    /// inside keeps its resting height, so the grid is clipped — never
    /// re-laid-out — and the pty sees no resize storm mid-motion. The dock
    /// stays mounted while it collapses and draws nothing once the spring
    /// has settled shut (nothing at all with no project).
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let open = crate::layout::Layout::get(cx).terminal_open;
        let resting = Self::resting_height(window, cx);
        let target = if open { px(resting) } else { Pixels::ZERO };
        // Mid-drag the height feeds straight through: `spring_px` would chase
        // a moving target and the divider would lag the pointer. On release
        // the spring re-arms from the current height, so there is no jump.
        // `drag` is the dock's equivalent of the columns' `resizing`.
        let shown = if self.drag.is_some() {
            target
        } else {
            spring_px(("terminal-dock", "dock-height"), target, SpringKind::Layout, window, cx).max(Pixels::ZERO)
        };
        // Under a pixel left: the dock has closed.
        if !open && shown < px(scale::SP_1 / 2.0) {
            return div().into_any_element();
        }
        let Some(root) = self.facts.project.clone() else { return div().into_any_element() };
        let height = resting;
        let (active_tab, hint_provider) = {
            let host = self.host.read(cx);
            match host.active_for(&root) {
                Some(tab) => (Some((tab.id.clone(), tab.session.clone())), self.mark_for(tab.origin_session.as_deref())),
                None => (None, None),
            }
        };
        let header = self.actions(window, cx).unwrap_or_else(|| div().into_any_element());
        let body = active_tab.map(|(tab_id, session)| {
            let intent = cx.processor(move |this: &mut Self, intent: TerminalGridIntent, window, cx| {
                this.handle_intent(&tab_id, intent, window, cx);
            });
            // The grid draws its cursor focused — and blinks it — only while
            // the handle it was GIVEN is focused. Hand it the dock's own
            // handle, the one `track_focus` binds below and ⌃` focuses, or
            // the grid falls back to a handle nothing in this app ever
            // focuses and the cursor stays hollow and still forever.
            terminal_grid(&session).focus_handle(self.focus.clone()).on_intent(intent).into_any_element()
        });
        let maximised = height >= clamp_dock_height(f32::MAX, Self::centre_height(window)) - 0.5;
        let dock_action = cx.processor(move |this: &mut Self, action: TerminalDockAction, window, cx| match action {
            TerminalDockAction::Maximize => {
                let max = clamp_dock_height(f32::MAX, Self::centre_height(window));
                let height = if maximised { Some(DOCK_DEFAULT_HEIGHT) } else { Some(max) };
                crate::layout::Layout::update(cx, |layout| layout.terminal_height = height);
            }
            TerminalDockAction::Close => {
                crate::layout::Layout::update(cx, |layout| layout.terminal_open = false);
            }
            TerminalDockAction::NewTerminal => this.new_terminal(window, cx),
        });
        let dock = terminal_dock("terminal-dock", header, body)
            .hint(Self::hint_name(hint_provider))
            .maximized(maximised)
            .on_action(dock_action);
        let resize_start = cx.processor(|this: &mut Self, grab: f32, _, cx| {
            this.drag = Some((grab, crate::layout::Layout::get(cx).terminal_height.unwrap_or(DOCK_DEFAULT_HEIGHT)));
        });
        let resize_move = cx.processor(|this: &mut Self, at: f32, window, cx| {
            if let Some((grab, start)) = this.drag {
                // Live while the drag runs; the release writes it.
                let height = clamp_dock_height(start + (grab - at), Self::centre_height(window));
                crate::layout::Layout::stage(cx, |layout| layout.terminal_height = Some(height));
            }
        });
        let end_weak = cx.entity().downgrade();
        let resize_end = move |_: &mut Window, cx: &mut App| {
            end_weak
                .update(cx, |this: &mut Self, cx| {
                    this.drag = None;
                    crate::layout::Layout::save(cx);
                    cx.notify();
                })
                .ok();
        };
        let dock = dock.on_resize_start(resize_start).on_resize(resize_move).on_resize_end(resize_end);
        div()
            .h(shown)
            .w_full()
            .flex_none()
            .overflow_hidden()
            .key_context(gpui::KeyContext::parse(crate::app::TERMINAL_CONTEXT).unwrap_or_default())
            // ⌃C under the dock's context: SIGINT to the active tab. An
            // action, not a key handler, so the grid's own key routing is
            // untouched.
            .on_action(cx.listener(|this, _: &crate::app::TerminalSigint, _, cx| this.sigint(cx)))
            // No key handler here: the grid below is given this very
            // handle, so it IS the focus node and routes keys itself.
            // A handler here would see every keystroke a second time
            // as it bubbles, and the pty would receive `aa` for `a`.
            .track_focus(&self.focus)
            // The body keeps its resting height while the outer springs,
            // the right pane's `right_inner`: the grid measures this box,
            // so its bounds — and the pty size it reports — never move
            // mid-motion. The shrinking outer clips it instead.
            .child(div().h(px(height)).w_full().flex_none().child(dock))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dock_never_leaves_its_range() {
        assert_eq!(clamp_dock_height(260.0, 800.0), 260.0);
        assert_eq!(clamp_dock_height(10.0, 800.0), DOCK_MIN_HEIGHT);
        assert_eq!(clamp_dock_height(10_000.0, 800.0), 560.0);
        assert_eq!(clamp_dock_height(10_000.0, 100.0), DOCK_MIN_HEIGHT);
    }
}
