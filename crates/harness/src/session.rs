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
use aui::transcript::{
    AssistantTurnAction, LinkTarget, TextSelection, ToolCardIntent, ToolGroupIntent, UserTurnAction,
    needs_you_banner, retry_row, status_row, StatusLead,
};
use aui_icons::IconName;
use aui_protocol::{Block, PermissionMode, PlanState, ReasoningEffort, Session, Turn};
use aui_tokens::scale;
use gpui::{
    div, list, prelude::*, px, AnyElement, ClipboardEntry, ClipboardItem, Context, Entity,
    EventEmitter, ExternalPaths, FocusHandle, Focusable, ListAlignment, ListState, SharedString,
    Task, Window,
};
use gpui_kit::base::input::{InputEvent, Position, TextareaState};
use gpui_kit::component::input::Textarea;
use gpui_kit::base::{h_flex, v_flex};
use muse_adapter::MuseFold;
use muse_client::schema::{
    ApprovalDecideParams, ApprovalListPendingParams, ApprovalMode, ApprovalResolutionSummary,
    ContextPressureLevel, ContextUsage, ErrorKind, ForkCutPoint, ModelCatalogEntry,
    ModelListParams, ModelSelection, SessionCompactParams, SessionForkParams,
    SessionSetApprovalModeParams, SessionSetModelParams, SessionUserShellParams, TurnInputPart,
    TurnInterruptParams, TurnStartDisposition, TurnStartParams, TurnStartResult, TurnSteerParams,
    TurnUnqueueParams, UserInputAnswer, UserInputAnswerParams, UserInputCancelParams,
    UserInputClarification, UserInputClarifyParams, UserInputSelectionMode, ViewPageParams,
};
use muse_client::{new_command_id, MuseClient, MuseError, MuseEvent};

use crate::conn::{self, Severity};
use crate::overlays::{Command, Menu, MenuKind, Overlays, EFFORTS, MODES};
use crate::transcript::{self, Cards, Folds, FullOutput, FullOutputState, PlanAction};
use crate::{attachments, files, full_output, history, images, plan, skills};

/// How often the "Working… 12 s" row re-reads the clock: 1 Hz, the finest
/// the elapsed row can show (P2 — the old 250 ms whole-view ticker rebuilt
/// the transcript four times a second on top of every streaming delta).
const TICK: Duration = Duration::from_secs(1);
/// How often a countdown — a question's auto-resolution, a scheduled retry —
/// re-reads it. Once a second, because that is all a countdown in seconds can
/// show.
const COUNTDOWN_TICK: Duration = Duration::from_secs(1);
/// How long the app waits before retrying a command the wire refused with
/// `overloaded` or `backpressured`. The wire carries no `retryAfter`, so this is
/// a client-side choice and a deliberately unhurried one.
const RETRY_BACKOFF: Duration = Duration::from_secs(3);
/// `view/page` takes 1–1000; the transport uses the ceiling and so does the
/// history backfill.
const PAGE_LIMIT: u32 = 1000;
/// The transcript's own padding, matching the assistant screen's `.tr`.
const TRANSCRIPT_PAD_X: f32 = scale::SP_7;
/// Top inset: the first turn's first line must clear the header (C8 — the
/// owner's screenshot showed it cut off). Same step as the horizontal gutter.
const TRANSCRIPT_PAD_TOP: f32 = scale::SP_7;
/// The key context the transcript list wears, so ⌘C reaches the selection
/// copy without ever matching inside the composer or a card field (C8b).
pub const TRANSCRIPT_CONTEXT: &str = "HarnessTranscript";
/// The ⌘C predicate for that copy: the transcript holds focus context, but
/// never the composer, a card field or the rename field.
pub const TRANSCRIPT_COPY_KEYS: &str =
    "HarnessTranscript && !HarnessComposer && !field && !HarnessRename";
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
    /// `session/fork` succeeded: open the new session as the active one.
    Forked {
        /// The new session's id. Its resume envelope has already been served.
        session_id: String,
        /// The envelope's own `session` object, so the new view can fold it and
        /// draw the `ForkedFrom` marker its `forkedFrom` carries.
        session: serde_json::Value,
    },
    /// `/logout`.
    Logout,
    /// `/status` or `/usage`: the application owns the dialog stack.
    Status {
        /// The lines of the dialog body, already formatted.
        detail: String,
    },
    /// `/name <text>`: rename the active session, or clear the name when the
    /// text was empty. The store is the application's (spec §3.7).
    Rename {
        /// The new name, or `None` to fall back to what the index calls it.
        name: Option<String>,
    },
    /// `/name` with nothing after it: open the sidebar row's inline field.
    RenameStart,
    /// `/hide`: take this session out of the list.
    Hide,
    /// `/empty`: show sessions with no turns in the sidebar, or hide them
    /// again. A window-level filter, so the application owns the state.
    ToggleEmpty,
    /// `/resume`: open the session picker.
    Resume,
    /// `/fork` with nothing named: open the turn picker over the session's
    /// completed assistant turns.
    ForkPicker,
    /// The deferred switch's first backfill batch applied: the application
    /// may now swap the pending view in (C2 — no empty-state flash between
    /// sessions).
    HistoryReady,
    /// "Send anyway" on the pay-as-you-go banner: the person accepts the bill
    /// for the rest of this app run.
    TierOverride,
    /// "Check again" on the unknown-plan banner: re-probe the billing tier.
    TierRecheck,
}

