//! One open session: the fold, the composer, and the centre pane.
//!
//! This is the entity the spec calls `SessionView` (§2.3). It owns a
//! [`MuseFold`] — the fold is per-connection in principle, but one session is
//! open at a time in this phase and keeping it here is what makes "close the
//! session, drop its transcript" free — plus the composer draft, the scroll
//! position, and which cards the person folded shut.
//!
//! Everything it renders is a pure function of the fold's `Session` and
//! `SideState`, recomputed every frame. Nothing is optimistic except the word
//! "Working…", which the first `turn/started` immediately replaces with the
//! real turn.
//!
//! # The composer controls (spec §5 phase 3)
//!
//! Every one of them is a round trip, and the chip is drawn from what came
//! back, never from what was sent:
//!
//! | control | command | what changes the chip |
//! |---|---|---|
//! | model | `session/setModel` | `session/modelChanged` |
//! | approval mode | `session/setApprovalMode` | `session/approvalModeChanged` |
//! | context meter | — | `session/contextUsage` |
//! | compaction | `session/compact` | the `compaction` item's marker |
//! | queue strip | `turn/start` (`ifBusy: queue`) | `SideState::queued` |
//! | steer | `turn/steer` | the running turn's own items |
//!
//! **Reasoning effort is the exception**: MSP carries `reasoningEffort` on
//! `turn/start` and `turn/steer` and nowhere else — there is no echo on the
//! `userMessage` item and none on `turn/started` — so the effort chip shows the
//! client's own value. It is the one control with no server reflection, and
//! `docs/CHANGELOG.md` says so.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aui::composer::{
    command_menu, composer, composer_state_rows, effort_menu, mention_picker, mode_menu,
    model_menu, plus_menu, queue_strip, CommandItem, CommandSection, ComposerChip,
    ComposerChipAnchor, ComposerChipKind, ComposerIntent, MentionIcon, MentionItem, MentionSection,
    PickerRow, PlusMenuItem, QueueIntent, QueueStripRow,
};
use aui::data::{ContextMeterState, ContextPressure};
use aui::feedback::{banner, BannerActionStyle, BannerKind, BannerRun};
use aui::overlay::popover_layer;
use aui::transcript::{status_row, StatusLead};
use aui_icons::IconName;
use aui_protocol::{Block, PermissionMode, PlanState, ReasoningEffort, Session};
use aui_tokens::scale;
use gpui::{
    div, prelude::*, px, AnyElement, ClipboardEntry, Context, Entity, EventEmitter, ExternalPaths,
    FocusHandle, Focusable, ScrollHandle, SharedString, Task, Window,
};
use gpui_kit::base::input::{InputEvent, Position, TextareaState};
use gpui_kit::base::{h_flex, v_flex};
use muse_adapter::MuseFold;
use muse_client::schema::{
    ApprovalMode, ContextPressureLevel, ContextUsage, ModelCatalogEntry, ModelListParams,
    ModelSelection, SessionCompactParams, SessionSetApprovalModeParams, SessionSetModelParams,
    TurnInputPart, TurnInterruptParams, TurnStartDisposition, TurnStartParams, TurnStartResult,
    TurnSteerParams, TurnUnqueueParams, ViewPageParams,
};
use muse_client::{new_command_id, MuseClient, MuseError, MuseEvent};

use crate::conn::{self, Severity};
use crate::overlays::{Command, Menu, MenuKind, Overlays, EFFORTS, MODES};
use crate::transcript::{self, Folds, PlanAction};
use crate::{files, history, images, plan, skills};

/// How often the "Working… 12 s" row re-reads the clock.
const TICK: Duration = Duration::from_millis(250);
/// `view/page` takes 1–1000; the transport uses the ceiling and so does the
/// history backfill.
const PAGE_LIMIT: u32 = 1000;
/// The transcript's own padding, matching the assistant screen's `.tr`.
const TRANSCRIPT_PAD_X: f32 = scale::SP_7;
const TRANSCRIPT_PAD_TOP: f32 = scale::SP_5;
/// How close to the bottom counts as "reading the tail" for auto-scroll.
const TAIL_SLACK: f32 = 48.0;
/// The gap the caret popovers leave above the composer, matching the
/// library's own `.pop{margin-bottom:8px}`.
const POPOVER_GAP: f32 = 8.0;
/// How tall a caret popover may grow before it scrolls. The `/` menu lists
/// every client command **and** every installed skill, and a workspace with
/// twenty skills would otherwise reach past the top of the window.
const POPOVER_MAX_H: f32 = 560.0;
/// How many skill rows the `/` menu offers at once.
const SKILL_ROWS: usize = 8;

/// What the session needs the application to do about something.
pub enum SessionEvent {
    /// Show this in a modal dialog: an identity or protocol failure.
    Dialog {
        /// Dialog heading.
        title: String,
        /// The body paragraph.
        detail: String,
    },
    /// A turn failed with a credential-shaped message: go to the login screen.
    SignedOut {
        /// What the provider said, for the dialog that offers to sign in again.
        message: String,
    },
    /// The child exited; the application owns the reconnect.
    Closed,
    /// `/clear`: start a new session in this workspace.
    NewSession,
    /// `/logout`.
    Logout,
    /// `/status` or `/usage`: the application owns the dialog stack.
    Status {
        /// The lines of the dialog body, already formatted.
        detail: String,
    },
}

/// A turn the server says is running.
struct Running {
    turn_id: String,
    started: Instant,
}

/// Why a queued row is being unqueued, which decides what happens to its text
/// when `turn/unqueued` lands.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Unqueue {
    /// Put the text back in the composer.
    Edit,
    /// Drop it.
    Remove,
    /// Interject it into the running turn instead.
    Steer,
}

/// One open Muse session.
pub struct SessionView {
    /// The Muse session id this view follows.
    pub session_id: String,
    fold: MuseFold,
    client: Arc<MuseClient>,
    /// `meta`, or `echo` under `HARNESS_PROVIDER=echo`.
    provider_id: String,
    /// The workspace the session runs in, for the header and the empty state.
    workspace: String,
    composer: Entity<TextareaState>,
    /// The window's floating state: menus, toasts, the modal, and the lists the
    /// menus are built from. Shared with [`crate::app::Harness`], which renders
    /// the halves that are not anchored to the composer.
    overlays: Entity<Overlays>,
    /// Cards the person folded away from their default.
    toggled: HashSet<String>,
    scroll: ScrollHandle,
    /// Set by every event that changed the transcript; the next frame consumes
    /// it and scrolls to the tail if the reader was already there.
    follow: bool,
    running: Option<Running>,
    /// A `turn/start` is in flight and no `turn/started` has arrived yet.
    submitting: bool,
    /// History is still being paged in behind the live stream.
    loading_history: bool,
    /// The inline banner over the composer: one recoverable command error.
    banner: Option<String>,
    /// A prompt handed back by a retraction, waiting for a frame with a
    /// `Window` in it to reach the composer.
    pending_prompt: Option<String>,
    /// The model catalog, fetched when the picker opens. A snapshot: MSP has no
    /// catalog subscription.
    models: Vec<ModelCatalogEntry>,
    /// The session's reasoning effort. `None` is "Default", which omits the
    /// field; client-side, because nothing on the wire reflects it back.
    effort: Option<ReasoningEffort>,
    /// Plan mode, a client-side overlay (spec §3.1).
    plan: bool,
    /// The approval mode plan mode displaced, restored on Accept or Reject.
    plan_previous_mode: Option<PermissionMode>,
    /// The turn started in plan mode, whose reply becomes a `Block::Plan`.
    plan_turn: Option<String>,
    /// Monotonic id source for the client-authored plan turns.
    plan_seq: u64,
    /// Images waiting to go out with the next turn.
    images: Vec<images::Image>,
    /// Monotonic id source for image chips.
    image_seq: u64,
    /// A drag is over the window, so the drop overlay is up.
    dragging: bool,
    /// The `+` menu.
    plus_open: bool,
    /// Where the composer is in this workspace's prompt history.
    history: history::Cursor,
    /// The canonical workspace key the history file is written under.
    workspace_key: String,
    /// Queued turns whose unqueue is in flight, and why.
    unqueueing: HashMap<String, Unqueue>,
    /// Pins the context meter's breakdown open, for a scripted capture.
    meter_open: bool,
    /// A synthetic `session/contextUsage`, for a scripted capture of a pressure
    /// state the echo provider cannot reach.
    fake_context: Option<ContextUsage>,
    focus: FocusHandle,
    tasks: Vec<Task<()>>,
    /// Held only while a turn runs, so the elapsed time advances.
    ticker: Option<Task<()>>,
    _subscriptions: Vec<gpui::Subscription>,
}

