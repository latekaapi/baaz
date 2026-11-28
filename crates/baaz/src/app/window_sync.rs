//! The window's lifecycle off the frame (task 4.9, audit A F19).
//!
//! A per-frame hook used to run first in every `Harness::render`: the
//! overlay windows, the focus restore, the window title, the boot gates, the
//! resize settle, the sweeps — each polled per frame, so a quiet window (no
//! frame) stalled them and every frame paid for all of them. Each now runs
//! from what causes it:
//!
//! * **the settle** ([`Harness::settle`]) — the overlay stack's focus
//!   restore and the palette's keyboard, the provider statuses, the scripted
//!   boot and steps, the composer's one-shot focus — runs once at the end of
//!   any effect cycle in which the root, the overlay stack, the Browse view,
//!   the right pane, the connect screen or the layout changed, or the window
//!   was (de)activated;
//! * **the floats** ([`Harness::sync_floats`]) — the overlay, corner and
//!   Page float windows over the native page — follow the settle, the Browse
//!   view's own events (its pages, its occlusion), the window's bounds and
//!   the browser chat's turn, one frame later: after the layout that moved
//!   what they anchor to;
//! * **the title** follows the session list's events, the switch path and
//!   the project path ([`Harness::queue_title`]);
//! * **activation** re-probes the provider statuses and settles a resize
//!   drag the pointer left armed ([`Harness::on_window_activation`]);
//! * **focus lost** — the focused element unmounted with the overlay that
//!   drew it — hands the keyboard back at the end of that very frame;
//! * **the sweeps** (`resize-sweep:`, `transcript-scroll-sweep:`) pace
//!   themselves one frame at a time from the step that starts them.
//!
//! What `render` still does is compose: it reads state and builds elements,
//! and its one remaining instrument is the env-gated frame trace row
//! ([`Harness::trace_root_frame`]). `tests/render_ratchet.rs` holds it there.
//!
//! Part of [`Harness`]; see [`crate::app`] for what the entity owns.

use super::*;

/// The window's lifecycle state between events: its handle, the two queued
/// passes, the composer's one-shot focus and the observations that drive
/// them. One field on [`Harness`].
#[derive(Default)]
pub(crate) struct WindowSync {
    /// This window's handle: overlay intents and the deferred passes run
    /// against it. Set when the root is built.
    pub(crate) handle: Option<gpui::AnyWindowHandle>,
    /// The composer takes the keyboard at the next settle (a one-shot arm:
    /// a landed switch, a closed sheet, a picked project).
    pub(crate) focus_composer: bool,
    /// A settle is queued for the end of this effect cycle.
    settle_queued: bool,
    /// A floats sync is queued for the frame after the next layout.
    floats_queued: bool,
    /// The browser chat's runtime, watched while it is the browser chat:
    /// its turn and its asks move the agent card.
    browser_chat_watch: Option<(gpui::EntityId, [Subscription; 2])>,
    /// The observations behind the settle, the floats and the title. Made
    /// once when the root is built, never cleared.
    observers: Vec<Subscription>,
}

impl WindowSync {
    /// Before the root is built: no handle yet, and the composer armed to
    /// take the keyboard once a session lands.
    pub(crate) fn new() -> Self {
        WindowSync { focus_composer: true, ..Default::default() }
    }
}