/// The billing guard's banner over the composer (Phase 5 A1), as the
/// application decided it.
///
/// The session view draws it and refuses to submit while `blocking` is set;
/// what the two buttons mean is the application's business, so both come back
/// as [`SessionEvent`]s.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TierBanner {
    /// The line the person reads.
    pub text: String,
    /// Whether a turn is refused until the person presses "Send anyway".
    pub blocking: bool,
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
    /// The child, or `None` for a replayed capture, which has no child at all.
    client: Option<Arc<MuseClient>>,
    /// A `--replay` session: the transcript is a file, and every command is
    /// refused rather than quietly dropped.
    replay: bool,
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
    /// Virtualized transcript list (gpui `list()`, bottom-aligned, one item
    /// per turn) and the item count it was last synced to. Only visible rows
    /// are built and laid out per frame; `splice` keeps indices stable across
    /// folds (C1). Tail-follow rides `is_scrolled_to_end`/`scroll_to_end`
    /// with the `TAIL_SLACK` semantics below.
    list_state: ListState,
    list_len: usize,
    /// What `render_transcript` reads every frame (C1): one snapshot shared
    /// by steady-state frames, refreshed only when the fold changes (length
    /// drift or `follow`), so per-frame cost stays bounded as the transcript
    /// grows. Event handlers keep reading the live fold.
    cached_turns: Rc<Vec<Turn>>,
    /// The truncated-output map for the cached turns (same refresh rule).
    cached_full_output: HashMap<String, FullOutput>,
    /// Set by every event that changed the transcript; the next frame consumes
    /// it and scrolls to the tail if the reader was already there.
    follow: bool,
    /// The transcript's current text selection (library selection model):
    /// one cell at a time, cleared on Escape; ⌘C copies it (C8b). The turn
    /// components do not forward selection intents yet, so this holds the
    /// state and the binding until the library wires the turns through.
    text_selection: Option<TextSelection>,
    /// Last elapsed second the turn ticker painted, so the 1 Hz clock
    /// notifies only when the displayed number changes (P2).
    last_tick_secs: Option<u64>,
    running: Option<Running>,
    /// A `turn/start` is in flight and no `turn/started` has arrived yet.
    submitting: bool,
    /// History is still being paged in behind the live stream.
    loading_history: bool,
    /// The inline banner over the composer: one recoverable command error.
    banner: Option<String>,
    /// The banner's action, when the error is one the person can do something
    /// about: the label and what pressing it does.
    banner_action: Option<BannerAction>,
    /// The billing guard's banner, and whether it is refusing turns.
    tier_banner: Option<TierBanner>,
    /// Which approval's feedback field is open, as `(approvalId, choiceId)`.
    feedback_open: Option<(String, String)>,
    /// The feedback field itself. The card never owns text; this does.
    feedback: Entity<TextareaState>,
    /// Which question block has its "Explain instead" field open.
    clarify_open: Option<String>,
    /// That field.
    clarify: Entity<TextareaState>,
    /// What is selected on each pending question, keyed by the question block's
    /// id. The card is stateless, so the selection lives here until it is sent.
    selections: HashMap<String, Vec<usize>>,
    /// Which option previews are expanded, keyed by the question block's id.
    previews: HashMap<String, Vec<usize>>,
    /// Answers gathered so far for a multi-question request, keyed by
    /// `userInputId` then `questionId`. MSP settles the whole prompt at once, so
    /// the app holds the earlier answers until the last question is answered.
    answers: HashMap<String, HashMap<String, muse_client::schema::UserInputAnswer>>,
    /// When each pending question's auto-resolution clock started, keyed by
    /// `userInputId`. MSP sends a duration, never a deadline.
    question_started: HashMap<String, Instant>,
    /// When the live `turn/retryScheduled` was observed, for the same reason.
    retry_started: Option<(String, Instant)>,
    /// A 1 s clock, held only while a countdown is on screen.
    countdown: Option<Task<()>>,
    /// A reconnect or a resume happened; `approval/listPending` needs
    /// re-reading before the next frame is trusted.
    refresh_pending: bool,
    /// Whether `initialize` granted the `userShell` capability. Without it the
    /// `!` path is disabled and says so rather than failing on the wire.
    user_shell: bool,
    /// Session id → the label the sidebar shows for it, so a `ForkedFrom`
    /// marker can name its source rather than print a uuid.
    titles: HashMap<String, String>,
    /// Draw the cards settled rather than entering.
    ///
    /// A `--screenshot` run renders a handful of frames and then quits, so a
    /// staggered button that is still fading in is simply missing from the PNG.
    /// The capture wants the card as a person sees it a moment later.
    at_rest: bool,
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
    /// Files waiting to go out with the next turn, as extracted text.
    files: Vec<attachments::AttachedFile>,
    /// Monotonic id source for file chips.
    file_seq: u64,
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
    /// "Show full output" fetches by tool block id: what the server's stored
    /// bytes came back as. The fold keeps the fetch handle (`outputRef`); this
    /// keeps the result, so the card renders fetched lines without the fold
    /// ever changing.
    full_outputs: HashMap<String, full_output::Fetch>,
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
        client: Option<Arc<MuseClient>>,
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
        let feedback = cx.new(|cx| composer_state_rows("Why not? Muse reads this.", 2, 4, window, cx));
        let clarify = cx.new(|cx| composer_state_rows("Say what you would rather Muse did", 2, 4, window, cx));
        let workspace_key = workspace.clone();
        Self {
            session_id,
            fold: MuseFold::new(),
            client,
            replay: false,
            provider_id,
            workspace,
            composer,
            overlays,
            toggled: HashSet::new(),
            list_state: ListState::new(
                0,
                ListAlignment::Bottom,
                // Overdraw covers the tail-slack zone twice over, so rows
                // entering at the tail are already measured (C1).
                px(TAIL_SLACK * 2.0),
            ),
            list_len: 0,
            cached_turns: Rc::new(Vec::new()),
            cached_full_output: HashMap::new(),
            follow: true,
            text_selection: None,
            last_tick_secs: None,
            running: None,
            submitting: false,
            loading_history: false,
            banner: None,
            banner_action: None,
            tier_banner: None,
            feedback_open: None,
            feedback,
            clarify_open: None,
            clarify,
            selections: HashMap::new(),
            previews: HashMap::new(),
            answers: HashMap::new(),
            question_started: HashMap::new(),
            retry_started: None,
            countdown: None,
            refresh_pending: false,
            user_shell: true,
            titles: HashMap::new(),
            at_rest: false,
            pending_prompt: None,
            models: Vec::new(),
            effort: None,
            plan: false,
            plan_previous_mode: None,
            plan_turn: None,
            plan_seq: 0,
            images: Vec::new(),
            image_seq: 0,
            files: Vec::new(),
            file_seq: 0,
            dragging: false,
            plus_open: false,
            history: history::Cursor::new(history::read(&workspace_key)),
            workspace_key,
            unqueueing: HashMap::new(),
            meter_open: false,
            full_outputs: HashMap::new(),
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

    /// Whether a card field (an approval's feedback, a question's
    /// clarification) is open, which is what Enter and Escape branch on.
    pub fn card_field_open(&self) -> bool {
        self.feedback_open.is_some() || self.clarify_open.is_some()
    }

    /// Whether a pending card should own the digit keys this frame (spec §3.9).
    ///
    /// Only with an empty draft: `1` in a half-typed sentence is a `1`, and a
    /// person mid-thought must never have a keystroke mean "allow".
    pub fn card_has_keys(&self, cx: &gpui::App) -> bool {
        self.draft_is_empty(cx) && !self.card_field_open() && self.newest_pending_approval().is_some()
    }

    /// Pick the n-th choice of the newest pending approval (`1`–`9`).
    pub fn choose_nth(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let step = format!("choose:{}", index + 1);
        self.step(&step, window, cx);
    }

    /// Whether plan mode is on, for the app's Shift+Tab.
    pub fn plan_mode(&self) -> bool {
        self.plan
    }

    /// What the application knows and the session does not: what the other
    /// sessions in this workspace are called (for a `ForkedFrom` marker), and
    /// whether `initialize` granted `userShell`.
    pub fn set_context(&mut self, titles: HashMap<String, String>, user_shell: bool) {
        self.titles = titles;
        self.user_shell = user_shell;
    }

    /// The billing guard's banner, or `None` when the login's tier is fine.
    ///
    /// Pushed by the application, which owns the probe: the session view knows
    /// only what to draw and what to refuse.
    pub fn set_tier_banner(&mut self, banner: Option<TierBanner>, cx: &mut Context<Self>) {
        if self.tier_banner != banner {
            self.tier_banner = banner;
            cx.notify();
        }
    }

    /// Draw the cards settled, for a `--screenshot` run.
    pub fn set_at_rest(&mut self, at_rest: bool) {
        self.at_rest = at_rest;
    }

    /// Fold a session envelope the app was handed as a **result**.
    ///
    /// `session/start`, `session/resume` and `session/fork` all return the
    /// session object that `session/started` would have carried, and the view
    /// stream never repeats it. Without this the provenance a fork records —
    /// `session.forkedFrom` — would never reach the transcript, and the new
    /// session would open with no sign of where it came from.
    pub fn seed_session(&mut self, session: serde_json::Value, cx: &mut Context<Self>) {
        if session.is_null() {
            return;
        }
        let session_id = self.session_id.clone();
        self.fold.apply(MuseEvent::Notification {
            method: "session/started".to_owned(),
            params: serde_json::json!({ "session": session }),
            cursor: None,
            session_id: Some(session_id),
        });
        self.follow = true;
        cx.notify();
    }

    /// Point the view at the respawned child after a reconnect. The fold and
    /// the transcript are untouched: the resume streamed only the suffix.
    pub fn reconnected(&mut self, client: Arc<MuseClient>) {
        self.client = Some(client);
        // A reconnect can have missed an `approval/request`, and the server does
        // not re-issue one it has already sent. The pull dual closes that hole.
        self.refresh_pending = true;
    }

    /// The child, or the banner explaining why there isn't one.
    ///
    /// Every command goes through here, so "this transcript came out of a file"
    /// is said once and refused once, instead of each call site quietly doing
    /// nothing and leaving the person to wonder.
    fn wire_client(&mut self, cx: &mut Context<Self>) -> Option<Arc<MuseClient>> {
        if let Some(client) = self.client.clone() {
            return Some(client);
        }
        if self.replay {
            self.banner = Some("Replayed capture — read-only".to_owned());
            self.banner_action = None;
            cx.notify();
        }
        None
    }

    /// Fold a capture file into this view: `--replay` (decision A0).
    ///
    /// The same `<-- ` lines the fixture test folds, through the same fold, with
    /// no child and no wire. It is how most of Phase 4's screenshots are taken,
    /// and it costs nothing at all.
    pub fn load_replay(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        self.replay = true;
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) => {
                self.banner = Some(format!("{}: {error}", path.display()));
                cx.notify();
                return;
            }
        };
        let mut sent: Vec<(String, String)> = Vec::new();
        for (number, line) in text.lines().enumerate() {
            // A capture holds both directions. The client-to-server half is what
            // the fixture tests skip, but it is the only record of what a turn
            // was *sent* with — and that is what an error card's retry needs, so
            // the prompts are read back out of it here.
            if let Some(body) = line.strip_prefix("--> ") {
                sent.extend(submitted_text(body));
                continue;
            }
            let Some(body) = line.strip_prefix("<-- ") else { continue };
            match muse_client::frame::parse_line(body) {
                Ok(Some(frame)) => {
                    if let Some(event) = MuseEvent::from_frame(frame) {
                        self.fold.apply(event);
                    }
                }
                Ok(None) => {}
                Err(error) => eprintln!("harness: {}:{}: {error}", path.display(), number + 1),
            }
        }
        // The capture names its own session; the view was opened on whatever the
        // caller guessed, so it follows the file rather than the guess.
        if let Some(id) = self.fold.session_ids().next() {
            self.session_id = id.to_owned();
        }
        for (command_id, text) in sent {
            self.fold.record_command(&self.session_id, &command_id, &text);
        }
        self.observe_clocks(cx);
        self.follow = true;
        cx.notify();
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
                        self.last_tick_secs = None;
                        self.start_ticker(cx);
                    }
                }
                "turn/completed" => {
                    let ours = params.get("turnId").and_then(|v| v.as_str());
                    if self.running.as_ref().is_some_and(|r| Some(r.turn_id.as_str()) == ours) {
                        self.running = None;
                        self.ticker = None;
                        self.last_tick_secs = None;
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
        // View state the fold does not report: prompt text handed back below,
        // and whether a turn is running, all change what the frame shows.
        let was_running = self.running.is_some();
        let was_submitting = self.submitting;
        let changed = !self.fold.apply(event).is_empty();
        // Restore a retracted prompt the moment the fold hands it back — unless
        // the unqueue was a Remove (the text is meant to be gone) or a Steer
        // (the text is going straight back out on the wire).
        let restored = self.fold.take_restored_prompt(&self.session_id);
        let unqueued_kind = unqueued.as_deref().and_then(|id| self.unqueueing.remove(id));
        if let Some(text) = restored {
            match unqueued_kind {
                Some(Unqueue::Remove) => {}
                Some(Unqueue::Steer) => self.steer_text(text, cx),
                _ => self.restore_prompt(text, cx),
            }
        }
        // Notify less (P1): a streaming delta that changed nothing visible
        // must not rebuild the whole transcript. Unchanged deltas arrive
        // constantly while a reply streams; only fold changes and view-state
        // changes earn a frame.
        let mut view_changed = unqueued.is_some()
            || was_running != self.running.is_some()
            || was_submitting != self.submitting;
        if changed {
            self.follow = true;
            view_changed = true;
        }
        // Every event can start or end a countdown; the clock is started and
        // stopped in one place rather than by each event that might matter.
        self.observe_clocks(cx);
        if std::mem::take(&mut self.refresh_pending) {
            self.refresh_pending_now(cx);
            view_changed = true;
        }
        if view_changed {
            cx.notify();
        }
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
        let Some(client) = self.wire_client(cx) else { return };
        self.loading_history = true;
        cx.notify();
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
                // The deferred switch's cue: the application swaps this view
                // in now, instead of having flashed the empty state (C2).
                cx.emit(SessionEvent::HistoryReady);
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
        if text.trim().is_empty() && self.images.is_empty() && self.files.is_empty() {
            return;
        }
        self.composer.update(cx, |state, cx| state.set_value("", window, cx));
        // `!` is the shell escape hatch (research §1.12): a command, not a turn,
        // outside any turn, and still subject to the approval policy.
        if let Some(command) = text.strip_prefix('!') {
            if !command.trim().is_empty() {
                self.run_user_shell(command.trim().to_owned(), cx);
                return;
            }
        }
        // A `/` command typed in full and sent is the command, not a prompt.
        // The menu is one way to reach these; typing is the other, and it is
        // the only way to reach the one that takes an argument (`/name`).
        if let Some((command, argument)) = Command::parse_line(&text) {
            self.run_command_with(command, argument.to_owned(), window, cx);
            return;
        }
        self.submit(text, cx);
    }

    /// Send `text` without touching the composer — the scripting hook.
    pub fn send_text(&mut self, text: String, cx: &mut Context<Self>) {
        self.submit(text, cx);
    }

    fn submit(&mut self, text: String, cx: &mut Context<Self>) {
        // Nothing leaves a replayed capture, and nothing about the person's
        // draft or their history is touched on the way to finding that out.
        if self.wire_client(cx).is_none() {
            self.restore_prompt(text, cx);
            return;
        }
        // The billing guard. A pay-as-you-go login bills every turn as API
        // usage, so the turn does not leave until the person has said once,
        // out loud, that they meant it. The draft goes back in the composer:
        // the banner explaining why is already above it.
        if self.tier_banner.as_ref().is_some_and(|b| b.blocking) {
            self.restore_prompt(text, cx);
            return;
        }
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
        self.files.clear();
        let Some(client) = self.wire_client(cx) else { return };
        let planning = self.plan;
        let call = cx.background_spawn(async move { client.turn_start(&params) });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = call.await;
            let _ = this.update(cx, |this, cx| this.sent(result, text, planning, cx));
        }));
        cx.notify();
    }

    /// The turn's content parts: one text part per attached file, then the
    /// prompt text, then every attached image.
    fn parts(&self, text: String) -> Vec<TurnInputPart> {
        let mut parts = Vec::new();
        parts.extend(self.files.iter().map(attachments::AttachedFile::part));
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
                // The turn never left, so the person keeps their words — and,
                // when the wire only said "not now", the banner offers to send
                // them again rather than making the person press Enter twice.
                self.report_retryable(&error, BannerAction::RetryTurn(text.clone()), cx);
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
        self.files.clear();
        let Some(client) = self.wire_client(cx) else { return };
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
        let Some(client) = self.wire_client(cx) else { return };
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
        if self.wire_client(cx).is_none() {
            return;
        }
        self.unqueueing.insert(turn_id.to_owned(), why);
        let params = TurnUnqueueParams {
            command_id: new_command_id(),
            session_id: self.session_id.clone(),
            turn_id: turn_id.to_owned(),
        };
        let Some(client) = self.wire_client(cx) else { return };
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
        let Some(client) = self.wire_client(cx) else { return };
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
        let Some(client) = self.wire_client(cx) else { return };
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
        let Some(client) = self.wire_client(cx) else { return };
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
        let Some(client) = self.wire_client(cx) else { return };
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

    /// Keep the elapsed time honest while a turn runs: 1 Hz, notifying only
    /// when the displayed second changes, so the clock costs one frame per
    /// second instead of four whole-transcript rebuilds (P2).
    fn start_ticker(&mut self, cx: &mut Context<Self>) {
        if self.ticker.is_some() {
            return;
        }
        self.ticker = Some(cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(TICK).await;
            let alive = this.update(cx, |this, cx| {
                let secs = this.running.as_ref().map(|r| r.started.elapsed().as_secs());
                if secs != this.last_tick_secs {
                    this.last_tick_secs = secs;
                    cx.notify();
                }
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
        let (items, sections) = plan::steps(&reply);
        if items.is_empty() {
            return;
        }
        self.plan_seq += 1;
        let id = format!("plan-{}", self.plan_seq);
        self.fold.append_client_block(
            &self.session_id,
            &id,
            Block::Plan { id: id.clone(), items, sections, state: PlanState::Proposed },
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
                let Block::Plan { items, sections, .. } = &block else { return };
                let replaced =
                    Block::Plan { id: id.to_owned(), items: items.clone(), sections: sections.clone(), state };
                self.fold.replace_client_block(&self.session_id, id, replaced);
                self.follow = true;
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
    ///
    /// `argument` is whatever followed the command when it was typed; the `/`
    /// menu always passes an empty one, because a menu row carries no text.
    pub fn run_command(&mut self, command: Command, window: &mut Window, cx: &mut Context<Self>) {
        self.run_command_with(command, String::new(), window, cx);
    }

    /// [`SessionView::run_command`] with whatever followed the command.
    fn run_command_with(&mut self, command: Command, argument: String, window: &mut Window, cx: &mut Context<Self>) {
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
            // `/fork` with nothing named opens the turn picker; `/fork <n>`
            // forks the nth newest completed turn with no picker in between.
            Command::Fork => {
                let argument = argument.trim();
                if argument.is_empty() {
                    cx.emit(SessionEvent::ForkPicker);
                } else {
                    match argument.parse::<usize>() {
                        Ok(n) => self.fork_nth(n, cx),
                        Err(_) => self.set_banner("`/fork` takes a turn number, e.g. `/fork 2`.", None, cx),
                    }
                }
            }
            // `/name Fix the parser` renames; `/name` on its own opens the
            // row's field, and `/name ` with nothing after it clears the name.
            Command::Name => {
                let text = argument.trim();
                match (text.is_empty(), argument.is_empty()) {
                    (true, true) => cx.emit(SessionEvent::RenameStart),
                    (true, false) => cx.emit(SessionEvent::Rename { name: None }),
                    _ => cx.emit(SessionEvent::Rename { name: Some(text.to_owned()) }),
                }
            }
            Command::Hide => cx.emit(SessionEvent::Hide),
            Command::Empty => cx.emit(SessionEvent::ToggleEmpty),
            Command::Resume => cx.emit(SessionEvent::Resume),
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

    /// The `+` menu's "Attach file or photo", and the drop of files from
    /// Finder. Image extensions attach as images; everything else is extracted
    /// to text by `attachments` (MSP has no file part to carry the bytes).
    pub fn attach_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        for path in paths {
            let ext =
                path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_lowercase();
            if matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp") {
                self.image_seq += 1;
                match images::from_path(format!("img-{}", self.image_seq), &path) {
                    Ok(image) => self.images.push(image),
                    Err(reason) => self.banner = Some(reason),
                }
                continue;
            }
            if self.files.len() >= attachments::MAX_FILES {
                self.banner = Some(format!(
                    "at most {} files per turn; the rest were not attached",
                    attachments::MAX_FILES
                ));
                continue;
            }
            self.file_seq += 1;
            match attachments::from_path(format!("file-{}", self.file_seq), &path) {
                Ok(file) => self.files.push(file),
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

    /// Open the system picker for a file. Image extensions attach as images;
    /// everything else is extracted to text, so the prompt accepts any file.
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

    /// Type a menu sigil (`@` or `/`) into the draft so its caret menu opens:
    /// what the `+` menu's Mention and commands rows do. The text goes through
    /// `set_draft`, so the caret lands at the end and the popover opens exactly
    /// as if the person had typed it.
    fn insert_sigil(&mut self, sigil: &str, window: &mut Window, cx: &mut Context<Self>) {
        let (draft, _) = self.draft_and_caret(cx);
        let mut next = draft;
        if !next.is_empty() && !next.ends_with(char::is_whitespace) {
            next.push(' ');
        }
        next.push_str(sigil);
        self.set_draft(next, window, cx);
        self.on_draft_changed(cx);
        self.focus_composer(window, cx);
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
            // The Phase 5 session operations, so their screenshots come from a
            // command line rather than from a pointer.
            "name" => self.run_command_with(Command::Name, rest.to_owned(), window, cx),
            "hide" => cx.emit(SessionEvent::Hide),
            "resume" => cx.emit(SessionEvent::Resume),
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
            "file" => self.attach_paths(vec![PathBuf::from(rest)], cx),
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
            // `session/userShell`: free on every provider, and the only way to
            // raise a real approval without spending a turn.
            "shell" => self.run_user_shell(rest.to_owned(), cx),
            "setmode" => {
                match MODES.iter().copied().find(|m| format!("{m:?}").eq_ignore_ascii_case(rest) || m.label().eq_ignore_ascii_case(rest)) {
                    Some(mode) => self.set_mode(mode, cx),
                    None => eprintln!("harness: unknown approval mode `{rest}`"),
                }
            }
            // The n-th choice of the newest pending approval, 1-based, exactly
            // as the digits on the card are.
            "choose" => {
                let Ok(n) = rest.parse::<usize>() else { return };
                let Some((approval_id, choices)) = self.newest_pending_approval() else { return };
                let Some(choice) = choices.get(n.saturating_sub(1)) else { return };
                if choice.accepts_feedback && self.feedback_open.is_none() {
                    // The same two-press dance a person does: the first press
                    // opens the field, `feedback:` fills it, the second sends.
                    self.toggle_feedback(approval_id, Some(choice.id.clone()), window, cx);
                    return;
                }
                let feedback = self.feedback_open.is_some().then(|| self.feedback.read(cx).value().to_string());
                self.decide_approval(approval_id, choice.id.clone(), feedback, cx);
            }
            // Type into whichever field is open — an approval's feedback or a
            // question's clarification — without sending it.
            "feedback" => {
                let field = if self.feedback_open.is_some() { self.feedback.clone() } else { self.clarify.clone() };
                field.update(cx, |state, cx| state.set_value(rest.to_owned(), window, cx));
                cx.notify();
            }
            "answer" | "answers" => {
                let Some((block_id, labels)) = self.newest_pending_question() else { return };
                for wanted in rest.split('|').map(str::trim).filter(|s| !s.is_empty()) {
                    if let Some(index) = labels.iter().position(|l| l == wanted) {
                        self.select_option(block_id.clone(), index, cx);
                    } else {
                        eprintln!("harness: no option labelled `{wanted}`");
                    }
                }
                self.answer_question(block_id, cx);
            }
            // `clarify` with no text only opens the field, which is what a
            // screenshot of the open field wants; with text it opens, fills and
            // sends, which is what the round-trip wants.
            "clarify" => {
                let Some((block_id, _)) = self.newest_pending_question() else { return };
                self.clarify_open = Some(block_id.clone());
                self.clarify.update(cx, |state, cx| state.set_value(rest.to_owned(), window, cx));
                if rest.trim().is_empty() {
                    cx.notify();
                    return;
                }
                self.send_clarification(&block_id, window, cx);
            }
            // The n-th option's preview on the newest question, 0-based.
            "preview" => {
                let Ok(n) = rest.parse::<usize>() else { return };
                if let Some((block_id, _)) = self.newest_pending_question() {
                    self.toggle_preview(block_id, n, cx);
                }
            }
            // Pick without sending, for a capture of a half-answered card.
            "select" => {
                let Some((block_id, labels)) = self.newest_pending_question() else { return };
                if let Some(index) = labels.iter().position(|l| l == rest) {
                    self.select_option(block_id, index, cx);
                }
            }
            "skip" => {
                if let Some((block_id, _)) = self.newest_pending_question() {
                    self.skip_question(block_id, cx);
                }
            }
            // Transcript inspection (Task C): jump without touching the
            // pointer, and open every tool group for its screenshot.
            "top" => {
                self.follow = false;
                self.list_state.scroll_to(gpui::ListOffset { item_ix: 0, offset_in_item: px(0.0) });
                cx.notify();
            }
            "end" => {
                self.list_state.scroll_to_end();
                cx.notify();
            }
            "bench" => {
                // Frame-stats driver (Task C item 3): N back-to-back frames
                // so HARNESS_FRAME_STATS percentiles have samples on a static
                // replay, which would otherwise idle after a few frames.
                let n: usize = rest.parse().unwrap_or(240);
                self.tasks.push(cx.spawn(async move |this, cx| {
                    for _ in 0..n {
                        cx.background_executor().timer(std::time::Duration::from_millis(16)).await;
                        if this.update(cx, |_, cx| cx.notify()).is_err() {
                            return;
                        }
                    }
                }));
            }
            "mid" => {
                self.follow = false;
                let mid = self.list_len / 2;
                self.list_state.scroll_to(gpui::ListOffset { item_ix: mid, offset_in_item: px(0.0) });
                cx.notify();
            }
            "expand-groups" => {
                self.expand_all_groups(cx);
            }
            "fork" => self.fork(None, cx),
            "retry" => {
                if let Some(turn_id) = self.newest_failed_turn() {
                    self.retry_turn(turn_id, cx);
                }
            }
            // `wait` is handled by the runner, which is the only thing that can
            // let the wire catch up; seeing it here means it slipped through.
            "wait" => {}
            other => eprintln!("harness: unknown step `{other}`"),
        }
    }

    /// The newest approval still awaiting a decision, and its current choices.
    ///
    /// Read from the transcript rather than from the pending map, because the
    /// transcript is in wire order and "newest" is a question about order.
    fn newest_pending_approval(&self) -> Option<(String, Vec<aui_protocol::ApprovalChoice>)> {
        let session = self.session()?;
        session.turns.iter().rev().flat_map(|turn| turn.blocks().iter().rev()).find_map(|block| match block {
            Block::Approval { id, state, choices, .. } if *state == aui_protocol::ApprovalState::Pending => {
                Some((id.clone(), choices.clone()))
            }
            _ => None,
        })
    }

    /// The newest question still awaiting an answer, and its option labels.
    fn newest_pending_question(&self) -> Option<(String, Vec<String>)> {
        let session = self.session()?;
        session.turns.iter().rev().flat_map(|turn| turn.blocks().iter().rev()).find_map(|block| match block {
            Block::Question { id, options, answer: None, .. } => {
                Some((id.clone(), options.iter().map(|o| o.label.clone()).collect()))
            }
            _ => None,
        })
    }

    /// F10. The first shell command in this session's transcript, as a title.
    ///
    /// The fold is already in memory, so this costs nothing at all — and it
    /// reaches the case `session/read` cannot, because the history of a session
    /// nobody has loaded is not served.
    pub fn first_shell_title(&self) -> Option<String> {
        let session = self.session()?;
        session
            .turns
            .iter()
            .flat_map(|turn| turn.blocks().iter())
            .find_map(|block| match block {
                Block::ToolCall { kind: aui_protocol::ToolKind::Shell, target, .. } => {
                    crate::sessions::shell_title(target)
                }
                _ => None,
            })
    }

    /// The newest turn that ended in an error card, for `--steps retry`.
    fn newest_failed_turn(&self) -> Option<String> {
        let session = self.session()?;
        session.turns.iter().rev().find_map(|turn| {
            turn.blocks()
                .iter()
                .any(|block| matches!(block, Block::Error { .. }))
                .then(|| turn.id().to_owned())
        })
    }

    // ----------------------------------------------------------------- render

    /// The centre pane: transcript, status row, banner, composer.
    pub fn render_centre(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if let Some(text) = self.pending_prompt.take() {
            self.composer.update(cx, |state, cx| state.set_value(text, window, cx));
        }
        // A `--screenshot` run that asked for an approval waits for one; this
        // is where the capture learns that it arrived (finding F9).
        crate::shot::set_pending_approval(self.newest_pending_approval().is_some());
        let transcript = self.render_transcript(window, cx);
        let status = self.render_status();
        let needs_you = self.render_needs_you(cx);
        let banner = self.render_banner(cx);
        let tier_banner = self.render_tier_banner(cx);
        let queue = self.render_queue(cx);
        let caret_menu = self.render_caret_menu(cx);
        let composer = self.render_composer(cx);
        let drop = self.dragging;
        v_flex()
            .size_full()
            .relative()
            .child(transcript)
            .children(status)
            .children(needs_you)
            .children(banner)
            .children(tier_banner)
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
        let frame_start = std::time::Instant::now();
        let empty = |view: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
            // A replayed capture is read-only, so its empty state offers
            // nothing to type: the chips would be three buttons that refuse.
            let pick = (!view.replay).then(|| {
                let pick = cx.listener(|this: &mut Self, index: &usize, window, cx| {
                    if let Some(text) = transcript::suggestion(*index) {
                        this.set_draft(text.to_owned(), window, cx);
                        this.focus_composer(window, cx);
                    }
                });
                std::rc::Rc::new(move |index: usize, window: &mut Window, cx: &mut gpui::App| {
                    pick(&index, window, cx)
                }) as transcript::PickSuggestion
            });
            let _ = window;
            transcript::empty_state(&view.workspace, pick, cx)
        };
        // Loading is not empty: while the first backfill batch is still on
        // the wire the old view stays up (C2, `Harness::open`), and when no
        // old view exists this neutral row stands in — never `empty_state`.
        // Steady-state frames share one snapshot: refresh only when the fold
        // grew/shrank under us or `follow` says content changed.
        let live_len = self.fold.session(&self.session_id).map(|s| s.turns.len()).unwrap_or(0);
        if live_len != self.cached_turns.len() || self.follow {
            self.refresh_render_cache();
        }
        if self.cached_turns.is_empty() {
            if self.loading_history {
                return Self::loading_row();
            }
            return empty(self, window, cx);
        }
        let folds = Folds {
            toggled: self.toggled.clone(),
            toggle: {
                let toggle = cx.listener(|this: &mut Self, key: &String, _, cx| {
                    this.toggle_fold(key.clone(), cx);
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
            // A replayed capture gets the same wiring: opening a preview and
            // picking an option are local, and anything that would reach the
            // wire is refused by `wire_client` with a banner that says why.
            cards: Some(self.card_intents(window, cx)),
            titles: self.titles.clone(),
            at_rest: self.at_rest,
            full_output: self.cached_full_output.clone(),
            show_full_output: {
                let show = cx.listener(|this: &mut Self, id: &String, _, cx| {
                    this.show_full_output(id.clone(), cx);
                });
                Some(Rc::new(move |id: String, window: &mut Window, cx: &mut gpui::App| {
                    show(&id, window, cx)
                }))
            },
            // Turn links and bottom-row actions (C5, C6): markdown URLs open
            // in the browser, workspace paths reveal in Finder, and every
            // wire action is live-only — replay answers with a toast.
            link: {
                let link = cx.listener(|this: &mut Self, target: &LinkTarget, _, cx| {
                    this.handle_link(target.clone(), cx);
                });
                Some(Rc::new(move |target: LinkTarget, window: &mut Window, cx: &mut gpui::App| {
                    link(&target, window, cx)
                }))
            },
            assistant_action: {
                let act = cx.listener(
                    |this: &mut Self, (id, action): &(String, AssistantTurnAction), window, cx| {
                        this.assistant_action(id.clone(), *action, window, cx);
                    },
                );
                Some(Rc::new(
                    move |id: String, action: AssistantTurnAction, window: &mut Window, cx: &mut gpui::App| {
                        act(&(id, action), window, cx)
                    },
                ))
            },
            user_action: {
                let act = cx.listener(
                    |this: &mut Self, (id, text, action): &(String, String, UserTurnAction), window, cx| {
                        this.user_action(id.clone(), text.clone(), *action, window, cx);
                    },
                );
                Some(Rc::new(
                    move |id: String,
                          text: String,
                          action: UserTurnAction,
                          window: &mut Window,
                          cx: &mut gpui::App| { act(&(id, text, action), window, cx) },
                ))
            },
            tool_group: {
                let act = cx.listener(
                    |this: &mut Self, (key, intent): &(String, ToolGroupIntent), window, cx| {
                        this.tool_group_action(key.clone(), *intent, window, cx);
                    },
                );
                Some(Rc::new(
                    move |key: String, intent: ToolGroupIntent, window: &mut Window, cx: &mut gpui::App| {
                        act(&(key, intent), window, cx)
                    },
                ))
            },
        };
        let count = self.cached_turns.len();
        // Sync the virtual list, splicing the changed range only so visible
        // rows keep their measurements and the tail stays pinned (C1).
        if count != self.list_len {
            let old = self.list_len;
            self.list_len = count;
            if old == 0 {
                self.list_state.reset(count);
            } else if count > old {
                // Pure append (the streaming case): only the new tail needs
                // measuring; visible rows keep theirs (C1).
                self.list_state.splice(old..old, count - old);
            } else {
                self.list_state.splice(0..old, count);
            }
        }
        // Follow the tail only when the reader was at the tail — the
        // TAIL_SLACK semantics, now owned by the list element itself.
        if std::mem::take(&mut self.follow) && self.list_state.is_scrolled_to_end().unwrap_or(true) {
            self.list_state.scroll_to_end();
        }
        let last = count.saturating_sub(1);
        // `Rc` clone: O(1). Only visible rows are built below.
        let turns = self.cached_turns.clone();
        let folds = Rc::new(folds);
        // The wrapper is a flex column so the virtual list's own
        //  resolves to the leftover centre height; without it the
        // list lays out at zero height and paints nothing.
        let element = div()
            .w_full()
            .flex_1()
            .min_h(px(0.0))
            .flex()
            .flex_col()
            .key_context(TRANSCRIPT_CONTEXT)
            .child(
                list(self.list_state.clone(), move |ix, window, cx| {
                    // One item per turn: only visible rows are built and laid
                    // out per frame, so per-frame cost stays bounded as the
                    // transcript grows (C1). Turn bodies still come from
                    // blocks exactly as before.
                    let mut row = v_flex().w_full().pb(px(scale::SP_5));
                    if ix == 0 {
                        row = row.pt(px(TRANSCRIPT_PAD_TOP));
                    }
                    match turns.get(ix) {
                        Some(turn) => row.children(transcript::turn(turn, ix != last, &folds, window, cx)),
                        None => row,
                    }
                    .into_any_element()
                })
                .flex_1()
                .into_any_element(),
            )
            .into_any_element();
        record_frame_stats(frame_start.elapsed());
        let _ = window;
        element
    }

    /// The neutral row while history is still paging in (C2): a spinner, and
    /// never the "New session" empty state.
    fn loading_row() -> AnyElement {
        div()
            .w_full()
            .pt(px(TRANSCRIPT_PAD_TOP))
            .px(px(TRANSCRIPT_PAD_X))
            .child(
                status_row("transcript-loading", "Loading history\u{2026}")
                    .lead(StatusLead::Spinner)
                    .shimmer(true),
            )
            .into_any_element()
    }

    /// Re-snapshot what `render_transcript` reads every frame: the turn list
    /// and the truncated-output map.
    ///
    /// The fold owns the fetch handle (`outputRef`); this view owns the
    /// result. Called when the fold changes (length drift or `follow`), never
    /// per frame, so steady-state frames share one `Rc`.
    fn refresh_render_cache(&mut self) {
        if let Some(session) = self.fold.session(&self.session_id) {
            self.cached_turns = Rc::new(session.turns.clone());
        }
        let mut full_output = HashMap::new();
        for turn in self.cached_turns.iter() {
            for block in turn.blocks() {
                if let Block::ToolCall { id, body: aui_protocol::ToolBody::Shell { .. }, .. } = block
                {
                    if self.fold.stored_output(&self.session_id, id).is_some() {
                        let state = match self.full_outputs.get(id) {
                            Some(full_output::Fetch::Fetching) => FullOutputState::Fetching,
                            Some(full_output::Fetch::Ready { lines, capped }) => {
                                FullOutputState::Ready { lines: lines.clone(), capped: *capped }
                            }
                            None => FullOutputState::Idle,
                        };
                        full_output.insert(id.clone(), FullOutput { fetchable: true, state });
                    }
                }
            }
        }
        self.cached_full_output = full_output;
    }

    /// Flip one card's fold override (C8: group headers and per-call cards
    /// share this, keyed stably).
    fn toggle_fold(&mut self, key: String, cx: &mut Context<Self>) {
        if !self.toggled.remove(&key) {
            self.toggled.insert(key);
        }
        cx.notify();
    }

    /// A markdown link click (C5): URLs open in the browser, paths resolve
    /// against the session workspace.
    fn handle_link(&mut self, target: LinkTarget, cx: &mut Context<Self>) {
        match target {
            LinkTarget::Url(url) => cx.open_url(&url),
            LinkTarget::Path(path) => self.reveal_workspace_path(&path, cx),
        }
    }

    /// Reveal a linked path in Finder (C5): resolve against the workspace,
    /// reject escapes above it, toast when nothing is there.
    fn reveal_workspace_path(&mut self, raw: &str, cx: &mut Context<Self>) {
        // A trailing `:line` is a viewer hint, not part of the path.
        let path_part = raw.split(':').next().unwrap_or(raw);
        let workspace = PathBuf::from(&self.workspace);
        let candidate = workspace.join(path_part.trim_start_matches('/'));
        // Reject escapes without touching the filesystem first: normalize
        // `..` lexically and require the workspace prefix.
        let mut normalized = PathBuf::new();
        for component in candidate.components() {
            match component {
                std::path::Component::ParentDir => {
                    normalized.pop();
                }
                std::path::Component::CurDir => {}
                other => normalized.push(other.as_os_str()),
            }
        }
        if !normalized.starts_with(&workspace) {
            self.toast("Link", "That path escapes the session workspace.", cx);
            return;
        }
        match std::fs::metadata(&normalized) {
            Ok(_) => cx.reveal_path(&normalized),
            Err(_) => self.toast("Link", format!("No such file: {path_part}"), cx),
        }
    }

    /// An assistant turn's bottom-row action (C6): Copy is local; Retry
    /// resends the user input behind the turn; Fork opens the turn picker;
    /// Pin has no meaning on a turn and says where it lives. Wire actions
    /// are live-only — replay answers with a toast.
    fn assistant_action(
        &mut self,
        turn_id: String,
        action: AssistantTurnAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let _ = window;
        match action {
            AssistantTurnAction::Copy => {
                let text = self.assistant_text(&turn_id).unwrap_or_default();
                if !text.is_empty() {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
            }
            AssistantTurnAction::Retry => {
                if self.replay {
                    self.toast("Replay", "Retry is not available in a replayed capture.", cx);
                    return;
                }
                match self.input_before(&turn_id) {
                    Some(text) => self.submit(text, cx),
                    None => self.toast("Retry", "There is no remembered input behind this turn.", cx),
                }
            }
            AssistantTurnAction::Fork => {
                if self.replay {
                    self.toast("Replay", "Fork is not available in a replayed capture.", cx);
                    return;
                }
                cx.emit(SessionEvent::ForkPicker);
            }
            AssistantTurnAction::Pin => {
                // The bottom row always draws Pin; a turn is not pinnable.
                self.toast("Pin", "Pin lives on sidebar sessions, not on turns.", cx);
            }
        }
    }

    /// A user turn's bottom-row action (C6): Copy is local, Edit drops the
    /// text into the composer draft, Resend sends it again (live only).
    fn user_action(
        &mut self,
        turn_id: String,
        text: String,
        action: aui::transcript::UserTurnAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let _ = turn_id;
        match action {
            aui::transcript::UserTurnAction::Copy => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
            aui::transcript::UserTurnAction::Edit => {
                self.set_draft(text, window, cx);
                self.focus_composer(window, cx);
            }
            aui::transcript::UserTurnAction::Resend => {
                if self.replay {
                    self.toast("Replay", "Resend is not available in a replayed capture.", cx);
                    return;
                }
                self.submit(text, cx);
            }
        }
    }

    /// A tool group's intents (C8): the header toggles the group, per-call
    /// toggles flip that call's card, and OpenInPane reveals the call's
    /// target path where it names one.
    fn tool_group_action(
        &mut self,
        key: String,
        intent: ToolGroupIntent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match intent {
            ToolGroupIntent::Toggle => self.toggle_fold(key, cx),
            ToolGroupIntent::Call { index, intent } => match intent {
                ToolCardIntent::OpenInPane => self.reveal_tool_target(&key, index, cx),
                _ => self.toggle_fold(format!("{key}:{index}"), cx),
            },
        }
    }

    /// A grouped call's header target, back to the text the lone card would
    /// have shown. The group key is `<turn id>:<block index>`.
    fn tool_call_target(&self, turn_id: &str, block_index: usize, call_index: usize) -> Option<String> {
        let session = self.fold.session(&self.session_id)?;
        session.turns.iter().find_map(|turn| match turn {
            Turn::Assistant { id, blocks, .. } if id == turn_id => match blocks.get(block_index) {
                Some(Block::ToolGroup { calls, .. }) => {
                    calls.get(call_index).map(|call| call.target.clone())
                }
                _ => None,
            },
            _ => None,
        })
    }

    /// Reveal a grouped call's target (C5 on grouped cards).
    fn reveal_tool_target(&mut self, key: &str, index: usize, cx: &mut Context<Self>) {
        let (turn_id, block_index) = key.rsplit_once(':').unwrap_or((key, ""));
        let block_index = block_index.parse::<usize>().unwrap_or(usize::MAX);
        match self.tool_call_target(turn_id, block_index, index) {
            Some(target) => self.reveal_workspace_path(&target, cx),
            None => self.toast("Open", "That call has no path to reveal.", cx),
        }
    }

    /// An assistant turn's prose, for Copy: every text block joined.
    fn assistant_text(&self, turn_id: &str) -> Option<String> {
        let session = self.fold.session(&self.session_id)?;
        session.turns.iter().find_map(|turn| match turn {
            Turn::Assistant { id, blocks, .. } if id == turn_id => {
                let texts: Vec<&str> = blocks
                    .iter()
                    .filter_map(|block| match block {
                        Block::Text { text, .. } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect();
                Some(texts.join("\n\n"))
            }
            _ => None,
        })
    }

    /// The user input behind an assistant turn, for Retry: the nearest user
    /// turn above it.
    fn input_before(&self, turn_id: &str) -> Option<String> {
        let session = self.fold.session(&self.session_id)?;
        let mut last_user: Option<String> = None;
        for turn in &session.turns {
            match turn {
                Turn::User { text, .. } => last_user = Some(text.clone()),
                Turn::Assistant { id, .. } if id == turn_id => return last_user,
                _ => {}
            }
        }
        None
    }

    /// ⌘C in the transcript context (C8b): copy the held selection, if
    /// any. The binding's own predicate already excludes the composer and
    /// card fields, so this never steals copy from an editor.
    pub fn copy_selected(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        // The turn components do not forward selection intents yet, so
        // `text_selection` can only be `None` today; the state, the clear
        // paths and this binding are the harness half, waiting on the
        // library's turn wiring.
        let _ = self.text_selection.as_ref();
    }

    /// Clear the transcript text selection (C8b). Returns whether one was
    /// held, so Escape prefers it over heavier dismissals.
    pub fn clear_selection(&mut self, cx: &mut Context<Self>) -> bool {
        if self.text_selection.take().is_some() {
            cx.notify();
            true
        } else {
            false
        }
    }

    /// Open every tool group for a screenshot: group keys default closed, so
    /// marking them toggled opens them; calls default open.
    fn expand_all_groups(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.fold.session(&self.session_id).cloned() else {
            return;
        };
        for turn in &session.turns {
            let Turn::Assistant { id, blocks, .. } = turn else {
                continue;
            };
            for (index, block) in blocks.iter().enumerate() {
                if matches!(block, Block::ToolGroup { .. }) {
                    self.toggled.insert(transcript::block_key(id, index));
                }
            }
        }
        cx.notify();
    }

    /// Everything the pending approval and question cards need to talk back.
    ///
    /// One struct built once a frame: every closure here is a `cx.listener`, so
    /// a click on a card and a keystroke on the same card run the same code.
    fn card_intents(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Cards {
        let choose = cx.listener(|this: &mut Self, (approval, choice, feedback): &(String, String, Option<String>), _, cx| {
            this.decide_approval(approval.clone(), choice.clone(), feedback.clone(), cx);
        });
        let feedback_toggle = cx.listener(|this: &mut Self, (approval, choice): &(String, Option<String>), window, cx| {
            this.toggle_feedback(approval.clone(), choice.clone(), window, cx);
        });
        let select = cx.listener(|this: &mut Self, (id, index): &(String, usize), _, cx| {
            this.select_option(id.clone(), *index, cx);
        });
        let toggle_preview = cx.listener(|this: &mut Self, (id, index): &(String, usize), _, cx| {
            this.toggle_preview(id.clone(), *index, cx);
        });
        let answer = cx.listener(|this: &mut Self, id: &String, _, cx| this.answer_question(id.clone(), cx));
        let skip = cx.listener(|this: &mut Self, id: &String, _, cx| this.skip_question(id.clone(), cx));
        let clarify = cx.listener(|this: &mut Self, id: &String, window, cx| {
            this.clarify_question(id.clone(), window, cx);
        });
        let retry = cx.listener(|this: &mut Self, id: &String, _, cx| this.retry_turn(id.clone(), cx));
        // The two text fields are the app's, exactly as the composer's editor
        // is: the cards are handed an element and never a character.
        let feedback_slot = self.feedback_open.is_some().then(|| {
            Textarea::new(&self.feedback).text_size(aui_tokens::scaled(scale::FS_12)).into_any_element()
        });
        let clarify_slot = self.clarify_open.is_some().then(|| {
            Textarea::new(&self.clarify).text_size(aui_tokens::scaled(scale::FS_12)).into_any_element()
        });
        let feedback_text = self.feedback.read(cx).value().to_string();
        let _ = window;
        Cards {
            choose: Rc::new(move |a, c, f, window, cx| choose(&(a, c, f), window, cx)),
            feedback_toggle: Rc::new(move |a, c, window, cx| feedback_toggle(&(a, c), window, cx)),
            feedback_open: self.feedback_open.clone(),
            feedback_slot: std::cell::RefCell::new(feedback_slot),
            feedback_text,
            select: Rc::new(move |id, index, window, cx| select(&(id, index), window, cx)),
            selections: self.selections.clone(),
            toggle_preview: Rc::new(move |id, index, window, cx| toggle_preview(&(id, index), window, cx)),
            previews: self.previews.clone(),
            answer: Rc::new(move |id, window, cx| answer(&id, window, cx)),
            skip: Rc::new(move |id, window, cx| skip(&id, window, cx)),
            clarify: Rc::new(move |id, window, cx| clarify(&id, window, cx)),
            clarify_open: self.clarify_open.clone(),
            clarify_slot: std::cell::RefCell::new(clarify_slot),
            countdowns: self.countdowns(),
            retry: Rc::new(move |id, window, cx| retry(&id, window, cx)),
            retryable_turns: self.retryable_turns(),
        }
    }

    /// The live status line: history loading, or a running turn with its
    /// elapsed time and the interrupt hint.
    fn render_status(&self) -> Option<AnyElement> {
        // A scheduled retry outranks "Working…": the turn is not working, it is
        // waiting out a backoff, and saying which is the whole point of the row.
        if let Some((attempt, max, remaining_ms, reason)) = self.retry_countdown() {
            return Some(
                h_flex()
                    .w_full()
                    .px(px(TRANSCRIPT_PAD_X))
                    .pb(px(scale::SP_4))
                    .child(retry_row("retry", attempt, max, remaining_ms, reason))
                    .into_any_element(),
            );
        }
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
    ///
    /// Its action is the error's own way out where there is one — "Retry" for a
    /// wire that said "not now" — and a plain dismiss otherwise.
    fn render_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let message = self.banner.clone()?;
        let label = match self.banner_action {
            Some(_) => "Retry",
            None => "Dismiss",
        };
        let press = cx.listener(|this: &mut Self, _: &(), _, cx| this.run_banner_action(cx));
        Some(
            div()
                .w_full()
                .px(px(TRANSCRIPT_PAD_X))
                .pb(px(scale::SP_3))
                .child(
                    banner("session-banner", BannerKind::Error, vec![BannerRun::Text(message.into())])
                        .action(label, BannerActionStyle::Ghost)
                        .on_action(move |window, cx| press(&(), window, cx)),
                )
                .into_any_element(),
        )
    }

    /// The billing guard's banner (Phase 5 A1), directly over the composer
    /// because it is about the thing the composer is for.
    ///
    /// Pay-as-you-go is `Waiting`-tinted and carries both the way out and the
    /// way through; an unknown plan is a quiet `Info` line that blocks nothing.
    fn render_tier_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let guard = self.tier_banner.clone()?;
        let sign_out = cx.listener(|_: &mut Self, _: &(), _, cx| cx.emit(SessionEvent::Logout));
        let through = cx.listener(move |_: &mut Self, _: &(), _, cx| {
            cx.emit(SessionEvent::TierOverride);
        });
        let recheck = cx.listener(|_: &mut Self, _: &(), _, cx| cx.emit(SessionEvent::TierRecheck));
        let kind = if guard.blocking { BannerKind::Waiting } else { BannerKind::Info };
        let mut row = banner("tier-banner", kind, vec![BannerRun::Text(guard.text.clone().into())]);
        row = if guard.blocking {
            row.secondary_action("Sign out", BannerActionStyle::Ghost)
                .on_secondary(move |window, cx| sign_out(&(), window, cx))
                .action("Send anyway", BannerActionStyle::Secondary)
                .on_action(move |window, cx| through(&(), window, cx))
        } else {
            row.action("Check again", BannerActionStyle::Ghost)
                .on_action(move |window, cx| recheck(&(), window, cx))
        };
        Some(div().w_full().px(px(TRANSCRIPT_PAD_X)).pb(px(scale::SP_3)).child(row).into_any_element())
    }

    /// The needs-you banner: something is waiting on the person and they are
    /// not looking at it.
    ///
    /// Only when the pending card is actually out of view — a banner pointing at
    /// a card the reader is already reading is noise.
    fn render_needs_you(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (approvals, questions) = self.waiting_on_you()?;
        let at_tail = self.list_state.is_scrolled_to_end().unwrap_or(true);
        if at_tail {
            return None;
        }
        let detail = match (approvals, questions) {
            (a, 0) => format!("{a} approval{} above.", plural(a)),
            (0, q) => format!("{q} question{} above.", plural(q)),
            (a, q) => format!("{a} approval{} and {q} question{} above.", plural(a), plural(q)),
        };
        let jump = cx.listener(|this: &mut Self, _: &(), _, cx| {
            this.list_state.scroll_to_end();
            cx.notify();
        });
        Some(
            div()
                .w_full()
                .px(px(TRANSCRIPT_PAD_X))
                .pb(px(scale::SP_3))
                .child(
                    needs_you_banner("needs-you", "Muse is waiting for you.", detail)
                        .on_jump(move |_, window, cx| jump(&(), window, cx)),
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
            // F7: a skill whose name is already a client command is hidden.
            // Muse ships `plan`, and the menu offering both `/plan` the mode and
            // `/plan` the skill — which do different things — was a trap.
            .filter(|s| !Command::ALL.iter().any(|c| c.slash().trim_start_matches('/') == s.name))
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
                this.files.retain(|file| file.id != id.as_ref());
                cx.notify();
            }
        });
        let plus = cx.listener(|this: &mut Self, id: &SharedString, window, cx| {
            this.plus_open = false;
            match id.as_ref() {
                "attach" => this.prompt_for_image(cx),
                "mention" => this.insert_sigil("@", window, cx),
                "commands" => this.insert_sigil("/", window, cx),
                _ => {}
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
                        thumbnail: image.thumb.clone(),
                        detail: None,
                    })
                    .chain(self.files.iter().map(|file| ComposerChip {
                        id: file.id.clone().into(),
                        kind: ComposerChipKind::File,
                        label: file.name.clone().into(),
                        removable: true,
                        thumbnail: None,
                        detail: Some(file.detail().into()),
                    }))
                    .collect(),
            )
            .streaming(self.busy())
            // Blocked context is the server refusing to take more, so the
            // composer refuses too and the meter offers the way out.
            .can_send(!blocked && (!draft.trim().is_empty() || !self.images.is_empty() || !self.files.is_empty()))
            .plus_menu(
                self.plus_open,
                Some(
                    plus_menu(
                        "plus",
                        vec![
                            PlusMenuItem::new("attach", IconName::Paperclip, "Attach file or photo")
                                .key("⌘U"),
                            PlusMenuItem::new("mention", IconName::At, "@ Mention file"),
                            PlusMenuItem::new("commands", IconName::Slash, "/ Slash commands"),
                        ],
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

// ---------------------------------------------------------------------------
// Approvals, questions, shell, fork, retry (spec §5 phase 4)
// ---------------------------------------------------------------------------

/// What the inline banner's action does, when the failure is one the person can
/// do something about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BannerAction {
    /// Resend a turn that never left, with the text it carried.
    RetryTurn(String),
    /// Re-run a user shell command that never left.
    RetryShell(String),
}

impl SessionView {
    /// A server-minted choice was pressed: `approval/decide`.
    ///
    /// The `requirementId` is the guard MSP requires: it must equal the
    /// approval's **current** stage token, or the wire answers
    /// `approvalRequirementStale`. It is read from the pending request the fold
    /// keeps rather than from the card, because the block has no room for it and
    /// the choices change between stages.
    pub fn decide_approval(&mut self, approval_id: String, choice_id: String, feedback: Option<String>, cx: &mut Context<Self>) {
        let Some(requirement_id) = self
            .fold
            .side(&self.session_id)
            .and_then(|side| side.pending_approvals.get(&approval_id))
            .map(|request| request.current_requirement_id.clone())
        else {
            // Nothing pending under that id: the resolution already landed.
            return;
        };
        let Some(client) = self.wire_client(cx) else { return };
        self.feedback_open = None;
        let params = ApprovalDecideParams {
            approval_id: approval_id.clone(),
            choice_id,
            command_id: new_command_id(),
            feedback,
            requirement_id,
            session_id: self.session_id.clone(),
        };
        let call = cx.background_spawn(async move { client.approval_decide(&params) });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = call.await;
            let _ = this.update(cx, |this, cx| {
                // The ack's `terminal` flag is admission only: what the card
                // shows next comes from `approval/updated` or
                // `approval/resolved`, never from here.
                if let Err(error) = result {
                    this.decide_failed(&approval_id, &error, cx);
                }
            });
        }));
        cx.notify();
    }

    /// The four ways `approval/decide` can lose (research §1.14).
    fn decide_failed(&mut self, approval_id: &str, error: &MuseError, cx: &mut Context<Self>) {
        match error.kind() {
            // Somebody else — a policy, the judge, another window — got there
            // first, and the error carries the winning resolution.
            Some(ErrorKind::ApprovalAlreadyResolved) => {
                self.fold.resolve_approval(&self.session_id, approval_id, resolution_of(error));
                self.follow = true;
                cx.notify();
            }
            // The choices moved under the press, which only happens between
            // stages; the update that moved them is already on its way, so
            // saying anything here would be noise.
            Some(ErrorKind::ApprovalRequirementStale) => {}
            Some(ErrorKind::ApprovalChoiceInvalid) => {
                self.set_banner("That choice is no longer offered for this command.", None, cx);
            }
            Some(ErrorKind::ApprovalNotFound) => {
                self.fold.resolve_approval(&self.session_id, approval_id, None);
                self.follow = true;
                self.set_banner("Muse no longer knows about that approval.", None, cx);
            }
            _ => self.report(error, cx),
        }
    }

    /// Open (or close) the feedback field a choice asks for.
    pub fn toggle_feedback(&mut self, approval_id: String, choice_id: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.feedback_open = choice_id.map(|choice| (approval_id, choice));
        self.feedback.update(cx, |state, cx| state.set_value("", window, cx));
        if self.feedback_open.is_some() {
            window.focus(&self.feedback.focus_handle(cx), cx);
        }
        cx.notify();
    }

    /// Enter in an open feedback or clarification field; `false` when neither is
    /// open, so the caller can let the key mean what it usually means.
    pub fn confirm_field(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if let Some((approval_id, choice_id)) = self.feedback_open.clone() {
            let text = self.feedback.read(cx).value().to_string();
            let feedback = (!text.trim().is_empty()).then_some(text);
            self.decide_approval(approval_id, choice_id, feedback, cx);
            return true;
        }
        if let Some(block_id) = self.clarify_open.clone() {
            self.send_clarification(&block_id, window, cx);
            return true;
        }
        false
    }

    /// Escape: close an open card field, and say whether it closed one.
    pub fn close_card_field(&mut self, cx: &mut Context<Self>) -> bool {
        let closed = self.feedback_open.take().is_some() || self.clarify_open.take().is_some();
        if closed {
            cx.notify();
        }
        closed
    }

    // ------------------------------------------------------------- questions

    /// A radio or checkbox on a pending question.
    pub fn select_option(&mut self, block_id: String, index: usize, cx: &mut Context<Self>) {
        let multi = self.question(&block_id).is_some_and(|q| q.multi);
        let selected = self.selections.entry(block_id).or_default();
        match (multi, selected.iter().position(|i| *i == index)) {
            (true, Some(at)) => {
                selected.remove(at);
            }
            (true, None) => selected.push(index),
            (false, _) => *selected = vec![index],
        }
        cx.notify();
    }

    /// An option's "Preview" chevron.
    pub fn toggle_preview(&mut self, block_id: String, index: usize, cx: &mut Context<Self>) {
        let open = self.previews.entry(block_id).or_default();
        match open.iter().position(|i| *i == index) {
            Some(at) => {
                open.remove(at);
            }
            None => open.push(index),
        }
        cx.notify();
    }

    /// The pending question a block id names.
    ///
    /// The fold names a question block `"<userInputId>:<questionId>"`, because
    /// one MSP request may carry several questions and each is its own card.
    fn question(&self, block_id: &str) -> Option<Pending> {
        let (input_id, question_id) = block_id.split_once(':')?;
        let side = self.fold.side(&self.session_id)?;
        let request = side.pending_inputs.get(input_id)?;
        let question = request.questions.iter().find(|q| q.id == question_id)?;
        Some(Pending {
            input_id: input_id.to_owned(),
            question_id: question_id.to_owned(),
            multi: matches!(question.selection.mode, UserInputSelectionMode::Multiple),
            labels: question.options.iter().map(|o| o.label.clone()).collect(),
            questions: request.questions.len(),
        })
    }

    /// "Continue" on a question.
    ///
    /// MSP settles the whole prompt at once and keys answers on option
    /// **labels**, so a request with several questions gathers its answers here
    /// and sends one `userInput/answer` when the last one is answered.
    pub fn answer_question(&mut self, block_id: String, cx: &mut Context<Self>) {
        let Some(pending) = self.question(&block_id) else { return };
        let selected = self.selections.get(&block_id).cloned().unwrap_or_default();
        if selected.is_empty() {
            return;
        }
        let chosen: Vec<String> = selected.iter().filter_map(|i| pending.labels.get(*i).cloned()).collect();
        let answer = UserInputAnswer {
            free_text: None,
            note: None,
            question_id: pending.question_id.clone(),
            // Exactly one of `selectedLabel` and `selectedLabels`, chosen by the
            // question's own mode: sending both is `userInputAnswerInvalid`.
            selected_label: (!pending.multi).then(|| chosen.first().cloned()).flatten(),
            selected_labels: pending.multi.then(|| chosen.clone()),
        };
        let gathered = self.answers.entry(pending.input_id.clone()).or_default();
        gathered.insert(pending.question_id.clone(), answer);
        if gathered.len() < pending.questions {
            // Not the last question of the prompt: the answers wait here until
            // its siblings are answered, and the prompt settles once.
            cx.notify();
            return;
        }
        let answers: Vec<UserInputAnswer> =
            self.answers.remove(&pending.input_id).unwrap_or_default().into_values().collect();
        let Some(client) = self.wire_client(cx) else { return };
        let input_id = pending.input_id.clone();
        let params = UserInputAnswerParams {
            answers,
            command_id: new_command_id(),
            session_id: self.session_id.clone(),
            user_input_id: input_id.clone(),
        };
        let call = cx.background_spawn(async move { client.user_input_answer(&params) });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = call.await;
            let _ = this.update(cx, |this, cx| {
                if let Err(error) = result {
                    this.settle_failed(&input_id, &error, cx);
                }
            });
        }));
        cx.notify();
    }

    /// "Skip": `userInput/cancel`.
    pub fn skip_question(&mut self, block_id: String, cx: &mut Context<Self>) {
        let Some(pending) = self.question(&block_id) else { return };
        let Some(client) = self.wire_client(cx) else { return };
        self.answers.remove(&pending.input_id);
        let input_id = pending.input_id.clone();
        let params = UserInputCancelParams {
            command_id: new_command_id(),
            reason: Some("The person declined to answer.".to_owned()),
            session_id: self.session_id.clone(),
            user_input_id: input_id.clone(),
        };
        let call = cx.background_spawn(async move { client.user_input_cancel(&params) });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = call.await;
            let _ = this.update(cx, |this, cx| {
                if let Err(error) = result {
                    this.settle_failed(&input_id, &error, cx);
                }
            });
        }));
        cx.notify();
    }

    /// "Explain instead": open the field, or send what is in it.
    pub fn clarify_question(&mut self, block_id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.clarify_open.as_deref() == Some(block_id.as_str()) {
            self.send_clarification(&block_id, window, cx);
            return;
        }
        self.clarify_open = Some(block_id);
        self.clarify.update(cx, |state, cx| state.set_value("", window, cx));
        window.focus(&self.clarify.focus_handle(cx), cx);
        cx.notify();
    }

    fn send_clarification(&mut self, block_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = self.question(block_id) else { return };
        let content = self.clarify.read(cx).value().to_string();
        if content.trim().is_empty() {
            return;
        }
        let Some(client) = self.wire_client(cx) else { return };
        self.clarify_open = None;
        self.clarify.update(cx, |state, cx| state.set_value("", window, cx));
        let input_id = pending.input_id.clone();
        let params = UserInputClarifyParams {
            // `format` is `"text"` in v1 and the field is open, so it is named
            // rather than left to a default that might change.
            clarification: UserInputClarification { content, format: "text".to_owned() },
            command_id: new_command_id(),
            session_id: self.session_id.clone(),
            user_input_id: input_id.clone(),
        };
        let call = cx.background_spawn(async move { client.user_input_clarify(&params) });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = call.await;
            let _ = this.update(cx, |this, cx| {
                if let Err(error) = result {
                    this.settle_failed(&input_id, &error, cx);
                }
            });
        }));
        cx.notify();
    }

    /// The two ways a `userInput/*` settlement can lose.
    fn settle_failed(&mut self, input_id: &str, error: &MuseError, cx: &mut Context<Self>) {
        match error.kind() {
            // Something already settled it — a timeout, most likely — and
            // `userInput/settled` is on its way with the real outcome.
            Some(ErrorKind::UserInputAlreadySettled) => {}
            Some(ErrorKind::UserInputAnswerInvalid) => {
                self.answers.remove(input_id);
                self.set_banner("Muse refused that answer; pick again.", None, cx);
            }
            _ => self.report(error, cx),
        }
    }

    // ------------------------------------------------------------ user shell

    /// A draft starting with `!` is a shell command, not a turn (research
    /// §1.12).
    ///
    /// It is also the free way to raise a real approval, and free here means
    /// what it says: a user shell command is run by the **server**, not by the
    /// model, so no provider turn is started and nothing is billed on any
    /// provider. (`echo` itself is not a free provider — see the `main.rs`
    /// header — but this path never reaches one.) Under `promptUnmatched`,
    /// `!echo hi && ls` raises the two-stage approval that
    /// `fixtures/msp/transcript-approve.jsonl` records.
    pub fn run_user_shell(&mut self, command_text: String, cx: &mut Context<Self>) {
        if !self.user_shell {
            self.set_banner("Muse did not grant this build the userShell capability.", None, cx);
            return;
        }
        let Some(client) = self.wire_client(cx) else { return };
        self.banner = None;
        self.banner_action = None;
        let command_id = new_command_id();
        // The shell item is filed under its own `commandId`, and so is any
        // approval it raises, so remembering the text here is what makes the
        // retry offer honest.
        self.fold.record_command(&self.session_id, &command_id, &format!("!{command_text}"));
        let params = SessionUserShellParams {
            command_id,
            command_text: command_text.clone(),
            session_id: self.session_id.clone(),
        };
        let call = cx.background_spawn(async move { client.session_user_shell(&params) });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = call.await;
            let _ = this.update(cx, |this, cx| {
                if let Err(error) = result {
                    this.report_retryable(&error, BannerAction::RetryShell(command_text.clone()), cx);
                }
            });
        }));
        cx.notify();
    }

    // ------------------------------------------------------------------ fork

    /// `/fork`, and an assistant turn's "Fork from here".
    ///
    /// The cut point is a **turn id** of a completed turn: naming an in-progress
    /// one is `forkBoundaryInvalid`. Invoked from `/fork` with nothing named, it
    /// is the newest completed turn.
    pub fn fork(&mut self, last_turn_id: Option<String>, cx: &mut Context<Self>) {
        let Some(client) = self.wire_client(cx) else { return };
        let cut_point =
            last_turn_id.or_else(|| self.newest_completed_turn()).map(|last_turn_id| ForkCutPoint { last_turn_id });
        let params = SessionForkParams {
            command_id: new_command_id(),
            cut_point,
            // The fork's history comes through `view/page`, like every other
            // attach in this app.
            exclude_items: Some(true),
            session_id: self.session_id.clone(),
        };
        let call = cx.background_spawn(async move { client.session_fork(&params) });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = call.await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(forked) => cx.emit(SessionEvent::Forked {
                    session_id: forked.session.session_id.clone(),
                    session: serde_json::to_value(&forked.session).unwrap_or_default(),
                }),
                Err(error) if error.kind() == Some(&ErrorKind::ForkBoundaryInvalid) => {
                    this.set_banner("That turn is still running, so there is nothing to fork from yet.", None, cx);
                }
                Err(error) => this.report(&error, cx),
            });
        }));
    }

    /// The completed assistant turns, newest first: the rows of the `/fork`
    /// picker. Each row is the turn id, the first line of the user prompt that
    /// started the turn, and the turn's wall-clock time.
    ///
    /// The filter is the same one a fork may name: no running turn (naming an
    /// in-progress one is `forkBoundaryInvalid`), no client-authored marker or
    /// plan turn. A `Turn::User`'s id is the message item's, not a turn id, so
    /// user turns only lend their text to the row that follows them.
    pub fn fork_turns(&self) -> Vec<(String, String, String)> {
        let Some(session) = self.session() else { return Vec::new() };
        let running = self.running.as_ref().map(|r| r.turn_id.as_str());
        let mut prompt = String::new();
        let mut rows = Vec::new();
        for turn in session.turns.iter() {
            match turn {
                aui_protocol::Turn::User { text, .. } => prompt = text.clone(),
                aui_protocol::Turn::Assistant { id, blocks, meta } => {
                    if Some(id.as_str()) == running {
                        continue;
                    }
                    // The client authors two kinds of turn of its own — marker rows and
                    // plan cards — and neither is a turn the server could fork at.
                    if id.starts_with("marker:") || id.starts_with("plan-") {
                        continue;
                    }
                    rows.push((id.clone(), fork_label(&prompt, blocks), fork_time(meta)));
                }
            }
        }
        rows.reverse();
        rows
    }

    /// The nth newest completed turn, 1-based: what `/fork <n>` names.
    fn nth_completed_turn(&self, n: usize) -> Option<String> {
        self.fork_turns().into_iter().nth(n.saturating_sub(1)).map(|(id, _, _)| id)
    }

    /// The newest turn the server has finished, which is the only boundary a
    /// fork may name.
    fn newest_completed_turn(&self) -> Option<String> {
        self.nth_completed_turn(1)
    }

    /// `/fork <n>`: fork the nth newest completed turn with no picker. A
    /// number with no turn behind it is a banner, never a fork of whatever the
    /// server thinks is newest.
    fn fork_nth(&mut self, n: usize, cx: &mut Context<Self>) {
        match self.nth_completed_turn(n) {
            Some(last_turn_id) => self.fork(Some(last_turn_id), cx),
            None => {
                self.set_banner(&format!("There is no completed turn #{n} to fork from yet."), None, cx);
            }
        }
    }

    // ---------------------------------------------------------- full output

    /// "Show full output" on a truncated tool card: page `item/readOutput` on
    /// a background task (02-app §3 — a page can block) and replace the card's
    /// body on the server's result (D4). A second press while pages are still
    /// arriving does nothing; a failed fetch reports its banner and leaves the
    /// truncated body alone.
    pub fn show_full_output(&mut self, block_id: String, cx: &mut Context<Self>) {
        let Some(client) = self.wire_client(cx) else { return };
        let Some(output_ref) = self.fold.stored_output(&self.session_id, &block_id).cloned()
        else {
            return;
        };
        if matches!(self.full_outputs.get(&block_id), Some(full_output::Fetch::Fetching)) {
            return;
        }
        self.full_outputs.insert(block_id.clone(), full_output::Fetch::Fetching);
        self.refresh_render_cache();
        cx.notify();
        let session_id = self.session_id.clone();
        let output_ref = output_ref.id.clone();
        let fetch_id = block_id.clone();
        // Every MSP request can block, so the pages run here and the card is
        // replaced below, on the server's result.
        let call = cx.background_spawn(async move {
            full_output::fetch_full_output(&client, &session_id, &fetch_id, &output_ref)
        });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = call.await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(fetched) => {
                    this.full_outputs.insert(
                        block_id,
                        full_output::Fetch::Ready { lines: fetched.lines, capped: fetched.capped },
                    );
                    this.refresh_render_cache();
                    cx.notify();
                }
                Err(error) => {
                    this.full_outputs.remove(&block_id);
                    this.report(&error, cx);
                }
            });
        }));
    }

    // ----------------------------------------------------------------- retry

    /// "Retry" on an error card: resend the failed turn's own input.
    ///
    /// The wire never gives a prompt back, so this only works where the app
    /// remembered it — which is every turn it sent itself. A turn that arrived
    /// through a backfill has no text here, and the card hides the button.
    pub fn retry_turn(&mut self, turn_id: String, cx: &mut Context<Self>) {
        let Some(text) = self.remembered_text(&turn_id) else { return };
        match text.strip_prefix('!') {
            Some(command) => self.run_user_shell(command.to_owned(), cx),
            None => self.submit(text, cx),
        }
    }

    /// What a turn was sent with, if this app sent it.
    ///
    /// A fresh `turn/start`'s `commandId` **equals** its `turnId`, which is what
    /// makes the command-text map a turn-text map for free.
    fn remembered_text(&self, turn_id: &str) -> Option<String> {
        self.fold.side(&self.session_id)?.command_text.get(turn_id).cloned()
    }

    /// Which failed turns the retry button may be offered on.
    fn retryable_turns(&self) -> HashSet<String> {
        self.fold
            .side(&self.session_id)
            .map(|side| side.command_text.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// A failure the person can try again, with the button that would do it.
    ///
    /// `overloaded` and `backpressured` also retry themselves once, after the
    /// backoff: they are the wire saying "not now", and "not now" deserves one
    /// unattended attempt before it deserves a person's attention.
    fn report_retryable(&mut self, error: &MuseError, action: BannerAction, cx: &mut Context<Self>) {
        let auto = matches!(error.kind(), Some(ErrorKind::Overloaded | ErrorKind::Backpressured));
        self.set_banner(&format!("{}. {error}", conn::title(error)), Some(action.clone()), cx);
        if !auto {
            return;
        }
        self.tasks.push(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(RETRY_BACKOFF).await;
            let _ = this.update(cx, |this, cx| {
                // Only if nothing else has happened to the banner since: a
                // person who dismissed it, or a newer error, wins.
                if this.banner_action.as_ref() == Some(&action) {
                    this.run_banner_action(cx);
                }
            });
        }));
    }

    /// Press the banner's action.
    pub fn run_banner_action(&mut self, cx: &mut Context<Self>) {
        let action = self.banner_action.take();
        self.banner = None;
        match action {
            Some(BannerAction::RetryTurn(text)) => self.submit(text, cx),
            Some(BannerAction::RetryShell(command)) => self.run_user_shell(command, cx),
            None => cx.notify(),
        }
    }

    /// One place that writes the banner, so its message and its action can never
    /// disagree.
    fn set_banner(&mut self, message: &str, action: Option<BannerAction>, cx: &mut Context<Self>) {
        self.banner = Some(message.to_owned());
        self.banner_action = action;
        cx.notify();
    }

    // ---------------------------------------------------------------- clocks

    /// Notice a countdown that has started, and start (or stop) the 1 s clock.
    ///
    /// MSP sends durations, never deadlines — an `autoResolutionMs` and a
    /// `retryDelayMs` — so the moment each one was observed is the app's to
    /// remember and the countdown is the app's to derive.
    fn observe_clocks(&mut self, cx: &mut Context<Self>) {
        let Some(side) = self.fold.side(&self.session_id) else { return };
        let pending: Vec<String> = side.pending_inputs.keys().cloned().collect();
        let retry = side.retry.as_ref().map(|r| format!("{}:{}", r.turn_id, r.attempt));
        for id in &pending {
            self.question_started.entry(id.clone()).or_insert_with(Instant::now);
        }
        self.question_started.retain(|id, _| pending.contains(id));
        match retry {
            Some(key) => {
                if self.retry_started.as_ref().map(|(k, _)| k.as_str()) != Some(key.as_str()) {
                    self.retry_started = Some((key, Instant::now()));
                }
            }
            None => self.retry_started = None,
        }
        let wanted = !self.question_started.is_empty() || self.retry_started.is_some();
        match (wanted, self.countdown.is_some()) {
            (true, false) => self.start_countdown(cx),
            (false, true) => self.countdown = None,
            _ => {}
        }
    }

    /// One frame a second while anything is counting down. Deliberately not the
    /// 250 ms turn ticker: a countdown that changes once a second has no
    /// business waking the window four times as often.
    fn start_countdown(&mut self, cx: &mut Context<Self>) {
        self.countdown = Some(cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(COUNTDOWN_TICK).await;
            let alive = this.update(cx, |this, cx| {
                cx.notify();
                !this.question_started.is_empty() || this.retry_started.is_some()
            });
            if !matches!(alive, Ok(true)) {
                return;
            }
        }));
    }

    /// How long each pending question has left, keyed by its block id.
    fn countdowns(&self) -> HashMap<String, (u64, u64)> {
        let Some(side) = self.fold.side(&self.session_id) else { return HashMap::new() };
        let mut out = HashMap::new();
        for (input_id, request) in &side.pending_inputs {
            let Some(total) = request.auto_resolution_ms else { continue };
            let Some(started) = self.question_started.get(input_id) else { continue };
            let remaining = total.saturating_sub(started.elapsed().as_millis() as u64);
            for question in &request.questions {
                out.insert(format!("{input_id}:{}", question.id), (remaining, total));
            }
        }
        out
    }

    /// Re-read what is pending, after a resume or a reconnect.
    ///
    /// The server does not re-issue an `approval/request` it already sent, so a
    /// client that was away has to pull. `approval/listPending` is that pull;
    /// the fold dedupes on ids it has already seen, so folding both lists is
    /// safe even when nothing was missed.
    fn refresh_pending_now(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        let params = ApprovalListPendingParams { session_id: self.session_id.clone() };
        let session_id = self.session_id.clone();
        let call = cx.background_spawn(async move { client.approval_list_pending(&params) });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let Ok(pending) = call.await else { return };
            let _ = this.update(cx, |this, cx| {
                let fold = |this: &mut Self, method: &str, value: &serde_json::Value| {
                    this.fold.apply(MuseEvent::Notification {
                        method: method.to_owned(),
                        params: value.clone(),
                        cursor: value.get("viewCursor").and_then(|v| v.as_str()).map(str::to_owned),
                        session_id: Some(session_id.clone()),
                    });
                };
                for approval in &pending.approvals {
                    if let Ok(value) = serde_json::to_value(approval) {
                        fold(this, "approval/requested", &value);
                    }
                }
                for request in &pending.user_inputs {
                    if let Ok(value) = serde_json::to_value(request) {
                        fold(this, "userInput/requested", &value);
                    }
                }
                this.observe_clocks(cx);
                cx.notify();
            });
        }));
    }

    /// Whether anything is waiting on the person, for the needs-you banner.
    fn waiting_on_you(&self) -> Option<(usize, usize)> {
        let side = self.fold.side(&self.session_id)?;
        let (approvals, questions) = (side.pending_approvals.len(), side.pending_inputs.len());
        (approvals + questions > 0).then_some((approvals, questions))
    }

    /// The live retry row's data: attempt, bound, what is left of the backoff,
    /// and the reason the provider gave.
    fn retry_countdown(&self) -> Option<(u32, u32, u64, String)> {
        let retry = self.fold.side(&self.session_id)?.retry.clone()?;
        let started = self.retry_started.as_ref()?.1;
        let remaining = retry.retry_delay_ms.saturating_sub(started.elapsed().as_millis() as u64);
        Some((retry.attempt, retry.max_attempts, remaining, retry.reason))
    }
}

