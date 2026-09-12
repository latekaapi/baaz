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
    AssistantTurnAction, LinkTarget, SelectionKey, TextSelection, ToolCardIntent, ToolGroupIntent,
    UserTurnAction, needs_you_banner, retry_row, status_row, StatusLead, turn_selected_text,
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
    UnframedViewNotification,
};
use muse_client::{new_command_id, MuseClient, MuseError, MuseEvent};

use crate::conn::{self, Severity};
use crate::overlays::{Command, Menu, MenuKind, Overlays, EFFORTS, MODES};
use crate::transcript::{self, Cards, Folds, FullOutput, FullOutputState, PlanAction};
use crate::shot::CaptureToken;
use crate::wire::WireCall;
use crate::{attachments, files, full_output, history, images, plan, search, skills};

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
/// The first backfill page's limit. The trace says page 1 of a 13 MB session
/// (790 events) fetches in ~3 ms and folds in ~11 ms, so a smaller first
/// page would buy nothing: every page, first included, runs at the ceiling.
const FIRST_PAGE_LIMIT: u32 = PAGE_LIMIT;
/// The transcript's own padding, matching the assistant screen's `.tr`.
const TRANSCRIPT_PAD_X: f32 = scale::SP_7;
/// The transcript measure: the design bounds the transcript column to
/// ~760-800 px and centres it, composer included. At the 1.1 text scale that
/// is 880 px here (800 x 1.1). Centre-pane rows keep their `TRANSCRIPT_PAD_X`
/// gutters and centre their content inside them with [`centred`], so below
/// 880 px of content nothing changes.
const TRANSCRIPT_MEASURE: f32 = 880.0;
/// Bound a full-width centre-pane row to the transcript measure and centre it.
///
/// An inner `w_full` wrapper: capped at `TRANSCRIPT_MEASURE`, the leftover
/// split by auto margins.
fn centred(content: impl IntoElement) -> AnyElement {
    div().w_full().max_w(px(TRANSCRIPT_MEASURE)).mx_auto().child(content).into_any_element()
}
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
/// The height hint a fresh transcript row carries before it is measured: a
/// typical settled turn. Unmeasured rows without a hint count as 0 px in the
/// list's sum tree, so one upward wheel event clamps at the head and
/// teleports there (H2); with the hint the scrollbar and the wheel map onto
/// roughly the right rows until measurement replaces it.
pub(crate) const TURN_HEIGHT_HINT: f32 = 120.0;
/// The gap the caret popovers leave above the composer, matching the
/// library's own `.pop{margin-bottom:8px}`.
const POPOVER_GAP: f32 = 8.0;
/// How tall a caret popover may grow before it scrolls. The `/` menu lists
/// every client command **and** every installed skill, and a workspace with
/// twenty skills would otherwise reach past the top of the window.
const POPOVER_MAX_H: f32 = 560.0;
/// How many skill rows the `/` menu offers at once.
const SKILL_ROWS: usize = 8;

mod approvals;
mod clocks;
mod commands;
mod composer;
mod events;
mod questions;
mod render;
mod scripting;
mod shell;