impl Harness {
    /// Wire the window's lifecycle to its causes (see the module doc). Runs
    /// once, from [`Harness::new`], with the window that hosts the root.
    pub(super) fn install_window_sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.window_sync.handle = Some(window.window_handle());
        let root = cx.entity();
        let mut observers = vec![
            // The root's own facts moved (an action, a landed reply, a pump
            // pass that changed the agent's page work).
            cx.observe(&root, |this, _, cx| this.queue_settle(cx)),
            // What is open, and what covers the page.
            cx.observe(&self.overlays, |this, _, cx| this.queue_settle(cx)),
            cx.observe(&self.browser, |this, _, cx| this.queue_settle(cx)),
            cx.observe(&self.right_pane, |this, _, cx| this.queue_settle(cx)),
            cx.observe(&self.connect_screen, |this, _, cx| this.queue_settle(cx)),
            // Mode, sidebar, right pane and theme all live in the layout.
            cx.observe_global::<layout::LayoutStore>(|this, cx| {
                this.note_pane_page_live(cx);
                this.queue_settle(cx);
            }),
            // The window moved or resized: the floats follow its frame.
            cx.observe_window_bounds(window, |this, _, cx| this.queue_floats(cx)),
            cx.observe_window_activation(window, |this, window, cx| this.on_window_activation(window, cx)),
            // The focused element unmounted (an overlay closed under it) or
            // the window lost its focus: hand the keyboard back now.
            cx.on_focus_lost(window, |this, window, cx| this.on_focus_lost(window, cx)),
        ];
        // The title is the open session's row against its project.
        let list = self.session_list.clone();
        observers.push(cx.subscribe(&list, |this, _, _: &crate::session_list::SessionListEvent, cx| this.queue_title(cx)));
        self.window_sync.observers = observers;
        // Where the window starts: the keyboard on the root (nothing is
        // focused yet), the pane page's wheel claim, the activation edge,
        // the title, and a first settle for the boot gates.
        window.focus(&self.focus_root, cx);
        self.note_pane_page_live(cx);
        crate::provider_status::note_window_active(window.is_window_active());
        self.queue_title(cx);
        self.queue_settle(cx);
    }

    /// Queue one settle for the end of this effect cycle (see
    /// [`Self::settle`]); a second ask before it runs is the same settle.
    pub(crate) fn queue_settle(&mut self, cx: &mut Context<Self>) {
        if std::mem::replace(&mut self.window_sync.settle_queued, true) {
            return;
        }
        self.run_on_main(cx, |this, window, cx| {
            this.window_sync.settle_queued = false;
            this.settle(window, cx);
        });
    }

    /// What follows a change to what the window shows, run once per effect
    /// cycle that changed it — never from a frame. In the order the frame
    /// used to apply them, so the last word on the keyboard stays the same:
    ///
    /// * the lifecycle ([`Self::settle_lifecycle`]): the provider statuses,
    ///   the scripted boot and steps, the composer's one-shot focus;
    /// * the URL field's focus a person's own Browser open owes it (Z7a2);
    /// * the overlay stack's keyboard: the palette takes it the moment it
    ///   opens (its query field where it has one), and an entry that closed
    ///   gives it back where it was ([`Self::restore_overlay_focus`]);
    /// * the browser chat's own browser tools are accepted for it, and the
    ///   floats follow it all on the next frame.
    ///
    /// Nothing here notifies the root: a settle that changed nothing ends
    /// here (one that moved the statuses notifies through the model).
    pub(crate) fn settle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settle_lifecycle(window, cx);
        // The person's own Browser open owes the URL field its focus (Z7a2):
        // the page is built or reused here, with the window, where the pane
        // stands open on it — before the overlay restore below, which has
        // the last word when a palette row opened the pane.
        if self.browser.read(cx).browser_url_focus_armed && !self.connect_open(cx) && self.page_strip_open(cx) {
            self.ensure_browser_person(window, cx);
        }
        // The palette takes the keyboard as it opens, so the arrows and the
        // return reach it rather than the composer under it: its query
        // field where it has one (the keys bubble from the field to the
        // palette's own menu context), else the palette itself. The attach
        // picker takes it the same way. The overlay window focuses a hosted
        // one itself.
        let modal = self.palette_view(cx).map(|view| view.focus_handle(cx)).or_else(|| self.attach_picker_view(cx).map(|view| view.focus_handle(cx)));
        if let (false, Some(focus)) = (self.modal_overlay_wanted(cx), modal) {
            if !focus.is_focused(window) {
                window.focus(&focus, cx);
            }
        }
        self.restore_overlay_focus(window, cx);
        self.auto_accept_browser_tools(cx);
        self.queue_floats(cx);
    }

    /// The settle's lifecycle half: the provider statuses, the scripted boot
    /// and steps, the composer's one-shot focus.
    fn settle_lifecycle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // The connect screen owns the whole window while it is up: the
        // provider model follows the status cache (the screen observes it
        // and redraws itself) and the session lifecycle below waits behind
        // it. A signed-out shell runs the lifecycle like a signed-in one;
        // only the offline and replay captures that boot signed out wait.
        if self.connect_open(cx) {
            self.sync_provider_state(cx);
            // Window verbs (the setup sheet, the footer menu) script the
            // connect screen's captures; nothing else runs here.
            #[cfg(feature = "probe")]
            if crate::steps::all_window_steps(&self.args.steps) {
                self.maybe_run_steps(window, cx);
            }
            return;
        }
        if !matches!(self.auth, Auth::SignedIn(_)) && (self.args.offline || self.args.replay.is_some()) {
            return;
        }
        self.sync_provider_state(cx);
        // The scripted boot, on every settle until it fires: the first
        // session for `--session`/`--send`/`--steps` opens without waiting
        // for `session/list`, then the script runs once the session is open.
        // Both are idempotent (consumed args, drained list), so the repeated
        // call is a gate, not a loop.
        self.ensure_boot_session(window, cx);
        #[cfg(feature = "probe")]
        self.maybe_run_steps(window, cx);
        // A deterministic capture never takes keyboard focus: a focused
        // composer paints the textarea's blinking caret, which lands on a
        // different phase every run.
        // Q4b: never re-focus the hidden session's composer while its
        // replacement loads — the click consumed the arm, and this drops
        // any arm raised mid-switch. The landing view arms its own focus
        // through `activate`.
        if std::mem::take(&mut self.window_sync.focus_composer)
            && !crate::runtime::captures()
            && self.pending_switch_dest(cx).is_none()
        {
            if let Some(view) = self.active.clone() {
                view.update(cx, |view, cx| view.focus_composer(window, cx));
            }
        }
    }

    /// Move the keyboard to the composer at the next settle: a landed
    /// switch, a closed sheet or rename, a picked project.
    pub(crate) fn arm_composer_focus(&mut self, cx: &mut Context<Self>) {
        self.window_sync.focus_composer = true;
        self.queue_settle(cx);
    }

    /// Give the keyboard back after an overlay closed (plan 4.6), from the
    /// restore the stack queued. An overlay that closes unmounts the element
    /// it had focused, and the window goes on reporting that handle as
    /// focused while gpui resolves a keystroke against the dispatch tree's
    /// root — above this view's div, so every `on_action` mounted here was
    /// skipped ("it works once, then it doesn't": one ⌘K open-and-close
    /// left ⌘B dead too).
    ///
    /// An entry that took the keyboard remembers what held it in the frame
    /// it opened. A modal or a menu always gives it back (whatever the
    /// window reports focused may be unmounted); the rest only while it is
    /// free — still on what the overlay held, on the root, or nowhere — so
    /// a composer menu closed by a click that focused another field (the
    /// sidebar search, a rename, the URL bar) leaves the keyboard in that
    /// field. Captures park on the root, so a capture never depends on
    /// where the keyboard was. Answers whether a restore was queued.
    fn restore_overlay_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(restore) = self.overlays.update(cx, |overlays, _| overlays.stack.take_restore()) else {
            return false;
        };
        let focused = window.focused(cx);
        let free = focused.is_none() || focused == restore.held || focused.as_ref() == Some(&self.focus_root);
        if free || restore.force {
            let to = restore.to.filter(|_| !crate::runtime::captures()).unwrap_or_else(|| self.focus_root.clone());
            window.focus(&to, cx);
        }
        true
    }

    /// The frame just drawn has nothing focused: the overlay that held the
    /// keyboard unmounted, or the window lost it. The stack's restore when
    /// one is queued, else the root — never nowhere, or the root actions
    /// stop resolving.
    fn on_focus_lost(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.restore_overlay_focus(window, cx) && window.focused(cx).is_none() {
            window.focus(&self.focus_root, cx);
        }
    }

    /// The window was activated or deactivated.
    fn on_window_activation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let active = window.is_window_active();
        // A regained window focus re-probes the provider statuses (Y4):
        // edge-triggered inside, at most every 15s per provider.
        crate::provider_status::note_window_active(active);
        if active {
            // The default-browser cache (B2a-H7b): render reads the cache
            // only, so the LaunchServices lookup runs at launch and on every
            // regained focus (a switch made in System Settings shows without
            // a restart).
            self.browser.update(cx, |browser, cx| browser.refresh_default_browser_cache(cx));
            // The keymap file (docs/82 8.8): an edit made outside Baaz —
            // Settings › Shortcuts' "Open keymap" — installs on the way
            // back, and the Shortcuts rows show it. One stat per regain.
            if crate::keymap::changed_since_install() {
                self.settings_page.update(cx, |page, cx| page.reinstall_keymap(cx));
            }
        } else {
            // A release outside the window never reaches the overlay: a drag
            // still armed as the window goes inactive is over.
            self.settle_resize_on_deactivate(cx);
        }
        self.queue_settle(cx);
    }

    /// The pane's page takes a Shift + sideways swipe as the strip's (docs/43
    /// §3): what the native wheel router claims by, from the layout.
    fn note_pane_page_live(&self, cx: &App) {
        #[cfg(feature = "cef")]
        crate::browser::PANE_PAGE_LIVE.with(|live| {
            let layout = crate::layout::Layout::get(cx);
            live.set(
                layout.mode == crate::layout::Mode::Sessions
                    && layout.right_open
                    && crate::layout::right_kind(layout) == crate::layout::RightKind::Browser,
            )
        });
        #[cfg(not(feature = "cef"))]
        let _ = cx;
    }

    /// Queue one floats sync for the frame after the next layout (see
    /// [`Self::sync_floats`]): the anchors it reads — the page rects, the
    /// window's frame — are the ones that frame laid out.
    pub(crate) fn queue_floats(&mut self, cx: &mut Context<Self>) {
        if std::mem::replace(&mut self.window_sync.floats_queued, true) {
            return;
        }
        self.run_on_main(cx, |_, window, cx| {
            cx.on_next_frame(window, |this, window, cx| {
                this.window_sync.floats_queued = false;
                this.sync_floats(window, cx);
            });
        });
    }

    /// The windows Browse floats over the native page follow what is open
    /// and where: the overlay (menus, the Field, the hosted modal), the
    /// corner (toasts) and the Page float (the agent
    /// line, the notes panel). Each opens, moves, repaints or closes only
    /// when what it shows moved; window changes are deferred past this
    /// update.
    pub(crate) fn sync_floats(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_overlay(window, cx);
        self.sync_corner(window, cx);
        self.sync_page_float(window, cx);
    }

    /// The browser chat changed (one set, or none): watch its runtime, whose
    /// turn and asks move the agent card (and whose browser tools are
    /// accepted for it, [`Self::auto_accept_browser_tools`]).
    pub(crate) fn watch_browser_chat(&mut self, cx: &mut Context<Self>) {
        let runtime = self.registry.browser_chat().map(|view| view.read(cx).runtime().clone());
        let watched = self.window_sync.browser_chat_watch.as_ref().map(|(id, _)| *id);
        if runtime.as_ref().map(|rt| rt.entity_id()) == watched {
            return;
        }
        self.window_sync.browser_chat_watch = runtime.map(|runtime| {
            // The runtime names its own changes as events; a write made
            // through `as_mut` (the view's) notifies it instead.
            let events = cx.subscribe(&runtime, |this, _, _: &crate::session::RuntimeEvent, cx| {
                this.auto_accept_browser_tools(cx);
                this.queue_floats(cx);
            });
            let writes = cx.observe(&runtime, |this, _, cx| {
                this.auto_accept_browser_tools(cx);
                this.queue_floats(cx);
            });
            (runtime.entity_id(), [events, writes])
        });
        self.queue_floats(cx);
    }

    /// Set the window's title from the open session, at the end of this
    /// effect cycle: the list changed, a switch landed, the project moved.
    pub(crate) fn queue_title(&mut self, cx: &mut Context<Self>) {
        self.run_on_main(cx, |this, window, cx| this.sync_window_title(window, cx));
    }

    /// The window's title is the session's, so a person with three baaz
    /// windows open can tell them apart in Mission Control. Set only when it
    /// differs from what the window wears.
    pub(crate) fn sync_window_title(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let title = self.window_title(cx);
        if window.window_title() != title {
            window.set_window_title(&title);
        }
    }

    /// The one idle-time Browser warmup (B7fix2), the first time the shell
    /// is up with a session open: a restore that left the pane open on
    /// Browser builds its webview off the frame, so no frame constructs
    /// one. Once only; never in tests or deterministic captures (the
    /// scripted page needs no warmup and captures build nothing unasked);
    /// never focused. Waits for an active session so it warms that
    /// session's browser, not a home one.
    pub(crate) fn maybe_idle_prewarm(&mut self, cx: &mut Context<Self>) {
        if self.browser.read(cx).browser_idle_warmed || !self.shell_up(cx) || self.active.is_none() {
            return;
        }
        self.browser.update(cx, |browser, _| browser.browser_idle_warmed = true);
        if !self.browser.read(cx).fake && !crate::runtime::captures() && self.args.screenshot.is_none() {
            self.defer_browser_prewarm(cx);
        }
    }

    #[cfg(feature = "probe")]
    /// One tick of a `transcript-scroll-sweep:` (scripting only): the
    /// transcript's twin of the sidebar pane's own sweep, one push into the
    /// active session's accumulator (`SessionView::push_wheel`) per frame.
    /// The sweep rides the tick chain itself; nothing else holds it.
    pub(crate) fn transcript_sweep_tick(
        &mut self,
        mut sweep: crate::resize::ScrollSweep,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dy) = sweep.advance() else { return };
        if let Some(view) = self.active.clone() {
            view.update(cx, |view, cx| {
                view.push_wheel(px(dy));
                cx.notify();
            });
        }
        cx.on_next_frame(window, move |this, window, cx| this.transcript_sweep_tick(sweep, window, cx));
    }

    /// The frame trace's per-tick row, from `render` — the one instrument it
    /// keeps (env-gated; a disabled build pays one flag check). Here rather
    /// than in `render_transcript` because the root renders once per tick
    /// regardless of which subtree rebuilt — the true per-display-tick hook,
    /// where a centre-scoped trace went silent through a sidebar-only or
    /// resize-only gesture. The pane's list top and `resize.width` are only
    /// read. `take_trace_rehint` is one flag shared with the
    /// `resize-sweep:` tick's log: on a tick where both run, the sweep's
    /// drain wins and this row reads `rehint=0` — irrelevant to the real
    /// CGEvent-driven measurements this instrument exists for.
    pub(crate) fn trace_root_frame(&mut self, cx: &mut Context<Self>) {
        if !session::frame_trace_enabled() {
            return;
        }
        let top = self.sidebar_list_top(cx);
        let (transcript, rehint) = self
            .active
            .clone()
            .map(|view| view.update(cx, |view, _| (Some(view.trace_tick()), view.take_trace_rehint())))
            .unwrap_or((None, false));
        session::note_root_frame_trace(
            top.item_ix,
            f32::from(top.offset_in_item),
            self.sidebar_width(cx),
            self.sidebar_resizing(cx),
            transcript,
            self.sidebar_gesture_active(cx),
            rehint,
        );
    }
}

