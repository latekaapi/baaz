//! The session apart from its view (docs/82 4.8, audit B S-9, D Q5 #2).
//!
//! A [`SessionRuntime`] is one session's model: the provider lane (the
//! `Provider` handle and the task draining its events), the
//! [`SessionStore`](session_store::SessionStore) the transcript folds into,
//! the turn state (sending, running, the queue, the optimistic bubbles), the
//! approvals and questions waiting on the person, the model / effort / mode
//! picks, plan mode, the outgoing attachments, the banners and the clocks.
//! It draws nothing — there is no `Render` here — and it lives in the
//! [`SessionRegistry`](crate::app) whether or not a [`SessionView`] shows it,
//! so a turn runs to its end, settles its row and lands its ledger row with
//! no view at all, and a view can be dropped (evicted) and built again over
//! the same runtime.
//!
//! It speaks twice:
//! - [`SessionEvent`]: what the application must decide (rows, records,
//!   titles, handoffs, dialogs). The root subscribes to every runtime.
//! - [`RuntimeEvent`]: what the view must redraw, named: the transcript's
//!   list operations (Zed's `AcpThreadEvent` shape) and the facets beside
//!   it (the turn, the banner, the composer, the menus, the clocks). The
//!   view redraws on every one; the runtime's own methods do not `notify`
//!   (docs/82 4.9c), so a watcher of its state subscribes as well as
//!   observes (an `Entity::as_mut` write from outside still notifies).
//!
//! Dropping the runtime hangs up its provider child ([`Drop`]); dropping a
//! view does not.

use super::*;