/// What every session view is handed by the window that opens it.
///
/// Four values that belong to the application rather than to any one session:
/// which provider the window is pointed at, which workspace it is in, the
/// shared overlay state, and this window's capture token. They travel together
/// because they are set together, once, at construction.
pub struct SessionHost {
    /// The provider id every turn this session starts carries.
    pub provider_id: String,
    /// The workspace root, which is also the prompt history's key.
    pub workspace: String,
    /// Everything that floats, shared with the application (spec §2.3).
    pub overlays: Entity<Overlays>,
    /// This window's `--screenshot` flags (see [`CaptureToken`]).
    pub capture: CaptureToken,
}

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
    /// `/search`: open the full-text search palette.
    Search,
    /// `/fork` with nothing named: open the turn picker over the session's
    /// completed assistant turns.
    ForkPicker,
    /// The backfill's first page applied: the application pins the tail while
    /// later pages land. The view is already on screen; this swaps nothing.
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
    ///
    /// Behind an `Rc` because [`Folds`] takes a snapshot every frame and a
    /// toggle is rare: the frame clones a refcount, `toggle_fold` clones the
    /// set once with `Rc::make_mut` (finding `performance-3`).
    toggled: Rc<HashSet<String>>,
    /// Virtualized transcript list (gpui `list()`, top-aligned, one item
    /// per turn) and the item count it was last synced to. Only visible rows
    /// are built and laid out per frame; `splice` keeps indices stable across
    /// folds (C1). Tail-follow rides `is_scrolled_to_end`/`scroll_to_end`,
    /// gated by the `follow` flag each fold change sets.
    list_state: ListState,
    list_len: usize,
    /// What `render_transcript` reads every frame (C1): one snapshot shared
    /// by steady-state frames, refreshed only when the fold changes (length
    /// drift or `follow`), so per-frame cost stays bounded as the transcript
    /// grows. Event handlers keep reading the live fold.
    /// Each turn is shared rather than copied: a refresh re-clones the turns
    /// that actually changed and hands the rest back their existing `Rc`, so
    /// a streaming chunk no longer deep-clones the whole transcript
    /// (finding `performance-4`).
    cached_turns: Rc<Vec<Rc<Turn>>>,
    /// The truncated-output map for the cached turns (same refresh rule).
    cached_full_output: Rc<HashMap<String, FullOutput>>,
    /// The newest approval still awaiting a decision, and its choices, found
    /// while the refresh above already walks every block (finding
    /// `performance-2`). Render used to reverse-scan the whole transcript and
    /// clone the choices on every frame; now the scan runs once per fold
    /// change and every reader borrows this.
    cached_pending_approval: Option<(String, Vec<aui_protocol::ApprovalChoice>)>,
    /// Set by every event that changed the transcript; the next frame consumes
    /// it and scrolls to the tail if the reader was already there.
    follow: bool,
    /// The transcript's text selections (library selection model), keyed by
    /// turn id: `(markdown source, selection)`. One cell at a time — a new
    /// drag replaces whatever was held — cleared on Escape or a plain click
    /// elsewhere; ⌘C copies the held one (C8b). Per turn, not one shared
    /// cell: the library scopes cell keys (`p0`, `b0-0`, …) to the markdown
    /// view that rendered them, so a single shared selection would light up
    /// the same key in every turn at once.
    text_selections: Rc<HashMap<String, (String, TextSelection)>>,
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
    titles: Rc<HashMap<String, String>>,
    /// Draw the cards settled rather than entering.
    ///
    /// A `--screenshot` run renders a handful of frames and then quits, so a
    /// staggered button that is still fading in is simply missing from the PNG.
    /// The capture wants the card as a person sees it a moment later.
    at_rest: bool,
    /// The two flags this window's `--screenshot` wait reads (see
    /// [`CaptureToken`]). The application's own, handed down at construction,
    /// so a second window's capture can never read this one's.
    capture: CaptureToken,
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
    /// Whether the composer's draft trims to nothing.
    ///
    /// Maintained rather than read, because reading it meant copying the whole
    /// draft out of the textarea on every frame only to ask whether it was
    /// blank (finding `performance-8`). `InputEvent::Change` covers typing;
    /// [`Self::note_draft`] covers the writes that set the value
    /// programmatically, which emit no change event at all — the library's
    /// `set_value` suppresses them on purpose.
    draft_empty: bool,
    /// Where the composer is in this workspace's prompt history.
    history: history::Cursor,
    /// The `@` picker's last completed rank and what it was ranked for. The
    /// rank runs on the background executor (a 5 000-path subsequence scan per
    /// keystroke does not belong on the UI thread); the render path reads this
    /// cache and never ranks.
    mention_cache: Vec<String>,
    /// The filter `mention_cache` was ranked for.
    mention_cache_for: String,
    /// How many files the rank above ran over: a re-walk invalidates it.
    mention_files_len: usize,
    /// The filter a background rank is computing, if any.
    mention_pending: Option<String>,
    /// Monotonic id for mention ranks; only the latest result is kept.
    mention_epoch: u64,
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

impl WireCall for SessionView {
    fn wire_tasks(&mut self) -> &mut Vec<Task<()>> {
        &mut self.tasks
    }
}