impl EventEmitter<SessionEvent> for SessionView {}

impl SessionView {
    /// A view over `session_id`. Nothing is loaded yet: the caller either just
    /// started the session or is about to [`SessionView::backfill`] it.
    pub fn new(
        session_id: String,
        client: Arc<MuseClient>,
        provider_id: String,
        workspace: String,
        overlays: Entity<Overlays>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let composer = cx.new(|cx| composer_state_rows("Ask Muse, or type / for commands", 1, 8, window, cx));
        // Typing is what opens, filters and closes the caret popovers, and what
        // takes the composer off the history it was walking.
        let subscription = cx.subscribe(&composer, |this: &mut Self, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.on_draft_changed(cx);
            }
        });
        let workspace_key = workspace.clone();
        Self {
            session_id,
            fold: MuseFold::new(),
            client,
            provider_id,
            workspace,
            composer,
            overlays,
            toggled: HashSet::new(),
            scroll: ScrollHandle::new(),
            follow: true,
            running: None,
            submitting: false,
            loading_history: false,
            banner: None,
            pending_prompt: None,
            models: Vec::new(),
            effort: None,
            plan: false,
            plan_previous_mode: None,
            plan_turn: None,
            plan_seq: 0,
            images: Vec::new(),
            image_seq: 0,
            dragging: false,
            plus_open: false,
            history: history::Cursor::new(history::read(&workspace_key)),
            workspace_key,
            unqueueing: HashMap::new(),
            meter_open: false,
            fake_context: None,
            focus: cx.focus_handle(),
            tasks: Vec::new(),
            ticker: None,
            _subscriptions: vec![subscription],
        }
    }

    /// The folded transcript, once anything has arrived.
    pub fn session(&self) -> Option<&Session> {
        self.fold.session(&self.session_id)
    }

    /// The last view cursor the fold observed, which is what a reconnect's
    /// `session/resume` needs.
    pub fn last_cursor(&self) -> Option<String> {
        self.fold.side(&self.session_id).map(|s| s.last_cursor.clone()).filter(|c| !c.is_empty())
    }

    /// The model id the session is actually on, for the composer chip.
    pub fn model(&self) -> SharedString {
        self.fold
            .side(&self.session_id)
            .and_then(|s| s.model.as_ref().map(|m| m.model_id.clone()))
            .or_else(|| self.session().map(|s| s.model.clone()).filter(|m| !m.is_empty()))
            .map(SharedString::from)
            .unwrap_or_else(|| SharedString::from(self.provider_id.clone()))
    }

    /// The context meter's state, from `session/contextUsage` joined with the
    /// session's cumulative counters.
    ///
    /// A session that has not reported context yet still gets a meter: the
    /// cumulative totals are real, and a meter that appeared only after the
    /// first turn would move the toolbar under the person's hand.
    pub fn context(&self) -> ContextMeterState {
        let side = self.fold.side(&self.session_id);
        let cumulative = side.map(|s| s.cumulative.clone()).unwrap_or_default();
        let usage = self.fake_context.clone().or_else(|| side.and_then(|s| s.context.clone()));
        let (used, window, pressure) = match usage {
            Some(usage) => (usage.used_tokens, usage.window_tokens, pressure(&usage.pressure)),
            None => (cumulative.total_tokens, None, ContextPressure::Normal),
        };
        ContextMeterState {
            used_tokens: used,
            window_tokens: window,
            pressure,
            prompt_tokens: cumulative.prompt_tokens,
            output_tokens: cumulative.output_tokens,
            total_tokens: cumulative.total_tokens,
        }
    }

    /// The approval mode chip's label.
    pub fn mode_label(&self) -> &'static str {
        self.mode().label()
    }

    /// The approval mode the server says the session is in.
    fn mode(&self) -> PermissionMode {
        self.session().map(|s| s.mode).unwrap_or_default()
    }

    /// Whether a turn is running, which is what the send button morphs on.
    pub fn busy(&self) -> bool {
        self.running.is_some() || self.submitting
    }

    /// Whether the composer is empty, which is what Escape branches on.
    pub fn draft_is_empty(&self, cx: &gpui::App) -> bool {
        self.composer.read(cx).value().trim().is_empty()
    }

    /// Whether plan mode is on, for the app's Shift+Tab.
    pub fn plan_mode(&self) -> bool {
        self.plan
    }

    /// Point the view at the respawned child after a reconnect. The fold and
    /// the transcript are untouched: the resume streamed only the suffix.
    pub fn reconnected(&mut self, client: Arc<MuseClient>) {
        self.client = client;
    }

    /// Put text in the composer. Only the scripted `--send` uses this; a person
    /// types.
    pub fn set_draft(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        // The caret goes where a person typing would have left it — at the end
        // — because that is what decides whether a `/` or `@` opens a popover.
        let position = position_of(&text, text.len());
        self.composer.update(cx, |state, cx| {
            state.set_value(text, window, cx);
            state.set_cursor_position(position, window, cx);
        });
    }

    /// Move the keyboard to the composer.
    pub fn focus_composer(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.composer.focus_handle(cx), cx);
    }

    // ---------------------------------------------------------------- events

    /// Fold one wire event and react to the few that are more than transcript.
    pub fn apply(&mut self, event: MuseEvent, cx: &mut Context<Self>) {
        if let MuseEvent::Closed(_) = &event {
            cx.emit(SessionEvent::Closed);
            self.running = None;
            self.submitting = false;
            self.ticker = None;
            cx.notify();
            return;
        }
        let mut unqueued: Option<String> = None;
        if let MuseEvent::Notification { method, params, session_id, .. } = &event {
            if session_id.as_deref().is_some_and(|id| id != self.session_id) {
                return;
            }
            match method.as_str() {
                "turn/started" => {
                    if let Some(turn_id) = params.get("turnId").and_then(|v| v.as_str()) {
                        self.running = Some(Running { turn_id: turn_id.to_owned(), started: Instant::now() });
                        self.submitting = false;
                        self.start_ticker(cx);
                    }
                }
                "turn/completed" => {
                    let ours = params.get("turnId").and_then(|v| v.as_str());
                    if self.running.as_ref().is_some_and(|r| Some(r.turn_id.as_str()) == ours) {
                        self.running = None;
                        self.ticker = None;
                    }
                    self.submitting = false;
                    if let Some(turn_id) = ours {
                        self.plan_completed(turn_id, cx);
                    }
                    self.turn_failure(params, cx);
                }
                "turn/unqueued" => {
                    unqueued = params.get("turnId").and_then(|v| v.as_str()).map(str::to_owned);
                }
                _ => {}
            }
        }
        let changed = !self.fold.apply(event).is_empty();
        // Restore a retracted prompt the moment the fold hands it back — unless
        // the unqueue was a Remove (the text is meant to be gone) or a Steer
        // (the text is going straight back out on the wire).
        if let Some(text) = self.fold.take_restored_prompt(&self.session_id) {
            match unqueued.as_deref().and_then(|id| self.unqueueing.remove(id)) {
                Some(Unqueue::Remove) => {}
                Some(Unqueue::Steer) => self.steer_text(text, cx),
                _ => self.restore_prompt(text, cx),
            }
        }
        if changed {
            self.follow = true;
        }
        cx.notify();
    }

    /// A `turn/completed` with `terminal: "failed"`: the fold already drew the
    /// error card, so all that is left is deciding whether the failure means
    /// the credential is gone (spec §3.2).
    fn turn_failure(&mut self, params: &serde_json::Value, cx: &mut Context<Self>) {
        let Some(error) = params.get("error") else { return };
        let kind = error.get("kind").and_then(|v| v.as_str());
        let message = error.get("message").and_then(|v| v.as_str()).unwrap_or_default();
        if conn::looks_like_signed_out(kind, message) {
            cx.emit(SessionEvent::SignedOut { message: message.to_owned() });
        }
    }

    /// A retraction or an unqueue handed the prompt back; put it in the
    /// composer, where it came from.
    ///
    /// `set_value` needs a `Window`, which a wire event never holds, so the
    /// text is parked here and the next frame picks it up — the same trick the
    /// gallery's mock uses for deferred focus.
    fn restore_prompt(&mut self, text: String, cx: &mut Context<Self>) {
        self.pending_prompt = Some(text);
        cx.notify();
    }

    /// Page the whole transcript in, oldest first, and fold it.
    ///
    /// `session/resume` is what attaches; the history itself comes through
    /// `view/page` from the beginning of the view, which is the one path that
    /// is contiguous, ordered and bounded. `view/page` never replays
    /// `item/delta`, so a backfilled message arrives whole and the fold takes
    /// it that way.
    pub fn backfill(&mut self, cx: &mut Context<Self>) {
        self.loading_history = true;
        cx.notify();
        let client = self.client.clone();
        let session_id = self.session_id.clone();
        let pages = cx.background_spawn(async move { page_all(&client, &session_id) });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let events = pages.await;
            let _ = this.update(cx, |this, cx| {
                for event in events {
                    this.fold.apply(event);
                }
                this.loading_history = false;
                this.follow = true;
                cx.notify();
            });
        }));
    }

    // -------------------------------------------------------------- commands

    /// Send the draft as a turn (`turn/start`, provider `meta`).
    ///
    /// `displayText` carries what the person typed, verbatim: it is what the
    /// transcript shows, and it stays the person's words even when plan mode
    /// prefixes the model-visible input.
    pub fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().to_string();
        if text.trim().is_empty() && self.images.is_empty() {
            return;
        }
        self.composer.update(cx, |state, cx| state.set_value("", window, cx));
        self.submit(text, cx);
    }

    /// Send `text` without touching the composer — the scripting hook.
    pub fn send_text(&mut self, text: String, cx: &mut Context<Self>) {
        self.submit(text, cx);
    }

    fn submit(&mut self, text: String, cx: &mut Context<Self>) {
        self.banner = None;
        self.submitting = true;
        self.history.set(history::append(&self.workspace_key, &text));
        let command_id = new_command_id();
        // The wire never gives the prompt back, so the fold has to remember it
        // before the command leaves: a retraction identifies the submission by
        // `commandId` and by nothing else.
        self.fold.record_command(&self.session_id, &command_id, &text);
        // Plan mode is the one thing that makes the model-visible text differ
        // from the person's: `/plan ` fires the bundled skill (see plan.rs).
        let model_text = if self.plan { plan::prefix(&text) } else { text.clone() };
        let params = TurnStartParams {
            command_id,
            session_id: self.session_id.clone(),
            input: self.parts(model_text),
            display_text: Some(text.clone()),
            reasoning_effort: self.effort.map(effort_wire),
            ..Default::default()
        };
        self.images.clear();
        let client = self.client.clone();
        let planning = self.plan;
        let call = cx.background_spawn(async move { client.turn_start(&params) });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = call.await;
            let _ = this.update(cx, |this, cx| this.sent(result, text, planning, cx));
        }));
        cx.notify();
    }

    /// The turn's content parts: the text, then every attached image.
    fn parts(&self, text: String) -> Vec<TurnInputPart> {
        let mut parts = Vec::new();
        if !text.trim().is_empty() {
            parts.push(TurnInputPart::text(text));
        }
        parts.extend(self.images.iter().map(images::Image::part));
        if parts.is_empty() {
            parts.push(TurnInputPart::text(String::new()));
        }
        parts
    }

    /// The `turn/start` ack. Admission only — the authority for what the turn
    /// is doing is always the view event.
    fn sent(
        &mut self,
        result: Result<TurnStartResult, MuseError>,
        text: String,
        planning: bool,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(ack) => {
                if ack.disposition == TurnStartDisposition::Queued {
                    self.fold.record_queued(&self.session_id, &ack.turn_id, &ack.command_id, &text);
                    self.submitting = false;
                } else if planning {
                    // The plan card is appended when this turn's reply lands.
                    self.plan_turn = Some(ack.turn_id.clone());
                }
            }
            Err(error) => {
                self.submitting = false;
                self.report(&error, cx);
                // The turn never left, so the person keeps their words.
                self.restore_prompt(text, cx);
            }
        }
        cx.notify();
    }

    /// ⌘↩: interject into the running turn instead of queueing behind it.
    pub fn steer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().to_string();
        if text.trim().is_empty() {
            return;
        }
        if self.running.is_none() {
            // Nothing to steer into; the honest thing is an ordinary send.
            self.send(window, cx);
            return;
        }
        self.composer.update(cx, |state, cx| state.set_value("", window, cx));
        self.steer_text(text, cx);
    }

    fn steer_text(&mut self, text: String, cx: &mut Context<Self>) {
        let Some(turn_id) = self.running.as_ref().map(|r| r.turn_id.clone()) else {
            self.restore_prompt(text, cx);
            return;
        };
        let command_id = new_command_id();
        self.fold.record_command(&self.session_id, &command_id, &text);
        self.history.set(history::append(&self.workspace_key, &text));
        let params = TurnSteerParams {
            command_id,
            session_id: self.session_id.clone(),
            expected_turn_id: turn_id,
            input: self.parts(text),
            reasoning_effort: self.effort.map(effort_wire),
        };
        self.images.clear();
        let client = self.client.clone();
        let call = cx.background_spawn(async move { client.turn_steer(&params) });
        self.tasks.push(cx.spawn(async move |this, cx| {
            if let Err(error) = call.await {
                let _ = this.update(cx, |this, cx| this.report(&error, cx));
            }
        }));
        cx.notify();
    }

    /// Stop, with the retract intent paired: a turn interrupted before any
    /// output committed is durably retracted and its prompt comes back.
    pub fn interrupt(&mut self, cx: &mut Context<Self>) {
        if !self.busy() {
            return;
        }
        let params = TurnInterruptParams {
            command_id: new_command_id(),
            session_id: self.session_id.clone(),
            retract: Some(true),
            turn_id: self.running.as_ref().map(|r| r.turn_id.clone()),
        };
        let client = self.client.clone();
        let call = cx.background_spawn(async move { client.turn_interrupt(&params) });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = call.await;
            let _ = this.update(cx, |this, cx| {
                if let Err(error) = result {
                    this.report(&error, cx);
                }
            });
        }));
    }

    /// `turn/unqueue`, remembering why so `turn/unqueued` knows what to do with
    /// the text it hands back.
    fn unqueue(&mut self, turn_id: &str, why: Unqueue, cx: &mut Context<Self>) {
        self.unqueueing.insert(turn_id.to_owned(), why);
        let params = TurnUnqueueParams {
            command_id: new_command_id(),
            session_id: self.session_id.clone(),
            turn_id: turn_id.to_owned(),
        };
        let client = self.client.clone();
        let turn_id = turn_id.to_owned();
        let call = cx.background_spawn(async move { client.turn_unqueue(&params) });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = call.await;
            let _ = this.update(cx, |this, cx| {
                if let Err(error) = result {
                    // The reclaim lost the race; the row stays, because only
                    // the wire removes it.
                    this.unqueueing.remove(&turn_id);
                    this.report(&error, cx);
                }
            });
        }));
        cx.notify();
    }

    /// `session/setModel`. The chip changes on `session/modelChanged`, never
    /// here.
    fn set_model(&mut self, model_id: &str, cx: &mut Context<Self>) {
        let row = self.models.iter().find(|m| m.model_id == model_id);
        let params = SessionSetModelParams {
            command_id: new_command_id(),
            session_id: self.session_id.clone(),
            model: ModelSelection {
                display_label: row.map(|r| r.display_label.clone()),
                model_id: model_id.to_owned(),
                profile_id: row.and_then(|r| r.profile_id.clone()),
                provider_id: row.map(|r| r.provider_id.clone()),
            },
        };
        let client = self.client.clone();
        let call = cx.background_spawn(async move { client.session_set_model(&params) });
        self.tasks.push(cx.spawn(async move |this, cx| {
            if let Err(error) = call.await {
                // On the echo provider this is `commandRejected:
                // unsupported_route`, and the banner saying so is correct.
                let _ = this.update(cx, |this, cx| this.report(&error, cx));
            }
        }));
    }

    /// `session/setApprovalMode`. The chip and the marker both come from
    /// `session/approvalModeChanged`.
    fn set_mode(&mut self, mode: PermissionMode, cx: &mut Context<Self>) {
        let params = SessionSetApprovalModeParams {
            command_id: new_command_id(),
            session_id: self.session_id.clone(),
            mode: wire_mode(mode),
        };
        let client = self.client.clone();
        let call = cx.background_spawn(async move { client.session_set_approval_mode(&params) });
        self.tasks.push(cx.spawn(async move |this, cx| {
            if let Err(error) = call.await {
                let _ = this.update(cx, |this, cx| this.report(&error, cx));
            }
        }));
    }

    /// `session/compact`. An ack of `noop` is a success, and says why.
    fn compact(&mut self, cx: &mut Context<Self>) {
        let params = SessionCompactParams {
            command_id: new_command_id(),
            session_id: self.session_id.clone(),
            turn_id: None,
        };
        let client = self.client.clone();
        let call = cx.background_spawn(async move { client.session_compact(&params) });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = call.await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(ack) if ack.status == muse_client::schema::CompactStatus::Noop => {
                    let reason = ack.reason.unwrap_or_else(|| "nothing to summarize".to_owned());
                    this.toast("Nothing to compact", reason, cx);
                }
                Ok(_) => {}
                Err(error) => this.report(&error, cx),
            });
        }));
    }

    /// Fetch the catalog for this session. A snapshot, on every open: MSP has
    /// no catalog subscription, so a stale list would be worse than a wait.
    fn load_models(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let params = ModelListParams { session_id: Some(self.session_id.clone()) };
        let call = cx.background_spawn(async move { client.model_list(&params) });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = call.await;
            let _ = this.update(cx, |this, cx| {
                if let Ok(list) = result {
                    this.models = list.models;
                }
                cx.notify();
            });
        }));
    }

    /// Route a failed command to its banner or its dialog (spec §3.8).
    fn report(&mut self, error: &MuseError, cx: &mut Context<Self>) {
        let title = conn::title(error);
        match conn::severity(error) {
            Severity::Banner => self.banner = Some(format!("{title}. {error}")),
            Severity::Dialog => cx.emit(SessionEvent::Dialog { title, detail: error.to_string() }),
        }
        cx.notify();
    }

    fn toast(&mut self, title: impl Into<String>, body: impl Into<String>, cx: &mut Context<Self>) {
        self.overlays.update(cx, |overlays, _| overlays.toast(title, body));
        cx.notify();
    }

    /// Keep the elapsed time honest while a turn runs.
    fn start_ticker(&mut self, cx: &mut Context<Self>) {
        if self.ticker.is_some() {
            return;
        }
        self.ticker = Some(cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(TICK).await;
            let alive = this.update(cx, |this, cx| {
                cx.notify();
                this.running.is_some()
            });
            if !matches!(alive, Ok(true)) {
                return;
            }
        }));
    }

    // -------------------------------------------------------------- plan mode

    /// Turn plan mode on or off (Shift+Tab, `/plan`, the pill's `x`).
    ///
    /// Turning it on asks the server for `denyUnmatched` and remembers what the
    /// session was in; the chip only moves when `session/approvalModeChanged`
    /// says it did.
    pub fn set_plan(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.plan == on {
            return;
        }
        self.plan = on;
        if on {
            self.plan_previous_mode = Some(self.mode());
            self.set_mode(PermissionMode::DenyUnmatched, cx);
        } else if let Some(previous) = self.plan_previous_mode.take() {
            self.set_mode(previous, cx);
        }
        cx.notify();
    }

    /// The plan turn finished: append the plan card built from its reply.
    fn plan_completed(&mut self, turn_id: &str, cx: &mut Context<Self>) {
        if self.plan_turn.as_deref() != Some(turn_id) {
            return;
        }
        self.plan_turn = None;
        let Some(reply) = self.last_assistant_text() else { return };
        let steps = plan::steps(&reply);
        if steps.is_empty() {
            return;
        }
        self.plan_seq += 1;
        let id = format!("plan-{}", self.plan_seq);
        self.fold.append_client_block(
            &self.session_id,
            &id,
            Block::Plan { id: id.clone(), items: steps, state: PlanState::Proposed },
        );
        self.follow = true;
        cx.notify();
    }

    /// The text of the newest assistant text block, which is the plan reply.
    fn last_assistant_text(&self) -> Option<String> {
        let session = self.session()?;
        for turn in session.turns.iter().rev() {
            if let aui_protocol::Turn::Assistant { blocks, .. } = turn {
                for block in blocks.iter().rev() {
                    if let Block::Text { text, .. } = block {
                        if !text.trim().is_empty() {
                            return Some(text.clone());
                        }
                    }
                }
            }
        }
        None
    }

    /// Accept / Refine / Reject on a plan card (spec §3.1).
    fn plan_action(&mut self, id: &str, action: PlanAction, window: &mut Window, cx: &mut Context<Self>) {
        let state = match action {
            PlanAction::Accept => PlanState::Accepted,
            PlanAction::Reject => PlanState::Rejected,
            PlanAction::Refine => PlanState::Proposed,
        };
        if action != PlanAction::Refine {
            if let Some(block) = self.plan_block(id) {
                let Block::Plan { items, .. } = &block else { return };
                let replaced = Block::Plan { id: id.to_owned(), items: items.clone(), state };
                self.fold.replace_client_block(&self.session_id, id, replaced);
            }
        }
        match action {
            PlanAction::Accept => {
                self.set_plan(false, cx);
                self.submit(plan::ACCEPT_PROMPT.to_owned(), cx);
            }
            PlanAction::Reject => self.set_plan(false, cx),
            PlanAction::Refine => self.focus_composer(window, cx),
        }
        cx.notify();
    }

    fn plan_block(&self, id: &str) -> Option<Block> {
        let session = self.session()?;
        session.turns.iter().find_map(|turn| match turn {
            aui_protocol::Turn::Assistant { id: turn_id, blocks, .. } if turn_id == id => blocks.first().cloned(),
            _ => None,
        })
    }

    // ------------------------------------------------------------ the menus

    /// The draft changed: re-derive the caret popovers and step off the
    /// history.
    fn on_draft_changed(&mut self, cx: &mut Context<Self>) {
        if self.history.walking() {
            self.history.reset();
        }
        let (draft, caret) = self.draft_and_caret(cx);
        let token = caret_token(&draft, caret);
        self.overlays.update(cx, |overlays, _| {
            let caret_menu = matches!(
                overlays.menu.as_ref().map(|m| m.kind),
                Some(MenuKind::Command) | Some(MenuKind::Mention)
            );
            match token {
                Some((kind, at, filter)) => {
                    let same = overlays.menu.as_ref().is_some_and(|m| m.kind == kind && m.at == at);
                    if same {
                        if let Some(menu) = overlays.menu.as_mut() {
                            menu.filter = filter;
                            menu.selected = 0;
                        }
                    } else {
                        let mut menu = Menu::caret(kind, at);
                        menu.filter = filter;
                        overlays.open(menu);
                    }
                }
                None if caret_menu => overlays.menu = None,
                None => {}
            }
        });
        cx.notify();
    }

    fn draft_and_caret(&self, cx: &gpui::App) -> (String, usize) {
        let state = self.composer.read(cx);
        (state.value().to_string(), state.cursor())
    }

    /// Open (or close) one of the chip pickers.
    pub fn toggle_picker(&mut self, kind: MenuKind, cx: &mut Context<Self>) {
        let open = self.overlays.read(cx).is_open(kind);
        if open {
            self.overlays.update(cx, |overlays, _| overlays.menu = None);
            cx.notify();
            return;
        }
        let selected = match kind {
            MenuKind::Model => {
                self.load_models(cx);
                self.models.iter().position(|m| m.is_active).unwrap_or(0)
            }
            MenuKind::Effort => EFFORTS.iter().position(|e| *e == self.effort).unwrap_or(0),
            MenuKind::Mode => MODES.iter().position(|m| *m == self.mode()).unwrap_or(0),
            _ => 0,
        };
        self.overlays.update(cx, |overlays, _| overlays.open(Menu::picker(kind, selected)));
        cx.notify();
    }

    /// How many rows the open menu has, which is what the arrow keys wrap on.
    pub fn menu_rows(&self, cx: &gpui::App) -> usize {
        let overlays = self.overlays.read(cx);
        match overlays.menu.as_ref().map(|m| m.kind) {
            Some(MenuKind::Model) => self.models.len(),
            Some(MenuKind::Effort) => EFFORTS.len(),
            Some(MenuKind::Mode) => MODES.len(),
            Some(MenuKind::Command) => {
                let filter = overlays.menu.as_ref().map(|m| m.filter.clone()).unwrap_or_default();
                let (commands, skill_rows) = self.command_rows(&filter, cx);
                commands.len() + skill_rows.len()
            }
            Some(MenuKind::Mention) => {
                let filter = overlays.menu.as_ref().map(|m| m.filter.clone()).unwrap_or_default();
                self.mention_rows(&filter, cx).len()
            }
            None => 0,
        }
    }

    /// Enter on the open menu.
    pub fn confirm_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((kind, selected, filter)) = self
            .overlays
            .read(cx)
            .menu
            .as_ref()
            .map(|m| (m.kind, m.selected, m.filter.clone()))
        else {
            return;
        };
        match kind {
            MenuKind::Model => {
                if let Some(model) = self.models.get(selected).map(|m| m.model_id.clone()) {
                    self.pick_model(&model, cx);
                }
            }
            MenuKind::Effort => {
                if let Some(effort) = EFFORTS.get(selected).copied() {
                    self.pick_effort(effort, cx);
                }
            }
            MenuKind::Mode => {
                if let Some(mode) = MODES.get(selected).copied() {
                    self.pick_mode(mode, cx);
                }
            }
            MenuKind::Command => {
                let (commands, skill_rows) = self.command_rows(&filter, cx);
                if let Some(command) = commands.get(selected).copied() {
                    self.run_command(command, window, cx);
                } else if let Some(skill) = skill_rows.get(selected.saturating_sub(commands.len())) {
                    let insertion = format!("/{} ", skill.name);
                    self.replace_token(&insertion, window, cx);
                }
            }
            MenuKind::Mention => {
                let rows = self.mention_rows(&filter, cx);
                if let Some(path) = rows.get(selected).cloned() {
                    self.replace_token(&format!("@{path} "), window, cx);
                }
            }
        }
    }

    /// A click on a `/` menu row, which names itself rather than its index.
    fn select_command(&mut self, id: &SharedString, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(command) = Command::parse(id.as_ref()) {
            self.run_command(command, window, cx);
            return;
        }
        let name = self
            .overlays
            .read(cx)
            .skills
            .iter()
            .find(|s| s.id == id.as_ref())
            .map(|s| s.name.clone());
        if let Some(name) = name {
            self.replace_token(&format!("/{name} "), window, cx);
        }
    }

    fn pick_model(&mut self, model_id: &str, cx: &mut Context<Self>) {
        self.set_model(model_id, cx);
        self.close_menu(cx);
    }

    fn pick_effort(&mut self, effort: Option<ReasoningEffort>, cx: &mut Context<Self>) {
        self.effort = effort;
        self.close_menu(cx);
    }

    fn pick_mode(&mut self, mode: PermissionMode, cx: &mut Context<Self>) {
        self.set_mode(mode, cx);
        self.close_menu(cx);
    }

    fn close_menu(&mut self, cx: &mut Context<Self>) {
        self.overlays.update(cx, |overlays, _| overlays.menu = None);
        cx.notify();
    }

    /// Run one client-side slash command (spec §3.10).
    fn run_command(&mut self, command: Command, window: &mut Window, cx: &mut Context<Self>) {
        self.replace_token("", window, cx);
        if !command.available() {
            let label = command.slash().to_owned();
            self.toast(format!("{label} is not in this build yet"), command.coming_in(), cx);
            return;
        }
        match command {
            Command::Model => self.toggle_picker(MenuKind::Model, cx),
            Command::Effort => self.toggle_picker(MenuKind::Effort, cx),
            Command::Mode => self.toggle_picker(MenuKind::Mode, cx),
            Command::Plan => {
                let on = !self.plan;
                self.set_plan(on, cx);
            }
            Command::Compact => self.compact(cx),
            Command::Clear => cx.emit(SessionEvent::NewSession),
            Command::Logout => cx.emit(SessionEvent::Logout),
            Command::Status | Command::Usage => {
                let detail = self.status_text(cx);
                cx.emit(SessionEvent::Status { detail });
            }
            Command::Help => {
                self.overlays.update(cx, |overlays, _| overlays.open(Menu::caret(MenuKind::Command, 0)));
                cx.notify();
            }
            Command::Fork | Command::Name | Command::Resume => {}
        }
    }

    /// The `/status` and `/usage` body: everything the session knows about
    /// itself, from the fold rather than from what the app last sent.
    fn status_text(&self, cx: &gpui::App) -> String {
        let context = self.context();
        let branch = self.session().and_then(|s| s.branch.clone()).unwrap_or_else(|| "—".to_owned());
        let queued = self.fold.side(&self.session_id).map(|s| s.queued.len()).unwrap_or(0);
        let _ = cx;
        format!(
            "Model: {}\nApproval mode: {}\nReasoning effort: {}\nPlan mode: {}\nContext: {}\nSession tokens: {} prompt · {} output · {} total\nQueued: {queued}\nSession: {}\nWorkspace: {}\nBranch: {branch}",
            self.model(),
            self.mode_label(),
            crate::overlays::effort_label(self.effort),
            if self.plan { "on" } else { "off" },
            context.label(),
            context.prompt_tokens,
            context.output_tokens,
            context.total_tokens,
            self.session_id,
            self.workspace,
        )
    }

    /// Replace the `/…` or `@…` token the caret is in with `insertion`.
    fn replace_token(&mut self, insertion: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(at) = self.overlays.read(cx).menu.as_ref().map(|m| m.at) else { return };
        let (draft, caret) = self.draft_and_caret(cx);
        if at > draft.len() || caret > draft.len() || at > caret {
            self.close_menu(cx);
            return;
        }
        let mut next = String::with_capacity(draft.len() + insertion.len());
        next.push_str(&draft[..at]);
        next.push_str(insertion);
        next.push_str(&draft[caret..]);
        let offset = at + insertion.len();
        let position = position_of(&next, offset);
        self.composer.update(cx, |state, cx| {
            state.set_value(next, window, cx);
            state.set_cursor_position(position, window, cx);
        });
        self.close_menu(cx);
    }

    // ---------------------------------------------------------------- history

    /// ↑ on the first line of the draft.
    pub fn history_prev(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let draft = self.composer.read(cx).value().to_string();
        if let Some(text) = self.history.prev(&draft) {
            self.set_history_value(text, window, cx);
        }
    }

    /// ↓ on the last line of the draft.
    pub fn history_next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.history.next() {
            self.set_history_value(text, window, cx);
        }
    }

    /// Whether the caret is on the draft's first (or last) line, which is what
    /// arms ↑ and ↓ for the history rather than for the editor.
    pub fn caret_edges(&self, cx: &gpui::App) -> (bool, bool) {
        let state = self.composer.read(cx);
        let value = state.value();
        let line = state.cursor_position().line;
        let last = value.matches('\n').count() as u32;
        (line == 0, line >= last)
    }

    /// Setting the value fires `InputEvent::Change`, which would reset the
    /// cursor the walk depends on, so the walk's own writes are marked.
    fn set_history_value(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        let position = position_of(&text, text.len());
        self.composer.update(cx, |state, cx| {
            state.set_value(text, window, cx);
            state.set_cursor_position(position, window, cx);
        });
        cx.notify();
    }

    // ----------------------------------------------------------------- images

    /// ⌘V with an image on the clipboard.
    ///
    /// Returns `false` when the clipboard holds no image, so the caller can let
    /// the textarea's own paste have the keystroke.
    pub fn paste_image(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(item) = cx.read_from_clipboard() else { return false };
        let mut pasted = false;
        for entry in item.into_entries() {
            if let ClipboardEntry::Image(image) = entry {
                self.attach_bytes("pasted", image.bytes.clone(), cx);
                pasted = true;
            }
        }
        pasted
    }

    /// The `+` menu's "Attach image", and the drop of a file from Finder.
    pub fn attach_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        for path in paths {
            self.image_seq += 1;
            match images::from_path(format!("img-{}", self.image_seq), &path) {
                Ok(image) => self.images.push(image),
                Err(reason) => self.banner = Some(reason),
            }
        }
        cx.notify();
    }

    fn attach_bytes(&mut self, name: &str, bytes: Vec<u8>, cx: &mut Context<Self>) {
        self.image_seq += 1;
        match images::from_bytes(format!("img-{}", self.image_seq), name, &bytes) {
            Ok(image) => self.images.push(image),
            Err(reason) => self.banner = Some(reason),
        }
        cx.notify();
    }

    /// Open the system picker for an image.
    pub fn prompt_for_image(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: None,
        });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else { return };
            let _ = this.update(cx, |this, cx| this.attach_paths(paths, cx));
        }));
    }

    // -------------------------------------------------------------- scripting

    /// One `--steps` item. Everything a screenshot needs, driven from a
    /// command line so every capture is reproducible.
    pub fn step(&mut self, step: &str, window: &mut Window, cx: &mut Context<Self>) {
        let (head, rest) = step.split_once(':').unwrap_or((step, ""));
        match head {
            "draft" => self.set_draft(rest.to_owned(), window, cx),
            "send" => self.send_text(rest.to_owned(), cx),
            "steer" => self.steer_text(rest.to_owned(), cx),
            "model" => self.toggle_picker(MenuKind::Model, cx),
            "effort" => self.toggle_picker(MenuKind::Effort, cx),
            "mode" => self.toggle_picker(MenuKind::Mode, cx),
            "confirm" => self.confirm_menu(window, cx),
            "setmodel" => self.set_model(rest, cx),
            "compact" => self.compact(cx),
            "meter" => {
                self.meter_open = true;
                cx.notify();
            }
            // A pressure state the echo provider cannot be pushed into: a
            // synthetic `session/contextUsage`, and it is only ever reachable
            // from this flag.
            "context" => {
                self.fake_context = parse_context(rest);
                cx.notify();
            }
            "plan" => self.set_plan(true, cx),
            "image" => self.attach_paths(vec![PathBuf::from(rest)], cx),
            "plus" => {
                self.plus_open = !self.plus_open;
                cx.notify();
            }
            "drop" => {
                self.dragging = true;
                cx.notify();
            }
            "command" | "mention" => {
                let sigil = if head == "command" { "/" } else { "@" };
                let text = format!("{sigil}{rest}");
                self.set_draft(text, window, cx);
                self.on_draft_changed(cx);
            }
            other => eprintln!("harness: unknown step `{other}`"),
        }
    }

    // ----------------------------------------------------------------- render

    /// The centre pane: transcript, status row, banner, composer.
    pub fn render_centre(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if let Some(text) = self.pending_prompt.take() {
            self.composer.update(cx, |state, cx| state.set_value(text, window, cx));
        }
        let transcript = self.render_transcript(window, cx);
        let status = self.render_status();
        let banner = self.render_banner(cx);
        let queue = self.render_queue(cx);
        let caret_menu = self.render_caret_menu(cx);
        let composer = self.render_composer(cx);
        let drop = self.dragging;
        v_flex()
            .size_full()
            .relative()
            .child(transcript)
            .children(status)
            .children(banner)
            .children(queue)
            .child(div().w_full().relative().px(px(TRANSCRIPT_PAD_X)).children(caret_menu))
            .child(composer)
            .child(aui::composer::drop_overlay("drop", drop))
            // gpui reports an external drag only while it moves, so that is
            // what raises the overlay; the drop takes it down again.
            .on_drag_move(cx.listener(|this: &mut Self, _: &gpui::DragMoveEvent<ExternalPaths>, _, cx| {
                if !this.dragging {
                    this.dragging = true;
                    cx.notify();
                }
            }))
            .on_drop(cx.listener(|this: &mut Self, paths: &ExternalPaths, _, cx| {
                this.dragging = false;
                this.attach_paths(paths.paths().to_vec(), cx);
            }))
            .into_any_element()
    }

    fn render_transcript(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        // Tail-follow: anything new scrolls the list down, but only for a
        // reader who was already at the bottom.
        if std::mem::take(&mut self.follow) {
            let at_end = (-self.scroll.offset().y) >= self.scroll.max_offset().y - px(TAIL_SLACK);
            if at_end {
                self.scroll.scroll_to_bottom();
            }
        }
        let Some(session) = self.fold.session(&self.session_id).cloned() else {
            return transcript::empty_state(&self.workspace, cx);
        };
        if session.turns.is_empty() {
            return transcript::empty_state(&self.workspace, cx);
        }
        let folds = Folds {
            toggled: self.toggled.clone(),
            toggle: {
                let toggle = cx.listener(|this: &mut Self, key: &String, _, cx| {
                    if !this.toggled.remove(key) {
                        this.toggled.insert(key.clone());
                    }
                    cx.notify();
                });
                Rc::new(move |key: String, window: &mut Window, cx: &mut gpui::App| toggle(&key, window, cx))
            },
            plan: {
                let act = cx.listener(|this: &mut Self, (id, action): &(String, PlanAction), window, cx| {
                    this.plan_action(id, *action, window, cx);
                });
                Some(Rc::new(move |id: String, action: PlanAction, window: &mut Window, cx: &mut gpui::App| {
                    act(&(id, action), window, cx)
                }))
            },
        };
        let last = session.turns.len().saturating_sub(1);
        let mut list = div()
            .id("transcript")
            .track_scroll(&self.scroll)
            .flex_1()
            .min_h(px(0.0))
            .w_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .pt(px(TRANSCRIPT_PAD_TOP))
            .px(px(TRANSCRIPT_PAD_X))
            .pb(px(scale::SP_4))
            .gap(px(scale::SP_5));
        for (index, turn) in session.turns.iter().enumerate() {
            for element in transcript::turn(turn, index != last, &folds, window, cx) {
                list = list.child(element);
            }
        }
        list.into_any_element()
    }

    /// The live status line: history loading, or a running turn with its
    /// elapsed time and the interrupt hint.
    fn render_status(&self) -> Option<AnyElement> {
        let row = if self.loading_history {
            status_row("status", "Loading history\u{2026}").lead(StatusLead::Spinner).shimmer(true)
        } else if self.busy() {
            let elapsed = self.running.as_ref().map(|r| r.started.elapsed().as_millis() as u64).unwrap_or(0);
            let mut row = status_row("status", "Working\u{2026}")
                .lead(StatusLead::Braille)
                .shimmer(true)
                .key_hint("esc", "to interrupt");
            if elapsed > 0 {
                row = row.elapsed(transcript::elapsed(elapsed));
            }
            let queued = self.fold.side(&self.session_id).map(|s| s.queued.len()).unwrap_or(0);
            if queued > 0 {
                row = row.note(format!("{queued} queued"));
            }
            row
        } else {
            return None;
        };
        Some(
            h_flex()
                .w_full()
                .px(px(TRANSCRIPT_PAD_X))
                .pb(px(scale::SP_4))
                .child(row)
                .into_any_element(),
        )
    }

    /// The inline banner over the composer, for a recoverable command error.
    fn render_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let message = self.banner.clone()?;
        let dismiss = cx.listener(|this: &mut Self, _: &(), _, cx| {
            this.banner = None;
            cx.notify();
        });
        Some(
            div()
                .w_full()
                .px(px(TRANSCRIPT_PAD_X))
                .pb(px(scale::SP_3))
                .child(
                    banner("session-banner", BannerKind::Error, vec![BannerRun::Text(message.into())])
                        .action("Dismiss", BannerActionStyle::Ghost)
                        .on_action(move |window, cx| dismiss(&(), window, cx)),
                )
                .into_any_element(),
        )
    }

    /// The queued strip: exactly what `SideState::queued` holds, in server
    /// order.
    fn render_queue(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let side = self.fold.side(&self.session_id)?;
        if side.queued.is_empty() {
            return None;
        }
        let editing = self.unqueueing.clone();
        let rows: Vec<QueueStripRow> = side
            .queued
            .iter()
            .map(|q| {
                let row = QueueStripRow::new(q.turn_id.clone(), q.text.clone());
                if editing.get(&q.turn_id) == Some(&Unqueue::Edit) {
                    row.editing()
                } else {
                    row
                }
            })
            .collect();
        let intent = cx.listener(|this: &mut Self, (id, intent): &(SharedString, QueueIntent), _, cx| {
            let why = match intent {
                QueueIntent::Edit => Unqueue::Edit,
                QueueIntent::Remove => Unqueue::Remove,
                QueueIntent::Steer => Unqueue::Steer,
            };
            this.unqueue(id.as_ref(), why, cx);
        });
        Some(
            div()
                .w_full()
                .px(px(TRANSCRIPT_PAD_X))
                .pb(px(scale::SP_3))
                .child(queue_strip("queue", rows).on_intent(move |id, i, window, cx| {
                    intent(&(id.clone(), i), window, cx)
                }))
                .into_any_element(),
        )
    }

    /// The `/` menu and the `@` picker, floating above the composer.
    fn render_caret_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let overlays = self.overlays.read(cx);
        let menu = overlays.menu.as_ref()?;
        let (kind, selected, filter) = (menu.kind, menu.selected, menu.filter.clone());
        let element = match kind {
            MenuKind::Command => {
                let (commands, skill_rows) = self.command_rows(&filter, cx);
                let mut sections = Vec::new();
                if !commands.is_empty() {
                    sections.push(CommandSection::new(
                        "Commands",
                        commands
                            .iter()
                            .map(|c| CommandItem::new(c.slash(), c.slash(), c.description()))
                            .collect(),
                    ));
                }
                if !skill_rows.is_empty() {
                    sections.push(CommandSection::new(
                        "Skills",
                        skill_rows
                            .iter()
                            .map(|s| {
                                CommandItem::new(s.id.clone(), format!("/{}", s.name), s.summary())
                                    .source_tag(s.scope().to_owned())
                            })
                            .collect(),
                    ));
                }
                if sections.is_empty() {
                    return None;
                }
                let pick = cx.listener(|this: &mut Self, id: &SharedString, window, cx| {
                    this.select_command(id, window, cx);
                });
                command_menu("command-menu", format!("/{filter}"), sections, selected)
                    .on_select(move |id, window, cx| pick(id, window, cx))
                    .into_any_element()
            }
            MenuKind::Mention => {
                let rows = self.mention_rows(&filter, cx);
                if rows.is_empty() {
                    return None;
                }
                let items: Vec<MentionItem> = rows
                    .iter()
                    .map(|path| {
                        let name = path.rsplit('/').next().unwrap_or(path).to_owned();
                        MentionItem::new(path.clone(), MentionIcon::Glyph(IconName::File), name, path.clone())
                            .detail_mono()
                            .matching(&filter)
                    })
                    .collect();
                let pick = cx.listener(|this: &mut Self, id: &SharedString, window, cx| {
                    let insertion = format!("@{id} ");
                    this.replace_token(&insertion, window, cx);
                });
                mention_picker("mention-picker", filter.clone(), vec![MentionSection::new("Files", items)], selected)
                    .on_select(move |id, window, cx| pick(id, window, cx))
                    .into_any_element()
            }
            // The chip pickers are anchored to their chips, not to the caret.
            MenuKind::Model | MenuKind::Effort | MenuKind::Mode => return None,
        };
        Some(
            popover_layer(
                div()
                    .id("caret-popover")
                    .absolute()
                    .bottom(px(POPOVER_GAP))
                    .left(px(0.0))
                    .right(px(0.0))
                    .max_h(px(POPOVER_MAX_H))
                    .overflow_y_scroll()
                    .child(element),
            )
            .into_any_element(),
        )
    }

    /// The `/` menu's two sections, filtered by what has been typed.
    fn command_rows(&self, filter: &str, cx: &gpui::App) -> (Vec<Command>, Vec<skills::Skill>) {
        let needle = filter.to_lowercase();
        let commands: Vec<Command> = Command::ALL
            .into_iter()
            .filter(|c| c.slash().trim_start_matches('/').to_lowercase().starts_with(&needle))
            .collect();
        let skill_rows: Vec<skills::Skill> = self
            .overlays
            .read(cx)
            .skills
            .iter()
            .filter(|s| s.name.to_lowercase().starts_with(&needle))
            .take(SKILL_ROWS)
            .cloned()
            .collect();
        (commands, skill_rows)
    }

    /// The `@` picker's candidates.
    fn mention_rows(&self, filter: &str, cx: &gpui::App) -> Vec<String> {
        let overlays = self.overlays.read(cx);
        files::filter(&overlays.files, filter).into_iter().cloned().collect()
    }

    fn render_composer(&self, cx: &mut Context<Self>) -> AnyElement {
        let draft = self.composer.read(cx).value().to_string();
        let blocked = self.context().pressure == ContextPressure::Blocked;
        let intent = cx.listener(|this: &mut Self, intent: &ComposerIntent, window, cx| match intent {
            ComposerIntent::Send => this.send(window, cx),
            ComposerIntent::Stop => this.interrupt(cx),
            ComposerIntent::Steer => this.steer(window, cx),
            ComposerIntent::Compact => this.compact(cx),
            ComposerIntent::ExitPlan => this.set_plan(false, cx),
            ComposerIntent::Attach => this.prompt_for_image(cx),
            ComposerIntent::Model => this.toggle_picker(MenuKind::Model, cx),
            ComposerIntent::Effort => this.toggle_picker(MenuKind::Effort, cx),
            ComposerIntent::Mode => this.toggle_picker(MenuKind::Mode, cx),
            ComposerIntent::TogglePlus => {
                this.plus_open = !this.plus_open;
                cx.notify();
            }
            ComposerIntent::RemoveChip(id) => {
                this.images.retain(|image| image.id != id.as_ref());
                cx.notify();
            }
        });
        let plus = cx.listener(|this: &mut Self, id: &SharedString, _, cx| {
            this.plus_open = false;
            if id.as_ref() == "attach-image" {
                this.prompt_for_image(cx);
            }
            cx.notify();
        });
        let mut element = composer("composer", &self.composer, aui_icons::Provider::Muse, self.model())
            .docked(true)
            .mode(self.mode_label())
            .effort(crate::overlays::effort_label(self.effort))
            .context(self.context())
            .context_open(self.meter_open)
            .plan(self.plan)
            .chips(
                self.images
                    .iter()
                    .map(|image| ComposerChip {
                        id: image.id.clone().into(),
                        kind: ComposerChipKind::Image,
                        label: image.name.clone().into(),
                        removable: true,
                    })
                    .collect(),
            )
            .streaming(self.busy())
            // Blocked context is the server refusing to take more, so the
            // composer refuses too and the meter offers the way out.
            .can_send(!blocked && (!draft.trim().is_empty() || !self.images.is_empty()))
            .plus_menu(
                self.plus_open,
                Some(
                    plus_menu(
                        "plus",
                        vec![PlusMenuItem::new("attach-image", IconName::Image, "Attach image")],
                        self.plus_open,
                    )
                    .on_activate(move |id, window, cx| plus(id, window, cx)),
                ),
            )
            .on_intent(move |i, window, cx| intent(&i, window, cx));
        for (anchor, menu) in self.render_pickers(cx) {
            element = element.chip_menu(anchor, menu);
        }
        element.into_any_element()
    }

    /// The three chip pickers, each anchored to the chip that opens it.
    ///
    /// None of them is given an `on_hover`, on purpose: the component already
    /// lets the pointer win the highlight for as long as it is over a row, so an
    /// app that *also* wrote the pointer's row into `selected` would give the
    /// selection two owners — and the check, which marks the session's actual
    /// value, would wander with the mouse. The keyboard owns `selected`; the
    /// pointer owns its own highlight and reports only a click.
    fn render_pickers(&self, cx: &mut Context<Self>) -> Vec<(ComposerChipAnchor, AnyElement)> {
        let Some((kind, selected)) = self.overlays.read(cx).menu.as_ref().map(|m| (m.kind, m.selected)) else {
            return Vec::new();
        };
        let close = cx.listener(|this: &mut Self, _: &(), _, cx| this.close_menu(cx));
        match kind {
            MenuKind::Model => {
                let rows: Vec<PickerRow> = self
                    .models
                    .iter()
                    .map(|m| {
                        let mut row = PickerRow::new(
                            m.model_id.clone(),
                            m.display_label.clone(),
                            m.description.clone().unwrap_or_default(),
                        );
                        if let Some(limit) = m.context_limit {
                            row = row.meta(format!("{} ctx", compact_count(limit)));
                        }
                        // A catalog may flag any number of rows either way, so
                        // both badges are drawn wherever they appear.
                        if m.is_default {
                            row = row.badge("default");
                        }
                        if m.is_active {
                            row = row.badge("active");
                        }
                        row
                    })
                    .collect();
                let pick = cx.listener(|this: &mut Self, id: &SharedString, _, cx| {
                    let id = id.to_string();
                    this.pick_model(&id, cx);
                });
                vec![(
                    ComposerChipAnchor::Model,
                    model_menu("model-menu", rows, selected, true)
                        .on_pick(move |id, window, cx| pick(id, window, cx))
                            .on_close(move |window, cx| close(&(), window, cx))
                        .into_any_element(),
                )]
            }
            MenuKind::Effort => {
                let rows: Vec<PickerRow> = EFFORTS
                    .iter()
                    .map(|effort| {
                        PickerRow::new(
                            effort.map(|e| format!("{e:?}")).unwrap_or_else(|| "default".to_owned()),
                            crate::overlays::effort_label(*effort),
                            crate::overlays::effort_detail(*effort),
                        )
                    })
                    .collect();
                let pick = cx.listener(|this: &mut Self, id: &SharedString, _, cx| {
                    let effort = EFFORTS
                        .iter()
                        .copied()
                        .find(|e| e.map(|e| format!("{e:?}")).unwrap_or_else(|| "default".to_owned()) == id.as_ref());
                    if let Some(effort) = effort {
                        this.pick_effort(effort, cx);
                    }
                });
                vec![(
                    ComposerChipAnchor::Effort,
                    effort_menu("effort-menu", rows, selected, true)
                        .on_pick(move |id, window, cx| pick(id, window, cx))
                            .on_close(move |window, cx| close(&(), window, cx))
                        .into_any_element(),
                )]
            }
            MenuKind::Mode => {
                let rows: Vec<PickerRow> = MODES
                    .iter()
                    .map(|mode| PickerRow::new(format!("{mode:?}"), mode.label(), mode.description()))
                    .collect();
                let pick = cx.listener(|this: &mut Self, id: &SharedString, _, cx| {
                    if let Some(mode) = MODES.iter().copied().find(|m| format!("{m:?}") == id.as_ref()) {
                        this.pick_mode(mode, cx);
                    }
                });
                vec![(
                    ComposerChipAnchor::Mode,
                    mode_menu("mode-menu", rows, selected, true)
                        .on_pick(move |id, window, cx| pick(id, window, cx))
                            .on_close(move |window, cx| close(&(), window, cx))
                        .into_any_element(),
                )]
            }
            MenuKind::Command | MenuKind::Mention => Vec::new(),
        }
    }
}