/// One session's model and its provider lane. See the module doc.
pub struct SessionRuntime {
    /// The session id this runtime follows.
    pub(crate) session_id: String,
    /// What the view renders, for every lane: the transcript, the facts
    /// beside it, the changed turns and the prompts this view sent.
    pub(super) store: session_store::SessionStore,
    /// The lane is the read-only one ([`SessionView::new`]): nothing live is
    /// behind this view, so a provider's offline fallbacks (Claude Code's
    /// supplied catalog) stand in for asking.
    pub(super) read_only: bool,
    /// Tests: the wire translation [`Self::apply`] folds raw events through.
    #[cfg(test)]
    pub(super) test_wire: provider_muse::replay::Folder,
    /// A `--replay` session: the transcript is a file, and every command is
    /// refused rather than quietly dropped.
    pub(super) replay: bool,
    /// `meta`, or `echo` under `BAAZ_PROVIDER=echo`.
    pub(super) provider_id: String,
    /// Which lane owns this view for its whole life: the muse pump or a
    /// provider task. Set at construction and never reassigned — there is no
    /// setter, so no code path can attach a second writer.
    pub(super) lane: lane::Lane,
    /// Approvals from a new provider (Claude Code / Codex) waiting on the
    /// person. The legacy lane has no room for them, so they live here and
    /// render on the same approvals surface.
    pub(super) external_approvals: ExternalApprovalStore,
    /// Decisions the press sent that the server has not answered yet: the
    /// `DecideApproval` commands the provider lane drains, oldest first.
    /// The card waits on them; it never settles on the press.
    pub(super) external_outbox: Vec<ProviderCommand>,
    /// Questions a new provider raised, waiting on the person. The full
    /// prompt arrived as a delta; this is the tap on the shoulder, kept so
    /// the question surface can answer it.
    pub(super) external_questions: lane::ExternalQuestions,
    /// The workspace the session runs in, for the header and the empty state.
    pub(super) workspace: String,
    /// The spans sent quotes were taken from, by (source turn, words): what
    /// a click on the sent quote flashes. This session's quotes only — a
    /// reopened session scrolls back without the flash.
    pub(super) sent_quote_spans: HashMap<(String, String), (String, aui::transcript::MessageSelection)>,
    /// Last elapsed second the turn ticker painted, so the 1 Hz clock
    /// notifies only when the displayed number changes (P2).
    pub(super) last_tick_secs: Option<u64>,
    pub(super) running: Option<Running>,
    /// Turn ids this view already saw complete. A `turn/started` for one of
    /// them is the re-attach replay, not new work, and never marks running.
    pub(super) completed_turns: HashSet<String>,
    /// Provider lane: submit-ack turn id → the id that turn streams under,
    /// from the adapter's `TurnIdentified` (Claude Code acks before the
    /// CLI names the turn). Everything keyed by an ack's id resolves
    /// through [`SessionRuntime::streamed_turn_id`]. Bounded like
    /// `completed_turns`.
    pub(super) turn_aliases: HashMap<String, String>,
    /// A `turn/start` is in flight and no `turn/started` has arrived yet.
    pub(super) submitting: bool,
    /// Optimistic user turns folded at send, oldest first, waiting on the
    /// provider's own user turn to replace them (X1). A refused submit or
    /// a dead connection removes them; each echo consumes the earliest
    /// still pending (X1b). An early stop drains them and hands the newest
    /// text back to the composer.
    pub(super) pending_optimistic: Vec<PendingOptimistic>,
    /// The running turn whose last model call ended on a whole reply: a
    /// token-usage reading landed while its reply stood complete. What
    /// [`Self::on_reminder_tail`] stands on, so a reply that completes
    /// while the model is still composing its next tool call never reads
    /// as finished. Cleared when the turn moves on or ends.
    pub(super) reply_done: Option<String>,
    /// `fixture-models` filled the model menu: no catalog request may
    /// replace it (captures only).
    pub(super) models_fixture: bool,
    /// Sends made during the reminder tail that the provider queued, by
    /// request id → their optimistic bubble: the bubble stands for the
    /// queued words (no queue-strip row) until the turn starts and its
    /// echo replaces it.
    pub(super) tail_queued: HashMap<String, String>,
    /// The skills the session's own provider offers as `/name`
    /// ([`provider::ProviderEvent::SkillsListed`]): the `/` menu's Skills
    /// section on a provider whose skills the app does not manage (docs/82
    /// 5.5).
    pub(super) provider_skills: Vec<crate::skills::Skill>,
    /// History is still being paged in behind the live stream.
    pub(super) loading_history: bool,
    /// A `view/page` chain is in flight. Separate from `loading_history`,
    /// which the open path raises before the resume ack so the first frame
    /// shows the loading row: guarding the chain on the display flag made
    /// `backfill` skip every cache-miss open.
    pub(super) backfill_running: bool,
    /// A stale-sidecar page failure was already retried once in this backfill
    /// chain: the second failure reports instead.
    pub(super) backfill_stale_retried: bool,
    /// The lane backfill's page in flight is its first: the tail-pin cue
    /// follows it.
    pub(super) backfill_first: bool,
    /// The lease is gone — the session is open in another window (or the
    /// child that held it exited) — so nothing is sent until a later resume
    /// succeeds. `Some` is the banner text, re-shown on
    /// every refused send so a dismissal cannot strand a silent refusal.
    pub(super) lease_notice: Option<String>,
    /// The inline banner over the composer: one recoverable command error.
    pub(super) banner: Option<String>,
    /// The banner's action, when the error is one the person can do something
    /// about: the label and what pressing it does.
    pub(super) banner_action: Option<BannerAction>,
    /// The banner shows the Auto-fallback notice (a model pick left Auto
    /// for Ask), which a later mode pick takes down.
    pub(super) banner_is_mode_fallback: bool,
    /// When the lane's child last died with no turn finished since: a
    /// second death inside [`LOST_TWICE_WINDOW`] turns the banner into the
    /// failure view. A finished turn clears it.
    pub(super) lost_at: Option<std::time::Instant>,
    /// The lane's child died twice within [`LOST_TWICE_WINDOW`] (docs/71
    /// §2): the lost banner grows in place into the typed failure — the
    /// kind's sentence, the child's last stderr lines, Retry and the kind's
    /// action — under the transcript, which stays (owner 2026-10-30: the
    /// old full-centre view hid the conversation and the composer). A
    /// restored connection or a finished turn takes it down.
    pub(super) child_failure: Option<ChildFailure>,
    /// The billing guard's banner, and whether it is refusing turns.
    pub(super) tier_banner: Option<TierBanner>,
    /// A live Muse draft opened while Muse itself is still starting (it
    /// starts on demand, docs/50 P1): the chip and the model menu say so
    /// instead of reading "Muse" over an empty menu. Set by the
    /// application, which owns the connection.
    pub(super) muse_starting: bool,
    /// Answers gathered so far for a multi-question request, keyed by
    /// `userInputId` then `questionId`. MSP settles the whole prompt at once, so
    /// the app holds the earlier answers until the last question is answered.
    pub(super) answers: HashMap<String, HashMap<String, provider::QuestionAnswer>>,
    /// When each pending question's auto-resolution clock started, keyed by
    /// `userInputId`. MSP sends a duration, never a deadline.
    pub(super) question_started: HashMap<String, Instant>,
    /// When the live `turn/retryScheduled` was observed, for the same reason.
    pub(super) retry_started: Option<(String, Instant)>,
    /// A 1 s clock, held only while a countdown is on screen.
    pub(super) countdown: Option<Task<()>>,
    /// A prompt handed back by a retraction, waiting for a frame with a
    /// `Window` in it to reach the composer.
    pub(super) pending_prompt: Option<String>,
    /// The model catalog, fetched when the picker opens. A snapshot: MSP has no
    /// catalog subscription.
    pub(super) models: Vec<ModelCatalogEntry>,
    /// Why the catalog is missing, when it is: the typed reason the picker
    /// shows instead of a silently empty menu. `None` means the last fetch
    /// succeeded (or none ran yet).
    pub(super) models_error: Option<String>,
    /// The model the person picked on the provider lane (Claude Code /
    /// Codex): what the chip reads until the server echoes. The muse lane
    /// never sets this — its chip moves only on `session/modelChanged` —
    /// and no lane ever changes provider, which has no setter by design.
    pub(super) pending_model: Option<String>,
    /// A muse mode pick's two wire steps (approval mode and permission
    /// profile) are on the wire. Picks never interleave: one runs at a
    /// time, and the latest pick made meanwhile waits in
    /// [`SessionRuntime::mode_pick_queued`].
    pub(super) mode_pick_in_flight: bool,
    /// The newest muse mode pick made while another was on the wire —
    /// latest wins, an earlier waiting pick is dropped.
    pub(super) mode_pick_queued: Option<PermissionMode>,
    /// The Claude Code chip's pre-first-turn seed, resolved once: it reads
    /// up to four files, and the composer asks for it on every frame.
    pub(super) claude_seed_label: std::cell::OnceCell<SharedString>,
    /// The last model a finished turn reported on the provider lane, in
    /// arrival order: replayed history's footers land here first, so a
    /// reopened session's chip names the session's model instead of the
    /// provider. A later pick (see [`SessionRuntime::pending_model`]) or a
    /// live catalog row still wins; the muse lane never sets this.
    pub(super) history_model: Option<String>,
    /// The Codex catalog's per-model reasoning levels, folded from the
    /// neutral catalog: model id → its `supportedReasoningEfforts`, in
    /// provider order. A snapshot like [`SessionRuntime::models`], keyed because
    /// the effort menu follows the *selected* model, not the provider.
    pub(super) codex_efforts: HashMap<String, Vec<provider_codex::child::SupportedEffort>>,
    /// The Claude Code catalog's per-model effort levels, folded from the
    /// `initialize` answer's `models[]`: catalog `value` → its
    /// `supportedEffortLevels`. A snapshot like [`SessionRuntime::codex_efforts`];
    /// a row that names no levels (Haiku) maps to an empty list, which the
    /// effort menu explains instead of listing the launch flag's levels.
    pub(super) claude_efforts: HashMap<String, Vec<String>>,
    /// The approval modes each catalog model cannot run in, folded from
    /// the neutral catalog's `unavailable_modes` (Claude Code: Auto on a
    /// row without `supportsAutoMode`). Model id → its limits; a model
    /// with none has no entry. The mode picker reads it for the chip's
    /// model, so a refused switch is explained before it is tried.
    pub(super) mode_limits: HashMap<String, Vec<PermissionMode>>,
    /// The session's reasoning effort. `None` is "Default", which omits the
    /// field; client-side, because nothing on the wire reflects it back.
    pub(super) effort: Option<ReasoningEffort>,
    /// Plan mode, a client-side overlay (spec §3.1).
    pub(super) plan: bool,
    /// The approval mode plan mode displaced, restored on Accept or Reject.
    pub(super) plan_previous_mode: Option<PermissionMode>,
    /// The turn started in plan mode, whose reply becomes a `Block::Plan`.
    pub(super) plan_turn: Option<String>,
    /// Monotonic id source for the client-authored plan turns.
    pub(super) plan_seq: u64,
    /// Images waiting to go out with the next turn.
    pub(super) images: Vec<images::Image>,
    /// Monotonic id source for image chips.
    pub(super) image_seq: u64,
    /// Files waiting to go out with the next turn, as extracted text.
    pub(super) files: Vec<attachments::AttachedFile>,
    /// Monotonic id source for file chips.
    pub(super) file_seq: u64,
    /// Attachments that could not be read (D4): a failed chip each, the
    /// reason as its meta, until ✕ removes it. `(chip id, name, reason)`.
    pub(super) failed_chips: Vec<(String, String, String)>,
    /// Quotations waiting to go out with the next turn (D4): the words and
    /// the turn they were quoted from.
    pub(super) quotes: Vec<QuoteDraft>,
    /// The next quote block's number (ids stay unique per view).
    pub(super) quote_seq: u64,
    /// A Browse annotation batch waiting to go out with the next turn.
    pub(super) annotation_draft: Option<crate::sent::SentAnnotations>,
    /// What each sent turn carried (D4, `crate::sent`), in send order.
    pub(super) sent_items: Vec<crate::sent::SentItem>,
    /// The turn count when created files were last recorded: the next
    /// settle scans from there (`None`: never, so it sweeps everything).
    pub(super) created_files_through: Option<usize>,
    /// Where the composer is in this workspace's prompt history.
    pub(super) history: history::Cursor,
    /// The canonical workspace key the history file is written under.
    pub(super) workspace_key: String,
    /// Queued turns whose unqueue is in flight, with the intent and the text
    /// captured when the row's button was clicked.
    pub(super) unqueueing: HashMap<String, PendingUnqueue>,
    /// "Show full output" fetches by tool block id: what the server's stored
    /// bytes came back as. The fold keeps the fetch handle (`outputRef`); this
    /// keeps the result, so the card renders fetched lines without the fold
    /// ever changing.
    pub(super) full_outputs: HashMap<String, full_output::Fetch>,
    /// A synthetic `session/contextUsage`, for a scripted capture of a pressure
    /// state the echo provider cannot reach.
    pub(super) fake_context: Option<provider::facts::ContextReading>,
    /// Held only while a turn runs, so the elapsed time advances.
    pub(super) ticker: Option<Task<()>>,
    /// `BAAZ_TRACE` send trace: the turn count when the traced send left, so
    /// `first-reply-painted` waits for its user turn and a reply after it.
    pub(super) send_trace_turns: Option<usize>,
    /// The handoff card's render state on this (source) session: the run's
    /// card, refreshed on every transition. `None` when no handoff ever
    /// started here (see [`crate::handoff`]).
    pub(super) handoff_card: Option<Block>,
    /// This session was retired by a handoff: the destination provider and
    /// session id. The composer refuses sends and names the new session.
    pub(super) handed_off_to: Option<(ProviderId, String)>,
    /// A provider switch is replacing this view (Y2b): sends are refused
    /// while the replacement opens, keeping the old view drawable.
    pub(super) input_locked: bool,
    /// Where this view's own `session/start` stands. A new muse session
    /// opens its view on the click, before the server answers, so sends
    /// made meanwhile wait for the start instead of reaching a session the
    /// server does not know yet.
    pub(super) start: StartState,
    /// The switch's pick, for the composer chip while this view is still
    /// drawn: the lane itself never changes, only the displayed mark.
    pub(super) switching_to: Option<ProviderId>,
    /// This session started as a handoff's destination: where it came from.
    /// Drawn as the quiet marker at the top of the transcript.
    pub(super) handoff_origin: Option<crate::handoff::HandoffOrigin>,
    /// The handoff prefix: the source transcript's turns at activation,
    /// view-side so provider deltas never touch them. Read-only history the
    /// destination renders above its own turns (`docs/22-handoff.md` §8).
    pub(super) handoff_prefix: Rc<Vec<Rc<Turn>>>,
    /// The one divider between the prefix and this session's own turns.
    /// View-side like the prefix: it replaces the old fold-written origin
    /// marker, so exactly one divider stands per handoff, never two.
    pub(super) handoff_divider: Option<Rc<Turn>>,
    /// The pack's full model-visible text and its short summary bubble text:
    /// what identifies the destination's first user turn so the view can
    /// hide it (the divider stands for it). The pack stays in the provider
    /// history untouched.
    pub(super) handoff_pack_full: Option<String>,
    pub(super) handoff_pack_display: Option<String>,
    /// The bubble texts submits that carried `display_text` showed, by the
    /// user turn and by the echoed text, read from the session's turns in
    /// `baaz.db` on first use. `None` until a provider submit records one
    /// or a replayed delta needs a lookup: the lazy load keeps sessions
    /// that never submit with a display text off the database entirely.
    pub(super) display_overrides: Option<crate::db::turns::Displays>,
    /// Submits whose bubble text waits for the provider's echo to name the
    /// user turn it is kept under: (model-visible text, bubble text).
    pub(super) pending_displays: Vec<(String, String)>,
    /// Foreground tasks that die with the runtime: the lane's event loop,
    /// command acks, the clocks.
    pub(super) tasks: Vec<Task<()>>,
}

