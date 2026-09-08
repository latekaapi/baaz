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

use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aui::composer::{composer, composer_state_rows, ComposerIntent};
use aui::feedback::{banner, BannerActionStyle, BannerKind, BannerRun};
use aui::transcript::{status_row, StatusLead};
use aui_protocol::{ReasoningEffort, Session};
use aui_tokens::scale;
use gpui::{
    div, prelude::*, px, AnyElement, Context, Entity, EventEmitter, FocusHandle, Focusable,
    ScrollHandle, SharedString, Task, Window,
};
use gpui_kit::base::input::TextareaState;
use gpui_kit::base::{h_flex, v_flex};
use muse_adapter::MuseFold;
use muse_client::schema::{
    TurnInputPart, TurnInterruptParams, TurnStartDisposition, TurnStartParams, TurnStartResult,
    ViewPageParams,
};
use muse_client::{new_command_id, MuseClient, MuseError, MuseEvent};

use crate::conn::{self, Severity};
use crate::transcript::{self, Folds};

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
}

/// A turn the server says is running.
struct Running {
    turn_id: String,
    started: Instant,
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
    focus: FocusHandle,
    tasks: Vec<Task<()>>,
    /// Held only while a turn runs, so the elapsed time advances.
    ticker: Option<Task<()>>,
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let composer = cx.new(|cx| composer_state_rows("Ask Muse, or type / for commands", 1, 8, window, cx));
        Self {
            session_id,
            fold: MuseFold::new(),
            client,
            provider_id,
            workspace,
            composer,
            toggled: HashSet::new(),
            scroll: ScrollHandle::new(),
            follow: true,
            running: None,
            submitting: false,
            loading_history: false,
            banner: None,
            pending_prompt: None,
            focus: cx.focus_handle(),
            tasks: Vec::new(),
            ticker: None,
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

    /// Context occupancy as a percentage, when the basis has a limit at all.
    pub fn context_percent(&self) -> Option<u8> {
        let context = self.fold.side(&self.session_id)?.context.as_ref()?;
        let window = context.window_tokens?;
        if window == 0 {
            return None;
        }
        Some(((context.used_tokens as f64 / window as f64) * 100.0).round().clamp(0.0, 100.0) as u8)
    }

    /// The approval mode chip's label.
    pub fn mode_label(&self) -> &'static str {
        self.session().map(|s| s.mode.label()).unwrap_or("Auto")
    }

    /// Whether a turn is running, which is what the send button morphs on.
    pub fn busy(&self) -> bool {
        self.running.is_some() || self.submitting
    }

    /// Whether the composer is empty, which is what Escape branches on.
    pub fn draft_is_empty(&self, cx: &gpui::App) -> bool {
        self.composer.read(cx).value().trim().is_empty()
    }

    /// Point the view at the respawned child after a reconnect. The fold and
    /// the transcript are untouched: the resume streamed only the suffix.
    pub fn reconnected(&mut self, client: Arc<MuseClient>) {
        self.client = client;
    }

    /// Put text in the composer. Only the scripted `--send` uses this; a person
    /// types.
    pub fn set_draft(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        self.composer.update(cx, |state, cx| state.set_value(text, window, cx));
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
                    self.turn_failure(params, cx);
                }
                _ => {}
            }
        }
        let changed = !self.fold.apply(event).is_empty();
        // Restore a retracted prompt the moment the fold hands it back.
        if let Some(text) = self.fold.take_restored_prompt(&self.session_id) {
            self.restore_prompt(text, cx);
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
    /// transcript shows, and it stays the person's words even when a later
    /// phase prefixes the model-visible input.
    pub fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().to_string();
        if text.trim().is_empty() {
            return;
        }
        self.composer.update(cx, |state, cx| state.set_value("", window, cx));
        self.banner = None;
        self.submitting = true;
        let command_id = new_command_id();
        // The wire never gives the prompt back, so the fold has to remember it
        // before the command leaves: a retraction identifies the submission by
        // `commandId` and by nothing else.
        self.fold.record_command(&self.session_id, &command_id, &text);
        let params = TurnStartParams {
            command_id,
            session_id: self.session_id.clone(),
            input: vec![TurnInputPart::text(&text)],
            display_text: Some(text.clone()),
            // Effort is the session's own until Phase 3 gives the chip a menu.
            ..Default::default()
        };
        let client = self.client.clone();
        let call = cx.background_spawn(async move { client.turn_start(&params) });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = call.await;
            let _ = this.update(cx, |this, cx| this.sent(result, text, cx));
        }));
        cx.notify();
    }

    /// The `turn/start` ack. Admission only — the authority for what the turn
    /// is doing is always the view event.
    fn sent(&mut self, result: Result<TurnStartResult, MuseError>, text: String, cx: &mut Context<Self>) {
        match result {
            Ok(ack) => {
                if ack.disposition == TurnStartDisposition::Queued {
                    self.fold.record_queued(&self.session_id, &ack.turn_id, &ack.command_id, &text);
                    self.submitting = false;
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

    /// Route a failed command to its banner or its dialog (spec §3.8).
    fn report(&mut self, error: &MuseError, cx: &mut Context<Self>) {
        let title = conn::title(error);
        match conn::severity(error) {
            Severity::Banner => self.banner = Some(format!("{title}. {error}")),
            Severity::Dialog => cx.emit(SessionEvent::Dialog { title, detail: error.to_string() }),
        }
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

    // ----------------------------------------------------------------- render

    /// The centre pane: transcript, status row, banner, composer.
    pub fn render_centre(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if let Some(text) = self.pending_prompt.take() {
            self.composer.update(cx, |state, cx| state.set_value(text, window, cx));
        }
        let transcript = self.render_transcript(window, cx);
        let status = self.render_status();
        let banner = self.render_banner(cx);
        let composer = self.render_composer(cx);
        v_flex()
            .size_full()
            .child(transcript)
            .children(status)
            .children(banner)
            .child(composer)
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

    fn render_composer(&self, cx: &mut Context<Self>) -> AnyElement {
        let draft = self.composer.read(cx).value().to_string();
        let mut composer = composer("composer", &self.composer, aui_icons::Provider::Muse, self.model())
            .docked(true)
            .mode(self.mode_label())
            // Phase 3 gives these chips their menus; the values are already the
            // server's, read back out of the fold.
            .effort(ReasoningEffort::default().label())
            .streaming(self.busy())
            .can_send(!draft.trim().is_empty())
            .on_intent({
                let handler = cx.listener(|this: &mut Self, intent: &ComposerIntent, window, cx| match intent {
                    ComposerIntent::Send => this.send(window, cx),
                    ComposerIntent::Stop => this.interrupt(cx),
                    _ => {}
                });
                move |intent, window, cx| handler(&intent, window, cx)
            });
        if let Some(percent) = self.context_percent() {
            composer = composer.context_percent(percent);
        }
        composer.into_any_element()
    }
}

impl Focusable for SessionView {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus.clone()
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