impl Focusable for SessionView {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

/// The `/…` or `@…` token the caret sits in, if there is one.
///
/// `/` counts only at the start of a line (spec §3.9); `@` counts anywhere a
/// word starts. Returns the menu it opens, the byte offset of the sigil, and
/// what has been typed after it.
fn caret_token(draft: &str, caret: usize) -> Option<(MenuKind, usize, String)> {
    if caret > draft.len() || !draft.is_char_boundary(caret) {
        return None;
    }
    let head = &draft[..caret];
    let start = head.rfind(|c: char| c.is_whitespace()).map(|i| i + 1).unwrap_or(0);
    let token = &head[start..];
    let mut chars = token.chars();
    let kind = match chars.next()? {
        '/' if start == 0 || head[..start].ends_with('\n') => MenuKind::Command,
        '@' => MenuKind::Mention,
        _ => return None,
    };
    Some((kind, start, token[1..].to_owned()))
}

/// The line/character position of a byte offset, in the units gpui-kit's
/// textarea counts them (UTF-16, as LSP does).
fn position_of(text: &str, offset: usize) -> Position {
    let head = &text[..offset.min(text.len())];
    let line = head.matches('\n').count() as u32;
    let column = head.rsplit('\n').next().unwrap_or("").encode_utf16().count() as u32;
    Position::new(line, column)
}

/// `1.0M`, `200k` — a context limit at the width a menu row has for it.
fn compact_count(n: u64) -> String {
    match n {
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1_000_000.0),
        n if n >= 1_000 => format!("{}k", n / 1_000),
        n => n.to_string(),
    }
}