/// What a [`SessionRuntime`] tells the view drawing it: each names what
/// moved, and the view redraws on every one (a list operation first, for
/// the transcript's). Nothing here reaches the root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeEvent {
    /// Turns arrived at the end of the transcript (a send's bubble, a
    /// provider turn's start): the view refreshes from the first new row
    /// and keeps the tail if the reader was there.
    EntriesAppended,
    /// The turn at this index changed in place (a streamed block, a card
    /// that settled): the view rebuilds from that turn and keeps the tail.
    EntryUpdated(usize),
    /// Turns left the transcript (a retracted bubble, a dropped card).
    EntriesRemoved,
    /// An approval was parked on the session's surface.
    ApprovalRequested,
    /// A decision on this approval left: a feedback field open on it closes.
    ApprovalDecided(String),
    /// Card state the view caches moved without the transcript moving (a
    /// full output fetched, a handoff pack noted): rebuild the cache.
    CardsChanged,
    /// The model catalog, the picked model or the approval mode moved: an
    /// open composer menu re-reads its check.
    MenusStale,
    /// The turn's own state moved without the transcript moving: sending,
    /// running, the queue, a stop, a lane attached or lost. The send/stop
    /// button, the status line and the queue strip read it.
    TurnChanged,
    /// A clock on screen advanced: the running turn's elapsed second, or a
    /// question's or a retry's countdown.
    Ticked,
    /// The banner over the composer went up, changed or came down (a
    /// command error, the lease, a lost child, the billing guard).
    BannerChanged,
    /// What goes out with the next turn moved: the attachments, the effort,
    /// plan mode, a prompt handed back, the lock during a provider switch.
    ComposerChanged,
    /// A question's gathered answers moved.
    QuestionsChanged,
    /// History paging started or finished: the loading row comes or goes.
    HistoryChanged,
    /// The session's header facts (or a scripted context reading) moved.
    HeaderChanged,
    /// A short notice for the window's toast stack.
    Toast {
        /// The toast's heading.
        title: String,
        /// Its body.
        body: String,
    },
}