impl SessionView {
    /// A view over `session_id`. Nothing is loaded yet: the caller either just
    /// started the session or is about to [`SessionView::backfill`] it.
    pub fn new(
        session_id: String,
        client: Option<Arc<MuseClient>>,
        host: SessionHost,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let SessionHost { provider_id, workspace, overlays, capture } = host;
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
            toggled: Rc::new(HashSet::new()),
            list_state: ListState::new(
                0,
                // Top, not Bottom: a short transcript starts at the top
                // instead of leaving a void above it. Tail-follow is owned
                // by the `follow` flag below (`scroll_to_end` when the
                // reader was at the tail), never by the alignment.
                ListAlignment::Top,
                // Overdraw is a viewport's worth (`WINDOW_H`): rows past the
                // visible edge are measured once each and never re-laid out
                // per frame, so the cost is one measurement per row while a
                // flick's whole travel stays on measured heights instead of
                // clamping into zero-height territory (H2).
                px(crate::WINDOW_H),
            ),
            list_len: 0,
            cached_turns: Rc::new(Vec::new()),
            cached_full_output: Rc::new(HashMap::new()),
            cached_pending_approval: None,
            follow: true,
            text_selections: Rc::new(HashMap::new()),
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
            titles: Rc::new(HashMap::new()),
            at_rest: false,
            capture,
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
            draft_empty: true,
            history: history::Cursor::new(Vec::new()),
            workspace_key,
            mention_cache: Vec::new(),
            mention_cache_for: String::new(),
            mention_files_len: 0,
            mention_pending: None,
            mention_epoch: 0,
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

    /// Whether the composer is empty, which is what Escape branches on — and
    /// what the send button is enabled by, once a frame (finding
    /// `performance-8`).
    pub fn draft_is_empty(&self, _cx: &gpui::App) -> bool {
        self.draft_empty
    }

    /// Re-read the draft's emptiness after something wrote it.
    ///
    /// The one place [`Self::draft_empty`] is computed. Every caller either
    /// handled an `InputEvent::Change` or just called `set_value`, which
    /// raises no event of its own.
    pub(crate) fn note_draft(&mut self, cx: &gpui::App) {
        self.draft_empty = self.composer.read(cx).value().trim().is_empty();
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
        // The same handler the `choose:<n>` step reaches, with the same
        // 1-based numbering the digits on the card carry.
        self.step_choose(&(index + 1).to_string(), window, cx);
    }

    /// Whether plan mode is on, for the app's Shift+Tab.
    pub fn plan_mode(&self) -> bool {
        self.plan
    }

    /// What the application knows and the session does not: what the other
    /// sessions in this workspace are called (for a `ForkedFrom` marker), and
    /// whether `initialize` granted `userShell`.
    pub fn set_context(&mut self, titles: HashMap<String, String>, user_shell: bool) {
        self.titles = Rc::new(titles);
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

    /// Whether this view folds a capture file rather than a live session.
    /// Replayed views are never parked in the session cache: their transcript
    /// is the file, and reopening re-reads it.
    pub fn is_replay(&self) -> bool {
        self.replay
    }

    /// Pin the tail after history landed. The application calls this on
    /// [`SessionEvent::HistoryReady`], which the backfill emits after its
    /// first page — not at the end — so later pages keep arriving under a
    /// pinned tail.
    pub fn follow_tail(&mut self, cx: &mut Context<Self>) {
        self.follow = true;
        cx.notify();
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
        let (events, sent) = match parse_replay_file(path) {
            Ok(parsed) => parsed,
            Err(error) => {
                self.banner = Some(format!("{}: {error}", path.display()));
                cx.notify();
                return;
            }
        };
        for event in events {
            self.fold.apply(event);
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

    /// Start a `--bench` run over a capture: read-only like a replay, but the
    /// events are fed one per cadence tick by the bench driver rather than
    /// folded all at once, so streaming cost is real.
    pub fn begin_bench_replay(&mut self, session_id: String, sent: Vec<(String, String)>, cx: &mut Context<Self>) {
        self.replay = true;
        self.session_id = session_id;
        for (command_id, text) in sent {
            self.fold.record_command(&self.session_id, &command_id, &text);
        }
        self.follow = true;
        cx.notify();
    }

    /// Drive the transcript list to `frac` of the folded turns (0 = top,
    /// 1 = tail) without touching the pointer, for `--bench --bench-scroll`.
    pub fn bench_scroll_to(&mut self, frac: f32, cx: &mut Context<Self>) {
        self.follow = false;
        let len = self.fold.session(&self.session_id).map(|s| s.turns.len()).unwrap_or(0);
        let ix = ((len.saturating_sub(1) as f32) * frac.clamp(0.0, 1.0)) as usize;
        self.list_state.scroll_to(gpui::ListOffset { item_ix: ix, offset_in_item: px(0.0) });
        cx.notify();
    }

    /// Pin the transcript to the tail, for `--bench --bench-scroll tail`.
    pub fn bench_scroll_tail(&mut self, cx: &mut Context<Self>) {
        self.follow = true;
        self.list_state.scroll_to_end();
        cx.notify();
    }

    /// The transcript list's current scroll offset, sampled once per frame by
    /// `--bench --bench-scroll wheel`.
    pub fn bench_list_top(&self) -> gpui::ListOffset {
        self.list_state.logical_scroll_top()
    }

    /// Whether the transcript list is pinned at its tail, sampled with the
    /// offset above: a wheel event that leaves the position unchanged *at*
    /// the limit is clamped, not stalled. Mirrors the app's own follow
    /// logic (`sync_virtual_list`, `render_needs_you`), which treats the
    /// unknown-height `None` as at the tail.
    pub fn bench_list_end(&self) -> bool {
        self.list_state.is_scrolled_to_end().unwrap_or(true)
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
        self.note_draft(cx);
    }

    /// Move the keyboard to the composer.
    pub fn focus_composer(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.composer.focus_handle(cx), cx);
    }
}

/// What the inline banner's action does, when the failure is one the person can
/// do something about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BannerAction {
    /// Resend a turn that never left, with the text it carried.
    RetryTurn(String),
    /// Re-run a user shell command that never left.
    RetryShell(String),
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
/// A capture file parsed: fold events plus the prompts turns were sent with.
type ParsedReplay = (Vec<MuseEvent>, Vec<(String, String)>);

/// Read a capture file into fold events plus the prompts turns were sent
/// with: the `<-- ` half the fixture tests fold, and the `--> ` half an
/// error card's retry needs. Shared by `--replay` (folds all at once) and
/// `--bench` (feeds one per cadence tick).
pub(crate) fn parse_replay_file(path: &std::path::Path) -> Result<ParsedReplay, std::io::Error> {
    let text = std::fs::read_to_string(path)?;
    let mut events = Vec::new();
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
                    events.push(event);
                }
            }
            Ok(None) => {}
            Err(error) => crate::harness_log!("{}:{}: {error}", path.display(), number + 1),
        }
    }
    Ok((events, sent))
}

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
/// Whether element-construction timing is recorded: `HARNESS_FRAME_STATS=1`
/// or `--bench`, which implies it. Read once (A-MECH-14): the flag never
/// changes at runtime, and the old code paid an env lookup per frame.
fn frame_stats_enabled() -> bool {
    use std::sync::OnceLock;
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("HARNESS_FRAME_STATS").as_deref() == Ok("1")
            || BENCH_FRAMES.load(std::sync::atomic::Ordering::Relaxed)
    })
}