/// MSP's pressure level, which is the only thing that picks the meter's colour.
///
/// A level this build has never seen is `Normal`: the server owns the
/// thresholds, and inventing a colour for a word we cannot read would be worse
/// than drawing the calm one.
fn pressure(level: &ContextPressureLevel) -> ContextPressure {
    match level {
        ContextPressureLevel::Warning => ContextPressure::Warning,
        ContextPressureLevel::Blocked => ContextPressure::Blocked,
        _ => ContextPressure::Normal,
    }
}

/// `used/window/pressure`, the `--steps context:` payload.
fn parse_context(spec: &str) -> Option<ContextUsage> {
    let mut parts = spec.split('/');
    let used: u64 = parts.next()?.parse().ok()?;
    let window: u64 = parts.next()?.parse().ok()?;
    let level = match parts.next().unwrap_or("normal") {
        "warning" => ContextPressureLevel::Warning,
        "blocked" => ContextPressureLevel::Blocked,
        _ => ContextPressureLevel::Normal,
    };
    Some(ContextUsage {
        pressure: level,
        used_tokens: used,
        window_tokens: (window > 0).then_some(window),
    })
}

/// The protocol's effort tier as MSP spells it.
fn effort_wire(effort: ReasoningEffort) -> muse_client::schema::ReasoningEffort {
    use muse_client::schema::ReasoningEffort as Wire;
    match effort {
        ReasoningEffort::None => Wire::None,
        ReasoningEffort::Minimal => Wire::Minimal,
        ReasoningEffort::Low => Wire::Low,
        ReasoningEffort::Medium => Wire::Medium,
        ReasoningEffort::High => Wire::High,
        ReasoningEffort::Xhigh => Wire::Xhigh,
        ReasoningEffort::Ultra => Wire::Ultra,
    }
}