impl EventEmitter<SessionEvent> for SessionRuntime {}
impl EventEmitter<RuntimeEvent> for SessionRuntime {}

impl WireCall for SessionRuntime {
    fn wire_tasks(&mut self) -> &mut Vec<Task<()>> {
        &mut self.tasks
    }
}

impl Drop for SessionRuntime {
    /// A dropped runtime owns no child anymore: hang up the lane's provider
    /// so no `claude` or `codex` process outlives the session that spawned
    /// it. Idempotent — [`SessionRuntime::shutdown_lane`] is — so an
    /// explicit shutdown beforehand changes nothing. Dropping a *view*
    /// never reaches here while the registry holds the runtime.
    fn drop(&mut self) {
        self.shutdown_lane();
    }
}

impl SessionRuntime {
    /// A runtime over `session_id` with no lane yet: every field but the
    /// lane, which [`Self::install_lane`] or an opening lane sets.
    pub(crate) fn new(session_id: String, provider_id: String, workspace: String) -> Self {
        let workspace_key = workspace.clone();
        let sent_items = crate::sent::load(&provider_id, &session_id);
        Self {
            store: session_store::SessionStore::new(&session_id, aui_protocol::Provider::Muse, workspace_key.clone()),
            read_only: false,
            #[cfg(test)]
            test_wire: provider_muse::replay::Folder::new(),
            session_id,
            replay: false,
            provider_id,
            lane: lane::Lane::unattached(),
            external_approvals: ExternalApprovalStore::empty(),
            external_outbox: Vec::new(),
            external_questions: lane::ExternalQuestions::default(),
            workspace,
            sent_quote_spans: HashMap::new(),
            last_tick_secs: None,
            running: None,
            completed_turns: HashSet::new(),
            turn_aliases: HashMap::new(),
            submitting: false,
            pending_optimistic: Vec::new(),
            reply_done: None,
            models_fixture: false,
            tail_queued: HashMap::new(),
            provider_skills: Vec::new(),
            loading_history: false,
            backfill_running: false,
            backfill_stale_retried: false,
            backfill_first: false,
            lease_notice: None,
            banner: None,
            banner_is_mode_fallback: false,
            lost_at: None,
            child_failure: None,
            banner_action: None,
            tier_banner: None,
            muse_starting: false,
            answers: HashMap::new(),
            question_started: HashMap::new(),
            retry_started: None,
            countdown: None,
            pending_prompt: None,
            models: Vec::new(),
            models_error: None,
            pending_model: None,
            mode_pick_in_flight: false,
            mode_pick_queued: None,
            claude_seed_label: std::cell::OnceCell::new(),
            history_model: None,
            codex_efforts: HashMap::new(),
            claude_efforts: HashMap::new(),
            mode_limits: HashMap::new(),
            effort: None,
            plan: false,
            plan_previous_mode: None,
            plan_turn: None,
            plan_seq: 0,
            images: Vec::new(),
            image_seq: 0,
            files: Vec::new(),
            file_seq: 0,
            failed_chips: Vec::new(),
            quotes: Vec::new(),
            quote_seq: 0,
            annotation_draft: None,
            sent_items,
            created_files_through: None,
            history: history::Cursor::new(Vec::new()),
            workspace_key,
            unqueueing: HashMap::new(),
            full_outputs: HashMap::new(),
            fake_context: None,
            ticker: None,
            send_trace_turns: None,
            handoff_card: None,
            handed_off_to: None,
            input_locked: false,
            start: StartState::Started,
            switching_to: None,
            handoff_origin: None,
            handoff_prefix: Rc::new(Vec::new()),
            handoff_divider: None,
            handoff_pack_full: None,
            handoff_pack_display: None,
            display_overrides: None,
            pending_displays: Vec::new(),
            tasks: Vec::new(),
        }
    }
}