/// Set by `--bench` before the window boots: frame stats are on, and the
/// periodic stderr percentiles stay quiet — the bench prints its own table
/// from the full sample at the end.
pub(crate) fn enable_frame_stats_for_bench() {
    BENCH_FRAMES.store(true, std::sync::atomic::Ordering::Relaxed);
}

static BENCH_FRAMES: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The recorded element-construction samples (microseconds), shared by the
/// periodic stderr percentiles and the bench's end-of-run drain.
static FRAME_SAMPLES: std::sync::OnceLock<std::sync::Mutex<Vec<u128>>> = std::sync::OnceLock::new();

/// One paint timestamp per recorded sample, kept only while `--bench` holds
/// them for its frame-interval table (bounded by the run, not by the
/// session: long `HARNESS_FRAME_STATS` runs pay no timestamp vec).
static FRAME_TIMES: std::sync::OnceLock<std::sync::Mutex<Vec<std::time::Instant>>> = std::sync::OnceLock::new();

static FRAME_SAMPLE_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Debug-only frame timer (C1/P1): with `HARNESS_FRAME_STATS=1`, record
/// every `render_transcript` duration and print p50/p90/p99 to stderr every
/// 120 frames, so the stress capture reports bounded per-frame cost.
fn record_frame_stats(elapsed: std::time::Duration) {
    if !frame_stats_enabled() {
        return;
    }
    FRAME_SAMPLE_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if BENCH_FRAMES.load(std::sync::atomic::Ordering::Relaxed) {
        FRAME_TIMES
            .get_or_init(|| std::sync::Mutex::new(Vec::new()))
            .lock()
            .map(|mut times| times.push(std::time::Instant::now()))
            .ok();
    }
    let samples = FRAME_SAMPLES.get_or_init(|| std::sync::Mutex::new(Vec::with_capacity(128)));
    let Ok(mut samples) = samples.lock() else {
        return;
    };
    samples.push(elapsed.as_micros());
    if !BENCH_FRAMES.load(std::sync::atomic::Ordering::Relaxed) && samples.len() >= 120 {
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

/// How many `render_transcript` constructions have been recorded. Every frame
/// renders the transcript, so the delta over a quiet window is the frame
/// count there — the bench's idle assertion.
pub(crate) fn frame_sample_count() -> u64 {
    FRAME_SAMPLE_COUNT.load(std::sync::atomic::Ordering::Relaxed)
}

/// Drain the recorded element-construction samples (microseconds), for the
/// bench's end-of-run table.
pub(crate) fn take_frame_samples() -> Vec<u128> {
    let samples = FRAME_SAMPLES.get_or_init(|| std::sync::Mutex::new(Vec::new()));
    samples.lock().map(|mut samples| std::mem::take(&mut *samples)).unwrap_or_default()
}

/// Drain the recorded paint timestamps, for the bench's frame-interval table.
pub(crate) fn take_frame_times() -> Vec<std::time::Instant> {
    let times = FRAME_TIMES.get_or_init(|| std::sync::Mutex::new(Vec::new()));
    times.lock().map(|mut times| std::mem::take(&mut *times)).unwrap_or_default()
}

/// One `view/page` result as foldable wire events, in page order.
///
/// Events whose params do not serialize are dropped, exactly as the old
/// whole-transcript backfill did: a page element the schema cannot carry is
/// a transport concern, not a transcript hole.
pub(crate) fn page_events(session_id: &str, events: &[UnframedViewNotification]) -> Vec<MuseEvent> {
    events
        .iter()
        .filter_map(|event| {
            let params = serde_json::to_value(&event.params).ok()?;
            let event_cursor = params.get("viewCursor").and_then(|v| v.as_str()).map(str::to_owned);
            Some(MuseEvent::Notification {
                method: event.method.clone(),
                params,
                cursor: event_cursor,
                session_id: Some(session_id.to_owned()),
            })
        })
        .collect()
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
    aui::transcript::format_duration(meta.duration_ms).to_string()
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
    fn the_height_hint_is_a_typical_settled_turn() {
        // First-fill hint (H2): what an unmeasured row counts as before
        // layout measures it. Kept as a named constant so the assumption
        // stays visible.
        assert_eq!(TURN_HEIGHT_HINT, 120.0);
    }

    #[test]
    fn pixel_scrolling_needs_row_heights() {
        // The scroll-jank mechanism (H2), without a window: rows with no
        // hint count as 0 px in the sum tree, so an upward wheel event from
        // the tail has no heights to move through — the offset sticks at
        // the past-end anchor (the element's wheel path then clamps through
        // that zero-stack and resolves at the head, which `bench-scroll
        // wheel` shows end to end). With the uniform first-fill hint the
        // same event climbs exactly one hinted row. `scroll_by` is the pixel
        // arithmetic the wheel handler runs, sign included.
        let bare = ListState::new(0, ListAlignment::Top, px(48.0));
        bare.reset(592);
        bare.scroll_to_end();
        bare.scroll_by(px(-120.0));
        assert_eq!(bare.logical_scroll_top().item_ix, 592);
        let hinted = ListState::new(0, ListAlignment::Top, px(crate::WINDOW_H));
        hinted.reset_with_uniform_height(592, px(TURN_HEIGHT_HINT));
        hinted.scroll_to_end();
        hinted.scroll_by(px(-120.0));
        assert_eq!(hinted.logical_scroll_top().item_ix, 591);
    }

    #[test]
    fn an_unlaid_list_reports_no_tail_state() {
        // What `bench_list_end` unwraps: before the first layout there are
        // no bounds, so the tail state is unknown — and the app treats
        // unknown as at the tail (`sync_virtual_list`, `render_needs_you`).
        let fresh = ListState::new(0, ListAlignment::Top, px(crate::WINDOW_H));
        assert_eq!(fresh.is_scrolled_to_end(), None);
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

    #[test]
    fn a_page_maps_to_foldable_events_in_order() {
        use muse_client::schema::{
            RecordPosition, SourceRange, StreamRef, UnframedViewNotificationParams,
        };
        let element = |cursor: &str| UnframedViewNotification {
            method: "item/completed".to_owned(),
            params: UnframedViewNotificationParams {
                session_id: "s".to_owned(),
                source_range: SourceRange {
                    first: RecordPosition { id: "r".to_owned(), sequence: 1 },
                    last: RecordPosition { id: "r".to_owned(), sequence: 1 },
                    stream: StreamRef { id: "run".to_owned(), kind: "run".to_owned() },
                },
                view_cursor: cursor.to_owned(),
                extra: serde_json::Map::new(),
            },
        };
        let events = page_events("s", &[element("v:s:1"), element("v:s:2")]);
        assert_eq!(events.len(), 2, "one wire event per page element");
        for (event, cursor) in events.iter().zip(["v:s:1", "v:s:2"]) {
            match event {
                MuseEvent::Notification { method, session_id, cursor: at, .. } => {
                    assert_eq!(method, "item/completed");
                    assert_eq!(session_id.as_deref(), Some("s"));
                    assert_eq!(at.as_deref(), Some(cursor), "the fold's reconnect cursor survives");
                }
                other => panic!("a page element is a notification, got {other:?}"),
            }
        }
    }
}