/// The protocol's approval mode as MSP spells it. The two enums have the same
/// four members by design (spec §3.6), so this is a rename and nothing more.
fn wire_mode(mode: PermissionMode) -> ApprovalMode {
    match mode {
        PermissionMode::AllowAll => ApprovalMode::AllowAll,
        PermissionMode::OnRequest => ApprovalMode::OnRequest,
        PermissionMode::PromptUnmatched => ApprovalMode::PromptUnmatched,
        PermissionMode::DenyUnmatched => ApprovalMode::DenyUnmatched,
    }
}

/// Page a session's whole view forward, from the beginning, into fold events.
///
/// Blocking: every call waits on the wire, so this runs on the background
/// executor. A page that comes back empty, or a `nextCursor` of `null`, is the
/// end of the view in that direction.
fn page_all(client: &MuseClient, session_id: &str) -> Vec<MuseEvent> {
    let mut out = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let params = ViewPageParams {
            session_id: session_id.to_owned(),
            limit: PAGE_LIMIT,
            cursor: cursor.clone(),
            direction: None,
            anchor: None,
        };
        let Ok(page) = client.view_page(&params) else { return out };
        let empty = page.events.is_empty();
        for event in page.events {
            let Ok(params) = serde_json::to_value(&event.params) else { continue };
            let event_cursor = params.get("viewCursor").and_then(|v| v.as_str()).map(str::to_owned);
            out.push(MuseEvent::Notification {
                method: event.method,
                params,
                cursor: event_cursor,
                session_id: Some(session_id.to_owned()),
            });
        }
        match page.next_cursor {
            Some(next) if !empty => cursor = Some(next),
            _ => return out,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slash_opens_the_command_menu_only_at_a_line_start() {
        assert_eq!(caret_token("/mo", 3).map(|t| t.0), Some(MenuKind::Command));
        assert_eq!(caret_token("run /mo", 7).map(|t| t.0), None);
        assert_eq!(caret_token("hi\n/mo", 6).map(|t| t.0), Some(MenuKind::Command));
    }

    #[test]
    fn an_at_opens_the_mention_picker_anywhere_a_word_starts() {
        let token = caret_token("look at @src/ma", 15).expect("a token");
        assert_eq!(token.0, MenuKind::Mention);
        assert_eq!(token.1, 8);
        assert_eq!(token.2, "src/ma");
    }

    #[test]
    fn a_caret_outside_a_token_closes_the_menu() {
        assert!(caret_token("plain words", 11).is_none());
        assert!(caret_token("", 0).is_none());
    }

    #[test]
    fn a_position_counts_lines_and_utf16_columns() {
        let position = position_of("one\ntwo", 7);
        assert_eq!((position.line, position.character), (1, 3));
    }

    #[test]
    fn a_context_limit_is_shortened_for_the_row() {
        assert_eq!(compact_count(1_000_000), "1.0M");
        assert_eq!(compact_count(200_000), "200k");
        assert_eq!(compact_count(512), "512");
    }
}