impl SessionRuntime {
    /// Tell the view what one fold batch did to the transcript, as one
    /// event: turns appended, turns removed, or the first turn that changed
    /// in place. Nothing landed, nothing said.
    pub(super) fn announce_landed(&self, landed: &[aui_protocol::Delta], cx: &mut Context<Self>) {
        use aui_protocol::Delta;
        if landed.is_empty() {
            return;
        }
        if landed.iter().any(|delta| matches!(delta, Delta::TurnStarted { .. })) {
            cx.emit(RuntimeEvent::EntriesAppended);
            return;
        }
        if landed.iter().any(|delta| matches!(delta, Delta::TurnRemoved { .. })) {
            cx.emit(RuntimeEvent::EntriesRemoved);
            return;
        }
        let turns = self.store.session().map(|session| session.turns.as_slice()).unwrap_or_default();
        let first = landed
            .iter()
            .filter_map(|delta| match delta {
                Delta::TextDelta { turn_id, .. }
                | Delta::BlockAdded { turn_id, .. }
                | Delta::BlockUpdated { turn_id, .. }
                | Delta::ThinkingDelta { turn_id, .. }
                | Delta::ToolOutputDelta { turn_id, .. }
                | Delta::BlockRemoved { turn_id, .. }
                | Delta::TurnFinished { turn_id, .. } => turns.iter().rposition(|turn| turn.id() == turn_id),
                Delta::TurnReplaced { turn } => turns.iter().rposition(|t| t.id() == turn.id()),
                Delta::TurnStarted { .. } | Delta::TurnRemoved { .. } => None,
            })
            .min();
        cx.emit(match first {
            Some(ix) => RuntimeEvent::EntryUpdated(ix),
            None => RuntimeEvent::EntriesAppended,
        });
    }
}