/// The pending question a block id names, resolved once so the callers do not
/// each re-walk the fold.
struct Pending {
    input_id: String,
    question_id: String,
    multi: bool,
    labels: Vec<String>,
    questions: usize,
}

/// The winning resolution an `approvalAlreadyResolved` error carries.
fn resolution_of(error: &MuseError) -> Option<ApprovalResolutionSummary> {
    match error {
        MuseError::Rpc(rpc) => rpc.data.as_ref().and_then(|data| data.resolution.clone()),
        _ => None,
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

/// The `(commandId, text)` a captured `turn/start` was submitted with.
///
/// `displayText` is what the person typed and is preferred; the text parts are
/// the fallback, joined the way the server joins them. A fresh `turn/start`'s
/// `commandId` equals its `turnId`, which is what makes this a turn-text map.
fn submitted_text(line: &str) -> Option<(String, String)> {
    let frame: serde_json::Value = serde_json::from_str(line).ok()?;
    if frame.get("method")?.as_str()? != "turn/start" {
        return None;
    }
    let params = frame.get("params")?;
    let command_id = params.get("commandId")?.as_str()?.to_owned();
    let text = match params.get("displayText").and_then(|v| v.as_str()) {
        Some(text) => text.to_owned(),
        None => params
            .get("input")?
            .as_array()?
            .iter()
            .filter_map(|part| part.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join(""),
    };
    (!text.is_empty()).then_some((command_id, text))
}

/// The `s` a count needs, so a banner never says "1 approvals".
fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
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
        ReasoningEffort::Max => Wire::Max,
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
/// Debug-only frame timer (C1/P1): with `HARNESS_FRAME_STATS=1`, record
/// every `render_transcript` duration and print p50/p90/p99 to stderr every
/// 120 frames, so the stress capture reports bounded per-frame cost.
fn record_frame_stats(elapsed: std::time::Duration) {
    use std::sync::{Mutex, OnceLock};
    static SAMPLES: OnceLock<Mutex<Vec<u128>>> = OnceLock::new();
    if std::env::var("HARNESS_FRAME_STATS").as_deref() != Ok("1") {
        return;
    }
    let samples = SAMPLES.get_or_init(|| Mutex::new(Vec::with_capacity(128)));
    let Ok(mut samples) = samples.lock() else {
        return;
    };
    samples.push(elapsed.as_micros());
    if samples.len() >= 120 {
        let mut sorted = samples.clone();
        sorted.sort_unstable();
        let at = |q: f64| sorted[((q * sorted.len() as f64) as usize).min(sorted.len() - 1)];
        eprintln!(
            "harness-frame-stats n={} p50={}us p90={}us p99={}us max={}us",
            sorted.len(),
            at(0.5),
            at(0.9),
            at(0.99),
            sorted[sorted.len() - 1]
        );
        samples.clear();
    }
}

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

/// The `/fork` picker's row label: the first line of the user prompt that
/// started the turn, falling back to the turn's own first text line.
fn fork_label(prompt: &str, blocks: &[Block]) -> String {
    let first = prompt.lines().next().unwrap_or("").trim();
    if !first.is_empty() {
        return first.to_owned();
    }
    blocks
        .iter()
        .find_map(|block| match block {
            Block::Text { text, .. } => {
                let line = text.lines().next().unwrap_or("").trim();
                (!line.is_empty()).then(|| line.to_owned())
            }
            _ => None,
        })
        .unwrap_or_else(|| "(no prompt)".to_owned())
}

/// The `/fork` picker's row detail: the turn's wall-clock time, in the same
/// words as the per-turn footer.
fn fork_time(meta: &aui_protocol::TurnMeta) -> String {
    aui::transcript::format_duration(meta.duration_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fork_takes_an_optional_turn_number() {
        assert_eq!(Command::parse_line("/fork"), Some((Command::Fork, "")));
        assert_eq!(Command::parse_line("/fork 2"), Some((Command::Fork, "2")));
        assert_eq!(Command::parse_line("/fork  "), Some((Command::Fork, "")));
    }

    #[test]
    fn a_fork_row_is_the_prompt_first_line() {
        let blocks = vec![Block::Text { text: "reply".into(), streaming: false }];
        assert_eq!(fork_label("Fix the parser\nmore detail", &blocks), "Fix the parser");
        assert_eq!(fork_label("  padded  ", &blocks), "padded");
        assert_eq!(fork_label("", &blocks), "reply");
        assert_eq!(fork_label("", &[]), "(no prompt)");
    }

    #[test]
    fn a_fork_time_is_the_turn_time_in_footer_words() {
        let meta = aui_protocol::TurnMeta { duration_ms: 12_400, ..Default::default() };
        assert_eq!(fork_time(&meta), "12.4 s");
    }

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
    fn tail_slack_stays_put() {
        // Tail-follow slack (C1): the virtual list pins the tail through
        // `is_scrolled_to_end`, and this is the slack readers still count as
        // "at the tail" — kept as a named constant so the behaviour stays put.
        assert_eq!(TAIL_SLACK, 48.0);
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