#[cfg(feature = "probe")]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::sessions::tests::support::{lane_args, lane_restore, lane_state};

    /// The Harness as the window's root, past the login screen, settled.
    fn rooted<'a>(cx: &'a mut gpui::TestAppContext, dir: &std::path::Path) -> (Entity<Harness>, &'a mut gpui::VisualTestContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let mut args = lane_args(dir);
        args.login = crate::LoginSample::SignedIn;
        let (baaz, vc) = cx.add_window_view(|window, cx| Harness::new(args, crate::runtime::CaptureToken::default(), window, cx));
        vc.run_until_parked();
        (baaz, vc)
    }

    /// Task 4.9, done-when: the title follows the session list's own event,
    /// with no frame drawn for it. A project made current and announced by
    /// `SessionListEvent::Changed` alone renames the window; a listener
    /// subscribed after the root's sees, at the end of the same effect cycle
    /// (after the title pass, before any draw), that the root has not
    /// rendered. (The sidebar column then redraws for the same event; no
    /// render can set the title, `tests/render_ratchet.rs`.)
    #[gpui::test]
    fn a_list_event_sets_the_title_with_no_frame(cx: &mut gpui::TestAppContext) {
        let state = lane_state("title-event");
        let other = state.2.join("Other Project");
        std::fs::create_dir_all(&other).expect("the second project's root");
        let (baaz, vc) = rooted(cx, &state.2);
        let boot = vc.window_title();
        assert!(boot.as_deref().is_some_and(|title| !title.is_empty()), "the boot title was set: {boot:?}");
        let list = vc.update(|_, cx| baaz.read(cx).session_list.clone());
        let renders_before_draw = std::rc::Rc::new(std::cell::Cell::new(None));
        let seen = renders_before_draw.clone();
        let _after_root = vc.update(|_, cx| {
            cx.subscribe(&list, move |_, _: &crate::session_list::SessionListEvent, cx| {
                let seen = seen.clone();
                cx.defer(move |_| seen.set(Some(crate::sidebar_view::take_baaz_root_renders())));
            })
        });
        crate::sidebar_view::take_baaz_root_renders();
        vc.update(|_, cx| {
            baaz.update(cx, |harness, cx| {
                let id = harness.session_list.update(cx, |list, _| list.projects.add(&other).id.clone());
                harness.session_list.update(cx, |list, _| list.projects.current = Some(id.clone()));
                harness.current_project = Some(id);
                harness.session_list.update(cx, |_, cx| cx.emit(crate::session_list::SessionListEvent::Changed));
            })
        });
        vc.run_until_parked();
        assert_eq!(vc.window_title().as_deref(), Some("Other Project"), "the list's event set the title");
        assert_eq!(renders_before_draw.get(), Some(0), "the title pass ran before any frame was drawn");
        lane_restore(state);
    }

    /// Task 4.9, done-when: the overlay windows follow the overlay stack, not
    /// the frame. A Browse menu opened on the stack (its owner notified, not
    /// the root): the frame that draws the change does not open its window
    /// (render syncs nothing); the next frame's sync does, before that frame
    /// draws anything of the root's.
    #[gpui::test]
    fn a_menu_on_the_stack_opens_its_window_off_the_frame(cx: &mut gpui::TestAppContext) {
        let state = lane_state("panel-sync");
        let (baaz, vc) = rooted(cx, &state.2);
        vc.update(|_, cx| crate::layout::Layout::update(cx, |layout| layout.mode = crate::layout::Mode::Browse));
        vc.run_until_parked();
        // Whatever the mode switch left pending for the next frame goes now.
        vc.update(|window, cx| window.simulate_next_frame(cx));
        vc.run_until_parked();
        let open_kind = |cx: &App| baaz.read(cx).browser.read(cx).overlay.as_ref().map(|overlay| overlay.kind());
        assert_eq!(vc.update(|_, cx| open_kind(cx)), None, "nothing floats before the menu");
        vc.update(|_, cx| {
            let overlays = baaz.read(cx).overlays.clone();
            overlays.update(cx, |overlays, cx| {
                let menu = aui::screens::sidebar::BrowseMenu {
                    target: aui::screens::sidebar::BrowseMenuTarget::Folder("f1".into()),
                    at: gpui::point(px(40.0), px(40.0)),
                };
                overlays.push(crate::overlay_stack::Entry::new(crate::overlay_stack::Layer::BrowseMenu(menu)), cx);
                cx.notify();
            });
        });
        vc.run_until_parked();
        assert_eq!(vc.update(|_, cx| open_kind(cx)), None, "the frame that drew the change opened nothing");
        crate::sidebar_view::take_baaz_root_renders();
        // The next frame's callbacks, read before that update draws.
        let (opened, renders) = vc.update(|window, cx| {
            window.simulate_next_frame(cx);
            (open_kind(cx), crate::sidebar_view::take_baaz_root_renders())
        });
        assert_eq!(opened, Some(crate::overlay::OverlayKind::BrowseMenu), "the next frame's sync opened the menu's window");
        assert_eq!(renders, 0, "with no render of the root's");
        lane_restore(state);
    }
}
